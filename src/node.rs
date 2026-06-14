use std::collections::{HashMap, HashSet};
use std::collections::VecDeque;
use rust_decimal::Decimal;
use crate::order::{OrderBrief, OrderType};
use crate::pair::{TraLog, CandleData};


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
}


pub trait TokenNode: Node {}

pub trait OrderNode: Node {
    fn get_owner_node_id(&self) -> usize;
    fn get_pair_node_id(&self) -> usize;
    fn get_src_token(&self) -> usize;
    fn get_dst_token(&self) -> usize;
    fn get_price(&self) -> &Decimal;
    fn get_step_count_created(&self) -> u64;
    fn get_order_type(&self) -> &OrderType;
    fn open(&mut self, owner_node_id: usize, pair_node_id: usize, src_token: usize, dst_token: usize, price: Decimal, step_count_created: u64, order_type: OrderType) -> bool;
    fn close(&mut self);
    fn is_open(&self) -> bool;
}


pub trait PairNode: Node {
    fn get_quote_token(&self) -> usize;
    fn get_base_token(&self) -> usize;
    fn get_current_price(&self) -> Decimal;
    fn get_order_book(&self, direction: Drt, depth: usize) -> OrderBookDepth;
    fn get_tra_logs(&self) -> VecDeque<TraLog>;
    fn get_candle_data(&self, interval: u64) -> VecDeque<CandleData>;
    fn latest_candle(&self, interval: u64) -> Option<CandleData>;
    fn push_tra_log(&mut self, step_count: u64, src_id: usize, dst_id: usize, price: Decimal, volume: Decimal);
    fn update_brief(&mut self, brief: OrderBrief);
    fn insert_brief(&mut self, brief: OrderBrief);
    fn cancel_brief(&mut self, id: usize);
    fn match_orders(&mut self);
    /// 市价单直接撮合，不创建临时订单节点。
    /// 返回 (转账指令列表, 需关闭的订单ID列表)，引擎在同一帧执行。
    fn process_swap(&mut self, owner_id: usize, direction: Drt, volume: Decimal) -> (Vec<SwapTransfer>, Vec<usize>);
    /// 批量市价单按比例分配撮合。
    /// swaps 为 [(owner_id, volume_in_source_token), ...]，同方向多个 swap 共享流动性。
    /// 返回 (转账指令列表, 需关闭的订单ID列表)。
    fn process_swaps_batch(&mut self, direction: Drt, swaps: &[(usize, Decimal)]) -> (Vec<SwapTransfer>, Vec<usize>);
}


pub trait AccountNode: Node {
    fn insert_order(&mut self, order_id: usize);
    fn remove_order(&mut self, order_id: usize);
    fn get_orders(&self) -> &HashSet<usize>;
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
        allow_negative: bool,
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
    },
    SwapOrder {
        src_id: usize,
        owner_node_id: usize,
        src_token: usize,
        dst_token: usize,
        volume: Decimal,
    },
    CloseOrder {
        order_id: usize,
    },
}
