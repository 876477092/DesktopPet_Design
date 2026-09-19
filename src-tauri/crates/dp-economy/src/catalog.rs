//! 商城目录（只读包装 `ShopConfig`）+ 成就判定器（FR-9-1）。

use std::collections::BTreeMap;

use dp_core::config::model::{AchievementsConfig, ShopConfig, ShopItemCfg};
use serde::{Deserialize, Serialize};

use crate::EconomyError;

/// 商城目录：按 item_id 索引 30 件商品 + 经济上限快照。
#[derive(Debug, Clone)]
pub struct Catalog {
    /// 商品索引。
    by_id: BTreeMap<String, ShopItemCfg>,
    /// 原始配置（供遍历 / UI）。
    shop: ShopConfig,
}

impl Catalog {
    /// 从 `ShopConfig` 构建（校验 ID 唯一）。
    pub fn new(shop: ShopConfig) -> Result<Self, String> {
        let mut by_id = BTreeMap::new();
        for item in &shop.items {
            if item.id.is_empty() {
                return Err("存在空的商品 ID".to_string());
            }
            if by_id.insert(item.id.clone(), item.clone()).is_some() {
                return Err(format!("商品 ID 重复：{}", item.id));
            }
        }
        Ok(Self { by_id, shop })
    }

    /// 按 ID 查商品。
    pub fn get(&self, id: &str) -> Option<&ShopItemCfg> {
        self.by_id.get(id)
    }

    /// 全部商品（遍历）。
    pub fn items(&self) -> &[ShopItemCfg] {
        &self.shop.items
    }

    /// 经济上限（C 契约）。
    #[must_use]
    pub fn caps(&self) -> crate::ledger::EconomyCaps {
        crate::ledger::EconomyCaps::new(
            self.shop.economy.coin_max,
            self.shop.economy.per_tx_max,
            self.shop.economy.daily_income_cap,
        )
    }

    /// 连续登录第 `day` 天奖励（超出数列尾部取 `loginStreakAfter`）。
    #[must_use]
    pub fn login_streak_reward(&self, day: u32) -> i64 {
        let idx = day as usize;
        if idx == 0 {
            return 0;
        }
        match self.shop.economy.login_streak.get(idx - 1) {
            Some(v) => *v,
            None => self.shop.economy.login_streak_after,
        }
    }

    /// 校验购买前置（存在 + 解锁），返回商品。
    pub fn check_purchasable(&self, id: &str, affinity_level: u32) -> Result<&ShopItemCfg, EconomyError> {
        let item = self.get(id).ok_or_else(|| EconomyError::ItemNotFound(id.to_string()))?;
        if affinity_level < item.unlock.affinity_level {
            return Err(EconomyError::Locked(item.name.clone()));
        }
        Ok(item)
    }
}

// ---------------------------------------------------------------------------
// 成就判定（FR-9-1）
// ---------------------------------------------------------------------------

/// 一条成就的持久化进度（save.economy.achievements）。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AchievementProgress {
    /// 累计进度（metric 累加值）。
    pub progress: f64,
    /// 是否已达成（一次性）。
    pub done: bool,
}

/// 成就判定器：消费「计数指标」增量，触发达成回调（幂等）。
#[derive(Debug, Clone)]
pub struct AchievementJudge {
    /// 成就目录。
    cfg: AchievementsConfig,
    /// id → 进度。
    progress: BTreeMap<String, AchievementProgress>,
}

impl AchievementJudge {
    /// 构建。
    #[must_use]
    pub fn new(cfg: AchievementsConfig) -> Self {
        Self { cfg, progress: BTreeMap::new() }
    }

    /// 从持久化形态还原。
    #[must_use]
    pub fn with_progress(mut self, progress: BTreeMap<String, AchievementProgress>) -> Self {
        self.progress = progress;
        self
    }

    /// 某指标当前进度。
    #[must_use]
    pub fn progress_of(&self, id: &str) -> AchievementProgress {
        self.progress.get(id).copied().unwrap_or_default()
    }

    /// 全部已达成成就 ID。
    pub fn done_ids(&self) -> Vec<&str> {
        self.progress
            .iter()
            .filter(|(_, p)| p.done)
            .map(|(k, _)| k.as_str())
            .collect()
    }

    /// 记录一次指标增量，返回本次**新达成**的成就（调用方据此入账 + 撒花）。
    ///
    /// 已达成的成就不会重复返回（幂等：`done=true` 后不再触发，FR-9-1）。
    pub fn record<'a>(&'a mut self, metric: &str, delta: f64) -> Vec<&'a dp_core::config::model::AchievementCfg> {
        // 本次新达成的成就 ID（只返回本次新达成的，FR-9-1 幂等）。
        let mut newly: Vec<String> = Vec::new();
        let targets: Vec<(String, f64)> = self
            .cfg
            .achievements
            .iter()
            .filter(|a| a.condition.metric == metric)
            .map(|a| (a.id.clone(), a.condition.target))
            .collect();
        for (id, target) in &targets {
            let p = self.progress.entry(id.clone()).or_default();
            if p.done {
                continue;
            }
            p.progress += delta;
            if p.progress >= *target {
                p.done = true;
                newly.push(id.clone());
            }
        }
        self.cfg
            .achievements
            .iter()
            .filter(|a| newly.contains(&a.id))
            .collect()
    }

    /// 序列化进度。
    #[must_use]
    pub fn progress_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.progress).unwrap_or(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dp_core::config::model::{AchievementCfg, AchievementConditionCfg, AchievementsConfig};

    fn cfg() -> AchievementsConfig {
        AchievementsConfig {
            version: 1,
            achievements: vec![
                AchievementCfg {
                    id: "ACH_FIRST_PET".into(),
                    name: "心心的第一个比心".into(),
                    desc: "第一次比心".into(),
                    coin: 20,
                    condition: AchievementConditionCfg { metric: "coaxCount".into(), target: 1.0 },
                },
                AchievementCfg {
                    id: "ACH_COAX_100".into(),
                    name: "百次抚摸".into(),
                    desc: "累计抚摸 100 次".into(),
                    coin: 100,
                    condition: AchievementConditionCfg { metric: "coaxCount".into(), target: 100.0 },
                },
            ],
        }
    }

    #[test]
    fn achievement_fires_once_and_is_idempotent() {
        let mut j = AchievementJudge::new(cfg());
        let fired = j.record("coaxCount", 1.0);
        assert_eq!(fired.len(), 1, "第 1 次应只触发 firstPet");
        assert_eq!(fired[0].id, "ACH_FIRST_PET");
        assert_eq!(fired[0].coin, 20);
        // 再记 99 次累计到 100。
        let fired2 = j.record("coaxCount", 99.0);
        assert_eq!(fired2.len(), 1);
        assert_eq!(fired2[0].id, "ACH_COAX_100");
        // 重复触发不再。
        let fired3 = j.record("coaxCount", 50.0);
        assert!(fired3.is_empty(), "已达成就不应重复触发（FR-9-1 幂等）");
    }

    #[test]
    fn catalog_login_streak_array() {
        use dp_core::config::model::{ShopConfig, ShopEconomyCfg};
        let shop = ShopConfig { version: 1, economy: ShopEconomyCfg::default(), items: vec![] };
        let cat = Catalog::new(shop).unwrap();
        assert_eq!(cat.login_streak_reward(1), 10);
        assert_eq!(cat.login_streak_reward(7), 50);
        assert_eq!(cat.login_streak_reward(8), 30, "第 8 天起取 after 值");
    }

    #[test]
    fn catalog_rejects_duplicate_item_id() {
        use dp_core::config::model::{ShopConfig, ShopEconomyCfg, ShopItemCfg, ItemQuotaCfg};
        let mk = |id: &str| ShopItemCfg {
            id: id.into(),
            name: String::new(),
            category: "food".into(),
            price: 10,
            effect: Default::default(),
            quota: ItemQuotaCfg::default(),
            unlock: Default::default(),
        };
        let shop = ShopConfig {
            version: 1,
            economy: ShopEconomyCfg::default(),
            items: vec![mk("A"), mk("A")],
        };
        assert!(Catalog::new(shop).is_err());
    }
}
