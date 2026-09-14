//! 感知时间服务（S2-M7，T-07）：`WallClock` 端口（C3）+ 时段判定。
//!
//! 职责（`02 §4.4` WallClock 端口签名 / `02 §5.14` ClockAnomaly / 03 台账 S2-M7 卡片）：
//!   - [`WallClock`] 端口：业务计时取时的**唯一入口**。C3 时间纪律——dp-core 内
//!     禁止裸读墙钟（`SystemTime` / `Utc::now` / `Instant`），[`SystemWallClock`]
//!     是唯一例外（它的职责就是封装真实时钟）；测试经 [`FakeWallClock`] 注入；
//!   - [`segment_of_hour`]：按本地小时判定时段。本阶段 4 段（早晨 / 白天 / 傍晚 /
//!     深夜），S7-M1 扩 6 段（早晨 / 上午 / 午间 / 下午 / 傍晚 / 深夜）。
//!
//! 注：本阶段不实现 `02 §5.14` 的 `ClockAnomaly` 异常枚举；时钟回拨的防御由
//! 使用方（如 [`crate::perception::pomodoro`]）按「记录但不推进」口径处理。

use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};

/// 墙钟端口（C3，`02 §4.4` 签名）：所有业务计时经此端口取时，禁裸读墙钟。
///
/// `Send + Sync` 约定：端口对象由上层（S7-M3 装配）跨线程持有与分发。
pub trait WallClock: Send + Sync {
    /// 当前 Unix 时刻（毫秒；绝对时间锚定的锚点来源）。
    fn now_ms(&self) -> i64;
    /// 当前本地时间（时段判定 / 日志展示用）。
    fn now_local(&self) -> chrono::DateTime<chrono::Local>;
    /// 本地时区相对 UTC 的偏移（秒；东八区 = 28_800）。
    fn local_offset_sec(&self) -> i32;
}

/// 真实时钟实现——**dp-core 内唯一允许读取真实时间的位置**（C3 登记处）。
///
/// `SystemWallClock` 的职责就是封装真实时钟；其余任何模块取时必须经
/// [`WallClock`] 端口（生产注入 `SystemWallClock`，测试注入 [`FakeWallClock`]）。
#[derive(Clone, Copy, Debug)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now_ms(&self) -> i64 {
        // C3 唯一豁免点：真实墙钟读取仅存在于本端口实现内。
        chrono::Utc::now().timestamp_millis()
    }

    fn now_local(&self) -> chrono::DateTime<chrono::Local> {
        chrono::Local::now()
    }

    fn local_offset_sec(&self) -> i32 {
        chrono::Local::now().offset().local_minus_utc()
    }
}

/// 测试注入时钟：构造给定初始毫秒，经 [`FakeWallClock::set_now_ms`] /
/// [`FakeWallClock::advance_ms`] 手动推进（C3 可测性配套，不读真实时间）。
#[derive(Debug)]
pub struct FakeWallClock {
    /// 注入的 Unix 毫秒（原子量：端口对象需跨线程共享）。
    now_ms: AtomicI64,
    /// 注入的时区偏移秒（原子量）。
    offset_sec: AtomicI32,
}

impl FakeWallClock {
    /// 以初始 Unix 毫秒构造（时区偏移默认 0，可经 `set_offset_sec` 注入）。
    #[must_use]
    pub fn new(now_ms: i64) -> Self {
        Self { now_ms: AtomicI64::new(now_ms), offset_sec: AtomicI32::new(0) }
    }

    /// 直接设置当前 Unix 毫秒（绝对值，非增量）。
    pub fn set_now_ms(&self, now_ms: i64) {
        self.now_ms.store(now_ms, Ordering::Relaxed);
    }

    /// 手动推进 `delta_ms`（可为负，用于模拟时钟回拨）。
    pub fn advance_ms(&self, delta_ms: i64) {
        let _ = self.now_ms.fetch_add(delta_ms, Ordering::Relaxed);
    }

    /// 注入本地时区偏移（秒；供 `local_offset_sec` 返回）。
    pub fn set_offset_sec(&self, offset_sec: i32) {
        self.offset_sec.store(offset_sec, Ordering::Relaxed);
    }
}

impl WallClock for FakeWallClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::Relaxed)
    }

    fn now_local(&self) -> chrono::DateTime<chrono::Local> {
        // 说明：chrono 0.4 的 `Local` 不支持注入自定义时区，故假时钟的 `now_local`
        // 以注入毫秒为时刻、用系统时区换算（保证 timestamp 与注入值一致）；
        // 时区偏移经 `set_offset_sec` 单独注入、由 `local_offset_sec` 返回，
        // 供时段判定等注入测试使用。
        let ms = self.now_ms.load(Ordering::Relaxed);
        let utc = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
            .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
        chrono::DateTime::<chrono::Local>::from(utc)
    }

    fn local_offset_sec(&self) -> i32 {
        self.offset_sec.load(Ordering::Relaxed)
    }
}

/// 时段（本阶段 4 段；S7-M1 扩 6 段：早晨 / 上午 / 午间 / 下午 / 傍晚 / 深夜）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeSegment {
    /// 早晨 [5, 11)。
    Morning,
    /// 白天 [11, 17)。
    Daytime,
    /// 傍晚 [17, 23)。
    Evening,
    /// 深夜 [23, 5)（跨零点区间）。
    Night,
}

/// 时段判定：早晨 [5,11) / 白天 [11,17) / 傍晚 [17,23) / 深夜其余（23~5）。
///
/// 入参 `hour` 取 0~23；≥24 按 `% 24` 防御折叠（03 台账 S2-M7 卡片登记）。
#[must_use]
pub fn segment_of_hour(hour: u8) -> TimeSegment {
    match hour % 24 {
        5..=10 => TimeSegment::Morning,
        11..=16 => TimeSegment::Daytime,
        17..=22 => TimeSegment::Evening,
        // 23 与 0..5 归深夜（跨零点）。
        _ => TimeSegment::Night,
    }
}

// ---------------------------------------------------------------------------
// 单元测试（时段边界 / 端口语义 / 注入时钟）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn segment_boundaries_follow_spec() {
        // 深夜 23~5
        assert_eq!(segment_of_hour(0), TimeSegment::Night);
        assert_eq!(segment_of_hour(4), TimeSegment::Night);
        // 早晨 [5, 11)
        assert_eq!(segment_of_hour(5), TimeSegment::Morning);
        assert_eq!(segment_of_hour(10), TimeSegment::Morning);
        // 白天 [11, 17)
        assert_eq!(segment_of_hour(11), TimeSegment::Daytime);
        assert_eq!(segment_of_hour(16), TimeSegment::Daytime);
        // 傍晚 [17, 23)
        assert_eq!(segment_of_hour(17), TimeSegment::Evening);
        assert_eq!(segment_of_hour(22), TimeSegment::Evening);
        // 23 归深夜（傍晚为左闭右开区间 [17, 23)）
        assert_eq!(segment_of_hour(23), TimeSegment::Night);
    }

    #[test]
    fn segment_folds_hours_beyond_24() {
        assert_eq!(segment_of_hour(24), TimeSegment::Night, "24 % 24 = 0 → 深夜");
        assert_eq!(segment_of_hour(28), TimeSegment::Night, "28 % 24 = 4 → 深夜");
        assert_eq!(segment_of_hour(29), TimeSegment::Morning, "29 % 24 = 5 → 早晨");
        assert_eq!(segment_of_hour(35), TimeSegment::Daytime, "35 % 24 = 11 → 白天");
    }

    #[test]
    fn fake_clock_set_and_read_are_consistent() {
        let clock = FakeWallClock::new(1_000);
        assert_eq!(clock.now_ms(), 1_000);
        clock.set_now_ms(2_500);
        assert_eq!(clock.now_ms(), 2_500, "set 为绝对值覆盖");
    }

    #[test]
    fn fake_clock_advance_supports_rollback() {
        let clock = FakeWallClock::new(1_000);
        clock.advance_ms(500);
        assert_eq!(clock.now_ms(), 1_500);
        clock.advance_ms(-2_000);
        assert_eq!(clock.now_ms(), -500, "负增量用于模拟时钟回拨");
    }

    #[test]
    fn fake_clock_offset_is_injected() {
        let clock = FakeWallClock::new(0);
        assert_eq!(clock.local_offset_sec(), 0);
        clock.set_offset_sec(8 * 3600);
        assert_eq!(clock.local_offset_sec(), 8 * 3600);
        clock.set_offset_sec(-5 * 3600);
        assert_eq!(clock.local_offset_sec(), -5 * 3600);
    }

    #[test]
    fn fake_clock_now_local_matches_injected_epoch_ms() {
        let clock = FakeWallClock::new(1_700_000_000_000);
        assert_eq!(clock.now_local().timestamp_millis(), 1_700_000_000_000);
    }

    #[test]
    fn fake_clock_concurrent_reads_smoke() {
        let clock = Arc::new(FakeWallClock::new(42));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let c = Arc::clone(&clock);
                std::thread::spawn(move || {
                    for _ in 0..1_000 {
                        let _ = c.now_ms();
                        let _ = c.local_offset_sec();
                        let _ = c.now_local();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("并发读线程应正常结束");
        }
        assert_eq!(clock.now_ms(), 42, "并发只读不改值");
    }

    #[test]
    fn system_clock_is_plausible_and_non_decreasing() {
        let clock = SystemWallClock;
        let a = clock.now_ms();
        let b = clock.now_ms();
        assert!(a > 1_600_000_000_000, "读值应在 2020-09 之后：{a}");
        assert!(b >= a, "同一实现两次读值不得回退：a={a} b={b}");
    }

    #[test]
    fn system_clock_offset_matches_now_local() {
        let clock = SystemWallClock;
        assert_eq!(clock.local_offset_sec(), clock.now_local().offset().local_minus_utc());
    }

    #[test]
    fn wall_clock_implementations_are_send_sync() {
        // 端口以 dyn 形态被上层跨线程持有（`02 §4.4`）。
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SystemWallClock>();
        assert_send_sync::<FakeWallClock>();
    }
}
