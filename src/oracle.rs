use std::collections::HashMap;
use rust_decimal::Decimal;

/// 帧级预言机：为合约提供只读的引擎状态快照。
///
/// 每帧开始时由引擎构建，注入到所有活跃合约中。
/// 价格数据预填充（O(pairs)），余额全量快照（O(nodes * tokens)）。
#[derive(Clone, Debug)]
pub struct Oracle {
    /// 所有交易对价格快照: (quote_token, base_token) -> price
    prices: HashMap<(usize, usize), Decimal>,
    /// 所有节点的余额快照: node_id -> (token -> balance)
    balances: HashMap<usize, HashMap<usize, Decimal>>,
    /// 全局计价代币 ID
    pub global_quote_token: usize,
}

impl Oracle {
    pub fn new(global_quote_token: usize) -> Self {
        Self {
            prices: HashMap::new(),
            balances: HashMap::new(),
            global_quote_token,
        }
    }

    /// 获取交易对价格（正向：1 base = price quote）
    pub fn price(&self, quote_token: usize, base_token: usize) -> Option<Decimal> {
        self.prices.get(&(quote_token, base_token)).copied()
    }

    /// 获取任意两个代币之间的价格（自动判断方向）。
    /// 返回 (price, quote_token, base_token)，含义为 1 base = price quote。
    pub fn price_between(&self, src_token: usize, dst_token: usize) -> Option<(Decimal, usize, usize)> {
        // 正向：src=quote, dst=base
        if let Some(&price) = self.prices.get(&(src_token, dst_token)) {
            return Some((price, src_token, dst_token));
        }
        // 反向：src=base, dst=quote → 价格取倒数
        if let Some(&price) = self.prices.get(&(dst_token, src_token)) {
            if !price.is_zero() {
                return Some((Decimal::ONE / price, dst_token, src_token));
            }
        }
        None
    }

    /// 查询节点余额快照
    pub fn balance(&self, node_id: usize, token: usize) -> Decimal {
        self.balances
            .get(&node_id)
            .and_then(|m| m.get(&token))
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    /// 查询节点的全部余额快照
    pub fn balances_of(&self, node_id: usize) -> Option<&HashMap<usize, Decimal>> {
        self.balances.get(&node_id)
    }

    /// 将指定代币数量换算为目标代币数量。
    /// 先尝试直接交易对，再尝试通过全局计价代币中转。
    pub fn convert(&self, src_token: usize, dst_token: usize, amount: Decimal) -> Decimal {
        if amount.is_zero() || src_token == dst_token {
            return amount;
        }
        // 直接交易对
        if let Some((price, quote, _base)) = self.price_between(src_token, dst_token) {
            if !price.is_zero() {
                if src_token == quote {
                    return amount / price; // quote → base
                } else {
                    return amount * price; // base → quote
                }
            }
        }
        // 中转
        let quote = self.global_quote_token;
        if src_token != quote && dst_token != quote {
            let mid = self.convert(src_token, quote, amount);
            if !mid.is_zero() {
                return self.convert(quote, dst_token, mid);
            }
        }
        Decimal::ZERO
    }

    /// 填充所有交易对价格（由引擎调用）
    pub(crate) fn set_prices(&mut self, prices: HashMap<(usize, usize), Decimal>) {
        self.prices = prices;
    }

    /// 填充所有节点余额（由引擎调用）
    pub(crate) fn set_balances(&mut self, balances: HashMap<usize, HashMap<usize, Decimal>>) {
        self.balances = balances;
    }
}
