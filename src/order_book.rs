use std::collections::{BTreeMap, HashMap};
use rust_decimal::Decimal;
use crate::node::Drt;
use crate::order::OrderBrief;

/// 订单簿数据结构，使用 BTreeMap 实现 O(log n) 的插入、删除和查找
/// 
/// 买单队列：按价格降序、时间升序排列
/// 卖单队列：按价格升序、时间升序排列
pub struct OrderBook {
    // 买单队列: (-price, step_count) -> OrderBrief，负数实现价格降序
    buy_orders: BTreeMap<(Decimal, u64), OrderBrief>,
    // 卖单队列: (price, step_count) -> OrderBrief
    sell_orders: BTreeMap<(Decimal, u64), OrderBrief>,
    // 订单ID到键的映射，用于 O(1) 查找和取消
    order_index: HashMap<usize, (Drt, Decimal, u64)>,
}

impl Default for OrderBook {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderBook {
    pub fn new() -> Self {
        Self {
            buy_orders: BTreeMap::new(),
            sell_orders: BTreeMap::new(),
            order_index: HashMap::new(),
        }
    }

    /// 插入或更新订单
    /// 返回 true 如果是新订单，false 如果是更新
    pub fn insert(&mut self, brief: &OrderBrief) -> bool {
        // 如果订单已存在，先移除旧的
        let existing = self.order_index.get(&brief.id).cloned();
        let is_new = existing.is_none();
        if let Some((dir, price_key, time)) = existing {
            self.remove_from_queue(dir, &price_key, time);
        }

        // 插入新订单
        let (price_key, time_key) = self.make_key(brief.direction, &brief.price, brief.step_count_created);
        
        if brief.direction == Drt::Buy {
            let key = (price_key, time_key);
            self.buy_orders.insert(key, brief.clone());
            self.order_index.insert(brief.id, (Drt::Buy, price_key, time_key));
        } else {
            let key = (price_key, time_key);
            self.sell_orders.insert(key, brief.clone());
            self.order_index.insert(brief.id, (Drt::Sell, price_key, time_key));
        }

        is_new
    }

    /// 取消订单
    pub fn cancel(&mut self, order_id: usize) -> bool {
        let Some((dir, price_key, time)) = self.order_index.remove(&order_id) else {
            return false;
        };
        self.remove_from_queue(dir, &price_key, time);
        true
    }

    /// 检查订单是否存在
    pub fn contains(&self, order_id: usize) -> bool {
        self.order_index.contains_key(&order_id)
    }

    /// 获取最佳买单（最高价格）
    pub fn best_buy(&self) -> Option<&OrderBrief> {
        // BTreeMap 按 key 升序，反转价格后第一个就是最高价格
        self.buy_orders.values().next()
    }

    /// 获取最佳卖单（最低价格）
    pub fn best_sell(&self) -> Option<&OrderBrief> {
        self.sell_orders.values().next()
    }

    /// 移除并返回最佳买单
    pub fn pop_best_buy(&mut self) -> Option<OrderBrief> {
        let key = self.buy_orders.keys().next().cloned()?;
        let brief = self.buy_orders.remove(&key)?;
        self.order_index.remove(&brief.id);
        Some(brief)
    }

    /// 移除并返回最佳卖单
    pub fn pop_best_sell(&mut self) -> Option<OrderBrief> {
        let key = self.sell_orders.keys().next().cloned()?;
        let brief = self.sell_orders.remove(&key)?;
        self.order_index.remove(&brief.id);
        Some(brief)
    }

    /// 更新订单数量（部分成交后）
    pub fn update_volume(&mut self, order_id: usize, new_src_volume: Decimal, new_dst_volume: Decimal) -> bool {
        let Some(&(dir, ref price_key, time)) = self.order_index.get(&order_id) else {
            return false;
        };

        let brief = match dir {
            Drt::Buy => self.buy_orders.get_mut(&(*price_key, time)),
            Drt::Sell => self.sell_orders.get_mut(&(*price_key, time)),
        };

        if let Some(b) = brief {
            b.src_volume = new_src_volume;
            b.dst_volume = new_dst_volume;
            true
        } else {
            false
        }
    }

    /// 获取买单队列长度
    pub fn buy_count(&self) -> usize {
        self.buy_orders.len()
    }

    /// 获取卖单队列长度
    pub fn sell_count(&self) -> usize {
        self.sell_orders.len()
    }

    /// 获取总订单数
    pub fn total_count(&self) -> usize {
        self.order_index.len()
    }

    /// 获取买单迭代器（价格降序）
    pub fn buy_iter(&self) -> impl Iterator<Item = &OrderBrief> {
        self.buy_orders.values()
    }

    /// 获取卖单迭代器（价格升序）
    pub fn sell_iter(&self) -> impl Iterator<Item = &OrderBrief> {
        self.sell_orders.values()
    }

    /// 获取指定深度的订单簿快照（返回聚合后的价格-数量列表）
    pub fn get_depth(&self, direction: Drt, depth: usize) -> Vec<(Decimal, Decimal)> {
        let mut result = Vec::with_capacity(depth);
        let mut current_price: Option<Decimal> = None;
        let mut current_volume = Decimal::ZERO;
        let mut count = 0;

        let iter: Box<dyn Iterator<Item = &OrderBrief>> = match direction {
            Drt::Buy => Box::new(self.buy_orders.values()),
            Drt::Sell => Box::new(self.sell_orders.values()),
        };

        for order in iter {
            match &current_price {
                None => {
                    current_price = Some(order.price);
                    current_volume = order.src_volume;
                }
                Some(price) => {
                    if order.price == *price {
                        current_volume += order.src_volume;
                    } else {
                        if count == depth {
                            break;
                        }
                        result.push((*price, current_volume));
                        count += 1;
                        current_price = Some(order.price);
                        current_volume = order.src_volume;
                    }
                }
            }
        }

        if let Some(price) = current_price && count < depth {
            result.push((price, current_volume));
        }

        result
    }

    // 辅助方法：从队列中移除订单
    fn remove_from_queue(&mut self, dir: Drt, price_key: &Decimal, time: u64) {
        match dir {
            Drt::Buy => {
                self.buy_orders.remove(&(*price_key, time));
            }
            Drt::Sell => {
                self.sell_orders.remove(&(*price_key, time));
            }
        }
    }

    // 辅助方法：生成排序键
    // 对于买单，返回 (-price, step_count) 实现价格降序、时间升序
    // 对于卖单，返回 (price, step_count) 实现价格升序、时间升序
    fn make_key(&self, direction: Drt, price: &Decimal, time: u64) -> (Decimal, u64) {
        match direction {
            Drt::Buy => (-price, time),   // 负数实现降序
            Drt::Sell => (*price, time),
        }
    }
}
