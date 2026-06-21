//! 多代币多路径压力测试 —— JSON Lines 格式输出。
//!
//! 拓扑:
//!   USDT(quote) ── BTC ── ETH     (三角形环路: USDT→BTC→ETH vs USDT→ETH)
//!       │                       
//!       ├── SOL ── BTC            (多路: USDT→SOL→BTC vs USDT→BTC)
//!       │                   
//!       └── SOL ── DOGE           (多路: USDT→SOL→DOGE vs USDT→DOGE)
//!
//! 操作: 限价单(Make) + 市价单(Swap) + 快速兑换(FastSwap)
//! 异常: 卡死检测、价格尖峰、价差爆裂
//! 输出: 步级 JSONL + 场景摘要

use rand::Rng;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use rpte::Rpte;
use rpte::Route;
use std::collections::VecDeque;
use std::time::Instant;

// ===================== 简易 JSON 构建器 =====================

struct JsonBuilder(Vec<String>);

impl JsonBuilder {
    fn new() -> Self { Self(Vec::new()) }

    fn kv(&mut self, key: &str, value: impl std::fmt::Display) {
        self.0.push(format!("\"{}\":{}", key, value));
    }

    fn kv_str(&mut self, key: &str, value: &str) {
        self.0.push(format!("\"{}\":\"{}\"", key, value));
    }

    fn kv_u64(&mut self, key: &str, value: u64) {
        self.0.push(format!("\"{}\":{}", key, value));
    }

    fn kv_usize(&mut self, key: &str, value: usize) {
        self.0.push(format!("\"{}\":{}", key, value));
    }

    fn kv_f64(&mut self, key: &str, value: f64) {
        self.0.push(format!("\"{}\":{:.6}", key, value));
    }

    fn build(&self) -> String {
        format!("{{{}}}", self.0.join(","))
    }
}

fn d(v: impl Into<f64>) -> Decimal {
    Decimal::from_f64(v.into()).unwrap()
}

// ===================== 代币 & 交易对拓扑 =====================

/// 代币定义
struct TokenDef {
    name: &'static str,
    /// 每个 bot 初始分配的该代币数量
    per_bot: u64,
}

const TOKENS: &[TokenDef] = &[
    TokenDef { name: "USDT", per_bot: 100_000 },
    TokenDef { name: "BTC",  per_bot: 10 },
    TokenDef { name: "ETH",  per_bot: 200 },
    TokenDef { name: "SOL",  per_bot: 2_000 },
    TokenDef { name: "DOGE", per_bot: 500_000 },
];

/// 交易对定义：(quote_token, base_token, initial_price)
const PAIRS: &[(&str, &str, f64)] = &[
    ("USDT", "BTC",  50000.0),
    ("USDT", "ETH",  3000.0),
    ("BTC",  "ETH",  0.06),     // ETH/BTC — 创造 USDT→BTC→ETH 环路
    ("USDT", "SOL",  150.0),
    ("SOL",  "BTC",  0.003),    // SOL/BTC — 多路: USDT→SOL→BTC vs USDT→BTC
    ("USDT", "DOGE", 0.15),
    ("SOL",  "DOGE", 0.001),    // DOGE/SOL — 多路: USDT→SOL→DOGE vs USDT→DOGE
];

/// 多跳路由定义（用于 fast_swap 测试）
const FAST_ROUTES: &[(&str, &str)] = &[
    ("USDT", "ETH"),   // 直连 + 经BTC两跳
    ("USDT", "BTC"),   // 直连 + 经SOL两跳
    ("USDT", "DOGE"),  // 直连 + 经SOL两跳
];

// ===================== 随机参数 =====================

fn random_amount(rng: &mut impl Rng, balance: Decimal) -> Decimal {
    let ratio: f64 = rng.gen_range(0.02..=0.8);
    d(ratio.powf(3.0)) * balance
}

fn random_price_ratio(rng: &mut impl Rng) -> Decimal {
    let r: f64 = rng.gen_range(0.0..0.9);
    d(r.powf(2.0))
}

fn pick_two_tokens(rng: &mut impl Rng, ids: &[usize]) -> (usize, usize) {
    let a = ids[rng.gen_range(0..ids.len())];
    let b = loop {
        let b = ids[rng.gen_range(0..ids.len())];
        if b != a { break b; }
    };
    (a, b)
}

// ===================== Bot 管理器 =====================

struct BotManager {
    tokens: Vec<usize>,
    bots: Vec<usize>,
    max_orders_per_bot: usize,
    token_pairs: Vec<(usize, usize)>,  // 所有已创建的 token pair
    // 统计
    step_swap: usize,
    step_make: usize,
    step_cancel: usize,
    step_fast_swap: usize,
}

impl BotManager {
    fn new() -> Self {
        Self {
            tokens: Vec::new(),
            bots: Vec::new(),
            max_orders_per_bot: 8,
            token_pairs: Vec::new(),
            step_swap: 0,
            step_make: 0,
            step_cancel: 0,
            step_fast_swap: 0,
        }
    }

    fn reset_counts(&mut self) {
        self.step_swap = 0;
        self.step_make = 0;
        self.step_cancel = 0;
        self.step_fast_swap = 0;
    }

    fn step(&mut self, rpte: &mut Rpte) {
        let mut rng = rand::thread_rng();

        for &bot in &self.bots {
            // 超出订单上限 → 随机取消一个
            if let Ok(orders) = rpte.get_account_orders(bot) {
                let ids: Vec<usize> = orders.iter().copied().collect();
                if ids.len() >= self.max_orders_per_bot {
                    let id = ids[rng.gen_range(0..ids.len())];
                    rpte.cancel_order(id);
                    self.step_cancel += 1;
                }
            }

            // 80% 概率跳过（控制频率）
            if rng.gen_range(0.0..=1.0) < 0.8 { continue; }

            // 随机选 src/dst
            let (src_token, dst_token) = pick_two_tokens(&mut rng, &self.tokens);
            let bal = rpte.get_node_balance(bot, src_token).unwrap_or(Decimal::ZERO);
            if bal <= Decimal::ZERO { continue; }

            let volume = random_amount(&mut rng, bal);
            if volume.is_zero() { continue; }

            // 10% 概率触发 fast_swap (多跳)
            let do_fast_swap: bool = rng.gen_range(0.0..=1.0) < 0.10
                && FAST_ROUTES.iter().any(|(s, d)| {
                    rpte.get_token_by_name(s) == Some(src_token)
                    && rpte.get_token_by_name(d) == Some(dst_token)
                });

            if do_fast_swap {
                // 用 auto_select_best_route 发现最优路径，然后用 swap 执行
                match rpte.auto_select_best_route(src_token, dst_token, volume) {
                    Ok(route) => rpte.swap(bot, volume, route),
                    Err(_) => rpte.swap(bot, volume, Route::auto(src_token, dst_token)),
                }
                self.step_fast_swap += 1;
                self.step_swap += 1;
                continue;
            }

            // 50% swap / 50% make
            if rng.gen_bool(0.5) {
                rpte.swap(bot, volume, Route::auto(src_token, dst_token));
                self.step_swap += 1;
            } else {
                // 获取当前价格，在上下浮动范围内放单
                let route = Route::auto(src_token, dst_token);
                if let Ok(prices) = rpte.get_current_price(route.clone()) {
                    if let Some(&(pair_price, quote, _)) = prices.first() {
                        if pair_price.is_zero() { continue; }
                        let price_ratio = random_price_ratio(&mut rng);
                        let price = if src_token == quote {
                            pair_price * (Decimal::ONE - price_ratio)
                        } else {
                            pair_price * (Decimal::ONE + price_ratio)
                        };
                        rpte.make(bot, volume, price, route);
                        self.step_make += 1;
                    }
                }
            }
        }
    }
}

// ===================== 异常检测 =====================

#[derive(Default)]
struct AnomalyTracker {
    prev_price: Option<Decimal>,
    max_price_pct_change: f64,
    max_spread_ratio: f64,
    freeze_steps: Vec<u64>,
    spike_steps: Vec<(u64, f64)>,
    spread_blowout_steps: Vec<(u64, f64)>,
}

impl AnomalyTracker {
    fn check(&mut self, step: u64, price: Decimal, bid: Decimal, ask: Decimal) {
        if let Some(prev) = self.prev_price {
            if !prev.is_zero() {
                let change = ((price - prev) / prev).abs();
                let cf = decimal_to_f64(change);
                if cf > self.max_price_pct_change { self.max_price_pct_change = cf; }
                if cf > 0.50 { self.spike_steps.push((step, cf)); }
            }
        }
        self.prev_price = Some(price);

        if !bid.is_zero() && !ask.is_zero() {
            let spread = ask / bid - Decimal::ONE;
            let sf = decimal_to_f64(spread);
            if sf > self.max_spread_ratio { self.max_spread_ratio = sf; }
            if sf > 5.0 { self.spread_blowout_steps.push((step, sf)); }
        }
    }

    fn report_freeze(&mut self, step: u64) { self.freeze_steps.push(step); }
}

fn decimal_to_f64(d: Decimal) -> f64 {
    d.to_string().parse::<f64>().unwrap_or(0.0)
}

// ===================== 主 =====================

fn main() {
    let num_bots = 150;
    let max_steps = 3000u64;

    // meta
    let mut m = JsonBuilder::new();
    m.kv_str("type", "meta");
    m.kv_str("engine", "rpte");
    m.kv_str("test", "stress_test_v2");
    m.kv_usize("num_bots", num_bots);
    m.kv_u64("max_steps", max_steps);
    m.kv_usize("token_count", TOKENS.len());
    m.kv_usize("pair_count", PAIRS.len());
    m.kv_usize("fast_route_count", FAST_ROUTES.len());
    println!("{}", m.build());

    run_stress(num_bots, max_steps);

    let mut d = JsonBuilder::new();
    d.kv_str("type", "done");
    println!("{}", d.build());
}

fn run_stress(num_bots: usize, max_steps: u64) {
    let mut rpte = Rpte::new("USDT", 6);
    let mut mgr = BotManager::new();

    // ========== 注册代币 ==========
    let usdt = rpte.get_token_by_name("USDT").unwrap();
    mgr.tokens.push(usdt);
    for td in &TOKENS[1..] {
        let id = rpte.register_token(td.name);
        mgr.tokens.push(id);
    }

    // ========== 创建交易对（通过 make+step 触发自动创建）==========
    for &(q_name, b_name, init_price) in PAIRS {
        let q = rpte.get_token_by_name(q_name).unwrap();
        let b = rpte.get_token_by_name(b_name).unwrap();
        // 用第一个 bot 发初始做市单，让 pair 自动创建
        let dummy = rpte.register_account();
        rpte.issue(dummy, q, 1_000_000_000u64).unwrap();
        rpte.issue(dummy, b, 1_000_000_000u64).unwrap();
        rpte.make(dummy, d(1000.0), d(init_price), Route::on(q, b, 0));
        rpte.step();
        // 取消初始做市单
        if let Some(&oid) = rpte.get_all_orders().first() {
            rpte.cancel_order(oid);
        }
    }

    // 记录已创建的 token pair (用于 bot 随机交易)
    for &(q_name, b_name, _) in PAIRS {
        let q = rpte.get_token_by_name(q_name).unwrap();
        let b = rpte.get_token_by_name(b_name).unwrap();
        mgr.token_pairs.push((q, b));
    }

    // ========== 注册 Bot 并发行资产 ==========
    for _ in 0..num_bots {
        let acc = rpte.register_account();
        for td in TOKENS {
            let id = rpte.get_token_by_name(td.name).unwrap();
            let _ = rpte.issue(acc, id, td.per_bot);
        }
        mgr.bots.push(acc);
    }

    // ========== 状态变量 ==========
    let mut anomaly = AnomalyTracker::default();
    let mut step = 0u64;
    let mut max_step_time_us = 0u64;
    let mut total_swap_count = 0usize;
    let mut total_make_count = 0usize;
    let mut total_fast_swap_count = 0usize;
    let mut total_vol_usdt = Decimal::ZERO;
    let mut total_vol_base = Decimal::ZERO;
    let mut prev_tra_log_lens: Vec<usize> = vec![0; PAIRS.len()];
    let mut first = true;

    // ========== 主循环 ==========
    while step < max_steps {
        let start = Instant::now();

        rpte.step();
        mgr.step(&mut rpte);

        let elapsed_us = start.elapsed().as_micros() as u64;
        if elapsed_us > max_step_time_us { max_step_time_us = elapsed_us; }

        // 卡死检测
        if elapsed_us > 10_000_000 {
            anomaly.report_freeze(step);
            let mut a = JsonBuilder::new();
            a.kv_str("type", "anomaly");
            a.kv_str("anomaly_type", "freeze");
            a.kv_u64("step", step);
            a.kv_u64("elapsed_us", elapsed_us);
            println!("{}", a.build());
            break;
        }

        // 采集 USDT↔BTC 指标作为参考
        let price = rpte.get_current_price(Route::auto(usdt, mgr.tokens[1]))
            .unwrap_or_default()
            .first()
            .copied()
            .map(|(p, _, _)| p)
            .unwrap_or(Decimal::ZERO);
        let order_count = rpte.get_all_orders().len();

        // 多级深度（USDT↔BTC）
        let ob1_buy = rpte.get_order_book(Route::auto(usdt, mgr.tokens[1]), 0).ok()
            .and_then(|v| v.into_iter().next())
            .unwrap_or(rpte::OrderBookDepth { price: Decimal::ZERO, volume: Decimal::ZERO });
        let ob1_sell = rpte.get_order_book(Route::auto(mgr.tokens[1], usdt), 0).ok()
            .and_then(|v| v.into_iter().next())
            .unwrap_or(rpte::OrderBookDepth { price: Decimal::ZERO, volume: Decimal::ZERO });

        let spread = if !ob1_buy.price.is_zero() && !ob1_sell.price.is_zero() {
            decimal_to_f64(&ob1_sell.price / &ob1_buy.price - Decimal::ONE)
        } else { f64::NAN };

        // Bot 权益
        let mut total_equity = Vec::new();
        for &tid in &mgr.tokens {
            let name = rpte.get_token_name(tid).unwrap_or("?").to_string();
            let mut eq = Decimal::ZERO;
            for &acc in &mgr.bots {
                eq += rpte.get_account_equity_token(acc, tid).unwrap_or(Decimal::ZERO);
            }
            total_equity.push((name, eq));
        }

        // 成交统计
        if !first {
            for (i, &(q_name, b_name, _)) in PAIRS.iter().enumerate() {
                let q = rpte.get_token_by_name(q_name).unwrap();
                let b = rpte.get_token_by_name(b_name).unwrap();
                if let Ok(logs) = rpte.get_tra_logs(Route::auto(q, b)) {
                    if let Some(v) = logs.into_iter().next() {
                        let len = v.len();
                        let new = len.saturating_sub(prev_tra_log_lens[i]);
                        if new > 0 {
                            for j in (len.saturating_sub(new))..len {
                                if let Some(t) = v.get(j) {
                                    total_vol_usdt += t.volume * t.price;
                                    total_vol_base += t.volume;
                                }
                            }
                        }
                        prev_tra_log_lens[i] = len;
                    }
                }
            }
        }
        first = false;

        total_swap_count += mgr.step_swap;
        total_make_count += mgr.step_make;
        total_fast_swap_count += mgr.step_fast_swap;

        // 异常检测
        anomaly.check(step, price, ob1_buy.price, ob1_sell.price);

        // 输出
        let mut j = JsonBuilder::new();
        j.kv_str("type", "step");
        j.kv_u64("step", step);
        j.kv_f64("price", decimal_to_f64(price));
        j.kv_f64("spread", spread);
        j.kv_f64("ob1_buy_price", decimal_to_f64(ob1_buy.price));
        j.kv_f64("ob1_buy_vol", decimal_to_f64(ob1_buy.volume));
        j.kv_f64("ob1_sell_price", decimal_to_f64(ob1_sell.price));
        j.kv_f64("ob1_sell_vol", decimal_to_f64(ob1_sell.volume));
        j.kv_usize("order_count", order_count);
        for (name, eq) in &total_equity {
            j.kv_f64(&format!("bot_{}_equity", name.to_lowercase()), decimal_to_f64(*eq));
        }
        j.kv_usize("step_swap", mgr.step_swap);
        j.kv_usize("step_make", mgr.step_make);
        j.kv_usize("step_cancel", mgr.step_cancel);
        j.kv_usize("step_fast_swap", mgr.step_fast_swap);
        j.kv_u64("step_time_us", elapsed_us);
        println!("{}", j.build());

        mgr.reset_counts();
        step += 1;
    }

    let final_price = rpte.get_current_price(Route::auto(usdt, mgr.tokens[1]))
        .unwrap_or_default()
        .first()
        .copied()
        .map(|(p, _, _)| p)
        .unwrap_or(Decimal::ZERO);

    let mut s = JsonBuilder::new();
    s.kv_str("type", "summary");
    s.kv_u64("completed_steps", step);
    s.kv_f64("final_price", decimal_to_f64(final_price));
    s.kv_u64("max_step_time_us", max_step_time_us);
    s.kv_f64("max_price_pct_change", anomaly.max_price_pct_change);
    s.kv_f64("max_spread_ratio", anomaly.max_spread_ratio);
    s.kv_usize("freeze_count", anomaly.freeze_steps.len());
    s.kv_usize("spike_count", anomaly.spike_steps.len());
    s.kv_usize("spread_blowout_count", anomaly.spread_blowout_steps.len());
    s.kv_usize("total_make_count", total_make_count);
    s.kv_usize("total_swap_count", total_swap_count);
    s.kv_usize("total_fast_swap_count", total_fast_swap_count);
    s.kv_f64("total_vol_usdt", decimal_to_f64(total_vol_usdt));
    s.kv_f64("total_vol_base", decimal_to_f64(total_vol_base));
    // 收集最终权益数据
    let final_equity: Vec<(String, Decimal)> = {
        let token_ids: Vec<usize> = rpte.get_all_tokens();
        token_ids.iter().filter_map(|&tid| {
            let n = rpte.get_token_name(tid).unwrap_or("?").to_string();
            let mut e = Decimal::ZERO;
            for &acc in &mgr.bots {
                e += rpte.get_account_equity_token(acc, tid).unwrap_or(Decimal::ZERO);
            }
            Some((n, e))
        }).collect()
    };
    for (name, eq) in &final_equity {
        s.kv_f64(&format!("final_bot_{}_equity", name.to_lowercase()), decimal_to_f64(*eq));
    }
    println!("{}", s.build());
}
