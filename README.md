# RPTE — Rust Perpetual Trading Engine

RPTE is a **node-based** perpetual contract trading simulation engine written in Rust.

## Features

- **Multi-token support** — Register and manage arbitrary tokens
- **Multi-account management** — Multiple independent accounts, each holding its own assets
- **Limit orders (Make)** — Price-time priority order book matching
- **Market orders (Swap)** — Instant execution at the best available order book price
- **Order book management** — Built on `BTreeMap`, O(log n) insert/delete/lookup
- **Candle aggregation** — Candle data generation for arbitrary time intervals
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
│  │           Pair (Trading Pair)           │         │
│  │  ┌───────────────┐  ┌───────────────┐  │         │
│  │  │   OrderBook   │  │   TradeLogs   │  │         │
│  └────────────────────────────────────────┘         │
└─────────────────────────────────────────────────────┘
```

All nodes are stored in a `Vec<Box<dyn Node>>` and communicate via **messages (Msg)**.

## Quick Start

### Create an Engine

```rust
use rust_decimal::Decimal;
use rpte::Rpte;

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
engine.make(alice, usdt, btc, Decimal::new(5000, 0), Decimal::new(50000, 0));
engine.step();

// Market sell order: Sell 0.1 BTC at market price
engine.swap(alice, btc, usdt, Decimal::new(1, 1));
engine.step();
```

### Queries

```rust
// Query balance
let balance = engine.get_node_balance(alice, usdt).unwrap();
println!("Alice USDT balance: {balance}");

// Query price with orientation info
let (price, quote, base) = engine.get_current_price(usdt, btc).unwrap();
println!("1 {base} = {price} {quote}");

// Query order book depth — direction auto-derived from src/dst tokens
let depth = engine.get_order_book(usdt, btc, 0).unwrap();
println!("Best price: {}, volume: {}", depth.price, depth.volume);

// Get latest candle
let candle = engine.latest_candle(usdt, btc, 10).unwrap();
println!("Latest candle: {candle:?}");
```

## Running Modes

RPTE supports two running modes:

| Mode | Method | Description |
|------|--------|-------------|
| Single-step | `step()` | Manually drive one frame, suitable for testing and precise control |
| Fixed frame rate | `run(fps)` | Continuously run at a specified frame rate until `stop()` is called |

## API Overview

### Engine Operations (`Rpte`)

| Method | Description |
|--------|-------------|
| `new(quote, precision)` | Create a new engine with the specified quote token and precision |
| `register_token(name)` | Register a token, returns token ID |
| `register_account()` | Register an account, returns account ID |
| `issue(node, token, volume)` | Issue assets to a specified node |
| `make(src, src_token, dst_token, volume, price)` | Create a limit order |
| `swap(src, src_token, dst_token, volume)` | Create a market order |
| `cancel(order_id)` | Cancel an order |
| `transfer(src, dst, token, volume)` | Transfer assets (strict: fails if balance insufficient) |
| `transfer_with_overdraft(src, dst, token, volume)` | Transfer assets (allows negative balance) |
| `step()` | Drive one frame |
| `run(fps)` | Run at a fixed frame rate (`fps` must be > 0) |
| `stop()` | Stop running |

### Query Methods

| Method | Return Type | Description |
|--------|-------------|-------------|
| `get_node_balance(node, token)` | `Result<Decimal>` | Query node balance |
| `get_current_price(src, dst)` | `Result<(Decimal, usize, usize)>` | Get current price as `(price, quote_token, base_token)`, meaning 1 base_token = price quote_token |
| `get_order_book(src, dst, depth)` | `Result<OrderBookDepth>` | Get order book depth at the specified level. Direction (Buy/Sell) is auto-derived from src/dst tokens |
| `get_all_orders()` | `Vec<usize>` | List all open order IDs |
| `get_all_tokens()` | `Vec<usize>` | List all registered token IDs |
| `get_all_accounts()` | `Vec<usize>` | List all registered account IDs |
| `get_all_pairs()` | `Vec<usize>` | List all registered pair IDs |
| `get_tra_logs(src, dst)` | `Result<VecDeque<TraLog>>` | Get trade logs for a trading pair |
| `get_candle_data(src, dst, interval)` | `Result<VecDeque<CandleData>>` | Get candle (OHLCV) data for a trading pair |
| `latest_candle(src, dst, interval)` | `Result<Option<CandleData>>` | Get the latest candle for a trading pair |

### Key Types

| Type | Fields | Description |
|------|--------|-------------|
| `OrderBookDepth` | `price: Decimal`, `volume: Decimal` | A single level in the order book |
| `TraLog` | `step_count`, `price`, `volume`, ... | A single trade log entry |
| `CandleData` | `step_count`, `open`, `high`, `low`, `close`, `volume` | OHLCV candle data |
| `OrderBrief` | `id`, `direction`, `src_token`, `dst_token`, `src_volume`, `dst_volume`, `price`, `step_count_created` | A summary of an active order |

### Error Handling

All fallible operations return `Result<T>`, with the error type `Error` including:

| Error Variant | Description |
|---------------|-------------|
| `NodeNotFound` | Node ID out of range |
| `TokenNotRegistered` | Token not registered |
| `InsufficientBalance` | Insufficient balance (raised by strict `transfer` and internal checks) |
| `OrderNotRegistered` | Order not registered |
| `NotAPairNode` / `NotAnOrderNode` / `NotAnAccountNode` | Node type mismatch |

## License

This project is licensed under the MIT License.