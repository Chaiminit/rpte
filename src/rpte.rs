use std::collections::{HashMap, HashSet};
use std::collections::VecDeque;
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use crate::error::{Error, Result};
use crate::node::{Node, Msg, Drt, OrderBookDepth, PairNode, OrderNode, AccountNode, ContractNode, ContractState, ContractFn, CalledFn, EngineReader};
use crate::token::Token;
use crate::pair::Pair;
use crate::account::Account;
use crate::contract::Contract;
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
    /// 合约主实例（独立存储，帧前 step() 调用 update_with_reader）
    contract_masters: Vec<Contract>,
    /// 从实例（位于 nodes 中）的回收池
    contract_pool: Vec<usize>,
    /// 当前活跃的从实例 ID 集合
    registered_contracts: HashSet<usize>,
    /// 从实例 node_id → 主实例索引
    slave_to_master: HashMap<usize, usize>,
    /// 代币虚拟锚定映射：token → anchor_token（1:1 价值绑定，用于查价）
    token_virtual_anchors: HashMap<usize, usize>,
    registered_accounts: HashSet<usize>,
    registered_pairs: HashSet<usize>,
    registered_token_pairs: HashMap<(usize, usize), usize>,
    step_count: u64,
    max_tra_log_length: usize,
    msgs: Vec<Msg>,
    running: bool,
    precision: u8,
}


impl EngineReader for Rpte {
    fn precision(&self) -> u8 { self.precision }
    fn global_quote_token(&self) -> usize { self.global_quote_token }

    fn get_token_name(&self, id: usize) -> Option<&str> {
        self.token_id_to_name.get(&id).map(|s| s.as_str())
    }

    fn get_token_by_name(&self, name: &str) -> Option<usize> {
        self.token_name_to_id.get(name).copied()
    }

    fn get_all_tokens(&self) -> Vec<usize> {
        self.token_id_to_name.keys().copied().collect()
    }

    fn get_all_accounts(&self) -> Vec<usize> {
        self.registered_accounts.iter().copied().collect()
    }

    fn node_balance(&self, node_id: usize, token: usize) -> Decimal {
        if node_id < self.nodes.len() {
            self.nodes[node_id].balance(token)
        } else {
            Decimal::ZERO
        }
    }

    fn get_current_price(&self, quote_token: usize, base_token: usize) -> Option<Decimal> {
        // 虚拟锚定检查：若一方是另一方的锚定代币，返回 1.0
        if self.token_virtual_anchors.get(&quote_token) == Some(&base_token)
            || self.token_virtual_anchors.get(&base_token) == Some(&quote_token)
        {
            return Some(Decimal::ONE);
        }
        // 查找已有交易对
        for &pid in &self.registered_pairs {
            if let Some(pair) = self.nodes[pid].as_pair_node_ref() {
                if pair.get_quote_token() == quote_token && pair.get_base_token() == base_token {
                    let price = pair.get_current_price();
                    return if price.is_zero() { None } else { Some(price) };
                }
            }
        }
        // 反向查找
        for &pid in &self.registered_pairs {
            if let Some(pair) = self.nodes[pid].as_pair_node_ref() {
                if pair.get_quote_token() == base_token && pair.get_base_token() == quote_token {
                    let price = pair.get_current_price();
                    return if price.is_zero() { None } else { Some(Decimal::ONE / price) };
                }
            }
        }
        None
    }

    fn price_between(&self, src: usize, dst: usize) -> Option<(Decimal, usize, usize)> {
        if let Some(price) = self.get_current_price(src, dst) {
            return Some((price, src, dst));
        }
        if let Some(price) = self.get_current_price(dst, src) {
            if !price.is_zero() {
                return Some((Decimal::ONE / price, dst, src));
            }
        }
        None
    }

    fn convert_value(&self, src: usize, dst: usize, amount: Decimal) -> Decimal {
        if amount.is_zero() || src == dst {
            return amount;
        }
        if let Some((price, quote, _base)) = self.price_between(src, dst) {
            if !price.is_zero() {
                return if src == quote { amount / price } else { amount * price };
            }
        }
        let quote = self.global_quote_token;
        if src != quote && dst != quote {
            let mid = self.convert_value(src, quote, amount);
            if !mid.is_zero() {
                return self.convert_value(quote, dst, mid);
            }
        }
        Decimal::ZERO
    }

    fn account_equity_token(&self, account_id: usize, token: usize) -> Decimal {
        if !self.registered_accounts.contains(&account_id) {
            return Decimal::ZERO;
        }
        let mut total = self.nodes[account_id].balance(token);
        if let Some(acct) = self.nodes[account_id].as_account_node_ref() {
            for &oid in acct.get_orders() {
                total += self.nodes[oid].balance(token);
            }
        }
        total
    }

    fn token_total_supply(&self, token: usize) -> Decimal {
        self.nodes.get(token)
            .and_then(|n| n.as_token_node_ref())
            .map(|t| t.total_supply())
            .unwrap_or(Decimal::ZERO)
    }

    fn token_can_be_negative(&self, token: usize) -> bool {
        self.nodes.get(token)
            .and_then(|n| n.as_token_node_ref())
            .map(|t| t.can_be_negative())
            .unwrap_or(false)
    }

    fn get_order_book(&self, quote_token: usize, base_token: usize, depth: usize) -> Vec<OrderBookDepth> {
        for &pid in &self.registered_pairs {
            if let Some(pair) = self.nodes[pid].as_pair_node_ref() {
                if pair.get_quote_token() == quote_token && pair.get_base_token() == base_token {
                    let buy = pair.get_order_book(Drt::Buy, depth);
                    let sell = pair.get_order_book(Drt::Sell, depth);
                    return vec![buy, sell];
                }
            }
        }
        Vec::new()
    }

    fn get_tra_logs(&self, quote_token: usize, base_token: usize) -> VecDeque<TraLog> {
        for &pid in &self.registered_pairs {
            if let Some(pair) = self.nodes[pid].as_pair_node_ref() {
                if pair.get_quote_token() == quote_token && pair.get_base_token() == base_token {
                    return pair.get_tra_logs().clone();
                }
            }
        }
        VecDeque::new()
    }

    fn get_candle_data(&self, quote_token: usize, base_token: usize, interval: u64) -> VecDeque<CandleData> {
        for &pid in &self.registered_pairs {
            if let Some(pair) = self.nodes[pid].as_pair_node_ref() {
                if pair.get_quote_token() == quote_token && pair.get_base_token() == base_token {
                    return pair.get_candle_data(interval);
                }
            }
        }
        VecDeque::new()
    }

    fn latest_candle(&self, quote_token: usize, base_token: usize, interval: u64) -> Option<CandleData> {
        for &pid in &self.registered_pairs {
            if let Some(pair) = self.nodes[pid].as_pair_node_ref() {
                if pair.get_quote_token() == quote_token && pair.get_base_token() == base_token {
                    return pair.latest_candle(interval);
                }
            }
        }
        None
    }
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
            contract_masters: Vec::new(),
            contract_pool: Vec::new(),
            registered_contracts: HashSet::new(),
            slave_to_master: HashMap::new(),
            token_virtual_anchors: HashMap::new(),
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

    /// 查询代币发行总量
    pub fn get_token_total_supply(&mut self, token: usize) -> Result<Decimal> {
        if !self.token_id_to_name.contains_key(&token) {
            return Err(Error::TokenNotRegistered(token));
        }
        let token_node = self.nodes.get_mut(token)
            .and_then(|n| n.as_token_node())
            .ok_or(Error::NotATokenNode(token))?;
        Ok(token_node.total_supply())
    }

    /// 设置代币是否允许负持仓
    pub fn set_token_can_be_negative(&mut self, token: usize, can: bool) -> Result<()> {
        if !self.token_id_to_name.contains_key(&token) {
            return Err(Error::TokenNotRegistered(token));
        }
        let token_node = self.nodes.get_mut(token)
            .and_then(|n| n.as_token_node())
            .ok_or(Error::NotATokenNode(token))?;
        token_node.set_can_be_negative(can);
        Ok(())
    }

    /// 查询代币是否允许负持仓
    pub fn get_token_can_be_negative(&self, token: usize) -> bool {
        self.nodes.get(token)
            .and_then(|n| n.as_token_node_ref())
            .map(|t| t.can_be_negative())
            .unwrap_or(false)
    }

    /// 检查两个代币是否允许组成交易对（白名单限制）
    pub fn is_swap_allowed(&self, a: usize, b: usize) -> bool {
        if a == b { return false; }
        let check = |token: usize, other: usize| -> bool {
            match self.nodes.get(token).and_then(|n| n.as_token_node_ref()) {
                Some(t) => {
                    if t.not_tradable() {
                        return false;
                    }
                    let wl = t.swap_whitelist();
                    wl.is_empty() || wl.contains(&other)
                }
                None => true,
            }
        };
        check(a, b) && check(b, a)
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

    /// 获取所有交易对的详细信息: (pair_id, quote_token, base_token, current_price)
    pub fn get_all_pairs_info(&mut self) -> Vec<(usize, usize, usize, Decimal)> {
        let ids: Vec<usize> = self.registered_pairs.iter().cloned().collect();
        let mut result = Vec::new();
        for id in ids {
            if let Some(pair) = self.nodes.get_mut(id).and_then(|n| n.as_pair_node()) {
                result.push((id, pair.get_quote_token(), pair.get_base_token(), pair.get_current_price()));
            }
        }
        result
    }

    /// 获取交易对的 quote 代币
    pub fn get_pair_quote_token(&mut self, pair_id: usize) -> Result<usize> {
        self.get_pair_node(pair_id).map(|p| p.get_quote_token())
    }

    /// 获取交易对的 base 代币
    pub fn get_pair_base_token(&mut self, pair_id: usize) -> Result<usize> {
        self.get_pair_node(pair_id).map(|p| p.get_base_token())
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

    /// 获取账户的完整权益 sheet（账户自身余额 + 所有挂单余额，按 token 汇总）。
    pub fn get_account_equity(&mut self, account_id: usize) -> Result<HashMap<usize, Decimal>> {
        if !self.registered_accounts.contains(&account_id) {
            return Err(Error::NodeNotFound { id: account_id, len: self.nodes.len() });
        }

        let tokens: Vec<usize> = self.token_id_to_name.keys().copied().collect();

        // 收集该账户下所有挂单 ID
        let order_ids: Vec<usize> = {
            let node = &mut self.nodes[account_id];
            let account = node.as_account_node().ok_or(Error::NotAnAccountNode(account_id))?;
            account.get_orders().iter().copied().collect()
        };

        let mut equity: HashMap<usize, Decimal> = HashMap::new();
        for &token in &tokens {
            let mut total = self.nodes[account_id].balance(token);
            for &oid in &order_ids {
                total += self.nodes[oid].balance(token);
            }
            if total != Decimal::ZERO {
                equity.insert(token, total);
            }
        }
        Ok(equity)
    }

    /// 获取账户在某个 token 上的权益量（账户自身余额 + 所有挂单中的该 token 余额）。
    pub fn get_account_equity_token(&mut self, account_id: usize, token: usize) -> Result<Decimal> {
        if !self.registered_accounts.contains(&account_id) {
            return Err(Error::NodeNotFound { id: account_id, len: self.nodes.len() });
        }
        if !self.token_id_to_name.contains_key(&token) {
            return Err(Error::TokenNotRegistered(token));
        }

        let order_ids: Vec<usize> = {
            let node = &mut self.nodes[account_id];
            let account = node.as_account_node().ok_or(Error::NotAnAccountNode(account_id))?;
            account.get_orders().iter().copied().collect()
        };

        let mut total = self.nodes[account_id].balance(token);
        for &oid in &order_ids {
            total += self.nodes[oid].balance(token);
        }
        Ok(total)
    }

    /// 获取当前价格。
    /// 返回 `(price, quote_token, base_token)`，含义为 1 base_token = price quote_token。
    pub fn get_current_price(&mut self, src_token: usize, dst_token: usize) -> Result<(Decimal, usize, usize)> {
        let (pair_id, _) = self.get_or_create_pair(src_token, dst_token)?;
        let pair = self.get_pair_node(pair_id)?;
        Ok((pair.get_current_price(), pair.get_quote_token(), pair.get_base_token()))
    }

    /// 获取订单簿深度。
    /// `src_token` 为支出代币，`dst_token` 为收入代币，由引擎自行推导买卖方向。
    pub fn get_order_book(&mut self, src_token: usize, dst_token: usize, depth: usize) -> Result<OrderBookDepth> {
        let (pair_id, is_forward) = self.get_or_create_pair(src_token, dst_token)?;
        // forward: src=quote, dst=base → Buy (支出 quote 买入 base)
        // reverse: src=base, dst=quote → Sell (卖出 base 获取 quote)
        let direction = if is_forward { Drt::Buy } else { Drt::Sell };
        Ok(self.get_pair_node(pair_id)?.get_order_book(direction, depth))
    }

    // ========== 账户操作代理方法 ==========

    /// 部署合约（通过 Msg::CreateContract）
    pub fn deploy(
        &mut self,
        owner_node_id: usize,
        on_create: ContractFn,
        on_update: ContractFn,
        on_end: ContractFn,
        on_called_fns: Vec<CalledFn>,
    ) {
        self.msgs.push(Msg::CreateContract {
            owner_node_id,
            on_create,
            on_update,
            on_end,
            on_called: on_called_fns,
        });
    }

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

    /// 调用合约
    pub fn call_contract(&mut self, src_id: usize, contract_id: usize, fn_id: u8, volume: impl Into<Decimal>) {
        let volume = self.round(volume.into());
        self.msgs.push(Msg::CallContract { src_id, contract_id, fn_id, volume });
    }

    /// 转账
    pub fn transfer(&mut self, src_id: usize, dst_id: usize, token: usize, volume: impl Into<Decimal>) {
        let volume = self.round(volume.into());
        self.msgs.push(Msg::Transfer {
            src_id,
            dst_id,
            token,
            volume,
        });
    }

    /// 取消订单
    pub fn cancel_order(&mut self, order_id: usize) {
        self.msgs.push(Msg::CloseOrder { order_id });
    }

    /// 根据 src_token/dst_token 查找或自动创建交易对
    /// 返回 (pair_id, is_forward)，is_forward 表示 src_token == pair.quote_token
    fn get_or_create_pair(&mut self, src_token: usize, dst_token: usize) -> Result<(usize, bool)> {
        // 1. 查缓存（两种方向）
        if let Some(&id) = self.registered_token_pairs.get(&(src_token, dst_token)) {
            return Ok((id, true));
        }
        if let Some(&id) = self.registered_token_pairs.get(&(dst_token, src_token)) {
            return Ok((id, false));
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
                return Ok((pair_id, true));
            }
            if qt == dst_token && bt == src_token {
                self.registered_token_pairs.insert((src_token, dst_token), pair_id);
                return Ok((pair_id, false));
            }
        }

        // 3. 白名单检查：只有不存在现成交易对时才检查
        if !self.is_swap_allowed(src_token, dst_token) {
            return Err(Error::SwapNotAllowed { src: src_token, dst: dst_token });
        }

        // 4. 自动创建新交易对
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
        Ok((pair_id, is_forward))
    }

    /// 截断到引擎精度
    pub fn round(&self, value: Decimal) -> Decimal {
        value.round_dp_with_strategy(self.precision as u32, RoundingStrategy::ToZero)
    }

    /// 通过交易对路径将 src_token 的数量换算为 dst_token 的数量。
    pub fn convert_value(&mut self, src_token: usize, dst_token: usize, amount: Decimal) -> Decimal {
        if amount.is_zero() || src_token == dst_token {
            return amount;
        }

        let pairs = self.get_all_pairs_info();
        let quote = self.global_quote_token;

        let try_pair = |pairs: &[(usize, usize, usize, Decimal)], src: usize, dst: usize, amt: Decimal| -> Option<Decimal> {
            for &(_, q, b, price) in pairs {
                if price.is_zero() {
                    continue;
                }
                if q == src && b == dst {
                    return Some(self.round(amt / price));
                }
                if q == dst && b == src {
                    return Some(self.round(amt * price));
                }
            }
            None
        };

        if let Some(result) = try_pair(&pairs, src_token, dst_token, amount) {
            return result;
        }

        if src_token != quote && dst_token != quote {
            if let Some(mid) = try_pair(&pairs, src_token, quote, amount) {
                if !mid.is_zero() {
                    if let Some(result) = try_pair(&pairs, quote, dst_token, mid) {
                        return result;
                    }
                }
            }
        }

        Decimal::ZERO
    }

    /// 将账户所有 token 的权益量（余额 + 挂单）换算为 dst_token 的数量。
    pub fn account_equity_value(&mut self, account_id: usize, dst_token: Option<usize>) -> Result<Decimal> {
        let dst = dst_token.unwrap_or(self.global_quote_token);
        let tokens: Vec<usize> = self.token_id_to_name.keys().copied().collect();
        let mut total = Decimal::ZERO;

        for &token in &tokens {
            let equity = self.get_account_equity_token(account_id, token)?;
            if equity.is_zero() {
                continue;
            }
            total += self.convert_value(token, dst, equity);
        }

        Ok(total)
    }

    /// 获取账户总资产：遍历所有 token，仅累加换算后为正值的部分。
    /// 例如持有 +100 USDT 和 -50 Claim_USDT（换算为 -49 USDT），
    /// 则总资产为 100，负债为 49，净资产为 51。
    pub fn get_account_assets(&mut self, account_id: usize, dst_token: Option<usize>) -> Result<Decimal> {
        let dst = dst_token.unwrap_or(self.global_quote_token);
        let tokens: Vec<usize> = self.token_id_to_name.keys().copied().collect();
        let mut total = Decimal::ZERO;

        for &token in &tokens {
            let equity = self.get_account_equity_token(account_id, token)?;
            if equity.is_zero() {
                continue;
            }
            let value = self.convert_value(token, dst, equity);
            if value > Decimal::ZERO {
                total += value;
            }
        }

        Ok(total)
    }

    /// 获取账户总负债：遍历所有 token，累加换算后为负值的部分的绝对值。
    pub fn get_account_liabilities(&mut self, account_id: usize, dst_token: Option<usize>) -> Result<Decimal> {
        let dst = dst_token.unwrap_or(self.global_quote_token);
        let tokens: Vec<usize> = self.token_id_to_name.keys().copied().collect();
        let mut total = Decimal::ZERO;

        for &token in &tokens {
            let equity = self.get_account_equity_token(account_id, token)?;
            if equity.is_zero() {
                continue;
            }
            let value = self.convert_value(token, dst, equity);
            if value < Decimal::ZERO {
                total += value.abs();
            }
        }

        Ok(total)
    }

    /// 发行资产到指定节点，同时更新对应代币的发行总量
    pub fn issue(&mut self, node_id: usize, token: usize, volume: impl Into<Decimal>) -> Result<()> {
        if node_id >= self.nodes.len() {
            return Err(Error::NodeNotFound { id: node_id, len: self.nodes.len() });
        }
        if !self.token_id_to_name.contains_key(&token) {
            return Err(Error::TokenNotRegistered(token));
        }
        let volume = self.round(volume.into());
        self.nodes[node_id].adjust_balance(token, volume);
        // 更新代币发行总量
        if let Some(token_node) = self.nodes.get_mut(token).and_then(|n| n.as_token_node()) {
            token_node.adjust_total_supply(volume);
        }
        Ok(())
    }

    pub fn new_order(&mut self) -> usize {
        if let Some(id) = self.order_pool.pop() {
            self.registered_orders.insert(id);
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

    // ========== 合约主从管理 ==========

    /// 创建一个新的合约主实例，同时创建从实例放入 nodes。
    /// 返回 (master_index, slave_node_id)
    fn create_master_slave(
        &mut self,
        owner_node_id: usize,
        on_create: ContractFn,
        on_update: ContractFn,
        on_end: ContractFn,
        on_called_fns: Vec<CalledFn>,
        step_count: u64,
    ) -> (usize, usize) {
        // 1. 创建主实例
        let mut master = Contract::new();
        let master_idx = self.contract_masters.len();
        master.deploy(owner_node_id, on_create, on_update, on_end, on_called_fns, step_count);

        // 2. 创建从实例（放入 nodes）
        let slave_id = self._new_contract_slot();
        // 从实例的 id 设为 slave_id（引擎中的 node_id）
        self.nodes[slave_id].set_id(slave_id);
        // 从实例的状态设为 Running（它不运行闭包）
        if let Some(slave) = self.nodes[slave_id].as_contract_node() {
            slave.deploy(owner_node_id,
                std::sync::Arc::new(|_, _, _| vec![]),  // 空操作
                std::sync::Arc::new(|_, _, _| vec![]),
                std::sync::Arc::new(|_, _, _| vec![]),
                Vec::new(),
                step_count,
            );
            // deploy 设置 state=Creating，但从实例的 update 是空操作
            // 所以手动设为 Running
            // 但由于没有办法直接 set_state，我们用"立即跑到 Running"的方式
            // 实际上，从实例的 update 是空操作，state=Creating 也无害
        }
        master.set_id(slave_id);

        // 3. 注册映射
        self.registered_contracts.insert(slave_id);
        self.slave_to_master.insert(slave_id, master_idx);
        self.contract_masters.push(master);

        (master_idx, slave_id)
    }

    /// 获取一个从实例 slot（从池中取或新建）
    fn _new_contract_slot(&mut self) -> usize {
        if let Some(id) = self.contract_pool.pop() {
            id
        } else {
            let contract = Contract::new();
            self.register_node(contract)
        }
    }

    /// 回收从实例到池中
    fn _return_contract_slot(&mut self, slave_id: usize) {
        self.contract_pool.push(slave_id);
    }

    fn _sync_master_to_slave(&mut self) {
        let pairs: Vec<(usize, usize)> = self.registered_contracts.iter()
            .filter_map(|&slave_id| {
                self.slave_to_master.get(&slave_id).map(|&mid| (mid, slave_id))
            })
            .collect();
        for (master_idx, slave_id) in pairs {
            if master_idx >= self.contract_masters.len() {
                continue;
            }
            let balances = self.contract_masters[master_idx].get_all_balances().clone();
            for (token, bal) in balances {
                self.nodes[slave_id].set_balance(token, bal);
            }
        }
    }

    fn _sync_slave_to_master(&mut self) {
        let pairs: Vec<(usize, usize)> = self.registered_contracts.iter()
            .filter_map(|&slave_id| {
                self.slave_to_master.get(&slave_id).map(|&mid| (mid, slave_id))
            })
            .collect();
        for (master_idx, slave_id) in pairs {
            if master_idx >= self.contract_masters.len() {
                continue;
            }
            let balances = self.nodes[slave_id].drain_balances();
            for (token, bal) in balances {
                self.contract_masters[master_idx].set_balance(token, bal);
            }
        }
    }

    fn _step_master_contracts(&mut self) -> Vec<Msg> {
        let mut msgs = Vec::new();
        let active: Vec<usize> = (0..self.contract_masters.len())
            .filter(|&i| self.contract_masters[i].state != ContractState::Destroyed)
            .collect();
        for master_idx in active {
            // 将主实例暂时 swap 出来，释放对 self.contract_masters 的可变借用
            let mut master = Contract::new();
            std::mem::swap(&mut master, &mut self.contract_masters[master_idx]);
            {
                let reader: &dyn EngineReader = self;
                master.update_with_reader(reader, self.step_count);
                msgs.extend(take(&mut master.msgs));
            } // reader dropped here
            // 将主实例放回
            self.contract_masters[master_idx] = master;
        }
        msgs
    }

    pub fn send_msg(&mut self, msg: Msg) { self.msgs.push(msg); }

    pub fn get_tra_logs(&mut self, src_token: usize, dst_token: usize) -> Result<VecDeque<TraLog>> {
        let (pair_id, _) = self.get_or_create_pair(src_token, dst_token)?;
        Ok(self.get_pair_node(pair_id)?.get_tra_logs().clone())
    }

    pub fn get_candle_data(&mut self, src_token: usize, dst_token: usize, interval: u64) -> Result<VecDeque<CandleData>> {
        let (pair_id, _) = self.get_or_create_pair(src_token, dst_token)?;
        Ok(self.get_pair_node(pair_id)?.get_candle_data(interval))
    }

    pub fn latest_candle(&mut self, src_token: usize, dst_token: usize, interval: u64) -> Result<Option<CandleData>> {
        let (pair_id, _) = self.get_or_create_pair(src_token, dst_token)?;
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
        // 收集初始消息（外部消息）
        let mut all_msgs = take(&mut self.msgs);

        // === 0. 帧前：运行所有主合约（master contracts），注入 EngineReader ===
        all_msgs.extend(self._step_master_contracts());

        // === 0b. 主 → 从同步（将主实例的余额复制到从实例） ===
        self._sync_master_to_slave();

        // === 0c. 从实例也参与 upload_msgs ===
        for node in &mut self.nodes {
            all_msgs.extend(node.upload_msgs(self.step_count));
        }

        // 按类型遍历 + 循环收敛：
        //  轮次内顺序：转账 → 关单 → 开单/市价单
        //  每轮结束后收集节点新产生的消息（match_orders 的 Transfer/CloseOrder），循环处理
        let mut converge_guard = 1000;
        loop {
            if all_msgs.is_empty() {
                break;
            }
            converge_guard -= 1;
            if converge_guard == 0 {
                break;
            }

            // === 1. 处理所有转账 ===
            let mut deferred: Vec<Msg> = Vec::new();
            for msg in take(&mut all_msgs) {
                match msg {
                    Msg::Transfer { src_id, dst_id, token, volume } => {
                        if let Err(e) = self._transfer(src_id, dst_id, token, volume) {
                            eprintln!("WARNING: transfer failed: {e}");
                        }
                        let _ = self._update_order_for_pairs(src_id);
                        let _ = self._update_order_for_pairs(dst_id);
                    }
                    Msg::TransferAll { src_id, dst_id } => {
                        if let Err(e) = self._transfer_all(src_id, dst_id) {
                            eprintln!("WARNING: transfer_all failed: {e}");
                        }
                        let _ = self._update_order_for_pairs(src_id);
                        let _ = self._update_order_for_pairs(dst_id);
                    }
                    other => deferred.push(other),
                }
            }

            // === 2. 处理所有关单 ===
            let mut remaining: Vec<Msg> = Vec::new();
            for msg in deferred {
                match msg {
                    Msg::CloseOrder { order_id } => {
                        if let Err(e) = self._close_order(order_id) {
                            eprintln!("WARNING: CloseOrder failed: {e}");
                        }
                    }
                    other => remaining.push(other),
                }
            }

            // === 3. 处理所有开单/市价单/创建合约 ===
            let mut swap_groups: HashMap<(usize, Drt), Vec<(usize, Decimal)>> = HashMap::new();
            for msg in remaining {
                match msg {
                    Msg::OpenOrder { src_id, owner_node_id, src_token, dst_token, volume, price } => {
                        let can_neg = self.get_token_can_be_negative(src_token);
                        if volume.is_zero() || price.is_zero() || (!can_neg && volume > self.get_node_balance(src_id, src_token).unwrap_or(Decimal::ZERO)) {
                            continue;
                        }
                        let (pair_node_id, _) = match self.get_or_create_pair(src_token, dst_token) {
                            Ok(v) => v,
                            Err(e) => {
                                eprintln!("WARNING: OpenOrder skipped: {e}");
                                continue;
                            }
                        };
                        let new_order_id = self.new_order();
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
                        if let Err(e) = self._transfer(src_id, new_order_id, src_token, volume) {
                            eprintln!("WARNING: OpenOrder transfer failed: {e}");
                        }
                        if let Ok(brief) = self.get_order_brief(new_order_id) {
                            if let Ok(pair) = self.get_pair_node(pair_node_id) {
                                pair.insert_brief(brief.clone());
                            }
                            if let Ok(account) = self.get_account_node(owner_node_id) {
                                account.insert_order(new_order_id);
                            }
                        }
                    }
                    Msg::SwapOrder { src_id: _, owner_node_id, src_token, dst_token, volume } => {
                        let (pair_node_id, _) = match self.get_or_create_pair(src_token, dst_token) {
                            Ok(v) => v,
                            Err(e) => {
                                eprintln!("WARNING: SwapOrder skipped: {e}");
                                continue;
                            }
                        };

                        // 检查余额（可为负的代币不受余额限制）
                        let balance = match self.get_node_balance(owner_node_id, src_token) {
                            Ok(b) => b,
                            Err(e) => {
                                eprintln!("ERROR: SwapOrder balance check failed: {e}");
                                continue;
                            }
                        };
                        let can_neg = self.get_token_can_be_negative(src_token);
                        let volume = if can_neg { volume } else { volume.min(balance) };
                        if volume.is_zero() {
                            continue;
                        }

                        // 判断方向
                        let direction = {
                            let pair = match self.get_pair_node(pair_node_id) {
                                Ok(p) => p,
                                Err(e) => {
                                    eprintln!("ERROR: SwapOrder: {e}");
                                    continue;
                                }
                            };
                            if pair.get_quote_token() == src_token && pair.get_base_token() == dst_token {
                                Drt::Buy
                            } else {
                                Drt::Sell
                            }
                        };

                        swap_groups
                            .entry((pair_node_id, direction))
                            .or_default()
                            .push((owner_node_id, volume));
                    }
                    Msg::CreateContract { owner_node_id, on_create, on_update, on_end, on_called } => {
                        let step = self.step_count;
                        self.create_master_slave(owner_node_id, on_create, on_update, on_end, on_called, step);
                    }
                    Msg::RegisterToken { name, can_be_negative, not_tradable, virtual_anchor, swap_whitelist } => {
                        let id = self.register_token(&name);
                        if can_be_negative {
                            let _ = self.set_token_can_be_negative(id, true);
                        }
                        if not_tradable {
                            if let Some(tn) = self.nodes.get_mut(id).and_then(|n| n.as_token_node()) {
                                tn.set_not_tradable(true);
                            }
                        }
                        if let Some(anchor) = virtual_anchor {
                            self.token_virtual_anchors.insert(id, anchor);
                        }
                        if !swap_whitelist.is_empty() {
                            let wl: HashSet<usize> = swap_whitelist.into_iter().collect();
                            if let Some(tn) = self.nodes.get_mut(id).and_then(|n| n.as_token_node()) {
                                tn.set_swap_whitelist(wl);
                            }
                        }
                    }
                    Msg::Transfer { .. } | Msg::TransferAll { .. } | Msg::CloseOrder { .. } => {
                        // 不应到达此处
                    }
                    Msg::CallContract { src_id, contract_id, fn_id, volume } => {
                        // 查找合约从实例，如果存在 on_called 则执行
                        let is_contract = self.registered_contracts.contains(&contract_id);
                        if !is_contract {
                            continue;
                        }
                        // 查找主实例索引
                        let master_idx = match self.slave_to_master.get(&contract_id) {
                            Some(&idx) => idx,
                            None => continue,
                        };
                        // 临时取出 master 以解除对 self 的借用冲突
                        let mut master = self.contract_masters.remove(master_idx);
                        let msgs = master.call(self, src_id, fn_id, volume);
                        self.contract_masters.insert(master_idx, master);
                        self.msgs.extend(msgs);
                    }
                    Msg::Issue { token, account_id, volume } => {
                        let _ = self.issue(account_id, token, volume);
                    }
                }
            }

            // === 3b. 批量处理按比例分配的市价单 ===
            for ((pair_node_id, direction), swaps) in swap_groups {
                if swaps.len() == 1 {
                    let (owner_id, volume) = swaps[0];
                    let (transfers, close_ids) = {
                        let pair = match self.get_pair_node(pair_node_id) {
                            Ok(p) => p,
                            Err(e) => {
                                eprintln!("ERROR: SwapOrder: {e}");
                                continue;
                            }
                        };
                        pair.process_swap(owner_id, direction, volume)
                    };

                    for t in transfers {
                        if let Err(e) = self._transfer(t.src_id, t.dst_id, t.token, t.volume) {
                            eprintln!("WARNING: Swap transfer failed: {e}");
                        }
                        let _ = self._update_order_for_pairs(t.src_id);
                        let _ = self._update_order_for_pairs(t.dst_id);
                    }

                    for order_id in close_ids {
                        self.msgs.push(Msg::CloseOrder { order_id });
                    }
                } else {
                    let (transfers, close_ids) = {
                        let pair = match self.get_pair_node(pair_node_id) {
                            Ok(p) => p,
                            Err(e) => {
                                eprintln!("ERROR: SwapOrder batch: {e}");
                                continue;
                            }
                        };
                        pair.process_swaps_batch(direction, &swaps)
                    };

                    for t in transfers {
                        if let Err(e) = self._transfer(t.src_id, t.dst_id, t.token, t.volume) {
                            eprintln!("WARNING: Swap transfer failed: {e}");
                        }
                        let _ = self._update_order_for_pairs(t.src_id);
                        let _ = self._update_order_for_pairs(t.dst_id);
                    }

                    for order_id in close_ids {
                        self.msgs.push(Msg::CloseOrder { order_id });
                    }
                }
            }

            // === 4. 收集节点新产生的消息（match_orders 的 Transfer/CloseOrder） ===
            for node in &mut self.nodes {
                all_msgs.extend(node.upload_msgs(self.step_count));
            }
        }

        // === 5. 从 → 主同步（将余额变化写回主实例） ===
        self._sync_slave_to_master();

        self.step_count += 1;

        // === 6. 回收销毁态的合约（从实例回池，主实例保留） ===
        let mut destroyed = Vec::new();
        for &slave_id in &self.registered_contracts {
            if let Some(&master_idx) = self.slave_to_master.get(&slave_id) {
                if master_idx < self.contract_masters.len()
                    && self.contract_masters[master_idx].state == ContractState::Destroyed
                {
                    destroyed.push(slave_id);
                }
            }
        }
        for slave_id in destroyed {
            self.registered_contracts.remove(&slave_id);
            self.slave_to_master.remove(&slave_id);
            self._return_contract_slot(slave_id);
        }
    }

    fn _close_order(&mut self, order_id: usize) -> Result<()> {
        if !self.registered_orders.remove(&order_id) {
            return Ok(());
        }
        let (owner_id, pair_id) = match self.get_order_node(order_id) {
            Ok(o) => {
                let (owner, pair) = (o.get_owner_node_id(), o.get_pair_node_id());
                o.close();
                (owner, pair)
            }
            Err(e) => return Err(e),
        };

        self._transfer_all(order_id, owner_id)?;
        if let Ok(pair) = self.get_pair_node(pair_id) {
            pair.cancel_brief(order_id);
        }
        // 更新 Account 订单簿
        if let Ok(account) = self.get_account_node(owner_id) {
            account.remove_order(order_id);
        }
        self.return_order(order_id);
        Ok(())
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

    fn _transfer(&mut self, src_id: usize, dst_id: usize, token: usize, volume: Decimal) -> Result<()> {
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

        // 查询 token 是否允许负持仓（在 split_at_mut 之前获取）
        let can_negative = self.check_token_can_be_negative(token).unwrap_or(false);

        let (left, right) = self.nodes.split_at_mut(src_id.max(dst_id));
        let (src, dst) = if src_id < dst_id {
            (&mut left[src_id], &mut right[0])
        } else {
            (&mut right[0], &mut left[dst_id])
        };

        let src_bal = src.balance(token);
        let dst_bal = dst.balance(token);

        if !can_negative && src_bal < volume {
            return Err(Error::InsufficientBalance {
                node_id: src_id,
                token,
                has: src_bal,
                need: volume,
            });
        }
        if !can_negative && dst_bal + volume < Decimal::ZERO {
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

    fn check_token_can_be_negative(&mut self, token: usize) -> Result<bool> {
        if !self.token_id_to_name.contains_key(&token) {
            return Err(Error::TokenNotRegistered(token));
        }
        let token_node = self.nodes.get_mut(token)
            .and_then(|n| n.as_token_node())
            .ok_or(Error::NotATokenNode(token))?;
        Ok(token_node.can_be_negative())
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