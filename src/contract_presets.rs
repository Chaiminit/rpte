//! 合约预设：提供常见 DeFi 合约的预制模板。
//!
//! 每个预设返回一组 `(on_create, on_update, on_end, vec![on_called...])`，
//! 可直接传入 `Rpte::deploy()`。

use std::sync::Arc;
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use rust_decimal::prelude::ToPrimitive;
use crate::node::{Node, Msg, EngineReader, ContractFn, CalledFn};
use crate::contract::Contract;

// ═══════════════════════════════════════════════════════════════
//  借贷合约预设（Lending Contract Preset）— 双向模型
// ═══════════════════════════════════════════════════════════════
//
// 模型：双池交叉质押借贷（浮动汇率 + 负债凭证）
//
//   代币体系（on_create 自动注册）：
//     - aUSDT（资产池 A 凭证）：浮动汇率 = 池 A 总价值 / 发行量
//     - aBTC（资产池 B 凭证）：浮动汇率 = 池 B 总价值 / 发行量
//     - dUSDT（负债凭证 A）：始终 1:1，负值 = 欠 USDT
//     - dBTC（负债凭证 B）：始终 1:1，负值 = 欠 BTC
//
//   四个 on_called 函数：
//     0: aUSDT/USDT    池 A 存款/取款
//     1: aBTC/BTC      池 B 存款/取款
//     2: dUSDT/USDT    USDT 借款/还款（交叉质押）
//     3: dBTC/BTC      BTC 借款/还款（交叉质押）
//
//   交叉质押：质押价值 = aUSDT 池价值 + aBTC 池价值，总负债 = dUSDT + dBTC 换算
//
//   on_update：
//     - 按利用率生成双池动态利率
//     - 向借款人铸造更多负 dUSDT/dBTC 计息
//     - 交叉质押清算（总权益 vs 总负债）
// ═══════════════════════════════════════════════════════════════

/// 合约内置状态 sentinel key：aUSDT ID 缓存
const KEY_ASSET_RECEIPT_ID: usize = usize::MAX - 1;
/// 合约内置状态 sentinel key：aBTC ID 缓存
const KEY_COLLATERAL_RECEIPT_ID: usize = usize::MAX - 2;
/// 合约内置状态 sentinel key：dUSDT ID 缓存
const KEY_DEBT_RECEIPT_ID: usize = usize::MAX - 3;
/// 合约内置状态 sentinel key：dBTC ID 缓存
const KEY_DEBT_RECEIPT_ID_B: usize = usize::MAX - 4;
/// 合约报价存储 key base：PRICE_BASE + fn_id → 该 fn 对应的虚拟交易对报价
const PRICE_BASE: usize = usize::MAX / 4;

/// 双向借贷合约配置参数
#[derive(Clone)]
pub struct LendingPreset {
    /// 资产代币 A（如 USDT）
    pub asset_token_a: usize,
    /// 资产代币 B（如 BTC）
    pub asset_token_b: usize,
    /// 存款凭证 A 名称（如 aUSDT）
    pub receipt_a_name: String,
    /// 存款凭证 B 名称（如 aBTC）
    pub receipt_b_name: String,
    /// 负债凭证 A 名称（如 dUSDT）
    pub debt_a_name: String,
    /// 负债凭证 B 名称（如 dBTC）
    pub debt_b_name: String,
    /// 最低质押率（如 1.50 = 150%）
    pub min_collateral_ratio: Decimal,
    /// 清算阈值（低于此值触发清算，如 1.20 = 120%）
    pub liquidation_threshold: Decimal,
    // ── 利率模型参数（共用，按各自池利用率计算） ──
    pub optimal_utilization: Decimal,
    pub base_rate: Decimal,
    pub slope1: Decimal,
    pub slope2: Decimal,
}

// ── 内部辅助函数 ──

/// 从合约 sheet 缓存中读取凭证代币 ID，未命中时按名称查询并缓存。
fn cached_receipt_id(
    contract: &mut Contract,
    reader: &dyn EngineReader,
    name: &str,
    cache_key: usize,
) -> Option<usize> {
    let cached = contract.balance(cache_key);
    if cached > Decimal::ZERO {
        return cached.to_u64().map(|id| id as usize);
    }
    let id = reader.get_token_by_name(name)?;
    contract.set_balance(cache_key, Decimal::new(id as i64, 0));
    Some(id)
}

impl LendingPreset {
    /// 创建双向借贷合约预设。
    pub fn new_bidirectional(
        asset_token_a: usize,
        asset_token_b: usize,
        receipt_a_name: &str,
        receipt_b_name: &str,
        debt_a_name: &str,
        debt_b_name: &str,
        min_collateral_ratio: Decimal,
        liquidation_threshold: Decimal,
    ) -> Self {
        Self {
            asset_token_a,
            asset_token_b,
            receipt_a_name: receipt_a_name.to_string(),
            receipt_b_name: receipt_b_name.to_string(),
            debt_a_name: debt_a_name.to_string(),
            debt_b_name: debt_b_name.to_string(),
            min_collateral_ratio,
            liquidation_threshold,
            optimal_utilization: Decimal::new(8, 1),  // 0.8
            base_rate:          Decimal::new(28, 7),  // 0.00000028 → ~5%/小时
            slope1:             Decimal::new(28, 7),  // 0.00000028 → 最优(80%利用率)时总计~10%/小时
            slope2:             Decimal::new(15, 6),  // 0.0000015  → 100%利用率时总计~37%/小时
        }
    }

    /// 构建合约行为函数，返回 `(on_create, on_update, on_end, on_called_fns)`。
    pub fn build(&self) -> (ContractFn, ContractFn, ContractFn, Vec<CalledFn>) {
        // ── on_create：注册四种凭证代币 + 创建 4 个虚拟交易对 ──
        let p_create = self.clone();
        let on_create: ContractFn = Arc::new(move |_contract: &mut Contract, _reader: &dyn EngineReader, _step: u64| -> Vec<Msg> {
            vec![
                // 存款凭证 A（aUSDT）
                Msg::RegisterToken {
                    name: p_create.receipt_a_name.clone(),
                    can_be_negative: false,
                },
                // 存款凭证 B（aBTC）
                Msg::RegisterToken {
                    name: p_create.receipt_b_name.clone(),
                    can_be_negative: false,
                },
                // 负债凭证 A（dUSDT）
                Msg::RegisterToken {
                    name: p_create.debt_a_name.clone(),
                    can_be_negative: true,
                },
                // 负债凭证 B（dBTC）
                Msg::RegisterToken {
                    name: p_create.debt_b_name.clone(),
                    can_be_negative: true,
                },
                // fn_id=0: aUSDT/USDT 虚拟对（浮动汇率，允许限价单）
                Msg::CreateVirtualPair {
                    contract_slave_id: _contract.id,
                    fn_id: 0,
                    quote_token: p_create.asset_token_a,
                    base_token_name: p_create.receipt_a_name.clone(),
                    swap_only: false,
                },
                // fn_id=1: aBTC/BTC 虚拟对（浮动汇率，允许限价单）
                Msg::CreateVirtualPair {
                    contract_slave_id: _contract.id,
                    fn_id: 1,
                    quote_token: p_create.asset_token_b,
                    base_token_name: p_create.receipt_b_name.clone(),
                    swap_only: false,
                },
                // fn_id=2: dUSDT/USDT 虚拟对（固定 1:1，只许市价单）
                Msg::CreateVirtualPair {
                    contract_slave_id: _contract.id,
                    fn_id: 2,
                    quote_token: p_create.asset_token_a,
                    base_token_name: p_create.debt_a_name.clone(),
                    swap_only: true,
                },
                // fn_id=3: dBTC/BTC 虚拟对（固定 1:1，只许市价单）
                Msg::CreateVirtualPair {
                    contract_slave_id: _contract.id,
                    fn_id: 3,
                    quote_token: p_create.asset_token_b,
                    base_token_name: p_create.debt_b_name.clone(),
                    swap_only: true,
                },
            ]
        });

        // ── on_update：利率生成、利息结算（双池）、交叉质押清算 ──
        let p_up = self.clone();
        let on_update: ContractFn = Arc::new(move |contract: &mut Contract, reader: &dyn EngineReader, _step: u64| -> Vec<Msg> {
            let prec = reader.precision();
            let round = |v: Decimal| v.round_dp_with_strategy(prec as u32, RoundingStrategy::ToZero);
            let mut msgs = Vec::new();

            let token_a = p_up.asset_token_a;
            let token_b = p_up.asset_token_b;

            // 读取凭证代币 ID
            let receipt_a = match cached_receipt_id(contract, reader, &p_up.receipt_a_name, KEY_ASSET_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let receipt_b = match cached_receipt_id(contract, reader, &p_up.receipt_b_name, KEY_COLLATERAL_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let debt_a = match cached_receipt_id(contract, reader, &p_up.debt_a_name, KEY_DEBT_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let debt_b = match cached_receipt_id(contract, reader, &p_up.debt_b_name, KEY_DEBT_RECEIPT_ID_B) {
                Some(id) => id, None => return Vec::new(),
            };

            // ── 1. 双池利率计算与利息结算 ──
            let pool_a_cash = contract.balance(token_a);
            let pool_b_cash = contract.balance(token_b);

            let debt_a_total_supply = reader.token_total_supply(debt_a);
            let debt_b_total_supply = reader.token_total_supply(debt_b);
            let borrowed_a = if debt_a_total_supply < Decimal::ZERO { -debt_a_total_supply } else { Decimal::ZERO };
            let borrowed_b = if debt_b_total_supply < Decimal::ZERO { -debt_b_total_supply } else { Decimal::ZERO };

            // 池 A 利率
            let pool_a_total = pool_a_cash + borrowed_a;
            if !pool_a_total.is_zero() && !borrowed_a.is_zero() {
                let util = borrowed_a / pool_a_total;
                let rate = p_up._calc_rate(util);
                let all_accounts = reader.get_all_accounts();
                for &acc in &all_accounts {
                    let bal = reader.node_balance(acc, debt_a);
                    if bal >= Decimal::ZERO { continue; }
                    let debt = -bal;
                    let interest = round(debt * rate).max(Decimal::new(1, prec as u32));
                    if interest > Decimal::ZERO {
                        msgs.push(Msg::Issue { token: debt_a, account_id: acc, volume: -interest });
                    }
                }
            }

            // 池 B 利率
            let pool_b_total = pool_b_cash + borrowed_b;
            if !pool_b_total.is_zero() && !borrowed_b.is_zero() {
                let util = borrowed_b / pool_b_total;
                let rate = p_up._calc_rate(util);
                let all_accounts = reader.get_all_accounts();
                for &acc in &all_accounts {
                    let bal = reader.node_balance(acc, debt_b);
                    if bal >= Decimal::ZERO { continue; }
                    let debt = -bal;
                    let interest = round(debt * rate).max(Decimal::new(1, prec as u32));
                    if interest > Decimal::ZERO {
                        msgs.push(Msg::Issue { token: debt_b, account_id: acc, volume: -interest });
                    }
                }
            }

            // ── 2. 交叉质押清算 ──
            // 遍历有负债的账户，计算总质押价值 vs 总负债
            let all_accounts = reader.get_all_accounts();
            for &acc in &all_accounts {
                let d_a_bal = reader.node_balance(acc, debt_a);
                let d_b_bal = reader.node_balance(acc, debt_b);
                if d_a_bal >= Decimal::ZERO && d_b_bal >= Decimal::ZERO {
                    continue; // 无任何负债
                }
                let debt_a_amt = if d_a_bal < Decimal::ZERO { -d_a_bal } else { Decimal::ZERO };
                let debt_b_amt = if d_b_bal < Decimal::ZERO { -d_b_bal } else { Decimal::ZERO };

                // 计算总负债（以 USDT 计价）
                let debt_b_in_usdt = reader.convert_value(token_b, token_a, debt_b_amt);
                let total_debt = debt_a_amt + debt_b_in_usdt;
                if total_debt <= Decimal::ZERO { continue; }

                // 计算总质押价值 = aUSDT 池份额 + aBTC 池份额（以 USDT 计价）
                let r_a_bal = reader.node_balance(acc, receipt_a);
                let r_b_bal = reader.node_balance(acc, receipt_b);

                let r_a_supply = reader.token_total_supply(receipt_a);
                let r_b_supply = reader.token_total_supply(receipt_b);

                let a_share = if r_a_supply.is_zero() { Decimal::ZERO }
                    else { (pool_a_cash + borrowed_a) * r_a_bal / r_a_supply };
                let b_share = if r_b_supply.is_zero() { Decimal::ZERO }
                    else { (pool_b_cash + borrowed_b) * r_b_bal / r_b_supply };

                let a_share_in_usdt = reader.convert_value(token_a, token_a, a_share); // 本身就是 USDT
                let b_share_in_usdt = reader.convert_value(token_b, token_a, b_share);
                let total_collateral = a_share_in_usdt + b_share_in_usdt;

                if total_collateral <= Decimal::ZERO { continue; }

                let health = total_collateral / total_debt;
                if health >= p_up.liquidation_threshold { continue; }

                // ── 触发清算：单池独立模型 ──
                // 各池独立清算，没收的质押品留在池中，负债清零后池子承担亏损
                // 交叉质押的意义：允许用任一池质押品借任一池资产，但清算时各池盈亏自担

                // 销毁所有存款凭证（质押品留在池中，由存款人共享）
                if r_a_bal > Decimal::ZERO {
                    msgs.push(Msg::Issue { token: receipt_a, account_id: acc, volume: -r_a_bal });
                }
                if r_b_bal > Decimal::ZERO {
                    msgs.push(Msg::Issue { token: receipt_b, account_id: acc, volume: -r_b_bal });
                }

                // 清零所有负债（池子承担亏损，如果有差额）
                if d_a_bal < Decimal::ZERO {
                    msgs.push(Msg::Issue { token: debt_a, account_id: acc, volume: -d_a_bal });
                }
                if d_b_bal < Decimal::ZERO {
                    msgs.push(Msg::Issue { token: debt_b, account_id: acc, volume: -d_b_bal });
                }
            }

            // ── 3. 写入虚拟交易对报价 ──
            // fn_id=0: aUSDT 汇率
            let r_a_supply = reader.token_total_supply(receipt_a);
            let rate_a = if r_a_supply.is_zero() { Decimal::ONE }
            else {
                let pv = pool_a_cash + borrowed_a;
                if pv.is_zero() { Decimal::ONE } else { pv / r_a_supply }
            };
            contract.set_balance(PRICE_BASE, rate_a);

            // fn_id=1: aBTC 汇率
            let r_b_supply = reader.token_total_supply(receipt_b);
            let rate_b = if r_b_supply.is_zero() { Decimal::ONE }
            else {
                let pv = pool_b_cash + borrowed_b;
                if pv.is_zero() { Decimal::ONE } else { pv / r_b_supply }
            };
            contract.set_balance(PRICE_BASE + 1, rate_b);

            // fn_id=2: dUSDT/USDT = 1:1
            contract.set_balance(PRICE_BASE + 2, Decimal::ONE);

            // fn_id=3: dBTC/BTC = 1:1
            contract.set_balance(PRICE_BASE + 3, Decimal::ONE);

            msgs
        });

        // ── on_end：合约结束，无特殊操作 ──
        let on_end: ContractFn = Arc::new(move |_contract: &mut Contract, _reader: &dyn EngineReader, _step: u64| -> Vec<Msg> {
            Vec::new()
        });

        // ── on_called[0]：aUSDT ↔ USDT 存款/取款（浮动汇率） ──
        let p0 = self.clone();
        let exchange_a: CalledFn = Arc::new(move |contract: &mut Contract, reader: &dyn EngineReader, caller_id: usize, volume: Decimal| -> Vec<Msg> {
            if volume.is_zero() { return Vec::new(); }
            let receipt_a = match cached_receipt_id(contract, reader, &p0.receipt_a_name, KEY_ASSET_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let debt_a = match cached_receipt_id(contract, reader, &p0.debt_a_name, KEY_DEBT_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let prec = reader.precision();
            let round = |v: Decimal| v.round_dp_with_strategy(prec as u32, RoundingStrategy::ToZero);
            let token = p0.asset_token_a;
            let mut res = Vec::new();

            // 计算 aUSDT 汇率
            let pool_cash = contract.balance(token);
            let d_supply = reader.token_total_supply(debt_a);
            let borrowed = if d_supply < Decimal::ZERO { -d_supply } else { Decimal::ZERO };
            let r_supply = reader.token_total_supply(receipt_a);
            let rate = if r_supply.is_zero() { Decimal::ONE }
            else {
                let pv = pool_cash + borrowed;
                if pv.is_zero() { Decimal::ONE } else { pv / r_supply }
            };

            if volume > Decimal::ZERO {
                // 存款：转入 volume USDT，铸造 aUSDT
                let caller_bal = reader.node_balance(caller_id, token);
                if caller_bal < volume { return Vec::new(); }
                let receipt_amt = if rate.is_zero() { volume } else { round(volume / rate) };
                if receipt_amt <= Decimal::ZERO { return Vec::new(); }
                res.push(Msg::Transfer { src_id: caller_id, dst_id: contract.id, token, volume });
                res.push(Msg::Issue { token: receipt_a, account_id: caller_id, volume: receipt_amt });
            } else {
                // 取款：销毁 aUSDT，取出 USDT
                let withdraw = -volume;
                let caller_receipt = reader.node_balance(caller_id, receipt_a);
                let max_w = if rate.is_zero() { caller_receipt } else { round(caller_receipt * rate) };
                let mut actual = withdraw.min(max_w);
                let pool_bal = contract.balance(token);
                actual = actual.min(pool_bal);
                if actual <= Decimal::ZERO { return Vec::new(); }

                // 取出后保证总质押率满足要求（交叉质押检查）
                let debt_b = match cached_receipt_id(contract, reader, &p0.debt_b_name, KEY_DEBT_RECEIPT_ID_B) {
                    Some(id) => id, None => return Vec::new(),
                };
                let receipt_b = match cached_receipt_id(contract, reader, &p0.receipt_b_name, KEY_COLLATERAL_RECEIPT_ID) {
                    Some(id) => id, None => return Vec::new(),
                };
                let d_a_bal = reader.node_balance(caller_id, debt_a);
                let d_b_bal = reader.node_balance(caller_id, debt_b);
                let d_a_amt = if d_a_bal < Decimal::ZERO { -d_a_bal } else { Decimal::ZERO };
                let d_b_amt = if d_b_bal < Decimal::ZERO { -d_b_bal } else { Decimal::ZERO };
                let total_debt = d_a_amt + reader.convert_value(p0.asset_token_b, p0.asset_token_a, d_b_amt);
                if total_debt > Decimal::ZERO {
                    // 估算取后剩余的质押价值
                    let needed = if rate.is_zero() { round(actual) } else { round(actual / rate) };
                    let remaining_receipt_a = caller_receipt - needed;
                    let r_b_bal = reader.node_balance(caller_id, receipt_b);
                    let r_a_supply = reader.token_total_supply(receipt_a);
                    let r_b_supply = reader.token_total_supply(receipt_b);
                    let pool_a_cash = contract.balance(p0.asset_token_a);
                    let pool_b_cash = contract.balance(p0.asset_token_b);
                    let d_a_sup = reader.token_total_supply(debt_a);
                    let d_b_sup = reader.token_total_supply(debt_b);
                    let borrowed_a = if d_a_sup < Decimal::ZERO { -d_a_sup } else { Decimal::ZERO };
                    let borrowed_b = if d_b_sup < Decimal::ZERO { -d_b_sup } else { Decimal::ZERO };

                    let a_val = if r_a_supply.is_zero() { Decimal::ZERO }
                        else { ((pool_a_cash + borrowed_a) - actual) * remaining_receipt_a / r_a_supply };
                    let b_val = if r_b_supply.is_zero() { Decimal::ZERO }
                        else { (pool_b_cash + borrowed_b) * r_b_bal / r_b_supply };
                    let total_collateral = a_val + reader.convert_value(p0.asset_token_b, p0.asset_token_a, b_val);
                    if total_debt * p0.min_collateral_ratio > total_collateral {
                        return Vec::new(); // 取出后质押率不足
                    }
                }

                let needed = if rate.is_zero() { round(actual) } else { round(actual / rate) };
                let to_burn = needed.min(caller_receipt);
                res.push(Msg::Issue { token: receipt_a, account_id: caller_id, volume: -to_burn });
                res.push(Msg::Transfer { src_id: contract.id, dst_id: caller_id, token, volume: actual });
            }
            res
        });

        // ── on_called[1]：aBTC ↔ BTC 存款/取款（浮动汇率） ──
        let p1 = self.clone();
        let exchange_b: CalledFn = Arc::new(move |contract: &mut Contract, reader: &dyn EngineReader, caller_id: usize, volume: Decimal| -> Vec<Msg> {
            if volume.is_zero() { return Vec::new(); }
            let receipt_b = match cached_receipt_id(contract, reader, &p1.receipt_b_name, KEY_COLLATERAL_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let debt_b = match cached_receipt_id(contract, reader, &p1.debt_b_name, KEY_DEBT_RECEIPT_ID_B) {
                Some(id) => id, None => return Vec::new(),
            };
            let prec = reader.precision();
            let round = |v: Decimal| v.round_dp_with_strategy(prec as u32, RoundingStrategy::ToZero);
            let token = p1.asset_token_b;
            let mut res = Vec::new();

            // 计算 aBTC 汇率
            let pool_cash = contract.balance(token);
            let d_supply = reader.token_total_supply(debt_b);
            let borrowed = if d_supply < Decimal::ZERO { -d_supply } else { Decimal::ZERO };
            let r_supply = reader.token_total_supply(receipt_b);
            let rate = if r_supply.is_zero() { Decimal::ONE }
            else {
                let pv = pool_cash + borrowed;
                if pv.is_zero() { Decimal::ONE } else { pv / r_supply }
            };

            if volume > Decimal::ZERO {
                // 存入 BTC
                let caller_bal = reader.node_balance(caller_id, token);
                if caller_bal < volume { return Vec::new(); }
                let receipt_amt = if rate.is_zero() { volume } else { round(volume / rate) };
                if receipt_amt <= Decimal::ZERO { return Vec::new(); }
                res.push(Msg::Transfer { src_id: caller_id, dst_id: contract.id, token, volume });
                res.push(Msg::Issue { token: receipt_b, account_id: caller_id, volume: receipt_amt });
            } else {
                // 取出 BTC
                let withdraw = -volume;
                let caller_receipt = reader.node_balance(caller_id, receipt_b);
                let max_w = if rate.is_zero() { caller_receipt } else { round(caller_receipt * rate) };
                let mut actual = withdraw.min(max_w);
                let pool_bal = contract.balance(token);
                actual = actual.min(pool_bal);
                if actual <= Decimal::ZERO { return Vec::new(); }

                // 取出后保证总质押率满足要求（交叉质押检查）
                let debt_a = match cached_receipt_id(contract, reader, &p1.debt_a_name, KEY_DEBT_RECEIPT_ID) {
                    Some(id) => id, None => return Vec::new(),
                };
                let receipt_a = match cached_receipt_id(contract, reader, &p1.receipt_a_name, KEY_ASSET_RECEIPT_ID) {
                    Some(id) => id, None => return Vec::new(),
                };
                let d_a_bal = reader.node_balance(caller_id, debt_a);
                let d_b_bal = reader.node_balance(caller_id, debt_b);
                let d_a_amt = if d_a_bal < Decimal::ZERO { -d_a_bal } else { Decimal::ZERO };
                let d_b_amt = if d_b_bal < Decimal::ZERO { -d_b_bal } else { Decimal::ZERO };
                let total_debt = d_a_amt + reader.convert_value(p1.asset_token_b, p1.asset_token_a, d_b_amt);
                if total_debt > Decimal::ZERO {
                    // 估算取后剩余的质押价值
                    let needed = if rate.is_zero() { round(actual) } else { round(actual / rate) };
                    let remaining_receipt_b = caller_receipt - needed;
                    let r_a_bal = reader.node_balance(caller_id, receipt_a);
                    let r_a_supply = reader.token_total_supply(receipt_a);
                    let r_b_supply = reader.token_total_supply(receipt_b);
                    let pool_a_cash = contract.balance(p1.asset_token_a);
                    let pool_b_cash = contract.balance(p1.asset_token_b);
                    let d_a_sup = reader.token_total_supply(debt_a);
                    let d_b_sup = reader.token_total_supply(debt_b);
                    let borrowed_a = if d_a_sup < Decimal::ZERO { -d_a_sup } else { Decimal::ZERO };
                    let borrowed_b = if d_b_sup < Decimal::ZERO { -d_b_sup } else { Decimal::ZERO };

                    let a_val = if r_a_supply.is_zero() { Decimal::ZERO }
                        else { (pool_a_cash + borrowed_a) * r_a_bal / r_a_supply };
                    let b_val = if r_b_supply.is_zero() { Decimal::ZERO }
                        else { ((pool_b_cash + borrowed_b) - actual) * remaining_receipt_b / r_b_supply };
                    let total_collateral = a_val + reader.convert_value(p1.asset_token_b, p1.asset_token_a, b_val);
                    if total_debt * p1.min_collateral_ratio > total_collateral {
                        return Vec::new(); // 取出后质押率不足
                    }
                }

                let needed = if rate.is_zero() { round(actual) } else { round(actual / rate) };
                let to_burn = needed.min(caller_receipt);
                res.push(Msg::Issue { token: receipt_b, account_id: caller_id, volume: -to_burn });
                res.push(Msg::Transfer { src_id: contract.id, dst_id: caller_id, token, volume: actual });
            }
            res
        });

        // ── on_called[2]：dUSDT ↔ USDT 借款/还款（交叉质押） ──
        let p2 = self.clone();
        let borrow_repay_a: CalledFn = Arc::new(move |contract: &mut Contract, reader: &dyn EngineReader, caller_id: usize, volume: Decimal| -> Vec<Msg> {
            if volume.is_zero() { return Vec::new(); }
            let debt_a = match cached_receipt_id(contract, reader, &p2.debt_a_name, KEY_DEBT_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let receipt_a = match cached_receipt_id(contract, reader, &p2.receipt_a_name, KEY_ASSET_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let receipt_b = match cached_receipt_id(contract, reader, &p2.receipt_b_name, KEY_COLLATERAL_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let debt_b = match cached_receipt_id(contract, reader, &p2.debt_b_name, KEY_DEBT_RECEIPT_ID_B) {
                Some(id) => id, None => return Vec::new(),
            };
            let mut res = Vec::new();
            let token = p2.asset_token_a;
            let token_b = p2.asset_token_b;

            // 交叉质押检查：总价值 = aUSDT 池份额 + aBTC 池份额（以 USDT 计价）
            let check_collateral = |reader: &dyn EngineReader, caller_id: usize, existing_debt_a: Decimal, additional_debt_a: Decimal,
                receipt_a: usize, receipt_b: usize, debt_b: usize, token_a: usize, token_b: usize, min_ratio: Decimal| -> bool {
                let d_b_bal = reader.node_balance(caller_id, debt_b);
                let d_b_amt = if d_b_bal < Decimal::ZERO { -d_b_bal } else { Decimal::ZERO };
                let total_debt = (existing_debt_a + additional_debt_a) + reader.convert_value(token_b, token_a, d_b_amt);
                if total_debt <= Decimal::ZERO { return true; }

                let r_a_bal = reader.node_balance(caller_id, receipt_a);
                let r_b_bal = reader.node_balance(caller_id, receipt_b);
                let pool_a_cash = contract.balance(token_a);
                let pool_b_cash = contract.balance(token_b);
                let d_a_sup = reader.token_total_supply(debt_a);
                let d_b_sup = reader.token_total_supply(debt_b);
                let borrowed_a = if d_a_sup < Decimal::ZERO { -d_a_sup } else { Decimal::ZERO };
                let borrowed_b = if d_b_sup < Decimal::ZERO { -d_b_sup } else { Decimal::ZERO };
                let r_a_sup = reader.token_total_supply(receipt_a);
                let r_b_sup = reader.token_total_supply(receipt_b);
                let a_val = if r_a_sup.is_zero() { Decimal::ZERO }
                    else { (pool_a_cash + borrowed_a) * r_a_bal / r_a_sup };
                let b_val = if r_b_sup.is_zero() { Decimal::ZERO }
                    else { (pool_b_cash + borrowed_b) * r_b_bal / r_b_sup };
                let total_collateral = a_val + reader.convert_value(token_b, token_a, b_val);
                total_collateral > Decimal::ZERO && total_debt * min_ratio <= total_collateral
            };

            if volume > Decimal::ZERO {
                // 还款
                let repay = volume;
                let caller_bal = reader.node_balance(caller_id, token);
                if caller_bal < repay { return Vec::new(); }
                let d_a_bal = reader.node_balance(caller_id, debt_a);
                let max_repay = if d_a_bal < Decimal::ZERO { -d_a_bal } else { Decimal::ZERO };
                let actual = repay.min(max_repay);
                if actual <= Decimal::ZERO { return Vec::new(); }
                res.push(Msg::Transfer { src_id: caller_id, dst_id: contract.id, token, volume: actual });
                res.push(Msg::Issue { token: debt_a, account_id: caller_id, volume: actual });
            } else {
                // 借款
                let mut borrow = -volume;
                let pool_bal = contract.balance(token);
                borrow = borrow.min(pool_bal);
                if borrow <= Decimal::ZERO { return Vec::new(); }
                let d_a_bal = reader.node_balance(caller_id, debt_a);
                let existing = if d_a_bal < Decimal::ZERO { -d_a_bal } else { Decimal::ZERO };
                if !check_collateral(reader, caller_id, existing, borrow, receipt_a, receipt_b, debt_b, token, token_b, p2.min_collateral_ratio) {
                    return Vec::new();
                }
                res.push(Msg::Transfer { src_id: contract.id, dst_id: caller_id, token, volume: borrow });
                res.push(Msg::Issue { token: debt_a, account_id: caller_id, volume: -borrow });
            }
            res
        });

        // ── on_called[3]：dBTC ↔ BTC 借款/还款（交叉质押） ──
        let p3 = self.clone();
        let borrow_repay_b: CalledFn = Arc::new(move |contract: &mut Contract, reader: &dyn EngineReader, caller_id: usize, volume: Decimal| -> Vec<Msg> {
            if volume.is_zero() { return Vec::new(); }
            let debt_b = match cached_receipt_id(contract, reader, &p3.debt_b_name, KEY_DEBT_RECEIPT_ID_B) {
                Some(id) => id, None => return Vec::new(),
            };
            let receipt_a = match cached_receipt_id(contract, reader, &p3.receipt_a_name, KEY_ASSET_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let receipt_b = match cached_receipt_id(contract, reader, &p3.receipt_b_name, KEY_COLLATERAL_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let debt_a = match cached_receipt_id(contract, reader, &p3.debt_a_name, KEY_DEBT_RECEIPT_ID) {
                Some(id) => id, None => return Vec::new(),
            };
            let mut res = Vec::new();
            let token = p3.asset_token_b;
            let token_a = p3.asset_token_a;

            let check_collateral = |reader: &dyn EngineReader, caller_id: usize, existing_debt_b: Decimal, additional_debt_b: Decimal,
                receipt_a: usize, receipt_b: usize, debt_a: usize, token_a: usize, token_b: usize, min_ratio: Decimal| -> bool {
                let d_a_bal = reader.node_balance(caller_id, debt_a);
                let d_a_amt = if d_a_bal < Decimal::ZERO { -d_a_bal } else { Decimal::ZERO };
                let additional_in_a = reader.convert_value(token_b, token_a, additional_debt_b);
                let existing_in_a = reader.convert_value(token_b, token_a, existing_debt_b);
                let total_debt = d_a_amt + existing_in_a + additional_in_a;
                if total_debt <= Decimal::ZERO { return true; }

                let r_a_bal = reader.node_balance(caller_id, receipt_a);
                let r_b_bal = reader.node_balance(caller_id, receipt_b);
                let pool_a_cash = contract.balance(token_a);
                let pool_b_cash = contract.balance(token_b);
                let d_a_sup = reader.token_total_supply(debt_a);
                let d_b_sup = reader.token_total_supply(debt_b);
                let borrowed_a = if d_a_sup < Decimal::ZERO { -d_a_sup } else { Decimal::ZERO };
                let borrowed_b = if d_b_sup < Decimal::ZERO { -d_b_sup } else { Decimal::ZERO };
                let r_a_sup = reader.token_total_supply(receipt_a);
                let r_b_sup = reader.token_total_supply(receipt_b);
                let a_val = if r_a_sup.is_zero() { Decimal::ZERO }
                    else { (pool_a_cash + borrowed_a) * r_a_bal / r_a_sup };
                let b_val = if r_b_sup.is_zero() { Decimal::ZERO }
                    else { (pool_b_cash + borrowed_b) * r_b_bal / r_b_sup };
                let total_collateral = a_val + reader.convert_value(token_b, token_a, b_val);
                total_collateral > Decimal::ZERO && total_debt * min_ratio <= total_collateral
            };

            if volume > Decimal::ZERO {
                // 还 BTC
                let repay = volume;
                let caller_bal = reader.node_balance(caller_id, token);
                if caller_bal < repay { return Vec::new(); }
                let d_b_bal = reader.node_balance(caller_id, debt_b);
                let max_repay = if d_b_bal < Decimal::ZERO { -d_b_bal } else { Decimal::ZERO };
                let actual = repay.min(max_repay);
                if actual <= Decimal::ZERO { return Vec::new(); }
                res.push(Msg::Transfer { src_id: caller_id, dst_id: contract.id, token, volume: actual });
                res.push(Msg::Issue { token: debt_b, account_id: caller_id, volume: actual });
            } else {
                // 借 BTC
                let mut borrow = -volume;
                let pool_bal = contract.balance(token);
                borrow = borrow.min(pool_bal);
                if borrow <= Decimal::ZERO { return Vec::new(); }
                let d_b_bal = reader.node_balance(caller_id, debt_b);
                let existing = if d_b_bal < Decimal::ZERO { -d_b_bal } else { Decimal::ZERO };
                if !check_collateral(reader, caller_id, existing, borrow, receipt_a, receipt_b, debt_a, token_a, token, p3.min_collateral_ratio) {
                    return Vec::new();
                }
                res.push(Msg::Transfer { src_id: contract.id, dst_id: caller_id, token, volume: borrow });
                res.push(Msg::Issue { token: debt_b, account_id: caller_id, volume: -borrow });
            }
            res
        });

        (
            on_create,
            on_update,
            on_end,
            vec![exchange_a, exchange_b, borrow_repay_a, borrow_repay_b],
        )
    }

    /// 按利用率计算利率（Compound 风格分段线性）
    fn _calc_rate(&self, utilization: Decimal) -> Decimal {
        if utilization < self.optimal_utilization {
            let slope = if self.optimal_utilization.is_zero() {
                Decimal::ZERO
            } else {
                self.slope1 * utilization / self.optimal_utilization
            };
            self.base_rate + slope
        } else {
            let one_minus = Decimal::ONE - self.optimal_utilization;
            let slope = if one_minus.is_zero() {
                Decimal::ZERO
            } else {
                self.slope2 * (utilization - self.optimal_utilization) / one_minus
            };
            self.base_rate + self.slope1 + slope
        }
    }
}
