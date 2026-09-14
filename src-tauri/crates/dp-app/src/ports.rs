//! `dp-app/src/ports.rs` —— S3-M0 装配层适配器（端口注入 / 重复类型换算收口，F-05）。
//!
//! 职责（**仅装配层换算，零算法**）：
//!   1. **`WindowBand → PlatformBand`**（F-05）：感知侧标题栏带（`dp-core::perception`）
//!      与平台图输入带（`dp-core::motion::platform`）的**显式**换算，杜绝「字段同名
//!      直转」任一侧增删字段时静默错位。
//!      ⚠️ **孤儿规则裁定**：`WindowBand` 与 `PlatformBand` **均定义于 `dp-core`**、
//!      `From` 定义于 `std`，`dp-app` 侧**无本地类型**，故 Rust 孤儿规则（E0117）
//!      **禁止** `impl From<WindowBand> for PlatformBand`。此处以**显式自由函数**
//!      [`to_platform_band`] 承载同一「显式、集中、可单测」的收口语义（不改
//!      `dp-core`，符合任务红线「不因可见性/孤儿规则大改结构」）。此偏离已登记。
//!   2. **`MonitorGeom` 提供者**：`dp_platform::MonitorInfo` → `dp_core::motion::MonitorGeom`
//!      纯函数换算（公式依据 `dp-core/src/motion/roam.rs:34-46` 文档口径，逆 `vdc_to_spc`）。
//!   3. **反弹边界**：由权威 `pos` 所在屏**工作区** x 区间得 `HorizontalBounds`。
//!   4. **`PetBBoxHandle`**：`AtomicI32 × 4`（**物理像素**，非 VDC）无锁整数包含读，
//!      供 `app.manage` 与 S3-M1 钩子读取；`Clone`（内部 `Arc`）。**不塞进 `PetPlatform`**。
//!   5. **窗口枚举 → `WindowBand`**：`winenum::enumerate_titlebar_windows()`（物理 `RectI`）
//!      → 经 `DisplayService::physical_to_vdc` 换算为 VDC 带；taskbars 恒空（§6-2，
//!      `dp-platform` 无任务栏带枚举）。
//!
//! 架构约束：C1（无盘符字面量，路径走配置/资源解析）；C6/RV-17（坐标一律 VDC）；
//! 端口注入在此收口，`dp-core` 内**零** `use dp_platform`（本文件属 `dp-app`）。
//!
//! Windows 门控：仅依赖 `dp-platform/win/*` 的换算函数以 `#[cfg(windows)]` 门禁；
//! 纯逻辑换算（`WindowBand→PlatformBand` / `bounds_of` / `PetBBoxHandle` / `Vec2`）跨平台可用。

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use dp_core::motion::{HorizontalBounds, MonitorGeom, PlatformBand, PlatformInputs, Vec2 as CoreVec2};
use dp_core::perception::WindowBand;

// 平台侧最小载体（跨平台可用）：`dp_platform::traits::Vec2`。
use dp_platform::Vec2 as PlatformVec2;

// Windows 专有换算所需平台类型。
#[cfg(windows)]
use dp_platform::{DisplayService, MonitorInfo, RectI};

// ---------------------------------------------------------------------------
// 内核 ⇄ 平台 坐标/载体换算（C6/RV-17）
// ---------------------------------------------------------------------------

/// 平台 VDC 向量 → 内核 VDC 向量（字段同名，仍显式转换，F-05）。
#[must_use]
pub fn to_core_vec2(v: PlatformVec2) -> CoreVec2 {
    CoreVec2::new(v.x, v.y)
}

/// 内核 VDC 向量 → 平台 VDC 向量（写窗口 `set_position_vdc` 入口）。
#[must_use]
pub fn to_platform_vec2(v: CoreVec2) -> PlatformVec2 {
    PlatformVec2::new(v.x, v.y)
}

// ---------------------------------------------------------------------------
// F-05：WindowBand → PlatformBand（显式换算收口；孤儿规则见模块文档）
// ---------------------------------------------------------------------------

/// 感知侧标题栏/任务栏带 → 平台图输入带的**显式**换算（F-05）。
///
/// 字段逐一显式搬运（`left` / `top` / `right`），任一侧增删字段时编译期报错，
/// 杜绝 `..Default` / 结构体直转造成的静默错位。
#[must_use]
pub fn to_platform_band(band: WindowBand) -> PlatformBand {
    PlatformBand { left: band.left, top: band.top, right: band.right }
}

/// 由感知 `perception::PerceptionEvent::Windows` 的两个带列表组装 `PlatformInputs`。
#[must_use]
pub fn platform_inputs(titlebars: Vec<WindowBand>, taskbars: Vec<WindowBand>) -> PlatformInputs {
    PlatformInputs {
        titlebars: titlebars.into_iter().map(to_platform_band).collect(),
        taskbars: taskbars.into_iter().map(to_platform_band).collect(),
    }
}

// ---------------------------------------------------------------------------
// MonitorGeom 提供者（MonitorInfo → MonitorGeom，跨 RV-17 口径）
// ---------------------------------------------------------------------------

/// 平台显示器列表 → 内核显示器几何列表（纯函数，顺序保持）。
///
/// 换算公式（依据 `dp-core/src/motion/roam.rs` `MonitorGeom` 文档，逆 `vdc_to_spc`）：
/// ```text
/// size_vdc        = (rc_monitor.width()  / scale, rc_monitor.height() / scale)
/// work_origin_vdc = origin_vdc + ((rc_work.left − rc_monitor.left) / scale,
///                                 (rc_work.top  − rc_monitor.top ) / scale)
/// work_size_vdc   = (rc_work.width() / scale, rc_work.height() / scale)
/// id              = id.0
/// primary         = primary
/// ```
#[cfg(windows)]
#[must_use]
pub fn to_monitor_geoms(monitors: Vec<MonitorInfo>) -> Vec<MonitorGeom> {
    monitors.into_iter().map(monitor_geom).collect()
}

/// 单个显示器换算（scale ≤ 0 / 非有限 → 1.0 防御，避免除零 / NaN）。
#[cfg(windows)]
#[must_use]
fn monitor_geom(m: MonitorInfo) -> MonitorGeom {
    let scale = if m.scale.is_finite() && m.scale > 0.0 { m.scale } else { 1.0 };
    let size_vdc = CoreVec2::new(
        m.rc_monitor.width() as f32 / scale,
        m.rc_monitor.height() as f32 / scale,
    );
    let work_origin_vdc = CoreVec2::new(
        m.origin_vdc.x + (m.rc_work.left - m.rc_monitor.left) as f32 / scale,
        m.origin_vdc.y + (m.rc_work.top - m.rc_monitor.top) as f32 / scale,
    );
    let work_size_vdc = CoreVec2::new(
        m.rc_work.width() as f32 / scale,
        m.rc_work.height() as f32 / scale,
    );
    MonitorGeom {
        id: m.id.0,
        origin_vdc: CoreVec2::new(m.origin_vdc.x, m.origin_vdc.y),
        size_vdc,
        work_origin_vdc,
        work_size_vdc,
        primary: m.primary,
    }
}

// ---------------------------------------------------------------------------
// 反弹边界（pos 所在屏工作区 x 区间）
// ---------------------------------------------------------------------------

/// 由权威 `pos` 所在屏的**工作区** x 区间得物理反弹边界（`02 §5 K-3` `vd.left/right` 口径）。
///
/// 选择顺序：① 命中某屏工作区 x 半开区间；② 未命中 → 工作区中心 x 最近者；
/// ③ 无显示器（防御）→ 无界区间（`±∞`，物理侧边界比较恒不触发，不 panic）。
#[must_use]
pub fn bounds_of(monitors: &[MonitorGeom], pos: CoreVec2) -> HorizontalBounds {
    let hit = monitors
        .iter()
        .find(|m| pos.x >= m.work_origin_vdc.x && pos.x < m.work_right_vdc())
        .or_else(|| {
            monitors.iter().min_by(|a, b| {
                let da = (pos.x - a.work_center().x).abs();
                let db = (pos.x - b.work_center().x).abs();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
        });
    match hit {
        Some(m) => HorizontalBounds { left: m.work_origin_vdc.x, right: m.work_right_vdc() },
        None => HorizontalBounds { left: f32::NEG_INFINITY, right: f32::INFINITY },
    }
}

// ---------------------------------------------------------------------------
// 虚拟桌面包围几何（S3-M4 拖拽钳制 / 甩出落地约束）
// ---------------------------------------------------------------------------

/// 全部显示器工作区的**外包矩形** x 区间（S3-M4 拖拽跟手钳制用）。
///
/// 与 [`bounds_of`]（pos 所在屏口径，物理反弹边界）不同：拖拽允许跨屏移动，
/// 钳制口径取全虚拟桌面工作区并集的 min/max x（FR-4-6「不出屏」的跨屏宽口径）。
/// 无显示器 / 工作区全退化（防御）→ `±∞`（钳制退化为恒等，不 panic）。
#[must_use]
pub fn vd_bounds_of(monitors: &[MonitorGeom]) -> HorizontalBounds {
    let mut left = f32::INFINITY;
    let mut right = f32::NEG_INFINITY;
    for m in monitors {
        left = left.min(m.work_origin_vdc.x);
        right = right.max(m.work_right_vdc());
    }
    if left > right {
        return HorizontalBounds { left: f32::NEG_INFINITY, right: f32::INFINITY };
    }
    HorizontalBounds { left, right }
}

/// 全部显示器工作区的垂直跨度（`(min work_origin.y, max work_bottom)`）。
///
/// 拖拽 y 钳制（上下界）与甩出 1.5s 落地钳制（最大可落高度 h）的几何输入
/// （FR-4-6）。无显示器 / 工作区全退化（防御）→ `(−∞, +∞)`（钳制退化为恒等）。
#[must_use]
pub fn vd_vertical_span(monitors: &[MonitorGeom]) -> (f32, f32) {
    let mut top = f32::INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    for m in monitors {
        top = top.min(m.work_origin_vdc.y);
        bottom = bottom.max(m.work_bottom_vdc());
    }
    if top > bottom {
        return (f32::NEG_INFINITY, f32::INFINITY);
    }
    (top, bottom)
}

/// 速度矢量 DPI 换算（屏幕物理像素/秒 → VDC 逻辑像素/秒；C6/RV-17）。
///
/// 手势轨迹判定在物理像素域（`HookEvent` 透传，`gesture.rs` 边界登记），而
/// 物理引擎积分在 VDC 域——甩出初速度过桥时按释放点所在屏 DPI 因子除算
///（scale 由调用方从 `DisplayService::monitor_at(pos).scale` 取）。
/// `scale` 非有限 / ≤ 0 → 1.0 防御（同 `MonitorInfo::safe_scale` 口径）。
#[must_use]
pub fn scale_velocity(vel_px_per_sec: (i32, i32), scale: f32) -> CoreVec2 {
    let s = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    CoreVec2::new(vel_px_per_sec.0 as f32 / s, vel_px_per_sec.1 as f32 / s)
}

// ---------------------------------------------------------------------------
// PetBBoxHandle：物理像素 bbox 的无锁共享句柄（S3-M1 钩子「矩形包含」读点）
// ---------------------------------------------------------------------------

/// pet 窗口物理像素包围盒句柄（`AtomicI32 × 4`；**物理像素非 VDC**）。
///
/// - core-loop 每 render 档以 `window_physical_rect()` 刷新；
/// - S3-M1 全局钩子回调内做**无锁整数包含**读（[`PetBBoxHandle::contains`]），
///   满足 `02 §5 K-2` 回调 P99 < 50μs 的预算；
/// - `Clone`（内部 `Arc`）供 `app.manage` 与 core-loop 共享同一槽；
/// - **不**塞进 `PetPlatform`（与窗口封装解耦，S3-M2 换 `HIT_LATEST` 时只换数据源）。
///
/// # S3-M2 衔接（`HIT_LATEST` 双实现，裁定1/裁定3）
/// 本句柄仍是**矩形粗筛唯一真源**：[`HitLatestHandle`](crate::hit_latest::HitLatestHandle)
/// 组合（而非扩展）本句柄，在粗筛之上叠加帧掩码做真三态判定；本类型自身
/// **零改动零回归**，并继续作为掩码库未就绪 / 构建失败时的行为回退
/// （过粗筛即 Hit，=S3-M1 行为）。
#[derive(Clone)]
pub struct PetBBoxHandle {
    /// 四个分量的原子槽（relaxed：粗筛场景，无需顺序一致性）。
    inner: Arc<BBoxCells>,
}

/// bbox 四分量原子槽（左 / 上 / 右 / 下，物理像素）。
struct BBoxCells {
    left: AtomicI32,
    top: AtomicI32,
    right: AtomicI32,
    bottom: AtomicI32,
}

impl PetBBoxHandle {
    /// 构造全零 bbox（未刷新前 `contains` 恒 `false`）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(BBoxCells {
                left: AtomicI32::new(0),
                top: AtomicI32::new(0),
                right: AtomicI32::new(0),
                bottom: AtomicI32::new(0),
            }),
        }
    }

    /// 写入 bbox 四分量（物理像素；render 档调）。
    pub fn store(&self, left: i32, top: i32, right: i32, bottom: i32) {
        self.inner.left.store(left, Ordering::Relaxed);
        self.inner.top.store(top, Ordering::Relaxed);
        self.inner.right.store(right, Ordering::Relaxed);
        self.inner.bottom.store(bottom, Ordering::Relaxed);
    }

    /// 读取 bbox 四分量（`(left, top, right, bottom)`，物理像素）。
    #[must_use]
    pub fn load(&self) -> (i32, i32, i32, i32) {
        (
            self.inner.left.load(Ordering::Relaxed),
            self.inner.top.load(Ordering::Relaxed),
            self.inner.right.load(Ordering::Relaxed),
            self.inner.bottom.load(Ordering::Relaxed),
        )
    }

    /// 物理点是否落在 bbox 内（半开区间 `[left, right) × [top, bottom)`，无锁整数比较）。
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.inner.left.load(Ordering::Relaxed)
            && x < self.inner.right.load(Ordering::Relaxed)
            && y >= self.inner.top.load(Ordering::Relaxed)
            && y < self.inner.bottom.load(Ordering::Relaxed)
    }
}

impl Default for PetBBoxHandle {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// PetBBoxHandle → dp-platform 钩子命中端口（S3-M1，设计补充 §2.2「端口在 platform、数据在 app」）
// ---------------------------------------------------------------------------

/// `dp-platform` 只读命中端口 [`dp_platform::win::hook::HitTest`] 的实现。
///
/// **唯一真源仍在 `dp-app`**：数据（4×`AtomicI32`）与写者（core-loop render 档）只在此；
/// `dp-platform` 仅持只读 trait 对象（`Arc<dyn HitTest>`），**不新建第二份矩形、不反向依赖**。
/// 判定式与既有 [`PetBBoxHandle::contains`] **逐位一致**（半开区间，物理像素）。
///
/// S3-M2 落地（裁定1/裁定3 + 主理人修正）：装配点已改注入
/// `hit_latest::HitLatestHandle`（覆写 [`dp_platform::win::hook::HitTest::outcome`]
/// 返回真三态 Hit/Hover/Miss）；本 `impl` **不覆写** `outcome`——默认实现退化为
/// `contains ? Hit : Miss`，故本类型**零改动零回归**，作为「掩码库未就绪 / 实现
/// 切换」的保底实现保留：钩子侧三态路由代码对两实现完全一致（切换 `HitSource`
/// 实现，事件路由行为一致，S3-M2 卡片验收）。
#[cfg(windows)]
impl dp_platform::win::hook::HitTest for PetBBoxHandle {
    fn contains(&self, x: i32, y: i32) -> bool {
        // 显式 UFCS 调用固有方法（与 trait 方法同名；固有方法优先，语义等价）。
        PetBBoxHandle::contains(self, x, y)
    }
}

// ---------------------------------------------------------------------------
// 窗口枚举 → WindowBand（VDC；taskbars 恒空，§6-2）
// ---------------------------------------------------------------------------

/// 枚举可见窗口标题栏带并换算为 VDC 的 [`WindowBand`] 列表（供 `PerceptionEvent::Windows`）。
///
/// 返回 `(titlebars, taskbars)`；`taskbars` **恒为空**（§6-2：`dp-platform` 无任务栏带枚举，
/// 且 `winenum` 过滤 `Shell_TrayWnd`）。站立仍靠 `PlatformGraph` 的 `DesktopBottom`
/// （工作区底边，已扣任务栏）。
///
/// 单窗换算失败（零尺寸 / 非法坐标）→ 跳过该窗，不 panic（`02 §7.4.2`）。
#[cfg(windows)]
#[must_use]
pub fn enumerate_window_bands(display: &DisplayService) -> (Vec<WindowBand>, Vec<WindowBand>) {
    let mut titlebars = Vec::new();
    for w in dp_platform::win::winenum::enumerate_titlebar_windows() {
        if let Some(band) = rect_to_band(display, w.rect) {
            titlebars.push(band);
        }
    }
    (titlebars, Vec::new())
}

/// 物理窗口矩形 → VDC 站立带（`top` = 窗口顶边 y；左右取顶边两端 x，防负宽）。
#[cfg(windows)]
#[must_use]
fn rect_to_band(display: &DisplayService, rect: RectI) -> Option<WindowBand> {
    if rect.width() <= 0 || rect.height() <= 0 {
        return None;
    }
    let tl = to_core_vec2(display.physical_to_vdc(rect.left, rect.top));
    let tr = to_core_vec2(display.physical_to_vdc(rect.right, rect.top));
    let (left, right) = if tl.x <= tr.x { (tl.x, tr.x) } else { (tr.x, tl.x) };
    if !left.is_finite() || !right.is_finite() || !tl.y.is_finite() || right <= left {
        return None;
    }
    Some(WindowBand { left, top: tl.y, right })
}

// ---------------------------------------------------------------------------
// 电量状态换算（dp-platform → dp-core；孤儿规则同 §F-05，用自由函数）
// ---------------------------------------------------------------------------

/// 平台电量状态 → 内核感知电量状态（字段逐一显式搬运，F-05）。
#[cfg(windows)]
#[must_use]
pub fn to_core_battery(b: dp_platform::win::system::BatteryState) -> dp_core::perception::BatteryState {
    dp_core::perception::BatteryState { percent: b.percent, charging: b.charging }
}

// ---------------------------------------------------------------------------
// 单元测试（纯逻辑换算；窗口相关换算另由 coreloop 冒烟覆盖）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- WindowBand → PlatformBand（F-05 显式换算） -------------------------------

    #[test]
    fn window_band_to_platform_band_is_field_exact() {
        let band = WindowBand { left: 400.0, top: 300.0, right: 1520.0 };
        let mapped = to_platform_band(band);
        assert!((mapped.left - 400.0).abs() < f32::EPSILON);
        assert!((mapped.top - 300.0).abs() < f32::EPSILON);
        assert!((mapped.right - 1520.0).abs() < f32::EPSILON);
        // 反向重建（往返）：以 mapped 还原 WindowBand 字段逐位一致。
        let back = WindowBand { left: mapped.left, top: mapped.top, right: mapped.right };
        assert_eq!(back, band, "字段往返应逐位一致");
    }

    #[test]
    fn platform_inputs_maps_both_lists_and_defaults_taskbars_empty() {
        let inputs = platform_inputs(
            vec![WindowBand { left: 0.0, top: 10.0, right: 100.0 }],
            Vec::new(),
        );
        assert_eq!(inputs.titlebars.len(), 1);
        assert!((inputs.titlebars[0].top - 10.0).abs() < f32::EPSILON);
        assert!(inputs.taskbars.is_empty(), "taskbars 恒空（§6-2）");
    }

    // -- Vec2 换算 ---------------------------------------------------------------

    #[test]
    fn vec2_roundtrip_between_core_and_platform() {
        let core = CoreVec2::new(-12.5, 340.0);
        let plat = to_platform_vec2(core);
        let back = to_core_vec2(plat);
        assert_eq!(back, core);
    }

    // -- bounds_of（工作区 x 区间选择） ------------------------------------------

    /// 主屏：VDC (0,0) 1920×1080，工作区 x ∈ [0, 1920)。
    fn mon_a() -> MonitorGeom {
        MonitorGeom {
            id: 1,
            origin_vdc: CoreVec2::new(0.0, 0.0),
            size_vdc: CoreVec2::new(1920.0, 1080.0),
            work_origin_vdc: CoreVec2::new(0.0, 0.0),
            work_size_vdc: CoreVec2::new(1920.0, 1040.0),
            primary: true,
        }
    }

    /// 左侧副屏：VDC x ∈ [-1920, 0)，工作区同宽。
    fn mon_b() -> MonitorGeom {
        MonitorGeom {
            id: 2,
            origin_vdc: CoreVec2::new(-1920.0, 0.0),
            size_vdc: CoreVec2::new(1920.0, 1080.0),
            work_origin_vdc: CoreVec2::new(-1920.0, 0.0),
            work_size_vdc: CoreVec2::new(1920.0, 1040.0),
            primary: false,
        }
    }

    /// 更低的屏（工作区底边 2000；与 coreloop.rs 测试夹具同构，跨屏跨度用）。
    fn mon_low() -> MonitorGeom {
        MonitorGeom {
            id: 3,
            origin_vdc: CoreVec2::new(0.0, 0.0),
            size_vdc: CoreVec2::new(1920.0, 2400.0),
            work_origin_vdc: CoreVec2::new(0.0, 0.0),
            work_size_vdc: CoreVec2::new(1920.0, 2000.0),
            primary: false,
        }
    }

    #[test]
    fn bounds_of_uses_monitor_of_pos() {
        let monitors = vec![mon_a(), mon_b()];
        let main = bounds_of(&monitors, CoreVec2::new(500.0, 1000.0));
        assert!((main.left - 0.0).abs() < f32::EPSILON && (main.right - 1920.0).abs() < f32::EPSILON);
        let left = bounds_of(&monitors, CoreVec2::new(-500.0, 1000.0));
        assert!((left.left + 1920.0).abs() < f32::EPSILON && (left.right - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn bounds_of_outside_all_screens_snaps_to_nearest_work() {
        // 远在右侧（超出主屏）→ 取中心 x 最近的主屏工作区。
        let monitors = vec![mon_a(), mon_b()];
        let b = bounds_of(&monitors, CoreVec2::new(5000.0, 500.0));
        assert!((b.left - 0.0).abs() < f32::EPSILON && (b.right - 1920.0).abs() < f32::EPSILON);
    }

    #[test]
    fn bounds_of_no_monitors_is_unbounded() {
        let b = bounds_of(&[], CoreVec2::new(10.0, 10.0));
        assert!(b.left == f32::NEG_INFINITY && b.right == f32::INFINITY);
    }

    // -- S3-M4：虚拟桌面包围几何（拖拽钳制 / 甩出落地约束） ----------------------

    #[test]
    fn vd_bounds_union_across_monitors_and_empty_defense() {
        // 双屏外包矩形：左界 = 最左屏工作区左缘（-1920），右界 = 最右屏右缘（1920）。
        let b = vd_bounds_of(&[mon_a(), mon_b()]);
        assert!((b.left + 1920.0).abs() < f32::EPSILON, "外包矩形左界：{}", b.left);
        assert!((b.right - 1920.0).abs() < f32::EPSILON, "外包矩形右界：{}", b.right);
        // 与 bounds_of（所在屏口径）区分：外包矩形覆盖双屏并集而非单屏区间。
        let single = bounds_of(&[mon_a(), mon_b()], CoreVec2::new(-500.0, 100.0));
        assert!(single.right < b.right, "所在屏口径 ≠ 外包矩形口径");
        // 无显示器 → ±∞ 防御（钳制退化为恒等）。
        let empty = vd_bounds_of(&[]);
        assert!(empty.left == f32::NEG_INFINITY && empty.right == f32::INFINITY);
    }

    #[test]
    fn vd_vertical_span_takes_extremes_and_empty_defense() {
        // 跨屏跨度：顶 = 最高工作区顶（0），底 = 最低工作区底边（2000）。
        let (top, bottom) = vd_vertical_span(&[mon_a(), mon_low()]);
        assert!((top - 0.0).abs() < f32::EPSILON);
        assert!((bottom - 2000.0).abs() < f32::EPSILON, "跨屏取最低工作区底边：{bottom}");
        // 单屏口径同值。
        let (top, bottom) = vd_vertical_span(&[mon_a()]);
        assert!((top - 0.0).abs() < f32::EPSILON && (bottom - 1040.0).abs() < f32::EPSILON);
        // 无显示器 → (−∞, +∞) 防御。
        let (top, bottom) = vd_vertical_span(&[]);
        assert!(top == f32::NEG_INFINITY && bottom == f32::INFINITY);
    }

    #[test]
    fn scale_velocity_divides_by_dpi_and_defends_bad_scale() {
        // 正常 DPI：物理 px/s ÷ scale = VDC px/s（y 分量符号保留）。
        let v = scale_velocity((1_200, -600), 1.5);
        assert!((v.x - 800.0).abs() < 1e-4 && (v.y + 400.0).abs() < 1e-4, "1200/1.5=800");
        // scale ≤ 0 / NaN → 1.0 防御（同 MonitorInfo::safe_scale 口径）。
        let v = scale_velocity((100, -50), 0.0);
        assert!((v.x - 100.0).abs() < 1e-4 && (v.y + 50.0).abs() < 1e-4, "scale=0 → 1.0");
        let v = scale_velocity((100, -50), f32::NAN);
        assert!((v.x - 100.0).abs() < 1e-4 && (v.y + 50.0).abs() < 1e-4, "NaN → 1.0");
        // 1.0 缩放（100% DPI）恒等。
        let v = scale_velocity((2_000, 0), 1.0);
        assert!((v.x - 2_000.0).abs() < 1e-4 && v.y == 0.0);
    }

    // -- PetBBoxHandle（读写 / 包含半开） ----------------------------------------

    #[test]
    fn pet_bbox_handle_read_write_and_contains() {
        let h = PetBBoxHandle::new();
        assert_eq!(h.load(), (0, 0, 0, 0), "初始全零");
        assert!(!h.contains(0, 0), "未刷新前恒 false");
        h.store(100, 200, 356, 456);
        assert_eq!(h.load(), (100, 200, 356, 456));
        assert!(h.contains(100, 200), "左上闭");
        assert!(h.contains(355, 455), "右下减一闭");
        assert!(!h.contains(356, 455), "右开");
        assert!(!h.contains(355, 456), "下开");
        assert!(!h.contains(99, 200));
    }

    #[test]
    fn pet_bbox_handle_clone_shares_slot() {
        let h = PetBBoxHandle::new();
        let clone = h.clone();
        h.store(1, 2, 3, 4);
        assert_eq!(clone.load(), (1, 2, 3, 4), "克隆共享同一原子槽");
    }
}
