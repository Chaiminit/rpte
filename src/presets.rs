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
//  借贷合约预设（Lending Contract Preset）
// ═══════════════════════════════════════════════════════════════
//
// 模型：池化借贷
//   - 存款人存入资产代币 → 获得资产凭证代币（享有利息）
//   - 借款人存入质押代币 → 获得质押凭证代币（获得借款额度）
//   - 借款人可借出资产代币，支付利息
//   - 质押率低于清算阈值时触发清算
//
// 两个 on_called 函数：
//   0: 资产代币 ↔ 资产凭证代币 交换 (存款/取款/借款)
//   1: 质押代币 ↔ 质押凭证代币 交换 (存入/取出质押品)
//
// on_update：
//   - 按利用率生成动态利率
//   - 向借款人计收利息（累加到 total_borrowed）
//   - 清算低于阈值的头寸
//
// 合约状态编码在 balance sheet 中：
//   - sheet[asset_token]             = 合约持有的资产池总量
//   - sheet[collateral_token]        = 合约持有的质押池总量
//   - sheet[TOTAL_BORROWED]          = 总借款额
//   - sheet[DEBT_BASE + account_id]  = 该账户的借款额
//   - sheet[KEY_ASSET_RECEIPT_ID]    = 资产凭证代币 ID（缓存）
//   - sheet[KEY_COLLATERAL_RECEIPT_ID] = 质押凭证代币 ID（缓存）
// ═══════════════════════════════════════════════════════════════

/// 合约内置状态 sentinel key：总借款额
const KEY_TOTAL_BORROWED: usize = usize::MAX;
/// 合约内置状态 sentinel key：资产凭证代币 ID 缓存
const KEY_ASSET_RECEIPT_ID: usize = usize::MAX - 1;
/// 合约内置状态 sentinel key：质押凭证代币 ID 缓存
const KEY_COLLATERAL_RECEIPT_ID: usize = usize::MAX - 2;
/// 合约内置状态 sentinel base：DEBT_BASE + account_id → 该账户借款额
const DEBT_BASE: usize = usize::MAX / 2;

/// 借贷合约配置参数
#[derive(Clone)]
pub struct LendingPreset {
    /// 资产代币（借出/存款的代币，如 USDT）
    pub asset_token: usize,
    /// 质押代币（作为抵押品的代币，如 BTC）
    pub collateral_token: usize,
    /// 资产凭证代币名称（on_create 时自动注册）
    pub asset_receipt_name: String,
    /// 质押凭证代币名称（on_create 时自动注册）
    pub collateral_receipt_name: String,
    /// 最低质押率（如 1.50 = 150%）
    pub min_collateral_ratio: Decimal,
    /// 清算阈值（低于此值触发清算，如 1.30 = 130%）
    pub liquidation_threshold: Decimal,
    // ── 利率模型参数（Compound 风格分段线性） ──
    /// 最优利用率 (0.0 ~ 1.0)，默认 0.80
    pub optimal_utilization: Decimal,
    /// 基础利率（按帧），默认 0.001 / 帧
    pub base_rate: Decimal,
    /// 斜率 1（利用率 < optimal），默认 0.02 / 帧
    pub slope1: Decimal,
    /// 斜率 2（利用率 >= optimal），默认 0.50 / 帧
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
    /// 创建借贷合约预设，使用默认利率参数。
    ///
    /// `asset_receipt_name` / `collateral_receipt_name` 是凭证代币名称，
    /// 会在 `on_create` 中自动注册为不可交易代币，并通过 `virtual_anchor` 与对应基础代币价值绑定。
    pub fn new(
        asset_token: usize,
        collateral_token: usize,
        asset_receipt_name: &str,
        collateral_receipt_name: &str,
        min_collateral_ratio: Decimal,
        liquidation_threshold: Decimal,
    ) -> Self {
        Self {
            asset_token,
            collateral_token,
            asset_receipt_name: asset_receipt_name.to_string(),
            collateral_receipt_name: collateral_receipt_name.to_string(),
            min_collateral_ratio,
            liquidation_threshold,
            optimal_utilization: Decimal::new(8, 1), // 0.8
            base_rate:          Decimal::new(1, 3),  // 0.001
            slope1:             Decimal::new(2, 3),  // 0.002
            slope2:             Decimal::new(5, 1),  // 0.5
        }
    }

    /// 构建合约行为函数，返回 `(on_create, on_update, on_end, on_called_fns)`。
    pub fn build(&self) -> (ContractFn, ContractFn, ContractFn, Vec<CalledFn>) {
        // ── 按名称注册凭证代币 ──
        let p_create = self.clone();
        let on_create: ContractFn = Arc::new(move |_contract: &mut Contract, _reader: &dyn EngineReader, _step: u64| -> Vec<Msg> {
            vec![
                Msg::RegisterToken {
                    name: p_create.asset_receipt_name.clone(),
                    can_be_negative: false,
                    not_tradable: true,
                    virtual_anchor: Some(p_create.asset_token),
                    swap_whitelist: Vec::new(),
                },
                Msg::RegisterToken {
                    name: p_create.collateral_receipt_name.clone(),
                    can_be_negative: false,
                    not_tradable: true,
                    virtual_anchor: Some(p_create.collateral_token),
                    swap_whitelist: Vec::new(),
                },
            ]
        });

        // ── on_update：利率生成、利息结算、清算 ──
        let p_up = self.clone();
        let on_update: ContractFn = Arc::new(move |contract: &mut Contract, reader: &dyn EngineReader, _step: u64| -> Vec<Msg> {
            let prec = reader.precision();
            let round = |v: Decimal| v.round_dp_with_strategy(prec as u32, RoundingStrategy::ToZero);

            let asset_token = p_up.asset_token;
            let collateral_token = p_up.collateral_token;

            // 读取凭证代币 ID（需要先完成注册）
            let collateral_receipt_token = match cached_receipt_id(
                contract, reader, &p_up.collateral_receipt_name, KEY_COLLATERAL_RECEIPT_ID,
            ) {
                Some(id) => id,
                None => return Vec::new(), // 尚未注册，跳过此帧
            };

            // 读取当前池状态
            let total_asset_pool = contract.balance(asset_token);
            let total_collateral_pool = contract.balance(collateral_token);
            let total_borrowed = contract.balance(KEY_TOTAL_BORROWED);

            // ── 1. 利率计算与利息结算 ──
            let pool_total = total_asset_pool + total_borrowed;
            let msgs = if !pool_total.is_zero() && !total_borrowed.is_zero() {
                let utilization = total_borrowed / pool_total;
                let rate = if utilization < p_up.optimal_utilization {
                    let slope = if p_up.optimal_utilization.is_zero() {
                        Decimal::ZERO
                    } else {
                        p_up.slope1 * utilization / p_up.optimal_utilization
                    };
                    p_up.base_rate + slope
                } else {
                    let one_minus_optimal = Decimal::ONE - p_up.optimal_utilization;
                    let slope = if one_minus_optimal.is_zero() {
                        Decimal::ZERO
                    } else {
                        p_up.slope2 * (utilization - p_up.optimal_utilization) / one_minus_optimal
                    };
                    p_up.base_rate + p_up.slope1 + slope
                };

                let interest = round(total_borrowed * rate);
                if interest > Decimal::ZERO {
                    let new_borrowed = total_borrowed + interest;
                    contract.set_balance(KEY_TOTAL_BORROWED, new_borrowed);
                }
                Vec::new()
            } else {
                Vec::new()
            };

            // ── 2. 清算低于清算阈值的头寸 ──
            if total_collateral_pool.is_zero() {
                return msgs;
            }

            let all_accounts = reader.get_all_accounts();
            let mut liq_msgs: Vec<Msg> = Vec::new();

            for &account_id in &all_accounts {
                let debt = contract.balance(DEBT_BASE + account_id);
                if debt <= Decimal::ZERO {
                    continue;
                }

                let collateral_receipt_balance = reader.node_balance(account_id, collateral_receipt_token);
                if collateral_receipt_balance <= Decimal::ZERO {
                    // 没有质押凭证但仍欠债 → 坏账，直接清零
                    contract.set_balance(DEBT_BASE + account_id, Decimal::ZERO);
                    let cur = contract.balance(KEY_TOTAL_BORROWED);
                    contract.set_balance(KEY_TOTAL_BORROWED, (cur - debt).max(Decimal::ZERO));
                    continue;
                }

                let total_collateral_receipt = reader.token_total_supply(collateral_receipt_token);
                let account_collateral_share = if total_collateral_receipt.is_zero() {
                    Decimal::ZERO
                } else {
                    total_collateral_pool * collateral_receipt_balance / total_collateral_receipt
                };

                let collateral_value = reader.convert_value(collateral_token, asset_token, account_collateral_share);

                if collateral_value <= Decimal::ZERO || debt <= Decimal::ZERO {
                    continue;
                }
                let health = collateral_value / debt;

                if health < p_up.liquidation_threshold {
                    // ── 触发清算 ──
                    liq_msgs.push(Msg::Issue {
                        token: collateral_receipt_token,
                        account_id,
                        volume: -collateral_receipt_balance,
                    });

                    let penalty = round(debt * Decimal::new(5, 2)); // 5% 清算惩罚
                    let needed_asset = debt + penalty;

                    let price_opt = reader.price_between(collateral_token, asset_token);
                    let swap_collateral_amount = if let Some((price, quote, _base)) = price_opt {
                        if price.is_zero() {
                            account_collateral_share
                        } else if quote == collateral_token {
                            round(needed_asset * price)
                        } else {
                            if price.is_zero() { account_collateral_share }
                            else { round(needed_asset / price) }
                        }
                    } else {
                        account_collateral_share
                    };

                    let swap_amount = swap_collateral_amount.min(account_collateral_share);

                    if swap_amount > Decimal::ZERO {
                        liq_msgs.push(Msg::SwapOrder {
                            src_id: contract.id,
                            owner_node_id: contract.id,
                            src_token: collateral_token,
                            dst_token: asset_token,
                            volume: swap_amount,
                        });
                    }

                    contract.set_balance(DEBT_BASE + account_id, Decimal::ZERO);
                    let cur_total = contract.balance(KEY_TOTAL_BORROWED);
                    contract.set_balance(KEY_TOTAL_BORROWED, (cur_total - debt).max(Decimal::ZERO));
                }
            }

            let mut all = msgs;
            all.extend(liq_msgs);
            all
        });

        // ── on_end：合约结束，无特殊操作 ──
        let on_end: ContractFn = Arc::new(move |_contract: &mut Contract, _reader: &dyn EngineReader, _step: u64| -> Vec<Msg> {
            Vec::new()
        });

        // ── on_called[0]：资产代币 ↔ 资产凭证代币 ──
        let p0 = self.clone();
        let exchange_asset: CalledFn = Arc::new(move |contract: &mut Contract, reader: &dyn EngineReader, caller_id: usize, volume: Decimal| -> Vec<Msg> {
            if volume.is_zero() {
                return Vec::new();
            }

            // 读取凭证代币 ID
            let asset_receipt_token = match cached_receipt_id(
                contract, reader, &p0.asset_receipt_name, KEY_ASSET_RECEIPT_ID,
            ) {
                Some(id) => id,
                None => return Vec::new(),
            };

            let prec = reader.precision();
            let round = |v: Decimal| v.round_dp_with_strategy(prec as u32, RoundingStrategy::ToZero);
            let mut msgs = Vec::new();

            let asset_token = p0.asset_token;
            let collateral_token = p0.collateral_token;

            // 计算资产凭证汇率: rate = (总资产池 + 总借款) / 总凭证发行量
            let total_asset_pool = contract.balance(asset_token);
            let total_borrowed = contract.balance(KEY_TOTAL_BORROWED);
            let total_receipt_supply = reader.token_total_supply(asset_receipt_token);
            let rate = if total_receipt_supply.is_zero() {
                Decimal::ONE
            } else {
                let pool_value = total_asset_pool + total_borrowed;
                if pool_value.is_zero() {
                    Decimal::ONE
                } else {
                    pool_value / total_receipt_supply
                }
            };

            if volume > Decimal::ZERO {
                // ── 存款：转入 volume 资产代币，铸造凭证 ──
                let caller_asset_bal = reader.node_balance(caller_id, asset_token);
                if caller_asset_bal < volume {
                    return Vec::new();
                }

                let receipt_amount = if rate.is_zero() { volume } else { round(volume / rate) };
                if receipt_amount <= Decimal::ZERO {
                    return Vec::new();
                }

                msgs.push(Msg::Transfer {
                    src_id: caller_id,
                    dst_id: contract.id,
                    token: asset_token,
                    volume,
                });
                msgs.push(Msg::Issue {
                    token: asset_receipt_token,
                    account_id: caller_id,
                    volume: receipt_amount,
                });
            } else {
                // ── volume < 0：取款 或 借款 ──
                let withdraw_amount = -volume;
                let caller_receipt_bal = reader.node_balance(caller_id, asset_receipt_token);

                let max_withdraw_from_receipt = if rate.is_zero() {
                    caller_receipt_bal
                } else {
                    round(caller_receipt_bal * rate)
                };

                if withdraw_amount <= max_withdraw_from_receipt {
                    // ── 纯取款 ──
                    // 检查池中流动性是否充足
                    if contract.balance(asset_token) < withdraw_amount {
                        return Vec::new();
                    }

                    let receipt_needed = if rate.is_zero() {
                        round(withdraw_amount)
                    } else {
                        round(withdraw_amount / rate)
                    };
                    let receipt_to_burn = receipt_needed.min(caller_receipt_bal);

                    msgs.push(Msg::Issue {
                        token: asset_receipt_token,
                        account_id: caller_id,
                        volume: -receipt_to_burn,
                    });
                    msgs.push(Msg::Transfer {
                        src_id: contract.id,
                        dst_id: caller_id,
                        token: asset_token,
                        volume: withdraw_amount,
                    });
                } else {
                    // ── 取款超出凭证额度 → 超出部分视为借款 ──
                    let deposit_part = max_withdraw_from_receipt;
                    let borrow_part = withdraw_amount - deposit_part;

                    // 检查借款人的质押品是否充足
                    let collateral_receipt_token = match cached_receipt_id(
                        contract, reader, &p0.collateral_receipt_name, KEY_COLLATERAL_RECEIPT_ID,
                    ) {
                        Some(id) => id,
                        None => return Vec::new(),
                    };
                    let caller_collateral_receipt = reader.node_balance(caller_id, collateral_receipt_token);
                    let total_collateral_receipt = reader.token_total_supply(collateral_receipt_token);
                    let total_collateral_pool = contract.balance(collateral_token);

                    let account_collateral_share = if total_collateral_receipt.is_zero() {
                        Decimal::ZERO
                    } else {
                        total_collateral_pool * caller_collateral_receipt / total_collateral_receipt
                    };

                    let existing_debt = contract.balance(DEBT_BASE + caller_id);
                    let collateral_value = reader.convert_value(collateral_token, asset_token, account_collateral_share);

                    let total_debt = existing_debt + borrow_part;
                    if collateral_value <= Decimal::ZERO || p0.min_collateral_ratio <= Decimal::ZERO {
                        return Vec::new();
                    }
                    if total_debt * p0.min_collateral_ratio > collateral_value {
                        return Vec::new(); // 质押率不足
                    }

                    // 检查池中流动性是否充足
                    if contract.balance(asset_token) < withdraw_amount {
                        return Vec::new();
                    }

                    // 销毁凭证（取回自有存款部分）
                    let receipt_to_burn = if rate.is_zero() {
                        round(deposit_part)
                    } else {
                        round(deposit_part / rate)
                    };
                    let receipt_to_burn = receipt_to_burn.min(caller_receipt_bal);
                    if receipt_to_burn > Decimal::ZERO {
                        msgs.push(Msg::Issue {
                            token: asset_receipt_token,
                            account_id: caller_id,
                            volume: -receipt_to_burn,
                        });
                    }

                    msgs.push(Msg::Transfer {
                        src_id: contract.id,
                        dst_id: caller_id,
                        token: asset_token,
                        volume: withdraw_amount,
                    });

                    // 记录借款
                    let new_debt = existing_debt + borrow_part;
                    contract.set_balance(DEBT_BASE + caller_id, new_debt);
                    contract.set_balance(KEY_TOTAL_BORROWED, total_borrowed + borrow_part);
                }
            }

            msgs
        });

        // ── on_called[1]：质押代币 ↔ 质押凭证代币 ──
        let p1 = self.clone();
        let exchange_collateral: CalledFn = Arc::new(move |contract: &mut Contract, reader: &dyn EngineReader, caller_id: usize, volume: Decimal| -> Vec<Msg> {
            if volume.is_zero() {
                return Vec::new();
            }

            // 读取凭证代币 ID
            let collateral_receipt_token = match cached_receipt_id(
                contract, reader, &p1.collateral_receipt_name, KEY_COLLATERAL_RECEIPT_ID,
            ) {
                Some(id) => id,
                None => return Vec::new(),
            };

            let prec = reader.precision();
            let round = |v: Decimal| v.round_dp_with_strategy(prec as u32, RoundingStrategy::ToZero);
            let mut msgs = Vec::new();

            let collateral_token = p1.collateral_token;
            let asset_token = p1.asset_token;

            // 计算质押凭证汇率: rate = 总质押池 / 总凭证发行量
            let total_collateral_pool = contract.balance(collateral_token);
            let total_receipt_supply = reader.token_total_supply(collateral_receipt_token);
            let rate = if total_receipt_supply.is_zero() {
                Decimal::ONE
            } else if total_collateral_pool.is_zero() {
                Decimal::ONE
            } else {
                total_collateral_pool / total_receipt_supply
            };

            if volume > Decimal::ZERO {
                // ── 存入质押品 ──
                let caller_bal = reader.node_balance(caller_id, collateral_token);
                if caller_bal < volume {
                    return Vec::new();
                }

                let receipt_amount = if rate.is_zero() { volume } else { round(volume / rate) };
                if receipt_amount <= Decimal::ZERO {
                    return Vec::new();
                }

                msgs.push(Msg::Transfer {
                    src_id: caller_id,
                    dst_id: contract.id,
                    token: collateral_token,
                    volume,
                });
                msgs.push(Msg::Issue {
                    token: collateral_receipt_token,
                    account_id: caller_id,
                    volume: receipt_amount,
                });
            } else {
                // ── volume < 0：取出质押品 ──
                let withdraw_amount = -volume;
                let caller_receipt_bal = reader.node_balance(caller_id, collateral_receipt_token);
                let max_withdraw = if rate.is_zero() {
                    caller_receipt_bal
                } else {
                    round(caller_receipt_bal * rate)
                };

                let actual_withdraw = withdraw_amount.min(max_withdraw);
                if actual_withdraw <= Decimal::ZERO {
                    return Vec::new();
                }

                // 取出后需保证仍满足质押率要求（如有借款）
                let debt = contract.balance(DEBT_BASE + caller_id);
                if debt > Decimal::ZERO {
                    let receipt_to_burn_for_check = round(actual_withdraw / rate);
                    let remaining_receipt_bal = caller_receipt_bal - receipt_to_burn_for_check;
                    let account_share = if total_receipt_supply.is_zero() {
                        Decimal::ZERO
                    } else {
                        (total_collateral_pool - actual_withdraw) * remaining_receipt_bal / total_receipt_supply
                    };
                    let collateral_value_after = reader.convert_value(collateral_token, asset_token, account_share);

                    if debt * p1.min_collateral_ratio > collateral_value_after {
                        return Vec::new(); // 取出后质押率不足
                    }
                }

                let receipt_needed = if rate.is_zero() {
                    round(actual_withdraw)
                } else {
                    round(actual_withdraw / rate)
                };
                let receipt_to_burn = receipt_needed.min(caller_receipt_bal);

                msgs.push(Msg::Issue {
                    token: collateral_receipt_token,
                    account_id: caller_id,
                    volume: -receipt_to_burn,
                });
                msgs.push(Msg::Transfer {
                    src_id: contract.id,
                    dst_id: caller_id,
                    token: collateral_token,
                    volume: actual_withdraw,
                });
            }

            msgs
        });

        (on_create, on_update, on_end, vec![exchange_asset, exchange_collateral])
    }
}