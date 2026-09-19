//! 限购追踪：按 item 记录当日 / 当周已购次数，跨日 / 跨周自动清零（AC-33）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 单个商品的限购计数。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaEntry {
    /// 当前周期键（日 = `YYYY-MM-DD`，周 = `YYYY-Www`）。
    period_key: String,
    /// 本周期已购次数。
    used: u32,
}

/// 限购表（序列化进 save.economy.quotas）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaTracker {
    /// item_id → 计数。
    entries: BTreeMap<String, QuotaEntry>,
}

impl QuotaTracker {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 本周期已购次数（跨周期自动归 0）。
    pub fn used(&mut self, item_id: &str, period_key: &str) -> u32 {
        match self.entries.get_mut(item_id) {
            Some(e) if e.period_key == period_key => e.used,
            Some(e) => {
                e.period_key = period_key.to_string();
                e.used = 0;
                0
            }
            None => 0,
        }
    }

    /// 剩余可购次数（`limit=0` 表示不限，返回 u32::MAX）。
    pub fn remaining(&mut self, item_id: &str, limit: u32, period_key: &str) -> u32 {
        if limit == 0 {
            return u32::MAX;
        }
        let used = self.used(item_id, period_key);
        limit.saturating_sub(used)
    }

    /// 记一次购买（成功后调用）。
    pub fn record(&mut self, item_id: &str, period_key: &str) {
        let e = self.entries.entry(item_id.to_string()).or_default();
        if e.period_key != period_key {
            e.period_key = period_key.to_string();
            e.used = 0;
        }
        e.used += 1;
    }

    /// 序列化（save.economy.quotas）。
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.entries).unwrap_or(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_quota_resets_next_day() {
        let mut q = QuotaTracker::new();
        // 饭团每日 5 个。
        for _ in 0..5 {
            assert!(q.remaining("RICE", 5, "2026-09-19") > 0);
            q.record("RICE", "2026-09-19");
        }
        assert_eq!(q.remaining("RICE", 5, "2026-09-19"), 0, "当日第 6 个应被拒（AC-33）");
        // 跨日 00:00 重置。
        assert_eq!(q.remaining("RICE", 5, "2026-09-20"), 5, "跨日应重置为满额");
    }

    #[test]
    fn weekly_quota_is_independent_key() {
        let mut q = QuotaTracker::new();
        q.record("FOAM", "2026-W38");
        q.record("FOAM", "2026-W38");
        assert_eq!(q.remaining("FOAM", 1, "2026-W38"), 0, "每周 1，已购应满");
        // 跨周。
        assert_eq!(q.remaining("FOAM", 1, "2026-W39"), 1);
    }

    #[test]
    fn unlimited_quota_returns_max() {
        let mut q = QuotaTracker::new();
        assert_eq!(q.remaining("DECOR", 0, "2026-09-19"), u32::MAX);
    }
}
