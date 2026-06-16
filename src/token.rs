use std::collections::{HashMap, HashSet};
use rust_decimal::Decimal;
use crate::node::{Node, Msg, TokenNode};

#[derive(Clone)]
pub struct Token {
    id: usize,
    name: String,
    msgs: Vec<Msg>,
    sheet: HashMap<usize, Decimal>,
    total_supply: Decimal,
    can_be_negative: bool,
    not_tradable: bool,
    swap_whitelist: HashSet<usize>,
}

impl Token {
    pub fn new(name: &str) -> Self {
        Self {
            id: 0,
            name: name.to_string(),
            msgs: Vec::new(),
            sheet: HashMap::new(),
            total_supply: Decimal::ZERO,
            can_be_negative: false,
            not_tradable: false,
            swap_whitelist: HashSet::new(),
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn total_supply(&self) -> Decimal {
        self.total_supply
    }
}

impl Node for Token {
    fn as_token_node(&mut self) -> Option<&mut dyn TokenNode> { Some(self) }
    fn as_token_node_ref(&self) -> Option<&dyn TokenNode> { Some(self) }
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

impl TokenNode for Token {
    fn total_supply(&self) -> Decimal {
        self.total_supply
    }
    fn set_total_supply(&mut self, supply: Decimal) {
        self.total_supply = supply;
    }
    fn can_be_negative(&self) -> bool {
        self.can_be_negative
    }
    fn set_can_be_negative(&mut self, can: bool) {
        self.can_be_negative = can;
    }
    fn not_tradable(&self) -> bool {
        self.not_tradable
    }
    fn set_not_tradable(&mut self, v: bool) {
        self.not_tradable = v;
    }
    fn swap_whitelist(&self) -> &HashSet<usize> {
        &self.swap_whitelist
    }
    fn set_swap_whitelist(&mut self, whitelist: HashSet<usize>) {
        self.swap_whitelist = whitelist;
    }
}
