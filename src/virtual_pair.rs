use std::collections::{HashMap, VecDeque};
use rust_decimal::Decimal;
use crate::node::{EngineReader, Node, Msg, Drt, PairNode, OrderBookDepth, SwapTransfer};
use crate::order::OrderBrief;
use crate::order_book::OrderBook;
use crate::pair::{TraLog, CandleData};

/// 虚拟交易对：撮合逻辑不走 orderbook，而是以每个限价单为源调用合约。
/// 合约的 CalledFn 充当"对手方"，成交结果和普通 pair 完全一致。
pub struct VirtualPair {
    id: usize,
    sheet: HashMap<usize, Decimal>,
    msgs: Vec<Msg>,

    quote_token: usize,
    base_token: usize,
    /// 缓存的当前报价（由引擎每帧从合约 master 拉取）
    price: Decimal,
    step_count: u64,
    max_tra_log_length: usize,
    precision: u8,
    /// 只许市价单模式：启用后所有限价单忽略价格，直接按市价成交
    swap_only: bool,

    /// 合约从实例 ID
    contract_slave_id: usize,
    /// 绑定的 CalledFn 索引
    fn_id: u8,

    fee_fn: Option<crate::fee::FeeFn>,

    order_book: OrderBook,
    tra_logs: VecDeque<TraLog>,
}

impl VirtualPair {
    pub fn new(
        contract_slave_id: usize,
        fn_id: u8,
        quote_token: usize,
        base_token: usize,
        max_tra_log_length: usize,
        precision: u8,
        swap_only: bool,
    ) -> Self {
        Self {
            id: 0,
            sheet: HashMap::new(),
            msgs: Vec::new(),
            tra_logs: VecDeque::new(),
            step_count: 0,
            quote_token,
            base_token,
            price: Decimal::ONE,
            max_tra_log_length,
            precision,
            swap_only,
            contract_slave_id,
            fn_id,
            fee_fn: None,
            order_book: OrderBook::new(),
        }
    }

    pub fn get_contract_slave_id(&self) -> usize { self.contract_slave_id }
    pub fn get_fn_id(&self) -> u8 { self.fn_id }

    /// 引擎在帧初写入合约提供的报价
    pub fn set_cached_price(&mut self, price: Decimal) {
        self.price = price;
    }
}

impl Node for VirtualPair {
    fn as_pair_node(&mut self) -> Option<&mut dyn PairNode> { Some(self) }
    fn as_pair_node_ref(&self) -> Option<&dyn PairNode> { Some(self) }
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

impl PairNode for VirtualPair {
    fn get_quote_token(&self) -> usize { self.quote_token }
    fn get_base_token(&self) -> usize { self.base_token }
    fn get_current_price(&self) -> Decimal { self.price }
    fn set_current_price(&mut self, price: Decimal) { self.price = price; }

    fn set_fee_fn(&mut self, fee_fn: Option<crate::fee::FeeFn>) {
        self.fee_fn = fee_fn;
    }

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
                candle_data.push_back(CandleData {
                    step_count: candle_start,
                    open,
                    high,
                    low,
                    close,
                    volume,
                });
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
        if self.order_book.contains(brief.id) {
            self.order_book.update_volume(brief.id, brief.src_volume, brief.dst_volume);
        }
    }

    fn insert_brief(&mut self, brief: OrderBrief, reader: &dyn EngineReader) {
        if self.swap_only {
            // swap_only 模式：限价单直接按市价处理，忽略价格
            let vol = if brief.direction == Drt::Buy {
                brief.src_volume
            } else {
                -(brief.src_volume * self.price).round_dp(self.precision as u32)
            };
            self.send_msg(Msg::CallContract {
                src_id: brief.id,
                contract_id: self.contract_slave_id,
                fn_id: self.fn_id,
                volume: vol,
            });
            self.push_tra_log(self.step_count, brief.id, self.contract_slave_id, self.price, brief.src_volume);
            // 手续费（swap_only 路径：买方向 brief.id 收到 base_token，卖方 slave_id 收到 quote_token）
            if let Some(ref fee_fn) = self.fee_fn {
                let (buyer, seller) = if brief.direction == Drt::Buy {
                    (brief.id, self.contract_slave_id)
                } else {
                    (self.contract_slave_id, brief.id)
                };
                let (base_vol, quote_vol) = if brief.direction == Drt::Buy {
                    (brief.src_volume / self.price, brief.src_volume)
                } else {
                    (brief.src_volume, brief.src_volume * self.price)
                };
                let fee_msgs = fee_fn(reader, crate::fee::FeeCtx {
                    base_token: self.base_token,
                    quote_token: self.quote_token,
                    buyer_node: buyer,
                    seller_node: seller,
                    taker_node: brief.id,
                    maker_node: self.contract_slave_id,
                    base_volume: base_vol,
                    quote_volume: quote_vol,
                });
                self.msgs.extend(fee_msgs);
            }
            self.send_msg(Msg::CloseOrder { order_id: brief.id });
            return;
        }
        self.order_book.insert(&brief);
        // 新订单插入时立即撮合
        self.match_virtual_orders(reader);
    }

    fn cancel_brief(&mut self, id: usize) {
        self.order_book.cancel(id);
    }

    fn match_orders(&mut self, reader: &dyn EngineReader) {
        self.match_virtual_orders(reader);
    }

    fn process_swap(
        &mut self,
        owner_id: usize,
        direction: Drt,
        volume: Decimal,
        reader: &dyn EngineReader,
    ) -> (Vec<SwapTransfer>, Vec<usize>) {
        // 市价单委托给合约：Buy → 正 volume（存款），Sell → 负 volume（取款）
        // Sell 方向时 volume 是 base token 量，需转换为 quote token 量
        let vol = if direction == Drt::Buy {
            volume
        } else {
            -(volume * self.price).round_dp(self.precision as u32)
        };
        self.send_msg(Msg::CallContract {
            src_id: owner_id,
            contract_id: self.contract_slave_id,
            fn_id: self.fn_id,
            volume: vol,
        });
        self.push_tra_log(self.step_count, owner_id, self.contract_slave_id, self.price, volume);
        // 手续费
        if let Some(ref fee_fn) = self.fee_fn {
            let (buyer, seller) = if direction == Drt::Buy {
                (owner_id, self.contract_slave_id)
            } else {
                (self.contract_slave_id, owner_id)
            };
            let (base_vol, quote_vol) = if direction == Drt::Buy {
                (volume / self.price, volume)
            } else {
                (volume, volume * self.price)
            };
            let fee_msgs = fee_fn(reader, crate::fee::FeeCtx {
                base_token: self.base_token,
                quote_token: self.quote_token,
                buyer_node: buyer,
                seller_node: seller,
                taker_node: owner_id,
                maker_node: self.contract_slave_id,
                base_volume: base_vol,
                quote_volume: quote_vol,
            });
            self.msgs.extend(fee_msgs);
        }
        (Vec::new(), Vec::new())
    }

    fn process_swaps_batch(
        &mut self,
        direction: Drt,
        swaps: &[(usize, Decimal)],
        reader: &dyn EngineReader,
    ) -> (Vec<SwapTransfer>, Vec<usize>) {
        // Sell 方向时 volume 是 base token 量，需转换为 quote token 量
        let vol_sign = if direction == Drt::Buy { Decimal::ONE } else { -Decimal::ONE };
        for &(owner_id, volume) in swaps {
            let vol = if direction == Drt::Buy {
                volume
            } else {
                volume * self.price
            };
            self.send_msg(Msg::CallContract {
                src_id: owner_id,
                contract_id: self.contract_slave_id,
                fn_id: self.fn_id,
                volume: vol * vol_sign,
            });
            self.push_tra_log(self.step_count, owner_id, self.contract_slave_id, self.price, volume);
            // 手续费
            if let Some(ref fee_fn) = self.fee_fn {
                let (buyer, seller) = if direction == Drt::Buy {
                    (owner_id, self.contract_slave_id)
                } else {
                    (self.contract_slave_id, owner_id)
                };
                let (base_vol, quote_vol) = if direction == Drt::Buy {
                    (volume / self.price, volume)
                } else {
                    (volume, volume * self.price)
                };
                let fee_msgs = fee_fn(reader, crate::fee::FeeCtx {
                    base_token: self.base_token,
                    quote_token: self.quote_token,
                    buyer_node: buyer,
                    seller_node: seller,
                    taker_node: owner_id,
                    maker_node: self.contract_slave_id,
                    base_volume: base_vol,
                    quote_volume: quote_vol,
                });
                self.msgs.extend(fee_msgs);
            }
        }
        (Vec::new(), Vec::new())
    }
}

impl VirtualPair {
    fn match_virtual_orders(&mut self, reader: &dyn EngineReader) {
        let eps = Decimal::new(1, 12);
        let slave_id = self.contract_slave_id;
        let fn_id = self.fn_id;

        // 收集所有订单
        let buys: Vec<OrderBrief> = self.order_book.buy_iter().cloned().collect();
        let sells: Vec<OrderBrief> = self.order_book.sell_iter().cloned().collect();

        // 买单：愿意支付的最高价 >= 合约报价 → 成交
        // swap_only 模式跳过价格检查（所有订单都成交）
        for buy in &buys {
            if buy.src_volume <= eps {
                self.send_msg(Msg::CloseOrder { order_id: buy.id });
                continue;
            }
            if !self.swap_only && (self.price <= eps || self.price > buy.price) {
                continue;
            }
            self.send_msg(Msg::CallContract {
                src_id: buy.id,
                contract_id: slave_id,
                fn_id,
                volume: buy.src_volume,
            });
            self.push_tra_log(self.step_count, buy.id, slave_id, self.price, buy.src_volume);
            // 手续费（买单：买方 buy.id 收到 base_token，卖方 slave_id 收到 quote_token）
            if let Some(ref fee_fn) = self.fee_fn {
                let base_vol = buy.src_volume / self.price;
                let quote_vol = buy.src_volume;
                let fee_msgs = fee_fn(reader, crate::fee::FeeCtx {
                    base_token: self.base_token,
                    quote_token: self.quote_token,
                    buyer_node: buy.id,
                    seller_node: slave_id,
                    taker_node: buy.id,
                    maker_node: self.contract_slave_id,
                    base_volume: base_vol,
                    quote_volume: quote_vol,
                });
                self.msgs.extend(fee_msgs);
            }
            self.send_msg(Msg::CloseOrder { order_id: buy.id });
        }

        // 卖单：愿意接受的最低价 <= 合约报价 → 成交（负 volume）
        // 卖单 volume 是 base token 量，需转换为 quote token 量
        for sell in &sells {
            if sell.src_volume <= eps {
                self.send_msg(Msg::CloseOrder { order_id: sell.id });
                continue;
            }
            if !self.swap_only && (self.price <= eps || self.price < sell.price) {
                continue;
            }
            let sell_vol = -(sell.src_volume * self.price).round_dp(self.precision as u32);
            self.send_msg(Msg::CallContract {
                src_id: sell.id,
                contract_id: slave_id,
                fn_id,
                volume: sell_vol,
            });
            self.push_tra_log(self.step_count, slave_id, sell.id, self.price, sell.src_volume);
            // 手续费（卖单：买方 buy.id 收到 base_token，卖方 slave_id 收到 quote_token）
            if let Some(ref fee_fn) = self.fee_fn {
                let base_vol = sell.src_volume;
                let quote_vol = sell.src_volume * self.price;
                let fee_msgs = fee_fn(reader, crate::fee::FeeCtx {
                    base_token: self.base_token,
                    quote_token: self.quote_token,
                    buyer_node: slave_id,
                    seller_node: sell.id,
                    taker_node: sell.id,
                    maker_node: self.contract_slave_id,
                    base_volume: base_vol,
                    quote_volume: quote_vol,
                });
                self.msgs.extend(fee_msgs);
            }
            self.send_msg(Msg::CloseOrder { order_id: sell.id });
        }

        // 从订单簿移除
        for buy in &buys {
            self.order_book.cancel(buy.id);
        }
        for sell in &sells {
            self.order_book.cancel(sell.id);
        }
    }
}
