# RPTE — Rust Perpetual Trading Engine

RPTE is a **node-based** perpetual contract trading simulation engine written in Rust.

## Features

- **Multi-token support** — Register and manage arbitrary tokens
- **Multi-account management** — Multiple independent accounts, each holding its own assets
- **Limit orders (Make)** — Price-time priority order book matching
- **Market orders (Swap)** — Instant execution at the best available order book price
- **Batch market orders** — Proportional allocation across multiple swappers in one frame
- **Order book management** — Built on `BTreeMap`, O(log n) insert/delete/lookup
- **Candle aggregation** — Candle data generation for arbitrary time intervals
- **Route routing** — `Route::auto` / `Route::on` for selecting specific trading pairs; `route_discover` for multi-hop path finding
- **Fee system** — Configurable taker/maker fee closures attached to any trading pair
- **Contract system** — Deploy custom on-chain logic (on_create / on_update / on_end / on_called)
- **Virtual trading pairs** — Contract-backed pairs with dynamic pricing
- **Built-in DeFi presets** — Lending market with cross-collateralization, liquidation, and dynamic interest rates
- **Message-driven** — Nodes communicate via messages, engine runs on frame-driven ticks
- **Frame sync control** — Supports fixed frame rate or single-step execution

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                      Rpte                           │
│      (Engine Core: Registry + Message Router + Transfer)
│                                                     │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐          │
│  │  Token   │  │ Account  │  │  Order   │          │
│  └──────────┘  └──────────┘  └──────────┘          │
│  ┌────────────────────────────────────────┐         │
│  │        Pair / VirtualPair              │         │
│  │  ┌───────────────┐  ┌───────────────┐  │         │
│  │  │   OrderBook   │  │   TradeLogs   │  │         │
│  └────────────────────────────────────────┘         │
│  ┌────────────────────────────────────────┐         │
│  │           Contract (Master/Slave)      │         │
│  └────────────────────────────────────────┘         │
└─────────────────────────────────────────────────────┘
```

All nodes are stored in a `Vec<Box<dyn Node>>` and communicate via **messages (Msg)**.

## Quick Start

### Create an Engine

```rust
use rust_decimal::Decimal;
use rpte::{Rpte, Route};

let mut engine = Rpte::new("USDT", 4);
```

### Register Tokens and Accounts

```rust
let usdt = engine.get_token_by_name("USDT").unwrap();
let btc = engine.register_token("BTC");
let alice = engine.register_account();
```

### Issue Assets

```rust
engine.issue(alice, usdt, Decimal::new(10000, 0)).unwrap();
engine.issue(alice, btc, Decimal::new(10, 0)).unwrap();
```

### Place Orders and Match

```rust
// Limit buy order: Buy BTC at 50000 USDT/BTC
engine.make(alice, Decimal::new(5000, 0), Decimal::new(50000, 0), Route::auto(usdt, btc));
engine.step();

// Market sell order: Sell 0.1 BTC at market price
engine.swap(alice, Decimal::new(1, 1), Route::auto(btc, usdt));
engine.step();
```

### Queries

```rust
// Query balance
let balance = engine.get_node_balance(alice, usdt).unwrap();
println!("Alice USDT balance: {balance}");

// Query price — returns Vec of matching pairs
let prices = engine.get_current_price(Route::auto(usdt, btc)).unwrap();
let (price, quote, base) = prices[0];
println!("1 {base} = {price} {quote}");

// Query order book depth
let depths = engine.get_order_book(Route::auto(usdt, btc), 0).unwrap();
println!("Best price: {}, volume: {}", depths[0].price, depths[0].volume);

// Get latest candle
let candles = engine.latest_candle(Route::auto(usdt, btc), 10).unwrap();
if let Some(candle) = candles[0] {
    println!("Latest candle: {candle:?}");
}
```

### Discover Routes

```rust
// Find multi-hop path: src_token → dst_token
let hops = engine.route_discover(usdt, btc);
for hop in &hops {
    let src_name = engine.get_token_name(hop.src_token).unwrap_or("?");
    let dst_name = engine.get_token_name(hop.dst_token).unwrap_or("?");
    println!("pair {}: {} → {}", hop.pair_id, src_name, dst_name);
}
```

### Set Trading Fees

```rust
use rpte::{taker_maker_fee, FeeCtx};
use std::sync::Arc;

// Use the built-in preset
let fee = taker_maker_fee(
    Decimal::new(3, 4),    // 0.03% taker
    Decimal::new(1, 4),    // 0.01% maker
    fee_collector,
);
engine.set_pair_fee(pair_id, Some(fee)).unwrap();
```

### Deploy a Lending Contract

```rust
use rpte::LendingPreset;

let lending = LendingPreset::new_bidirectional(
    usdt, btc,
    "aUSDT", "aBTC",
    "dUSDT", "dBTC",
    Decimal::new(130, 2),  // min_collateral_ratio = 1.30
    Decimal::new(110, 2),  // liquidation_threshold = 1.10
);
let (on_create, on_update, on_end, on_called_fns) = lending.build();
engine.deploy(player, "USDT/BTC Lending", on_create, on_update, on_end, on_called_fns);
engine.step();
engine.step();
```

## Running Modes

RPTE supports two running modes:

| Mode | Method | Description |
|------|--------|-------------|
| Single-step | `step()` | Manually drive one frame, suitable for testing and precise control |
| Fixed frame rate | `run(fps)` | Continuously run at a specified frame rate until `stop()` is called |

## Modules

| Module | Description |
|--------|-------------|
| `rpte` | Engine core, exposes all operational interfaces |
| `node` | Core trait definitions: `Node`, `PairNode`, `OrderNode`, `AccountNode`, etc. |
| `token` | Token node implementation |
| `account` | Account node implementation |
| `order` | Order node implementation |
| `pair` | Trading pair node, order book, candle aggregation |
| `virtual_pair` | Contract-backed virtual trading pair |
| `contract` | State-machine contract with lifecycle hooks |
| `route` | Route routing (`Route`, `RouteHop`) |
| `fee` | Fee system (`FeeFn`, `FeeCtx`) |
| `fee_presets` | Pre-built fee strategies (`taker_maker_fee`) |
| `contract_presets` | Pre-built contract templates (`LendingPreset`) |
| `order_book` | BTreeMap-based order book |
| `tui` | Terminal UI (ratatui-based) |

## Key Types

| Type | Description |
|------|-------------|
| `Route` | Trading route: src_token, dst_token, optional pair_id |
| `RouteHop` | A single hop in a discovered route path |
| `FeeCtx` | Fee invocation context (tokens, volumes, taker/maker) |
| `FeeFn` | Configurable fee closure |
| `OrderBookDepth` | A single level in the order book |
| `TraLog` | A single trade log entry |
| `CandleData` | OHLCV candle data |
| `OrderBrief` | Summary of an active order |

## Error Handling

All fallible operations return `Result<T>`, with the error type `Error` including:

| Error Variant | Description |
|---------------|-------------|
| `NodeNotFound` | Node ID out of range |
| `TokenNotRegistered` | Token not registered |
| `InsufficientBalance` | Insufficient balance |
| `OrderNotRegistered` | Order not registered |
| `PairNotFound` | Pair ID not found |
| `NoRouteFound` | No conversion route between two tokens |
| `SwapNotAllowed` | Token swap blacklisted by whitelist closure |
| `NotAPairNode` / `NotAnOrderNode` / etc. | Node type mismatch |

## Examples

```bash
cargo run --example simswap      # Lending market simulation with random bots
cargo run --example stress_test  # Stress test with many orders
```

## License

This project is licensed under the MIT License.
