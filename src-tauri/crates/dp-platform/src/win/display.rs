//! Windows 显示器 / DPI 服务（`02 §7.2` RV-17 · 多显示器）。
//!
//! 提供 `virtualOrigin(monitor)` 的实装：每个显示器的 `origin_vdc` / `scale` /
//! `rc_monitor` / 工作区 / `id`，并提供 `monitor_at(vdc)`。坐标换算**含显示器原点偏移**：
//!
//! ```text
//! SPC = (VDC − origin_vdc(m)) × scale(m)
//! VDC = origin_vdc(m) + SPC / scale(m)
//! w_px = logical_w × userScale × scale(m)
//! ```
//!
//! 硬约束（`02 §7.2`）：**禁止**硬编码 1920/1080；**禁止**用「虚拟桌面全局原点」
//! 代替「显示器原点」。显示器 / DPI 换算部分有纯函数单元测试覆盖（跨屏、负坐标、非 100% 缩放）。
//!
//! 显示器变更（`WM_DISPLAYCHANGE / WM_SETTINGCHANGE / WM_DPICHANGED`）由上层
//! （S1-M4 supervisor / 消息循环）收到消息后调用 [`DisplayService::refresh`] 重建缓存；
//! 本模块只提供可调用能力，不自行挂消息钩子。

use std::sync::Mutex;

use crate::traits::{Result, Vec2};

/// 显示器标识。
///
/// 当前实现取 Win32 `HMONITOR` 句柄值，**仅用于本次进程会话内的索引**（跨进程不稳定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MonitorId(pub u64);

/// 整型矩形（物理像素）。
///
/// Win32 `RECT` 的平台无关镜像，便于纯函数单测（`RECT` 依赖平台，无法在测试里直接构造语义断言）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RectI {
    /// 左边界（含）。
    pub left: i32,
    /// 上边界（含）。
    pub top: i32,
    /// 右边界（不含）。
    pub right: i32,
    /// 下边界（不含）。
    pub bottom: i32,
}

impl RectI {
    /// 构造矩形。
    #[inline]
    #[must_use]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }

    /// 宽度（像素）。
    #[inline]
    #[must_use]
    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    /// 高度（像素）。
    #[inline]
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }

    /// 是否包含物理点（半开区间 `[left, right) × [top, bottom)`）。
    #[inline]
    #[must_use]
    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// 中心点。
    #[inline]
    #[must_use]
    pub fn center(&self) -> Vec2 {
        Vec2::new(
            (self.left + self.right) as f32 / 2.0,
            (self.top + self.bottom) as f32 / 2.0,
        )
    }
}

/// 单个显示器的平台无关描述（`virtualOrigin` 提供者的对外结构）。
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorInfo {
    /// 显示器标识。
    pub id: MonitorId,
    /// 显示器左上角在 VDC（96-dpi 逻辑像素）中的坐标，可为负。
    pub origin_vdc: Vec2,
    /// DPI 缩放（`dpiX / 96.0`）。
    pub scale: f32,
    /// 显示器矩形（虚拟桌面物理像素）。
    pub rc_monitor: RectI,
    /// 工作区矩形（扣除任务栏等）。
    pub rc_work: RectI,
    /// 设备名（如 `\\.\DISPLAY1`）。
    pub device_id: String,
    /// 是否主显示器。
    pub primary: bool,
}

impl MonitorInfo {
    /// 安全缩放（防御 scale ≤ 0 的异常输入，避免除零 / NaN）。
    #[inline]
    fn safe_scale(&self) -> f32 {
        if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        }
    }

    /// 显示器在 VDC 中的宽度（逻辑像素）。
    #[inline]
    #[must_use]
    pub fn vdc_width(&self) -> f32 {
        self.rc_monitor.width() as f32 / self.safe_scale()
    }

    /// 显示器在 VDC 中的高度（逻辑像素）。
    #[inline]
    #[must_use]
    pub fn vdc_height(&self) -> f32 {
        self.rc_monitor.height() as f32 / self.safe_scale()
    }

    /// VDC → SPC（所在显示器物理左上，物理像素）。**含 RV-17 原点偏移**。
    #[inline]
    #[must_use]
    pub fn vdc_to_spc(&self, vdc: Vec2) -> Vec2 {
        let s = self.safe_scale();
        Vec2::new((vdc.x - self.origin_vdc.x) * s, (vdc.y - self.origin_vdc.y) * s)
    }

    /// SPC → VDC（`vdc_to_spc` 的逆换算）。
    #[inline]
    #[must_use]
    pub fn spc_to_vdc(&self, spc: Vec2) -> Vec2 {
        let s = self.safe_scale();
        Vec2::new(self.origin_vdc.x + spc.x / s, self.origin_vdc.y + spc.y / s)
    }

    /// VDC → 虚拟桌面物理像素坐标（写 `SetWindowPos` 用）。
    ///
    /// `物理 = rc_monitor.left/top + SPC`：SPC 是「显示器本地」坐标，需再加显示器物理原点。
    #[inline]
    #[must_use]
    pub fn vdc_to_physical(&self, vdc: Vec2) -> (i32, i32) {
        let spc = self.vdc_to_spc(vdc);
        (
            self.rc_monitor.left + spc.x.round() as i32,
            self.rc_monitor.top + spc.y.round() as i32,
        )
    }

    /// 虚拟桌面物理像素坐标 → VDC。
    #[inline]
    #[must_use]
    pub fn physical_to_vdc(&self, x: i32, y: i32) -> Vec2 {
        let spc = Vec2::new(
            (x - self.rc_monitor.left) as f32,
            (y - self.rc_monitor.top) as f32,
        );
        self.spc_to_vdc(spc)
    }

    /// VDC 点是否落在本显示器（半开区间）。
    #[inline]
    #[must_use]
    pub fn contains_vdc(&self, vdc: Vec2) -> bool {
        let right = self.origin_vdc.x + self.vdc_width();
        let bottom = self.origin_vdc.y + self.vdc_height();
        vdc.x >= self.origin_vdc.x && vdc.x < right && vdc.y >= self.origin_vdc.y && vdc.y < bottom
    }

    /// 逻辑尺寸 → 物理尺寸（`w_px = logical × userScale × scale`，向上取整且至少 1px）。
    #[inline]
    #[must_use]
    pub fn size_logical_to_px(&self, logical: u32, user_scale: f32) -> u32 {
        let user = if user_scale.is_finite() && user_scale > 0.0 { user_scale } else { 1.0 };
        let px = logical as f32 * user * self.safe_scale();
        px.round().max(1.0) as u32
    }
}

/// 在显示器列表中为 VDC 点选择显示器。
///
/// 规则：① 命中（半开区间）优先；② 未命中时取 VDC 中心最近者（拔屏 / 越界场景钳制）；
/// ③ 列表为空返回 `None`（调用方再钳制到合成主屏）。
#[must_use]
pub fn select_monitor_at(monitors: &[MonitorInfo], vdc: Vec2) -> Option<&MonitorInfo> {
    if monitors.is_empty() {
        return None;
    }
    if let Some(hit) = monitors.iter().find(|m| m.contains_vdc(vdc)) {
        return Some(hit);
    }
    // 未命中：取 VDC 中心欧氏距离最近者。
    monitors.iter().min_by(|a, b| {
        let da = distance_sq_to(a, vdc);
        let db = distance_sq_to(b, vdc);
        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// VDC 点到显示器（VDC 空间）中心的平方距离。
fn distance_sq_to(m: &MonitorInfo, vdc: Vec2) -> f32 {
    let cx = m.origin_vdc.x + m.vdc_width() / 2.0;
    let cy = m.origin_vdc.y + m.vdc_height() / 2.0;
    let dx = vdc.x - cx;
    let dy = vdc.y - cy;
    dx * dx + dy * dy
}

/// 合成主屏（显示器列表为空时的最后兜底；「显示器异常 → 钳制主屏」，`02 §7.4.2`）。
#[must_use]
fn synthetic_primary() -> MonitorInfo {
    MonitorInfo {
        id: MonitorId(0),
        origin_vdc: Vec2::ZERO,
        scale: 1.0,
        rc_monitor: RectI::default(),
        rc_work: RectI::default(),
        device_id: String::new(),
        primary: true,
    }
}

/// 显示器 / DPI 服务：`virtualOrigin(monitor)` 提供者（`02 §7.2`）。
///
/// 内部缓存可经 [`DisplayService::refresh`] 重建（供 `WM_DISPLAYCHANGE / WM_DPICHANGED` 消息路径调用）。
pub struct DisplayService {
    monitors: Mutex<Vec<MonitorInfo>>,
}

impl DisplayService {
    /// 空服务（无显示器缓存；`monitor_at` 会钳制到合成主屏）。
    #[must_use]
    pub fn empty() -> Self {
        Self { monitors: Mutex::new(Vec::new()) }
    }

    /// 由给定显示器列表构造（**单测 / 注入**用）。
    #[must_use]
    pub fn from_monitors(monitors: Vec<MonitorInfo>) -> Self {
        Self { monitors: Mutex::new(monitors) }
    }

    /// 探测本机显示器（失败降级为空，不 panic：`02 §7.4.2`）。
    #[must_use]
    pub fn detect() -> Self {
        let service = Self::empty();
        if let Err(err) = service.refresh() {
            // 降级：保留空缓存，monitor_at 会钳制到合成主屏。
            eprintln!("[dp-platform] 显示器探测降级：{err}");
        }
        service
    }

    /// 重建显示器缓存（`WM_DISPLAYCHANGE / WM_SETTINGCHANGE / WM_DPICHANGED` 后调用）。
    pub fn refresh(&self) -> Result<()> {
        let list = imp::enumerate_monitors()?;
        let mut guard = self.monitors.lock().unwrap_or_else(|e| e.into_inner());
        *guard = list;
        Ok(())
    }

    /// 当前显示器列表快照。
    #[must_use]
    pub fn monitors(&self) -> Vec<MonitorInfo> {
        self.monitors.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 按显示器标识查找。
    #[must_use]
    pub fn monitor_by_id(&self, id: MonitorId) -> Option<MonitorInfo> {
        let list = self.monitors();
        list.into_iter().find(|m| m.id == id)
    }

    /// 选择 VDC 点所在的显示器（`02 §7.2`：`display.monitor_at(vdc)`）。
    ///
    /// 永不失败：列表为空 / 越界时钳制到主屏或最近显示器。
    #[must_use]
    pub fn monitor_at(&self, vdc: Vec2) -> MonitorInfo {
        let list = self.monitors();
        select_monitor_at(&list, vdc).cloned().unwrap_or_else(synthetic_primary)
    }

    /// 主显示器（无主标记时取第一个）。
    #[must_use]
    pub fn primary(&self) -> Option<MonitorInfo> {
        let list = self.monitors();
        list.iter()
            .find(|m| m.primary)
            .or_else(|| list.first())
            .cloned()
    }

    /// VDC → 虚拟桌面物理像素（写窗口用）。
    #[must_use]
    pub fn vdc_to_physical(&self, vdc: Vec2) -> (i32, i32) {
        self.monitor_at(vdc).vdc_to_physical(vdc)
    }

    /// 虚拟桌面物理像素 → VDC。
    #[must_use]
    pub fn physical_to_vdc(&self, x: i32, y: i32) -> Vec2 {
        let list = self.monitors();
        let found = list
            .iter()
            .find(|m| m.rc_monitor.contains(x, y))
            .or_else(|| {
                list.iter().min_by(|a, b| {
                    let da = distance_sq_to_physical(a, x, y);
                    let db = distance_sq_to_physical(b, x, y);
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
            });
        match found {
            Some(m) => m.physical_to_vdc(x, y),
            None => Vec2::new(x as f32, y as f32),
        }
    }
}

impl Default for DisplayService {
    fn default() -> Self {
        Self::detect()
    }
}

/// 物理点到显示器物理矩形中心的平方距离。
fn distance_sq_to_physical(m: &MonitorInfo, x: i32, y: i32) -> f32 {
    let c = m.rc_monitor.center();
    let dx = x as f32 - c.x;
    let dy = y as f32 - c.y;
    dx * dx + dy * dy
}

// ---------------------------------------------------------------------------
// Windows 实装
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{GetLastError, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    use super::{MonitorId, MonitorInfo, RectI};
    use crate::traits::{PlatformError, Result, Vec2};

    /// `MONITORINFOF_PRIMARY`（windows crate 未导出该常量，按 Win32 SDK 定义补）。
    const MONITORINFOF_PRIMARY: u32 = 0x0000_0001;

    /// 枚举回调上下文：以裸指针经 `LPARAM` 传入，仅在同步回调期间存活。
    struct EnumCtx {
        list: Vec<MonitorInfo>,
    }

    struct PendingMonitor {
        id: MonitorId,
        rc_monitor: RectI,
        rc_work: RectI,
        device_id: String,
        primary: bool,
        dpi_x: u32,
    }

    /// 枚举本机所有显示器（虚拟桌面物理像素；含负坐标副屏）。
    pub(super) fn enumerate_monitors() -> Result<Vec<MonitorInfo>> {
        let mut ctx = EnumCtx { list: Vec::new() };
        let ctx_ptr: *mut EnumCtx = &mut ctx;

        // 不变量：LPARAM 承载 ctx_ptr；`EnumDisplayMonitors` 同步回调期间 ctx 存活，
        // 回调只做同步 push、不外泄该指针。故此处裸指针传递在其生命周期内有效。
        let called = unsafe {
            EnumDisplayMonitors(None, None, Some(monitor_enum_proc), LPARAM(ctx_ptr as isize))
        };
        if !called.as_bool() {
            return Err(last_error("EnumDisplayMonitors"));
        }

        // 主屏兜底标记：若系统未标注任何 primary，则取第一个。
        if !ctx.list.iter().any(|m| m.primary) {
            if let Some(first) = ctx.list.first_mut() {
                first.primary = true;
            }
        }
        Ok(ctx.list)
    }

    /// 查询单个 `HMONITOR` 的显示器信息（供窗口层的全屏判定复用）。
    pub(crate) fn query_monitor(hmon: HMONITOR) -> Option<MonitorInfo> {
        let pending = unsafe { read_monitor(hmon) }?;
        Some(pending.into_info())
    }

    unsafe extern "system" fn monitor_enum_proc(
        hmon: HMONITOR,
        _hdc: HDC,
        _clip: *mut RECT,
        lparam: LPARAM,
    ) -> windows::core::BOOL {
        // 不变量：lparam 由 `enumerate_monitors` 传入，指向存活中的 `EnumCtx`。
        let ctx = unsafe { &mut *(lparam.0 as *mut EnumCtx) };
        if let Some(pending) = unsafe { read_monitor(hmon) } {
            ctx.list.push(pending.into_info());
        }
        // 始终返回 TRUE：单个显示器读取失败不应中断整体枚举。
        windows::core::BOOL(1)
    }

    /// 读取一个显示器的原始信息（读取失败返回 `None`，不中断枚举）。
    unsafe fn read_monitor(hmon: HMONITOR) -> Option<PendingMonitor> {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = core::mem::size_of::<MONITORINFOEXW>() as u32;
        // 不变量：`MONITORINFOEXW` 首字段即 `MONITORINFO`，将其地址按 `*mut MONITORINFO`
        // 传入 `GetMonitorInfoW` 是 Win32 官方规定的用法（cbSize 已置为 EXW 大小以取设备名）。
        let got = unsafe { GetMonitorInfoW(hmon, &mut mi.monitorInfo as *mut MONITORINFO) };
        if !got.as_bool() {
            return None;
        }
        let rc = mi.monitorInfo.rcMonitor;
        let work = mi.monitorInfo.rcWork;

        let mut dpi_x: u32 = 96;
        let mut dpi_y: u32 = 96;
        let dpi_ok =
            unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_ok();

        Some(PendingMonitor {
            id: MonitorId(hmon.0 as u64),
            rc_monitor: RectI::new(rc.left, rc.top, rc.right, rc.bottom),
            rc_work: RectI::new(work.left, work.top, work.right, work.bottom),
            device_id: wide_to_string(&mi.szDevice),
            primary: mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            dpi_x: if dpi_ok && dpi_x > 0 { dpi_x } else { 96 },
        })
    }

    impl PendingMonitor {
        /// 补出 `origin_vdc`（= 物理左上 / scale）与 scale 后转为对外结构。
        ///
        /// 说明：VDC 定义为「虚拟桌面左上、96-dpi 逻辑像素」。以 `rcMonitor` 物理左上
        /// 除以本屏 scale 得到该屏原点在 VDC 中的位置（主屏物理 (0,0) 恒映射到 VDC (0,0)），
        /// 与 `02 §7.2` 的 `SPC ↔ VDC` 公式互为逆运算。
        fn into_info(self) -> MonitorInfo {
            let scale = self.dpi_x as f32 / 96.0;
            let safe = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
            let origin_vdc = Vec2::new(
                self.rc_monitor.left as f32 / safe,
                self.rc_monitor.top as f32 / safe,
            );
            MonitorInfo {
                id: self.id,
                origin_vdc,
                scale: safe,
                rc_monitor: self.rc_monitor,
                rc_work: self.rc_work,
                device_id: self.device_id,
                primary: self.primary,
            }
        }
    }

    /// UTF-16 缓冲区 → `String`（在首个 NUL 处截断）。
    fn wide_to_string(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    /// 构造「Win32 调用失败」错误（取当前线程 `GetLastError`）。
    fn last_error(op: &'static str) -> PlatformError {
        // 不变量：`GetLastError` 无副作用，仅在紧随失败调用后读取才有意义。
        let code = unsafe { GetLastError() }.0;
        PlatformError::Win32 { op, code }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::MonitorInfo;
    use crate::traits::{PlatformError, Result};

    /// 非 Windows 目标不提供显示器枚举（本项目仅 Windows）。
    pub(super) fn enumerate_monitors() -> Result<Vec<MonitorInfo>> {
        Err(PlatformError::NoMonitor)
    }
}

/// 供窗口层（全屏判定）复用的单显示器查询。
#[cfg(windows)]
pub(crate) use imp::query_monitor;

// ---------------------------------------------------------------------------
// 单元测试（RV-17 换算 + 多屏选择）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 主屏：1920×1080 @1.5（物理），VDC 尺寸 = 1280×720。
    fn primary() -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(1),
            origin_vdc: Vec2::new(0.0, 0.0),
            scale: 1.5,
            rc_monitor: RectI::new(0, 0, 1920, 1080),
            rc_work: RectI::new(0, 0, 1920, 1040),
            device_id: "\\\\.\\DISPLAY1".to_string(),
            primary: true,
        }
    }

    /// 左侧副屏：1920×1080 @1.0，物理 x ∈ [-1920, 0)。
    fn left() -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(2),
            origin_vdc: Vec2::new(-1920.0, 0.0),
            scale: 1.0,
            rc_monitor: RectI::new(-1920, 0, 0, 1080),
            rc_work: RectI::new(-1920, 0, 0, 1040),
            device_id: "\\\\.\\DISPLAY2".to_string(),
            primary: false,
        }
    }

    /// 上方副屏：1920×1080 @1.25，物理 y ∈ [-1080, 0)，VDC 原点 y = -864。
    fn top() -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(3),
            origin_vdc: Vec2::new(0.0, -864.0),
            scale: 1.25,
            rc_monitor: RectI::new(0, -1080, 1920, 0),
            rc_work: RectI::new(0, -1080, 1920, -40),
            device_id: "\\\\.\\DISPLAY3".to_string(),
            primary: false,
        }
    }

    fn three() -> Vec<MonitorInfo> {
        vec![primary(), left(), top()]
    }

    #[test]
    fn monitor_at_selects_by_vdc_with_negative_and_scaled_monitors() {
        let svc = DisplayService::from_monitors(three());
        assert_eq!(svc.monitor_at(Vec2::new(100.0, 100.0)).id, MonitorId(1));
        // 左副屏（负 x）
        assert_eq!(svc.monitor_at(Vec2::new(-100.0, 100.0)).id, MonitorId(2));
        // 上副屏（负 y）
        assert_eq!(svc.monitor_at(Vec2::new(100.0, -100.0)).id, MonitorId(3));
    }

    #[test]
    fn monitor_at_clamps_to_nearest_when_outside_all() {
        let svc = DisplayService::from_monitors(three());
        // 远在左侧屏之外 → 最近为左副屏（其中心 (-960,540)）
        assert_eq!(svc.monitor_at(Vec2::new(-5000.0, 500.0)).id, MonitorId(2));
        // 右下远方 → 主屏中心 (640,360) 比上副屏中心 (768,-432) 更近
        assert_eq!(svc.monitor_at(Vec2::new(3000.0, 1500.0)).id, MonitorId(1));
    }

    #[test]
    fn monitor_at_on_boundary_is_half_open() {
        let svc = DisplayService::from_monitors(three());
        // 主屏 VDC x ∈ [0, 1280)；x=1280 不在主屏 → 落到最近（仍主屏中心更近，但断言不落在左屏）
        assert_eq!(svc.monitor_at(Vec2::new(0.0, 0.0)).id, MonitorId(1));
        // 左副屏 VDC x ∈ [-1920, 0)；x=0 属主屏
        assert_eq!(svc.monitor_at(Vec2::new(-0.5, 10.0)).id, MonitorId(2));
    }

    #[test]
    fn select_monitor_at_empty_returns_none() {
        assert!(select_monitor_at(&[], Vec2::ZERO).is_none());
    }

    #[test]
    fn vdc_spc_roundtrip_includes_origin_offset() {
        // 主屏 scale=1.5，origin=(0,0)
        let m = primary();
        let vdc = Vec2::new(300.0, 200.0);
        let spc = m.vdc_to_spc(vdc);
        assert!((spc.x - 450.0).abs() < 1e-4, "spc.x={}", spc.x);
        assert!((spc.y - 300.0).abs() < 1e-4, "spc.y={}", spc.y);
        let back = m.spc_to_vdc(spc);
        assert!((back.x - vdc.x).abs() < 1e-4);
        assert!((back.y - vdc.y).abs() < 1e-4);

        // 左副屏 scale=1.0，origin=(-1920,0)：含负原点偏移
        let l = left();
        let vdc2 = Vec2::new(-1800.0, 50.0);
        let spc2 = l.vdc_to_spc(vdc2);
        assert!((spc2.x - 120.0).abs() < 1e-4, "spc2.x={}", spc2.x);
        assert!((spc2.y - 50.0).abs() < 1e-4);
        let back2 = l.spc_to_vdc(spc2);
        assert!((back2.x - vdc2.x).abs() < 1e-4);
        assert!((back2.y - vdc2.y).abs() < 1e-4);
    }

    #[test]
    fn vdc_physical_roundtrip() {
        let svc = DisplayService::from_monitors(three());
        for (vdc, expect) in [
            (Vec2::new(300.0, 200.0), (450, 300)),   // 主屏 1.5：spc=(450,300)，物理原点 (0,0)
            (Vec2::new(-1800.0, 50.0), (-1800, 50)), // 左副屏 1.0：spc=(120,50)，物理原点 (-1920,0)
            // 上副屏 1.25：origin_vdc=(0,-864)，spc=((100-0)*1.25,(-100+864)*1.25)=(125,955)
            // 物理原点 (0,-1080) → 物理=(125, -125)
            (Vec2::new(100.0, -100.0), (125, -125)),
        ] {
            let (px, py) = svc.vdc_to_physical(vdc);
            assert_eq!((px, py), expect, "vdc={vdc:?}");
            let back = svc.physical_to_vdc(px, py);
            assert!((back.x - vdc.x).abs() < 1e-3, "back={back:?} 期望={vdc:?}");
            assert!((back.y - vdc.y).abs() < 1e-3, "back={back:?} 期望={vdc:?}");
        }
    }

    #[test]
    fn size_logical_to_physical_applies_scales() {
        let m = primary(); // scale 1.5
        assert_eq!(m.size_logical_to_px(256, 1.0), 384);
        assert_eq!(m.size_logical_to_px(256, 0.5), 192);
        // 非法 userScale 退化为 1.0
        assert_eq!(m.size_logical_to_px(100, f32::NAN), 150);
    }

    #[test]
    fn empty_service_clamps_to_synthetic_primary() {
        let svc = DisplayService::from_monitors(Vec::new());
        let m = svc.monitor_at(Vec2::new(10.0, 10.0));
        assert_eq!(m.id, MonitorId(0));
        assert_eq!(m.scale, 1.0);
        assert_eq!(svc.primary(), None);
    }

    #[test]
    fn rect_contains_half_open() {
        let r = RectI::new(-10, -20, 30, 40);
        assert!(r.contains(-10, -20));
        assert!(r.contains(29, 39));
        assert!(!r.contains(30, 39));
        assert!(!r.contains(-11, 0));
    }
}
