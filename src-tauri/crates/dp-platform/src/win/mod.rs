//! Windows 平台实装装配（`02 §3`：`win/{window,display,...}.rs`）。
//!
//! 本模块提供 `WinPlatform` 装配入口：持有 `DisplayService`，把原生窗口句柄挂成
//! `WinPlatformWindow`。`tray`（S1-M3）、`cursor` / `winenum` / `system`（S2-M7 /
//! S2-M6）已落地；`session`（S4-M1 会话暂停检测）已落地；`proc` 等随后续模块增补，
//! 本文件只做装配与重导出。

pub mod cursor;
pub mod display;
// S5-M4（T-15 段 · 中 / `01 FR-1-9`）：开机自启——HKCU `...\CurrentVersion\Run` 读写。
// C9：新增 `Win32_System_Registry` feature（已登记 `src-tauri/Cargo.toml` 根清单），零网络。
pub mod autostart;
// S5-M4（T-15 段 · 中 / `01 FR-1-10` + `02 §5 K-7`）：原生对话框——退出确认 / 存档异常提示。
// 复用已启用的 `Win32_UI_WindowsAndMessaging`（`MessageBoxW`），零新增 feature、零新增依赖。
pub mod dialog;
// S3-M1（T-08 段 · 上 / `02 §5 K-2`）：全局低阶鼠标钩子（`WH_MOUSE_LL`）· 穿透兜底与幂等装卸。
pub mod hook;
// S7-M1（T-20 / `02 §5.6`）：全局低阶键盘钩子（`WH_KEYBOARD_LL`）· **只计数** + 隐私/穿透双 gate
// 幂等装卸（零新增依赖 / 零新增 feature）。
pub mod keyhook;
// S4-M1（T-11 段 · 上 / FR-1-10 / F10）：会话状态检测（锁屏 / 远程桌面）→ 情绪 P 暂停。
// C9：新增 `Win32_System_RemoteDesktop` feature（已登记 `Cargo.toml` 根清单），零网络。
pub mod session;
// S1-M3：托盘菜单契约与纯逻辑（`02 §5 K-9` / §5.23）。
pub mod tray;
// S6-M2（T-16 段 · 下 / 渲染自愈）：托盘气泡通知——`Shell_NotifyIconW` NIF_INFO 临时
// 图标气泡（自愈失败提示）。复用已启用的 `Win32_UI_Shell`，零新增 feature、零新增依赖。
pub mod tray_balloon;
// S7-M1（T-20 / FR-6-3 增量 / `02 §5.6`）：前台进程类别感知——文件名→小写→FNV-1a64，
// 明文不出函数（隐私红线）。复用既有 feature，零新增依赖、零网络（C9）。
pub mod proc;
pub mod window;
// S2-M7（T-07 / FR-6-3）：系统状态感知实装——电量 / CPU 负载差分 / 键鼠空闲采样。
pub mod system;
// S2-M6（T-05 段 · 下 / FR-6-4）：窗口枚举——标题栏平台数据源（失败降级空列表，R2）。
pub mod winenum;

pub use display::{select_monitor_at, DisplayService, MonitorId, MonitorInfo, RectI};
// S5-M4：开机自启（注册表 Run 项）——设置项「开机自启」的唯一落地点。
pub use autostart::{AutostartState, AUTOSTART_VALUE_NAME, RUN_KEY_PATH};
// S5-M4：原生对话框（退出确认 / 存档异常提示；零依赖，文案由调用方本地化）。
pub use dialog::DialogOutcome;
// S4-M1：会话暂停判定（轮询 + 迟滞；平台不可用时降级 idle 近似，P1-2）。
pub use session::{query_session_state, SessionState, SessionWatcher};
// S3-M1：低阶鼠标钩子端口与幂等服务（回调类型与 `CursorEventsHook` 同构：平台定义、app 注入）。
pub use hook::{HitTest, HookEvent, HookService, HookSink, MouseButton};
// S7-M1：键盘只计数钩子（计数器 / 后端端口 / 双 gate 服务）。
pub use keyhook::{KeyCounters, KeyHookBackend, KeyHookService};
pub use tray::{
    action_for_menu_id, fmt_label, icon_for_state, menu_spec, TrayAction, TrayIconState,
    TrayMenuState, TrayMenuItem,
};
// S7-M1：前台进程类别哈希（唯一出参 = u64；进程名明文不出该函数）。
pub use proc::{fnv1a64, foreground_process_hash};
pub use window::{
    foreground_fullscreen_monitor, topmost_plan, FullscreenWatch, TopmostPlan, WatchAction,
    WinPlatformWindow, ZOrder, RESTORE_DELAY_MS,
};

use std::sync::Arc;

use crate::traits::Result;

/// 平台装配入口。
pub struct WinPlatform {
    display: Arc<DisplayService>,
}

impl WinPlatform {
    /// 探测本机显示器并构造平台。
    #[must_use]
    pub fn new() -> Self {
        Self { display: Arc::new(DisplayService::detect()) }
    }

    /// 复用既有 `DisplayService` 构造平台。
    #[must_use]
    pub fn with_display(display: Arc<DisplayService>) -> Self {
        Self { display }
    }

    /// 取显示器服务（供坐标换算 / 显示器变更后 `refresh`）。
    #[must_use]
    pub fn display(&self) -> Arc<DisplayService> {
        Arc::clone(&self.display)
    }

    /// 把一个原生窗口句柄（`HWND` 指针值）挂接为平台窗口。
    pub fn attach(&self, hwnd: isize) -> Result<WinPlatformWindow> {
        WinPlatformWindow::new(hwnd, self.display())
    }
}

impl Default for WinPlatform {
    fn default() -> Self {
        Self::new()
    }
}
