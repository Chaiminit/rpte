use std::collections::{HashMap, VecDeque};
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use crate::node::{Node, Msg, Drt, PairNode, OrderBookDepth, SwapTransfer};
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
    pub step_count: u64,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
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
        let min_unit = Decimal::new(1, self.precision as u32);
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

            let match_base_volume = self.round((buy.src_volume / match_price).min(sell.src_volume));
            let match_quote_volume = self.round((match_base_volume * match_price).min(buy.src_volume));

            if match_base_volume <= eps {
                // 精度截断后成交量为0 → 买单剩余量太小无法继续成交
                // 关闭买单，卖单保留继续等待后续对手单
                self.send_msg(Msg::CloseOrder { order_id: buy.id });
                self.order_book.cancel(buy.id);
                continue;
            }

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
            // 若剩余量 < min_unit，或剩余量不足以产生至少 1 min_unit 的下轮成交 → 关单
            if buy_remaining <= min_unit || self.round(buy_remaining / buy.price) < min_unit {
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
            if sell_remaining <= min_unit {
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

    fn process_swap(&mut self, owner_id: usize, direction: Drt, volume: Decimal) -> (Vec<SwapTransfer>, Vec<usize>) {
        let eps = Decimal::new(1, 12); // 1e-12
        let min_unit = Decimal::new(1, self.precision as u32);
        let mut transfers = Vec::new();
        let mut close_ids = Vec::new();

        if direction == Drt::Buy {
            // Swap Buy: 花费 quote_token 买入 base_token，匹配卖单队列
            let mut remaining = volume; // quote_token

            while remaining > eps {
                let sell_opt = self.order_book.best_sell().cloned();
                let sell = match sell_opt {
                    Some(s) => s,
                    None => break,
                };

                // 跳过零价格卖单
                if sell.price <= eps {
                    close_ids.push(sell.id);
                    self.order_book.cancel(sell.id);
                    continue;
                }

                let max_base = remaining / sell.price;
                let match_base = self.round(max_base.min(sell.src_volume));
                let match_quote = self.round((match_base * sell.price).min(remaining));

                if match_base <= eps {
                    close_ids.push(sell.id);
                    self.order_book.cancel(sell.id);
                    continue;
                }

                transfers.push(SwapTransfer {
                    src_id: owner_id,
                    dst_id: sell.id,
                    token: self.quote_token,
                    volume: match_quote,
                });
                transfers.push(SwapTransfer {
                    src_id: sell.id,
                    dst_id: owner_id,
                    token: self.base_token,
                    volume: match_base,
                });

                self.push_tra_log(self.step_count, owner_id, sell.id, sell.price, match_base);
                self.price = self.round(sell.price);

                remaining -= match_quote;

                let sell_remaining = sell.src_volume - match_base;
                if sell_remaining <= min_unit {
                    close_ids.push(sell.id);
                    self.order_book.cancel(sell.id);
                } else {
                    self.order_book.update_volume(sell.id, sell_remaining, sell.dst_volume);
                }
            }
        } else {
            // Swap Sell: 卖出 base_token 换取 quote_token，匹配买单队列
            let mut remaining = volume; // base_token

            while remaining > eps {
                let buy_opt = self.order_book.best_buy().cloned();
                let buy = match buy_opt {
                    Some(b) => b,
                    None => break,
                };

                // 跳过零价格买单
                if buy.price <= eps {
                    close_ids.push(buy.id);
                    self.order_book.cancel(buy.id);
                    continue;
                }

                let max_quote = buy.src_volume;
                let max_base = max_quote / buy.price;
                let match_base = self.round(max_base.min(remaining));
                let match_quote = self.round((match_base * buy.price).min(max_quote));

                if match_base <= eps {
                    close_ids.push(buy.id);
                    self.order_book.cancel(buy.id);
                    continue;
                }

                transfers.push(SwapTransfer {
                    src_id: owner_id,
                    dst_id: buy.id,
                    token: self.base_token,
                    volume: match_base,
                });
                transfers.push(SwapTransfer {
                    src_id: buy.id,
                    dst_id: owner_id,
                    token: self.quote_token,
                    volume: match_quote,
                });

                self.push_tra_log(self.step_count, buy.id, owner_id, buy.price, match_base);
                self.price = self.round(buy.price);

                remaining -= match_base;

                let buy_remaining = buy.src_volume - match_quote;
                if buy_remaining <= min_unit || self.round(buy_remaining / buy.price) < min_unit {
                    close_ids.push(buy.id);
                    self.order_book.cancel(buy.id);
                } else {
                    self.order_book.update_volume(buy.id, buy_remaining, buy.dst_volume);
                }
            }
        }

        (transfers, close_ids)
    }

    /// 批量按比例分配市价单撮合。
    /// 同帧内所有 swap 共享流动性，按剩余需求比例分配每个价位的流动性。
    fn process_swaps_batch(&mut self, direction: Drt, swaps: &[(usize, Decimal)]) -> (Vec<SwapTransfer>, Vec<usize>) {
        let eps = Decimal::new(1, 12);
        let min_unit = Decimal::new(1, self.precision as u32);
        let mut transfers = Vec::new();
        let mut close_ids = Vec::new();
        let n = swaps.len();
        let mut remain: Vec<Decimal> = swaps.iter().map(|(_, v)| *v).collect();

        let mut batch_guard = 500;
        loop {
            let total_remain: Decimal = remain.iter().sum();
            if total_remain <= eps { break; }
            batch_guard -= 1;
            if batch_guard == 0 { break; }

            // 获取本价位的最佳对手单
            let ob_opt = if direction == Drt::Buy {
                self.order_book.best_sell().cloned()
            } else {
                self.order_book.best_buy().cloned()
            };
            let ob = match ob_opt { Some(o) => o, None => break };
            if ob.price <= eps {
                close_ids.push(ob.id);
                self.order_book.cancel(ob.id);
                continue;
            }

            // 计算该订单能提供的流动性（以 source token 计）
            // Buy: source=quote, 卖单 src_volume 是 base, 可提供 quote = src_volume * price
            // Sell: source=base, 买单 src_volume 是 quote, 可提供 base = src_volume / price
            let available_source = if direction == Drt::Buy {
                (ob.src_volume * ob.price).min(total_remain)
            } else {
                (ob.src_volume / ob.price).min(total_remain)
            };

            if available_source <= eps {
                close_ids.push(ob.id);
                self.order_book.cancel(ob.id);
                continue;
            }

            let (src_token, dst_token) = if direction == Drt::Buy {
                (self.quote_token, self.base_token)
            } else {
                (self.base_token, self.quote_token)
            };

            // 按比例分配本价位流动性（最大余数法，零残差）
            // Phase 1: 计算每个 swap 的 floor 分配值
            let mut allocs: Vec<(usize, Decimal)> = Vec::new(); // (swap_index, floor_source)
            let mut total_floor = Decimal::ZERO;

            for i in 0..n {
                if remain[i] <= eps { continue; }
                let exact = available_source * remain[i] / total_remain;
                let floor = self.round(exact);
                if floor <= eps { continue; }
                allocs.push((i, floor));
                total_floor += floor;
            }

            if allocs.is_empty() {
                // 所有分配都 round 到 0，跳过该订单
                close_ids.push(ob.id);
                self.order_book.cancel(ob.id);
                continue;
            }

            // Phase 2: 将剩余量分配给 fractional loss 最大的 swap
            let remainder = available_source - total_floor;
            if remainder >= min_unit {
                // 按 fractional loss 降序排列（即 exact - floor 最大的排前面）
                let mut frac_losses: Vec<(usize, Decimal)> = allocs.iter().map(|&(idx, floor)| {
                    // 重新计算 exact
                    let i = idx;
                    let exact = available_source * remain[i] / total_remain;
                    (idx, exact - floor)
                }).collect();
                frac_losses.sort_by(|a, b| b.1.cmp(&a.1));

                let mut rem = remainder;
                for (alloc_idx, _) in &frac_losses {
                    if rem < min_unit { break; }
                    // 找到 allocs 中的对应项并增加
                    if let Some(entry) = allocs.iter_mut().find(|(idx, _)| idx == alloc_idx) {
                        entry.1 += min_unit;
                        rem -= min_unit;
                    }
                }
            }

            // Phase 3: 根据最终分配生成转账指令
            let mut order_consumed_src = Decimal::ZERO;

            for &(i, match_source) in &allocs {
                let (match_dest, order_delta) = if direction == Drt::Buy {
                    let md = self.round((match_source / ob.price).min(ob.src_volume - order_consumed_src));
                    (md, md)
                } else {
                    let md = self.round((match_source * ob.price).min(ob.src_volume - order_consumed_src));
                    (md, md)
                };

                if match_dest <= eps { continue; }

                transfers.push(SwapTransfer {
                    src_id: swaps[i].0,
                    dst_id: ob.id,
                    token: src_token,
                    volume: match_source,
                });
                transfers.push(SwapTransfer {
                    src_id: ob.id,
                    dst_id: swaps[i].0,
                    token: dst_token,
                    volume: match_dest,
                });

                // 记录成交日志
                let (buy_node, sell_node, log_volume) = if direction == Drt::Buy {
                    (swaps[i].0, ob.id, match_dest)
                } else {
                    (ob.id, swaps[i].0, match_source)
                };
                self.push_tra_log(self.step_count, buy_node, sell_node, ob.price, log_volume);

                remain[i] -= match_source;
                order_consumed_src += order_delta;
            }

            if order_consumed_src <= eps {
                close_ids.push(ob.id);
                self.order_book.cancel(ob.id);
                continue;
            }

            // 更新价格
            self.price = self.round(ob.price);

            // 更新订单簿
            let order_remaining = if direction == Drt::Buy {
                ob.src_volume - order_consumed_src  // base remaining
            } else {
                ob.src_volume - order_consumed_src  // quote remaining
            };

            // 对于买单剩余(quote)，额外检查是否能产生至少 1 min_unit 的下一轮成交
            let too_small_for_next = if direction == Drt::Sell {
                self.round(order_remaining / ob.price) < min_unit
            } else {
                false
            };
            if order_remaining <= min_unit || too_small_for_next {
                // 残留量低于最小精度单位，无实际价值，关闭
                close_ids.push(ob.id);
                self.order_book.cancel(ob.id);
            } else {
                let (new_src, new_dst) = if direction == Drt::Buy {
                    // Sell order: src=base, dst=quote
                    (order_remaining, ob.dst_volume + ob.price * order_consumed_src)
                } else {
                    // Buy order: src=quote, dst=base
                    (order_remaining, ob.dst_volume + order_consumed_src / ob.price)
                };
                self.order_book.update_volume(ob.id, new_src, new_dst);
            }
        }

        (transfers, close_ids)
    }
}
