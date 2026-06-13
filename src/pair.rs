use std::collections::{HashMap, VecDeque};
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use crate::node::{Node, Msg, Drt, PairNode, OrderBookDepth};
use crate::order::OrderBrief;
use crate::order_book::OrderBook;


#[derive(Clone)]
pub struct TraLog {
    step_count: u64,
    buy_node: usize,
    sell_node: usize,
    price: Decimal,
    volume: Decimal,
}


#[derive(Clone)]
pub struct CandleData {
    step_count: u64,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: Decimal,
}


pub struct Pair {
    id: usize,
    sheet: HashMap<usize, Decimal>,
    msgs: Vec<Msg>,

    quote_token: usize,
    base_token: usize,
    price: Decimal,
    step_count: u64,
    max_tra_log_length: usize,
    precision: u8,

    order_book: OrderBook,
    tra_logs: VecDeque<TraLog>,
}


impl Pair {
    pub fn new(quote_token: usize, base_token: usize, price: Decimal, max_tra_log_length: usize, precision: u8) -> Self {
        Self {
            id: 0,
            sheet: HashMap::new(),
            msgs: Vec::new(),
            tra_logs: VecDeque::new(),
            step_count: 0,
            max_tra_log_length,
            precision,
            quote_token,
            base_token,
            price,
            order_book: OrderBook::new(),
        }
    }

    fn round(&self, value: Decimal) -> Decimal {
        value.round_dp_with_strategy(self.precision as u32, RoundingStrategy::ToZero)
    }
}

impl Node for Pair {
    fn as_pair_node(&mut self) -> Option<&mut dyn PairNode> { Some(self) }
    fn get_msgs(&mut self) -> &mut Vec<Msg> { &mut self.msgs }
    fn get_id(&self) -> usize { self.id } 
    fn set_id(&mut self, id: usize) { self.id = id; }

    fn balance(&self, token: usize) -> Decimal {
        self.sheet.get(&token).copied().unwrap_or(Decimal::ZERO)
    }
    fn set_balance(&mut self, token: usize, volume: Decimal) {
        self.sheet.insert(token, volume);
    }
    fn drain_balances(&mut self) -> HashMap<usize, Decimal> {
        std::mem::take(&mut self.sheet)
    }

    fn update(&mut self, step_count: u64) {
        self.step_count = step_count;
    }
}


impl PairNode for Pair {
    fn get_quote_token(&self) -> usize { self.quote_token }
    fn get_base_token(&self) -> usize { self.base_token }
    fn get_current_price(&self) -> Decimal { self.price }
    
    fn get_order_book(&self, direction: Drt, depth: usize) -> OrderBookDepth {
        let depth_data = self.order_book.get_depth(direction, depth + 1);
        if let Some((price, volume)) = depth_data.get(depth) {
            OrderBookDepth { price: *price, volume: *volume }
        } else {
            OrderBookDepth { price: Decimal::ZERO, volume: Decimal::ZERO }
        }
    }
    
    fn get_tra_logs(&self) -> VecDeque<TraLog> { self.tra_logs.clone() }
    
    fn push_tra_log(&mut self, step_count: u64, src_id: usize, dst_id: usize, price: Decimal, volume: Decimal) {
        self.tra_logs.push_back(TraLog {
            step_count,
            buy_node: src_id,
            sell_node: dst_id,
            price,
            volume,
        });
        if self.tra_logs.len() > self.max_tra_log_length {
            self.tra_logs.pop_front();
        }
    }

    fn get_candle_data(&self, interval: u64) -> VecDeque<CandleData> {
        let mut candle_data = VecDeque::new();
        if self.tra_logs.is_empty() || interval == 0 {
            return candle_data;
        }

        let mut iter = self.tra_logs.iter();
        let first = match iter.next() {
            Some(log) => log,
            None => return candle_data,
        };

        let mut candle_start = (first.step_count / interval) * interval;
        let mut open = first.price;
        let mut high = first.price;
        let mut low = first.price;
        let mut close = first.price;
        let mut volume = first.volume;

        for log in iter {
            let cs = (log.step_count / interval) * interval;
            if cs != candle_start {
                // 完成当前 K 线
                candle_data.push_back(CandleData {
                    step_count: candle_start,
                    open,
                    high,
                    low,
                    close,
                    volume,
                });
                // 开始新 K 线
                candle_start = cs;
                open = log.price;
                high = log.price;
                low = log.price;
                close = log.price;
                volume = log.volume;
            } else {
                if log.price > high { high = log.price; }
                if log.price < low { low = log.price; }
                close = log.price;
                volume += log.volume;
            }
        }

        // 最后一根为未完结 K 线
        candle_data.push_back(CandleData {
            step_count: candle_start,
            open,
            high,
            low,
            close,
            volume,
        });

        candle_data
    }

    fn latest_candle(&self, interval: u64) -> Option<CandleData> {
        if self.tra_logs.is_empty() || interval == 0 {
            return None;
        }

        let latest_step = self.tra_logs.back()?.step_count;
        let candle_start = (latest_step / interval) * interval;

        let mut open = None;
        let mut high: Option<Decimal> = None;
        let mut low: Option<Decimal> = None;
        let mut close: Option<Decimal> = None;
        let mut volume = Decimal::ZERO;

        for log in &self.tra_logs {
            if (log.step_count / interval) * interval != candle_start {
                continue;
            }
            let p = log.price;
            if open.is_none() {
                open = Some(p);
                high = Some(p);
                low = Some(p);
            }
            if p > high.unwrap() { high = Some(p); }
            if p < low.unwrap() { low = Some(p); }
            close = Some(p);
            volume += log.volume;
        }

        open.map(|open| CandleData {
            step_count: candle_start,
            open,
            high: high.unwrap(),
            low: low.unwrap(),
            close: close.unwrap(),
            volume,
        })
    }

    fn update_brief(&mut self, brief: OrderBrief) {
        // 仅更新已在订单簿中的订单（余额变化通过 Transfer 同步）
        if self.order_book.contains(brief.id) {
            self.order_book.update_volume(brief.id, brief.src_volume, brief.dst_volume);
        }
    }

    fn insert_brief(&mut self, brief: OrderBrief) {
        // 新订单插入订单簿并触发撮合
        self.order_book.insert(&brief);
        self.match_orders();
    }

    fn cancel_brief(&mut self, id: usize) {
        self.order_book.cancel(id);
    }

    fn match_orders(&mut self) {
        let eps = Decimal::new(1, 12); // 1e-12
        let mut match_price = self.price;

        loop {
            // 获取最佳买卖单
            let buy_opt = self.order_book.best_buy().cloned();
            let sell_opt = self.order_book.best_sell().cloned();

            let (buy, sell) = match (buy_opt, sell_opt) {
                (Some(b), Some(s)) => (b, s),
                _ => break,
            };

            // 跳过零价格订单（避免除零错误）
            if buy.price <= eps {
                self.send_msg(Msg::CloseOrder { order_id: buy.id });
                self.order_book.cancel(buy.id);
                continue;
            }
            if sell.price <= eps {
                self.send_msg(Msg::CloseOrder { order_id: sell.id });
                self.order_book.cancel(sell.id);
                continue;
            }

            // 最高买价 < 最低卖价 → 撮合结束
            if buy.price < sell.price {
                break;
            }

            // 先挂单的价格优先
            match_price = if buy.step_count_created < sell.step_count_created {
                buy.price
            } else if buy.step_count_created > sell.step_count_created {
                sell.price
            } else {
                (sell.price + buy.price) / Decimal::new(2, 0)
            };

            let match_base_volume = (buy.src_volume / match_price).min(sell.src_volume);
            let match_quote_volume = (match_base_volume * match_price).min(buy.src_volume);

            // 发消息让 Engine 执行转账
            self.send_msg(Msg::Transfer {
                src_id: buy.id,
                dst_id: sell.id,
                token: self.quote_token,
                volume: match_quote_volume,
                allow_negative: false,
            });
            self.send_msg(Msg::Transfer {
                src_id: sell.id,
                dst_id: buy.id,
                token: self.base_token,
                volume: match_base_volume,
                allow_negative: false,
            });

            self.push_tra_log(self.step_count, buy.id, sell.id, self.round(match_price), match_base_volume);

            // 计算剩余量 (use updated values from the cloned briefs)
            let buy_remaining = buy.src_volume - match_quote_volume;
            let sell_remaining = sell.src_volume - match_base_volume;

            // 处理买单
            if buy_remaining <= eps {
                self.send_msg(Msg::CloseOrder { order_id: buy.id });
                self.order_book.cancel(buy.id);
            } else {
                self.order_book.update_volume(
                    buy.id,
                    buy_remaining,
                    buy.dst_volume + match_base_volume
                );
            }

            // 处理卖单
            if sell_remaining <= eps {
                self.send_msg(Msg::CloseOrder { order_id: sell.id });
                self.order_book.cancel(sell.id);
            } else {
                self.order_book.update_volume(
                    sell.id,
                    sell_remaining,
                    sell.dst_volume + match_quote_volume
                );
            }
        }

        self.price = self.round(match_price);
    }

    /// 市价单直接撮合（同步完成，不创建临时订单节点，不产生跨帧消息）
    /// `transfer` 闭包由引擎提供，执行即时余额划转
    fn process_swap(
        &mut self,
        user_id: usize,
        direction: Drt,
        volume: Decimal,
    ) {
        let eps = Decimal::new(1, 12); // 1e-12

        if direction == Drt::Buy {
            let mut remaining = volume; // quote_token

            while remaining > eps {
                let sell_opt = self.order_book.best_sell().cloned();
                let sell = match sell_opt {
                    Some(s) => s,
                    None => break,
                };

                if sell.price <= eps {
                    self.send_msg(Msg::CloseOrder { order_id: sell.id });
                    self.order_book.cancel(sell.id);
                    continue;
                }

                let max_base = remaining / sell.price;
                let match_base = max_base.min(sell.src_volume);
                let match_quote = (match_base * sell.price).min(remaining);
                let match_quote = self.round(match_quote);
                let match_base = self.round(match_base);

                // 精度不足，无法进一步成交
                if match_quote.is_zero() || match_base.is_zero() {
                    break;
                }

                self.send_msg(Msg::Transfer {
                    src_id: user_id,
                    dst_id: sell.id,
                    token: self.quote_token,
                    volume: match_quote,
                    allow_negative: false,
                });
                self.send_msg(Msg::Transfer {
                    src_id: sell.id,
                    dst_id: user_id,
                    token: self.base_token,
                    volume: match_base,
                    allow_negative: false,
                });

                self.push_tra_log(self.step_count, user_id, sell.id, sell.price, match_base);
                self.price = sell.price;

                remaining -= match_quote;

                let sell_remaining = sell.src_volume - match_base;
                if sell_remaining <= eps {
                    self.send_msg(Msg::CloseOrder { order_id: sell.id });
                    self.order_book.cancel(sell.id);
                } else {
                    self.order_book.update_volume(sell.id, sell_remaining, sell.dst_volume + match_quote);
                }
            }
        } else {
            let mut remaining = volume; // base_token

            while remaining > eps {
                let buy_opt = self.order_book.best_buy().cloned();
                let buy = match buy_opt {
                    Some(b) => b,
                    None => break,
                };

                if buy.price <= eps {
                    self.send_msg(Msg::CloseOrder { order_id: buy.id });
                    self.order_book.cancel(buy.id);
                    continue;
                }

                let max_quote = buy.src_volume;
                let max_base = max_quote / buy.price;
                let match_base = max_base.min(remaining);
                let match_quote = (match_base * buy.price).min(max_quote);
                let match_quote = self.round(match_quote);
                let match_base = self.round(match_base);

                // 精度不足，无法进一步成交
                if match_quote.is_zero() || match_base.is_zero() {
                    break;
                }

                self.send_msg(Msg::Transfer {
                    src_id: user_id,
                    dst_id: buy.id,
                    token: self.base_token,
                    volume: match_base,
                    allow_negative: false,
                });
                self.send_msg(Msg::Transfer {
                    src_id: buy.id,
                    dst_id: user_id,
                    token: self.quote_token,
                    volume: match_quote,
                    allow_negative: false,
                });

                self.push_tra_log(self.step_count, buy.id, user_id, buy.price, match_base);
                self.price = buy.price;

                remaining -= match_base;

                let buy_remaining = buy.src_volume - match_quote;
                if buy_remaining <= eps {
                    self.send_msg(Msg::CloseOrder { order_id: buy.id });
                    self.order_book.cancel(buy.id);
                } else {
                    self.order_book.update_volume(buy.id, buy_remaining, buy.dst_volume + match_base);
                }
            }
        }
    }
}
