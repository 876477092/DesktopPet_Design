//! 感知时间服务（S2-M7，T-07）：`WallClock` 端口（C3）+ 时段判定。
//!
//! 职责（`02 §4.4` WallClock 端口签名 / `02 §5.14` ClockAnomaly / 03 台账 S2-M7 卡片）：
//!   - [`WallClock`] 端口：业务计时取时的**唯一入口**。C3 时间纪律——dp-core 内
//!     禁止裸读墙钟（`SystemTime` / `Utc::now` / `Instant`），[`SystemWallClock`]
//!     是唯一例外（它的职责就是封装真实时钟）；测试经 [`FakeWallClock`] 注入；
//!   - [`segment_of_hour`] / [`TimeRhythm`]：按本地时间判定时段。**S7-M1 起为 6 段**
//!     （早晨 / 上午 / 午间 / 下午 / 傍晚 / 深夜，与 `emotion.json.rhythm.segments[].id`
//!     一一对应），另提供**用餐窗口**判定（07-09 / 11-13 / 17-19，`01 FR-6-2`）。
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

/// 时段（**6 段**，S7-M1 扩段；与 `emotion.json.rhythm.segments[].id` 同口径）。
///
/// 边界（配置默认值，`01 FR-6-2`）：
///   - `Morning` 早晨 [05:00, 09:00)
///   - `Forenoon` 上午 [09:00, 11:00)
///   - `Noon` 午间 [11:00, 13:00)
///   - `Afternoon` 下午 [13:00, 17:00)
///   - `Dusk` 傍晚 [17:00, 23:00)
///   - `Night` 深夜 [23:00, 05:00)（跨零点区间）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeSegment {
    /// 早晨 [05:00, 09:00)。
    Morning,
    /// 上午 [09:00, 11:00)。
    Forenoon,
    /// 午间 [11:00, 13:00)。
    Noon,
    /// 下午 [13:00, 17:00)。
    Afternoon,
    /// 傍晚 [17:00, 23:00)。
    Dusk,
    /// 深夜 [23:00, 05:00)（跨零点区间）。
    Night,
}

impl TimeSegment {
    /// 全部 6 段（固定顺序，供快照 / 遍历）。
    pub const ALL: [TimeSegment; 6] = [
        TimeSegment::Morning,
        TimeSegment::Forenoon,
        TimeSegment::Noon,
        TimeSegment::Afternoon,
        TimeSegment::Dusk,
        TimeSegment::Night,
    ];

    /// 段 ID——**与 `emotion.json.rhythm.segments[].id` 逐字一致**（C7 单一真源）。
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            TimeSegment::Morning => "morning",
            TimeSegment::Forenoon => "forenoon",
            TimeSegment::Noon => "noon",
            TimeSegment::Afternoon => "afternoon",
            TimeSegment::Dusk => "dusk",
            TimeSegment::Night => "night",
        }
    }

    /// 由段 ID 反查（未知 ID → `None`，不 panic）。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        TimeSegment::ALL.into_iter().find(|seg| seg.id() == id)
    }
}

/// 一日内分钟刻度（0 ~ 1439）。
pub type MinuteOfDay = u16;

/// 时段判定（默认 6 段边界）：早晨 [5,9) / 上午 [9,11) / 午间 [11,13) /
/// 下午 [13,17) / 傍晚 [17,23) / 深夜其余（23~5）。
///
/// 入参 `hour` 取 0~23；≥24 按 `% 24` 防御折叠（03 台账 S2-M7 卡片登记）。
#[must_use]
pub fn segment_of_hour(hour: u8) -> TimeSegment {
    match hour % 24 {
        5..=8 => TimeSegment::Morning,
        9..=10 => TimeSegment::Forenoon,
        11..=12 => TimeSegment::Noon,
        13..=16 => TimeSegment::Afternoon,
        17..=22 => TimeSegment::Dusk,
        // 23 与 0..5 归深夜（跨零点）。
        _ => TimeSegment::Night,
    }
}

/// 由本地时间取一日内分钟（纯函数；时间由调用方经 [`WallClock`] 注入，C3）。
#[must_use]
pub fn minute_of_day(dt: &chrono::DateTime<chrono::Local>) -> MinuteOfDay {
    use chrono::Timelike;
    // `Timelike::hour()/minute()` 已返回 `u32`；`%` 为越界防御（正常恒不触发）。
    let hour = dt.hour() % 24;
    let minute = dt.minute() % 60;
    (hour * 60 + minute) as MinuteOfDay
}

/// 解析配置里的 `"HH:MM"` 时刻 → 一日内分钟（非法格式 / 越界 → `None`，不 panic）。
#[must_use]
pub fn parse_hhmm(text: &str) -> Option<MinuteOfDay> {
    let (h, m) = text.trim().split_once(':')?;
    let hour: u32 = h.trim().parse().ok()?;
    let minute: u32 = m.trim().parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some((hour * 60 + minute) as MinuteOfDay)
}

/// 单个时段区间（半开 `[from, to)`；`from > to` 表示跨零点）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct RhythmSpan {
    from: MinuteOfDay,
    to: MinuteOfDay,
    segment: TimeSegment,
    factor: f32,
}

/// 一日节律表（**配置驱动**，`emotion.json.rhythm`）：6 段区间 + 用餐窗口。
///
/// 用途（`02 §5.2` 七因子 `rhythm` 因子 / `02 §5.9` `NeedsEnv.meal_window`）：
///   - [`TimeRhythm::segment_at`]：注入时刻 → [`TimeSegment`]；
///   - [`TimeRhythm::is_meal_window`]：是否落在用餐窗口（07-09 / 11-13 / 17-19）；
///   - [`TimeRhythm::rhythm_factor`]：用餐窗口 → `mealFactor`（1.15），否则取所在段因子
///     （深夜 0.35 / 其余 1.0 / 午间等按配置）。
///
/// 边界：**开机宽限**（`warmupMinutes` × `warmupFactor`）与会话起点相关，属七因子
/// 求解（S7-M4），不在本结构内实现；本结构只回答「此刻属于哪一段 / 是否用餐」。
#[derive(Clone, Debug, PartialEq)]
pub struct TimeRhythm {
    spans: Vec<RhythmSpan>,
    meal_windows: Vec<(MinuteOfDay, MinuteOfDay)>,
    meal_factor: f32,
}

impl TimeRhythm {
    /// 由 `emotion.json.rhythm` 构造（非法时刻 / 未知段 ID 的条目**跳过**，
    /// 保持「配置写坏不崩、退化为默认段判定」）。
    #[must_use]
    pub fn from_cfg(cfg: &crate::config::model::RhythmCfg) -> Self {
        let mut spans = Vec::with_capacity(cfg.segments.len());
        for seg in &cfg.segments {
            let (Some(from), Some(to)) = (parse_hhmm(&seg.from), parse_hhmm(&seg.to)) else {
                continue;
            };
            let Some(segment) = TimeSegment::from_id(&seg.id) else {
                continue;
            };
            spans.push(RhythmSpan { from, to, segment, factor: seg.factor });
        }
        let meal_windows = cfg
            .meal_windows
            .iter()
            .filter_map(|w| {
                let (from, to) = (parse_hhmm(&w[0])?, parse_hhmm(&w[1])?);
                Some((from, to))
            })
            .collect();
        Self { spans, meal_windows, meal_factor: cfg.meal_factor }
    }

    /// 注入时刻（一日内分钟）所属时段。
    ///
    /// 命中规则：区间半开 `[from, to)`；跨零点区间（`from > to`）取并集
    /// `m >= from || m < to`；`from == to` 视为覆盖全天（防御）。未命中任何区间
    /// （配置缺失 / 写坏）→ 退化到 [`segment_of_hour`] 的默认边界。
    #[must_use]
    pub fn segment_at(&self, minute: MinuteOfDay) -> TimeSegment {
        let m = minute % MINUTES_PER_DAY;
        for span in &self.spans {
            if span.from == span.to {
                return span.segment;
            }
            let hit = if span.from < span.to {
                m >= span.from && m < span.to
            } else {
                m >= span.from || m < span.to
            };
            if hit {
                return span.segment;
            }
        }
        segment_of_hour((m / 60) as u8)
    }

    /// 是否落在用餐窗口（`01 FR-6-2`：07-09 / 11-13 / 17-19）。
    #[must_use]
    pub fn is_meal_window(&self, minute: MinuteOfDay) -> bool {
        self.meal_window_index(minute).is_some()
    }

    /// 用餐窗口序号（0 = 早餐 / 1 = 午餐 / 2 = 晚餐；非用餐时段 → `None`）。
    #[must_use]
    pub fn meal_window_index(&self, minute: MinuteOfDay) -> Option<usize> {
        let m = minute % MINUTES_PER_DAY;
        self.meal_windows.iter().position(|(from, to)| {
            if from == to {
                return false;
            }
            if from < to {
                m >= *from && m < *to
            } else {
                m >= *from || m < *to
            }
        })
    }

    /// 节律因子（`02 §5.2` `rhythm` 因子本体）：用餐窗口优先取 `mealFactor`，
    /// 否则取所在段因子；无匹配段 → 深夜 0.35 / 其余 1.0 的默认口径。
    #[must_use]
    pub fn rhythm_factor(&self, minute: MinuteOfDay) -> f32 {
        let m = minute % MINUTES_PER_DAY;
        if self.is_meal_window(m) {
            return self.meal_factor;
        }
        for span in &self.spans {
            if span.from == span.to {
                return span.factor;
            }
            let hit = if span.from < span.to {
                m >= span.from && m < span.to
            } else {
                m >= span.from || m < span.to
            };
            if hit {
                return span.factor;
            }
        }
        DEFAULT_SEGMENT_FACTORS[self.segment_at(m) as usize]
    }

    /// 已解析的段数量（配置自检用）。
    #[must_use]
    pub fn span_count(&self) -> usize {
        self.spans.len()
    }

    /// 已解析的用餐窗口数量（配置自检用）。
    #[must_use]
    pub fn meal_window_count(&self) -> usize {
        self.meal_windows.len()
    }
}

impl Default for TimeRhythm {
    /// 默认 = `emotion.json.rhythm` 的内置默认（6 段 + 3 用餐窗口 + 1.15）。
    fn default() -> Self {
        Self::from_cfg(&crate::config::model::RhythmCfg::default())
    }
}

/// 一日分钟数。
pub const MINUTES_PER_DAY: MinuteOfDay = 1440;

/// 默认段因子（配置缺失时的兜底；顺序与 [`TimeSegment::ALL`] 对齐）。
const DEFAULT_SEGMENT_FACTORS: [f32; 6] = [1.0, 1.0, 1.15, 1.0, 1.15, 0.35];

// ---------------------------------------------------------------------------
// 单元测试（时段边界 / 端口语义 / 注入时钟）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn segment_boundaries_follow_six_segment_spec() {
        // 深夜 [23, 5)：23 与 0..5
        assert_eq!(segment_of_hour(23), TimeSegment::Night);
        assert_eq!(segment_of_hour(0), TimeSegment::Night);
        assert_eq!(segment_of_hour(4), TimeSegment::Night);
        // 早晨 [5, 9)
        assert_eq!(segment_of_hour(5), TimeSegment::Morning);
        assert_eq!(segment_of_hour(8), TimeSegment::Morning);
        // 上午 [9, 11)
        assert_eq!(segment_of_hour(9), TimeSegment::Forenoon);
        assert_eq!(segment_of_hour(10), TimeSegment::Forenoon);
        // 午间 [11, 13)
        assert_eq!(segment_of_hour(11), TimeSegment::Noon);
        assert_eq!(segment_of_hour(12), TimeSegment::Noon);
        // 下午 [13, 17)
        assert_eq!(segment_of_hour(13), TimeSegment::Afternoon);
        assert_eq!(segment_of_hour(16), TimeSegment::Afternoon);
        // 傍晚 [17, 23)
        assert_eq!(segment_of_hour(17), TimeSegment::Dusk);
        assert_eq!(segment_of_hour(22), TimeSegment::Dusk);
    }

    #[test]
    fn segment_folds_hours_beyond_24() {
        assert_eq!(segment_of_hour(24), TimeSegment::Night, "24 % 24 = 0 → 深夜");
        assert_eq!(segment_of_hour(28), TimeSegment::Night, "28 % 24 = 4 → 深夜");
        assert_eq!(segment_of_hour(29), TimeSegment::Morning, "29 % 24 = 5 → 早晨");
        assert_eq!(segment_of_hour(34), TimeSegment::Forenoon, "34 % 24 = 10 → 上午");
        assert_eq!(segment_of_hour(36), TimeSegment::Noon, "36 % 24 = 12 → 午间");
    }

    #[test]
    fn segment_ids_match_rhythm_config_ids() {
        // C7 单一真源：段 ID 必须与 `emotion.json.rhythm.segments[].id` 逐字一致。
        let cfg = crate::config::model::RhythmCfg::default();
        let ids: Vec<&str> = cfg.segments.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["morning", "forenoon", "noon", "afternoon", "dusk", "night"]);
        for (i, seg) in TimeSegment::ALL.into_iter().enumerate() {
            assert_eq!(seg.id(), ids[i], "段顺序与配置一致");
            assert_eq!(TimeSegment::from_id(ids[i]), Some(seg));
        }
        assert_eq!(TimeSegment::from_id("nightfall"), None, "未知 ID 不 panic");
    }

    #[test]
    fn parse_hhmm_accepts_valid_and_rejects_invalid() {
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("07:30"), Some(450));
        assert_eq!(parse_hhmm("23:59"), Some(1439));
        assert_eq!(parse_hhmm(" 09:00 "), Some(540), "允许空白容错");
        assert_eq!(parse_hhmm("24:00"), None, "越界小时");
        assert_eq!(parse_hhmm("10:60"), None, "越界分钟");
        assert_eq!(parse_hhmm("nope"), None);
        assert_eq!(parse_hhmm(""), None);
    }

    #[test]
    fn time_rhythm_segments_follow_config_windows() {
        let r = TimeRhythm::default();
        assert_eq!(r.span_count(), 6, "默认 6 段全部可解析");
        // 段内时刻。
        assert_eq!(r.segment_at(5 * 60), TimeSegment::Morning);
        assert_eq!(r.segment_at(8 * 60 + 59), TimeSegment::Morning);
        assert_eq!(r.segment_at(9 * 60), TimeSegment::Forenoon);
        assert_eq!(r.segment_at(11 * 60), TimeSegment::Noon);
        assert_eq!(r.segment_at(12 * 60 + 59), TimeSegment::Noon);
        assert_eq!(r.segment_at(13 * 60), TimeSegment::Afternoon);
        assert_eq!(r.segment_at(17 * 60), TimeSegment::Dusk);
        assert_eq!(r.segment_at(23 * 60), TimeSegment::Night);
        // 跨零点区间：23:00~05:00 内全部为深夜，含 00:00。
        assert_eq!(r.segment_at(0), TimeSegment::Night);
        assert_eq!(r.segment_at(4 * 60 + 59), TimeSegment::Night);
        assert_eq!(r.segment_at(1_439), TimeSegment::Night);
    }

    #[test]
    fn time_rhythm_meal_windows_match_spec() {
        let r = TimeRhythm::default();
        assert_eq!(r.meal_window_count(), 3, "07-09 / 11-13 / 17-19");
        // 窗口内（半开左闭右开）。
        assert_eq!(r.meal_window_index(7 * 60), Some(0));
        assert_eq!(r.meal_window_index(8 * 60 + 59), Some(0));
        assert_eq!(r.meal_window_index(11 * 60), Some(1));
        assert_eq!(r.meal_window_index(17 * 60), Some(2));
        // 窗口右端点已越界（半开区间）。
        assert_eq!(r.meal_window_index(9 * 60), None);
        assert_eq!(r.meal_window_index(13 * 60), None);
        assert_eq!(r.meal_window_index(19 * 60), None);
        // 非用餐时刻。
        assert!(!r.is_meal_window(10 * 60));
        assert!(!r.is_meal_window(0));
    }

    #[test]
    fn time_rhythm_factor_prefers_meal_then_segment() {
        let r = TimeRhythm::default();
        // 用餐窗口（早餐 07-09 落在早晨段）→ mealFactor 1.15 优先于段因子 1.0。
        assert!((r.rhythm_factor(7 * 60) - 1.15).abs() < 1e-6);
        // 深夜（23:00 起）→ 0.35；午夜亦同。
        assert!((r.rhythm_factor(23 * 60) - 0.35).abs() < 1e-6);
        assert!((r.rhythm_factor(2 * 60) - 0.35).abs() < 1e-6);
        // 非用餐的常规时段 → 1.0。
        assert!((r.rhythm_factor(10 * 60) - 1.0).abs() < 1e-6);
        // 午间段因子 1.15（配置 noon=1.15），且 11-13 同时是午餐窗口 → 仍 1.15。
        assert!((r.rhythm_factor(12 * 60) - 1.15).abs() < 1e-6);
    }

    #[test]
    fn time_rhythm_skips_malformed_entries_and_falls_back() {
        let mut cfg = crate::config::model::RhythmCfg::default();
        cfg.segments.push(crate::config::model::RhythmSegmentCfg {
            id: "siesta".to_string(),
            from: "13:00".to_string(),
            to: "14:00".to_string(),
            factor: 2.0,
        });
        cfg.segments.push(crate::config::model::RhythmSegmentCfg {
            id: "bad-time".to_string(),
            from: "25:00".to_string(),
            to: "26:00".to_string(),
            factor: 9.0,
        });
        let r = TimeRhythm::from_cfg(&cfg);
        assert_eq!(r.span_count(), 6, "未知段 ID 与非法时刻一律跳过");
        // 全段被清空 → 退化到 `segment_of_hour` 默认边界。
        let mut empty = crate::config::model::RhythmCfg::default();
        empty.segments.clear();
        let r2 = TimeRhythm::from_cfg(&empty);
        assert_eq!(r2.span_count(), 0);
        assert_eq!(r2.segment_at(12 * 60), TimeSegment::Noon, "退化默认边界");
        assert!((r2.rhythm_factor(12 * 60) - 1.15).abs() < 1e-6, "兜底段因子");
        assert!((r2.rhythm_factor(23 * 60) - 0.35).abs() < 1e-6, "深夜兜底 0.35");
    }

    #[test]
    fn minute_of_day_reads_injected_local_time() {
        // 经假时钟换取本地时间（C3：测试不读真实时钟语义，只验证换算）。
        let clock = FakeWallClock::new(1_700_000_000_000);
        let m = minute_of_day(&clock.now_local());
        assert!(m < MINUTES_PER_DAY, "一日内分钟必须落在 0..1440：{m}");
        // 与同一时刻的小时/分钟分量一致。
        use chrono::Timelike;
        let local = clock.now_local();
        assert_eq!(m, (local.hour() as u16) * 60 + local.minute() as u16);
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
