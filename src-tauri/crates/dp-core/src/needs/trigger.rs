//! 生存动作**触发调度器**（S7-M9，T-18 段 · 资源批次 B / `02 §5.11` / `01 §6.12` FR-12）。
//!
//! ## 职责（03 台账 S7-M9「完成 needs→动作 触发映射」）
//!
//!   - **分档轮询**：消费 [`BandEffects::action_candidates`]（S7-M2 只给「该触发哪个
//!     动作 ID」，本模块负责**何时触发**——按档位 `intervalSec` 做间隔闸门，缺省高频档
//!     用 [`DEFAULT_BAND_INTERVAL_SEC`] 兜底，避免 1Hz 轮询把仲裁器打满）；
//!   - **求助气泡映射**：讨食（ACT-N-01）→ `begFood` 池；求洗澡（ACT-N-06）→ `begBath`
//!     池（`01 §6.16.3` 求助气泡）；其余需求动作不弹求助气泡；
//!   - **喂食 / 洗澡事务**：喂食开始 → ACT-N-02（吃饭演出）、喂食完成且饱食 ≥ 满档阈值
//!     → ACT-N-03（吃饱满足，AC-22）；洗澡开始 / 结束 → `needs.json.bath.actionId` /
//!     `endActionId`（ACT-N-07 / ACT-N-08，AC-22）。
//!
//! ## 与 S7-M2 的切割（禁止顺手改动）
//!
//!   - 本模块**不含**数值推进 / 分档解析（归 [`super::bands`] / `super` tick）；
//!   - **不含**洗澡流程状态机（时长 8~12s 演出编排归 S8；本模块只给事务**端点动作**）；
//!   - 触发动作最终能否实播由 `actions.json` 的 `disabled` 标志 + 仲裁器定夺
//!     （资源批次 B 图集未交付 → 出厂 `disabled=true`，`02 §10.1` R3 自动跳过；
//!     资源到位后翻 false 即零改动实播，与 ACT-P-03 同口径）。
//!
//! ## 时间纪律（C3）与配置纪律（C7）
//!
//!   `now_ms` 由调用方注入；分档动作 / 间隔 / 洗澡端点动作全部取自 `needs.json`。
//!   喂食端点动作（ACT-N-02 / ACT-N-03）按 `actions.json` 触发值语义以**常量化**
//!   固定（与 core-loop 既有 `REMINDER_ACTION_ID` 同口径，ID 为冻结契约），
//!   实播与否仍由配置 `disabled` 门控。

use std::collections::BTreeMap;

use crate::config::model::NeedsConfig;
use crate::needs::bands::BandEffects;

/// 无 `intervalSec` 档位的默认触发间隔（秒）。
///
/// 口径：`needs.json` 中 `peckish`（3~8min）/ `dirty`（2~5min）显式声明间隔；
/// `hungry` / `starving` / `stained` / `filthy` 属「高频」档但未声明 → 默认 60s
/// （高频但不过载，与求助退避 R-C 的分钟级量级一致；数值待 S8 演出节奏验收时复核）。
pub const DEFAULT_BAND_INTERVAL_SEC: u64 = 60;

/// 喂食开始动作（`actions.json` ACT-N-02 触发值 `feed`；冻结契约，见模块文档）。
pub const FEED_START_ACTION_ID: &str = "ACT-N-02";

/// 喂食完成且饱食达满档后的满足动作（`actions.json` ACT-N-03 触发值 `satiety>70`）。
pub const FEED_DONE_SATISFIED_ACTION_ID: &str = "ACT-N-03";

/// 一次「该触发」的动作意图（S7-M9）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedActionIntent {
    /// 触发动作 ID（如 `ACT-N-01`；取自 `needs.json` 分档配置）。
    pub action_id: String,
    /// 求助气泡池（讨食 → `begFood`；求洗澡 → `begBath`；`None` = 不弹求助气泡）。
    pub bubble_pool: Option<String>,
}

/// 求助气泡池映射（`01 §6.16.3`：讨食 / 求洗澡是仅有的两类主动求助气泡）。
///
/// 口径登记：ACT-N-05（挠痒）在 `actions.json` 声明 `helpRequest.withBubble=true`，
/// 但台词池键已冻结（L-02 `linePools.count=13`），无「挠痒池」→ 本卡不弹求助气泡，
/// 待台词池扩展（S9+）时再补。
fn bubble_pool_of(action_id: &str) -> Option<&'static str> {
    match action_id {
        "ACT-N-01" => Some("begFood"),
        "ACT-N-06" => Some("begBath"),
        _ => None,
    }
}

/// N 系列生存动作触发调度器（S7-M9）。
///
/// 零时钟：`now_ms` 由调用方注入；间隔闸门键为 `"{dim}.{band_id}"`——
/// 档位恶化（如 `peckish` → `hungry`）会换键，视为新档立即可触发（高频口径）。
#[derive(Debug, Clone, Default)]
pub struct NeedsActionTrigger {
    /// 键 `"{dim}.{band_id}"` → 下次允许触发时刻（单调毫秒；缺席 = 立即就绪）。
    next_allowed_ms: BTreeMap<String, i64>,
}

impl NeedsActionTrigger {
    /// 空调度器（全部档位立即可触发）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 分档轮询：返回**到点**应触发的动作意图（含间隔闸门；未到点 / 无动作 → 不产出）。
    ///
    /// 触发后按该档间隔重排下次时刻（`intervalSec` 区间内**确定性**取值，可单测）。
    #[must_use]
    pub fn poll(&mut self, effects: &BandEffects<'_>, now_ms: i64) -> Vec<NeedActionIntent> {
        let mut out = Vec::with_capacity(2);
        let bands: [(&str, &str, Option<&crate::config::model::NeedBandCfg>); 2] = [
            ("satiety", effects.satiety_band.id(), effects.satiety_cfg),
            ("cleanliness", effects.clean_band.id(), effects.clean_cfg),
        ];
        for (dim, band_id, band_cfg) in bands {
            let Some(cfg) = band_cfg else { continue };
            let Some(action_id) = cfg.action.as_deref() else { continue };
            let key = format!("{dim}.{band_id}");
            if now_ms < self.next_allowed_ms.get(&key).copied().unwrap_or(0) {
                continue;
            }
            out.push(NeedActionIntent {
                action_id: action_id.to_string(),
                bubble_pool: bubble_pool_of(action_id).map(str::to_string),
            });
            let interval_ms = match cfg.interval_sec {
                Some([lo, hi]) => pick_interval_ms(lo, hi, &key, now_ms),
                None => (DEFAULT_BAND_INTERVAL_SEC as i64).saturating_mul(1_000),
            };
            self.next_allowed_ms.insert(key, now_ms.saturating_add(interval_ms));
        }
        out
    }

    /// 喂食开始：ACT-N-02（吃饭演出；每次喂食都触发，仲裁器按优先级 / 演出态定夺）。
    #[must_use]
    pub const fn feed_started() -> &'static str {
        FEED_START_ACTION_ID
    }

    /// 喂食完成且饱食度 ≥ 满档阈值 → ACT-N-03（吃饱满足，AC-22）。
    ///
    /// 阈值取 `needs.json.bands.satiety` 首档（`full.min = 70`，配置驱动，C7）；
    /// 配置缺档 → `None`（诚实降级：不编造阈值）。
    #[must_use]
    pub fn feed_completed(satiety: f32, cfg: &NeedsConfig) -> Option<&'static str> {
        let full_min = cfg.bands.satiety.first().map(|b| b.min)?;
        (satiety >= full_min).then_some(FEED_DONE_SATISFIED_ACTION_ID)
    }

    /// 洗澡开始动作（`needs.json.bath.actionId` → ACT-N-07）。
    #[must_use]
    pub fn bath_started(cfg: &NeedsConfig) -> &str {
        &cfg.bath.action_id
    }

    /// 洗澡结束动作（`needs.json.bath.endActionId` → ACT-N-08，AC-22）。
    #[must_use]
    pub fn bath_completed(cfg: &NeedsConfig) -> &str {
        &cfg.bath.end_action_id
    }
}

/// 间隔区间内确定性取值（毫秒；`hi < lo` 时钳为 `lo`，不 panic）。
fn pick_interval_ms(lo_sec: u64, hi_sec: u64, key: &str, now_ms: i64) -> i64 {
    let lo = (lo_sec as i64).saturating_mul(1_000);
    let hi = (hi_sec as i64).saturating_mul(1_000).max(lo);
    let span = (hi - lo + 1) as u64;
    lo + (mix_hash(key, now_ms) % span) as i64
}

/// 确定性终混（FNV-1a 喂键 → SplitMix64 终混；与 `lines`/`speech` 同族，可单测）。
fn mix_hash(key: &str, now_ms: i64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^= now_ms as u64;
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> NeedsConfig {
        NeedsConfig::default()
    }

    #[test]
    fn poll_emits_band_candidate_with_help_bubble() {
        // 有点饿（30）→ ACT-N-01 + 讨食求助气泡（AC-21 触发面）。
        let c = cfg();
        let effects = BandEffects::from_cfg(&c, 30.0, 80.0);
        let mut t = NeedsActionTrigger::new();
        let intents = t.poll(&effects, 0);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].action_id, "ACT-N-01");
        assert_eq!(intents[0].bubble_pool.as_deref(), Some("begFood"));
    }

    #[test]
    fn poll_emits_clean_band_candidate() {
        // 很脏（10）→ ACT-N-06 + 求洗澡气泡（AC-22 触发面）。
        let c = cfg();
        let effects = BandEffects::from_cfg(&c, 80.0, 10.0);
        let mut t = NeedsActionTrigger::new();
        let intents = t.poll(&effects, 0);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].action_id, "ACT-N-06");
        assert_eq!(intents[0].bubble_pool.as_deref(), Some("begBath"));
    }

    #[test]
    fn poll_emits_both_dimensions_when_both_active() {
        // 饥饿 + 很脏 → 讨食 + 求洗澡（两维并集）。
        let c = cfg();
        let effects = BandEffects::from_cfg(&c, 3.0, 10.0);
        let mut t = NeedsActionTrigger::new();
        let intents = t.poll(&effects, 0);
        assert_eq!(intents.len(), 2);
        assert_eq!(intents[0].action_id, "ACT-N-01");
        assert_eq!(intents[1].action_id, "ACT-N-06");
    }

    #[test]
    fn poll_is_interval_gated_and_deterministic() {
        // peckish intervalSec=[180,480]：到点一次，间隔内不再触发。
        let c = cfg();
        let effects = BandEffects::from_cfg(&c, 30.0, 80.0);
        let mut t = NeedsActionTrigger::new();
        let first = t.poll(&effects, 1_000);
        assert_eq!(first.len(), 1);
        let key = "satiety.peckish";
        let next = t.next_allowed_ms.get(key).copied().expect("应已排期");
        assert!(next > 1_000 + 180_000 - 1 && next <= 1_000 + 480_000, "间隔 ∈ [180,480]s：{next}");
        assert!(t.poll(&effects, next - 1).is_empty(), "间隔内不得再触发");
        assert_eq!(t.poll(&effects, next).len(), 1, "到点重新触发");

        // 确定性：同键同刻 → 同间隔。
        let mut t2 = NeedsActionTrigger::new();
        let _ = t2.poll(&effects, 1_000);
        assert_eq!(t2.next_allowed_ms.get(key), Some(&next));
    }

    #[test]
    fn poll_default_interval_for_high_frequency_bands() {
        // hungry 无 intervalSec → 默认 60s。
        let c = cfg();
        let effects = BandEffects::from_cfg(&c, 10.0, 80.0);
        let mut t = NeedsActionTrigger::new();
        let _ = t.poll(&effects, 0);
        assert_eq!(t.next_allowed_ms.get("satiety.hungry"), Some(&60_000));
    }

    #[test]
    fn poll_skips_bands_without_action() {
        // 满饱 + 清爽：无动作候选 → 不产出。
        let c = cfg();
        let effects = BandEffects::from_cfg(&c, 80.0, 80.0);
        let mut t = NeedsActionTrigger::new();
        assert!(t.poll(&effects, 0).is_empty());
    }

    #[test]
    fn poll_band_worsening_resets_interval() {
        // peckish（有间隔）→ hungry（换键 = 新档立即可触发，高频口径）。
        let c = cfg();
        let mut t = NeedsActionTrigger::new();
        let peckish = BandEffects::from_cfg(&c, 30.0, 80.0);
        let _ = t.poll(&peckish, 0);
        let hungry = BandEffects::from_cfg(&c, 10.0, 80.0);
        let intents = t.poll(&hungry, 1_000);
        assert_eq!(intents.len(), 1, "档位恶化应立即触发（换键重置间隔）");
        assert_eq!(intents[0].action_id, "ACT-N-01");
    }

    #[test]
    fn bubble_pool_mapping_covers_help_actions_only() {
        assert_eq!(bubble_pool_of("ACT-N-01"), Some("begFood"));
        assert_eq!(bubble_pool_of("ACT-N-06"), Some("begBath"));
        assert_eq!(bubble_pool_of("ACT-N-04"), None);
        assert_eq!(bubble_pool_of("ACT-N-05"), None, "挠痒无专用台词池（口径登记）");
        assert_eq!(bubble_pool_of("ACT-N-02"), None);
    }

    #[test]
    fn feed_transaction_endpoints_follow_spec() {
        assert_eq!(NeedsActionTrigger::feed_started(), "ACT-N-02");
        let c = cfg();
        // 满档阈值 70：≥70 → 吃饱满足；<70 → 不触发（AC-22）。
        assert_eq!(NeedsActionTrigger::feed_completed(70.0, &c), Some("ACT-N-03"));
        assert_eq!(NeedsActionTrigger::feed_completed(69.9, &c), None);
        assert_eq!(NeedsActionTrigger::feed_completed(100.0, &c), Some("ACT-N-03"));
    }

    #[test]
    fn bath_transaction_endpoints_follow_config() {
        let c = cfg();
        assert_eq!(NeedsActionTrigger::bath_started(&c), "ACT-N-07");
        assert_eq!(NeedsActionTrigger::bath_completed(&c), "ACT-N-08");
        // C7：端点动作取自配置，改配置即改行为。
        let mut c2 = c.clone();
        c2.bath.action_id = "ACT-N-99".to_string();
        c2.bath.end_action_id = "ACT-N-98".to_string();
        assert_eq!(NeedsActionTrigger::bath_started(&c2), "ACT-N-99");
        assert_eq!(NeedsActionTrigger::bath_completed(&c2), "ACT-N-98");
    }

    #[test]
    fn feed_completed_degrades_gracefully_without_full_band() {
        let mut c = cfg();
        c.bands.satiety.clear();
        assert_eq!(NeedsActionTrigger::feed_completed(80.0, &c), None, "配置缺满档 → 不编造阈值");
    }

    #[test]
    fn pick_interval_clamps_and_is_in_range() {
        assert!(pick_interval_ms(180, 480, "k", 7) >= 180_000);
        assert!(pick_interval_ms(180, 480, "k", 7) <= 480_000);
        // hi < lo → 钳为 lo（不 panic）。
        assert_eq!(pick_interval_ms(300, 100, "k", 7), 300_000);
    }
}
