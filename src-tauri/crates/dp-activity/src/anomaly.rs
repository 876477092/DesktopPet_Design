//! 时钟异常检测与保底结算（S8-M2，T-21 段 · 2/4；`02 §5.14` 异常表）。
//!
//! 对应「异常分支」验收（`03` S8-M2 卡片）：
//!   - **回拨**：`now_ms < last_tick_ms` → `elapsed=0`、`end_ms` 不变；
//!     连续 3 次 → [`ClockVerdict::BackwardAbort`]（dp-app 走保底结算）；
//!   - **大幅前跳**（>7 天）：`MAX_CATCHUP` 7 天封顶（`ForwardCatchup`），
//!     到期判定按封顶后时刻推进；
//!   - **小幅前拨**（0 < Δ ≤ 7 天）：≤5min 视为时钟抖动忽略（`ForwardJitter`）；
//!     5min~7 天按真实推进正常结算并记 `ForwardMinor`；
//!   - **DST 切换**：本地偏移 ±3600 且墙钟跳变 ≤1h（本地时钟回拨场景）或
//!     `Δ≥0`（UTC 时钟场景）→ `DstShift`，**不计入回拨计数**、收益不变；
//!   - **跨时区**：偏移差 >60s 且非 DST → `TimezoneChanged`（正常推进，仅记录）。
//!
//! 保底结算语义：`Aborted` 时 dp-app 以 `elapsed/planned`（封顶 1.0）为比例
//! 调 [`crate::settle`]（`RecallKind::Abnormal`：按已完成比例保底、不再额外打折）。

use crate::clock::ClockDelta;

/// 时钟异常判定配置（默认与 `02 §5.14` 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnomalyCfg {
    /// 回拨容忍次数（连续 3 次 → 中止；`backward_limit`）。
    pub backward_limit: u32,
    /// 小幅前拨忽略窗口（毫秒；5min = 时钟抖动）。
    pub minor_ignore_ms: i64,
    /// 大幅前跳封顶（毫秒；7 天 = `MAX_CATCHUP`）。
    pub max_catchup_ms: i64,
    /// DST 偏移容差（秒；±3600）。
    pub dst_tolerance_sec: i32,
    /// 跨时区偏移容差（秒；>60 且非 DST）。
    pub timezone_tolerance_sec: i32,
}

impl Default for AnomalyCfg {
    fn default() -> Self {
        Self {
            backward_limit: 3,
            minor_ignore_ms: 5 * 60 * 1000,
            max_catchup_ms: 7 * 24 * 3600 * 1000,
            dst_tolerance_sec: 3600,
            timezone_tolerance_sec: 60,
        }
    }
}

/// 时钟异常判定结果（`02 §5.14` 异常表词汇）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockVerdict {
    /// 正常推进（含首拍建档与 ≤5min 抖动忽略）。
    Normal,
    /// 时钟抖动（0 < Δ ≤ 5min，忽略不计）。
    ForwardJitter,
    /// 小幅前拨（5min < Δ ≤ 7 天；按真实推进正常结算）。
    ForwardMinor,
    /// 大幅前跳（Δ > 7 天；有效推进按 `max_catchup_ms` 封顶）。
    ForwardCatchup,
    /// 回拨（第 1~2 次：`elapsed=0`、`end_ms` 不变）。
    Backward { count: u32 },
    /// 回拨达上限（第 3 次）：异常中止 → 保底结算。
    BackwardAbort,
    /// DST 切换（偏移 ±3600；不计回拨计数、收益不变）。
    DstShift,
    /// 跨时区（偏移差 >60s 且非 DST；正常推进，仅记录）。
    TimezoneChanged,
}

/// 回拨 / 前跳跟踪器（跨 tick 状态：仅回拨计数；其余判定纯函数）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnomalyTracker {
    /// 连续回拨次数。
    pub backward_count: u32,
}

impl AnomalyTracker {
    /// 判定一次时钟观察（消费 [`ClockDelta`]）。
    ///
    /// 优先级：**回拨（含 DST 本地回拨特判）→ 偏移变化（DST / 跨时区）→ 前跳**。
    #[must_use]
    pub fn classify(&mut self, delta: ClockDelta, cfg: &AnomalyCfg) -> ClockVerdict {
        // DST 本地时钟回拨特判：Δ<0 且偏移同刻变化 ±3600 且回拨量 ≤1h。
        if delta.delta_ms < 0
            && delta.offset_delta_sec.abs() == cfg.dst_tolerance_sec
            && delta.delta_ms.abs() <= i64::from(cfg.dst_tolerance_sec) * 1000
        {
            return ClockVerdict::DstShift;
        }
        if delta.delta_ms < 0 {
            self.backward_count += 1;
            if self.backward_count >= cfg.backward_limit {
                self.backward_count = 0; // 中止后清零（实例结束）
                return ClockVerdict::BackwardAbort;
            }
            return ClockVerdict::Backward { count: self.backward_count };
        }
        // 前向推进（含 0）：偏移变化优先于前跳判定。
        if delta.offset_delta_sec.abs() == cfg.dst_tolerance_sec {
            return ClockVerdict::DstShift;
        }
        if delta.offset_delta_sec.abs() > cfg.timezone_tolerance_sec {
            return ClockVerdict::TimezoneChanged;
        }
        if delta.delta_ms == 0 {
            return ClockVerdict::Normal;
        }
        if delta.delta_ms <= cfg.minor_ignore_ms {
            return ClockVerdict::ForwardJitter;
        }
        if delta.delta_ms > cfg.max_catchup_ms {
            return ClockVerdict::ForwardCatchup;
        }
        ClockVerdict::ForwardMinor
    }

    /// 有效推进毫秒（`elapsed`；前跳封顶、回拨归零、抖动忽略）。
    ///
    /// `Backward*` → 0；`ForwardCatchup` → `max_catchup_ms`；`DstShift` →
    /// 墙钟原始 Δ（UTC 时钟场景收益不变）；其余 → 原始 Δ。
    #[must_use]
    pub fn effective_elapsed_ms(verdict: ClockVerdict, raw_delta_ms: i64, cfg: &AnomalyCfg) -> i64 {
        match verdict {
            ClockVerdict::Backward { .. } | ClockVerdict::BackwardAbort => 0,
            ClockVerdict::ForwardCatchup => cfg.max_catchup_ms,
            ClockVerdict::ForwardJitter => 0,
            _ => raw_delta_ms.max(0),
        }
    }

    /// 重置回拨计数（新活动开始 / 中止结算后）。
    pub fn reset_backward(&mut self) {
        self.backward_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AnomalyCfg {
        AnomalyCfg::default()
    }

    fn d(delta_ms: i64, offset_delta_sec: i32) -> ClockDelta {
        ClockDelta { delta_ms, offset_delta_sec }
    }

    #[test]
    fn first_observation_and_zero_are_normal() {
        let mut t = AnomalyTracker::default();
        assert_eq!(t.classify(d(0, 0), &cfg()), ClockVerdict::Normal);
        assert_eq!(t.backward_count, 0);
    }

    #[test]
    fn forward_jitter_ignored_minor() {
        let mut t = AnomalyTracker::default();
        let v = t.classify(d(4 * 60 * 1000, 0), &cfg());
        assert_eq!(v, ClockVerdict::ForwardJitter);
        assert_eq!(AnomalyTracker::effective_elapsed_ms(v, 4 * 60 * 1000, &cfg()), 0);
    }

    #[test]
    fn forward_minor_is_real_progress() {
        let mut t = AnomalyTracker::default();
        let v = t.classify(d(60 * 60 * 1000, 0), &cfg());
        assert_eq!(v, ClockVerdict::ForwardMinor);
        assert_eq!(AnomalyTracker::effective_elapsed_ms(v, 60 * 60 * 1000, &cfg()), 60 * 60 * 1000);
    }

    #[test]
    fn forward_catchup_caps_at_seven_days() {
        let mut t = AnomalyTracker::default();
        let big = 10 * 24 * 3600 * 1000;
        let v = t.classify(d(big, 0), &cfg());
        assert_eq!(v, ClockVerdict::ForwardCatchup);
        assert_eq!(
            AnomalyTracker::effective_elapsed_ms(v, big, &cfg()),
            cfg().max_catchup_ms,
            "离线 10 天按 7 天封顶"
        );
    }

    #[test]
    fn backward_three_times_aborts() {
        let mut t = AnomalyTracker::default();
        assert_eq!(t.classify(d(-1000, 0), &cfg()), ClockVerdict::Backward { count: 1 });
        assert_eq!(t.classify(d(-1000, 0), &cfg()), ClockVerdict::Backward { count: 2 });
        assert_eq!(t.classify(d(-1000, 0), &cfg()), ClockVerdict::BackwardAbort);
        assert_eq!(t.backward_count, 0, "中止后清零");
        // 清零后可重新计数。
        assert_eq!(t.classify(d(-1, 0), &cfg()), ClockVerdict::Backward { count: 1 });
    }

    #[test]
    fn backward_elapsed_is_zero() {
        let v = ClockVerdict::Backward { count: 1 };
        assert_eq!(AnomalyTracker::effective_elapsed_ms(v, -1000, &cfg()), 0);
        let abort = ClockVerdict::BackwardAbort;
        assert_eq!(AnomalyTracker::effective_elapsed_ms(abort, -1000, &cfg()), 0);
    }

    #[test]
    fn dst_local_rollback_not_counted() {
        let mut t = AnomalyTracker::default();
        // 本地时钟回拨 1h 且偏移从 +28800 → +25200（Δoffset = -3600）。
        let v = t.classify(d(-3600 * 1000, -3600), &cfg());
        assert_eq!(v, ClockVerdict::DstShift, "DST 回拨不计回拨计数");
        assert_eq!(t.backward_count, 0);
        // 普通回拨 1h（无偏移变化）仍是回拨。
        let v2 = t.classify(d(-3600 * 1000, 0), &cfg());
        assert_eq!(v2, ClockVerdict::Backward { count: 1 });
    }

    #[test]
    fn dst_utc_clock_forward_shift_not_counted() {
        let mut t = AnomalyTracker::default();
        // UTC 时钟场景：Δ≥0 且偏移 +3600 → DST。
        let v = t.classify(d(60_000, 3600), &cfg());
        assert_eq!(v, ClockVerdict::DstShift);
        assert_eq!(t.backward_count, 0);
    }

    #[test]
    fn timezone_change_detected() {
        let mut t = AnomalyTracker::default();
        let v = t.classify(d(3_600_000, -7_200), &cfg());
        assert_eq!(v, ClockVerdict::TimezoneChanged);
        assert_eq!(AnomalyTracker::effective_elapsed_ms(v, 3_600_000, &cfg()), 3_600_000);
    }

    #[test]
    fn reset_clears_count() {
        let mut t = AnomalyTracker::default();
        let _ = t.classify(d(-1, 0), &cfg());
        t.reset_backward();
        assert_eq!(t.backward_count, 0);
    }
}
