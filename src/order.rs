use crate::node::{Node, Msg, Drt, OrderNode};
use std::collections::HashMap;
use rust_decimal::Decimal;


#[derive(Default, PartialEq, Clone)]
pub enum OrderType {
    #[default]
    Make,
    Swap,
}


#[derive(Default)]
pub struct Order {
    id: usize,
    msgs: Vec<Msg>,
    sheet: HashMap<usize, Decimal>,
    owner_node_id: usize,
    pair_node_id: usize,
    src_token: usize,
    dst_token: usize,
    price: Decimal,
    step_count_created: u64,
    openning: bool,
    order_type: OrderType,
}


#[derive(Clone)]
pub struct OrderBrief {
    pub id: usize,
    pub direction: Drt,
    pub src_token: usize,
    pub dst_token: usize,
    pub src_volume: Decimal,
    pub dst_volume: Decimal,
    pub price: Decimal,
    pub step_count_created: u64,
}


impl Node for Order {
    fn as_order_node(&mut self) -> Option<&mut dyn OrderNode> { Some(self) }
    fn get_msgs(&mut self) -> &mut Vec<Msg> { &mut self.msgs }
    fn get_id(&self) -> usize { self.id }
    fn set_id(&mut self, id: usize) { self.id = id; }
    fn is_open(&self) -> bool { self.openning }

    fn balance(&self, token: usize) -> Decimal {
        self.sheet.get(&token).copied().unwrap_or(Decimal::ZERO)
    }
    fn set_balance(&mut self, token: usize, volume: Decimal) {
        self.sheet.insert(token, volume);
    }
    fn drain_balances(&mut self) -> HashMap<usize, Decimal> {
        std::mem::take(&mut self.sheet)
    }
}

impl OrderNode for Order {
    fn get_owner_node_id(&self) -> usize { self.owner_node_id }
    fn get_pair_node_id(&self) -> usize { self.pair_node_id }
    fn get_src_token(&self) -> usize { self.src_token }
    fn get_dst_token(&self) -> usize { self.dst_token }
    fn get_price(&self) -> &Decimal { &self.price }
    fn get_step_count_created(&self) -> u64 { self.step_count_created }
    fn get_order_type(&self) -> &OrderType { &self.order_type }

    fn open(&mut self, owner_node_id: usize, pair_node_id: usize, src_token: usize, dst_token: usize, price: Decimal, step_count_created: u64, order_type: OrderType) -> bool {
        if owner_node_id == self.id { return false; }
        self.owner_node_id = owner_node_id;
        self.pair_node_id = pair_node_id;
        self.src_token = src_token;
        self.dst_token = dst_token;
        self.price = price;
        self.step_count_created = step_count_created;
        self.openning = true;
        self.order_type = order_type;
        true
    }

    fn close(&mut self) {
        self.openning = false;
    }

    fn is_open(&self) -> bool {
        self.openning
    }
}
