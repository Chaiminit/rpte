//! # Fee — 手续费系统
//!
//! 提供手续费闭包类型 `FeeFn`，通过闭包注入模式让 Pair 和 VirtualPair
//! 在每笔成交后收取手续费。

use std::sync::Arc;
use rust_decimal::Decimal;
use crate::node::{Msg, EngineReader};

/// 手续费调用上下文：包含一笔成交的所有信息。
#[derive(Debug, Clone)]
pub struct FeeCtx {
    /// base token ID（买方收到的代币）
    pub base_token: usize,
    /// quote token ID（卖方收到的代币）
    pub quote_token: usize,
    /// 收到 base_token 的节点 ID（买方节点）
    pub buyer_node: usize,
    /// 收到 quote_token 的节点 ID（卖方节点）
    pub seller_node: usize,
    /// 主动吃单的节点 ID（市价单发起方）
    pub taker_node: usize,
    /// 被动挂单的节点 ID（限价单）
    pub maker_node: usize,
    /// 成交的 base_token 数量
    pub base_volume: Decimal,
    /// 成交的 quote_token 数量
    pub quote_volume: Decimal,
}

/// 手续费闭包：在每笔成交后调用，返回额外的 `Msg`（通常为 `Msg::Transfer`），
/// 用于将一部分成交代币发送到若干手续费收集节点。
///
/// # 参数
///
/// | 参数 | 类型 | 含义 |
/// |------|------|------|
/// | `reader` | `&dyn EngineReader` | 引擎只读视图，可用于查询余额、价格等 |
/// | `ctx` | `FeeCtx` | 成交上下文 |
pub type FeeFn = Arc<dyn Fn(&dyn EngineReader, FeeCtx) -> Vec<Msg> + Send + Sync>;
