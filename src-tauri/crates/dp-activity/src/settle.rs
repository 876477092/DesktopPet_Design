//! 活动收益结算（S8-M2，T-21 段 · 2/4；`02 §5.14` / §5.15 / §5.16）。
//!
//! 结算流水（`02 §5.14`）：**基础收益 → 修正乘子 → 随机事件 → 召回比例**。
//!
//! - **打工**：基础 = 时薪 × 实际时长（`ratio × planned`）；修正乘子逐条评估
//!   （`mood>=80 ×1.15` / `cleanliness<15 ×0.6` / `timeSegment==morning ×1.2` /
//!   `satiety<20 ×0.7`，RV-01 解除死亡负循环）；随机事件乘子（`WACT-E-01 ×1.3` /
//!   `WACT-E-02 ×0.0`）；召回比例 ×`recall_penalty.rewardRatio`（0.5）。
//! - **学习**：技能点 = `⌊时长/30 × (0.85+0.3d) × (Mood≥70 ? 1.1 : 0.9)⌋`，最低 1
//!   （`02 §5.15`；`d` = 勤奋 0~100 归一化到 [0,1]，默认 60 → ×1.03）；
//! - **旅游**：Mood +`rewards.mood`、亲密度 +`rewards.affinityExp`、清洁度
//!   −`rewards.cleanliness`、产出照片 / 纪念品道具；天气事件乘子作用于 Mood。
//!
//! 口径登记：
//!   - **经济入账归 S8-M5**：心币 / 道具只**计算**并随 [`ActivityReward`] 输出，
//!     不入账本（dp-app 在 S8-M5 前只展示）；
//!   - **召回惩罚**（Mood−4 / P+6 / rough+0.15）拆成两部分：Mood 惩罚并入
//!     `reward.mood_delta`；P / rough 经 `neglect_delta` / `rough_delta` 字段输出，
//!     由 dp-app 应用到情绪内核（P/rough 属 `EmotionEngine` 状态，不在本模块）。
//!   - 勤奋 `d` 归一化：`SettleInputs.diligence` 为 0~100（与七因子同口径），
//!     结算时 `d = diligence / 100`——默认 60 → 0.85+0.3×0.6 = 1.03。

use crate::model::{ActivityKind, ActivityReward, RecallKind, SettleInputs};
use dp_core::config::model::{CourseCfg, JobCfg, TripCfg};

/// 学习技能点公式常数（`02 §5.15` / §5.16 `pointFormula` 的唯一真源说明）。
/// 0.85 基数 + 0.3×d 勤奋加成；Mood≥70 ×1.1，否则 ×0.9；每 30 分钟一段。
const STUDY_BASE_MUL: f32 = 0.85;
const STUDY_DILIGENCE_MUL: f32 = 0.3;
const STUDY_MOOD_HIGH_MUL: f32 = 1.1;
const STUDY_MOOD_LOW_MUL: f32 = 0.9;
const STUDY_DURATION_STEP_MIN: u32 = 30;

/// 打工收益常数（`01 §6.13.3`：时薪 × 分钟）。
pub const JOB_WAGE_BASE: f32 = 1.0;

/// 统一结算入口：按活动类别分派到打工 / 学习 / 旅游结算。
///
/// `def` 为三类配置之一（`JobCfg` / `CourseCfg` / `TripCfg`）；`ratio` 为已完成
/// 比例（正常 1.0；提前召回 = `elapsed/planned` 截断 [0,1]）；`kind` 为召回类别。
///
/// 参数较多（8）系**分派入口显式化**：`job/course/trip` 三配置与 `inputs/ratio/
/// recall_kind` 均由 `ActivityRuntime::confirm_reported` 单点组装，打包结构体
/// 会引入无收益中间层；以 `#[allow]` 保留可读分派签名。
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn settle(
    kind: ActivityKind,
    job: Option<&JobCfg>,
    course: Option<&CourseCfg>,
    trip: Option<&TripCfg>,
    duration_min: u32,
    inputs: &SettleInputs,
    ratio: f32,
    recall_kind: RecallKind,
) -> ActivityReward {
    let ratio = ratio.clamp(0.0, 1.0);
    match kind {
        ActivityKind::Work => {
            let Some(job) = job else {
                return ActivityReward::default();
            };
            settle_work(job, duration_min, inputs, ratio, recall_kind)
        }
        ActivityKind::Study => {
            let Some(course) = course else {
                return ActivityReward::default();
            };
            settle_study(course, duration_min, inputs, ratio, recall_kind)
        }
        ActivityKind::Travel => {
            let Some(trip) = trip else {
                return ActivityReward::default();
            };
            settle_travel(trip, inputs, ratio, recall_kind)
        }
    }
}

/// 打工结算（`01 §6.13.3` / `02 §5.16` jobs[]）。
fn settle_work(
    job: &JobCfg,
    duration_min: u32,
    inputs: &SettleInputs,
    ratio: f32,
    recall_kind: RecallKind,
) -> ActivityReward {
    let effective_min = (duration_min as f32) * ratio;
    // 基础收益 = 时薪 × 实际时长。
    let base = job.wage_per_minute * effective_min;
    // 修正乘子（modifiers 逐条评估；未知条件跳过）。
    let mut mul = 1.0f32;
    for m in &job.modifiers {
        if eval_when(&m.when, inputs) {
            mul *= m.reward_multiplier;
        }
    }
    // 随机事件（seed 确定性；条件由 inputs 评估）。
    let mut mood_delta: f32 = 0.0;
    if let Some(roll) = crate::events::roll_event(
        ActivityKind::Work,
        inputs.seed,
        &job.events,
        inputs.now_ms,
        &|cond| eval_when(cond, inputs),
    ) {
        if let Some(ev_mul) = roll.event.effects.reward_multiplier {
            mul *= ev_mul;
        }
        mood_delta += roll.event.effects.mood.unwrap_or(0.0);
    }
    // 召回比例（0.5；recall_penalty.rewardRatio 由调用方传入 inputs？——不，
    // 直接按 RecallKind 语义：Early → ×0.5，Abnormal → 按已完成比例保底不额外折）。
    let recall_ratio = match recall_kind {
        RecallKind::Normal => 1.0,
        RecallKind::Early => 0.5,
        RecallKind::Abnormal => 1.0, // 保底结算不再额外打折（已完成比例已折）
    };
    // 经济倍率（S8-M5 起消费；默认 standard 1.0）。
    let scale = if inputs.economy_scale > 0.0 { inputs.economy_scale } else { 1.0 };
    let coin = (base * mul * recall_ratio * scale).round().max(0.0) as i64;
    // 打工 Mood 基础 −2~+5（确定性：seed 映射到 [-2, +5] 闭区间）。
    mood_delta += work_mood_jitter(inputs.seed);
    let (neglect_delta, rough_delta) = recall_penalties(inputs, recall_kind);
    ActivityReward {
        coin,
        skill_points: 0,
        mood_delta,
        affinity_exp: 0.0,
        cleanliness_delta: 0.0,
        energy_delta: 0.0,
        item_ids: Vec::new(),
        deferred: false,
        kind: recall_kind,
        neglect_delta,
        rough_delta,
    }
}

/// 学习结算（`02 §5.15`：技能点公式 + 最低 1）。
fn settle_study(
    course: &CourseCfg,
    duration_min: u32,
    inputs: &SettleInputs,
    ratio: f32,
    recall_kind: RecallKind,
) -> ActivityReward {
    let d = (inputs.diligence / 100.0).clamp(0.0, 1.0);
    let mood_mul = if inputs.mood >= 70.0 { STUDY_MOOD_HIGH_MUL } else { STUDY_MOOD_LOW_MUL };
    let steps = (duration_min as f32 * ratio / STUDY_DURATION_STEP_MIN as f32).max(0.0);
    let points = (steps * (STUDY_BASE_MUL + STUDY_DILIGENCE_MUL * d) * mood_mul).floor() as u32;
    let points = points.max(course.min_points.max(1)).min(duration_min); // 上限 = 时长分钟（防御）
    let (neglect_delta, rough_delta) = recall_penalties(inputs, recall_kind);
    ActivityReward {
        coin: 0,
        skill_points: points,
        mood_delta: 0.0,
        affinity_exp: 0.0,
        cleanliness_delta: 0.0,
        energy_delta: 0.0,
        item_ids: Vec::new(),
        deferred: false,
        kind: recall_kind,
        neglect_delta,
        rough_delta,
    }
}

/// 旅游结算（`01 §6.13.5` / `02 §5.16` trips[]）。
fn settle_travel(
    trip: &TripCfg,
    inputs: &SettleInputs,
    ratio: f32,
    recall_kind: RecallKind,
) -> ActivityReward {
    // 天气 / 随机事件（seed 确定性；rewardMultiplier 作用于 Mood 收益）。
    let mut mood_mul = 1.0f32;
    let mut extra_mood: f32 = 0.0;
    if let Some(roll) = crate::events::roll_event(
        ActivityKind::Travel,
        inputs.seed,
        &trip.weather_table,
        inputs.now_ms,
        &|cond| eval_when(cond, inputs),
    ) {
        if let Some(ev_mul) = roll.event.effects.reward_multiplier {
            mood_mul *= ev_mul;
        }
        extra_mood += roll.event.effects.mood.unwrap_or(0.0);
    }
    let recall_ratio = match recall_kind {
        RecallKind::Normal => 1.0,
        RecallKind::Early => 0.5,
        RecallKind::Abnormal => 1.0,
    };
    let mood_delta = (trip.rewards.mood * mood_mul + extra_mood) * ratio * recall_ratio;
    let affinity_exp = trip.rewards.affinity_exp * ratio * recall_ratio;
    let cleanliness_delta = trip.rewards.cleanliness * ratio * recall_ratio;
    let mut items = Vec::new();
    if ratio > 0.0 {
        items.push(trip.rewards.photo.clone());
        items.push(trip.rewards.souvenir.clone());
    }
    let (neglect_delta, rough_delta) = recall_penalties(inputs, recall_kind);
    ActivityReward {
        coin: 0,
        skill_points: 0,
        mood_delta,
        affinity_exp,
        cleanliness_delta,
        energy_delta: 0.0,
        item_ids: items,
        deferred: false,
        kind: recall_kind,
        neglect_delta,
        rough_delta,
    }
}

/// 召回惩罚（Mood−4 并入 mood_delta 由各结算函数处理；此处输出 P / rough 增量）。
fn recall_penalties(inputs: &SettleInputs, recall_kind: RecallKind) -> (f32, f32) {
    match recall_kind {
        RecallKind::Early => (inputs.recall_neglect_add, inputs.recall_rough_step),
        _ => (inputs.regress_neglect_delta, 0.0),
    }
}

/// 打工 Mood 抖动：seed 确定性映射到 [-2, +5]（`02 §5.15`：打工 Mood−2~+5）。
fn work_mood_jitter(seed: u64) -> f32 {
    let h = mix_u64(seed);
    // [-2, +5] 共 8 个整值。
    (h % 8) as f32 - 2.0
}

/// 条件表达式求值（`02 §5.16` modifiers[].when / events[].condition）。
///
/// 支持：`mood>=80` / `cleanliness<15` / `satiety<20` / `timeSegment==morning` /
/// `mood>=70`；未知表达式 → 保守 false（不误加成，防御性收口）。
#[must_use]
pub fn eval_when(cond: &str, inputs: &SettleInputs) -> bool {
    let c = cond.trim();
    if c.is_empty() {
        return true;
    }
    let (lhs, op, rhs) = match split_cond(c) {
        Some(t) => t,
        None => return false,
    };
    let val = match lhs {
        "mood" => Some(inputs.mood),
        "cleanliness" => Some(inputs.cleanliness),
        "satiety" => Some(inputs.satiety),
        "timeSegment" => None, // 字符串比较走独立分支
        _ => None,
    };
    match (lhs, op, rhs) {
        ("timeSegment", "==", seg) => inputs.time_segment == seg,
        ("timeSegment", "!=", seg) => inputs.time_segment != seg,
        _ => {
            let Some(v) = val else { return false };
            let Some(rhs_num) = rhs.parse::<f32>().ok() else { return false };
            match op {
                ">=" => v >= rhs_num,
                "<=" => v <= rhs_num,
                ">" => v > rhs_num,
                "<" => v < rhs_num,
                "==" => (v - rhs_num).abs() < f32::EPSILON,
                "!=" => (v - rhs_num).abs() >= f32::EPSILON,
                _ => false,
            }
        }
    }
}

/// 拆条件表达式为 (左值, 操作符, 右值)。
fn split_cond(cond: &str) -> Option<(&str, &str, &str)> {
    for op in ["<=", ">=", "==", "!=", "<", ">"] {
        if let Some(pos) = cond.find(op) {
            let lhs = cond[..pos].trim();
            let rhs = cond[pos + op.len()..].trim();
            if lhs.is_empty() || rhs.is_empty() {
                return None;
            }
            return Some((lhs, op, rhs));
        }
    }
    None
}

/// 简单确定性终混（打工抖动用；FNV-1a 喂 seed → SplitMix64）。
fn mix_u64(seed: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ seed;
    h = h.wrapping_mul(0x0000_0100_0000_01b3);
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

    fn inputs(mood: f32, cleanliness: f32, satiety: f32) -> SettleInputs {
        SettleInputs {
            mood,
            cleanliness,
            satiety,
            diligence: 60.0,
            economy_scale: 1.0,
            time_segment: "afternoon",
            present: true,
            local_hour: 14,
            seed: 42,
            now_ms: 1_000,
            recall_neglect_add: 6.0,
            recall_rough_step: 0.15,
            regress_neglect_delta: -20.0,
        }
    }

    fn job() -> JobCfg {
        serde_json::from_str(
            r#"{
                "id": "W-01", "name": "咖啡店店员", "icon": "", "wagePerMinute": 1.2,
                "durationOptions": [15, 30, 60],
                "unlock": { "affinityLevel": 0, "skills": {} },
                "cost": { "energy": 12, "cleanliness": 12 },
                "modifiers": [
                    { "when": "mood>=80", "rewardMultiplier": 1.15 },
                    { "when": "cleanliness<15", "rewardMultiplier": 0.6 },
                    { "when": "timeSegment==morning", "rewardMultiplier": 1.2 },
                    { "when": "satiety<20", "rewardMultiplier": 0.7 }
                ],
                "events": [],
                "actionIds": { "depart": "ACT-N-09", "return": "ACT-N-10", "deskLoop": "", "postcard": "" },
                "sound": { "depart": "a", "return": "b" }
            }"#,
        )
        .expect("job cfg 应可反序列化")
    }

    #[test]
    fn work_basic_coin_is_wage_times_minutes() {
        let inp = inputs(50.0, 80.0, 70.0);
        // 30 分钟 × 1.2 = 36；无修正乘子命中（mood 50 <80、clean 80 ≥15、afternoon 非 morning、satiety 70 ≥20）。
        let r = settle(ActivityKind::Work, Some(&job()), None, None, 30, &inp, 1.0, RecallKind::Normal);
        assert_eq!(r.coin, 36, "基础收益 = 时薪 × 时长");
    }

    #[test]
    fn work_mood_bonus_modifier_applies() {
        let inp = inputs(85.0, 80.0, 70.0);
        // 36 × 1.15 = 41.4 → round 41。
        let r = settle(ActivityKind::Work, Some(&job()), None, None, 30, &inp, 1.0, RecallKind::Normal);
        assert_eq!(r.coin, 41, "mood≥80 ×1.15 应放大收益");
    }

    #[test]
    fn early_recall_halves_reward() {
        let inp = inputs(50.0, 80.0, 70.0);
        let normal = settle(ActivityKind::Work, Some(&job()), None, None, 30, &inp, 1.0, RecallKind::Normal);
        // 提前召回：已完成比例 0.5 × 召回惩罚 0.5 → 36×0.25 = 9（PRD：按已完成时长比例 × 50%）。
        let early = settle(ActivityKind::Work, Some(&job()), None, None, 30, &inp, 0.5, RecallKind::Early);
        assert_eq!(normal.coin, 36);
        assert_eq!(early.coin, 9, "已完成 50% 再按 50% 结算");
        assert_eq!(early.neglect_delta, 6.0, "提前召回 P+6");
        assert!((early.rough_delta - 0.15).abs() < 1e-6, "提前召回 rough+0.15");
        assert_eq!(normal.neglect_delta, -20.0, "正常回归 P−20");
    }

    #[test]
    fn study_points_follow_formula_floor_and_min_one() {
        // 30 分钟、diligence 60、mood 50 → d=0.6、mood_mul=0.9 → 1×(0.85+0.18)×0.9
        // = 0.927 → floor 0 → 最低 1。
        let inp = inputs(50.0, 80.0, 70.0);
        let course: CourseCfg = serde_json::from_str(
            r#"{
                "id": "CRS-01", "name": "礼仪课", "tuition": 30, "durationOptions": [30, 60],
                "skillType": "etiquette", "cost": { "energy": 8, "cleanliness": 2 },
                "pointFormula": "floor(durationMin/30 * (0.85+0.3*diligence) * (mood>=70 ? 1.1 : 0.9))",
                "minPoints": 1, "levelUpCost": "10*level", "requireItem": "book_etiquette",
                "actionIds": { "depart": "ACT-N-09", "return": "", "deskLoop": "ACT-N-11", "postcard": "" },
                "sound": { "depart": null, "return": null }
            }"#,
        )
        .expect("course cfg 应可反序列化");
        let r = settle(ActivityKind::Study, None, Some(&course), None, 30, &inp, 1.0, RecallKind::Normal);
        assert_eq!(r.skill_points, 1, "最低 1");
        // 60 分钟 → 2×0.927 = 1.854 → floor 1。
        let r60 = settle(ActivityKind::Study, None, Some(&course), None, 60, &inp, 1.0, RecallKind::Normal);
        assert_eq!(r60.skill_points, 1);
        // Mood≥70 → ×1.1：60 分钟 → 2×1.03×1.1 = 2.266 → floor 2。
        let high = inputs(75.0, 80.0, 70.0);
        let r_high = settle(ActivityKind::Study, None, Some(&course), None, 60, &high, 1.0, RecallKind::Normal);
        assert_eq!(r_high.skill_points, 2);
        // 勤奋 100 → d=1 → 0.85+0.3 = 1.15：30 分钟、mood 50 → 1.15×0.9 = 1.035 → floor 1。
        let diligent = SettleInputs { diligence: 100.0, ..inp };
        let r_d = settle(ActivityKind::Study, None, Some(&course), None, 30, &diligent, 1.0, RecallKind::Normal);
        assert_eq!(r_d.skill_points, 1);
    }

    #[test]
    fn travel_rewards_follow_trip_cfg() {
        let inp = inputs(50.0, 80.0, 70.0);
        let trip: TripCfg = serde_json::from_str(
            r#"{
                "id": "TR-01", "name": "海边", "durationMin": 120,
                "cost": { "coin": 120, "ticketItem": "ticket_travel" },
                "unlock": { "affinityLevel": 0, "skills": {} },
                "postcardIntervalMin": 40,
                "rewards": { "photo": "photo_sea_01", "souvenir": "item_shell", "mood": 25,
                             "affinityExp": 40, "cleanliness": -15 },
                "weatherTable": [],
                "actionIds": { "depart": "ACT-N-12", "return": "ACT-N-14", "deskLoop": "", "postcard": "ACT-N-13" },
                "sound": { "depart": "a", "return": "b" }
            }"#,
        )
        .expect("trip cfg 应可反序列化");
        let r = settle(ActivityKind::Travel, None, None, Some(&trip), 120, &inp, 1.0, RecallKind::Normal);
        assert_eq!(r.mood_delta, 25.0);
        assert_eq!(r.affinity_exp, 40.0);
        assert_eq!(r.cleanliness_delta, -15.0);
        assert_eq!(r.item_ids, vec!["photo_sea_01".to_string(), "item_shell".to_string()]);
        // 提前召回：已完成比例 0.5 × 召回惩罚 0.5 → Mood 25×0.25 = 6.25、Affinity 10、
        // Cleanliness -3.75、道具仍产出。
        let early = settle(ActivityKind::Travel, None, None, Some(&trip), 120, &inp, 0.5, RecallKind::Early);
        assert!((early.mood_delta - 6.25).abs() < 1e-6);
        assert!((early.affinity_exp - 10.0).abs() < 1e-6);
        assert!((early.cleanliness_delta + 3.75).abs() < 1e-6);
        assert_eq!(early.item_ids.len(), 2, "道具按比例产出（>0）");
    }

    #[test]
    fn eval_when_supports_all_operators() {
        let inp = inputs(80.0, 10.0, 15.0);
        assert!(eval_when("mood>=80", &inp));
        assert!(!eval_when("mood>80", &inp));
        assert!(eval_when("cleanliness<15", &inp));
        assert!(eval_when("cleanliness<=10", &inp), "cleanliness=10 满足 <=10");
        assert!(eval_when("satiety<20", &inp));
        assert!(eval_when("timeSegment==afternoon", &inp));
        assert!(!eval_when("timeSegment==morning", &inp));
        assert!(!eval_when("bogus>=1", &inp), "未知维度保守 false");
        assert!(!eval_when("mood>=", &inp), "残缺表达式 false");
    }

    #[test]
    fn work_mood_jitter_in_range() {
        for seed in 0..64u64 {
            let j = work_mood_jitter(seed);
            assert!((-2.0..=5.0).contains(&j), "Mood 抖动应在 [-2, +5]：{j}");
        }
    }
}
