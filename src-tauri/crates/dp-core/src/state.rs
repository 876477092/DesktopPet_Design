//! `state`：宠物可存档状态容器（`02 §4.1` `PetState` / §4.4 `PetSnapshotV2` 的 Rust 侧来源）。
//!
//! 边界（S4-M1 卡面「单次会话边界：只做数值与状态机骨架」）：
//!   - 本模块**只承载数据结构与纯数据操作**（构造 / 派生展示量 / 快照投影 / 暂停窗口记账），
//!     不含任何 tick 编排、因子求解与阶段结算逻辑（归 `emotion::engine`）。
//!   - 时间纪律（C3）：本模块**零时钟**，一切时间由调用方注入 `now_ms: i64`
//!     （相对 / 绝对口径均由调用方决定，见 [`PauseWindow`]）。
//!   - 不引入七因子（归 S7-M4）；不新增 `pet://` 事件（C8）。
//!
//! 暂停数据模型（台账行 1534 P2-2 裁定）：**「起止时间戳入状态，S5-M2 只读」**——
//! 因此暂停窗口以 `start_ms` / `end_ms` 形式**落在状态里**，本模块提供只读查询口，
//! 不做任何 UI 联动（「恢复后提示『你刚才不在呀』」归 S4-M5 台词层）。

use serde::{Deserialize, Serialize};

/// Boredom 展示映射分母（`01 §6.11` / RV-02 冻结口径：`min(100, P×100/120)`）。
///
/// 单一真源 = P，本常量只是展示换算的分母；分母随 `emotion.json.thresholds.l5`
/// 变化时由 [`PetValues::boredom_display`] 的调用方传入，不在此硬编码业务阈值。
pub const BOREDOM_DISPLAY_DIVISOR: f32 = 120.0;
/// Boredom 展示上限（与 `boredom.formula` 的 `min(100, …)` 一致）。
pub const BOREDOM_DISPLAY_MAX: f32 = 100.0;

/// 六维数值当前值（`01 §6.5.1`：Mood / Energy / Affinity / Boredom / Satiety / Cleanliness）。
///
/// **Boredom 不在此结构体存字段** —— 它是 `neglect.p` 的派生展示量（RV-02 / B-02 冻结），
/// 存字段会造成双真源；展示口径见 [`PetValues::boredom_display`]。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PetValues {
    /// 心情 0~100。
    pub mood: f32,
    /// 精力 0~100。
    pub energy: f32,
    /// 亲密度等级 Lv1~10。
    pub affinity_level: u32,
    /// 亲密度经验（当前级内累计）。
    pub affinity_exp: f32,
    /// 饱食度 0~100。
    pub satiety: f32,
    /// 清洁度 0~100。
    pub cleanliness: f32,
}

impl Default for PetValues {
    /// 默认值与 `emotion.json` / `needs.json` 的 `dimensions.*.default` 一致。
    fn default() -> Self {
        Self {
            mood: 60.0,
            energy: 100.0,
            affinity_level: 1,
            affinity_exp: 0.0,
            satiety: 70.0,
            cleanliness: 85.0,
        }
    }
}

impl PetValues {
    /// 由配置默认值构造（C7：数值外置，禁止在逻辑侧写字面量）。
    pub fn from_cfg(emotion: &crate::config::model::EmotionConfig, needs: &crate::config::model::NeedsConfig) -> Self {
        Self {
            mood: emotion.dimensions.mood.default,
            energy: emotion.dimensions.energy.default,
            affinity_level: emotion.dimensions.affinity.min_level,
            affinity_exp: emotion.dimensions.affinity.default_exp,
            satiety: needs.dimensions.satiety.default,
            cleanliness: needs.dimensions.cleanliness.default,
        }
    }

    /// 各维 clamp 到配置区间（越界保护；存档读入 / 事件扣减后调用）。
    pub fn clamp_to_cfg(
        &mut self,
        emotion: &crate::config::model::EmotionConfig,
        needs: &crate::config::model::NeedsConfig,
    ) {
        let d = &emotion.dimensions;
        self.mood = self.mood.clamp(d.mood.min, d.mood.max);
        self.energy = self.energy.clamp(d.energy.min, d.energy.max);
        self.affinity_level = self.affinity_level.clamp(d.affinity.min_level, d.affinity.max_level);
        self.affinity_exp = self.affinity_exp.max(0.0);
        let dn = &needs.dimensions;
        self.satiety = self.satiety.clamp(dn.satiety.min, dn.satiety.max);
        self.cleanliness = self.cleanliness.clamp(dn.cleanliness.min, dn.cleanliness.max);
    }

    /// Boredom 派生展示量：`min(100, P × 100 / divisor)`（`01 §6.11`，RV-02 冻结公式）。
    ///
    /// `divisor` 取 `emotion.json.thresholds.l5`（默认 120）。公式写在
    /// `emotion.json.dimensions.boredom.formula` 中作为唯一真源说明，此处为其实装。
    #[inline]
    pub fn boredom_display(p: f32, divisor: f32) -> f32 {
        let d = if divisor > 0.0 { divisor } else { BOREDOM_DISPLAY_DIVISOR };
        (p * BOREDOM_DISPLAY_MAX / d).clamp(0.0, BOREDOM_DISPLAY_MAX)
    }

    /// 亲和度经验推进（v1 骨架：公式 `100 × level`，`emotion.json.dimensions.affinity.expFormula`）。
    ///
    /// 返回提升的等级数（0 表示未升级）。S4-M1 只提供数据操作，触发时机归 S4-M2/S4-M5。
    pub fn add_affinity_exp(&mut self, exp: f32, emotion: &crate::config::model::EmotionConfig) -> u32 {
        if exp <= 0.0 {
            return 0;
        }
        let max_lv = emotion.dimensions.affinity.max_level;
        let mut gained = 0u32;
        self.affinity_exp += exp;
        while self.affinity_level < max_lv {
            let need = 100.0 * self.affinity_level as f32;
            if self.affinity_exp < need {
                break;
            }
            self.affinity_exp -= need;
            self.affinity_level += 1;
            gained += 1;
        }
        if self.affinity_level >= max_lv {
            self.affinity_exp = 0.0;
        }
        gained
    }
}

/// S8-M1/M2：活动结算 / 前置消耗的**数值净变化**（`02 §5.15` 数值面）。
///
/// 由 `dp-activity` 的结算结果（`ActivityReward`）在 `dp-app` 侧翻译为纯数值增量，
/// 再经 [`crate::emotion::EmotionEngine::apply_activity_deltas`] 一次性落地——
/// 引擎持有 `PetValues` / `neglect.p` / `RoughTracker` 的私有写权，core-loop 不绕过引擎。
///
/// 边界：**经济入账 / 技能升级 / 学费与旅行券扣款归 S8-M5**（本结构不承载金额 /
/// 道具 / 技能点，只承载六维数值与 P / rough 两个派生量）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActivityDeltas {
    /// Mood 增量（正 = 提升）。
    pub mood: f32,
    /// Energy 增量（前置消耗为负）。
    pub energy: f32,
    /// Cleanliness 增量（旅游 / 打工为负）。
    pub cleanliness: f32,
    /// 亲密度经验增量（正）。
    pub affinity_exp: f32,
    /// 冷落压力 P 增量（提前召回 +6 / 正常回归 −20；clamp [0, cap]）。
    pub neglect_p_delta: f32,
    /// 粗暴度 step 触发（>0 时 `RoughTracker::observe_negative`；召回 +0.15）。
    pub rough_step: f32,
}

/// 会话暂停窗口（P2-2 裁定：**起止时间戳入状态，S5-M2 只读**）。
///
/// 语义：`start_ms` = 进入暂停的时刻；`end_ms` = `None` 表示**仍在暂停中**。
/// 时间口径由调用方统一注入（`dp-app` 用 `WallClock::now_ms()` 的 i64 墙钟）。
/// 本结构**不做持久化**（跨进程重启的暂停窗口无意义，离线补偿走 `offline_compensate`），
/// 但保留 `Serialize` 以便 S5-M2 的调试快照与托盘面板只读展示。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PauseWindow {
    /// 进入暂停的墙钟毫秒（未暂停时为 `None`）。
    pub start_ms: Option<i64>,
    /// 退出暂停的墙钟毫秒（进行中为 `None`）。
    pub end_ms: Option<i64>,
    /// 本次暂停已累计毫秒（暂停中为「至今」，已结束为「总时长」）。
    pub accumulated_ms: i64,
    /// 本次运行会话内的暂停次数（观测用）。
    pub count: u32,
}

impl PauseWindow {
    /// 是否处于暂停中。
    #[inline]
    pub fn is_paused(&self) -> bool {
        self.start_ms.is_some() && self.end_ms.is_none()
    }

    /// 推进到「暂停 / 非暂停」的期望态，返回**状态是否发生迁移**。
    ///
    /// 幂等：重复传入同一期望态不产生任何副作用（对应锁屏 30min 内每秒收到
    /// `session_paused = true`、以及恢复后每秒 `false` 的场景）。
    /// 恢复时把本段时长累加进 `accumulated_ms` 并写 `end_ms`（起止时间戳齐全）。
    pub fn observe(&mut self, paused: bool, now_ms: i64) -> bool {
        match (self.is_paused(), paused) {
            (false, true) => {
                self.start_ms = Some(now_ms);
                self.end_ms = None;
                self.count = self.count.saturating_add(1);
                true
            }
            (true, false) => {
                let start = self.start_ms.unwrap_or(now_ms);
                let span = (now_ms - start).max(0);
                self.accumulated_ms = self.accumulated_ms.saturating_add(span);
                self.end_ms = Some(now_ms);
                true
            }
            _ => false,
        }
    }

    /// 只读：当前暂停段时长（暂停中）或最近一段时长（已结束）；未发生过暂停返回 0。
    ///
    /// S5-M2 托盘 / 设置面板只读消费此口。`now_ms` 由调用方注入（C3）。
    pub fn current_span_ms(&self, now_ms: i64) -> i64 {
        match (self.start_ms, self.end_ms) {
            (Some(s), None) => (now_ms - s).max(0),
            (Some(s), Some(e)) => (e - s).max(0),
            _ => 0,
        }
    }
}

/// 宠物可存档状态（S4-M1 骨架）。
///
/// **不含** P / 阶段 / 因子（归 `emotion::EmotionEngine`）——本结构只承载
/// 「跨会话可持久化的数值 + 会话内暂停记账」，与 `02 §4.1` 的 `PetState` 对齐并
/// 保持 S4-M1 最小面。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PetState {
    /// 六维数值。
    pub values: PetValues,
    /// 当前情绪状态（展示用枚举；P2-1 裁定：结构占位 + v2 冻结阈值简化判定）。
    pub emotion: crate::emotion::EmotionState,
    /// 会话暂停窗口（只读消费归 S5-M2）。
    pub pause: PauseWindow,
    /// 最近一次 tick 的墙钟毫秒（i64；`0` = 尚未 tick）。
    pub last_tick_ms: i64,
    /// 今日日期键（`YYYY-MM-DD`，日切用；空串 = 未日切）。
    pub today: String,
}

impl Default for PetState {
    fn default() -> Self {
        Self {
            values: PetValues::default(),
            emotion: crate::emotion::EmotionState::Idle,
            pause: PauseWindow::default(),
            last_tick_ms: 0,
            today: String::new(),
        }
    }
}

impl PetState {
    /// 由配置构造初始状态（C7）。
    pub fn from_cfg(
        emotion: &crate::config::model::EmotionConfig,
        needs: &crate::config::model::NeedsConfig,
    ) -> Self {
        Self { values: PetValues::from_cfg(emotion, needs), ..Self::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60_000;

    fn cfg_pair() -> (
        crate::config::model::EmotionConfig,
        crate::config::model::NeedsConfig,
    ) {
        (
            crate::config::model::EmotionConfig::default(),
            crate::config::model::NeedsConfig::default(),
        )
    }

    #[test]
    fn boredom_display_matches_frozen_mapping_table() {
        // `02 §5.7` Boredom 展示映射表（可复算）：5/15/30/60/120 → 4.17/12.5/25/50/100
        let cases = [
            (5.0f32, 4.166_667f32),
            (15.0, 12.5),
            (30.0, 25.0),
            (60.0, 50.0),
            (120.0, 100.0),
        ];
        for (p, want) in cases {
            let got = PetValues::boredom_display(p, 120.0);
            assert!((got - want).abs() < 1e-4, "P={p} got={got} want={want}");
        }
    }

    #[test]
    fn boredom_display_clamps_and_is_monotonic() {
        assert_eq!(PetValues::boredom_display(0.0, 120.0), 0.0);
        // 超阈封顶 100
        assert_eq!(PetValues::boredom_display(1000.0, 120.0), 100.0);
        // 分母非法时退化到冻结分母 120，不得 panic / 除零
        assert!((PetValues::boredom_display(120.0, 0.0) - 100.0).abs() < 1e-6);
        // 单调不减
        let mut prev = -1.0;
        for i in 0..=130 {
            let v = PetValues::boredom_display(i as f32, 120.0);
            assert!(v >= prev, "非单调于 P={i}");
            prev = v;
        }
    }

    #[test]
    fn values_default_align_with_config_defaults() {
        let (e, n) = cfg_pair();
        let v = PetValues::from_cfg(&e, &n);
        assert_eq!(v.mood, e.dimensions.mood.default);
        assert_eq!(v.energy, e.dimensions.energy.default);
        assert_eq!(v.satiety, n.dimensions.satiety.default);
        assert_eq!(v.cleanliness, n.dimensions.cleanliness.default);
        assert_eq!(v.affinity_level, 1);
        assert_eq!(v.affinity_exp, 0.0);
    }

    #[test]
    fn values_clamp_is_bounded() {
        let (e, n) = cfg_pair();
        let mut v = PetValues {
            mood: 999.0,
            energy: -50.0,
            affinity_level: 99,
            affinity_exp: -3.0,
            satiety: 1e6,
            cleanliness: -1.0,
        };
        v.clamp_to_cfg(&e, &n);
        assert_eq!(v.mood, 100.0);
        assert_eq!(v.energy, 0.0);
        assert_eq!(v.affinity_level, 10);
        assert_eq!(v.affinity_exp, 0.0);
        assert_eq!(v.satiety, 100.0);
        assert_eq!(v.cleanliness, 0.0);
    }

    #[test]
    fn affinity_exp_levels_up_by_formula() {
        let (e, _) = cfg_pair();
        let mut v = PetValues::default();
        // Lv1 需要 100 经验 → 升到 Lv2 且余 0
        assert_eq!(v.add_affinity_exp(100.0, &e), 1);
        assert_eq!(v.affinity_level, 2);
        assert_eq!(v.affinity_exp, 0.0);
        // 一次跨级：Lv2 需 200，累计 250 → 到 Lv3 余 50
        assert_eq!(v.add_affinity_exp(250.0, &e), 1);
        assert_eq!(v.affinity_level, 3);
        assert!((v.affinity_exp - 50.0).abs() < 1e-6);
        // 非正经验无副作用
        assert_eq!(v.add_affinity_exp(0.0, &e), 0);
        assert_eq!(v.add_affinity_exp(-5.0, &e), 0);
    }

    #[test]
    fn affinity_exp_stops_at_max_level() {
        let (e, _) = cfg_pair();
        let mut v = PetValues { affinity_level: 10, affinity_exp: 0.0, ..Default::default() };
        assert_eq!(v.add_affinity_exp(10_000.0, &e), 0);
        assert_eq!(v.affinity_level, 10);
        assert_eq!(v.affinity_exp, 0.0);
    }

    #[test]
    fn pause_window_observe_is_idempotent() {
        let mut w = PauseWindow::default();
        // 首次进入暂停 → 迁移
        assert!(w.observe(true, 1_000));
        assert!(w.is_paused());
        assert_eq!(w.count, 1);
        // 暂停中重复观察 → 无副作用（锁屏 30min 内每秒都会调用）
        assert!(!w.observe(true, 2_000));
        assert!(!w.observe(true, 30 * MINUTE + 1_000));
        assert_eq!(w.start_ms, Some(1_000));
        assert_eq!(w.end_ms, None);
        assert_eq!(w.accumulated_ms, 0);
        // 恢复 → 迁移，起止时间戳齐全
        assert!(w.observe(false, 30 * MINUTE + 1_000));
        assert!(!w.is_paused());
        assert_eq!(w.end_ms, Some(30 * MINUTE + 1_000));
        assert_eq!(w.accumulated_ms, 30 * MINUTE);
        // 非暂停态重复观察 → 无副作用
        assert!(!w.observe(false, 30 * MINUTE + 2_000));
        assert_eq!(w.count, 1);
    }

    #[test]
    fn pause_window_accumulates_across_multiple_windows() {
        let mut w = PauseWindow::default();
        w.observe(true, 0);
        w.observe(false, 5 * MINUTE);
        w.observe(true, 10 * MINUTE);
        w.observe(false, 12 * MINUTE);
        assert_eq!(w.accumulated_ms, 7 * MINUTE);
        assert_eq!(w.count, 2);
        assert_eq!(w.current_span_ms(999 * MINUTE), 2 * MINUTE);
    }

    #[test]
    fn pause_window_span_is_readonly_and_zero_before_any_pause() {
        let w = PauseWindow::default();
        assert_eq!(w.current_span_ms(123_456), 0);
        assert!(!w.is_paused());
    }

    #[test]
    fn pause_window_handles_backward_clock_without_negative_span() {
        // 墙钟回拨防御：跨度不得为负
        let mut w = PauseWindow::default();
        w.observe(true, 10_000);
        w.observe(false, 5_000);
        assert_eq!(w.accumulated_ms, 0);
        w.observe(true, 20_000);
        assert_eq!(w.current_span_ms(1_000), 0);
    }

    #[test]
    fn pet_state_from_cfg_matches_config() {
        let (e, n) = cfg_pair();
        let s = PetState::from_cfg(&e, &n);
        assert_eq!(s.values, PetValues::from_cfg(&e, &n));
        assert_eq!(s.emotion, crate::emotion::EmotionState::Idle);
        assert_eq!(s.last_tick_ms, 0);
        assert!(!s.pause.is_paused());
    }

    #[test]
    fn pet_state_serde_roundtrip_is_camel_case() {
        let s = PetState::default();
        let js = serde_json::to_string(&s).expect("序列化");
        assert!(js.contains("affinityLevel"), "应使用 camelCase：{js}");
        assert!(js.contains("lastTickMs"), "应使用 camelCase：{js}");
        let back: PetState = serde_json::from_str(&js).expect("反序列化");
        assert_eq!(back, s);
    }
}
