//! 购买事务执行器（K-13：校验→限购预留→扣款→入库；失败冲正回到调用前）。

use crate::catalog::Catalog;
use crate::inventory::Inventory;
use crate::ledger::Ledger;
use crate::order::{OrderOutcome, OrderRequest};
use crate::quota::QuotaTracker;
use crate::EconomyError;

/// 执行一笔购买。
///
/// 步骤（任一失败即整体回滚，账本 / 背包 / 限购不变）：
///   1. 商品存在 + 亲密度解锁；
///   2. 限购余量足够（按 item.quota.kind 选日 / 周桶）；
///   3. 余额足够（总价 = price × qty）；
///   4. 扣款（debit）→ 入库（inventory.add）→ 记限购（quota.record）。
///
/// # Errors
/// 商品不存在 / 未解锁 / 超限购 / 余额不足时返回错误，状态不变。
pub fn purchase(
    catalog: &Catalog,
    ledger: &mut Ledger,
    quota: &mut QuotaTracker,
    inv: &mut Inventory,
    req: &OrderRequest,
    at_ms: i64,
) -> Result<OrderOutcome, EconomyError> {
    let qty = req.qty.max(1);

    // 1. 解锁校验。
    let item = catalog.check_purchasable(&req.item_id, req.affinity_level)?;
    let price = item.price;

    // 2. 限购预留（按周期选桶）。
    let (period_key, limit) = match item.quota.kind.as_str() {
        "daily" => (req.day_key.clone(), item.quota.limit),
        "weekly" => (req.week_key.clone(), item.quota.limit),
        "once" => (format!("once:{}", item.id), 1u32),
        _ => (String::new(), 0u32), // unlimited
    };
    if limit > 0 {
        let remaining = quota.remaining(&item.id, limit, &period_key);
        if remaining < qty {
            return Err(EconomyError::QuotaExceeded(format!(
                "{}（剩余 {remaining} / 需求 {qty}）",
                item.name
            )));
        }
    }

    // 3. 余额校验。
    let total = price
        .checked_mul(qty as i64)
        .ok_or_else(|| EconomyError::QuotaExceeded("数量溢出".to_string()))?;
    if total <= 0 {
        return Ok(OrderOutcome { qty: 0, spent: 0, balance_after: ledger.balance() });
    }
    if ledger.balance() < total {
        return Err(EconomyError::InsufficientCoin { need: total, have: ledger.balance() });
    }

    // 4. 扣款（成功才继续；失败时下面三步都不会发生，自然回滚）。
    let ref_id = format!("purchase:{}:{at_ms}:{qty}", item.id);
    ledger.debit(total, &ref_id, at_ms, &req.day_key)?;

    // 5. 入库 + 限购记账。
    inv.add(&item.id, qty);
    if limit > 0 {
        for _ in 0..qty {
            quota.record(&item.id, &period_key);
        }
    }

    Ok(OrderOutcome { qty, spent: total, balance_after: ledger.balance() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CoinSource;
    use dp_core::config::model::{
        AchievementsConfig, ItemEffectCfg, ItemQuotaCfg, ItemUnlockCfg, ShopConfig,
        ShopEconomyCfg, ShopItemCfg,
    };

    fn catalog(rice_limit: u32) -> Catalog {
        let shop = ShopConfig {
            version: 1,
            economy: ShopEconomyCfg::default(),
            items: vec![ShopItemCfg {
                id: "FOOD_RICEBALL".into(),
                name: "米饭团".into(),
                category: "food".into(),
                price: 15,
                effect: ItemEffectCfg { satiety: Some(20.0), ..Default::default() },
                quota: ItemQuotaCfg { kind: "daily".into(), limit: rice_limit },
                unlock: ItemUnlockCfg { affinity_level: 0, ..Default::default() },
            }],
        };
        Catalog::new(shop).unwrap()
    }

    fn buy(cat: &Catalog, coin: i64) -> (Ledger, QuotaTracker, Inventory) {
        // 种子余额代表用户**已有**心币（非今日新赚），绕过日顶——
        // 与生产侧 `EconomyRuntime::restore` 用种子流水还原同口径。
        use crate::ledger::CreditRecord;
        let rec = CreditRecord {
            seq: 1,
            at_ms: 1,
            ref_id: "seed".into(),
            source: CoinSource::MigrationGrant,
            amount: coin,
            balance_after: coin,
            day_bucket: "2026-09-19".into(),
        };
        let ledger = Ledger::from_parts(vec![rec], 0, "2026-09-19".into(), cat.caps());
        (ledger, QuotaTracker::new(), Inventory::new())
    }

    #[test]
    fn buy_riceball_deduct_and_stock() {
        let cat = catalog(5);
        let (mut ledger, mut quota, mut inv) = buy(&cat, 1280);
        let out = purchase(
            &cat,
            &mut ledger,
            &mut quota,
            &mut inv,
            &OrderRequest {
                item_id: "FOOD_RICEBALL".into(),
                qty: 1,
                affinity_level: 1,
                day_key: "2026-09-19".into(),
                week_key: "2026-W38".into(),
            },
            100,
        )
        .unwrap();
        assert_eq!(out.spent, 15);
        assert_eq!(out.balance_after, 1265, "1280 买 15 应剩 1265（AC-24 同型）");
        assert_eq!(inv.count("FOOD_RICEBALL"), 1);
    }

    #[test]
    fn quota_exceeded_then_resets_next_day() {
        let cat = catalog(5);
        let (mut ledger, mut quota, mut inv) = buy(&cat, 500);
        for _ in 0..5 {
            purchase(
                &cat,
                &mut ledger,
                &mut quota,
                &mut inv,
                &OrderRequest {
                    item_id: "FOOD_RICEBALL".into(),
                    qty: 1,
                    affinity_level: 1,
                    day_key: "2026-09-19".into(),
                    week_key: "2026-W38".into(),
                },
                100,
            )
            .unwrap();
        }
        // 第 6 个被拒。
        let err = purchase(
            &cat,
            &mut ledger,
            &mut quota,
            &mut inv,
            &OrderRequest {
                item_id: "FOOD_RICEBALL".into(),
                qty: 1,
                affinity_level: 1,
                day_key: "2026-09-19".into(),
                week_key: "2026-W38".into(),
            },
            101,
        )
        .unwrap_err();
        assert!(matches!(err, EconomyError::QuotaExceeded { .. }), "应超限购被拒（AC-33）");
        // 跨日重置。
        purchase(
            &cat,
            &mut ledger,
            &mut quota,
            &mut inv,
            &OrderRequest {
                item_id: "FOOD_RICEBALL".into(),
                qty: 1,
                affinity_level: 1,
                day_key: "2026-09-20".into(),
                week_key: "2026-W38".into(),
            },
            102,
        )
        .unwrap();
    }

    #[test]
    fn insufficient_coin_rolls_back() {
        let cat = catalog(5);
        let (mut ledger, mut quota, mut inv) = buy(&cat, 10); // 只够买不到 1 个（15）
        let err = purchase(
            &cat,
            &mut ledger,
            &mut quota,
            &mut inv,
            &OrderRequest {
                item_id: "FOOD_RICEBALL".into(),
                qty: 1,
                affinity_level: 1,
                day_key: "2026-09-19".into(),
                week_key: "2026-W38".into(),
            },
            100,
        )
        .unwrap_err();
        assert!(matches!(err, EconomyError::InsufficientCoin { .. }));
        assert_eq!(ledger.balance(), 10, "失败不应动账");
        assert_eq!(inv.count("FOOD_RICEBALL"), 0);
    }

    #[allow(dead_code)]
    fn _unused(_: &AchievementsConfig) {}
}
