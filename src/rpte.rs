use std::collections::{HashMap, HashSet};
use std::collections::VecDeque;
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use crate::error::{Error, Result};
use crate::node::{Node, Msg, Drt, OrderBookDepth, PairNode, OrderNode, AccountNode};
use crate::token::Token;
use crate::pair::Pair;
use crate::account::Account;
use std::mem::take;
use crate::order::{Order, OrderBrief, OrderType};
use crate::pair::{TraLog, CandleData};


pub struct Rpte {
    nodes: Vec<Box<dyn Node>>,
    token_id_to_name: HashMap<usize, String>,
    token_name_to_id: HashMap<String, usize>,
    global_quote_token: usize,
    order_pool: Vec<usize>,
    registered_orders: HashSet<usize>,
    registered_accounts: HashSet<usize>,
    registered_pairs: HashSet<usize>,
    registered_token_pairs: HashMap<(usize, usize), usize>,
    step_count: u64,
    max_tra_log_length: usize,
    msgs: Vec<Msg>,
    running: bool,
    precision: u8,
}


impl Rpte {
    pub fn new(global_quote_token_name: &str, precision: u8) -> Self {
        let mut rpte = Self {
            nodes: Vec::new(),
            token_id_to_name: HashMap::new(),
            token_name_to_id: HashMap::new(),
            global_quote_token: 0,
            order_pool: Vec::new(),
            registered_orders: HashSet::new(),
            registered_accounts: HashSet::new(),
            registered_pairs: HashSet::new(),
            registered_token_pairs: HashMap::new(),
            step_count: 0,
            max_tra_log_length: 10000000000,
            msgs: Vec::new(),
            running: true,
            precision,
        };
        let id = rpte.register_token(global_quote_token_name);
        rpte.global_quote_token = id;
        rpte
    }

    pub fn get_precision(&self) -> u8 {
        self.precision
    }

    /// 处理一帧消息，供外部手动驱动
    pub fn step(&mut self) {
        self.update();
    }

    /// 停止引擎（将在当前帧处理完毕后退出）
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// 以固定帧率运行引擎循环，直到调用 stop()
    pub fn run<F>(&mut self, fps: u64, mut callback: F)
    where
        F: FnMut(&mut Self)
    {
        assert!(fps > 0, "run: fps must be > 0");
        let frame_duration = std::time::Duration::from_secs_f64(1.0 / fps as f64);
        while self.running {
            let start = std::time::Instant::now();
            self.step();
            callback(self);
            let elapsed = start.elapsed();
            if elapsed < frame_duration {
                std::thread::sleep(frame_duration - elapsed);
            } else {
                use std::sync::Once;
                static WARN: Once = Once::new();
                WARN.call_once(|| {
                    eprintln!("WARNING: engine falling behind (update took {:?} > {:?} per frame)",
                        elapsed, frame_duration);
                });
            }
        }
    }

    pub fn register_token(&mut self, name: &str) -> usize {
        if let Some(&id) = self.token_name_to_id.get(name) {
            return id;
        }
        let token = Token::new(name);
        let id = self.register_node(token);
        self.token_name_to_id.insert(name.to_string(), id);
        self.token_id_to_name.insert(id, name.to_string());
        id
    }

    pub fn get_global_quote_token(&self) -> usize {
        self.global_quote_token
    }

    pub fn get_token_by_name(&self, name: &str) -> Option<usize> {
        self.token_name_to_id.get(name).copied()
    }

    pub fn get_token_name(&self, id: usize) -> Option<&str> {
        self.token_id_to_name.get(&id).map(|s| s.as_str())
    }

    pub fn get_all_tokens(&self) -> Vec<usize> {
        self.token_id_to_name.keys().copied().collect()
    }

    pub fn register_pair(&mut self, new_pair: impl PairNode + 'static) -> usize {
        let pair_id = self.register_node(new_pair);
        self.registered_pairs.insert(pair_id);
        pair_id
    }

    pub fn register_node(&mut self, mut new_node: impl Node + 'static) -> usize {
        let id = self.nodes.len();
        new_node.set_id(id);
        self.nodes.push(Box::new(new_node));
        id
    }

    /// 注册账户，返回账户 ID
    pub fn register_account(&mut self) -> usize {
        let id = self.register_node(Account::new());
        self.registered_accounts.insert(id);
        id
    }

    pub fn get_all_accounts(&self) -> Vec<usize> {
        self.registered_accounts.iter().copied().collect()
    }

    pub fn get_all_pairs(&self) -> Vec<usize> {
        self.registered_pairs.iter().cloned().collect()
    }

    pub fn get_all_orders(&self) -> Vec<usize> {
        self.registered_orders.iter()
            .filter(|&&id| id < self.nodes.len() && self.nodes[id].is_open())
            .copied()
            .collect()
    }

    pub fn get_node_balance(&mut self, id: usize, token: usize) -> Result<Decimal> {
        if id >= self.nodes.len() {
            return Err(Error::NodeNotFound { id, len: self.nodes.len() });
        }
        if !self.token_id_to_name.contains_key(&token) {
            return Err(Error::TokenNotRegistered(token));
        }
        Ok(self.nodes[id].balance(token))
    }

    /// 获取当前价格。
    /// 返回 `(price, quote_token, base_token)`，含义为 1 base_token = price quote_token。
    pub fn get_current_price(&mut self, src_token: usize, dst_token: usize) -> Result<(Decimal, usize, usize)> {
        let (pair_id, _) = self.get_or_create_pair(src_token, dst_token);
        let pair = self.get_pair_node(pair_id)?;
        Ok((pair.get_current_price(), pair.get_quote_token(), pair.get_base_token()))
    }

    /// 获取订单簿深度。
    /// `src_token` 为支出代币，`dst_token` 为收入代币，由引擎自行推导买卖方向。
    pub fn get_order_book(&mut self, src_token: usize, dst_token: usize, depth: usize) -> Result<OrderBookDepth> {
        let (pair_id, is_forward) = self.get_or_create_pair(src_token, dst_token);
        // forward: src=quote, dst=base → Buy (支出 quote 买入 base)
        // reverse: src=base, dst=quote → Sell (卖出 base 获取 quote)
        let direction = if is_forward { Drt::Buy } else { Drt::Sell };
        Ok(self.get_pair_node(pair_id)?.get_order_book(direction, depth))
    }

    // ========== 账户操作代理方法 ==========

    /// 创建限价单
    pub fn make(&mut self, src_id: usize, src_token: usize, dst_token: usize, volume: impl Into<Decimal>, price: impl Into<Decimal>) {
        let volume = self.round(volume.into());
        let price = self.round(price.into());
        self.msgs.push(Msg::OpenOrder {
            src_id,
            owner_node_id: src_id,
            src_token,
            dst_token,
            volume,
            price,
        });
    }

    /// 创建市价单
    pub fn swap(&mut self, src_id: usize, src_token: usize, dst_token: usize, volume: impl Into<Decimal>) {
        let volume = self.round(volume.into());
        self.msgs.push(Msg::SwapOrder {
            src_id,
            owner_node_id: src_id,
            src_token,
            dst_token,
            volume,
        });
    }

    /// 转账（默认严格模式：余额不足时报错）
    pub fn transfer(&mut self, src_id: usize, dst_id: usize, token: usize, volume: impl Into<Decimal>) {
        let volume = self.round(volume.into());
        self.msgs.push(Msg::Transfer {
            src_id,
            dst_id,
            token,
            volume,
            allow_negative: false,
        });
    }

    /// 允许透支的转账（余额不足时余额变负）
    pub fn transfer_with_overdraft(&mut self, src_id: usize, dst_id: usize, token: usize, volume: impl Into<Decimal>) {
        let volume = self.round(volume.into());
        self.msgs.push(Msg::Transfer {
            src_id,
            dst_id,
            token,
            volume,
            allow_negative: true,
        });
    }

    /// 取消订单
    pub fn cancel_order(&mut self, order_id: usize) {
        self.msgs.push(Msg::CloseOrder { order_id });
    }

    /// 根据 src_token/dst_token 查找或自动创建交易对
    /// 返回 (pair_id, is_forward)，is_forward 表示 src_token == pair.quote_token
    fn get_or_create_pair(&mut self, src_token: usize, dst_token: usize) -> (usize, bool) {
        // 1. 查缓存（两种方向）
        if let Some(&id) = self.registered_token_pairs.get(&(src_token, dst_token)) {
            return (id, true);
        }
        if let Some(&id) = self.registered_token_pairs.get(&(dst_token, src_token)) {
            return (id, false);
        }

        // 2. 遍历已注册的交易对（兼容手动 register_pair）
        for &pair_id in &self.registered_pairs {
            let pair = self.nodes.get_mut(pair_id).and_then(|n| n.as_pair_node());
            let (qt, bt) = match pair {
                Some(p) => (p.get_quote_token(), p.get_base_token()),
                None => continue,
            };
            if qt == src_token && bt == dst_token {
                self.registered_token_pairs.insert((src_token, dst_token), pair_id);
                return (pair_id, true);
            }
            if qt == dst_token && bt == src_token {
                self.registered_token_pairs.insert((src_token, dst_token), pair_id);
                return (pair_id, false);
            }
        }

        // 3. 自动创建新交易对
        let (quote_token, base_token) = if src_token == self.global_quote_token {
            (src_token, dst_token)
        } else if dst_token == self.global_quote_token {
            (dst_token, src_token)
        } else {
            let src_name = self.token_id_to_name.get(&src_token).map(|s| s.as_str()).unwrap_or("");
            let dst_name = self.token_id_to_name.get(&dst_token).map(|s| s.as_str()).unwrap_or("");
            if src_name <= dst_name {
                (src_token, dst_token)
            } else {
                (dst_token, src_token)
            }
        };

        let is_forward = src_token == quote_token;
        let price = Decimal::ONE;
        let pair = Pair::new(quote_token, base_token, price, self.max_tra_log_length, self.precision);
        let pair_id = self.register_pair(pair);
        self.registered_token_pairs.insert((src_token, dst_token), pair_id);
        (pair_id, is_forward)
    }

    fn round(&self, value: Decimal) -> Decimal {
        value.round_dp_with_strategy(self.precision as u32, RoundingStrategy::ToZero)
    }

    /// 发行资产到指定节点
    pub fn issue(&mut self, node_id: usize, token: usize, volume: impl Into<Decimal>) -> Result<()> {
        if node_id >= self.nodes.len() {
            return Err(Error::NodeNotFound { id: node_id, len: self.nodes.len() });
        }
        if !self.token_id_to_name.contains_key(&token) {
            return Err(Error::TokenNotRegistered(token));
        }
        let volume = self.round(volume.into());
        self.nodes[node_id].adjust_balance(token, volume);
        Ok(())
    }

    pub fn new_order(&mut self) -> usize {
        if let Some(id) = self.order_pool.pop() {
            id
        } else {
            let order = Order::default();
            let order_id = self.register_node(order);
            self.registered_orders.insert(order_id);
            order_id
        }
    }

    pub fn get_account_orders(&mut self, account_id: usize) -> Result<&HashSet<usize>> {
        let account = self.get_account_node(account_id)?;
        Ok(account.get_orders())
    }

    pub fn get_order_brief(&mut self, order_id: usize) -> Result<OrderBrief> {
        if !self.registered_orders.contains(&order_id) {
            return Err(Error::OrderNotRegistered(order_id));
        }

        let (pair_id, src_token, dst_token, price, step_count_created, src_volume, dst_volume) = {
            let order = self.get_order_node(order_id)?;
            let src = order.get_src_token();
            let dst = order.get_dst_token();
            let price = *order.get_price();
            let step = order.get_step_count_created();
            let pair = order.get_pair_node_id();
            let sv = order.balance(src);
            let dv = order.balance(dst);
            (pair, src, dst, price, step, sv, dv)
        };

        let drt = {
            let pair = self.get_pair_node(pair_id)?;
            if pair.get_quote_token() == src_token && pair.get_base_token() == dst_token {
                Drt::Buy
            } else {
                Drt::Sell
            }
        };

        Ok(OrderBrief {
            id: order_id,
            direction: drt,
            src_token,
            dst_token,
            src_volume,
            dst_volume,
            price,
            step_count_created,
        })
    }

    pub fn return_order(&mut self, order_id: usize) {
        self.order_pool.push(order_id);
    }

    fn _close_order_immediate(&mut self, order_id: usize) {
        if !self.registered_orders.contains(&order_id) {
            return;
        }
        // 读取 owner_id 和 pair_id，然后关闭
        let (owner_id, pair_id) = match self.get_order_node(order_id) {
            Ok(o) => {
                let (owner, pair) = (o.get_owner_node_id(), o.get_pair_node_id());
                o.close();
                (owner, pair)
            }
            Err(e) => {
                eprintln!("ERROR: CloseOrder: {e}");
                return;
            }
        };

        if let Err(e) = self._transfer_all(order_id, owner_id) {
            eprintln!("WARNING: CloseOrder transfer_all failed: {e}");
        }
        match self.get_pair_node(pair_id) {
            Ok(p) => p.cancel_brief(order_id),
            Err(e) => eprintln!("ERROR: CloseOrder: {e}"),
        }
        // 更新 Account 订单簿
        if let Ok(account) = self.get_account_node(owner_id) {
            account.remove_order(order_id);
        }
        self.return_order(order_id);
    }

    pub fn send_msg(&mut self, msg: Msg) { self.msgs.push(msg); }

    pub fn get_tra_logs(&mut self, src_token: usize, dst_token: usize) -> Result<VecDeque<TraLog>> {
        let (pair_id, _) = self.get_or_create_pair(src_token, dst_token);
        Ok(self.get_pair_node(pair_id)?.get_tra_logs().clone())
    }

    pub fn get_candle_data(&mut self, src_token: usize, dst_token: usize, interval: u64) -> Result<VecDeque<CandleData>> {
        let (pair_id, _) = self.get_or_create_pair(src_token, dst_token);
        Ok(self.get_pair_node(pair_id)?.get_candle_data(interval))
    }

    pub fn latest_candle(&mut self, src_token: usize, dst_token: usize, interval: u64) -> Result<Option<CandleData>> {
        let (pair_id, _) = self.get_or_create_pair(src_token, dst_token);
        Ok(self.get_pair_node(pair_id)?.latest_candle(interval))
    }

    fn _update_order_for_pairs(&mut self, order_id: usize) -> Result<()> {
        if !self.registered_orders.contains(&order_id) {
            return Ok(());
        }

        // 读出 owner_id、pair_id、订单类型和状态
        let (owner_id, pair_id, is_swap, is_open) = {
            let order = self.get_order_node(order_id)?;
            (
                order.get_owner_node_id(),
                order.get_pair_node_id(),
                order.get_order_type() == &OrderType::Swap,
                OrderNode::is_open(order),
            )
        };

        // 更新 Pair 订单簿（仅限已开启的限价单）
        if is_open && !is_swap {
            let brief = self.get_order_brief(order_id)?;
            if let Ok(pair) = self.get_pair_node(pair_id) {
                pair.update_brief(brief.clone());
            }
        }

        // 更新 Account 订单簿
        if let Ok(account) = self.get_account_node(owner_id) {
            account.insert_order(order_id);
        }

        Ok(())
    }

    pub fn update(&mut self) {
        let mut all_msgs = take(&mut self.msgs);

        for node in &mut self.nodes {
            let msgs = node.upload_msgs(self.step_count);
            all_msgs.extend(msgs);
        }

        // === 第一遍：仅处理转账 ===
        // 所有转账处理完毕后余额才是最终状态，避免后续 _update_order_for_pairs 读到半成品余额。
        for msg in &all_msgs {
            match msg {
                Msg::Transfer { src_id, dst_id, token, volume, allow_negative } => {
                    if let Err(e) = self._transfer(*src_id, *dst_id, *token, *volume, *allow_negative) {
                        eprintln!("WARNING: transfer failed: {e}");
                    }
                    if let Err(e) = self._update_order_for_pairs(*src_id) {
                        eprintln!("WARNING: update_order_for_pairs (src) failed: {e}");
                    }
                    if let Err(e) = self._update_order_for_pairs(*dst_id) {
                        eprintln!("WARNING: update_order_for_pairs (dst) failed: {e}");
                    }
                }
                Msg::TransferAll { src_id, dst_id } => {
                    if let Err(e) = self._transfer_all(*src_id, *dst_id) {
                        eprintln!("WARNING: transfer_all failed: {e}");
                    }
                    if let Err(e) = self._update_order_for_pairs(*src_id) {
                        eprintln!("WARNING: update_order_for_pairs (src) failed: {e}");
                    }
                    if let Err(e) = self._update_order_for_pairs(*dst_id) {
                        eprintln!("WARNING: update_order_for_pairs (dst) failed: {e}");
                    }
                }
                _ => {}
            }
        }

        // === 第二遍：订单创建、撮合、关闭 ===
        use std::collections::HashMap;
        let mut committed: HashMap<(usize, usize), Decimal> = HashMap::new();
        for msg in all_msgs {
            match msg {
                Msg::Transfer { .. } | Msg::TransferAll { .. } => {
                    // 已在第一遍处理过
                }
                Msg::OpenOrder { src_id, owner_node_id, src_token, dst_token, volume, price } => {
                    if volume.is_zero() || price.is_zero() || volume > self.get_node_balance(src_id, src_token).unwrap() {
                        continue;
                    }
                    let new_order_id = self.new_order();
                    let (pair_node_id, _) = self.get_or_create_pair(src_token, dst_token);
                    // 初始化新 Order + 转账
                    let step_count_created = self.step_count;
                    let order = match self.get_order_node(new_order_id) {
                        Ok(o) => o,
                        Err(e) => {
                            eprintln!("ERROR: OpenOrder: {e}");
                            continue;
                        }
                    };
                    if !order.open(owner_node_id, pair_node_id, src_token, dst_token, price, step_count_created, OrderType::Make) {
                        eprintln!("ERROR: OpenOrder: order.open failed (owner={owner_node_id})");
                        continue;
                    }
                    if let Err(e) = self._transfer(src_id, new_order_id, src_token, volume, false) {
                        eprintln!("WARNING: OpenOrder transfer failed: {e}");
                    }
                    // 插入订单簿并触发撮合
                    if let Ok(brief) = self.get_order_brief(new_order_id) {
                        if let Ok(pair) = self.get_pair_node(pair_node_id) {
                            pair.insert_brief(brief.clone());
                        }
                        // 向 Owner Account 转发 brief
                        if let Ok(account) = self.get_account_node(owner_node_id) {
                            account.insert_order(new_order_id);
                        }
                    }
                }
                Msg::SwapOrder { src_id, owner_node_id, src_token, dst_token, volume } => {
                    let (pair_node_id, _) = self.get_or_create_pair(src_token, dst_token);

                    // 检查用户余额（考虑本帧已承诺的支出）
                    let available = {
                        let act = self.get_node_balance(src_id, src_token).unwrap_or(Decimal::ZERO);
                        let cmt = committed.get(&(src_id, src_token)).copied().unwrap_or(Decimal::ZERO);
                        if act <= cmt { Decimal::ZERO } else { act - cmt }
                    };
                    let volume = self.round(volume).min(available);
                    if volume.is_zero() {
                        continue;
                    }
                    // 记录本帧承诺支出（防止同帧多次 swap 透支）
                    let entry = committed.entry((src_id, src_token)).or_insert(Decimal::ZERO);
                    *entry += volume;

                    // 判断市价单方向
                    let direction = {
                        let pair = match self.get_pair_node(pair_node_id) {
                            Ok(p) => p,
                            Err(e) => { eprintln!("ERROR: SwapOrder: {e}"); continue; }
                        };
                        if pair.get_quote_token() == src_token && pair.get_base_token() == dst_token {
                            Drt::Buy
                        } else {
                            Drt::Sell
                        }
                    };

                    // 直接撮合（转账通过 pair 的消息队列延后到下一帧统一处理）
                    let pair = match self.get_pair_node(pair_node_id) {
                        Ok(p) => p,
                        Err(e) => { eprintln!("ERROR: SwapOrder: {e}"); continue; }
                    };
                    pair.process_swap(src_id, direction, volume);
                }
                Msg::CloseOrder { order_id } => {
                    self._close_order_immediate(order_id);
                }
            }
        }
        self.step_count += 1;
    }

    // ========== 类型化节点访问辅助方法 ==========

    fn get_pair_node(&mut self, id: usize) -> Result<&mut dyn PairNode> {
        if id >= self.nodes.len() {
            return Err(Error::NodeNotFound { id, len: self.nodes.len() });
        }
        self.nodes[id].as_pair_node().ok_or(Error::NotAPairNode(id))
    }

    fn get_order_node(&mut self, id: usize) -> Result<&mut dyn OrderNode> {
        if id >= self.nodes.len() {
            return Err(Error::NodeNotFound { id, len: self.nodes.len() });
        }
        self.nodes[id].as_order_node().ok_or(Error::NotAnOrderNode(id))
    }

    fn get_account_node(&mut self, id: usize) -> Result<&mut dyn AccountNode> {
        if id >= self.nodes.len() {
            return Err(Error::NodeNotFound { id, len: self.nodes.len() });
        }
        self.nodes[id].as_account_node().ok_or(Error::NotAnAccountNode(id))
    }

    fn _transfer(&mut self, src_id: usize, dst_id: usize, token: usize, volume: Decimal, allow_negative: bool) -> Result<()> {
        let volume = self.round(volume);
        if !self.token_id_to_name.contains_key(&token) {
            return Err(Error::TokenNotRegistered(token));
        }
        if src_id == dst_id {
            return Ok(());
        }
        if src_id >= self.nodes.len() || dst_id >= self.nodes.len() {
            return Err(Error::IndexOutOfBounds {
                id: src_id.max(dst_id),
                len: self.nodes.len(),
            });
        }

        let (left, right) = self.nodes.split_at_mut(src_id.max(dst_id));
        let (src, dst) = if src_id < dst_id {
            (&mut left[src_id], &mut right[0])
        } else {
            (&mut right[0], &mut left[dst_id])
        };

        let src_bal = src.balance(token);
        let dst_bal = dst.balance(token);

        if !allow_negative && src_bal < volume {
            return Err(Error::InsufficientBalance {
                node_id: src_id,
                token,
                has: src_bal,
                need: volume,
            });
        }
        if !allow_negative && dst_bal + volume < Decimal::ZERO {
            return Err(Error::NegativeDestination {
                node_id: dst_id,
                token,
                current: dst_bal,
                volume,
            });
        }

        src.set_balance(token, src_bal - volume);
        dst.set_balance(token, dst_bal + volume);
        Ok(())
    }

    fn _transfer_all(&mut self, src_id: usize, dst_id: usize) -> Result<()> {
        if src_id == dst_id { return Ok(()); }
        if src_id >= self.nodes.len() || dst_id >= self.nodes.len() {
            return Err(Error::IndexOutOfBounds {
                id: src_id.max(dst_id),
                len: self.nodes.len(),
            });
        }

        let (left, right) = self.nodes.split_at_mut(src_id.max(dst_id));
        let (src, dst) = if src_id < dst_id {
            (&mut left[src_id], &mut right[0])
        } else {
            (&mut right[0], &mut left[dst_id])
        };

        let balances = src.drain_balances();
        for (token, volume) in balances {
            let current = dst.balance(token);
            dst.set_balance(token, current + volume);
        }
        Ok(())
    }
}
