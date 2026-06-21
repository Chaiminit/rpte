use rand::Rng;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use rpte::tui;
use rpte::Rpte;
use rpte::LendingPreset;

pub fn random_bot() -> (bool, Decimal, Decimal) {
    let mut rng = rand::thread_rng();
    let d: f64 = rng.gen_range(0.0..=1.0);
    if d <= 0.2 {
        let amount_ratio: f64 = rng.gen_range(0.05..=0.8);
        let amount_ratio = amount_ratio.powf(3.0);
        let price_ratio = Decimal::ZERO;
        return (true, Decimal::from_f64(amount_ratio).unwrap(), price_ratio);
    } else {
        let amount_ratio: f64 = rng.gen_range(0.1..=0.95);
        let amount_ratio = amount_ratio.powf(2.0);
        let price_ratio: f64 = rng.gen_range(0.0..0.9);
        let price_ratio = price_ratio.powf(2.0);
        return (false, Decimal::from_f64(amount_ratio).unwrap(), Decimal::from_f64(price_ratio).unwrap());
    }
}


pub struct RandomBotManager {
    bots: Vec<usize>,
    max_order_ratio: usize,
    // token IDs
    usdt_token: usize,
    btc_token: usize,
    eth_token: usize,
    ausdt_token: usize,
    abtc_token: usize,
    dusdt_token: usize,
    dbtc_token: usize,
}


impl RandomBotManager {
    pub fn new() -> Self {
        Self {
            bots: Vec::new(),
            max_order_ratio: 10,
            usdt_token: 0,
            btc_token: 0,
            eth_token: 0,
            ausdt_token: 0,
            abtc_token: 0,
            dusdt_token: 0,
            dbtc_token: 0,
        }
    }

    pub fn set_lending_tokens(&mut self, usdt: usize, btc: usize, eth: usize, ausdt: usize, abtc: usize, dusdt: usize, dbtc: usize) {
        self.usdt_token = usdt;
        self.btc_token = btc;
        self.eth_token = eth;
        self.ausdt_token = ausdt;
        self.abtc_token = abtc;
        self.dusdt_token = dusdt;
        self.dbtc_token = dbtc;
    }

    pub fn add_bot(&mut self, bot: usize) {
        self.bots.push(bot);
    }

    /// 生成当前 bot 可用的交易路线，返回 (src, dst, swap_only)
    /// 分离常规交易和合约交互，给合约路线低权重
    fn get_available_routes(&self, rpte: &mut Rpte, bot: usize) -> Vec<(usize, usize, bool)> {
        let zero = Decimal::ZERO;
        let eps = Decimal::new(1, 6);
        let usdt_bal = rpte.get_node_balance(bot, self.usdt_token).unwrap_or(zero);
        let btc_bal  = rpte.get_node_balance(bot, self.btc_token).unwrap_or(zero);
        let ausdt_bal = rpte.get_node_balance(bot, self.ausdt_token).unwrap_or(zero);
        let abtc_bal = rpte.get_node_balance(bot, self.abtc_token).unwrap_or(zero);
        let dusdt_bal = rpte.get_node_balance(bot, self.dusdt_token).unwrap_or(zero);
        let dbtc_bal = rpte.get_node_balance(bot, self.dbtc_token).unwrap_or(zero);

        let mut routes = Vec::new();

        // ── 常规交易路线（高权重，~90%） ──
        // 每个符合条件的路线重复 9 次，提高选中概率
        if usdt_bal > eps {
            for _ in 0..9 { routes.push((self.usdt_token, self.btc_token, false)); }
            // ETH 交易
            for _ in 0..6 { routes.push((self.usdt_token, self.eth_token, false)); }
            if dusdt_bal < -eps {
                for _ in 0..1 { routes.push((self.usdt_token, self.dusdt_token, true)); } // 还款
            }
        }
        if btc_bal > eps {
            for _ in 0..9 { routes.push((self.btc_token, self.usdt_token, false)); }
            if dbtc_bal < -eps {
                for _ in 0..1 { routes.push((self.btc_token, self.dbtc_token, true)); } // 还 BTC
            }
        }

        // ── 合约交互路线（低权重，~10%） ──
        // 每个路线只出现一次
        if usdt_bal > eps {
            routes.push((self.usdt_token, self.ausdt_token, false)); // 存款
        }
        if btc_bal > eps {
            routes.push((self.btc_token, self.abtc_token, true));    // 存质押
        }
        // aUSDT 也可作为质押：取款、借 USDT、借 BTC
        if ausdt_bal > eps {
            routes.push((self.ausdt_token, self.usdt_token, false)); // 取款
            routes.push((self.dusdt_token, self.usdt_token, true));  // 借 USDT（用 aUSDT 质押）
            routes.push((self.dbtc_token, self.btc_token, true));    // 借 BTC（用 aUSDT 质押）
        }
        // aBTC 作为质押：取质押、借 USDT、借 BTC
        if abtc_bal > eps {
            routes.push((self.abtc_token, self.btc_token, true));    // 取质押
            routes.push((self.dusdt_token, self.usdt_token, true));  // 借 USDT（用 aBTC 质押）
            routes.push((self.dbtc_token, self.btc_token, true));    // 借 BTC（用 aBTC 质押）
        }

        routes
    }

    pub fn step(&mut self, rpte: &mut Rpte) {
        let mut rng = rand::thread_rng();
        for bot in &self.bots {
            // 取消超量订单
            let cancel_target = rpte.get_account_orders(*bot).ok().and_then(|order_set| {
                let ids: Vec<usize> = order_set.iter().copied().collect();
                if ids.len() >= self.max_order_ratio {
                    Some(ids[rng.gen_range(0..ids.len())])
                } else {
                    None
                }
            });
            if let Some(order_id) = cancel_target {
                rpte.cancel_order(order_id);
            }

            if rng.gen_range(0.0..=1.0) < 0.8 {
                continue;
            }

            let routes = self.get_available_routes(rpte, *bot);
            if routes.is_empty() {
                continue;
            }
            let (src_token, dst_token, swap_only) = &routes[rng.gen_range(0..routes.len())];
            let src_token = *src_token;
            let dst_token = *dst_token;
            let swap_only = *swap_only;

            let (is_swap, amount_ratio, price_ratio) = random_bot();

            // 计算交易量
            let volume = if src_token == self.dusdt_token && dst_token == self.usdt_token {
                // 借 USDT：以 USDT 余额为参考
                let ref_bal = rpte.get_node_balance(*bot, self.usdt_token).unwrap_or(Decimal::ZERO);
                if ref_bal > Decimal::ZERO {
                    amount_ratio * ref_bal
                } else {
                    amount_ratio * Decimal::new(1000, 0)
                }
            } else if src_token == self.dbtc_token && dst_token == self.btc_token {
                // 借 BTC：以 BTC 余额为参考
                let ref_bal = rpte.get_node_balance(*bot, self.btc_token).unwrap_or(Decimal::ZERO);
                if ref_bal > Decimal::ZERO {
                    amount_ratio * ref_bal
                } else {
                    amount_ratio * Decimal::new(1, 0) // 首次借 1 BTC
                }
            } else {
                let bal = rpte.get_node_balance(*bot, src_token).unwrap_or(Decimal::ZERO);
                amount_ratio * bal
            };

            if volume <= Decimal::ZERO {
                continue;
            }

            // swap_only 对强制市价单，其他随机市价/限价
            if swap_only || is_swap {
                rpte.swap(*bot, volume, rpte::Route::auto(src_token, dst_token));
            } else {
                let prices = rpte.get_current_price(rpte::Route::auto(src_token, dst_token)).unwrap();
                let (pair_price, quote, _base) = prices.into_iter().next().unwrap_or((Decimal::ZERO, 0, 0));
                let price = if src_token == quote {
                    pair_price * (Decimal::ONE - price_ratio)
                } else {
                    pair_price * (Decimal::ONE + price_ratio)
                };
                rpte.make(*bot, volume, price, rpte::Route::auto(src_token, dst_token));
            }
        }
    }
}


fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut rpte = Rpte::new("USDT", 8);
    let mut bot_manager = RandomBotManager::new();

    let btc_token = rpte.register_token("BTC");
    let usdt_token = rpte.get_token_by_name("USDT").unwrap();
    let eth_token = rpte.register_token("ETH");

    for _i in 0..400 {
        let account = rpte.register_account();
        let _ = rpte.issue(account, usdt_token, 100000000u64);
        let _ = rpte.issue(account, btc_token, 1000u64);
        let _ = rpte.issue(account, eth_token, 4000000u64);
        bot_manager.add_bot(account);
    }

    let player = rpte.register_account();

    // 部署双向借贷合约（USDT + BTC 双池，交叉质押）
    let lending = LendingPreset::new_bidirectional(
        usdt_token,      // asset_token_a
        btc_token,       // asset_token_b
        "aUSDT",         // receipt_a_name
        "aBTC",          // receipt_b_name
        "dUSDT",         // debt_a_name
        "dBTC",          // debt_b_name
        Decimal::new(130, 2),  // min_collateral_ratio = 1.30
        Decimal::new(110, 2),  // liquidation_threshold = 1.10
    );
    let (on_create, on_update, on_end, on_called_fns) = lending.build();
    rpte.deploy(player, "USDT/BTC Lending", on_create, on_update, on_end, on_called_fns);
    // 跑两帧：第一帧处理 CreateContract 消息，第二帧触发 on_create 注册凭证代币
    rpte.step();
    rpte.step();

    // 将借贷凭证代币 ID 注册到机器人管理器
    let ausdt_token = rpte.get_token_by_name("aUSDT").unwrap();
    let abtc_token = rpte.get_token_by_name("aBTC").unwrap();
    let dusdt_token = rpte.get_token_by_name("dUSDT").unwrap();
    let dbtc_token = rpte.get_token_by_name("dBTC").unwrap();
    bot_manager.set_lending_tokens(usdt_token, btc_token, eth_token, ausdt_token, abtc_token, dusdt_token, dbtc_token);

    // ── 触发创建 USDT↔ETH 交易对 ──
    let _ = rpte.get_current_price(rpte::Route::auto(usdt_token, eth_token)).unwrap();
    rpte.step();

    // ── 给 USDT-BTC 交易对设手续费，收给 player ──
    use rpte::taker_maker_fee;
    // 先触发自动创建 USDT-BTC 交易对
    let _ = rpte.get_current_price(rpte::Route::auto(usdt_token, btc_token)).unwrap();
    rpte.step();
    // 找到刚创建的 USDT-BTC 交易对
    let pairs = rpte.get_all_pairs_info();
    for (pid, quote, base, _) in &pairs {
        if *quote == usdt_token && *base == btc_token {
            let fee = taker_maker_fee(
                Decimal::new(3, 10),  // 万分之0.1 ≈ 0.001%
                Decimal::new(1, 10),         // maker 免费
                player,
            );
            rpte.set_pair_fee(*pid, Some(fee)).unwrap();
            eprintln!("[Fee] USDT-BTC pair {} fee set, collecting to player {}", pid, player);
            break;
        }
    }

    tui::run_tui(&mut rpte, 20, 120, 10, Some(player), |eng| {
        bot_manager.step(eng);
    })?;

    Ok(())
}
