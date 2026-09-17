//! `emotion::rough`：粗暴对待计数与 `roughFactor`（`02 §5.1` / §5.2 因子 F7；`01` F7 / B-6）。
//!
//! ## 口径（`01 §6.11.2` F7）
//!
//! `roughFactor ∈ [1.0, rough.max]`（默认上限 1.6），每次负向事件（甩出 / 连点戳痒 /
//! 打断三部曲或活动）**+`rough.step`**（默认 0.15），此后**线性衰减**：
//! 距最后一次负向事件满 `rough.decayMin`（默认 120min）回到 1.0。
//!
//! 另有两条**惩罚量**（不由本模块落地，只提供换算）：
//!   - `rough.interruptPenalty`（默认 10）：打断正在进行的道歉三部曲 / 活动 → `P += 10`；
//!   - `rough.recallPenalty`（默认 6）：召回（归 S8 活动系统消费）。
//!
//! ## 时间纪律（C3）
//!
//! 零时钟：`now_ms` 由调用方注入，衰减用**绝对锚定**（`last_negative_ms` + `decayMin`）。

use crate::config::model::RoughCfg;
use crate::save::schema::RoughSave;

/// 粗暴对待计量器（与存档段 C `emotion.rough` 同构）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoughTracker {
    /// 当前粗暴度（恒 ≥ 1.0）。
    value: f32,
    /// 最近一次负向事件墙钟毫秒（`0` = 从未）。
    last_negative_ms: i64,
}

impl Default for RoughTracker {
    /// 与 `RoughSave::default()` 同值（`value = 1.0` / 从未负向）。
    fn default() -> Self {
        Self { value: 1.0, last_negative_ms: 0 }
    }
}

impl RoughTracker {
    /// 由存档恢复（存档值先钳到 `[1.0, max]`，脏档不放大压力）。
    #[must_use]
    pub fn from_save(save: &RoughSave, cfg: &RoughCfg) -> Self {
        let v = if save.value.is_finite() { save.value } else { 1.0 };
        Self {
            value: v.clamp(1.0, cfg.max.max(1.0)),
            last_negative_ms: save.last_negative_ms.max(0),
        }
    }

    /// 投影回存档段 C。
    #[must_use]
    pub const fn to_save(self) -> RoughSave {
        RoughSave { value: self.value, last_negative_ms: self.last_negative_ms }
    }

    /// 原始粗暴度（未衰减）。
    #[must_use]
    pub const fn raw_value(self) -> f32 {
        self.value
    }

    /// 最近一次负向事件时刻（`0` = 从未）。
    #[must_use]
    pub const fn last_negative_ms(self) -> i64 {
        self.last_negative_ms
    }

    /// 记录一次负向事件：`value = clamp(decayed + step, 1.0, max)`（B-6）。
    ///
    /// **先按当下衰减再叠加**，避免长时间无扰动后的旧值虚高。
    pub fn observe_negative(&mut self, now_ms: i64, cfg: &RoughCfg) {
        let decayed = self.factor(now_ms, cfg);
        let step = if cfg.step.is_finite() { cfg.step.max(0.0) } else { 0.0 };
        let max = cfg.max.max(1.0);
        self.value = (decayed + step).clamp(1.0, max);
        self.last_negative_ms = now_ms;
        if self.value <= 1.0 {
            // 退化情形（step = 0）：保持「从未负向」语义，避免伪造衰减锚点。
            self.last_negative_ms = 0;
        }
    }

    /// `roughFactor`：距最后一次负向事件满 `decayMin` 线性回到 1.0。
    #[must_use]
    pub fn factor(&self, now_ms: i64, cfg: &RoughCfg) -> f32 {
        let max = cfg.max.max(1.0);
        if self.value <= 1.0 || self.last_negative_ms <= 0 {
            return 1.0;
        }
        let decay_ms = (cfg.decay_min as i64).saturating_mul(60_000);
        if decay_ms <= 0 {
            return 1.0;
        }
        let elapsed = (now_ms - self.last_negative_ms).max(0);
        if elapsed >= decay_ms {
            return 1.0;
        }
        let remain = 1.0 - elapsed as f32 / decay_ms as f32;
        (1.0 + (self.value.min(max) - 1.0) * remain).clamp(1.0, max)
    }

    /// 打断三部曲 / 活动的 `P` 惩罚量（B-6）。
    #[must_use]
    pub fn interrupt_penalty(cfg: &RoughCfg) -> f32 {
        cfg.interrupt_penalty as f32
    }

    /// 召回惩罚量（消费点归 S8 活动系统）。
    #[must_use]
    pub fn recall_penalty(cfg: &RoughCfg) -> f32 {
        cfg.recall_penalty as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RoughCfg {
        RoughCfg::default()
    }

    #[test]
    fn default_is_neutral() {
        let t = RoughTracker::default();
        assert!((t.factor(0, &cfg()) - 1.0).abs() < 1e-6);
        assert!((t.factor(i64::MAX, &cfg()) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn each_negative_adds_step() {
        let c = cfg();
        let mut t = RoughTracker::default();
        // 时间戳 0 是「从未负向」哨兵（存档默认），故用例从 1_000 起算。
        t.observe_negative(1_000, &c);
        assert!((t.factor(1_000, &c) - 1.15).abs() < 1e-6);
        t.observe_negative(1_000, &c);
        assert!((t.factor(1_000, &c) - 1.30).abs() < 1e-6);
    }

    #[test]
    fn capped_at_max() {
        let c = cfg();
        let mut t = RoughTracker::default();
        for i in 1..=50 {
            t.observe_negative(i * 1000, &c);
        }
        assert!(t.factor(50_000, &c) <= c.max + 1e-6);
        assert!((t.raw_value() - c.max).abs() < 1e-6);
    }

    #[test]
    fn linear_decay_back_to_one() {
        let c = cfg();
        let mut t = RoughTracker::default();
        t.observe_negative(1_000, &c);
        // 半衰期：60min 后剩一半增量
        let half = t.factor(1_000 + 60 * 60_000, &c);
        assert!((half - 1.075).abs() < 1e-5, "got {half}");
        // 满 120min 归 1.0
        assert!((t.factor(1_000 + 120 * 60_000, &c) - 1.0).abs() < 1e-6);
        assert!((t.factor(1_000 + 200 * 60_000, &c) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn decay_applied_before_accumulate() {
        let c = cfg();
        let mut t = RoughTracker::default();
        t.observe_negative(1_000, &c);
        // 2h 后（已完全衰减）再记一次 → 只剩 step
        t.observe_negative(1_000 + 120 * 60_000, &c);
        assert!((t.factor(1_000 + 120 * 60_000, &c) - 1.15).abs() < 1e-6);
    }

    #[test]
    fn save_roundtrip_and_dirty_input_clamped() {
        let c = cfg();
        let mut t = RoughTracker::default();
        t.observe_negative(1_000, &c);
        let save = t.to_save();
        assert_eq!(RoughTracker::from_save(&save, &c), t);
        let dirty = RoughSave { value: 99.0, last_negative_ms: -5 };
        let t2 = RoughTracker::from_save(&dirty, &c);
        assert!((t2.raw_value() - c.max).abs() < 1e-6);
        assert_eq!(t2.last_negative_ms(), 0);
    }

    #[test]
    fn penalties_come_from_config() {
        let c = cfg();
        assert!((RoughTracker::interrupt_penalty(&c) - 10.0).abs() < 1e-6);
        assert!((RoughTracker::recall_penalty(&c) - 6.0).abs() < 1e-6);
    }
}
