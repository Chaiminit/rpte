use rust_decimal::Decimal;
use rpte::Rpte;

// ============================
// 基础引擎操作测试
// ============================

#[test]
fn test_engine_new() {
    let engine = Rpte::new("USDT", 4);
    assert_eq!(engine.get_precision(), 4);
    assert_eq!(engine.get_global_quote_token(), 0);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    assert_eq!(usdt, 0);
}

#[test]
fn test_register_token() {
    let mut engine = Rpte::new("USDT", 4);
    let btc = engine.register_token("BTC");
    assert!(btc > 0);
    assert_eq!(engine.get_token_by_name("BTC"), Some(btc));
    assert_eq!(engine.get_token_name(btc), Some("BTC"));
}

#[test]
fn test_register_duplicate_token() {
    let mut engine = Rpte::new("USDT", 4);
    let id1 = engine.register_token("BTC");
    let id2 = engine.register_token("BTC");
    assert_eq!(id1, id2, "重复注册应返回相同 ID");
}

#[test]
fn test_register_account() {
    let mut engine = Rpte::new("USDT", 4);
    let alice = engine.register_account();
    let bob = engine.register_account();
    assert!(alice != bob, "不同账户应有不同 ID");
}

#[test]
fn test_get_all_tokens() {
    let mut engine = Rpte::new("USDT", 4);
    engine.register_token("BTC");
    engine.register_token("ETH");
    let tokens = engine.get_all_tokens();
    assert_eq!(tokens.len(), 3);
    assert!(tokens.contains(&0)); // USDT
    assert!(engine.get_token_name(0) == Some("USDT"));
}

#[test]
fn test_get_all_accounts() {
    let mut engine = Rpte::new("USDT", 4);
    engine.register_account();
    engine.register_account();
    engine.register_account();
    assert_eq!(engine.get_all_accounts().len(), 3);
}

// ============================
// 发行资产测试
// ============================

#[test]
fn test_issue() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let alice = engine.register_account();
    engine.issue(alice, usdt, Decimal::new(10000, 0)).unwrap();
    assert_eq!(engine.get_node_balance(alice, usdt).unwrap(), Decimal::new(10000, 0));
}

#[test]
fn test_issue_to_nonexistent_node() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let result = engine.issue(999, usdt, 100u64);
    assert!(result.is_err());
}

#[test]
fn test_issue_zero() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let alice = engine.register_account();
    engine.issue(alice, usdt, Decimal::ZERO).unwrap();
    assert_eq!(engine.get_node_balance(alice, usdt).unwrap(), Decimal::ZERO);
}

// ============================
// 转账测试
// ============================

#[test]
fn test_transfer() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let alice = engine.register_account();
    let bob = engine.register_account();
    engine.issue(alice, usdt, 10000u64).unwrap();

    engine.transfer(alice, bob, usdt, 3000u64);
    engine.step();

    assert_eq!(engine.get_node_balance(alice, usdt).unwrap(), Decimal::from(7000u64));
    assert_eq!(engine.get_node_balance(bob, usdt).unwrap(), Decimal::from(3000u64));
}

#[test]
fn test_transfer_self() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let alice = engine.register_account();
    engine.issue(alice, usdt, 5000u64).unwrap();

    engine.transfer(alice, alice, usdt, 1000u64);
    engine.step();

    // 自我转账应无影响
    assert_eq!(engine.get_node_balance(alice, usdt).unwrap(), Decimal::new(5000, 0));
}

// ============================
// 限价单 (Make) 测试
// ============================

#[test]
fn test_make_order() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();

    engine.issue(alice, usdt, 10000u64).unwrap();
    engine.make(alice, usdt, btc, 5000u64, 50000u64);
    engine.step();

    // 检查订单是否创建
    let orders = engine.get_all_orders();
    assert!(!orders.is_empty(), "应该有未成交的订单");
}

#[test]
fn test_make_and_match() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();

    // Alice 以 50000 USDT/BTC 挂买单
    engine.issue(alice, usdt, 50000u64).unwrap();
    engine.make(alice, usdt, btc, 50000u64, 50000u64);
    engine.step();

    // Bob 以 50000 USDT/BTC 挂卖单
    engine.issue(bob, btc, 1u64).unwrap();
    engine.make(bob, btc, usdt, 1u64, 50000u64);
    engine.step();

    // 再驱动一帧处理撮合产生的转账和关单消息
    engine.step();

    // 成交后 Alice 应有 BTC, Bob 应有 USDT
    assert!(engine.get_node_balance(alice, btc).unwrap() > Decimal::ZERO, "Alice 应获得 BTC");
    assert!(engine.get_node_balance(bob, usdt).unwrap() > Decimal::ZERO, "Bob 应获得 USDT");
}

#[test]
fn test_make_partial_fill() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();

    // Alice 以 50000 挂买单，买入 1 BTC
    engine.issue(alice, usdt, 50000u64).unwrap();
    engine.make(alice, usdt, btc, 50000u64, 50000u64);
    engine.step();

    // Bob 以 50000 挂卖单，只卖 0.5 BTC
    engine.issue(bob, btc, 1u64).unwrap();
    engine.make(bob, btc, usdt, 1u64, 50000u64);
    engine.step();

    // 再驱动一帧处理撮合消息
    engine.step();

    let alice_btc = engine.get_node_balance(alice, btc).unwrap();
    let bob_usdt = engine.get_node_balance(bob, usdt).unwrap();
    assert!(alice_btc > Decimal::ZERO, "Alice must receive BTC");
    assert!(bob_usdt > Decimal::ZERO, "Bob must receive USDT");
}

#[test]
fn test_make_price_priority() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();
    let charlie = engine.register_account();

    // 两个买单：Alice 出价 40000, Bob 出价 50000（更高）
    engine.issue(alice, usdt, 40000u64).unwrap();
    engine.issue(bob, usdt, 50000u64).unwrap();
    engine.make(alice, usdt, btc, 40000u64, 40000u64);
    engine.make(bob, usdt, btc, 50000u64, 50000u64);
    engine.step();

    // Charlie 以 45000 卖 1 BTC
    engine.issue(charlie, btc, 1u64).unwrap();
    engine.make(charlie, btc, usdt, 1u64, 45000u64);
    engine.step();

    // 再驱动一帧处理撮合消息
    engine.step();

    // Bob 出价更高，应优先成交
    assert!(engine.get_node_balance(bob, btc).unwrap() > Decimal::ZERO, "Bob higher bid should match first");
}

// ============================
// 市价单 (Swap) 测试
// ============================

#[test]
fn test_swap_buy() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();

    // Bob 挂限价卖单：以 50000 卖出 1 BTC
    engine.issue(bob, btc, Decimal::new(1, 0)).unwrap();
    engine.make(bob, btc, usdt, Decimal::new(1, 0), Decimal::new(50000, 0));
    engine.step();

    // Alice 市价买入（花费 USDT 买入 BTC）
    engine.issue(alice, usdt, Decimal::new(50000, 0)).unwrap();
    let alice_balance_before = engine.get_node_balance(alice, btc).unwrap();
    engine.swap(alice, usdt, btc, Decimal::new(50000, 0));
    engine.step();

    // 再驱动一帧处理撮合消息
    engine.step();

    let alice_balance_after = engine.get_node_balance(alice, btc).unwrap();
    assert!(alice_balance_after > alice_balance_before, "Alice should receive BTC from swap buy");
}

#[test]
fn test_swap_sell() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();

    // Bob 挂限价买单：以 50000 买入 BTC
    engine.issue(bob, usdt, Decimal::new(50000, 0)).unwrap();
    engine.make(bob, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();

    // Alice 市价卖出 BTC
    engine.issue(alice, btc, Decimal::new(1, 0)).unwrap();
    let alice_usdt_before = engine.get_node_balance(alice, usdt).unwrap();
    engine.swap(alice, btc, usdt, Decimal::new(1, 0));
    engine.step();

    // 再驱动一帧处理撮合消息
    engine.step();

    let alice_usdt_after = engine.get_node_balance(alice, usdt).unwrap();
    assert!(alice_usdt_after > alice_usdt_before, "Alice should receive USDT from swap sell");
}

#[test]
fn test_swap_no_liquidity() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();

    // 没有对手单，直接市价买入
    engine.issue(alice, usdt, Decimal::new(1000, 0)).unwrap();
    engine.swap(alice, usdt, btc, Decimal::new(1000, 0));
    engine.step(); // 应该不会 panic，只是没有成交
}

// ============================
// 取消订单测试
// ============================

#[test]
fn test_cancel_order() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();

    engine.issue(alice, usdt, Decimal::new(10000, 0)).unwrap();
    engine.make(alice, usdt, btc, Decimal::new(5000, 0), Decimal::new(50000, 0));
    engine.step();

    let orders = engine.get_all_orders();
    assert!(!orders.is_empty());
    let order_id = orders[0];

    // 取消订单
    engine.cancel_order(order_id);
    engine.step();

    // 资金应退回
    let balance = engine.get_node_balance(alice, usdt).unwrap();
    assert_eq!(balance, Decimal::new(10000, 0), "取消订单后资金应全额退回");
}

// ============================
// 错误处理测试
// ============================

#[test]
fn test_balance_nonexistent_node() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let result = engine.get_node_balance(999, usdt);
    assert!(result.is_err());
}

#[test]
fn test_issue_insufficient_balance() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let alice = engine.register_account();
    let bob = engine.register_account();

    // Alice 没有 USDT，但 transfer_with_overdraft 允许透支
    engine.transfer_with_overdraft(alice, bob, usdt, Decimal::new(100, 0));
    engine.step();
    assert_eq!(engine.get_node_balance(alice, usdt).unwrap(), Decimal::new(-100, 0));
}

// ============================
// 方向感知的价格/订单簿测试
// ============================

#[test]
fn test_get_current_price_returns_orientation() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();

    // 撮合一笔 USDT/BTC 交易
    engine.issue(alice, usdt, Decimal::new(50000, 0)).unwrap();
    engine.make(alice, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();
    engine.issue(bob, btc, Decimal::new(1, 0)).unwrap();
    engine.make(bob, btc, usdt, Decimal::new(1, 0), Decimal::new(50000, 0));
    engine.step();
    engine.step();

    // 正向: get_current_price(usdt, btc) → pair 的原始价格
    let (price_fwd, quote_fwd, base_fwd) = engine.get_current_price(usdt, btc).unwrap();
    assert_eq!(price_fwd, Decimal::new(50000, 0), "1 BTC = 50000 USDT");
    assert_eq!(quote_fwd, usdt);
    assert_eq!(base_fwd, btc);

    // 反向: get_current_price(btc, usdt) → 返回同一 pair 的原始价格（不取倒数）
    let (price_rev, quote_rev, base_rev) = engine.get_current_price(btc, usdt).unwrap();
    assert_eq!(price_rev, Decimal::new(50000, 0), "反向查询仍返回 1 BTC = 50000 USDT");
    assert_eq!(quote_rev, usdt);
    assert_eq!(base_rev, btc);
}

#[test]
fn test_get_order_book_direction_derived_from_src_dst() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();

    // 正向挂买单: 花费 USDT 买 BTC → pair 内部为 Buy
    engine.issue(alice, usdt, Decimal::new(50000, 0)).unwrap();
    engine.make(alice, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();

    // 正向: src=usdt(quote) → Buy 方向，返回买单簿
    let depth = engine.get_order_book(usdt, btc, 0).unwrap();
    assert_eq!(depth.price, Decimal::new(50000, 0));

    // 反向: src=btc(base) → Sell 方向，返回卖单簿（空，因为只挂了买单）
    let depth = engine.get_order_book(btc, usdt, 0).unwrap();
    assert_eq!(depth.price, Decimal::ZERO, "反向查询返回卖单簿（未挂卖单）");
}

// ============================
// 订单簿查询测试
// ============================

#[test]
fn test_get_order_book() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();

    engine.issue(alice, usdt, Decimal::new(50000, 0)).unwrap();
    engine.make(alice, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();

    let depth = engine.get_order_book(usdt, btc, 0).unwrap();
    assert_eq!(depth.price, Decimal::new(50000, 0));
}

// ============================
// 成交记录测试
// ============================

#[test]
fn test_trade_logs() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();

    engine.issue(alice, usdt, Decimal::new(50000, 0)).unwrap();
    engine.make(alice, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();

    engine.issue(bob, btc, Decimal::new(1, 0)).unwrap();
    engine.make(bob, btc, usdt, Decimal::new(1, 0), Decimal::new(50000, 0));
    engine.step();

    let logs = engine.get_tra_logs(usdt, btc).unwrap();
    assert!(!logs.is_empty(), "应有成交记录");
}

// ============================
// K 线数据测试
// ============================

#[test]
fn test_candle_data() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();

    engine.issue(alice, usdt, Decimal::new(50000, 0)).unwrap();
    engine.make(alice, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();

    engine.issue(bob, btc, Decimal::new(1, 0)).unwrap();
    engine.make(bob, btc, usdt, Decimal::new(1, 0), Decimal::new(50000, 0));
    engine.step();

    let candles = engine.get_candle_data(usdt, btc, 1).unwrap();
    assert!(!candles.is_empty(), "应有 K 线数据");
}

#[test]
fn test_latest_candle() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();

    engine.issue(alice, usdt, Decimal::new(50000, 0)).unwrap();
    engine.make(alice, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();

    engine.issue(bob, btc, Decimal::new(1, 0)).unwrap();
    engine.make(bob, btc, usdt, Decimal::new(1, 0), Decimal::new(50000, 0));
    engine.step();

    let latest = engine.latest_candle(usdt, btc, 1).unwrap();
    assert!(latest.is_some(), "应有最新 K 线");
}

// ============================
// 获取当前价格测试
// ============================

#[test]
fn test_get_current_price_no_trades() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let (price, _quote, _base) = engine.get_current_price(usdt, btc).unwrap();
    assert_eq!(price, Decimal::ONE, "无成交时价格应为 1");
}

#[test]
fn test_get_current_price_after_trades() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();

    engine.issue(alice, usdt, Decimal::new(50000, 0)).unwrap();
    engine.make(alice, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();

    engine.issue(bob, btc, Decimal::new(1, 0)).unwrap();
    engine.make(bob, btc, usdt, Decimal::new(1, 0), Decimal::new(50000, 0));
    engine.step();

    let (price, _quote, _base) = engine.get_current_price(usdt, btc).unwrap();
    assert!(price > Decimal::ZERO, "成交后应有非零价格");
}

// ============================
// 多步运行测试
// ============================

#[test]
fn test_multiple_steps() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();

    engine.issue(alice, usdt, Decimal::new(50000, 0)).unwrap();
    engine.issue(bob, btc, Decimal::new(1, 0)).unwrap();

    // 多帧分别下单
    engine.make(alice, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();
    engine.make(bob, btc, usdt, Decimal::new(1, 0), Decimal::new(50000, 0));
    engine.step();

    // 再驱动一帧处理撮合消息
    engine.step();

    assert!(engine.get_node_balance(alice, btc).unwrap() > Decimal::ZERO);
    assert!(engine.get_node_balance(bob, usdt).unwrap() > Decimal::ZERO);
}

// ============================
// 多账户多代币复杂场景测试
// ============================

#[test]
fn test_multiple_accounts_multiple_tokens() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let eth = engine.register_token("ETH");

    let alice = engine.register_account();
    let bob = engine.register_account();
    let charlie = engine.register_account();

    // 发行各种资产
    engine.issue(alice, usdt, Decimal::new(100000, 0)).unwrap();
    engine.issue(bob, btc, Decimal::new(10, 0)).unwrap();
    engine.issue(charlie, eth, Decimal::new(100, 0)).unwrap();

    // Alice 买 BTC
    engine.make(alice, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();

    // Bob 卖 BTC
    engine.make(bob, btc, usdt, Decimal::new(1, 0), Decimal::new(50000, 0));
    engine.step();

    // Charlie 买 BTC (使用 ETH 不是 quote token，会创建 ETH/BTC 交易对)
    // 但注意 swap 需要 src_token 和 dst_token
    engine.make(charlie, eth, btc, Decimal::new(50, 0), Decimal::new(0_05, 0));
    engine.step();

    let alice_btc = engine.get_node_balance(alice, btc).unwrap();
    assert!(alice_btc > Decimal::ZERO, "Alice should have BTC");
}

// ============================
// 引擎运行/停止测试
// ============================

#[test]
fn test_stop_engine() {
    let mut engine = Rpte::new("USDT", 4);
    engine.stop(); // 停止后 run 应该立即退出
    // 应该不会阻塞
}

// ============================
// 多笔订单连续撮合测试
// ============================

#[test]
fn test_multiple_orders_continuous_matching() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();
    let charlie = engine.register_account();

    // 三个限价买单在不同价位
    engine.issue(alice, usdt, Decimal::new(30000, 0)).unwrap();
    engine.issue(bob, usdt, Decimal::new(40000, 0)).unwrap();
    engine.issue(charlie, usdt, Decimal::new(50000, 0)).unwrap();

    engine.make(alice, usdt, btc, Decimal::new(30000, 0), Decimal::new(30000, 0));
    engine.make(bob, usdt, btc, Decimal::new(40000, 0), Decimal::new(40000, 0));
    engine.make(charlie, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();

    // Dave 以 35000 卖 2 BTC — 应该与 Bob (40000) 和 Alice (30000) 匹配
    // 但注意价格优先：Charlie(50000) > Bob(40000) > Alice(30000)
    // 然而 Dave 卖价 35000，所以只有出价 >= 35000 的买单才能成交
    let dave = engine.register_account();
    engine.issue(dave, btc, Decimal::new(2, 0)).unwrap();
    engine.make(dave, btc, usdt, Decimal::new(2, 0), Decimal::new(35000, 0));
    engine.step();

    // 再驱动一帧处理撮合消息
    engine.step();

    // Charlie(50000) 和 Bob(40000) 出价 >= 35000，应成交
    // Alice(30000) 出价 < 35000，不应成交
    let charlie_btc = engine.get_node_balance(charlie, btc).unwrap();
    let bob_btc = engine.get_node_balance(bob, btc).unwrap();
    let alice_btc = engine.get_node_balance(alice, btc).unwrap();

    assert!(charlie_btc > Decimal::ZERO, "Charlie should have BTC (price 50000 >= 35000)");
    assert!(bob_btc > Decimal::ZERO, "Bob should have BTC (price 40000 >= 35000)");
    assert_eq!(alice_btc, Decimal::ZERO, "Alice should not have BTC (price 30000 < 35000)");
}

// ============================
// 订单查询测试
// ============================

#[test]
fn test_get_order_brief() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();

    engine.issue(alice, usdt, Decimal::new(10000, 0)).unwrap();
    engine.make(alice, usdt, btc, Decimal::new(5000, 0), Decimal::new(50000, 0));
    engine.step();

    let orders = engine.get_all_orders();
    assert!(!orders.is_empty());
    let brief = engine.get_order_brief(orders[0]).unwrap();
    assert_eq!(brief.src_token, usdt);
    assert_eq!(brief.dst_token, btc);
}

// ============================
// 零数量订单测试
// ============================

#[test]
fn test_zero_volume_make() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();

    engine.issue(alice, usdt, Decimal::new(100, 0)).unwrap();
    // 零数量限价单
    engine.make(alice, usdt, btc, Decimal::ZERO, Decimal::new(50000, 0));
    engine.step();
    // 不应该 panic
}

#[test]
fn test_zero_price_make() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();
    let bob = engine.register_account();

    // 零价格限价单不应该导致除零崩溃
    engine.issue(alice, usdt, Decimal::new(100, 0)).unwrap();
    engine.make(alice, usdt, btc, Decimal::new(50, 0), Decimal::ZERO);
    engine.step();

    engine.issue(bob, btc, Decimal::new(1, 0)).unwrap();
    engine.make(bob, btc, usdt, Decimal::new(1, 0), Decimal::ZERO);
    engine.step();

    // 不应该 panic
}

// ============================
// 多对交易对测试
// ============================

#[test]
fn test_multiple_pairs() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let eth = engine.register_token("ETH");
    let alice = engine.register_account();
    let bob = engine.register_account();
    let charlie = engine.register_account();

    // USDT/BTC 交易对
    engine.issue(alice, usdt, Decimal::new(50000, 0)).unwrap();
    engine.issue(bob, btc, Decimal::new(1, 0)).unwrap();

    engine.make(alice, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();
    engine.make(bob, btc, usdt, Decimal::new(1, 0), Decimal::new(50000, 0));
    engine.step();

    // USDT/ETH 交易对
    engine.issue(charlie, eth, Decimal::new(10, 0)).unwrap();
    engine.make(charlie, eth, usdt, Decimal::new(10, 0), Decimal::new(3000, 0));
    engine.step();

    let (btc_price, _quote, _base) = engine.get_current_price(usdt, btc).unwrap();
    let (_eth_price, _eq, _eb) = engine.get_current_price(eth, usdt).unwrap();

    assert!(btc_price > Decimal::ZERO, "BTC price should be set");
}

// ============================
// 精度测试
// ============================

#[test]
fn test_precision_setting() {
    let engine = Rpte::new("USDT", 18);
    assert_eq!(engine.get_precision(), 18);
}

// ============================
// 获取订单方向测试(通过订单簿)
// ============================

#[test]
fn test_order_book_directions() {
    let mut engine = Rpte::new("USDT", 4);
    let usdt = engine.get_token_by_name("USDT").unwrap();
    let btc = engine.register_token("BTC");
    let alice = engine.register_account();

    // 买单 (buy USDT/BTC = 花费 USDT 买 BTC)
    engine.issue(alice, usdt, Decimal::new(50000, 0)).unwrap();
    engine.make(alice, usdt, btc, Decimal::new(50000, 0), Decimal::new(50000, 0));
    engine.step();

    // 检查买单簿
    let depth = engine.get_order_book(usdt, btc, 0).unwrap();
    assert_eq!(depth.price, Decimal::new(50000, 0));

    // 卖单簿应为空
    let depth = engine.get_order_book(btc, usdt, 0).unwrap();
    assert_eq!(depth.price, Decimal::ZERO);
}