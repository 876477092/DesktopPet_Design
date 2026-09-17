//! `emotion::neglect`：P 累积 / 封顶 / 阶段阈值纯逻辑，以及两条「保持窗口」计时器
//! （`02 §5.1` / §5.3 / §5.23；`01 §6.11.8.1` D-4、FR-11-12 第 3 层）。
//!
//! ## 本模块职责（与 `engine` 的切分）
//!
//!   - **纯逻辑**：ΔP 累积 + 封顶、阶段阈值比较、确认期时长选择 —— 无状态、可独立复算；
//!   - **两个计时器**：
//!     1. [`NaturalCoolMeter`]：自然消气通道（**仅 L3→L2**，四条件全满足）；
//!     2. [`UnreachableMeter`]：交互不可达时的 L4→L3 兜底（`02 §5.23` 第 3 层，**L5 永不开放**）。
//!   - **不在本模块**：阶段结算的逐层扣 Mood / 事件产出 / 状态写入 —— 需要跨模块状态，
//!     仍由 [`crate::emotion::engine::EmotionEngine`] 编排（登记见台账口径登记）。
//!
//! ## 时间纪律（C3）
//!
//! 零时钟：`now_ms` 由调用方注入；所有窗口**绝对锚定**（`hold_since_ms` / `last_negative_ms`
//! 比较 `now_ms − 锚点`，不做逐 tick 累加），避免时钟大幅前跳时挂死或漏判。

use crate::config::model::{ConfirmCfg, ThresholdsCfg};
use crate::emotion::engine::NeglectPressure;

/// ΔP 累积 + 封顶（`02 §5.2` 第 2 步；`P = clamp(P + Δt×速率, 0, cap)`）。
///
/// `dt_min ≤ 0` 或非有限速率**不推进**（脏输入不污染状态）；`cap` 非有限或为负视为不封顶。
pub fn accrue(p: &mut NeglectPressure, dt_min: f32, rate_per_min: f32, cap: f32) {
    if !dt_min.is_finite() || dt_min <= 0.0 || !rate_per_min.is_finite() || rate_per_min < 0.0 {
        return;
    }
    let upper = if cap.is_finite() && cap >= 0.0 { cap } else { f32::MAX };
    let next = p.p + dt_min * rate_per_min;
    p.p = if next.is_finite() { next.clamp(0.0, upper) } else { p.p };
    p.cap = upper;
}

/// 从 P 中扣除缓解量（`relief.*`；钳 `[0, cap]`，负数忽略）。
pub fn relieve(p: &mut NeglectPressure, amount: f32) {
    if !amount.is_finite() || amount <= 0.0 {
        return;
    }
    p.p = (p.p - amount).clamp(0.0, p.cap.max(0.0));
}

/// P（比较值）→ 目标档位（`02 §5.3` 冻结阈值表 5/15/30/60/120）。
#[must_use]
pub fn level_for(p_eff: f32, t: &ThresholdsCfg) -> u8 {
    if !p_eff.is_finite() {
        return 0;
    }
    if p_eff >= t.l5 as f32 {
        5
    } else if p_eff >= t.l4 as f32 {
        4
    } else if p_eff >= t.l3 as f32 {
        3
    } else if p_eff >= t.l2 as f32 {
        2
    } else if p_eff >= t.l1 as f32 {
        1
    } else {
        0
    }
}

/// 档位变化所需的确认期（`02 §5.3`：升级 `upSec` / 回退 `downSec`）。
#[must_use]
pub fn confirm_ms(from: u8, to: u8, cfg: &ConfirmCfg) -> i64 {
    let sec = if to > from { cfg.up_sec } else { cfg.down_sec };
    (sec as i64).saturating_mul(1000)
}

/// 自然消气计量器（`01 §6.11.8.1` D-4 收紧版；`02 §5.3`）。
///
/// 触发条件（**四条全满足**）：
///   1. `P < naturalPThreshold`（默认 15）**且持续 `naturalHoldSec`**（默认 60s）；
///   2. **期间**累计正向交互 ≥ `naturalMinPositive`（默认 3 次）；
///   3. 近 `noNegativeWindowSec`（默认 1h）**无负向事件**；
///   4. **非关系降温期**（`cool_days < coolDaysBlock`，由 `engine` 经 `cooling` 参数传入）。
///
/// 中断规则：计时期间出现任一负向事件 → 保持窗口与正向计数**整个 reset 重来**
/// （但 `last_negative_ms` 保留，用于条件 ③ 的 1h 窗口）。
#[derive(Clone, Debug, Default)]
pub struct NaturalCoolMeter {
    hold_since_ms: Option<i64>,
    positives: Vec<i64>,
    last_negative_ms: Option<i64>,
}

impl NaturalCoolMeter {
    /// 正向交互记录上限（防御无界增长；远超 `naturalMinPositive`）。
    pub const MAX_POSITIVES: usize = 64;

    /// 新建。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 每拍观察（仅 `level == 3` 时由 `engine` 调用）。
    pub fn observe(&mut self, now_ms: i64, p: f32, cfg: &ConfirmCfg, negative_happened: bool) {
        if negative_happened {
            // 中断：整个 reset 重来（条件 ③ 的锚点另行保留）。
            self.last_negative_ms = Some(now_ms);
            self.hold_since_ms = None;
            self.positives.clear();
            return;
        }
        if !p.is_finite() || p >= cfg.natural_p_threshold as f32 {
            self.hold_since_ms = None;
            self.positives.clear();
            return;
        }
        self.hold_since_ms.get_or_insert(now_ms);
    }

    /// 记录一次正向交互（窗口内计数）。
    pub fn observe_positive(&mut self, now_ms: i64) {
        if self.positives.len() >= Self::MAX_POSITIVES {
            self.positives.remove(0);
        }
        self.positives.push(now_ms);
    }

    /// 四条件是否全满足。
    #[must_use]
    pub fn satisfied(&self, now_ms: i64, p: f32, cooling: bool, cfg: &ConfirmCfg) -> bool {
        if cooling {
            return false;
        }
        if !p.is_finite() || p >= cfg.natural_p_threshold as f32 {
            return false;
        }
        let hold_ms = (cfg.natural_hold_sec as i64).saturating_mul(1000);
        let Some(since) = self.hold_since_ms else {
            return false;
        };
        if now_ms - since < hold_ms {
            return false;
        }
        if self.positives.len() < cfg.natural_min_positive as usize {
            return false;
        }
        let no_neg_ms = (cfg.no_negative_window_sec as i64).saturating_mul(1000);
        match self.last_negative_ms {
            Some(t) => now_ms - t >= no_neg_ms,
            None => true,
        }
    }

    /// 窗口内正向交互数（诊断 / 测试）。
    #[must_use]
    pub fn positive_count(&self) -> usize {
        self.positives.len()
    }

    /// 保持窗口已持续毫秒（`None` = 未在计时）。
    #[must_use]
    pub fn hold_elapsed_ms(&self, now_ms: i64) -> Option<i64> {
        self.hold_since_ms.map(|t| (now_ms - t).max(0))
    }

    /// 最近一次负向事件时刻（`None` = 从未）。
    #[must_use]
    pub const fn last_negative_ms(&self) -> Option<i64> {
        self.last_negative_ms
    }

    /// 触发后复位（保持窗口 + 正向计数；负向锚点保留）。
    pub fn reset(&mut self) {
        self.hold_since_ms = None;
        self.positives.clear();
    }
}

/// 交互不可达时的 L4→L3 兜底计量器（`02 §5.23` 第 3 层 / `01 FR-11-12` P2 兜底）。
///
/// 条件（仅两条，**已删除恒假的「正向交互 ≥6 次」**）：
///   1. `P < unreachableFallbackPThreshold`（默认 10）**且持续 `unreachableHoldSec`**（默认 180s）；
///   2. 近 `unreachableNoNegativeSec`（默认 2h）无负向事件。
///
/// **L5 任何情况下不开放本通道**（由 `engine` 以档位门禁保证，本计量器不感知档位）。
#[derive(Clone, Debug, Default)]
pub struct UnreachableMeter {
    /// 兜底判定的 P 阈值（默认 10；`01 FR-11-12` 表）。
    pub p_threshold: f32,
    hold_since_ms: Option<i64>,
    last_negative_ms: Option<i64>,
}

impl UnreachableMeter {
    /// 默认 P 阈值（`01 FR-11-12` 第 3 层：`P < 10`）。
    pub const DEFAULT_P_THRESHOLD: f32 = 10.0;

    /// 新建。
    #[must_use]
    pub fn new() -> Self {
        Self { p_threshold: Self::DEFAULT_P_THRESHOLD, ..Self::default() }
    }

    /// 每拍观察（`condition_ok` = `P < 阈值`；`negative_happened` = 本拍有负向事件）。
    pub fn observe(&mut self, now_ms: i64, condition_ok: bool, negative_happened: bool) {
        if negative_happened {
            self.last_negative_ms = Some(now_ms);
            self.hold_since_ms = None;
            return;
        }
        if condition_ok {
            self.hold_since_ms.get_or_insert(now_ms);
        } else {
            self.hold_since_ms = None;
        }
    }

    /// 条件是否满足（`hold_sec` / `no_negative_sec` 由 `settings.json.interaction` 提供）。
    #[must_use]
    pub fn satisfied(&self, now_ms: i64, hold_sec: u64, no_negative_sec: u64) -> bool {
        let Some(since) = self.hold_since_ms else {
            return false;
        };
        if now_ms - since < (hold_sec as i64).saturating_mul(1000) {
            return false;
        }
        match self.last_negative_ms {
            Some(t) => now_ms - t >= (no_negative_sec as i64).saturating_mul(1000),
            None => true,
        }
    }

    /// 触发后复位。
    pub fn reset(&mut self) {
        self.hold_since_ms = None;
    }

    /// 保持窗口已持续毫秒（`None` = 未在计时）。
    #[must_use]
    pub fn hold_elapsed_ms(&self, now_ms: i64) -> Option<i64> {
        self.hold_since_ms.map(|t| (now_ms - t).max(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confirm() -> ConfirmCfg {
        ConfirmCfg::default()
    }

    fn pressure(p: f32, cap: f32) -> NeglectPressure {
        NeglectPressure { p, cap, ..NeglectPressure::default() }
    }

    #[test]
    fn accrue_basic_and_capped() {
        let mut n = pressure(0.0, 120.0);
        accrue(&mut n, 1.0, 1.0, 120.0);
        assert!((n.p - 1.0).abs() < 1e-6);
        accrue(&mut n, 1000.0, 1.0, 12.0);
        assert!((n.p - 12.0).abs() < 1e-6, "忙碌封顶 12");
        assert!((n.cap - 12.0).abs() < 1e-6);
    }

    #[test]
    fn accrue_ignores_dirty_input() {
        let mut n = pressure(5.0, 120.0);
        accrue(&mut n, 0.0, 1.0, 120.0);
        accrue(&mut n, -1.0, 1.0, 120.0);
        accrue(&mut n, 1.0, f32::NAN, 120.0);
        accrue(&mut n, 1.0, -3.0, 120.0);
        accrue(&mut n, 1.0, 1.0, f32::NAN);
        assert!((n.p - 6.0).abs() < 1e-6, "只有最后一条合法增量生效");
    }

    #[test]
    fn relieve_clamped_and_dirty_safe() {
        let mut n = pressure(3.0, 120.0);
        relieve(&mut n, 60.0);
        assert_eq!(n.p, 0.0);
        relieve(&mut n, -1.0);
        relieve(&mut n, f32::NAN);
        assert_eq!(n.p, 0.0);
    }

    #[test]
    fn level_for_matches_frozen_thresholds() {
        let t = ThresholdsCfg::default();
        assert_eq!(level_for(4.999, &t), 0);
        assert_eq!(level_for(5.0, &t), 1);
        assert_eq!(level_for(15.0, &t), 2);
        assert_eq!(level_for(30.0, &t), 3);
        assert_eq!(level_for(60.0, &t), 4);
        assert_eq!(level_for(120.0, &t), 5);
        assert_eq!(level_for(f32::NAN, &t), 0);
    }

    #[test]
    fn confirm_ms_picks_up_for_upgrade() {
        let c = confirm();
        assert_eq!(confirm_ms(0, 1, &c), 60_000);
        assert_eq!(confirm_ms(3, 2, &c), 30_000);
    }

    #[test]
    fn natural_cool_requires_all_four_conditions() {
        let c = confirm();
        let mut m = NaturalCoolMeter::new();
        // 条件 ① 持续 60s：不足 60s 不满足
        m.observe(0, 10.0, &c, false);
        assert!(!m.satisfied(30_000, 10.0, false, &c));
        // 补足正向交互（条件 ②）
        for i in 0..3 {
            m.observe_positive(i * 1000);
        }
        assert!(m.satisfied(60_000, 10.0, false, &c), "四条件满足");
        // 条件 ① 破坏：P 回到阈值上
        assert!(!m.satisfied(60_000, 15.0, false, &c));
        // 条件 ④ 破坏：关系降温期
        assert!(!m.satisfied(60_000, 10.0, true, &c));
    }

    #[test]
    fn natural_cool_hold_restarts_when_p_recovers() {
        let c = confirm();
        let mut m = NaturalCoolMeter::new();
        m.observe(0, 10.0, &c, false);
        for i in 0..3 {
            m.observe_positive(i * 1000);
        }
        // P 越过阈值 → 窗口清零（含正向计数）
        m.observe(30_000, 20.0, &c, false);
        assert_eq!(m.hold_elapsed_ms(30_000), None);
        assert_eq!(m.positive_count(), 0);
        m.observe(31_000, 10.0, &c, false);
        assert!(!m.satisfied(31_000 + 59_000, 10.0, false, &c), "须重新计时");
    }

    #[test]
    fn negative_interrupts_whole_meter() {
        let c = confirm();
        let mut m = NaturalCoolMeter::new();
        m.observe(0, 10.0, &c, false);
        for i in 0..3 {
            m.observe_positive(i * 1000);
        }
        m.observe(60_000, 10.0, &c, true);
        assert_eq!(m.hold_elapsed_ms(60_000), None);
        assert_eq!(m.positive_count(), 0);
        assert_eq!(m.last_negative_ms(), Some(60_000));
        // 条件 ③：1h 内不满足
        m.observe(61_000, 10.0, &c, false);
        for i in 0..3 {
            m.observe_positive(62_000 + i);
        }
        assert!(!m.satisfied(130_000, 10.0, false, &c), "近 1h 有负向事件");
        assert!(m.satisfied(61_000 + 3_600_000 + 60_000, 10.0, false, &c));
    }

    #[test]
    fn natural_cool_single_stroke_is_not_enough() {
        let c = confirm();
        let mut m = NaturalCoolMeter::new();
        m.observe(0, 10.0, &c, false);
        m.observe_positive(1_000);
        assert!(!m.satisfied(120_000, 10.0, false, &c), "单次正向交互不足 3 次");
    }

    #[test]
    fn unreachable_meter_two_conditions_only() {
        let mut m = UnreachableMeter::new();
        m.observe(0, true, false);
        assert!(!m.satisfied(179_000, 180, 7200));
        assert!(m.satisfied(180_000, 180, 7200), "无正向交互要求");
        m.observe(200_000, true, true);
        assert!(!m.satisfied(400_000, 180, 7200), "近 2h 有负向事件");
        m.observe(201_000, true, false);
        assert!(m.satisfied(201_000 + 180_000 + 7_200_000, 180, 7200));
    }

    #[test]
    fn unreachable_meter_needs_continuous_p_below_threshold() {
        let mut m = UnreachableMeter::new();
        m.observe(0, true, false);
        m.observe(100_000, false, false);
        m.observe(101_000, true, false);
        assert!(!m.satisfied(200_000, 180, 7200), "中断后须重新计时");
        assert!(m.satisfied(281_000, 180, 7200));
    }
}
