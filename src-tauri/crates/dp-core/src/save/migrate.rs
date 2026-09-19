//! v1 → v2 存档迁移（S8-M7；`02 §5 K-7` 规则①~⑥）。
//!
//! v1 形状（`01` 旧基线）：`{ "v":1, "boredom":f32, "coldLevel":u8, "coin":i64 }`。
//! v2 全量默认档之上做最小叠加：
//!   ① 六维补默认（"状态良好"）；
//!   ② P 由旧 boredom 反推 `p = boredom × 1.2`（封顶 120），coldLevel → neglect.level；
//!   ③ 性格 / 自适应 / 粗暴 / 敏感度取 v2 默认；
//!   ④ 经济老用户补偿 `min(20 × 亲密度等级, 200)`，走 `CoinSource::MigrationGrant`，
//!      `refId="migration:v1tov2"`（幂等，一次性）；
//!   ⑤ 其余容器补空；⑥ `v = 2`。
//!
//! 本函数**纯函数、零时钟、零 IO**：备份 v1bak 与写回 v2 由调用方（store::load）完成。

use serde_json::Value;

use super::schema::SaveFileV2;

/// 由 v1 JSON 构造 v2 存档（叠加最小迁移）。
///
/// `v1` 必须是 `v == 1` 的对象；缺字段一律取 v2 默认，不 panic（R19 不崩）。
#[must_use]
pub fn migrate(v1: &Value) -> SaveFileV2 {
    let mut save = SaveFileV2::default();

    // ② boredom → P（×1.2，封顶 120）；coldLevel → neglect.level。
    let boredom = v1.get("boredom").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let p = (boredom * 1.2).min(save.emotion.neglect.cap);
    save.emotion.neglect.p = p;
    let cold_level = v1.get("coldLevel").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
    save.emotion.neglect.level = cold_level;

    // ④ 经济：保留 v1 coin 作为本金，再加迁移补偿 min(20×亲密度等级,200)。
    let old_coin = v1.get("coin").and_then(|v| v.as_i64()).unwrap_or(0).max(0);
    let affinity_level = save.values.affinity_level;
    let grant = (20 * affinity_level as i64).min(200);
    let coin = old_coin + grant;

    save.economy = serde_json::json!({
        "coin": coin,
        "ledger": [
            {
                "seq": 1,
                "refId": "migration:v1tov2",
                "source": { "kind": "migrationGrant" },
                "amount": grant,
                "balanceAfter": grant
            }
        ],
        "earnedToday": 0,
        "dayKey": "",
        "quotas": {},
        "loginStreak": { "days": 0, "dayKey": "" }
    });

    save
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_boredom_coldlevel_and_coin() {
        let v1 = serde_json::json!({ "v": 1, "boredom": 40.0, "coldLevel": 2, "coin": 500 });
        let save = migrate(&v1);
        assert_eq!(save.v, 2);
        // p = 40 × 1.2 = 48（未超顶）。
        assert!((save.emotion.neglect.p - 48.0).abs() < 0.01, "p={}", save.emotion.neglect.p);
        assert_eq!(save.emotion.neglect.level, 2);
        // 亲密度默认 Lv1 → 补偿 20；500 + 20 = 520。
        assert_eq!(save.economy["coin"], 520);
        assert_eq!(save.economy["ledger"][0]["refId"], "migration:v1tov2");
    }

    #[test]
    fn p_is_capped_at_120() {
        let v1 = serde_json::json!({ "v": 1, "boredom": 200.0, "coldLevel": 0, "coin": 0 });
        let save = migrate(&v1);
        assert!(save.emotion.neglect.p <= 120.0 + 0.01);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let v1 = serde_json::json!({ "v": 1 });
        let save = migrate(&v1);
        assert_eq!(save.v, 2);
        assert_eq!(save.economy["coin"], 20, "无旧币时仍给 Lv1 补偿 20");
        // v2 可回环反序列化。
        let text = serde_json::to_string(&save).unwrap();
        let _: SaveFileV2 = serde_json::from_str(&text).unwrap();
    }
}
