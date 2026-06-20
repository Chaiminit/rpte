use rust_decimal::Decimal;
use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub enum Error {
    /// 节点 ID 超出范围
    NodeNotFound { id: usize, len: usize },
    /// 节点不是 TokenNode
    NotATokenNode(usize),
    /// 节点不是 PairNode
    NotAPairNode(usize),
    /// 节点不是 OrderNode
    NotAnOrderNode(usize),
    /// 节点不是 AccountNode
    NotAnAccountNode(usize),
    /// 节点不是 ContractNode
    NotAContractNode(usize),
    /// Token 未注册
    TokenNotRegistered(usize),
    /// 订单未注册
    OrderNotRegistered(usize),
    /// 合约未注册
    ContractNotRegistered(usize),
    /// 余额不足
    InsufficientBalance {
        node_id: usize,
        token: usize,
        has: Decimal,
        need: Decimal,
    },
    /// 目标账户余额将为负
    NegativeDestination {
        node_id: usize,
        token: usize,
        current: Decimal,
        volume: Decimal,
    },
    /// 订单初始化失败
    OrderOpenFailed(usize),
    /// 索引越界
    IndexOutOfBounds { id: usize, len: usize },
    /// 代币交换不被允许（白名单限制）
    SwapNotAllowed { src: usize, dst: usize },
    /// 无可用兑换路径
    NoRouteFound { src: usize, dst: usize },
    /// 交易对未找到
    PairNotFound(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NodeNotFound { id, len } => {
                write!(f, "node {id} not found (len={len})")
            }
            Error::NotATokenNode(id) => {
                write!(f, "node {id} is not a TokenNode")
            }
            Error::NotAPairNode(id) => {
                write!(f, "node {id} is not a PairNode")
            }
            Error::NotAnOrderNode(id) => {
                write!(f, "node {id} is not an OrderNode")
            }
            Error::NotAnAccountNode(id) => {
                write!(f, "node {id} is not an AccountNode")
            }
            Error::NotAContractNode(id) => {
                write!(f, "node {id} is not a ContractNode")
            }
            Error::TokenNotRegistered(id) => {
                write!(f, "token {id} is not registered")
            }
            Error::OrderNotRegistered(id) => {
                write!(f, "order {id} is not registered")
            }
            Error::ContractNotRegistered(id) => {
                write!(f, "contract {id} is not registered")
            }
            Error::InsufficientBalance {
                node_id,
                token,
                has,
                need,
            } => {
                write!(
                    f,
                    "insufficient balance: node {node_id} token {token} has {has} need {need}"
                )
            }
            Error::NegativeDestination {
                node_id,
                token,
                current,
                volume,
            } => {
                write!(
                    f,
                    "negative destination: node {node_id} token {token} current {current} volume {volume}"
                )
            }
            Error::OrderOpenFailed(id) => {
                write!(f, "order {id} failed to open")
            }
            Error::IndexOutOfBounds { id, len } => {
                write!(f, "index {id} out of bounds (len={len})")
            }
            Error::SwapNotAllowed { src, dst } => {
                write!(
                    f,
                    "swap not allowed: token {src} ↔ token {dst} (whitelist restriction)"
                )
            }
            Error::NoRouteFound { src, dst } => {
                write!(f, "no route found: token {src} → token {dst}")
            }
            Error::PairNotFound(id) => {
                write!(f, "pair {id} not found")
            }
        }
    }
}

impl std::error::Error for Error {}
