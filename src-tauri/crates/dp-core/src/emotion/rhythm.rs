//! `emotion::rhythm`：时段节律因子 `rhythmFactor` 与深夜 / 用餐重定向（`02 §5.1` / §5.2 因子 F5；`01` B-4 / B-5）。
//!
//! ## 因子来源
//!
//! 因子本体来自 S7-M1 交付的 [`TimeRhythm::rhythm_factor`]（**6 段时段表 + 用餐窗口覆盖**，
//! 全部配置驱动）：深夜 0.35 / 用餐 1.15 / 其余按时段配置。本模块只叠加**开机宽限**：
//!
//! ```text
//! rhythmFactor = base × warmup（宽限窗口内）      // warmupFactor 默认 0.5
//! ```
//!
//! ## 预热锚点的口径裁定（S7-M5 登记）
//!
//! `01 §6.11.13` 只写「开机宽限 10min / 0.5」，**未定义「开机」的锚点**。本卡取
//! **「在场回迁」（离场 / 会话暂停恢复 → 重新在场）后的 `warmupMinutes` 分钟**为窗口：
//!
//!   1. 若锚在**进程启动**，则全新安装的 AC-16 标定（5/15/30/60/120 min）会被宽限
//!      整体拉长（L1 从 5min 变 10min，超 ±10% 容差），与验收基准直接冲突；
//!   2. 锚在「刚回到电脑前」既保住 AC-16 可复算，又完整保留设计意图
//!      （人刚坐下还没进入状态时不要立刻委屈）——`02 §6.1` 启动时序里宽限与
//!      「离线补偿 → 回归问候」同属「刚回来」语义；
//!   3. 判定完全由注入的 `now_ms` 驱动（C3 零时钟）。
//!
//! ## 时间纪律（C3）
//!
//! 零时钟：`now_ms` / `now_local` 均由调用方注入；窗口用**绝对锚定**
//! （`until_ms = 锚点 + warmupMinutes×60_000`，不逐 tick 累减）。

use chrono::Local;

use crate::config::model::RhythmCfg;
use crate::perception::time::{minute_of_day, TimeRhythm, TimeSegment};

/// 节律求解结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RhythmOutput {
    /// 最终 `rhythmFactor`（已含预热折扣）。
    pub factor: f32,
    /// 当前时段（6 段之一）。
    pub segment: TimeSegment,
    /// 是否用餐窗口。
    pub meal: bool,
    /// 是否深夜段（`TimeSegment::Night`）。
    pub night: bool,
    /// 本拍是否仍处于开机宽限窗口内。
    pub warmup: bool,
}

impl Default for RhythmOutput {
    fn default() -> Self {
        Self {
            factor: 1.0,
            segment: TimeSegment::Morning,
            meal: false,
            night: false,
            warmup: false,
        }
    }
}

/// 节律求解器（持有预热窗口锚点）。
#[derive(Clone, Copy, Debug, Default)]
pub struct RhythmSolver {
    /// 预热窗口结束时刻（绝对锚定；`None` = 无宽限）。
    warmup_until_ms: Option<i64>,
}

impl RhythmSolver {
    /// 新建（无预热窗口）。
    #[must_use]
    pub const fn new() -> Self {
        Self { warmup_until_ms: None }
    }

    /// 记录「在场回迁」：开启一段 `warmupMinutes` 宽限窗口（绝对锚定）。
    ///
    /// `warmupMinutes == 0` 不生效（配置关闭宽限）。**窗口只延长不缩短**：重复调用取
    /// 「原窗口」与「当前 + 宽限」的较大者，因此窗口内抖动不会把宽限提前结束。
    pub fn note_reentry(&mut self, now_ms: i64, cfg: &RhythmCfg) {
        if cfg.warmup_minutes == 0 {
            return;
        }
        let span = (cfg.warmup_minutes as i64).saturating_mul(60_000);
        let until = now_ms.saturating_add(span);
        self.warmup_until_ms = Some(match self.warmup_until_ms {
            Some(prev) if prev >= now_ms => prev.max(until),
            _ => until,
        });
    }

    /// 清空预热窗口（暂停 / 离场时调用：宽限语义只对「刚回来」成立）。
    pub fn clear_warmup(&mut self) {
        self.warmup_until_ms = None;
    }

    /// 本拍是否处于宽限窗口内。
    #[must_use]
    pub fn in_warmup(&self, now_ms: i64) -> bool {
        self.warmup_until_ms.is_some_and(|until| now_ms < until)
    }

    /// 求解 `rhythmFactor`（`02 §5.2` 因子 F5）。
    #[must_use]
    pub fn solve(&self, now_local: &chrono::DateTime<Local>, rhythm: &TimeRhythm, cfg: &RhythmCfg, now_ms: i64) -> RhythmOutput {
        let minute = minute_of_day(now_local);
        let segment = rhythm.segment_at(minute);
        let meal = rhythm.is_meal_window(minute);
        // `TimeRhythm::rhythm_factor` 已实现「用餐窗口覆盖时段因子」的口径（S7-M1）。
        let base = rhythm.rhythm_factor(minute);
        let warmup = self.in_warmup(now_ms) && cfg.warmup_factor.is_finite();
        let factor = if warmup { base * cfg.warmup_factor } else { base };
        RhythmOutput {
            factor: if factor.is_finite() { factor.max(0.0) } else { base.max(0.0) },
            segment,
            meal,
            night: segment == TimeSegment::Night,
            warmup,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    fn at(hour: u32, minute: u32) -> chrono::DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 9, 17, hour, minute, 0)
            .single()
            .expect("有效本地时刻")
    }

    fn pair() -> (TimeRhythm, RhythmCfg) {
        let cfg = RhythmCfg::default();
        (TimeRhythm::from_cfg(&cfg), cfg)
    }

    #[test]
    fn factor_follows_segment_table() {
        let (t, c) = pair();
        let s = RhythmSolver::new();
        // 14:00 下午 1.0
        let out = s.solve(&at(14, 0), &t, &c, 0);
        assert!((out.factor - 1.0).abs() < 1e-6);
        assert_eq!(out.segment, TimeSegment::Afternoon);
        // 00:00 深夜 0.35
        let out = s.solve(&at(0, 0), &t, &c, 0);
        assert!((out.factor - 0.35).abs() < 1e-6);
        assert!(out.night);
    }

    #[test]
    fn meal_window_overrides_segment_factor() {
        let (t, c) = pair();
        let s = RhythmSolver::new();
        // 08:00 属早晨段（因子 1.0），但落在早餐窗口 → 1.15
        let out = s.solve(&at(8, 0), &t, &c, 0);
        assert!(out.meal);
        assert!((out.factor - 1.15).abs() < 1e-6, "用餐窗口须覆盖时段因子");
    }

    #[test]
    fn warmup_halves_factor_and_expires() {
        let (t, c) = pair();
        let mut s = RhythmSolver::new();
        s.note_reentry(0, &c);
        let out = s.solve(&at(14, 0), &t, &c, 1_000);
        assert!(out.warmup);
        assert!((out.factor - 0.5).abs() < 1e-6);
        // 10min 后窗口结束
        let out = s.solve(&at(14, 0), &t, &c, 600_000);
        assert!(!out.warmup);
        assert!((out.factor - 1.0).abs() < 1e-6);
    }

    #[test]
    fn warmup_clear_and_idempotent_extend() {
        let c = RhythmCfg::default();
        let mut s = RhythmSolver::new();
        s.note_reentry(0, &c);
        assert!(s.in_warmup(1));
        s.clear_warmup();
        assert!(!s.in_warmup(1));
        // 窗口内重复 call 只延长到「当前 + 宽限」
        s.note_reentry(0, &c);
        s.note_reentry(60_000, &c);
        assert!(s.in_warmup(600_000));
        assert!(!s.in_warmup(660_000));
    }

    #[test]
    fn warmup_disabled_by_zero_minutes() {
        let c = RhythmCfg { warmup_minutes: 0, ..RhythmCfg::default() };
        let mut s = RhythmSolver::new();
        s.note_reentry(0, &c);
        assert!(!s.in_warmup(0));
    }

    #[test]
    fn minute_of_day_consistent_with_input() {
        let dt = at(23, 30);
        assert_eq!(u32::from(minute_of_day(&dt)), dt.hour() * 60 + dt.minute());
    }
}
