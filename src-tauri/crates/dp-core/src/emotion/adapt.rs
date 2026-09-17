//! `emotion::adapt`：自适应基线与关系降温（`02 §5.1` / §5.4；`01 §6.11.7`）。
//!
//! ## 口径
//!
//! ```text
//! T_avg7     = 近 7 天「有效交互间隔」滑动平均（剔除 >outlierHours 的离线段）
//! T_exp      = clamp(avgRatio × T_avg7, tMin, tMax)     再受「单日 ±maxDailyChange」限幅
//! adaptFactor = clamp((T_exp0 / T_exp)^0.5, factorMin, factorMax)
//! ```
//!
//! `T_exp0 = baseMin − clingyCoef × 粘人度` 由 [`crate::emotion::personality::Personality::t_exp0`]
//! **运行时派生**（配置中不存在扁平 `adapt.exp0`，出现即 Schema 报错）。
//!
//! **关系降温**（连续 `coolDays` 天日均有效互动 < `coolDaysMinInteract`）：
//! 关闭 L3 自然消气通道 + `P` 累积 ×`coolMultiplier`（消费点在 `engine`）；
//! 连续 `zeroDays` 天几乎零互动 → 追加二级降温事件（回归冷淡台词 / 停主动求助归 S7-M8）。
//!
//! ## 存储
//!
//! 与存档段 C `emotion.adapt`（[`AdaptationSave`]）**同构**，最多 7 条样本 ≈ 420B；
//! 两侧经显式转换函数搬运，字段逐项一一对应。
//!
//! ## 时间纪律（C3）
//!
//! 零时钟：`now_ms` / 日期键均由调用方注入；日切由 [`AdaptationState::roll_day`] 显式驱动。

use crate::config::model::AdaptCfg;
use crate::save::schema::{AdaptSampleSave, AdaptationSave};

/// 单日交互样本（`02 §5.4` `DailySample`）。
#[derive(Clone, Debug, PartialEq)]
pub struct DailySample {
    /// 日期键（`YYYY-MM-DD`）。
    pub date: String,
    /// 当日交互间隔累计（分钟）。
    pub interval_sum_min: f32,
    /// 当日间隔采样次数。
    pub interval_samples: u32,
    /// 当日交互次数（关系降温判定用）。
    pub interact_count: u32,
}

impl DailySample {
    /// 新建空样本。
    #[must_use]
    pub fn new(date: &str) -> Self {
        Self {
            date: date.to_string(),
            interval_sum_min: 0.0,
            interval_samples: 0,
            interact_count: 0,
        }
    }

    /// 由存档样本转换。
    #[must_use]
    pub fn from_save(save: &AdaptSampleSave) -> Self {
        Self {
            date: save.date.clone(),
            interval_sum_min: if save.interval_sum_min.is_finite() {
                save.interval_sum_min.max(0.0)
            } else {
                0.0
            },
            interval_samples: save.interval_samples,
            interact_count: save.interact_count,
        }
    }

    /// 投影回存档样本。
    #[must_use]
    pub fn to_save(&self) -> AdaptSampleSave {
        AdaptSampleSave {
            date: self.date.clone(),
            interval_sum_min: self.interval_sum_min,
            interval_samples: self.interval_samples,
            interact_count: self.interact_count,
        }
    }
}

/// 关系降温事件（`02 §5.4` `AdaptEvent`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdaptEvent {
    /// 降温等级（1 = 连续 `coolDays` 天；2 = 连续 `zeroDays` 天）。
    RelationCooling(u8),
}

/// 自适应基线状态（最多 7 条日样本）。
#[derive(Clone, Debug, PartialEq)]
pub struct AdaptationState {
    /// 近 7 日样本（`push_back` 新日 + 超 7 弹最旧）。
    samples: Vec<DailySample>,
    /// 期望交互间隔（分钟）。
    t_exp: f32,
    /// 关系降温累计天数。
    cool_days: u32,
    /// 零交互降温累计天数。
    zero_days: u32,
}

impl Default for AdaptationState {
    /// 与 `AdaptationSave::default()` 同值（空样本 / `t_exp = 25` / 无降温）。
    fn default() -> Self {
        let s = AdaptationSave::default();
        Self::from_save(&s)
    }
}

impl AdaptationState {
    /// 由存档恢复。
    #[must_use]
    pub fn from_save(save: &AdaptationSave) -> Self {
        let t_exp = if save.t_exp.is_finite() && save.t_exp > 0.0 { save.t_exp } else { 25.0 };
        let mut samples: Vec<DailySample> =
            save.samples.iter().map(DailySample::from_save).collect();
        while samples.len() > Self::MAX_SAMPLES {
            samples.remove(0);
        }
        Self {
            samples,
            t_exp,
            cool_days: save.cool_days,
            zero_days: save.zero_days,
        }
    }

    /// 投影回存档段 C。
    #[must_use]
    pub fn to_save(&self) -> AdaptationSave {
        AdaptationSave {
            samples: self.samples.iter().map(DailySample::to_save).collect(),
            t_exp: self.t_exp,
            cool_days: self.cool_days,
            zero_days: self.zero_days,
        }
    }

    /// 样本窗口上限（7 天）。
    pub const MAX_SAMPLES: usize = 7;

    /// 只读：期望交互间隔。
    #[must_use]
    pub const fn t_exp(&self) -> f32 {
        self.t_exp
    }

    /// 只读：关系降温累计天数。
    #[must_use]
    pub const fn cool_days(&self) -> u32 {
        self.cool_days
    }

    /// 只读：零交互降温累计天数。
    #[must_use]
    pub const fn zero_days(&self) -> u32 {
        self.zero_days
    }

    /// 只读：样本数（诊断 / 测试）。
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// 取（或新建）当日样本。
    fn today_mut(&mut self, date: &str) -> &mut DailySample {
        if let Some(idx) = self.samples.iter().position(|s| s.date == date) {
            return &mut self.samples[idx];
        }
        self.samples.push(DailySample::new(date));
        let last = self.samples.len() - 1;
        &mut self.samples[last]
    }

    /// 记录一段「有效交互间隔」（`> outlierHours×60` 的离线段**剔除**）。
    pub fn on_interval(&mut self, date: &str, interval_min: f32, cfg: &AdaptCfg) {
        if !interval_min.is_finite() || interval_min <= 0.0 {
            return;
        }
        if interval_min > cfg.outlier_hours as f32 * 60.0 {
            return;
        }
        let s = self.today_mut(date);
        s.interval_sum_min += interval_min;
        s.interval_samples = s.interval_samples.saturating_add(1);
    }

    /// 记录一次有效交互（`interact_count` 用于关系降温判定）。
    pub fn observe_interaction(&mut self, date: &str) {
        let s = self.today_mut(date);
        s.interact_count = s.interact_count.saturating_add(1);
    }

    /// 日切：结算 `T_exp` 与降温天数，返回降温事件。
    ///
    /// **降温计数口径（本卡登记）**：`01 §6.11.7` 表写的是「**连续 3 天**日均有效互动 <3 次」
    /// 与「**连续 7 天**几乎零互动」，故本卡按**连续天数**计数（`cool_days` / `zero_days`
    /// 每日按「刚结束那一天」的 `interact_count` 递增或清零）。
    /// `02 §5.4` 草稿写的是「尾 3 日窗口全低 → 计数器 +1」，该写法下 `coolDays=3` 需第 **5**
    /// 天才达成，与 `01` 表的「连续 3 天」不自洽 ⇒ 以 `01` 为准并在此登记。
    pub fn roll_day(&mut self, today: &str, t_exp0: f32, cfg: &AdaptCfg) -> Vec<AdaptEvent> {
        // ① 结算「刚结束的那一天」（样本中日期最大的、非 today 的一条）。
        let closed = self
            .samples
            .iter()
            .filter(|s| s.date != today)
            .max_by(|a, b| a.date.cmp(&b.date))
            .map(|s| s.interact_count);
        if let Some(n) = closed {
            if n < cfg.cool_days_min_interact {
                self.cool_days = self.cool_days.saturating_add(1).min(7);
            } else {
                self.cool_days = 0;
            }
            if n == 0 {
                self.zero_days = self.zero_days.saturating_add(1).min(7);
            } else {
                self.zero_days = 0;
            }
        }

        // ② 新的一天：开一条空样本（昨日样本保留参与滑动平均）。
        if !self.samples.iter().any(|s| s.date == today) {
            self.samples.push(DailySample::new(today));
        }
        while self.samples.len() > Self::MAX_SAMPLES {
            self.samples.remove(0);
        }

        // ③ 滑动平均 + 单日限幅。
        let (sum, n) = self.samples.iter().fold((0.0f32, 0u32), |(s, n), d| {
            (s + d.interval_sum_min, n.saturating_add(d.interval_samples))
        });
        let t_avg7 = if n > 0 { sum / n as f32 } else { t_exp0 };
        let mut raw = (cfg.avg_ratio * t_avg7).clamp(cfg.t_min as f32, cfg.t_max as f32);
        let delta = cfg.max_daily_change.max(0.0);
        raw = raw.clamp(self.t_exp * (1.0 - delta), self.t_exp * (1.0 + delta));
        if raw.is_finite() && raw > 0.0 {
            self.t_exp = raw;
        }

        let mut ev = Vec::new();
        if cfg.cool_days > 0 && self.cool_days >= cfg.cool_days {
            ev.push(AdaptEvent::RelationCooling(1));
        }
        if cfg.zero_days > 0 && self.zero_days >= cfg.zero_days {
            ev.push(AdaptEvent::RelationCooling(2));
        }
        ev
    }

    /// `adaptFactor = clamp((T_exp0 / T_exp)^0.5, factorMin, factorMax)`。
    #[must_use]
    pub fn factor(&self, t_exp0: f32, cfg: &AdaptCfg) -> f32 {
        if !t_exp0.is_finite() || self.t_exp <= 0.0 {
            return 1.0;
        }
        let r = (t_exp0 / self.t_exp).max(0.0).sqrt();
        if r.is_finite() {
            r.clamp(cfg.factor_min, cfg.factor_max)
        } else {
            cfg.factor_min
        }
    }

    /// 是否处于关系降温期（关闭 L3 自然消气通道 + `P × coolMultiplier`）。
    #[must_use]
    pub fn is_cooling(&self, cfg: &AdaptCfg) -> bool {
        cfg.cool_days > 0 && self.cool_days >= cfg.cool_days
    }

    /// 是否处于「几乎零互动」二级降温期（`zeroDays`）。
    #[must_use]
    pub fn is_zero_interaction(&self, cfg: &AdaptCfg) -> bool {
        cfg.zero_days > 0 && self.zero_days >= cfg.zero_days
    }

    /// 降温期的 `P` 累积乘子（`01 §6.11.7`：×1.2；非降温期恒 1.0）。
    #[must_use]
    pub fn cool_multiplier(&self, cfg: &AdaptCfg) -> f32 {
        if self.is_cooling(cfg) {
            let m = cfg.cool_multiplier;
            if m.is_finite() && m > 0.0 {
                m
            } else {
                1.0
            }
        } else {
            1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AdaptCfg {
        AdaptCfg::default()
    }

    #[test]
    fn outlier_intervals_are_dropped() {
        let c = cfg();
        let mut s = AdaptationState::default();
        s.on_interval("2026-09-17", 9.0 * 60.0, &c);
        assert_eq!(s.sample_count(), 0, ">8h 离线段必须剔除");
        s.on_interval("2026-09-17", 10.0, &c);
        s.on_interval("2026-09-17", 20.0, &c);
        assert_eq!(s.sample_count(), 1);
        let s0 = s.samples.first().expect("样本");
        assert_eq!(s0.interval_samples, 2);
        assert!((s0.interval_sum_min - 30.0).abs() < 1e-6);
    }

    #[test]
    fn factor_clamped_to_cfg_range() {
        let c = cfg();
        let mut s = AdaptationState::default();
        // T_exp 初值 25 → (25/25)^0.5 = 1.0
        assert!((s.factor(25.0, &c) - 1.0).abs() < 1e-6);
        s.t_exp = 1.0;
        assert!((s.factor(25.0, &c) - c.factor_max).abs() < 1e-6);
        s.t_exp = 1000.0;
        assert!((s.factor(25.0, &c) - c.factor_min).abs() < 1e-6);
    }

    #[test]
    fn t_exp_respects_daily_change_limit() {
        let c = cfg();
        let mut s = AdaptationState::default();
        // 极端高频互动：t_avg7 = 1min → raw 先钳到 tMin=8，再受 ±10% 单日限幅 → 22.5
        s.on_interval("2026-09-16", 1.0, &c);
        let _ = s.roll_day("2026-09-17", 25.0, &c);
        assert!((s.t_exp() - 25.0 * 0.9).abs() < 1e-4, "got {}", s.t_exp());
    }

    #[test]
    fn roll_day_keeps_at_most_seven_samples() {
        let c = cfg();
        let mut s = AdaptationState::default();
        for d in 1..=12 {
            let _ = s.roll_day(&format!("2026-09-{d:02}"), 25.0, &c);
        }
        assert_eq!(s.sample_count(), AdaptationState::MAX_SAMPLES);
    }

    #[test]
    fn cooling_after_three_low_interaction_days() {
        let c = cfg();
        let mut s = AdaptationState::default();
        let mut evs = Vec::new();
        // 09-01/02/03 三天低互动 → 在滚入 09-04 时判定降温（第 3 天结束即生效）
        for d in 1..=4 {
            evs.extend(s.roll_day(&format!("2026-09-{d:02}"), 25.0, &c));
        }
        assert_eq!(s.cool_days(), c.cool_days);
        assert!(s.is_cooling(&c));
        assert_eq!(evs, vec![AdaptEvent::RelationCooling(1)]);
        assert!((s.cool_multiplier(&c) - c.cool_multiplier).abs() < 1e-6);

        // 达标一天（≥ coolDaysMinInteract 次）即清零恢复
        for _ in 0..c.cool_days_min_interact {
            s.observe_interaction("2026-09-04");
        }
        let _ = s.roll_day("2026-09-05", 25.0, &c);
        assert!(!s.is_cooling(&c));
        assert!((s.cool_multiplier(&c) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn zero_interaction_escalates_to_level_two() {
        let c = cfg();
        let mut s = AdaptationState::default();
        let mut evs = Vec::new();
        // 连续 7 天零互动 → 滚入第 8 天时触发二级降温
        for d in 1..=8 {
            evs.extend(s.roll_day(&format!("2026-09-{d:02}"), 25.0, &c));
        }
        assert!(evs.contains(&AdaptEvent::RelationCooling(1)));
        assert!(evs.contains(&AdaptEvent::RelationCooling(2)));
        assert!(s.is_zero_interaction(&c));
    }

    #[test]
    fn cooling_counter_resets_on_any_activity() {
        let c = cfg();
        let mut s = AdaptationState::default();
        for d in 1..=3 {
            let _ = s.roll_day(&format!("2026-09-{d:02}"), 25.0, &c);
        }
        assert_eq!(s.cool_days(), 2);
        for _ in 0..c.cool_days_min_interact {
            s.observe_interaction("2026-09-03");
        }
        let _ = s.roll_day("2026-09-04", 25.0, &c);
        assert_eq!(s.cool_days(), 0, "有一天达标即清零重计");
    }

    #[test]
    fn save_roundtrip() {
        let c = cfg();
        let mut s = AdaptationState::default();
        s.on_interval("2026-09-17", 12.0, &c);
        s.observe_interaction("2026-09-17");
        let _ = s.roll_day("2026-09-17", 25.0, &c);
        let back = AdaptationState::from_save(&s.to_save());
        assert_eq!(back, s);
    }

    #[test]
    fn dirty_save_is_sanitized() {
        let dirty = AdaptationSave {
            samples: Vec::new(),
            t_exp: f32::NAN,
            cool_days: 99,
            zero_days: 0,
        };
        let s = AdaptationState::from_save(&dirty);
        assert!((s.t_exp() - 25.0).abs() < 1e-6);
        assert!(s.factor(25.0, &cfg()).is_finite());
    }
}
