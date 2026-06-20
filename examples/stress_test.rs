//! 机器人压力测试 —— 输出为 JSON Lines 格式，适合 AI 工具分析。
//!
//! 特性:
//! - 多价格场景 (price > 1, price < 1, 极低价格)
//! - 每步输出 JSONL 行 (价格、深度、成交量、账户权益、耗时)
//! - 多级深度扫描 (best 1, 3, 5 档)
//! - 异常自动标注 (卡死、价格尖峰、价差异常)
//! - 最终聚合摘要 (JSON)
//! - 无额外依赖

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
    fn new() -> Self {
        Self(Vec::new())
    }

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

// ===================== 随机机器人行为 =====================

/// 生成随机交易参数: (is_swap, amount_ratio, price_ratio)
fn random_bot() -> (bool, Decimal, Decimal) {
    let mut rng = rand::thread_rng();
    let d: f64 = rng.gen_range(0.0..=1.0);
    if d <= 0.25 {
        // Swap
        let amount_ratio: f64 = rng.gen_range(0.05..=0.8);
        (true, Decimal::from_f64(amount_ratio.powf(3.0)).unwrap(), Decimal::ZERO)
    } else {
        // Make
        let amount_ratio: f64 = rng.gen_range(0.1..=0.95);
        let price_ratio: f64 = rng.gen_range(0.0..0.9);
        (
            false,
            Decimal::from_f64(amount_ratio.powf(2.0)).unwrap(),
            Decimal::from_f64(price_ratio.powf(2.0)).unwrap(),
        )
    }
}

// ===================== Bot 管理器 =====================

struct BotManager {
    tokens: Vec<usize>,
    bots: Vec<usize>,
    max_order_ratio: usize,
    // 本步统计数据
    step_swap_count: usize,
    step_make_count: usize,
    step_cancel_count: usize,
}

impl BotManager {
    fn new() -> Self {
        Self {
            tokens: Vec::new(),
            bots: Vec::new(),
            max_order_ratio: 10,
            step_swap_count: 0,
            step_make_count: 0,
            step_cancel_count: 0,
        }
    }

    fn reset_step_counts(&mut self) {
        self.step_swap_count = 0;
        self.step_make_count = 0;
        self.step_cancel_count = 0;
    }

    fn step(&mut self, rpte: &mut Rpte) {
        let mut rng = rand::thread_rng();

        for &bot in &self.bots {
            // 订单数过多时取消一个
            let cancel_target = rpte.get_account_orders(bot).ok().and_then(|order_set| {
                let ids: Vec<usize> = order_set.iter().copied().collect();
                if ids.len() >= self.max_order_ratio {
                    Some(ids[rng.gen_range(0..ids.len())])
                } else {
                    None
                }
            });
            if let Some(order_id) = cancel_target {
                rpte.cancel_order(order_id);
                self.step_cancel_count += 1;
            }

            if rng.gen_range(0.0..=1.0) < 0.8 {
                continue;
            }

            let src_token = self.tokens[rng.gen_range(0..self.tokens.len())];
            let dst_token = loop {
                let dst = self.tokens[rng.gen_range(0..self.tokens.len())];
                if dst != src_token {
                    break dst;
                }
            };

            // get_current_price now returns Vec, take first entry
            let pair_prices = match rpte.get_current_price(Route::auto(src_token, dst_token)) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some((pair_price, quote, _base)) = pair_prices.into_iter().next() else {
                continue;
            };

            let volume = {
                let bal = rpte.get_node_balance(bot, src_token).unwrap_or(Decimal::ZERO);
                let (_, amount_ratio, _) = random_bot();
                amount_ratio * bal
            };
            if volume.is_zero() {
                continue;
            }

            let (is_swap, _, price_ratio) = random_bot();

            if is_swap {
                rpte.swap(bot, volume, Route::auto(src_token, dst_token));
                self.step_swap_count += 1;
            } else {
                let price = if src_token == quote {
                    pair_price * (Decimal::ONE - price_ratio)
                } else {
                    pair_price * (Decimal::ONE + price_ratio)
                };
                rpte.make(bot, volume, price, Route::auto(src_token, dst_token));
                self.step_make_count += 1;
            }
        }
    }
}

// ===================== 场景配置 =====================

struct Scenario {
    label: &'static str,
    usdt_per_bot: u64,
    btc_per_bot: u64,
}

const SCENARIOS: &[Scenario] = &[
    Scenario { label: "price_gt_1",  usdt_per_bot: 1000, btc_per_bot: 10 },
    Scenario { label: "price_lt_1",  usdt_per_bot: 10,   btc_per_bot: 10 },
    Scenario { label: "price_lt_1_2", usdt_per_bot: 5,   btc_per_bot: 10 },
];

// ===================== 异常检测 =====================

#[derive(Default)]
struct AnomalyTracker {
    prev_price: Option<Decimal>,
    max_price_pct_change: f64,      // 最大单步价格变化百分比
    max_spread_ratio: f64,          // 最大买卖价差比 (ask/bid - 1)
    freeze_steps: Vec<u64>,         // 卡死的步号
    spike_steps: Vec<(u64, f64)>,   // (步号, 价格变化百分比)
    spread_blowout_steps: Vec<(u64, f64)>, // (步号, 价差比)
}

impl AnomalyTracker {
    fn check(&mut self, step: u64, price: Decimal, ob_buy_price: Decimal, ob_sell_price: Decimal) {
        // 价格尖峰检测
        if let Some(prev) = self.prev_price {
            if !prev.is_zero() {
                let change = ((price - prev) / prev).abs();
                let change_f64 = decimal_to_f64(change);
                if change_f64 > self.max_price_pct_change {
                    self.max_price_pct_change = change_f64;
                }
                if change_f64 > 0.50 {
                    // 单步变化超过 50%
                    self.spike_steps.push((step, change_f64));
                }
            }
        }
        self.prev_price = Some(price);

        // 价差异常检测
        if !ob_buy_price.is_zero() && !ob_sell_price.is_zero() {
            let spread = ob_sell_price / ob_buy_price - Decimal::ONE;
            let spread_f64 = decimal_to_f64(spread);
            if spread_f64 > self.max_spread_ratio {
                self.max_spread_ratio = spread_f64;
            }
            if spread_f64 > 5.0 {
                // 价差超过 500%
                self.spread_blowout_steps.push((step, spread_f64));
            }
        }
    }

    fn report_freeze(&mut self, step: u64) {
        self.freeze_steps.push(step);
    }
}

fn decimal_to_f64(d: Decimal) -> f64 {
    d.to_string().parse::<f64>().unwrap_or(0.0)
}

// ===================== 主函数 =====================

fn main() {
    // 元数据输出
    let mut meta = JsonBuilder::new();
    meta.kv_str("type", "meta");
    meta.kv_str("engine", "rpte");
    meta.kv_str("test", "stress_test");
    meta.kv_usize("scenario_count", SCENARIOS.len());
    println!("{}", meta.build());

    for scenario in SCENARIOS {
        run_scenario(scenario);
    }

    // 完成标记
    let mut done = JsonBuilder::new();
    done.kv_str("type", "done");
    println!("{}", done.build());
}

fn run_scenario(sc: &Scenario) {
    let num_bots = 200;
    let max_steps = 5000u64;
    let report_interval = 1; // 每步都输出

    // ========== 初始化 ==========
    let mut rpte = Rpte::new("USDT", 5);
    let mut manager = BotManager::new();

    let btc = rpte.register_token("BTC");
    let usdt = rpte.get_token_by_name("USDT").unwrap();
    manager.tokens = vec![btc, usdt];

    for _ in 0..num_bots {
        let acc = rpte.register_account();
        let _ = rpte.issue(acc, usdt, sc.usdt_per_bot);
        let _ = rpte.issue(acc, btc, sc.btc_per_bot);
        manager.bots.push(acc);
    }

    // ========== 状态追踪 ==========
    let mut anomaly = AnomalyTracker::default();
    let mut step = 0u64;
    let mut max_step_time_us = 0u64;
    let mut total_swap_vol_quote = Decimal::ZERO;
    let mut total_swap_vol_base = Decimal::ZERO;
    let mut total_make_count = 0usize;
    let mut total_swap_count = 0usize;
    let mut prev_tra_log_len: Option<usize> = None;

    // 场景开始标记
    let mut scenario_start = JsonBuilder::new();
    scenario_start.kv_str("type", "scenario_start");
    scenario_start.kv_str("label", sc.label);
    scenario_start.kv_u64("usdt_per_bot", sc.usdt_per_bot);
    scenario_start.kv_u64("btc_per_bot", sc.btc_per_bot);
    scenario_start.kv_usize("num_bots", num_bots);
    scenario_start.kv_u64("max_steps", max_steps);
    println!("{}", scenario_start.build());

    // ========== 主循环 ==========
    while step < max_steps {
        let step_start = Instant::now();

        // --- 逐步驱动引擎 + Bot ---
        rpte.step();
        manager.step(&mut rpte);

        let elapsed_us = step_start.elapsed().as_micros() as u64;
        max_step_time_us = max_step_time_us.max(elapsed_us);

        // --- 卡死检测 ---
        if elapsed_us > 10_000_000 {
            anomaly.report_freeze(step);
            // 输出异常行然后 break
            let mut alert = JsonBuilder::new();
            alert.kv_str("type", "anomaly");
            alert.kv_str("anomaly_type", "freeze");
            alert.kv_u64("step", step);
            alert.kv_u64("elapsed_us", elapsed_us);
            println!("{}", alert.build());
            break;
        }

        // --- 采集指标 ---
        let price = rpte.get_current_price(Route::auto(usdt, btc)).unwrap()[0].0;
        let price_f64 = decimal_to_f64(price);

        // 订单总数
        let order_count = rpte.get_all_orders().len();

        // 多级深度: best 1, 3, 5
        let ob1_buy = rpte.get_order_book(Route::auto(usdt, btc), 0)
            .ok()
            .and_then(|v| v.into_iter().next())
            .unwrap_or(rpte::node::OrderBookDepth { price: Decimal::ZERO, volume: Decimal::ZERO });
        let ob1_sell = rpte.get_order_book(Route::auto(btc, usdt), 0)
            .ok()
            .and_then(|v| v.into_iter().next())
            .unwrap_or(rpte::node::OrderBookDepth { price: Decimal::ZERO, volume: Decimal::ZERO });
        let ob3_buy = rpte.get_order_book(Route::auto(usdt, btc), 2)
            .ok()
            .and_then(|v| v.into_iter().next())
            .unwrap_or(rpte::node::OrderBookDepth { price: Decimal::ZERO, volume: Decimal::ZERO });
        let ob3_sell = rpte.get_order_book(Route::auto(btc, usdt), 2)
            .ok()
            .and_then(|v| v.into_iter().next())
            .unwrap_or(rpte::node::OrderBookDepth { price: Decimal::ZERO, volume: Decimal::ZERO });
        let ob5_buy = rpte.get_order_book(Route::auto(usdt, btc), 4)
            .ok()
            .and_then(|v| v.into_iter().next())
            .unwrap_or(rpte::node::OrderBookDepth { price: Decimal::ZERO, volume: Decimal::ZERO });
        let ob5_sell = rpte.get_order_book(Route::auto(btc, usdt), 4)
            .ok()
            .and_then(|v| v.into_iter().next())
            .unwrap_or(rpte::node::OrderBookDepth { price: Decimal::ZERO, volume: Decimal::ZERO });

        // 价差 (best ask / best bid - 1)
        let spread = if !ob1_buy.price.is_zero() && !ob1_sell.price.is_zero() {
            decimal_to_f64(&ob1_sell.price / &ob1_buy.price - Decimal::ONE)
        } else {
            f64::NAN
        };

        // Bot 总权益 (余额 + 挂单)
        let mut total_usdt_equity = Decimal::ZERO;
        let mut total_btc_equity = Decimal::ZERO;
        for &acc in &manager.bots {
            total_usdt_equity += rpte.get_account_equity_token(acc, usdt).unwrap_or(Decimal::ZERO);
            total_btc_equity += rpte.get_account_equity_token(acc, btc).unwrap_or(Decimal::ZERO);
        }

        // 成交追踪
        let tra_logs: VecDeque<rpte::pair::TraLog> = rpte.get_tra_logs(Route::auto(usdt, btc))
            .unwrap_or_default()
            .into_iter()
            .next()
            .unwrap_or_default();
        let tra_log_len = tra_logs.len();
        if let Some(prev) = prev_tra_log_len {
            // 本步新增的成交
            let new_trades = tra_log_len.saturating_sub(prev);
            if new_trades > 0 {
                for i in (tra_log_len.saturating_sub(new_trades))..tra_log_len {
                    if let Some(t) = tra_logs.get(i) {
                        // In TraLog, the volume stored is base volume (BTC)
                        // The price is stored in TraLog.price
                        total_swap_vol_base += t.volume;
                        total_swap_vol_quote += t.volume * t.price;
                    }
                }
            }
        }
        prev_tra_log_len = Some(tra_log_len);

        total_make_count += manager.step_make_count;
        total_swap_count += manager.step_swap_count;

        // --- 异常检测 ---
        anomaly.check(step, price, ob1_buy.price, ob1_sell.price);

        // --- 输出 JSONL ---
        if step % report_interval == 0 {
            let mut j = JsonBuilder::new();
            j.kv_str("type", "step");
            j.kv_str("scenario", sc.label);
            j.kv_u64("step", step);
            j.kv_f64("price", price_f64);
            j.kv_f64("spread", spread);

            // 深度 (best 1/3/5)
            j.kv_f64("ob1_buy_price", decimal_to_f64(ob1_buy.price));
            j.kv_f64("ob1_buy_vol", decimal_to_f64(ob1_buy.volume));
            j.kv_f64("ob1_sell_price", decimal_to_f64(ob1_sell.price));
            j.kv_f64("ob1_sell_vol", decimal_to_f64(ob1_sell.volume));
            j.kv_f64("ob3_buy_vol", decimal_to_f64(ob3_buy.volume));
            j.kv_f64("ob3_sell_vol", decimal_to_f64(ob3_sell.volume));
            j.kv_f64("ob5_buy_vol", decimal_to_f64(ob5_buy.volume));
            j.kv_f64("ob5_sell_vol", decimal_to_f64(ob5_sell.volume));

            j.kv_usize("order_count", order_count);
            j.kv_f64("bot_usdt_equity", decimal_to_f64(total_usdt_equity));
            j.kv_f64("bot_btc_equity", decimal_to_f64(total_btc_equity));
            j.kv_usize("step_swap", manager.step_swap_count);
            j.kv_usize("step_make", manager.step_make_count);
            j.kv_usize("step_cancel", manager.step_cancel_count);
            j.kv_u64("step_time_us", elapsed_us);

            println!("{}", j.build());

            // 如果有异常需要标注，追加输出
            if elapsed_us > 5_000_000 {
                // > 5s 但 < 10s，警告
                let mut warn = JsonBuilder::new();
                warn.kv_str("type", "warning");
                warn.kv_str("warning_type", "slow_step");
                warn.kv_u64("step", step);
                warn.kv_u64("elapsed_us", elapsed_us);
                println!("{}", warn.build());
            }
        }

        manager.reset_step_counts();
        step += 1;
    }

    // ========== 场景摘要输出 ==========
    let final_price = rpte.get_current_price(Route::auto(usdt, btc)).unwrap()[0].0;

    let mut summary = JsonBuilder::new();
    summary.kv_str("type", "scenario_summary");
    summary.kv_str("label", sc.label);
    summary.kv_u64("completed_steps", step);
    summary.kv_f64("final_price", decimal_to_f64(final_price));
    summary.kv_u64("max_step_time_us", max_step_time_us);
    summary.kv_f64("max_price_pct_change", anomaly.max_price_pct_change);
    summary.kv_f64("max_spread_ratio", anomaly.max_spread_ratio);
    summary.kv_usize("freeze_count", anomaly.freeze_steps.len());
    summary.kv_usize("spike_count", anomaly.spike_steps.len());
    summary.kv_usize("spread_blowout_count", anomaly.spread_blowout_steps.len());
    summary.kv_usize("total_make_count", total_make_count);
    summary.kv_usize("total_swap_count", total_swap_count);
    summary.kv_f64("total_trade_vol_quote", decimal_to_f64(total_swap_vol_quote));
    summary.kv_f64("total_trade_vol_base", decimal_to_f64(total_swap_vol_base));
    summary.kv_f64("final_bot_usdt_equity", {
        let mut e = Decimal::ZERO;
        for &acc in &manager.bots {
            e += rpte.get_account_equity_token(acc, usdt).unwrap_or(Decimal::ZERO);
        }
        decimal_to_f64(e)
    });
    summary.kv_f64("final_bot_btc_equity", {
        let mut e = Decimal::ZERO;
        for &acc in &manager.bots {
            e += rpte.get_account_equity_token(acc, btc).unwrap_or(Decimal::ZERO);
        }
        decimal_to_f64(e)
    });

    let freeze_summary: Vec<String> = anomaly.freeze_steps.iter().map(|s| s.to_string()).collect();
    let spike_summary: Vec<String> = anomaly.spike_steps.iter().map(|(s, pct)| format!("{}:{:.2}%", s, pct * 100.0)).collect();
    let spread_summary: Vec<String> = anomaly.spread_blowout_steps.iter().map(|(s, r)| format!("{}:{:.2}%", s, r * 100.0)).collect();

    // 将数组信息放在额外字段中
    summary.0.push(format!("\"freeze_steps\":[{}]", freeze_summary.join(",")));
    if !spike_summary.is_empty() {
        summary.0.push(format!("\"spike_steps\":[\"{}\"]", spike_summary.join("\",\"")));
    }
    if !spread_summary.is_empty() {
        summary.0.push(format!("\"spread_blowout_steps\":[\"{}\"]", spread_summary.join("\",\"")));
    }

    println!("{}", summary.build());
}
