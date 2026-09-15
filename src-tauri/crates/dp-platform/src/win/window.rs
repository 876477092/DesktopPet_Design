//! Windows 窗口实装：透明 / 置顶三态 / 穿透 / 全屏判定（`02 §5 K-1`）。
//!
//! 关键设计：
//!   - **样式位补写策略**：一律 `|=` 叠加、只补不删，**严格保留系统管理的
//!     `WS_EX_TOPMOST`**；强制清除 `WS_EX_NOREDIRECTIONBITMAP`（B-09 门禁禁止项）。
//!   - **分层窗口语义**：补 `WS_EX_LAYERED` 后显式 `SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA)`，
//!     保证 WebView2 alpha 合成有效（SG-M1 已在 Win11 24H2 + WebView2 141 实测可行）。
//!   - **全屏三重校验**：前台窗口无标题栏 / 无粗边框 → `DWMWA_EXTENDED_FRAME_BOUNDS`
//!     → 是否覆盖所在显示器 `rcMonitor`；隐藏 / 恢复均带 **3s 迟滞**。
//!   - **时间来源**：3s 迟滞的 `now_ms` 由调用方传入（S1-M4 经 `WallClock` 端口取值），
//!     本模块**不自行读取系统时钟**，符合 C3（禁止直接 `Utc::now()`/`SystemTime`）。
//!
//! ⚠️ `foreground_fullscreen_monitor()` 在 `02` 中定位为 `win/winenum.rs`，但 S1-M2 交付物
//! 清单只含 `win/{mod,window,display}.rs`。故暂置于此，**待 S2-M7 建 `winenum.rs` 后迁移**。
//!
//! ⚠️ 边界：本模块**不实现** `WH_MOUSE_LL` 钩子（S3-M1）、**不实现**像素命中（S3-M2）、
//! **不起** 30s 定时线程（定时线程在 `dp-app::supervisor`，S1-M4）、**不做**托盘（S1-M3）。
//! `ensure_styles()` 只提供「一次性补回」的可调用能力；`set_click_through` 中「卸载钩子」留待 S3-M1。
//!
//! ★ S1-M4 追加（本文件「只加不改」既有公开语义）：[`WinPlatformWindow::self_check`]
//! （30s 自检的**动作本体**：复用 `ensure_styles` 并读回校验三必要位与置顶位一致性）、
//! [`WinPlatformWindow::current_monitor_id`] / [`WinPlatformWindow::window_physical_rect`] /
//! [`WinPlatformWindow::ensure_on_screen`]（拔插屏迁移兜底，FR-1-4）与纯函数
//! [`recovery_target`]。**窗口创建参数与既有方法语义保持 S1-M2 冻结口径不变。**

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use windows::core::GUID;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::ITaskbarList;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetDesktopWindow, GetForegroundWindow, GetShellWindow, GetWindowLongPtrW,
    GetWindowRect, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    GWL_EXSTYLE, GWL_STYLE, HWND_NOTOPMOST, HWND_TOPMOST, LWA_ALPHA, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_CAPTION, WS_EX_APPWINDOW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_THICKFRAME,
};

use super::display::{DisplayService, MonitorId, MonitorInfo, RectI};
use crate::traits::{
    HitMask, PlatformError, PlatformWindow, RawWindowHandle, Result, TopmostMode, Vec2,
};

/// 退出全屏到恢复显示的迟滞（毫秒）：避免全屏切换过程中的闪切（`02 §5 K-1`）。
pub const RESTORE_DELAY_MS: i64 = 3_000;

/// 目标 Z 序位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZOrder {
    /// `HWND_TOPMOST`。
    Topmost,
    /// `HWND_NOTOPMOST`。
    NoTopmost,
}

/// 一次置顶决策的结果（Z 序位 + 可选可见性变更）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopmostPlan {
    /// 目标 Z 序位。
    pub zorder: ZOrder,
    /// `Some(false)` = 隐藏；`Some(true)` = 显示；`None` = 不改可见性。
    pub visible: Option<bool>,
}

/// 全屏轮询产生的动作（供上层日志 / 事件广播）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchAction {
    /// 无变化。
    None,
    /// 进入全屏：隐藏窗口 + 取消置顶。
    Hide,
    /// 退出全屏（迟滞期满）：恢复显示 + 恢复置顶。
    Show,
}

/// 一次窗口自检的结果（S1-M4，`02 §5 K-1`「30s 自检重设」）。
///
/// 由 [`WinPlatformWindow::self_check`] 产出，供 `dp-app::supervisor` 每 30s 统计与事件广播。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfCheckReport {
    /// 综合结论：三必要扩展样式位齐备 **且** 置顶位与当前置顶计划一致。
    pub ok: bool,
    /// 三必要位（`WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`）的**缺失掩码**。
    /// `0` 表示齐备。
    pub missing_ex_style: u32,
    /// 置顶位（`WS_EX_TOPMOST`）是否与当前置顶计划（[`TopmostPlan::zorder`]）一致。
    pub topmost_ok: bool,
}

/// 置顶三态 → 目标 Z 序 / 可见性映射（纯函数，单测覆盖）。
///
/// | mode | fullscreen/hidden | zorder | visible |
/// |---|---|---|---|
/// | `Always` | — | `Topmost` | `None` |
/// | `Never` | — | `NoTopmost` | `None` |
/// | `BelowFullscreen` | 是 | `NoTopmost` | `Some(false)` |
/// | `BelowFullscreen` | 否 | `Topmost` | `None` |
#[must_use]
pub fn topmost_plan(mode: TopmostMode, fullscreen: bool, hidden: bool) -> TopmostPlan {
    match mode {
        TopmostMode::Always => TopmostPlan { zorder: ZOrder::Topmost, visible: None },
        TopmostMode::Never => TopmostPlan { zorder: ZOrder::NoTopmost, visible: None },
        TopmostMode::BelowFullscreen => {
            if fullscreen || hidden {
                TopmostPlan { zorder: ZOrder::NoTopmost, visible: Some(false) }
            } else {
                TopmostPlan { zorder: ZOrder::Topmost, visible: None }
            }
        }
    }
}

/// BelowFullscreen 全屏状态的**纯逻辑**迟滞状态机。
///
/// 不持有任何计时器 / 时钟：每次轮询由调用方传入 `now_ms`（经 `WallClock` 端口获取），
/// 返回「本次应执行的动作」。S1-M4 只需每 2s 调一次 [`FullscreenWatch::on_poll`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FullscreenWatch {
    mode: TopmostMode,
    hidden: bool,
    fullscreen: bool,
    last_fullscreen_ms: Option<i64>,
}

impl FullscreenWatch {
    /// 按初始置顶模式创建。
    #[must_use]
    pub fn new(mode: TopmostMode) -> Self {
        Self { mode, hidden: false, fullscreen: false, last_fullscreen_ms: None }
    }

    /// 当前置顶模式。
    #[must_use]
    pub fn mode(&self) -> TopmostMode {
        self.mode
    }

    /// 切换置顶模式（离开 `BelowFullscreen` 时清除全屏隐藏态）。
    pub fn set_mode(&mut self, mode: TopmostMode) {
        self.mode = mode;
        if mode != TopmostMode::BelowFullscreen {
            self.hidden = false;
        }
    }

    /// 最近一次轮询观察到的全屏状态。
    #[must_use]
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// 是否因全屏而隐藏。
    #[must_use]
    pub fn is_hidden_for_fullscreen(&self) -> bool {
        self.hidden
    }

    /// 当前状态对应的置顶计划。
    #[must_use]
    pub fn plan(&self) -> TopmostPlan {
        topmost_plan(self.mode, self.fullscreen, self.hidden)
    }

    /// 轮询一次全屏状态，推进状态机并返回应执行的动作。
    ///
    /// 语义：全屏出现 → `Hide`；全屏消失后 **从「最近一次观测到全屏的轮询时刻」起算**
    /// 等待 `RESTORE_DELAY_MS` 迟滞期满 → `Show`；其余 → `None`。
    /// 非 `BelowFullscreen` 模式恒为 `None`。
    ///
    /// ⚠️ 迟滞以「观测时刻」起算；上层（S1-M4）为 1s 轮询时，实际恢复相对真实退出全屏
    /// 最多晚一个轮询周期（AC-11「退出 3s 内恢复」由 S1-M4 调度频率共同决定；如需严格 ≤3s
    /// 可提高轮询频率）。
    pub fn on_poll(&mut self, now_ms: i64, fullscreen: bool) -> WatchAction {
        self.fullscreen = fullscreen;

        if self.mode != TopmostMode::BelowFullscreen {
            self.hidden = false;
            return WatchAction::None;
        }

        if fullscreen {
            self.last_fullscreen_ms = Some(now_ms);
            if self.hidden {
                return WatchAction::None;
            }
            self.hidden = true;
            return WatchAction::Hide;
        }

        if self.hidden {
            if let Some(last) = self.last_fullscreen_ms {
                if now_ms.saturating_sub(last) >= RESTORE_DELAY_MS {
                    self.hidden = false;
                    return WatchAction::Show;
                }
            }
        }
        WatchAction::None
    }
}

// ---------------------------------------------------------------------------
// 三重校验的纯函数部分（可用假输入单测）
// ---------------------------------------------------------------------------

/// 前台窗口是否带「窗口化」样式（有标题栏或粗边框 → 不是全屏）。
///
/// 判据源自 `02 §5 K-1`：`style & (WS_CAPTION | WS_THICKFRAME) != 0` → 非全屏。
#[must_use]
pub fn style_has_frame(style: u32) -> bool {
    style & (WS_CAPTION.0 | WS_THICKFRAME.0) != 0
}

/// 壳层窗口类名（应被全屏判定排除）。
#[must_use]
pub fn is_shell_class_name(name: &str) -> bool {
    matches!(
        name,
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    )
}

/// `bounds` 是否覆盖 `monitor`（允许 `tolerance` 像素误差；规避 Win10/11 阴影边框差异）。
#[must_use]
pub fn covers(bounds: RectI, monitor: RectI, tolerance: i32) -> bool {
    bounds.left <= monitor.left + tolerance
        && bounds.top <= monitor.top + tolerance
        && bounds.right >= monitor.right - tolerance
        && bounds.bottom >= monitor.bottom - tolerance
}

/// 计算「窗口物理矩形中心」落在当前显示器列表之外时的**迁移目标 VDC**（纯函数，S1-M4）。
///
/// 语义（`03 §2 S1-M4`，FR-1-4 拔屏兜底）：
///   1. 若 `center_px` 落在任一 `monitors[i].rc_monitor` 内 → 返回 `None`（无需迁移）；
///   2. 否则（含「当前所在屏已从列表消失」= 拔屏）→ 取**最近**显示器
///      （按 `rc_monitor.center()` 的欧氏距离）`rc_work` 中心的**物理坐标**，
///      经 [`MonitorInfo::physical_to_vdc`] 换算为 VDC 返回；
///   3. `monitors` 为空 → 返回 `None`（调用方保持原位，不做无依据的迁移）。
///
/// 为可被 `tests/` 外部引用，本函数定义为模块级 `pub fn`（不依赖任何平台句柄）。
#[must_use]
pub fn recovery_target(center_px: (i32, i32), monitors: &[MonitorInfo]) -> Option<Vec2> {
    if monitors.is_empty() {
        return None;
    }

    // ① 中心仍在任一显示器 `rc_monitor` 内 → 无需迁移。
    if monitors
        .iter()
        .any(|m| m.rc_monitor.contains(center_px.0, center_px.1))
    {
        return None;
    }

    // ② 未命中：取面板中心欧氏距离最近者（用平方距离比较，省去开方且保持序关系一致）。
    let (cx, cy) = (center_px.0 as f32, center_px.1 as f32);
    let nearest = monitors.iter().min_by(|a, b| {
        let da = dist_sq_to_center(a.rc_monitor.center(), cx, cy);
        let db = dist_sq_to_center(b.rc_monitor.center(), cx, cy);
        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
    })?;

    // ③ 目标 = 最近显示器工作区中心的物理坐标 → VDC。
    let work_center = nearest.rc_work.center();
    Some(nearest.physical_to_vdc(
        work_center.x.round() as i32,
        work_center.y.round() as i32,
    ))
}

/// 物理点到面板中心的平方距离（欧氏距离的单调等价量）。
#[inline]
#[must_use]
fn dist_sq_to_center(center: Vec2, px: f32, py: f32) -> f32 {
    let dx = center.x - px;
    let dy = center.y - py;
    dx * dx + dy * dy
}

/// 判定前台窗口是否全屏，返回其所在显示器标识（三重校验，`02 §5 K-1`）。
///
/// 步骤：① 前台窗口存在且非壳层；② 无标题栏 / 粗边框；③ `DWMWA_EXTENDED_FRAME_BOUNDS`
/// 覆盖所在显示器 `rcMonitor`。任一不满足返回 `None`。
///
/// ⚠️ 待 S2-M7 建 `win/winenum.rs` 后迁移至此（本文件属 S1-M2 交付范围）。
#[must_use]
pub fn foreground_fullscreen_monitor() -> Option<MonitorId> {
    // 不变量：`GetForegroundWindow` 可返回空句柄，先判空再使用。
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() || is_shell_window(hwnd) {
        return None;
    }

    let style = (unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) }) as u32;
    if style_has_frame(style) {
        return None;
    }

    let bounds = unsafe { dwm_extended_bounds(hwnd) }.or_else(|| unsafe { window_rect(hwnd) })?;

    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let monitor = super::display::query_monitor(hmon)?;

    if covers(bounds, monitor.rc_monitor, 0) {
        Some(monitor.id)
    } else {
        None
    }
}

/// 读取窗口的 DWM 扩展边框（`DWMWA_EXTENDED_FRAME_BOUNDS`）；失败返回 `None`。
///
/// # Safety
/// `hwnd` 须为有效窗口句柄；本函数只读，不产生副作用。
unsafe fn dwm_extended_bounds(hwnd: HWND) -> Option<RectI> {
    let mut rect = RECT::default();
    // 不变量：rect 为出参，传入大小与 DWMWA_EXTENDED_FRAME_BOUNDS 要求的 RECT 一致。
    DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut rect as *mut RECT as *mut core::ffi::c_void,
        core::mem::size_of::<RECT>() as u32,
    )
    .ok()?;
    Some(RectI::new(rect.left, rect.top, rect.right, rect.bottom))
}

/// 读取窗口矩形（DWM 不可用时的兜底）。
///
/// # Safety
/// `hwnd` 须为有效窗口句柄。
unsafe fn window_rect(hwnd: HWND) -> Option<RectI> {
    let mut rect = RECT::default();
    GetWindowRect(hwnd, &mut rect).ok()?;
    Some(RectI::new(rect.left, rect.top, rect.right, rect.bottom))
}

/// 是否系统壳层 / 桌面窗口。
#[must_use]
pub fn is_shell_window(hwnd: HWND) -> bool {
    let shell = unsafe { GetShellWindow() };
    let desktop = unsafe { GetDesktopWindow() };
    if hwnd.0 == shell.0 || hwnd.0 == desktop.0 {
        return true;
    }
    is_shell_class_name(&class_name(hwnd))
}

/// 读取窗口类名（失败返回空串）。
fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// 由 `windows::core::Error` 构造错误。
fn win_err(op: &'static str, err: windows::core::Error) -> PlatformError {
    PlatformError::Win32 { op, code: err.code().0 as u32 }
}

/// 计算期望的扩展样式位：补足三必要位、清除禁止位与 `WS_EX_APPWINDOW`、按需加/摘穿透位，
/// **其余位一律保留**。
///
/// ★ 严格「按位增删」而非整体覆写：保留系统管理的 `WS_EX_TOPMOST` 等位（S1-M1 QA 结论）。
/// ★ 额外清除 `WS_EX_APPWINDOW`：tao 因 `ON_TASKBAR=true` 默认置该位（S1-M1 实测
///   exstyle=0x00040110），而 `WS_EX_APPWINDOW` 会强制进任务栏、与 `WS_EX_TOOLWINDOW`
///   的语义冲突；设计窗口参数（`02 §5 K-1`）不含该位，故在此清除，确保「不进 Alt+Tab / 任务栏」。
#[must_use]
fn desired_ex_style(current: u32, click_through: bool) -> u32 {
    let mut want = current;
    want |= WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0;
    want &= !(WS_EX_NOREDIRECTIONBITMAP.0 | WS_EX_APPWINDOW.0);
    if click_through {
        want |= WS_EX_TRANSPARENT.0;
    } else {
        want &= !WS_EX_TRANSPARENT.0;
    }
    want
}

/// 二次保险：把窗口从任务栏移除（`ITaskbarList::DeleteTab`，`02 §5 K-1`）。
fn remove_from_taskbar(hwnd: HWND) -> Result<()> {
    // ITaskbarList 的 CLSID 常量 windows crate 未导出，按 Win32 SDK 值补（非盘符字面量，C1 无关）。
    let clsid = GUID::from_u128(0x56fdf344_fd6d_11d0_958a_006097c9a090);
    let taskbar: ITaskbarList = unsafe { CoCreateInstance(&clsid, None, CLSCTX_ALL) }
        .map_err(|e| win_err("CoCreateInstance(ITaskbarList)", e))?;
    unsafe { taskbar.HrInit() }.map_err(|e| win_err("ITaskbarList::HrInit", e))?;
    unsafe { taskbar.DeleteTab(hwnd) }.map_err(|e| win_err("ITaskbarList::DeleteTab", e))?;
    Ok(())
}

/// 穿透「光标事件开关」回调（Tauri `set_ignore_cursor_events` 侧，双写口径）。
pub type CursorEventsHook = Arc<dyn Fn(bool) + Send + Sync>;

/// 穿透「钩子装/卸观测者」回调（S3-M1：随穿透态启用 / 卸载 `WH_MOUSE_LL`）。
///
/// 与 [`CursorEventsHook`] **同构但语义分离**：前者是 Tauri `set_ignore_cursor_events` 双写，
/// 后者承载「钩子随穿透装 / 卸」；二者在同一挂点（[`WinPlatformWindow::set_click_through`]）被调，
/// 但**不互相嵌套**（关注点分离，设计补充 §4.1）。
pub type ClickThroughHook = Arc<dyn Fn(bool) + Send + Sync>;

/// 窗口运行期状态（受 `Mutex` 保护的可变部分）。
struct WindowRuntime {
    click_through: bool,
    watch: FullscreenWatch,
}

/// Windows 平台窗口实装（`PlatformWindow` 的 Win 落地）。
pub struct WinPlatformWindow {
    hwnd: HWND,
    display: Arc<DisplayService>,
    runtime: Mutex<WindowRuntime>,
    hit_mask: Mutex<Option<Arc<HitMask>>>,
    user_scale: AtomicU32,
    cursor_hook: Mutex<Option<CursorEventsHook>>,
    /// S3-M1：穿透装 / 卸观测者（随 `set_click_through` 触发钩子装 / 卸）。
    click_through_hook: Mutex<Option<ClickThroughHook>>,
}

// 不变量：HWND 仅经线程安全的 Win32 API 使用（SetWindowPos / ShowWindow / SetWindowLongPtrW /
// SetLayeredWindowAttributes 均可跨线程调用），且所有可变状态由 Mutex / Atomic 串行化，
// 故将含裸句柄的封装标记为 Send + Sync 是安全的（`PlatformWindow: Send + Sync` 契约要求）。
unsafe impl Send for WinPlatformWindow {}
unsafe impl Sync for WinPlatformWindow {}

impl WinPlatformWindow {
    /// 由原生句柄值（Win32 `HWND` 指针值）挂接平台窗口。
    pub fn new(hwnd: isize, display: Arc<DisplayService>) -> Result<Self> {
        if hwnd == 0 {
            return Err(PlatformError::InvalidHandle);
        }
        Ok(Self {
            hwnd: HWND(hwnd as *mut core::ffi::c_void),
            display,
            runtime: Mutex::new(WindowRuntime {
                click_through: false,
                watch: FullscreenWatch::new(TopmostMode::Always),
            }),
            hit_mask: Mutex::new(None),
            user_scale: AtomicU32::new(1.0f32.to_bits()),
            cursor_hook: Mutex::new(None),
            click_through_hook: Mutex::new(None),
        })
    }

    /// 设置用户缩放因子（`02 §7.2`：`w_px = logical_w × userScale × scale`）。
    pub fn set_user_scale(&self, scale: f32) {
        let safe = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        self.user_scale.store(safe.to_bits(), Ordering::Relaxed);
    }

    /// 原生窗口句柄值（Win32 `HWND` 指针值；S6-M2 托盘气泡宿主窗口用）。
    #[must_use]
    pub fn hwnd(&self) -> isize {
        self.hwnd.0 as isize
    }

    /// 注册「光标事件开关」回调（Tauri `set_ignore_cursor_events` 侧，穿透双写口径）。
    pub fn set_cursor_events_hook(&self, hook: CursorEventsHook) {
        *self.cursor_hook.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    /// 注册「穿透装 / 卸观测者」（S3-M1：随穿透态启用 / 卸载 `WH_MOUSE_LL`）。
    ///
    /// 挂点复用 [`WinPlatformWindow::set_click_through`]，保证「任何人切穿透 → 钩子随之装 / 卸」
    /// 单入口、零竞态；与 `cursor_hook` 完全同构（设计补充 §4.1）。
    pub fn set_click_through_observer(&self, hook: ClickThroughHook) {
        *self.click_through_hook.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    /// 轮询全屏状态并应用结果（供 S1-M4 每 2s 调度；`now_ms` 由 `WallClock` 提供）。
    pub fn poll_fullscreen(&self, now_ms: i64) -> Result<WatchAction> {
        let fullscreen = foreground_fullscreen_monitor().is_some();
        let action = {
            let mut st = self.lock_state();
            st.watch.on_poll(now_ms, fullscreen)
        };
        match action {
            WatchAction::Hide => self.apply_plan(TopmostPlan {
                zorder: ZOrder::NoTopmost,
                visible: Some(false),
            })?,
            WatchAction::Show => {
                let mut plan = self.lock_state().watch.plan();
                plan.visible = Some(true);
                self.apply_plan(plan)?;
            }
            WatchAction::None => {}
        }
        Ok(action)
    }

    /// 是否因前台全屏而隐藏（`BelowFullscreen` 语义）。
    #[must_use]
    pub fn is_hidden_for_fullscreen(&self) -> bool {
        self.lock_state().watch.is_hidden_for_fullscreen()
    }

    /// 当前是否处于穿透态。
    #[must_use]
    pub fn is_click_through(&self) -> bool {
        self.lock_state().click_through
    }

    /// 取已存储的命中掩码（供 S3 消费）。
    #[must_use]
    pub fn hit_mask(&self) -> Option<Arc<HitMask>> {
        self.hit_mask.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 锁运行期状态（poison 亦不 panic）。
    fn lock_state(&self) -> MutexGuard<'_, WindowRuntime> {
        self.runtime.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 当前窗口所在显示器（`MonitorFromWindow`，失败钳制到 VDC 原点显示器）。
    fn current_monitor(&self) -> super::display::MonitorInfo {
        let hmon = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        super::display::query_monitor(hmon).unwrap_or_else(|| self.display.monitor_at(Vec2::ZERO))
    }

    /// 当前用户缩放。
    fn user_scale(&self) -> f32 {
        f32::from_bits(self.user_scale.load(Ordering::Relaxed))
    }

    /// 读取扩展样式位。
    fn ex_style(&self) -> u32 {
        (unsafe { GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) }) as u32
    }

    /// 应用置顶计划（Z 序 + 可选可见性）。
    fn apply_plan(&self, plan: TopmostPlan) -> Result<()> {
        let insert_after: Option<HWND> = match plan.zorder {
            ZOrder::Topmost => Some(HWND_TOPMOST),
            ZOrder::NoTopmost => Some(HWND_NOTOPMOST),
        };
        unsafe {
            SetWindowPos(
                self.hwnd,
                insert_after,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        }
        .map_err(|e| win_err("SetWindowPos(zorder)", e))?;

        if let Some(visible) = plan.visible {
            // 返回值是「此前的可见性」，此处不关心。
            let _ = unsafe {
                ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE })
            };
        }
        Ok(())
    }

    /// 移动窗口到虚拟桌面物理像素坐标。
    fn set_position_px(&self, x: i32, y: i32) -> Result<()> {
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .map_err(|e| win_err("SetWindowPos(move)", e))
    }

    /// 设置窗口物理尺寸。
    fn set_size_px(&self, w: i32, h: i32) -> Result<()> {
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                w,
                h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .map_err(|e| win_err("SetWindowPos(size)", e))
    }

    // -----------------------------------------------------------------------
    // S1-M4：窗口自检与多显示器恢复（本模块只做「自检 + 恢复」，
    // 不改窗口创建参数；创建参数属 S1-M2，语义冻结）。
    // -----------------------------------------------------------------------

    /// 窗口自检（`02 §5 K-1`「30s 自检重设」，S1-M4 交付物）。
    ///
    /// 步骤：① 复用既有 [`PlatformWindow::ensure_styles`]（样式读回校验 + 补写 +
    /// `ITaskbarList::DeleteTab` 任务栏移除 + 按当前模式/全屏态重设 Z 序与可见性）；
    /// ② 重新读回 `WS_EX_*` 位，计算三必要位的**缺失掩码** `missing_ex_style`；
    /// ③ 校验 `WS_EX_TOPMOST` 位是否与当前置顶计划（`watch.plan().zorder == Topmost`）一致。
    ///
    /// **失败降级不 panic**：任何 Win32 失败只返回 `Err`（调用方转降级行为）或
    /// 一份 `ok = false` 的报告，绝不 `unwrap` / `panic`（`02 §7.4.2`）。
    ///
    /// # Errors
    /// 当底层 [`PlatformWindow::ensure_styles`] 失败（如 `SetLayeredWindowAttributes`
    /// 或样式读回校验不通过）时返回 [`PlatformError`]。
    pub fn self_check(&self) -> Result<SelfCheckReport> {
        // ① 先补回（幂等）：这是 K-1「缺失即补回」的动作本体。
        self.ensure_styles()?;

        // ② 读回扩展样式位，计算三必要位缺失掩码。
        let now = self.ex_style();
        let required = WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0;
        let missing_ex_style = required & !now;

        // ③ 置顶位与计划一致性：计划要求 Topmost ⇔ WS_EX_TOPMOST 置位。
        let plan_topmost = self.lock_state().watch.plan().zorder == ZOrder::Topmost;
        let bit_set = now & WS_EX_TOPMOST.0 != 0;
        let topmost_ok = plan_topmost == bit_set;

        let ok = missing_ex_style == 0 && topmost_ok;
        if !ok {
            eprintln!(
                "[dp-platform] 窗口自检未通过：missing_ex_style={missing_ex_style:#010X} topmost_ok={topmost_ok}"
            );
        }
        Ok(SelfCheckReport { ok, missing_ex_style, topmost_ok })
    }

    /// 当前窗口所在显示器标识（`MonitorFromWindow`；查询失败返回 `None`）。
    ///
    /// 与私有 [`Self::current_monitor`] 的区别：本方法**不**钳制到合成主屏，
    /// 以便调用方区分「真在某个屏上」与「查询失败」。
    #[must_use]
    pub fn current_monitor_id(&self) -> Option<MonitorId> {
        let hmon = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        super::display::query_monitor(hmon).map(|m| m.id)
    }

    /// 读取窗口当前的**物理像素**矩形（`GetWindowRect`，虚拟桌面坐标系）。
    ///
    /// # Errors
    /// `GetWindowRect` 失败（句柄失效等）时返回 [`PlatformError::Win32`]。
    pub fn window_physical_rect(&self) -> Result<RectI> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(self.hwnd, &mut rect) }.map_err(|e| win_err("GetWindowRect", e))?;
        Ok(RectI::new(rect.left, rect.top, rect.right, rect.bottom))
    }

    /// 拔插屏迁移兜底（FR-1-4）：确保窗口落在当前显示器集合内。
    ///
    /// 取窗口物理矩形中心 → [`recovery_target`] 判定是否需要迁移：
    ///   - `Some(vdc)`：调用 `set_position_vdc(vdc)` 迁移并返回该 VDC；
    ///   - `None`：窗口仍在屏内（或列表为空）→ 不迁移，返回 `None`。
    ///
    /// # Errors
    /// 读取窗口矩形或写窗口位置失败时返回 [`PlatformError`]（调用方降级为日志）。
    pub fn ensure_on_screen(&self, monitors: &[MonitorInfo]) -> Result<Option<Vec2>> {
        let rect = self.window_physical_rect()?;
        let center = rect.center();
        let center_px = (center.x.round() as i32, center.y.round() as i32);

        match recovery_target(center_px, monitors) {
            Some(vdc) => {
                // 内部经 DisplayService 换算 VDC → 物理像素后落窗口（RV-17）。
                self.set_position_vdc(vdc)?;
                Ok(Some(vdc))
            }
            None => Ok(None),
        }
    }
}

impl PlatformWindow for WinPlatformWindow {
    fn set_position_vdc(&self, p: Vec2) -> Result<()> {
        // 入参 VDC → 物理像素（含显示器原点偏移，RV-17）。
        let (x, y) = self.display.vdc_to_physical(p);
        self.set_position_px(x, y)
    }

    fn set_size_logical(&self, w: u32, h: u32) -> Result<()> {
        let monitor = self.current_monitor();
        let scale = self.user_scale();
        let px_w = monitor.size_logical_to_px(w, scale) as i32;
        let px_h = monitor.size_logical_to_px(h, scale) as i32;
        self.set_size_px(px_w.max(1), px_h.max(1))
    }

    fn set_topmost(&self, mode: TopmostMode) -> Result<()> {
        let plan = {
            let mut st = self.lock_state();
            st.watch.set_mode(mode);
            st.watch.plan()
        };
        self.apply_plan(plan)
    }

    fn set_click_through(&self, on: bool) -> Result<()> {
        {
            self.lock_state().click_through = on;
        }
        // 样式位（WS_EX_TRANSPARENT）随之重设。
        self.ensure_styles()?;
        // Tauri 侧 set_ignore_cursor_events 双写（SG-M1 定版口径）。
        if let Some(hook) = self.cursor_hook.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            hook(on);
        }
        // S3-M1：钩子装 / 卸观测者（紧随 cursor_hook 之后；语义分离，不嵌套）。
        if let Some(hook) =
            self.click_through_hook.lock().unwrap_or_else(|e| e.into_inner()).clone()
        {
            hook(on);
        }
        Ok(())
    }

    fn set_hit_mask(&self, mask: Arc<HitMask>) -> Result<()> {
        // 只做「存储 + 供后续消费」，不实现像素命中（S3-M2）。
        *self.hit_mask.lock().unwrap_or_else(|e| e.into_inner()) = Some(mask);
        Ok(())
    }

    fn set_visible(&self, v: bool) -> Result<()> {
        // 返回值是「此前的可见性」，此处不关心。
        let _ = unsafe {
            ShowWindow(self.hwnd, if v { SW_SHOWNOACTIVATE } else { SW_HIDE })
        };
        Ok(())
    }

    fn ensure_styles(&self) -> Result<()> {
        let current = self.ex_style();
        let click_through = self.lock_state().click_through;
        let want = desired_ex_style(current, click_through);

        if want != current {
            // 不变量：调用后紧接读回校验，无需依赖旧返回值判成败。
            unsafe { SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, want as isize) };
        }

        // 补 WS_EX_LAYERED 后必须配套分层语义，否则 WebView2 内容可能整体不可见（S1-M1 QA 提示）。
        unsafe { SetLayeredWindowAttributes(self.hwnd, COLORREF(0), 255, LWA_ALPHA) }
            .map_err(|e| win_err("SetLayeredWindowAttributes", e))?;

        // 读回校验：三必要位必须在位、禁止位（NOREDIRECTIONBITMAP / APPWINDOW）必须不在。
        let now = self.ex_style();
        let required = WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0;
        let missing = required & !now;
        let forbidden = now & (WS_EX_NOREDIRECTIONBITMAP.0 | WS_EX_APPWINDOW.0);
        if missing != 0 || forbidden != 0 {
            return Err(PlatformError::Win32 {
                op: "EnsureExStyles",
                code: missing | forbidden,
            });
        }

        // 按当前模式 / 全屏态重设 Z 序与可见性。
        let plan = self.lock_state().watch.plan();
        self.apply_plan(plan)?;

        // 二次保险：从任务栏移除（best-effort，失败仅告警，tao skipTaskbar 已生效）。
        if let Err(err) = remove_from_taskbar(self.hwnd) {
            eprintln!("[dp-platform] ITaskbarList::DeleteTab 降级：{err}");
        }
        Ok(())
    }

    fn raw_handle(&self) -> RawWindowHandle {
        RawWindowHandle::from_hwnd(self.hwnd.0 as isize)
    }
}

// ---------------------------------------------------------------------------
// 单元测试（纯逻辑部分，不依赖真机）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topmost_plan_mapping_table() {
        // Always：常驻置顶，不改可见性
        assert_eq!(
            topmost_plan(TopmostMode::Always, false, false),
            TopmostPlan { zorder: ZOrder::Topmost, visible: None }
        );
        assert_eq!(
            topmost_plan(TopmostMode::Always, true, true),
            TopmostPlan { zorder: ZOrder::Topmost, visible: None }
        );
        // Never：取消置顶，不改可见性
        assert_eq!(
            topmost_plan(TopmostMode::Never, false, false),
            TopmostPlan { zorder: ZOrder::NoTopmost, visible: None }
        );
        // BelowFullscreen + 全屏 → 取消置顶 + 隐藏
        assert_eq!(
            topmost_plan(TopmostMode::BelowFullscreen, true, false),
            TopmostPlan { zorder: ZOrder::NoTopmost, visible: Some(false) }
        );
        // BelowFullscreen + 已隐藏（非全屏但迟滞未满）→ 保持取消置顶 + 隐藏
        assert_eq!(
            topmost_plan(TopmostMode::BelowFullscreen, false, true),
            TopmostPlan { zorder: ZOrder::NoTopmost, visible: Some(false) }
        );
        // BelowFullscreen + 正常 → 恢复置顶
        assert_eq!(
            topmost_plan(TopmostMode::BelowFullscreen, false, false),
            TopmostPlan { zorder: ZOrder::Topmost, visible: None }
        );
    }

    #[test]
    fn below_fullscreen_hides_and_restores_with_3s_hysteresis() {
        let mut w = FullscreenWatch::new(TopmostMode::BelowFullscreen);
        // 初始非全屏 → 无动作
        assert_eq!(w.on_poll(0, false), WatchAction::None);
        assert!(!w.is_hidden_for_fullscreen());

        // 进入全屏 → 隐藏
        assert_eq!(w.on_poll(1_000, true), WatchAction::Hide);
        assert!(w.is_hidden_for_fullscreen());

        // 仍在全屏 → 无动作（不重复隐藏），最近观测时刻推进到 2000
        assert_eq!(w.on_poll(2_000, true), WatchAction::None);

        // 退出全屏：迟滞从最近观测到的全屏时刻 2000 起算
        assert_eq!(w.on_poll(3_000, false), WatchAction::None); // 距 2000 为 1000ms
        assert!(w.is_hidden_for_fullscreen());
        assert_eq!(w.on_poll(4_000, false), WatchAction::None); // 距 2000 为 2000ms
        assert!(w.is_hidden_for_fullscreen());

        // 迟滞期满（2000 + 3000 = 5000）→ 恢复
        assert_eq!(w.on_poll(5_000, false), WatchAction::Show);
        assert!(!w.is_hidden_for_fullscreen());

        // 已恢复后再轮询 → 无重复动作
        assert_eq!(w.on_poll(6_000, false), WatchAction::None);
    }

    #[test]
    fn always_and_never_modes_never_hide() {
        let mut always = FullscreenWatch::new(TopmostMode::Always);
        assert_eq!(always.on_poll(0, true), WatchAction::None);
        assert!(!always.is_hidden_for_fullscreen());
        assert_eq!(always.plan().zorder, ZOrder::Topmost);

        let mut never = FullscreenWatch::new(TopmostMode::Never);
        assert_eq!(never.on_poll(0, true), WatchAction::None);
        assert_eq!(never.plan().zorder, ZOrder::NoTopmost);
    }

    #[test]
    fn leaving_below_fullscreen_clears_hidden() {
        let mut w = FullscreenWatch::new(TopmostMode::BelowFullscreen);
        assert_eq!(w.on_poll(0, true), WatchAction::Hide);
        assert!(w.is_hidden_for_fullscreen());
        w.set_mode(TopmostMode::Always);
        assert!(!w.is_hidden_for_fullscreen());
        assert_eq!(w.plan().zorder, ZOrder::Topmost);
    }

    #[test]
    fn style_has_frame_detects_caption_or_thickframe() {
        assert!(style_has_frame(WS_CAPTION.0));
        assert!(style_has_frame(WS_THICKFRAME.0));
        assert!(style_has_frame(WS_CAPTION.0 | WS_THICKFRAME.0));
        // 仅 WS_POPUP（纯全屏窗口）：无标题栏 / 无粗边框
        assert!(!style_has_frame(0x8000_0000));
        assert!(!style_has_frame(0));
    }

    #[test]
    fn shell_class_names_are_excluded() {
        assert!(is_shell_class_name("Progman"));
        assert!(is_shell_class_name("WorkerW"));
        assert!(is_shell_class_name("Shell_TrayWnd"));
        assert!(!is_shell_class_name("Chrome_WidgetWin_1"));
        assert!(!is_shell_class_name(""));
    }

    #[test]
    fn covers_handles_exact_larger_smaller_and_tolerance() {
        let monitor = RectI::new(0, 0, 1920, 1080);
        // 恰好相等 → 覆盖
        assert!(covers(monitor, monitor, 0));
        // 更大 → 覆盖
        assert!(covers(RectI::new(-10, -10, 1930, 1090), monitor, 0));
        // 更小 → 不覆盖
        assert!(!covers(RectI::new(0, 0, 1900, 1080), monitor, 0));
        // 带容差 → 允许 7~8px 阴影边框差异（Win11）
        assert!(covers(RectI::new(0, 0, 1912, 1080), monitor, 8));
    }

    #[test]
    fn desired_ex_style_adds_required_keeps_topmost_removes_forbidden() {
        // 起始：仅 WS_EX_TOPMOST（系统管理位）+ 禁止位 + tao 默认的 APPWINDOW
        let cur = 0x0000_0008 | 0x0020_0000 | WS_EX_APPWINDOW.0;
        let want = desired_ex_style(cur, false);

        // 三必要位必须补上
        assert_ne!(want & WS_EX_LAYERED.0, 0);
        assert_ne!(want & WS_EX_TOOLWINDOW.0, 0);
        assert_ne!(want & WS_EX_NOACTIVATE.0, 0);
        // 系统管理的 TOPMOST 必须保留
        assert_ne!(want & 0x0000_0008, 0);
        // 禁止位必须清除
        assert_eq!(want & WS_EX_NOREDIRECTIONBITMAP.0, 0);
        // APPWINDOW（会强制进任务栏）必须清除
        assert_eq!(want & WS_EX_APPWINDOW.0, 0);
        // 未开穿透 → 穿透位不置位
        assert_eq!(want & WS_EX_TRANSPARENT.0, 0);

        // 开穿透 → 穿透位置位，其余保持
        let want2 = desired_ex_style(want, true);
        assert_ne!(want2 & WS_EX_TRANSPARENT.0, 0);
        assert_ne!(want2 & 0x0000_0008, 0);
        assert_eq!(want2 & WS_EX_APPWINDOW.0, 0);
        // 关穿透 → 摘除穿透位
        let want3 = desired_ex_style(want2, false);
        assert_eq!(want3 & WS_EX_TRANSPARENT.0, 0);
    }

    // -----------------------------------------------------------------------
    // S1-M4：recovery_target 纯函数（多显示器迁移兜底，FR-1-4）
    // -----------------------------------------------------------------------

    /// 构造测试用显示器描述（物理矩形 + 工作区）。
    fn mk(
        id: u64,
        ox: f32,
        oy: f32,
        scale: f32,
        rc_monitor: RectI,
        rc_work: RectI,
    ) -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(id),
            origin_vdc: Vec2::new(ox, oy),
            scale,
            rc_monitor,
            rc_work,
            device_id: format!("DISPLAY{id}"),
            primary: id == 1,
        }
    }

    /// 主屏：1920×1080 @1.5（物理），工作区底部扣 40px 任务栏。
    fn primary() -> MonitorInfo {
        mk(
            1,
            0.0,
            0.0,
            1.5,
            RectI::new(0, 0, 1920, 1080),
            RectI::new(0, 0, 1920, 1040),
        )
    }

    /// 左侧副屏：1920×1080 @1.0，物理 x ∈ [-1920, 0)。
    fn left() -> MonitorInfo {
        mk(
            2,
            -1920.0,
            0.0,
            1.0,
            RectI::new(-1920, 0, 0, 1080),
            RectI::new(-1920, 0, 0, 1040),
        )
    }

    /// 上方副屏：1920×1080 @1.25，物理 y ∈ [-1080, 0)，VDC 原点 y = -864。
    fn top() -> MonitorInfo {
        mk(
            3,
            0.0,
            -864.0,
            1.25,
            RectI::new(0, -1080, 1920, 0),
            RectI::new(0, -1080, 1920, -40),
        )
    }

    #[test]
    fn recovery_target_none_when_center_on_single_screen() {
        // ① 单屏 + 中心在屏内 → None（无需迁移）。
        let list = vec![primary()];
        assert!(recovery_target((100, 100), &list).is_none());
        // 边界：左上角 (0,0) 与右下角外 (1920,1080) 语义（半开区间）
        assert!(recovery_target((0, 0), &list).is_none());
        assert!(recovery_target((1919, 1079), &list).is_none());
    }

    #[test]
    fn recovery_target_migrates_when_current_monitor_removed() {
        // ② 双屏拔掉当前屏：注入列表已不含原「左副屏」，窗口中心仍在左侧区。
        let remaining = vec![primary()];
        let vdc = recovery_target((-500, 100), &remaining).expect("应迁移到剩余屏");
        // 该 VDC 必须落在剩余屏（主屏）内（monitor_at + contains_vdc 双重断言）。
        let svc = DisplayService::from_monitors(remaining.clone());
        let m = svc.monitor_at(vdc);
        assert_eq!(m.id, MonitorId(1), "应迁移到剩余屏（主屏）");
        assert!(m.contains_vdc(vdc), "迁移目标 VDC 应落在剩余屏内：{vdc:?}");
    }

    #[test]
    fn recovery_target_none_when_monitor_list_empty() {
        // ③ 列表为空 → None（不迁移，避免无依据移动）。
        assert!(recovery_target((100, 100), &[]).is_none());
    }

    #[test]
    fn recovery_target_handles_negative_origin_and_mixed_scale() {
        // ④ 负坐标 + 多 scale（1.5 / 1.0 / 1.25）多屏场景。
        let list = vec![primary(), left(), top()];

        // (a) 远在左侧（含负坐标）→ 迁移到左副屏，目标 VDC 为负 x 且落在左副屏内。
        let vdc_left = recovery_target((-3000, 1000), &list).expect("应迁移到左副屏");
        assert!(vdc_left.x < 0.0, "左副屏 VDC 应为负 x：{vdc_left:?}");
        let svc = DisplayService::from_monitors(list.clone());
        let ml = svc.monitor_at(vdc_left);
        assert_eq!(ml.id, MonitorId(2));
        assert!(ml.contains_vdc(vdc_left), "应落在左副屏内：{vdc_left:?}");

        // (b) 远在上方（负 y、scale 1.25）→ 迁移到上副屏，目标 VDC 落在上副屏内。
        let vdc_top = recovery_target((100, -3000), &list).expect("应迁移到上副屏");
        let mt = svc.monitor_at(vdc_top);
        assert_eq!(mt.id, MonitorId(3));
        assert!(mt.contains_vdc(vdc_top), "应落在上副屏内：{vdc_top:?}");
    }

    // -----------------------------------------------------------------------
    // S1-M4：self_check 幂等（真机临时窗口）
    // -----------------------------------------------------------------------

    /// 最小 user32 FFI（仅测试用，不进入产品路径）。
    mod ffi {
        use std::ffi::c_void;
        use std::os::raw::{c_int, c_uint};

        pub type Hwnd = *mut c_void;

        #[link(name = "user32")]
        extern "system" {
            pub fn CreateWindowExW(
                ex_style: c_uint,
                class: *const u16,
                name: *const u16,
                style: c_uint,
                x: c_int,
                y: c_int,
                w: c_int,
                h: c_int,
                parent: Hwnd,
                menu: Hwnd,
                instance: Hwnd,
                param: *mut c_void,
            ) -> Hwnd;
            pub fn DestroyWindow(hwnd: Hwnd) -> c_int;
            pub fn GetWindowLongPtrW(hwnd: Hwnd, index: c_int) -> isize;
        }

        pub const WS_VISIBLE: u32 = 0x1000_0000;
        pub const GWL_EXSTYLE: c_int = -20;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 临时原生窗口守卫（Drop 时销毁）。
    struct TempWindow(ffi::Hwnd);

    impl TempWindow {
        /// 创建失败（无桌面会话等）返回 `None`，由调用方优雅跳过而非 panic。
        fn new() -> Option<Self> {
            let class = wide("STATIC");
            // 不变量：CreateWindowExW 各句柄参数允许为空（顶层无父窗口）。
            let hwnd = unsafe {
                ffi::CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    ffi::WS_VISIBLE,
                    0,
                    0,
                    120,
                    120,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if hwnd.is_null() {
                None
            } else {
                Some(TempWindow(hwnd))
            }
        }

        fn ex_style(&self) -> u32 {
            unsafe { ffi::GetWindowLongPtrW(self.0, ffi::GWL_EXSTYLE) as u32 }
        }
    }

    impl Drop for TempWindow {
        fn drop(&mut self) {
            unsafe {
                ffi::DestroyWindow(self.0);
            }
        }
    }

    #[test]
    fn self_check_is_idempotent_ok_on_real_window() {
        // ⑤ self_check 幂等：连续两次 ok=true（真机临时窗口）。
        let Some(tw) = TempWindow::new() else {
            eprintln!("[dp-platform] 创建临时窗口失败（无桌面会话？）→ 跳过 self_check 幂等用例");
            return;
        };
        let w = WinPlatformWindow::new(tw.0 as isize, Arc::new(DisplayService::detect()))
            .expect("attach 临时窗口");

        let r1 = w.self_check().expect("self_check #1 不应返回 Err");
        assert!(r1.ok, "首次自检应通过：{r1:?}");
        assert_eq!(r1.missing_ex_style, 0, "三必要位应齐备");
        assert!(r1.topmost_ok, "置顶位应与计划一致");
        // 旁证：三必要位确实在位、TOPMOST 位置位。
        let ex = tw.ex_style();
        let required = WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0;
        assert_eq!(ex & required, required, "三必要位应在位：{ex:#010X}");
        assert_ne!(ex & WS_EX_TOPMOST.0, 0, "Always 模式下应置 TOPMOST");

        // 第二次应完全幂等（不来回翻转位）。
        let r2 = w.self_check().expect("self_check #2 不应返回 Err");
        assert!(r2.ok, "二次自检应仍通过：{r2:?}");
        assert_eq!(r1, r2, "self_check 应幂等：{r1:?} vs {r2:?}");
        assert_eq!(tw.ex_style(), ex, "self_check 不应改变样式位");
    }
}
