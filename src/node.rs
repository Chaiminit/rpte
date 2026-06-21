use std::collections::{HashMap, HashSet};
use std::collections::VecDeque;
use std::fmt::Debug;
use std::sync::Arc;
use rust_decimal::Decimal;
use crate::order::{OrderBrief, OrderType};
use crate::pair::{TraLog, CandleData};
use crate::fee::FeeFn;
use crate::route::Route;

/// Token 交换检查闭包：(EngineReader, self_token, other_token, src_node, dst_node) → 是否允许
pub type SwapCheckFn = Arc<dyn Fn(&dyn EngineReader, usize, usize, usize, usize) -> bool + Send + Sync>;

pub trait Node { 
    fn update(&mut self, _step_count: u64) {}
    fn upload_msgs(&mut self, step_count: u64) -> Vec<Msg> {
        self.update(step_count);
        std::mem::take(self.get_msgs())
    }
    fn get_msgs(&mut self) -> &mut Vec<Msg>;
    fn set_id(&mut self, id: usize);
    fn get_id(&self) -> usize;

    /// 读取余额
    fn balance(&self, _token: usize) -> Decimal {
        Decimal::ZERO
    }
    /// 设置余额
    fn set_balance(&mut self, _token: usize, _volume: Decimal) {}
    /// 增减余额，默认实现基于 balance + set_balance
    fn adjust_balance(&mut self, token: usize, delta: Decimal) {
        let current = self.balance(token);
        self.set_balance(token, current + delta);
    }
    /// 取出全部余额
    fn drain_balances(&mut self) -> HashMap<usize, Decimal> {
        HashMap::new()
    }

    /// 返回订单是否处于开启状态（默认 false，Order 重写）
    fn is_open(&self) -> bool {
        false
    }

    fn send_msg(&mut self, msg: Msg) {
        self.get_msgs().push(msg);
    }

    fn as_order_node(&mut self) -> Option<&mut dyn OrderNode> { None }
    fn as_pair_node(&mut self) -> Option<&mut dyn PairNode> { None }
    fn as_account_node(&mut self) -> Option<&mut dyn AccountNode> { None }
    fn as_token_node(&mut self) -> Option<&mut dyn TokenNode> { None }
    fn as_contract_node(&mut self) -> Option<&mut dyn ContractNode> { None }

    /// &self 版本转换方法（用于 EngineReader 只读访问）
    fn as_pair_node_ref(&self) -> Option<&dyn PairNode> { None }
    fn as_token_node_ref(&self) -> Option<&dyn TokenNode> { None }
    fn as_account_node_ref(&self) -> Option<&dyn AccountNode> { None }
    fn as_order_node_ref(&self) -> Option<&dyn OrderNode> { None }
}


pub trait TokenNode: Node {
    /// 查询代币发行总量
    fn total_supply(&self) -> Decimal;
    /// 设置发行总量
    fn set_total_supply(&mut self, supply: Decimal);
    /// 增减发行量（默认实现基于 total_supply + set_total_supply）
    fn adjust_total_supply(&mut self, delta: Decimal) {
        let current = self.total_supply();
        self.set_total_supply(current + delta);
    }
    /// 查询是否允许负持仓
    fn can_be_negative(&self) -> bool;
    /// 设置是否允许负持仓
    fn set_can_be_negative(&mut self, can: bool);
    /// 检查此代币是否可与 another 交易（闭包逻辑）。
    /// 闭包签名：(EngineReader, self_token, other_token, src_node, dst_node) → bool
    /// None = 无限制（默认允许）
    fn can_swap_with(&self, reader: &dyn EngineReader, self_token: usize, other_token: usize, src_node: usize, dst_node: usize) -> bool;
    /// 设置交换检查闭包
    fn set_swap_check_fn(&mut self, f: Option<SwapCheckFn>);
}

pub trait OrderNode: Node {
    fn get_owner_node_id(&self) -> usize;
    fn get_pair_node_id(&self) -> usize;
    fn get_src_token(&self) -> usize;
    fn get_dst_token(&self) -> usize;
    fn get_price(&self) -> &Decimal;
    fn get_step_count_created(&self) -> u64;
    fn get_order_type(&self) -> &OrderType;
    fn get_route(&self) -> Option<&Route> { None }
    fn get_current_hop(&self) -> usize { 0 }
    fn open(
        &mut self,
        owner_node_id: usize,
        pair_node_id: usize,
        src_token: usize,
        dst_token: usize,
        price: Decimal,
        step_count_created: u64,
        order_type: OrderType,
    ) -> bool;
    fn open_with_route(
        &mut self,
        owner_node_id: usize,
        pair_node_id: usize,
        src_token: usize,
        dst_token: usize,
        price: Decimal,
        step_count_created: u64,
        order_type: OrderType,
        route: Option<Route>,
        current_hop: usize,
    ) -> bool {
        self.open(owner_node_id, pair_node_id, src_token, dst_token, price, step_count_created, order_type)
    }
    fn close(&mut self);
    fn is_open(&self) -> bool;
}


pub trait PairNode: Node {
    fn get_quote_token(&self) -> usize;
    fn get_base_token(&self) -> usize;
    fn get_current_price(&self) -> Decimal;
    /// 设置当前价格（虚拟交易对用于缓存合约报价）
    fn set_current_price(&mut self, _price: Decimal) {}
    fn get_order_book(&self, direction: Drt, depth: usize) -> OrderBookDepth;
    fn get_tra_logs(&self) -> VecDeque<TraLog>;
    fn get_candle_data(&self, interval: u64) -> VecDeque<CandleData>;
    fn latest_candle(&self, interval: u64) -> Option<CandleData>;
    fn push_tra_log(&mut self, step_count: u64, src_id: usize, dst_id: usize, price: Decimal, volume: Decimal);
    fn update_brief(&mut self, brief: OrderBrief);
    /// 插入订单并触发撮合
    fn insert_brief(&mut self, brief: OrderBrief, reader: &dyn EngineReader);
    fn cancel_brief(&mut self, id: usize);
    /// 限价单撮合
    fn match_orders(&mut self, reader: &dyn EngineReader);
    /// 返回 (转账指令列表, 需关闭的订单ID列表)，引擎在同一帧执行。
    fn process_swap(&mut self, owner_id: usize, direction: Drt, volume: Decimal, reader: &dyn EngineReader) -> (Vec<SwapTransfer>, Vec<usize>);
    /// 批量市价单按比例分配撮合。
    /// swaps 为 [(owner_id, volume_in_source_token), ...]，同方向多个 swap 共享流动性。
    /// 返回 (转账指令列表, 需关闭的订单ID列表)。
    fn process_swaps_batch(&mut self, direction: Drt, swaps: &[(usize, Decimal)], reader: &dyn EngineReader) -> (Vec<SwapTransfer>, Vec<usize>);
    /// 设置手续费闭包
    fn set_fee_fn(&mut self, _fee_fn: Option<FeeFn>) {}
}


pub trait AccountNode: Node {
    fn insert_order(&mut self, order_id: usize);
    fn remove_order(&mut self, order_id: usize);
    fn get_orders(&self) -> &HashSet<usize>;
}

/// 合约状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractState {
    /// 创建态：等待首次 update 触发 on_create
    Creating,
    /// 运行态：每帧调用 on_update
    Running,
    /// 结束态：等待下次 update 触发 on_end
    Ending,
    /// 销毁态：引擎回收
    Destroyed,
}

/// 合约行为函数类型（Arc 包装，可 Clone，可放入 Msg）
pub type ContractFn = Arc<dyn Fn(&mut crate::contract::Contract, &dyn EngineReader, u64) -> Vec<Msg> + Send + Sync>;
/// 合约调用函数类型：传入合约、引擎只读视图、调用者 ID、调用量，返回消息列表
pub type CalledFn = Arc<dyn Fn(&mut crate::contract::Contract, &dyn EngineReader, usize, Decimal) -> Vec<Msg> + Send + Sync>;

pub trait ContractNode: Node {
    fn get_state(&self) -> ContractState;
    fn set_state(&mut self, state: ContractState);
    fn get_owner_node_id(&self) -> usize;
    fn get_step_count_created(&self) -> u64;
    fn get_name(&self) -> &str;
    fn set_name(&mut self, name: &str);
    /// 用行为函数部署合约（从池中取出或新建时调用）
    fn deploy(&mut self, owner_node_id: usize, name: &str, on_create: ContractFn, on_update: ContractFn, on_end: ContractFn, on_called_fns: Vec<CalledFn>, step_count: u64);
    /// 标记为结束态（由 on_update 内部调用）
    fn end(&mut self);
    /// 获取合约的所有余额（用于主从同步）
    fn get_all_balances(&self) -> &HashMap<usize, Decimal>;
}

/// 引擎只读视图：合约通过此 trait 读取引擎状态。
///
/// 所有方法均为 &self，可在引擎持有 &mut self.nodes 时安全调用。
pub trait EngineReader {
    fn precision(&self) -> u8;
    fn global_quote_token(&self) -> usize;
    fn get_token_name(&self, id: usize) -> Option<&str>;
    fn get_token_by_name(&self, name: &str) -> Option<usize>;
    fn get_all_tokens(&self) -> Vec<usize>;
    fn get_all_accounts(&self) -> Vec<usize>;

    /// 查询节点余额
    fn node_balance(&self, node_id: usize, token: usize) -> Decimal;

    /// 获取交易对当前价格（仅查询已有交易对，不自动创建）
    fn get_current_price(&self, quote_token: usize, base_token: usize) -> Option<Decimal>;
    /// 获取任意两个代币之间的价格
    fn price_between(&self, src: usize, dst: usize) -> Option<(Decimal, usize, usize)>;
    /// 跨代币换算
    fn convert_value(&self, src: usize, dst: usize, amount: Decimal) -> Decimal;

    /// 获取账户权益量（余额 + 挂单）
    fn account_equity_token(&self, account_id: usize, token: usize) -> Decimal;

    /// 代币元数据
    fn token_total_supply(&self, token: usize) -> Decimal;
    fn token_can_be_negative(&self, token: usize) -> bool;

    /// 市场数据
    fn get_order_book(&self, quote_token: usize, base_token: usize, depth: usize) -> Vec<OrderBookDepth>;
    fn get_tra_logs(&self, quote_token: usize, base_token: usize) -> VecDeque<TraLog>;
    fn get_candle_data(&self, quote_token: usize, base_token: usize, interval: u64) -> VecDeque<CandleData>;
    fn latest_candle(&self, quote_token: usize, base_token: usize, interval: u64) -> Option<CandleData>;
}



#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub enum Drt {
    Buy,
    Sell,
}

/// 撮合生成的单笔转账指令（引擎直接执行，不入消息队列）
#[derive(Debug, Clone)]
pub struct SwapTransfer {
    pub src_id: usize,
    pub dst_id: usize,
    pub token: usize,
    pub volume: Decimal,
}

/// 订单簿深度快照
#[derive(Debug, Clone, Copy)]
pub struct OrderBookDepth {
    /// 价格
    pub price: Decimal,
    /// 数量
    pub volume: Decimal,
}


#[derive(Clone)]
pub enum Msg {
    Transfer {
        src_id: usize,
        dst_id: usize,
        token: usize,
        volume: Decimal,
    },
    TransferAll {
        src_id: usize,
        dst_id: usize,
    },
    OpenOrder {
        src_id: usize,
        owner_node_id: usize,
        src_token: usize,
        dst_token: usize,
        volume: Decimal,
        price: Decimal,
        pair_id: Option<usize>,
        route: Option<Route>,
        current_hop: usize,
    },
    SwapOrder {
        src_id: usize,
        owner_node_id: usize,
        src_token: usize,
        dst_token: usize,
        volume: Decimal,
        pair_id: Option<usize>,
        route: Option<Route>,
        current_hop: usize,
    },
    CloseOrder {
        order_id: usize,
    },
    CreateContract {
        owner_node_id: usize,
        name: String,
        on_create: ContractFn,
        on_update: ContractFn,
        on_end: ContractFn,
        on_called: Vec<CalledFn>,
    },
    RegisterToken {
        name: String,
        can_be_negative: bool,
    },
    CreateVirtualPair {
        /// 合约从实例 ID
        contract_slave_id: usize,
        /// 绑定的 CalledFn 索引
        fn_id: u8,
        quote_token: usize,
        /// base token 名称（引擎按名称解析 ID，支持跨帧创建）
        base_token_name: String,
        /// 只许市价单模式（固定汇率对如 dUSDT/USDT 适用）
        swap_only: bool,
    },
    /// 调用合约：src_id 向 contract_id 发起调用，携带 volume
    CallContract {
        src_id: usize,
        contract_id: usize,
        fn_id: u8,
        volume: Decimal,
    },
    /// 增发（正 volume）或销毁（负 volume）代币
    Issue {
        token: usize,
        account_id: usize,
        volume: Decimal,
    },
    /// 设置交易对的手续费
    SetPairFee {
        pair_id: usize,
        fee_fn: Option<FeeFn>,
    },
    /// 快速兑换：按发现的路由逐跳自动兑换
    FastSwap {
        src_id: usize,
        route: Route,
        volume: Decimal,
        /// 当前正在处理的跳数
        current_hop: usize,
    },
}