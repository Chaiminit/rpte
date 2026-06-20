//! # RPTE — Rust Perpetual Trading Engine
//!
//! A node-based perpetual contract trading simulation engine supporting multi-token,
//! multi-account, limit/market order matching, order book management, and candle data aggregation.
//!
//! ## Core Concepts
//!
//! - **Node**: The fundamental unit of the engine. Token, Account, Order, and Pair all implement the `Node` trait.
//! - **Msg**: The communication carrier between nodes. The engine collects and processes all messages per frame.
//! - **Pair**: Manages the order book, matching logic, and trade logs.
//! - **Order**: Supports limit orders (Make) and market orders (Swap).
//!
//! ## Quick Start
//!
//! ```rust
//! use rust_decimal::Decimal;
//! use rpte::Rpte;
//!
//! // Create an engine
//! let mut engine = Rpte::new("USDT", 4);
//!
//! // Register tokens
//! let usdt = engine.get_token_by_name("USDT").unwrap();
//! let btc = engine.register_token("BTC");
//!
//! // Register an account
//! let alice = engine.register_account();
//!
//! // Issue assets
//! engine.issue(alice, usdt, 10000u64).unwrap();
//! engine.issue(alice, btc, 10u64).unwrap();
//!
//! // Create a limit buy order
//! engine.make(alice, usdt, btc, 5000u64, 50000u64);
//! engine.step(); // Drive one frame
//!
//! // Query balance
//! let balance = engine.get_node_balance(alice, usdt).unwrap();
//!
//! // Query price with orientation info: (price, quote_token, base_token)
//! let (price, quote, base) = engine.get_current_price(usdt, btc).unwrap();
//!
//! // Query order book depth — direction auto-derived from src/dst tokens
//! let depth = engine.get_order_book(usdt, btc, 0).unwrap();
//! println!("Best price: {}, volume: {}", depth.price, depth.volume);
//! ```
//!
//! ## Modules
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`rpte`] | Engine core, exposes all operational interfaces |
//! | [`node`] | Core trait definitions: `Node`, `PairNode`, `OrderNode`, `AccountNode`, etc. |
//! | [`token`] | Token node implementation |
//! | [`account`] | Account node implementation |
//! | [`order`] | Order node and order brief implementation |
//! | [`pair`] | Trading pair node implementation, including candle aggregation |
//! | [`order_book`] | Order book data structure (BTreeMap-based) |
//! | [`error`] | Error type definitions |

pub mod error;
pub mod rpte;
pub mod order;
pub mod token;
pub mod pair;
pub mod virtual_pair;
pub mod node;
pub mod account;
pub mod contract;
pub mod order_book;
pub mod contract_presets;
pub mod tui;
pub mod fee;
pub mod fee_presets;

pub use rpte::Rpte;
pub use fee::FeeCtx;
pub use fee_presets::empty_fee;
pub use fee_presets::taker_maker_fee;
pub use node::{Node, Msg, Drt, OrderBookDepth, PairNode, OrderNode, AccountNode, TokenNode, ContractNode, ContractState, ContractFn, EngineReader, SwapCheckFn};
pub use order::{Order, OrderBrief, OrderType};
pub use token::Token;
pub use account::Account;
pub use pair::{Pair, TraLog, CandleData};
pub use virtual_pair::VirtualPair;
pub use contract::Contract;
pub use order_book::OrderBook;
pub use contract_presets::LendingPreset;
pub use error::{Error, Result};