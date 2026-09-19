//! 经济运行时（S8-M5/M6）：把 dp-economy 的账本 / 限购 / 目录 / 背包 / 成就判定
//! 装配进 core-loop，并负责与 `save.economy` / `save.inventory` 两段 JSON 的互转。
//!
//! 唯一写者 = core-loop 线程（与 `emotion` 同生命周期）；购买经
//! [`crate::bridge::CoreInput::Purchase`] 进入 [`Self::buy`]，打工结算经
//! [`Self::credit_work`] 入账。本类型零时钟：`at_ms` / `day_key` 由调用方注入。

use dp_core::config::model::{AchievementsConfig, ShopConfig, ShopItemCfg};
use dp_economy::{
    AchievementJudge, Catalog, CoinSource, CreditOutcome, EconomyError, Inventory, Ledger,
    OrderOutcome, OrderRequest, QuotaTracker,
};
use serde_json::Value;

/// 经济运行时容器。
pub struct EconomyRuntime {
    /// 商城目录（只读；含 30 商品索引与经济上限快照）。
    catalog: Catalog,
    /// 成就判定器（幂等触发）。
    judge: AchievementJudge,
    /// 心币账本（可重放；三道硬顶）。
    ledger: Ledger,
    /// 限购追踪（日 / 周桶自动重置）。
    quota: QuotaTracker,
    /// 背包。
    inventory: Inventory,
}

impl EconomyRuntime {
    /// 全新档（经济从零开始；配置缺失时降级为空目录）。
    #[must_use]
    pub fn new(shop: ShopConfig, achievements: AchievementsConfig) -> Self {
        let catalog =
            Catalog::new(shop).unwrap_or_else(|_| Catalog::new(ShopConfig::default()).unwrap());
        let caps = catalog.caps();
        Self {
            catalog,
            judge: AchievementJudge::new(achievements),
            ledger: Ledger::new(caps),
            quota: QuotaTracker::new(),
            inventory: Inventory::new(),
        }
    }

    /// 从存档段还原（容错：流水缺字段时用余额合成一笔种子，保证 `replay()==balance()`）。
    #[must_use]
    pub fn restore(
        shop: ShopConfig,
        achievements: AchievementsConfig,
        economy: &Value,
        inventory: &Value,
    ) -> Self {
        let mut this = Self::new(shop, achievements);
        let caps = this.catalog.caps();

        let stored_coin = economy.get("coin").and_then(|v| v.as_i64()).unwrap_or(0);
        let earned_today =
            economy.get("earnedToday").and_then(|v| v.as_i64()).unwrap_or(0);
        let day_key =
            economy.get("dayKey").and_then(|v| v.as_str()).unwrap_or_default().to_string();

        // 尝试完整反序列化流水；失败/缺字段则用余额合成种子（重放不变量不破）。
        let records = economy
            .get("ledger")
            .and_then(|v| serde_json::from_value::<Vec<dp_economy::CreditRecord>>(v.clone()).ok())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| {
                if stored_coin == 0 {
                    Vec::new()
                } else {
                    vec![dp_economy::CreditRecord {
                        seq: 1,
                        at_ms: 0,
                        ref_id: "restore:seed".to_string(),
                        source: CoinSource::MigrationGrant,
                        amount: stored_coin,
                        balance_after: stored_coin,
                        day_bucket: day_key.clone(),
                    }]
                }
            });

        this.ledger = Ledger::from_parts(records, earned_today, day_key, caps);

        // 限购表。
        if let Some(q) = economy.get("quotas") {
            if let Ok(t) = serde_json::from_value::<QuotaTracker>(q.clone()) {
                this.quota = t;
            }
        }
        // 背包。
        if let Ok(wire) = serde_json::from_value::<Vec<dp_core::event::InventoryItemWire>>(
            inventory.clone(),
        ) {
            this.inventory = Inventory::from_wire(&wire);
        }
        this
    }

    /// 当前余额。
    #[must_use]
    pub fn balance(&self) -> i64 {
        self.ledger.balance()
    }

    /// 背包某物品数量。
    #[must_use]
    pub fn inventory_count(&self, item_id: &str) -> u32 {
        self.inventory.count(item_id)
    }

    /// 全部商品（前端商城卡渲染）。
    #[must_use]
    pub fn catalog_items(&self) -> &[ShopItemCfg] {
        self.catalog.items()
    }

    /// 背包 wire（前端 InventoryList 渲染）。
    #[must_use]
    pub fn inventory_wire(&self) -> Vec<dp_core::event::InventoryItemWire> {
        self.inventory.to_wire()
    }

    /// 商城购买（事务；失败整体回滚，账 / 包 / 限购不变）。
    pub fn buy(
        &mut self,
        item_id: &str,
        qty: u32,
        affinity_level: u32,
        day_key: &str,
        week_key: &str,
        at_ms: i64,
    ) -> Result<OrderOutcome, EconomyError> {
        let req = OrderRequest {
            item_id: item_id.to_string(),
            qty,
            affinity_level,
            day_key: day_key.to_string(),
            week_key: week_key.to_string(),
        };
        dp_economy::fulfillment::purchase(
            &self.catalog,
            &mut self.ledger,
            &mut self.quota,
            &mut self.inventory,
            &req,
            at_ms,
        )
    }

    /// 打工结算入账（主渠道；经三道硬顶截断，幂等 refId）。
    pub fn credit_work(
        &mut self,
        activity_id: &str,
        amount: i64,
        ref_id: &str,
        at_ms: i64,
        day_key: &str,
    ) -> CreditOutcome {
        self.ledger.credit(
            CoinSource::WorkSettle { activity_id: activity_id.to_string() },
            amount,
            ref_id,
            at_ms,
            day_key,
        )
    }

    /// 存档经济段（`save.economy`）。
    #[must_use]
    pub fn to_save_economy(&self) -> Value {
        serde_json::json!({
            "coin": self.ledger.balance(),
            "ledger": self.ledger.records(),
            "earnedToday": self.ledger.earned_today(),
            "dayKey": self.ledger.day_key(),
            "quotas": self.quota.to_json(),
            "achievements": self.judge.progress_json(),
        })
    }

    /// 存档背包段（`save.inventory`）。
    #[must_use]
    pub fn to_save_inventory(&self) -> Value {
        serde_json::to_value(self.inventory.to_wire()).unwrap_or(Value::Array(Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dp_core::config::model::{
        AchievementsConfig, ItemEffectCfg, ItemQuotaCfg, ItemUnlockCfg, ShopConfig,
        ShopEconomyCfg, ShopItemCfg,
    };

    fn shop(cracker_price: i64) -> ShopConfig {
        ShopConfig {
            version: 1,
            economy: ShopEconomyCfg::default(),
            items: vec![ShopItemCfg {
                id: "FOOD_CRACKER".into(),
                name: "狐狸饼干".into(),
                category: "food".into(),
                price: cracker_price,
                effect: ItemEffectCfg { satiety: Some(15.0), ..Default::default() },
                quota: ItemQuotaCfg { kind: "once".into(), limit: 1 },
                unlock: ItemUnlockCfg { affinity_level: 0, ..Default::default() },
            }],
        }
    }

    #[test]
    fn buy_cracker_deducts_and_stocks() {
        // 种子余额代表已有心币（绕过日顶），经 restore 注入。
        let mut rt = EconomyRuntime::restore(
            shop(30),
            AchievementsConfig::default(),
            &serde_json::json!({ "coin": 1280, "ledger": [], "earnedToday": 0, "dayKey": "2026-09-19" }),
            &serde_json::json!([]),
        );
        let out = rt
            .buy("FOOD_CRACKER", 1, 1, "2026-09-19", "2026-W38", 100)
            .unwrap();
        assert_eq!(out.spent, 30);
        assert_eq!(out.balance_after, 1250, "1280 买 30 应剩 1250");
        assert_eq!(rt.inventory_count("FOOD_CRACKER"), 1);
    }

    #[test]
    fn restore_preserves_balance_and_inventory() {
        let mut rt = EconomyRuntime::restore(
            shop(10),
            AchievementsConfig::default(),
            &serde_json::json!({ "coin": 500, "ledger": [], "earnedToday": 0, "dayKey": "2026-09-19" }),
            &serde_json::json!([]),
        );
        rt.buy("FOOD_CRACKER", 1, 1, "2026-09-19", "2026-W38", 100).unwrap();
        let econ = rt.to_save_economy();
        let inv = rt.to_save_inventory();

        let rt2 = EconomyRuntime::restore(
            shop(10),
            AchievementsConfig::default(),
            &econ,
            &inv,
        );
        assert_eq!(rt2.balance(), 490, "500 - 10");
        assert_eq!(rt2.inventory_count("FOOD_CRACKER"), 1);
    }
}
