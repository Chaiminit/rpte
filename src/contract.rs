use std::collections::HashMap;
use crate::node::{Node, Msg, ContractNode, ContractState, ContractFn, EngineReader};

/// 合约节点：包含三个行为函数的状态机
pub struct Contract {
    pub id: usize,
    pub msgs: Vec<Msg>,
    pub owner_node_id: usize,
    pub state: ContractState,
    pub step_count_created: u64,
    on_create: Option<ContractFn>,
    on_update: Option<ContractFn>,
    on_end: Option<ContractFn>,
    sheet: HashMap<usize, rust_decimal::Decimal>,
}

impl Contract {
    pub fn new() -> Self {
        Self {
            id: 0,
            msgs: Vec::new(),
            owner_node_id: 0,
            state: ContractState::Destroyed,
            step_count_created: 0,
            on_create: None,
            on_update: None,
            on_end: None,
            sheet: HashMap::new(),
        }
    }

    /// 主实例专用：使用 EngineReader 触发状态机更新
    pub fn update_with_reader(&mut self, reader: &dyn EngineReader, step_count: u64) {
        match self.state {
            ContractState::Creating => {
                if let Some(on_create) = self.on_create.take() {
                    let msgs = on_create(self, reader, step_count);
                    self.msgs.extend(msgs);
                }
                self.state = ContractState::Running;
            }
            ContractState::Running => {
                let on_update = self.on_update.clone();
                if let Some(on_update) = on_update {
                    let msgs = on_update(self, reader, step_count);
                    self.msgs.extend(msgs);
                }
            }
            ContractState::Ending => {
                if let Some(on_end) = self.on_end.take() {
                    let msgs = on_end(self, reader, step_count);
                    self.msgs.extend(msgs);
                }
                self.state = ContractState::Destroyed;
                // 清理行为函数引用
                self.on_update = None;
            }
            ContractState::Destroyed => {}
        }
    }
}

impl Node for Contract {
    fn as_contract_node(&mut self) -> Option<&mut dyn ContractNode> { Some(self) }
    fn get_msgs(&mut self) -> &mut Vec<Msg> { &mut self.msgs }
    fn get_id(&self) -> usize { self.id }
    fn set_id(&mut self, id: usize) { self.id = id; }

    /// 从实例（在 nodes 中）的 update 是空操作，
    /// 主实例的 update 由引擎通过 update_with_reader 调用。
    fn update(&mut self, _step_count: u64) {}

    fn balance(&self, token: usize) -> rust_decimal::Decimal {
        self.sheet.get(&token).copied().unwrap_or(rust_decimal::Decimal::ZERO)
    }
    fn set_balance(&mut self, token: usize, volume: rust_decimal::Decimal) {
        self.sheet.insert(token, volume);
    }
    fn drain_balances(&mut self) -> HashMap<usize, rust_decimal::Decimal> {
        std::mem::take(&mut self.sheet)
    }
}

impl ContractNode for Contract {
    fn get_state(&self) -> ContractState { self.state }
    fn get_owner_node_id(&self) -> usize { self.owner_node_id }
    fn get_step_count_created(&self) -> u64 { self.step_count_created }

    fn deploy(&mut self, owner_node_id: usize, on_create: ContractFn, on_update: ContractFn, on_end: ContractFn, step_count: u64) {
        self.owner_node_id = owner_node_id;
        self.on_create = Some(on_create);
        self.on_update = Some(on_update);
        self.on_end = Some(on_end);
        self.step_count_created = step_count;
        self.state = ContractState::Creating;
    }

    fn end(&mut self) {
        if self.state == ContractState::Running {
            self.state = ContractState::Ending;
        }
    }

    fn get_all_balances(&self) -> &HashMap<usize, rust_decimal::Decimal> {
        &self.sheet
    }
}