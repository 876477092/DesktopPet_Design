//! `dp-platform`：平台抽象层。
//!
//! 承载内容（`02 §3`）：`traits.rs`（端口定义）、`win/{window,display,tray,cursor,
//! winenum,system,...}.rs`（Windows 实装；S1-M3 托盘、S2-M6/S2-M7 感知已落地），
//! 以及后续 `hook/proc/autostart`（随 S3-M1 及后续模块增补）。
//!
//! 冻结契约（`02 §4.3` / `02 §5 K-1` / `02 §7.2`）：
//!   - `PlatformWindow`：透明 / 置顶三态 / 穿透 / 多屏换算，
//!     `SPC = (VDC − origin_vdc(monitor)) × scale`（RV-17）；
//!   - `HitSource`：钩子主 + `WM_NCHITTEST` 兜底双实现（实装在 S3）；本模块仅落端口与最小载体；
//!   - 窗口样式位：`WS_POPUP | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`，
//!     **不启用** `WS_EX_NOREDIRECTIONBITMAP`（`02 §2.3` B-09 / SG-M1 门禁结论）；
//!   - 穿透用运行时动态加/摘 `WS_EX_TRANSPARENT`（SG-M1 定版口径，非 `HTTRANSPARENT`）。
//!
//! 首个落地模块：**S1-M2**（平台窗口层：透明 / 置顶 / 全屏，T-02 段上）。
//!
//! ⚠️ 端口级最小定义提示：`Vec2` / `HitMask` / `HitResult` / `RawWindowHandle` 在 `02` 中
//! 只有签名没有定义，本 crate 给出最小可用实现（见 [`traits`] 文档），后续模块统一或替换。

pub mod traits;

#[cfg(windows)]
pub mod win;

pub use traits::{
    HitMask, HitResult, HitSource, HitZone, PlatformError, PlatformWindow, RawWindowHandle, Result,
    TopmostMode, Vec2,
};

#[cfg(windows)]
pub use win::tray;

#[cfg(windows)]
pub use win::{
    action_for_menu_id, fmt_label, foreground_fullscreen_monitor, icon_for_state, menu_spec,
    topmost_plan, DisplayService, FullscreenWatch, MonitorId, MonitorInfo, RectI, TopmostPlan,
    TrayAction, TrayIconState, TrayMenuState, TrayMenuItem, WatchAction, WinPlatform,
    WinPlatformWindow, ZOrder, RESTORE_DELAY_MS,
};
