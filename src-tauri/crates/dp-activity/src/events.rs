//! 活动随机事件抽取（S8-M2，T-21 段 · 2/4；`02 §5.13` dp-activity/src/events.rs）。
//!
//! 职责：按实例 `seed` **确定性**抽取打工随机事件（`WACT-E-*`，`01 §6.13.3`）与
//! 旅游天气表（`TACT-E-*`，`01 §6.13.5`）。同一实例（同 seed / 同 def_id / 同
//! 时刻）在任何机器 / 任何次数下抽取结果一致——离线回归补抽时追记到
//! `ActivityInstance::rolled_events`，保证不重复计。
//!
//! 抽取口径（`02 §5.14`：随机事件表按 seed 确定性抽取）：
//!   - 事件表项带 `weight`；命中权重总和为分母做加权抽样；
//!   - 条件表达式（`mood>=70` 等）不满足的项**移出候选**（权重不参与分母）；
//!   - 权重全零 / 空表 / 全不满足 → 返回 `None`（诚实降级：无事件）。

use crate::model::{ActivityEventRoll, ActivityKind};
use dp_core::config::model::{ActivityEventCfg, EventEffectsCfg};

/// 一次抽取结果（确定性）。
#[derive(Clone, Debug, PartialEq)]
pub struct EventRoll {
    /// 命中事件配置。
    pub event: ActivityEventCfg,
    /// 抽取时刻（UTC 毫秒；记录用）。
    pub at_ms: i64,
}

/// 按 seed 确定性抽取一次随机事件。
///
/// `events` 为事件表（打工 `events` / 旅游 `weather_table`）；`condition_hit`
/// 回调评估条件表达式是否满足（`SettleInputs` 已注入，见 [`crate::settle`]）。
///
/// 确定性算法（与 `dp-core` 的 `mix_hash` 同族）：seed 与事件表项 `(id, weight)`
/// 及 `at_ms` 混合后取模——同输入恒同输出，可单测锁定。
#[must_use]
pub fn roll_event(
    kind: ActivityKind,
    seed: u64,
    events: &[ActivityEventCfg],
    at_ms: i64,
    condition_hit: &dyn Fn(&str) -> bool,
) -> Option<EventRoll> {
    if events.is_empty() {
        return None;
    }
    // 候选：条件满足（空条件恒满足）且 weight > 0。
    let mut total: u64 = 0;
    let mut candidates: Vec<&ActivityEventCfg> = Vec::new();
    for ev in events {
        if ev.weight == 0 {
            continue;
        }
        if !ev.condition.is_empty() && !condition_hit(&ev.condition) {
            continue;
        }
        total = total.saturating_add(u64::from(ev.weight));
        candidates.push(ev);
    }
    if total == 0 {
        return None;
    }
    let pick = mix_hash(kind, seed, at_ms) % total;
    let mut acc: u64 = 0;
    for ev in &candidates {
        acc = acc.saturating_add(u64::from(ev.weight));
        if pick < acc {
            return Some(EventRoll { event: (*ev).clone(), at_ms });
        }
    }
    // 浮点边界防御：命中最后一项。
    candidates.last().copied().map(|event| EventRoll { event: event.clone(), at_ms })
}

/// 打工事件效果空壳（效果应用在 [`crate::settle`] 内完成）。
#[must_use]
pub fn is_positive(effects: &EventEffectsCfg) -> bool {
    effects.reward_multiplier.unwrap_or(1.0) >= 1.0
        || effects.mood.unwrap_or(0.0) > 0.0
        || effects.skill_points.unwrap_or(0) > 0
}

/// 确定性终混（FNV-1a 喂 kind/seed/at_ms → SplitMix64 终混；与 `dp-core`
/// `mix_hash` 同族，可单测锁定）。
fn mix_hash(kind: ActivityKind, seed: u64, at_ms: i64) -> u64 {
    let kind_tag = match kind {
        ActivityKind::Work => 0x57_u64,
        ActivityKind::Study => 0x53_u64,
        ActivityKind::Travel => 0x54_u64,
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in [kind_tag, seed, at_ms as u64] {
        h ^= b;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    h
}

/// 便捷：把一次抽取结果转成持久化记录（`rolled_events` 追记用）。
#[must_use]
pub fn to_roll(roll: &EventRoll) -> ActivityEventRoll {
    ActivityEventRoll {
        event_id: roll.event.id.clone(),
        at_ms: roll.at_ms,
        weight: roll.event.weight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dp_core::config::model::{ActivityEventCfg, EventEffectsCfg};

    fn ev(id: &str, weight: u32, condition: &str) -> ActivityEventCfg {
        ActivityEventCfg {
            id: id.to_string(),
            weight,
            condition: condition.to_string(),
            effects: EventEffectsCfg::default(),
            line_key: None,
        }
    }

    fn always_true(_: &str) -> bool {
        true
    }

    fn always_false(_: &str) -> bool {
        false
    }

    #[test]
    fn empty_table_returns_none() {
        let r = roll_event(ActivityKind::Work, 1, &[], 0, &always_true);
        assert!(r.is_none());
    }

    #[test]
    fn zero_weight_table_returns_none() {
        let events = [ev("W-0", 0, "")];
        let r = roll_event(ActivityKind::Work, 1, &events, 0, &always_true);
        assert!(r.is_none());
    }

    #[test]
    fn condition_filters_candidates() {
        let events = [ev("A", 10, "mood>=70"), ev("B", 10, "")];
        // 条件不满足 → 只有 B 可选。
        let r = roll_event(ActivityKind::Work, 42, &events, 0, &always_false);
        let roll = r.expect("应有结果");
        assert_eq!(roll.event.id, "B");
    }

    #[test]
    fn deterministic_same_input_same_output() {
        let events = [ev("A", 3, ""), ev("B", 5, ""), ev("C", 2, "")];
        let a = roll_event(ActivityKind::Work, 1234, &events, 99_000, &always_true);
        let b = roll_event(ActivityKind::Work, 1234, &events, 99_000, &always_true);
        assert_eq!(a, b, "同输入必须同输出（确定性）");
        let c = roll_event(ActivityKind::Work, 1235, &events, 99_000, &always_true);
        assert_ne!(a.unwrap().event.id, c.unwrap().event.id, "不同 seed 通常不同命中（概率上可断言于固定输入）");
    }

    #[test]
    fn weighted_sampling_respects_weights() {
        // A 权重 90 / B 权重 10：抽样 200 次，A 占比应显著高于 B（确定性种子遍历）。
        let events = [ev("A", 90, ""), ev("B", 10, "")];
        let mut a_count = 0u32;
        for seed in 0..200u64 {
            let roll = roll_event(ActivityKind::Travel, seed, &events, 0, &always_true)
                .expect("应命中");
            if roll.event.id == "A" {
                a_count += 1;
            }
        }
        assert!(a_count > 120, "A 应占多数（90/100 权重）：{a_count}/200");
    }

    #[test]
    fn to_roll_records_provenance() {
        let events = [ev("TACT-E-01", 55, "")];
        let r = roll_event(ActivityKind::Travel, 7, &events, 5_000, &always_true).expect("应命中");
        let record = to_roll(&r);
        assert_eq!(record.event_id, "TACT-E-01");
        assert_eq!(record.at_ms, 5_000);
        assert_eq!(record.weight, 55);
    }
}
