//! # Route — 交易路由
//!
//! 封装交易对的选择策略和路径发现。
//!
//! - `Route::auto(src, dst)`: 自动发现最优路径（引擎会跑 `route_discover` 选汇率最高的）
//! - `Route::on(src, dst, pair_id)`: 指定直连交易对
//! - `Route::via(src, dst, hops)`: 指定完整多跳路径

use rust_decimal::Decimal;

/// 路由跳：单步兑换路径。
///
/// 表示通过某个交易对将 `src_token` 兑换为 `dst_token`。
#[derive(Clone, Copy, Debug)]
pub struct RouteHop {
    pub pair_id: usize,
    pub src_token: usize,
    pub dst_token: usize,
}

/// 交易路由。
///
/// 可以是一条直连路径（`pair_id = Some`），也可以是多跳路径（`hops` 非空）。
/// `Route::auto` 表示未选择，引擎会自行发现。
#[derive(Clone, Debug)]
pub struct Route {
    /// 路径起点
    pub src_token: usize,
    /// 路径终点
    pub dst_token: usize,
    /// 直连交易对 ID（如果指定）
    pub pair_id: Option<usize>,
    /// 多跳路径（如果存在）
    pub hops: Vec<RouteHop>,
}

impl Route {
    /// 自动发现最优路径（引擎决定）
    pub fn auto(src_token: usize, dst_token: usize) -> Self {
        Self { src_token, dst_token, pair_id: None, hops: Vec::new() }
    }

    /// 指定直连交易对
    pub fn on(src_token: usize, dst_token: usize, pair_id: usize) -> Self {
        Self { src_token, dst_token, pair_id: Some(pair_id), hops: Vec::new() }
    }

    /// 指定完整多跳路径
    pub fn via(src_token: usize, dst_token: usize, hops: Vec<RouteHop>) -> Self {
        Self { src_token, dst_token, pair_id: None, hops }
    }

    /// 返回此路由是否是直连（单跳或无跳）
    pub fn is_direct(&self) -> bool {
        self.pair_id.is_some()
    }

    /// 返回此路由是否有明确的路径（已发现或已指定）
    pub fn is_resolved(&self) -> bool {
        self.pair_id.is_some() || !self.hops.is_empty()
    }

    /// 格式化路径文本（用于日志/调试）
    pub fn display(&self, name_fn: impl Fn(usize) -> Option<String>) -> String {
        if self.pair_id.is_some() {
            let src = name_fn(self.src_token).unwrap_or_default();
            let dst = name_fn(self.dst_token).unwrap_or_default();
            format!("{src}↔{dst} (pair {})", self.pair_id.unwrap())
        } else if !self.hops.is_empty() {
            let hop_strs: Vec<String> = self.hops.iter().map(|h| {
                let s = name_fn(h.src_token).unwrap_or_default();
                let d = name_fn(h.dst_token).unwrap_or_default();
                format!("{s}→{d}")
            }).collect();
            hop_strs.join(" → ")
        } else {
            format!("auto({})→auto({})", self.src_token, self.dst_token)
        }
    }
}
