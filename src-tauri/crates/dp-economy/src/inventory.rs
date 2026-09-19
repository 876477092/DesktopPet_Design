//! 背包：item_id → 数量（save.inventory）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 背包（键序 item_id，便于稳定序列化）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    /// item_id → 数量。
    items: BTreeMap<String, u32>,
}

impl Inventory {
    /// 空背包。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 某物品数量。
    #[must_use]
    pub fn count(&self, item_id: &str) -> u32 {
        self.items.get(item_id).copied().unwrap_or(0)
    }

    /// 增加（购买成功后调用）。
    pub fn add(&mut self, item_id: &str, qty: u32) {
        if qty == 0 {
            return;
        }
        *self.items.entry(item_id.to_string()).or_insert(0) += qty;
    }

    /// 消耗（使用道具；数量不足返回 false，背包不变）。
    pub fn consume(&mut self, item_id: &str, qty: u32) -> bool {
        let have = self.count(item_id);
        if have < qty {
            return false;
        }
        let new = have - qty;
        if new == 0 {
            self.items.remove(item_id);
        } else {
            self.items.insert(item_id.to_string(), new);
        }
        true
    }

    /// 序列化为 save.inventory（`[{itemId,qty}]`，与前端 wire 对齐）。
    #[must_use]
    pub fn to_wire(&self) -> Vec<dp_core::event::InventoryItemWire> {
        self.items
            .iter()
            .map(|(id, &qty)| dp_core::event::InventoryItemWire { item_id: id.clone(), qty })
            .collect()
    }

    /// 从 wire 还原。
    #[must_use]
    pub fn from_wire(wire: &[dp_core::event::InventoryItemWire]) -> Self {
        let mut items = BTreeMap::new();
        for w in wire {
            if w.qty > 0 {
                items.insert(w.item_id.clone(), w.qty);
            }
        }
        Self { items }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_consume_roundtrip() {
        let mut inv = Inventory::new();
        inv.add("RICE", 3);
        inv.add("RICE", 2);
        assert_eq!(inv.count("RICE"), 5);
        assert!(inv.consume("RICE", 2));
        assert_eq!(inv.count("RICE"), 3);
        assert!(!inv.consume("RICE", 10), "不足不应扣");
        assert_eq!(inv.count("RICE"), 3);
    }
}
