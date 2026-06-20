//! # Fee Presets — 手续费预设
//!
//! 提供常见手续费收取策略的预制模板。
//! 每个预设返回 `FeeFn`，可直接传入 `Pair::set_fee_fn()` 或 `VirtualPair::set_fee_fn()`。

use std::sync::Arc;
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use crate::fee::{FeeCtx, FeeFn};
use crate::node::Msg;

/// 创建一个空手续费函数（不收任何费用）。
pub fn empty_fee() -> FeeFn {
    Arc::new(|_reader, _ctx: FeeCtx| Vec::new())
}

/// Taker/Maker 分层费率手续费预设。
///
/// # 参数
///
/// | 参数 | 类型 | 含义 |
/// |------|------|------|
/// | `taker_rate` | `Decimal` | taker 费率（例如 0.0003 = 0.03%）|
/// | `maker_rate` | `Decimal` | maker 费率（例如 0.0001 = 0.01%）|
/// | `fee_collector` | `usize` | 收取手续费的节点 ID |
///
/// # 收费规则
///
/// - taker 从它**收到**的 token 中扣费（收到 base 就扣 base，收到 quote 就扣 quote）
/// - maker 从它**收到**的 token 中扣费
/// - 费率先乘成交 volume 再按引擎精度截断
/// - round 后为 0 则不发送
pub fn taker_maker_fee(
    taker_rate: Decimal,
    maker_rate: Decimal,
    fee_collector: usize,
) -> FeeFn {
    Arc::new(move |reader, ctx: FeeCtx| {
        let prec = reader.precision();
        let round = |v: Decimal| v.round_dp_with_strategy(prec as u32, RoundingStrategy::ToZero);
        let mut msgs = Vec::new();

        // ── Taker fee ──
        if ctx.taker_node == ctx.buyer_node {
            // taker 收到了 base_token
            let fee = round(ctx.base_volume * taker_rate);
            if fee > Decimal::ZERO {
                msgs.push(Msg::Transfer {
                    src_id: ctx.taker_node,
                    dst_id: fee_collector,
                    token: ctx.base_token,
                    volume: fee,
                });
            }
        } else {
            // taker 收到了 quote_token（作为卖方）
            let fee = round(ctx.quote_volume * taker_rate);
            if fee > Decimal::ZERO {
                msgs.push(Msg::Transfer {
                    src_id: ctx.taker_node,
                    dst_id: fee_collector,
                    token: ctx.quote_token,
                    volume: fee,
                });
            }
        }

        // ── Maker fee ──
        if maker_rate.is_zero() {
            return msgs;
        }
        if ctx.maker_node == ctx.buyer_node {
            let fee = round(ctx.base_volume * maker_rate);
            if fee > Decimal::ZERO {
                msgs.push(Msg::Transfer {
                    src_id: ctx.maker_node,
                    dst_id: fee_collector,
                    token: ctx.base_token,
                    volume: fee,
                });
            }
        } else {
            let fee = round(ctx.quote_volume * maker_rate);
            if fee > Decimal::ZERO {
                msgs.push(Msg::Transfer {
                    src_id: ctx.maker_node,
                    dst_id: fee_collector,
                    token: ctx.quote_token,
                    volume: fee,
                });
            }
        }

        msgs
    })
}
