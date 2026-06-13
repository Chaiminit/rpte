use std::collections::{HashMap, HashSet};
use rust_decimal::Decimal;
use crate::node::{Node, Msg, AccountNode};
use crate::order::OrderBrief;


pub struct Account {
    id: usize,
    msgs: Vec<Msg>,
    sheet: HashMap<usize, Decimal>,
    order_ids: HashSet<usize>,
}


impl Default for Account {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for Account {
    fn as_account_node(&mut self) -> Option<&mut dyn AccountNode> { Some(self) }
    fn get_msgs(&mut self) -> &mut Vec<Msg> { &mut self.msgs }
    fn get_id(&self) -> usize { self.id }
    fn set_id(&mut self, id: usize) { self.id = id; }

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


impl AccountNode for Account {
    fn insert_order(&mut self, order_id: usize) {
        self.order_ids.insert(order_id);
    }
    fn remove_order(&mut self, order_id: usize) {
        self.order_ids.remove(&order_id);
    }
    fn get_orders(&self) -> &HashSet<usize> {
        &self.order_ids
    }
}


impl Account {
    pub fn new() -> Self {
        Self {
            id: 0,
            msgs: Vec::new(),
            sheet: HashMap::new(),
            order_ids: HashSet::new(),
        }
    }
}
