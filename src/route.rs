//! # Route — 交易路由
//!
//! 封装交易对的选择策略。当同一对代币存在多个交易对（普通或虚拟）时，
//! 通过 `Route` 显式指定使用哪一个。

/// 交易路由：指定交易的源和目标代币，以及可选的交易对 ID。
///
/// - `Route::auto(src, dst)`: 自动选择 id 最小的可用交易对
/// - `Route::on(src, dst, pair_id)`: 指定使用某个交易对，验证不通过会报错
pub struct Route {
    pub src_token: usize,
    pub dst_token: usize,
    /// 指定交易对 ID。`None` = 自动选择 id 最小的可用交易对。
    pub pair_id: Option<usize>,
}

impl Route {
    /// 自动选择交易对（选 id 最小的）
    pub fn auto(src_token: usize, dst_token: usize) -> Self {
        Self { src_token, dst_token, pair_id: None }
    }

    /// 指定使用某个交易对
    pub fn on(src_token: usize, dst_token: usize, pair_id: usize) -> Self {
        Self { src_token, dst_token, pair_id: Some(pair_id) }
    }
}
