use rand::Rng;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use rpte::Rpte;

pub fn random_bot() -> (bool, Decimal, Decimal) {
    let mut rng = rand::thread_rng();
    let d: f64 = rng.gen_range(0.0..=1.0);
    if d <= 0.2 {
        let amount_ratio: f64 = rng.gen_range(0.05..=0.95);
        let amount_ratio = amount_ratio.powf(2.5);
        let price_ratio = Decimal::ZERO;
        return (true, Decimal::from_f64(amount_ratio).unwrap(), price_ratio);
    } else {
        let amount_ratio: f64 = rng.gen_range(0.05..=0.95);
        let amount_ratio = amount_ratio.powf(2.0);
        let price_ratio: f64 = rng.gen_range(0.0..0.9);
        let price_ratio = price_ratio.powf(3.0);
        return (false, Decimal::from_f64(amount_ratio).unwrap(), Decimal::from_f64(price_ratio).unwrap());
    }
}


pub struct RandomBotManager {
    tokens: Vec<usize>,
    bots: Vec<usize>,
    max_order_num: usize,
}


impl RandomBotManager {
    pub fn new() -> Self {
        Self {
            tokens: Vec::new(),
            bots: Vec::new(),
            max_order_num: 1000,
        }
    }

    pub fn add_bot(&mut self, bot: usize) {
        self.bots.push(bot);
    }

    pub fn add_token(&mut self, token: usize) {
        self.tokens.push(token);
    }

    pub fn step(&mut self, rpte: &mut Rpte) {
        if self.tokens.len() < 2 {
            return;
        }
        let mut rng = rand::thread_rng();
        for bot in &self.bots {
            let src_token = self.tokens[rng.gen_range(0..self.tokens.len())];
            let dst_token = loop {
                let dst = self.tokens[rng.gen_range(0..self.tokens.len())];
                if dst != src_token {
                    break dst;
                }
            };
            let (is_swap, amount_ratio, price_ratio) = random_bot();
            let (pair_price, quote, _base) = rpte.get_current_price(src_token, dst_token).unwrap();

            let volume = amount_ratio * rpte.get_node_balance(*bot, src_token).unwrap();
            let price = if src_token == quote {
                pair_price * (Decimal::ONE + price_ratio)
            } else {
                pair_price * (Decimal::ONE - price_ratio)
            };

            if is_swap {
                rpte.swap(*bot, src_token, dst_token, volume);
            } else {
                rpte.make(*bot, src_token, dst_token, volume, price);
            }
        }
    }
}


fn main() {
    let mut rpte = Rpte::new("USDT", 5);
    let mut bot_manager = RandomBotManager::new();

    let btc_token = rpte.register_token("BTC");
    let usdt_token = rpte.get_token_by_name("USDT").unwrap();
    bot_manager.add_token(btc_token);
    bot_manager.add_token(usdt_token);

    for _i in 0..50 {
        let account = rpte.register_account();
        let _ = rpte.issue(account, usdt_token, Decimal::new(10000, 0));
        let _ = rpte.issue(account, btc_token, Decimal::new(1, 0));
        bot_manager.add_bot(account);
    }

    for _i in 0..10 {
        rpte.step();
        rpte.step();
        bot_manager.step(&mut rpte);
        let (price, _, _) = rpte.get_current_price(btc_token, usdt_token).unwrap();
        println!("btc price: {}", price);
    }
}
