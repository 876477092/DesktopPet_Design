//! 漫游决策：站立面端口 / 兜底地面 / 采样器 / 播种 PRNG / 决策间隔（S2-M4）。
//!
//! K-3 口径（`02 §5`）：
//!   - 决策间隔 5~30s（`RoamCfg::decision_interval_sec`），按 `RoamCfg::pace`
//!     缩放：**pace 越大越活泼 → 间隔越短**（base / pace）；
//!   - `decide_target` 最多 12 次采样：范围内随机取点 → 无合法站立面则重试 →
//!     落在光标热区内跳过 → 禁区跳过 → 12 次全失败返回 `None`（上层回退 idle）；
//!   - 活动范围约束：采样限制在「当前显示器工作区 ∪ 相邻屏」。S2-M4 取默认
//!     「全工作区」口径（不做中心比例缩小；精细范围配置属 `01 §6.2` FR-2-7
//!     后续）。
//!
//! 站立面端口 [`StandSurface`] 保持最小（`is_valid_stand` + `clamp_to_stand`）：
//! 本模块提供兜底实现 [`DesktopFloor`]（工作区底边行走，显示器列表由调用方
//! 注入）；S2-M6 的 PlatformGraph（窗口标题栏平台）将实现同一端口，接口不再
//! 扩充。
//!
//! 时间纪律（C3）：本模块零时钟——[`decision_interval_ms`] 为纯计算，随机数
//! 来自调用方注入种子的 [`SplitMix64`]（C3 零新增依赖：不引入 rand）。

use super::avoid::CursorHeat;
use super::Vec2;
use crate::config::model::RoamCfg;

/// 单次决策最大采样次数（`02 §5` K-3：最多 12 次）。
pub const MAX_SAMPLES: u32 = 12;

/// 站立面校验容差（VDC 逻辑像素；底边线上的点视为合法站立）。
pub const STAND_EPS_PX: f32 = 0.5;

// ---------------------------------------------------------------------------
// 显示器几何（轻量端口结构，由调用方从 MonitorInfo 注入）
// ---------------------------------------------------------------------------

/// 单个显示器的纯逻辑几何（VDC 96-dpi 逻辑像素，C6/RV-17，原点可为负）。
///
/// dp-core 不依赖 dp-platform：由调用方从 `dp_platform::win::display::MonitorInfo`
/// 换算注入，公式（与 `MonitorInfo::{vdc_width, vdc_height, vdc_to_spc}` 互逆）：
///
/// ```text
/// size_vdc        = (m.vdc_width(), m.vdc_height())
/// work_origin_vdc = m.origin_vdc
///                 + ((rc_work.left − rc_monitor.left) / scale,
///                    (rc_work.top  − rc_monitor.top ) / scale)
/// work_size_vdc   = (rc_work.width() / scale, rc_work.height() / scale)
/// id              = m.id.0
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorGeom {
    /// 显示器标识（与 `MonitorId` 一致的会话内索引）。
    pub id: u64,
    /// 显示器左上角在 VDC 中的坐标（可为负）。
    pub origin_vdc: Vec2,
    /// 显示器 VDC 尺寸（逻辑像素）。
    pub size_vdc: Vec2,
    /// 工作区左上角（VDC，扣除任务栏等）。
    pub work_origin_vdc: Vec2,
    /// 工作区 VDC 尺寸（逻辑像素）。
    pub work_size_vdc: Vec2,
    /// 是否主显示器。
    pub primary: bool,
}

impl MonitorGeom {
    /// 显示器右缘（VDC x，不含）。
    #[inline]
    #[must_use]
    pub fn right_vdc(&self) -> f32 {
        self.origin_vdc.x + self.size_vdc.x
    }

    /// 显示器下缘（VDC y，不含）。
    #[inline]
    #[must_use]
    pub fn bottom_vdc(&self) -> f32 {
        self.origin_vdc.y + self.size_vdc.y
    }

    /// 显示器矩形中心（VDC）。
    #[inline]
    #[must_use]
    pub fn center_vdc(&self) -> Vec2 {
        Vec2::new(
            self.origin_vdc.x + self.size_vdc.x / 2.0,
            self.origin_vdc.y + self.size_vdc.y / 2.0,
        )
    }

    /// VDC 点是否落在本显示器矩形内（半开区间，`display.rs` 同口径）。
    #[inline]
    #[must_use]
    pub fn contains_vdc(&self, p: Vec2) -> bool {
        p.x >= self.origin_vdc.x
            && p.x < self.right_vdc()
            && p.y >= self.origin_vdc.y
            && p.y < self.bottom_vdc()
    }

    /// 工作区右缘（VDC x，不含）。
    #[inline]
    #[must_use]
    pub fn work_right_vdc(&self) -> f32 {
        self.work_origin_vdc.x + self.work_size_vdc.x
    }

    /// 工作区底边（VDC y）——`DesktopFloor` 的地面线。
    #[inline]
    #[must_use]
    pub fn work_bottom_vdc(&self) -> f32 {
        self.work_origin_vdc.y + self.work_size_vdc.y
    }

    /// 工作区中心（VDC）。
    #[inline]
    #[must_use]
    pub fn work_center(&self) -> Vec2 {
        Vec2::new(
            self.work_origin_vdc.x + self.work_size_vdc.x / 2.0,
            self.work_origin_vdc.y + self.work_size_vdc.y / 2.0,
        )
    }

    /// VDC → NDC（0~1 归一，基于显示器矩形；零尺寸防御退化为 0.5）。
    ///
    /// FR-1-4：引擎以 NDC + monitorId 做显示器变更快照。
    #[must_use]
    pub fn vdc_to_ndc(&self, p: Vec2) -> Vec2 {
        let w = if self.size_vdc.x > 0.0 { self.size_vdc.x } else { 1.0 };
        let h = if self.size_vdc.y > 0.0 { self.size_vdc.y } else { 1.0 };
        Vec2::new((p.x - self.origin_vdc.x) / w, (p.y - self.origin_vdc.y) / h)
    }

    /// NDC → VDC（`vdc_to_ndc` 的逆换算，显示器几何变更后的重算入口）。
    #[must_use]
    pub fn ndc_to_vdc(&self, ndc: Vec2) -> Vec2 {
        Vec2::new(
            self.origin_vdc.x + ndc.x * self.size_vdc.x,
            self.origin_vdc.y + ndc.y * self.size_vdc.y,
        )
    }
}

// ---------------------------------------------------------------------------
// 站立面端口 + 兜底地面
// ---------------------------------------------------------------------------

/// 站立面端口（最小接口）。
///
/// S2-M4 兜底实现 = [`DesktopFloor`]（工作区底边行走）；S2-M6 的
/// PlatformGraph（窗口标题栏平台）将实现同一端口——接口保持最小、不再扩充。
pub trait StandSurface {
    /// 点是否为合法站立点。
    fn is_valid_stand(&self, p: Vec2) -> bool;

    /// 将任意点钳制到最近合法站立点。
    fn clamp_to_stand(&self, p: Vec2) -> Vec2;
}

/// 兜底站立面：工作区底边行走（S2-M4 默认地面）。
///
/// 显示器列表由调用方注入（`MonitorGeom`，见其文档注释的换算公式）。
/// 合法站立点 = 某显示器工作区底边线上的点（x 在该屏工作区 x 范围内，
/// y 距地面线 ≤ [`STAND_EPS_PX`]）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesktopFloor {
    monitors: Vec<MonitorGeom>,
}

impl DesktopFloor {
    /// 由显示器列表构造。
    #[must_use]
    pub fn new(monitors: Vec<MonitorGeom>) -> Self {
        Self { monitors }
    }

    /// 显示器列表快照（只读）。
    #[must_use]
    pub fn monitors(&self) -> &[MonitorGeom] {
        &self.monitors
    }

    /// 按 x 选择所在（或最近）显示器：先命中工作区 x 区间（半开），
    /// 未命中取工作区 x 中心最近者。
    fn monitor_for_x(&self, x: f32) -> Option<&MonitorGeom> {
        if let Some(m) = self
            .monitors
            .iter()
            .find(|m| x >= m.work_origin_vdc.x && x < m.work_right_vdc())
        {
            return Some(m);
        }
        self.monitors.iter().min_by(|a, b| {
            let da = (x - a.work_center().x).abs();
            let db = (x - b.work_center().x).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

impl StandSurface for DesktopFloor {
    fn is_valid_stand(&self, p: Vec2) -> bool {
        match self.monitor_for_x(p.x) {
            Some(m) => (p.y - m.work_bottom_vdc()).abs() <= STAND_EPS_PX,
            None => false,
        }
    }

    fn clamp_to_stand(&self, p: Vec2) -> Vec2 {
        // 无显示器（防御）：原样返回，交由 is_valid_stand 判 false。
        match self.monitor_for_x(p.x) {
            Some(m) => Vec2::new(
                p.x.clamp(m.work_origin_vdc.x, m.work_right_vdc()),
                m.work_bottom_vdc(),
            ),
            None => p,
        }
    }
}

// ---------------------------------------------------------------------------
// 采样范围 / 禁区
// ---------------------------------------------------------------------------

/// 漫游采样范围（轴对齐矩形，VDC）。
///
/// `02 §5` K-3 活动范围约束语义 =「当前显示器工作区 ∪ 相邻屏」；矩形近似取
/// 该并集的包围盒（采样点随后经 `StandSurface::clamp_to_stand` 投影到站立面，
/// 间隙区自然落回最近屏）。S2-M4 取默认「全工作区」口径，不做中心比例缩小
/// （精细范围配置属 `01 §6.2` FR-2-7 后续）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoamRegion {
    /// 左上角（VDC）。
    pub min: Vec2,
    /// 右下角（VDC，不含）。
    pub max: Vec2,
}

impl RoamRegion {
    /// 单显示器工作区范围。
    #[must_use]
    pub fn from_monitor_work(m: &MonitorGeom) -> Self {
        Self { min: m.work_origin_vdc, max: m.work_origin_vdc + m.work_size_vdc }
    }

    /// 「当前显示器工作区 ∪ 相邻屏工作区」的包围盒。
    ///
    /// 相邻 = 两显示器矩形间隙 ≤ [`ADJACENT_GAP_TOL_PX`]（见 [`is_adjacent`]；
    /// 共享边界、角接触与微小排列间隙均算，跨大段空白的远屏不算）。
    #[must_use]
    pub fn from_current_and_adjacent(current: &MonitorGeom, monitors: &[MonitorGeom]) -> Self {
        let mut min = current.work_origin_vdc;
        let mut max = current.work_origin_vdc + current.work_size_vdc;
        for m in monitors {
            if m.id == current.id || !is_adjacent(current, m) {
                continue;
            }
            min.x = min.x.min(m.work_origin_vdc.x);
            min.y = min.y.min(m.work_origin_vdc.y);
            max.x = max.x.max(m.work_right_vdc());
            max.y = max.y.max(m.work_bottom_vdc());
        }
        Self { min, max }
    }

    /// 点是否在范围内（半开区间）。
    #[must_use]
    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.min.x && p.x < self.max.x && p.y >= self.min.y && p.y < self.max.y
    }

    /// 范围内均匀随机取点（退化范围返回 min，不 panic）。
    pub fn sample(&self, rng: &mut SplitMix64) -> Vec2 {
        let dx = (self.max.x - self.min.x).max(0.0);
        let dy = (self.max.y - self.min.y).max(0.0);
        Vec2::new(self.min.x + rng.next_f32() * dx, self.min.y + rng.next_f32() * dy)
    }
}

/// 相邻判定容差（VDC 逻辑像素；容忍系统显示器排列时的微小间隙）。
const ADJACENT_GAP_TOL_PX: f32 = 2.0;

/// 两屏是否相邻：显示器矩形间隙（x/y 轴向间隙的欧氏组合，重叠则为 0）
/// 不超过 [`ADJACENT_GAP_TOL_PX`]——共享边界、角接触与微小排列间隙均算相邻，
/// 中间隔着大段空白的远屏不算。
fn is_adjacent(a: &MonitorGeom, b: &MonitorGeom) -> bool {
    let dx = (a.origin_vdc.x - b.right_vdc())
        .max(b.origin_vdc.x - a.right_vdc())
        .max(0.0);
    let dy = (a.origin_vdc.y - b.bottom_vdc())
        .max(b.origin_vdc.y - a.bottom_vdc())
        .max(0.0);
    (dx * dx + dy * dy).sqrt() <= ADJACENT_GAP_TOL_PX
}

/// 禁区（轴对齐矩形，VDC；半开区间）。
///
/// 语义占位：S2-M4 引擎默认传空列表（无禁区）；列表由上层维护并注入
/// （如后续 FR 扩展的「避开托盘区」等）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ForbiddenZone {
    /// 左上角（VDC）。
    pub min: Vec2,
    /// 右下角（VDC，不含）。
    pub max: Vec2,
}

impl ForbiddenZone {
    /// 构造禁区。
    #[must_use]
    pub const fn new(min: Vec2, max: Vec2) -> Self {
        Self { min, max }
    }

    /// 点是否在禁区内（半开区间）。
    #[must_use]
    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.min.x && p.x < self.max.x && p.y >= self.min.y && p.y < self.max.y
    }
}

// ---------------------------------------------------------------------------
// 播种 PRNG（splitmix64，零新增依赖）
// ---------------------------------------------------------------------------

/// 可播种 PRNG（splitmix64；C3 零新增依赖：不引入 rand/glam，测试可复现）。
///
/// u64 状态 → [`SplitMix64::next_f32`] 产出 f32 [0, 1)（取高 24 位，精度足够
/// 像素级采样且无 1.0 端点）。
#[derive(Clone, Debug)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// 由种子构造（任意 u64 皆可，含 0）。
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// 产出下一个 u64（splitmix64 标准混洗）。
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 产出 f32 [0, 1)（取 u64 高 24 位 / 2^24）。
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// 产出 f32 [min, max)（max ≤ min 时返回 min，防御不 panic）。
    #[must_use]
    pub fn next_range_f32(&mut self, min: f32, max: f32) -> f32 {
        if max <= min {
            return min;
        }
        min + self.next_f32() * (max - min)
    }
}

// ---------------------------------------------------------------------------
// 漫游采样器
// ---------------------------------------------------------------------------

/// 漫游决策采样器（`02 §5` K-3：`decide_target` 最多 [`MAX_SAMPLES`] 次采样）。
#[derive(Clone, Debug)]
pub struct RoamSampler {
    rng: SplitMix64,
}

impl RoamSampler {
    /// 由种子构造（测试可复现）。
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { rng: SplitMix64::new(seed) }
    }

    /// 决策一个漫游目标（决策顺序即 K-3 语义）：
    ///
    /// 1. 在 `region` 内随机取点；
    /// 2. `clamp_to_stand` 投影到站立面 → 非法站立点则重试；
    /// 3. 落在光标热区（`heat`）内 → 跳过重试；
    /// 4. 落在任一禁区（`forbidden`）内 → 跳过重试；
    /// 5. [`MAX_SAMPLES`] 次全失败 → `None`（上层回退 idle）。
    pub fn decide_target(
        &mut self,
        region: &RoamRegion,
        surfaces: &dyn StandSurface,
        heat: Option<CursorHeat>,
        forbidden: &[ForbiddenZone],
    ) -> Option<Vec2> {
        for _ in 0..MAX_SAMPLES {
            let raw = region.sample(&mut self.rng);
            let p = surfaces.clamp_to_stand(raw);
            if !surfaces.is_valid_stand(p) {
                continue;
            }
            if heat.is_some_and(|h| h.contains(p)) {
                continue;
            }
            if forbidden.iter().any(|z| z.contains(p)) {
                continue;
            }
            return Some(p);
        }
        None
    }
}

// ---------------------------------------------------------------------------
// 决策间隔（pace 缩放，纯计算）
// ---------------------------------------------------------------------------

/// 本次决策间隔（毫秒）。
///
/// 口径：在 `cfg.decision_interval_sec`（秒）内均匀取 base，再按 pace 缩放——
/// **pace 越大越活泼 → 间隔越短**（`base / pace`，`02 K-3` `roamPace` 语义，
/// 缩放方向在此显式声明）。防御：
///   - 区间端点倒置 → 自动交换；
///   - `pace` 非有限 / ≤ 0 → 按 1.0；
///   - 结果下限 1ms。
///
/// 区间端点相等时不消耗随机数流（确定性间隔，测试可直接断言）。
pub fn decision_interval_ms(rng: &mut SplitMix64, cfg: &RoamCfg) -> u64 {
    let lo = cfg.decision_interval_sec[0];
    let hi = cfg.decision_interval_sec[1];
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    let pace = if cfg.pace.is_finite() && cfg.pace > 0.0 { cfg.pace } else { 1.0 };
    let base_sec =
        if hi == lo { lo as f32 } else { rng.next_range_f32(lo as f32, hi as f32) };
    let scaled_ms = base_sec / pace * 1000.0;
    (scaled_ms.round().max(1.0)) as u64
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 主屏：VDC (0,0) 1920×1080，工作区底边 1040。
    fn mon_a() -> MonitorGeom {
        MonitorGeom {
            id: 1,
            origin_vdc: Vec2::new(0.0, 0.0),
            size_vdc: Vec2::new(1920.0, 1080.0),
            work_origin_vdc: Vec2::new(0.0, 0.0),
            work_size_vdc: Vec2::new(1920.0, 1040.0),
            primary: true,
        }
    }

    /// 左侧副屏：VDC x ∈ [-1920, 0)（负坐标口径）。
    fn mon_b() -> MonitorGeom {
        MonitorGeom {
            id: 2,
            origin_vdc: Vec2::new(-1920.0, 0.0),
            size_vdc: Vec2::new(1920.0, 1080.0),
            work_origin_vdc: Vec2::new(-1920.0, 0.0),
            work_size_vdc: Vec2::new(1920.0, 1040.0),
            primary: false,
        }
    }

    // -- SplitMix64 ------------------------------------------------------------

    #[test]
    fn splitmix64_same_seed_reproducible() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        for _ in 0..8 {
            assert_eq!(a.next_u64(), b.next_u64(), "同种子序列应逐位一致");
        }
        let mut c = SplitMix64::new(43);
        let mut differs = false;
        for _ in 0..8 {
            if a.next_u64() != c.next_u64() {
                differs = true;
                break;
            }
        }
        assert!(differs, "不同种子序列应不同");
    }

    #[test]
    fn splitmix64_f32_in_unit_range() {
        let mut rng = SplitMix64::new(7);
        for _ in 0..1000 {
            let v = rng.next_f32();
            assert!((0.0..1.0).contains(&v), "next_f32 应在 [0,1)：{v}");
        }
        // next_range_f32 区间与退化防御。
        assert_eq!(rng.next_range_f32(5.0, 5.0), 5.0);
        assert_eq!(rng.next_range_f32(9.0, 1.0), 9.0, "max ≤ min 返回 min");
        for _ in 0..100 {
            let v = rng.next_range_f32(-10.0, 10.0);
            assert!((-10.0..10.0).contains(&v));
        }
    }

    // -- 决策间隔（pace 缩放） ---------------------------------------------------

    fn interval_cfg(pace: f32) -> RoamCfg {
        RoamCfg {
            pace_options: crate::config::model::RoamCfg::default().pace_options,
            pace,
            decision_interval_sec: [10, 10],
            cursor_avoid_radius_px: 150,
            walk_speed_px_per_sec: 60.0,
        }
    }

    #[test]
    fn decision_interval_ms_pace_scales_interval_direction() {
        // pace 越大越活泼 → 间隔越短（base / pace）。
        assert_eq!(decision_interval_ms(&mut SplitMix64::new(1), &interval_cfg(1.0)), 10_000);
        assert_eq!(decision_interval_ms(&mut SplitMix64::new(1), &interval_cfg(2.0)), 5_000);
        assert_eq!(decision_interval_ms(&mut SplitMix64::new(1), &interval_cfg(0.5)), 20_000);
        // 防御：pace ≤ 0 / NaN → 按 1.0。
        assert_eq!(decision_interval_ms(&mut SplitMix64::new(1), &interval_cfg(0.0)), 10_000);
        assert_eq!(decision_interval_ms(&mut SplitMix64::new(1), &interval_cfg(f32::NAN)), 10_000);
    }

    #[test]
    fn decision_interval_ms_within_configured_range() {
        let cfg = RoamCfg { decision_interval_sec: [5, 30], ..interval_cfg(1.0) };
        let mut rng = SplitMix64::new(2024);
        for _ in 0..200 {
            let v = decision_interval_ms(&mut rng, &cfg);
            assert!((5_000..=30_000).contains(&v), "间隔应落在 [5000,30000]ms：{v}");
        }
        // 端点倒置 → 自动交换（[-? 不可能为负；交换 30,5]）。
        let swapped = RoamCfg { decision_interval_sec: [30, 5], ..cfg };
        for _ in 0..50 {
            let v = decision_interval_ms(&mut rng, &swapped);
            assert!((5_000..=30_000).contains(&v), "倒置区间应被交换：{v}");
        }
    }

    // -- DesktopFloor ---------------------------------------------------------

    #[test]
    fn desktop_floor_clamps_to_floor_line_and_x_range() {
        let floor = DesktopFloor::new(vec![mon_a()]);
        let p = floor.clamp_to_stand(Vec2::new(500.0, 900.0));
        assert!((p.y - 1040.0).abs() < 1e-6, "y 投影到工作区底边");
        assert!((p.x - 500.0).abs() < 1e-6, "x 在工作区内不变");
        // x 越界 → 钳制到工作区 x 范围。
        let clamped = floor.clamp_to_stand(Vec2::new(5000.0, 900.0));
        assert!((clamped.x - 1920.0).abs() < 1e-6, "x 钳制到工作区右缘");
        assert!(floor.is_valid_stand(clamped), "钳制点为合法站立点");
        // 底边线上的点合法，离线点非法。
        assert!(floor.is_valid_stand(Vec2::new(960.0, 1040.0)));
        assert!(!floor.is_valid_stand(Vec2::new(960.0, 900.0)));
    }

    #[test]
    fn desktop_floor_negative_vdc_left_monitor() {
        let floor = DesktopFloor::new(vec![mon_a(), mon_b()]);
        // 左副屏（负 x）底边站立合法。
        assert!(floor.is_valid_stand(Vec2::new(-1800.0, 1040.0)));
        assert!(!floor.is_valid_stand(Vec2::new(-1800.0, 900.0)));
        // 负坐标点钳制保持副屏地面线。
        let p = floor.clamp_to_stand(Vec2::new(-2500.0, 900.0));
        assert!((p.x + 1920.0).abs() < 1e-6, "x 钳制到左副屏工作区左缘");
        assert!((p.y - 1040.0).abs() < 1e-6);
    }

    #[test]
    fn desktop_floor_empty_is_never_valid_and_clamps_identity() {
        let floor = DesktopFloor::new(Vec::new());
        let p = Vec2::new(100.0, 200.0);
        assert!(!floor.is_valid_stand(p), "无显示器无合法站立点");
        assert_eq!(floor.clamp_to_stand(p), p, "无显示器钳制为恒等（防御）");
    }

    // -- RoamRegion -------------------------------------------------------------

    #[test]
    fn roam_region_union_includes_adjacent_monitors() {
        let region = RoamRegion::from_current_and_adjacent(&mon_a(), &[mon_a(), mon_b()]);
        // 当前屏 + 相邻左副屏 → 包围盒 x ∈ [-1920, 1920)。
        assert!((region.min.x + 1920.0).abs() < 1e-6);
        assert!((region.max.x - 1920.0).abs() < 1e-6);
        assert!(region.contains(Vec2::new(-1000.0, 500.0)));
        assert!(region.contains(Vec2::new(1000.0, 500.0)));
        // 不相邻的远屏不并入。
        let mut far = mon_b();
        far.origin_vdc = Vec2::new(-10_000.0, 0.0);
        let region = RoamRegion::from_current_and_adjacent(&mon_a(), &[mon_a(), far]);
        assert!((region.min.x - 0.0).abs() < 1e-6, "不相邻远屏不并入范围");
    }

    // -- RoamSampler（12 次采样语义） ---------------------------------------------

    #[test]
    fn roam_sampler_returns_valid_target_on_clear_floor() {
        let floor = DesktopFloor::new(vec![mon_a()]);
        let region = RoamRegion::from_monitor_work(&mon_a());
        let mut sampler = RoamSampler::new(11);
        let target = sampler
            .decide_target(&region, &floor, None, &[])
            .expect("空旷地面应决策出目标");
        assert!(floor.is_valid_stand(target), "目标为合法站立点");
        assert!((target.y - 1040.0).abs() < 1e-6, "目标落在工作区底边");
        assert!((0.0..1920.0).contains(&target.x));
    }

    #[test]
    fn roam_sampler_rejects_targets_in_cursor_heat() {
        let floor = DesktopFloor::new(vec![mon_a()]);
        let region = RoamRegion::from_monitor_work(&mon_a());
        // 热区覆盖部分底边（x ∈ [810, 1110) 附近被拒）。
        let heat = CursorHeat::new(Vec2::new(960.0, 1040.0), 150.0);
        let mut sampler = RoamSampler::new(7);
        for _ in 0..20 {
            let target = sampler
                .decide_target(&region, &floor, Some(heat), &[])
                .expect("热区外仍有大量合法点，12 次采样应命中");
            assert!(!heat.contains(target), "目标不得落在光标热区内");
        }
    }

    #[test]
    fn roam_sampler_returns_none_when_heat_covers_everything() {
        let floor = DesktopFloor::new(vec![mon_a()]);
        let region = RoamRegion::from_monitor_work(&mon_a());
        // 热区覆盖全屏底边 → 12 次全失败 → None（上层回退 idle）。
        let heat = CursorHeat::new(Vec2::new(960.0, 1040.0), 5000.0);
        let mut sampler = RoamSampler::new(7);
        assert!(
            sampler.decide_target(&region, &floor, Some(heat), &[]).is_none(),
            "热区全覆盖应返回 None"
        );
    }

    #[test]
    fn roam_sampler_rejects_forbidden_zones() {
        let floor = DesktopFloor::new(vec![mon_a()]);
        let region = RoamRegion::from_monitor_work(&mon_a());
        // 禁区覆盖整条底边带 → 全部被拒。
        let zone = ForbiddenZone::new(Vec2::new(0.0, 1000.0), Vec2::new(1920.0, 1100.0));
        let mut sampler = RoamSampler::new(7);
        assert!(sampler.decide_target(&region, &floor, None, &[zone]).is_none());

        // 禁区只覆盖左半 → 目标必在右半。
        let half = ForbiddenZone::new(Vec2::new(0.0, 1000.0), Vec2::new(960.0, 1100.0));
        for _ in 0..20 {
            let target = sampler
                .decide_target(&region, &floor, None, &[half])
                .expect("右半仍有合法点");
            assert!(!half.contains(target), "目标不得落在禁区内");
        }
    }

    #[test]
    fn roam_sampler_none_when_no_stand_surface() {
        let floor = DesktopFloor::new(Vec::new());
        let region = RoamRegion::from_monitor_work(&mon_a());
        let mut sampler = RoamSampler::new(7);
        assert!(
            sampler.decide_target(&region, &floor, None, &[]).is_none(),
            "无站立面 12 次采样全失败 → None"
        );
    }

    // -- MonitorGeom NDC 快照换算（FR-1-4 数据基础） -------------------------------

    #[test]
    fn monitor_geom_ndc_roundtrip() {
        let m = mon_a();
        let p = Vec2::new(960.0, 1040.0);
        let ndc = m.vdc_to_ndc(p);
        assert!((ndc.x - 0.5).abs() < 1e-6);
        assert!((ndc.y - 1040.0 / 1080.0).abs() < 1e-6);
        let back = m.ndc_to_vdc(ndc);
        assert!((back.x - p.x).abs() < 1e-4 && (back.y - p.y).abs() < 1e-4);
        // 零尺寸防御。
        let mut degenerate = mon_a();
        degenerate.size_vdc = Vec2::ZERO;
        let ndc = degenerate.vdc_to_ndc(p);
        assert!(ndc.x.is_finite() && ndc.y.is_finite(), "零尺寸不产生 NaN");
    }
}
