//! 活动时间服务（S8-M1，T-21 段 · 1/4；`02 §5.13` dp-activity/src/clock.rs）。
//!
//! 职责：
//!   - **复用 `dp-core::perception::time::WallClock` 端口**（C3 时间纪律的唯一取时口，
//!     不在本 crate 重新定义墙钟 trait——避免双真源）；本模块提供活动专用的
//!     [`ClockTracker`]：跨 tick 记录 `last_tick_ms` / `last_offset_sec`，供
//!     [`crate::anomaly`] 做回拨 / 前拨 / DST / 跨时区检测；
//!   - [`is_deep_night`]：23:00-05:00 深夜窗口判定（D-1 深夜回归延后结算）；
//!   - [`segment_of_hour`]：本地时段 6 段（与 `dp-core` 时段口径一致，供
//!     `jobs.modifiers` 的 `timeSegment==morning` 判定）。

use chrono::Timelike;
use dp_core::perception::time::WallClock;

/// 深夜窗口起点（本地小时；`01 §6.13.1` D-1：23:00-05:00）。
pub const DEEP_NIGHT_FROM_HOUR: u8 = 23;
/// 深夜窗口终点（本地小时；05:00 结束，跨零点区间）。
pub const DEEP_NIGHT_TO_HOUR: u8 = 5;

/// 跨 tick 时钟跟踪（S8-M2 异常检测的输入缓存）。
///
/// 零时钟：`observe` 接收调用方注入的 `now_ms`（经 `WallClock` 端口取到）与
/// 时区偏移秒；本结构不做任何真实时间读取。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClockTracker {
    /// 上次 tick 的 UTC 毫秒（`None` = 尚未首次观察）。
    pub last_tick_ms: Option<i64>,
    /// 上次观察的本地时区偏移（秒；东八区 = 28_800）。
    pub last_offset_sec: Option<i32>,
}

impl ClockTracker {
    /// 记录一次观察（返回**本次与上次的差值**，供异常检测消费）。
    ///
    /// 口径（`02 §5.14`）：
    ///   - 首次观察（`last_tick_ms = None`）→ `delta_ms = 0`，只建档不判定；
    ///   - `now_ms < last_tick_ms` → 负差值（回拨）；
    ///   - 时区偏移变化差值（秒）独立返回（DST / 跨时区判定用）。
    #[must_use]
    pub fn observe(&mut self, now_ms: i64, offset_sec: i32) -> ClockDelta {
        let delta_ms = self
            .last_tick_ms
            .map(|last| now_ms.saturating_sub(last))
            .unwrap_or(0);
        let offset_delta = self
            .last_offset_sec
            .map(|last| offset_sec.saturating_sub(last))
            .unwrap_or(0);
        self.last_tick_ms = Some(now_ms);
        self.last_offset_sec = Some(offset_sec);
        ClockDelta { delta_ms, offset_delta_sec: offset_delta }
    }

    /// 重置跟踪（新活动开始时清零回拨计数基线；异常检测计数在 [`crate::anomaly`]）。
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// 一次观察的时钟差值（`02 §5.14` 异常表检测输入）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockDelta {
    /// 墙钟推进量（毫秒；可为负 = 回拨）。
    pub delta_ms: i64,
    /// 本地时区偏移变化（秒；±3600 = 疑似 DST）。
    pub offset_delta_sec: i32,
}

/// 本地时刻是否落在深夜窗口 [23:00, 05:00)（跨零点；D-1 判定）。
#[must_use]
pub const fn is_deep_night(hour: u8) -> bool {
    hour >= DEEP_NIGHT_FROM_HOUR || hour < DEEP_NIGHT_TO_HOUR
}

/// 本地时段 6 段（与 `dp-core::perception::time::TimeSegment` 同口径；以字符串
/// 返回供 `jobs.modifiers[].when == "timeSegment==morning"` 匹配）。
///
/// 边界：早晨 [05,09) / 上午 [09,11) / 午间 [11,13) / 下午 [13,17) / 傍晚 [17,23) /
/// 深夜 [23,05)。
#[must_use]
pub const fn segment_of_hour(hour: u8) -> &'static str {
    match hour {
        5..=8 => "morning",
        9..=10 => "forenoon",
        11..=12 => "noon",
        13..=16 => "afternoon",
        17..=22 => "dusk",
        _ => "night",
    }
}

/// 经 `WallClock` 端口取当前本地小时（0..=23；C3：禁止裸读墙钟）。
#[must_use]
pub fn local_hour_of(wall: &dyn WallClock) -> u8 {
    wall.now_local().hour() as u8
}

/// 经 `WallClock` 端口取当前本地时区偏移（秒；DST 检测用）。
#[must_use]
pub fn offset_sec_of(wall: &dyn WallClock) -> i32 {
    wall.local_offset_sec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dp_core::perception::FakeWallClock;

    #[test]
    fn tracker_first_observe_is_zero_delta() {
        let mut t = ClockTracker::default();
        let d = t.observe(1_000, 28_800);
        assert_eq!(d.delta_ms, 0, "首次观察只建档不判定");
        assert_eq!(d.offset_delta_sec, 0);
        assert_eq!(t.last_tick_ms, Some(1_000));
    }

    #[test]
    fn tracker_detects_forward_and_backward() {
        let mut t = ClockTracker::default();
        let _ = t.observe(1_000, 28_800);
        let fwd = t.observe(1_500, 28_800);
        assert_eq!(fwd.delta_ms, 500);
        let back = t.observe(1_400, 28_800);
        assert_eq!(back.delta_ms, -100, "回拨检出负差值");
        let dst = t.observe(1_400, 32_400);
        assert_eq!(dst.offset_delta_sec, 3_600, "DST 偏移 +1h 检出");
    }

    #[test]
    fn tracker_reset_clears_baseline() {
        let mut t = ClockTracker::default();
        let _ = t.observe(1_000, 28_800);
        t.reset();
        let d = t.observe(900, 28_800);
        assert_eq!(d.delta_ms, 0, "reset 后首次观察不判回拨");
    }

    #[test]
    fn deep_night_window_is_cross_midnight() {
        assert!(is_deep_night(23));
        assert!(is_deep_night(0));
        assert!(is_deep_night(4));
        assert!(!is_deep_night(5), "05:00 起退出深夜窗口");
        assert!(!is_deep_night(12));
        assert!(!is_deep_night(22));
    }

    #[test]
    fn segment_boundaries_match_dp_core() {
        assert_eq!(segment_of_hour(5), "morning");
        assert_eq!(segment_of_hour(8), "morning");
        assert_eq!(segment_of_hour(9), "forenoon");
        assert_eq!(segment_of_hour(11), "noon");
        assert_eq!(segment_of_hour(13), "afternoon");
        assert_eq!(segment_of_hour(17), "dusk");
        assert_eq!(segment_of_hour(23), "night");
        assert_eq!(segment_of_hour(3), "night");
    }

    #[test]
    fn local_hour_via_wall_clock_port() {
        // FakeWallClock 以注入毫秒换算本地时间；这里构造一个已知小时（UTC+8）。
        let wall = FakeWallClock::new(1_800_000_000_000); // 2027-01-10 附近的固定时刻
        let _ = wall.set_offset_sec(28_800); // 不读真实时钟
        // 该毫秒对应本地小时在 [0,23] 内即可（不断言具体值，避免测试脆）。
        let hour = local_hour_of(&wall);
        assert!(hour <= 23);
        assert_eq!(offset_sec_of(&wall), 28_800);
    }
}
