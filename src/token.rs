use std::collections::HashMap;
use rust_decimal::Decimal;
use crate::node::{Node, Msg, TokenNode};

#[derive(Clone)]
pub struct Token {
    id: usize,
    name: String,
    msgs: Vec<Msg>,
    sheet: HashMap<usize, Decimal>,
}

impl Token {
    pub fn new(name: &str) -> Self {
        Self {
            id: 0,
            name: name.to_string(),
            msgs: Vec::new(),
            sheet: HashMap::new(),
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Node for Token {
    fn as_token_node(&mut self) -> Option<&mut dyn TokenNode> { Some(self) }
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

impl TokenNode for Token {}
