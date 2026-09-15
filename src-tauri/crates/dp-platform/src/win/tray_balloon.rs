#![cfg(windows)]

//! 托盘气泡通知（**S6-M2**，`03 S6-M2` 卡片「连续 3 次自愈失败 → 重启进程并弹托盘提示」）。
//!
//! ## 为什么需要它
//! 自愈失败的提示必须走**托盘通知**（`Shell_NotifyIcon` 气泡），不能依赖 WebView
//! 前端（渲染进程可能已死，正是自愈失败的场景）。Tauri 2.11 的 `TrayIcon` 不提供
//! 通知 API（`show_notification` 不在该版本），故本模块用 `Shell_NotifyIconW`
//! 以**临时托盘图标**方式弹气泡：
//!
//! ```text
//! NIM_ADD（挂到调用方提供的宿主窗口，uID 取私有常量）
//!   → NIM_MODIFY（NIF_INFO，带标题/正文/超时）
//!   → NIM_DELETE（立即移除临时图标，避免常驻脏图标）
//! ```
//!
//! ## 边界与纪律
//! - **宿主窗口**：调用方传宠物窗口 HWND（自愈流程主线程执行，窗口存活）；不新建窗口；
//! - **失败降级**（`02 §7.4.2`）：`Shell_NotifyIcon` 任一环失败 → 返回 `false`，
//!   调用方按「无提示」处理（自愈失败仍会走 `app.restart()`，不阻塞主流程）；
//! - **零时钟**（C3）：`uTimeout` 固定 3000ms，不做任何时间读取；
//! - **文案由调用方传入**（本模块零中文硬编码，与 `dialog.rs` 同口径）；
//! - **测试**：真实气泡在单测内**不弹出**（CI 无交互桌面）；只单测纯映射逻辑
//!   （UTF-16 定长填充与截断）。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICONDATAW_0,
};

/// 临时图标的私有 uID（避免与 Tauri 托盘图标冲突；非 0 即视为私有）。
const BALLOON_UID: u32 = 0xD0CE;
/// 气泡显示时长（毫秒；`NIF_INFO` 的 `uTimeout` 固定值，C3 零时钟）。
const BALLOON_TIMEOUT_MS: u32 = 3_000;

/// 弹出托盘气泡通知。
///
/// - `hwnd`：宿主窗口句柄（宠物窗口 HWND；`isize` 指针值，与 `WinPlatformWindow` 同口径）；
/// - `title` / `body`：**已本地化**文案（正文超出 256 字符截断，标题超出 64 截断）；
/// - 返回 `false` = 气泡不可用（Shell 调用失败），调用方按「无提示」降级处理。
#[must_use]
pub fn show_balloon(hwnd: isize, title: &str, body: &str) -> bool {
    // HWND 为 0（未挂接）→ 直接按不可用处理，不弹。
    if hwnd == 0 {
        return false;
    }

    let mut data = NOTIFYICONDATAW {
        cbSize: core::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(hwnd as *mut core::ffi::c_void),
        uID: BALLOON_UID,
        uFlags: NIF_MESSAGE | NIF_TIP | NIF_INFO,
        uCallbackMessage: 0,
        hIcon: Default::default(),
        szTip: to_wide_fixed(""),
        dwState: Default::default(),
        dwStateMask: Default::default(),
        szInfo: to_wide_fixed(body),
        Anonymous: NOTIFYICONDATAW_0 { uTimeout: BALLOON_TIMEOUT_MS },
        szInfoTitle: to_wide_fixed(title),
        dwInfoFlags: Default::default(),
        guidItem: Default::default(),
        hBalloonIcon: Default::default(),
    };

    let ptr = &mut data as *mut NOTIFYICONDATAW;
    // Safety：`data` 为本函数持有的合法缓冲，`cbSize` 已按 SDK 规定填充；
    // 各环失败均返回 `false`（`Shell_NotifyIconW` 返回 `BOOL`，非零为成功）。
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, ptr) }.as_bool();
    if !added {
        return false;
    }
    // 打开现代气泡样式（NIM_SETVERSION 后 NIF_INFO 走新版气泡；失败不阻塞——旧版亦可显示）。
    let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, ptr) };
    let shown = unsafe { Shell_NotifyIconW(NIM_MODIFY, ptr) }.as_bool();
    // 无论是否显示成功，立即移除临时图标（避免常驻脏图标）。
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, ptr) };
    shown
}

/// UTF-16 定长填充（NUL 结尾；超长截断到 `N - 1`，C7 字符串安全；返回定长数组）。
#[must_use]
pub fn to_wide_fixed<const N: usize>(text: &str) -> [u16; N] {
    let mut buf = [0u16; N];
    for (i, unit) in text.encode_utf16().take(N.saturating_sub(1)).enumerate() {
        buf[i] = unit;
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_fixed_is_nul_terminated_and_padded() {
        let buf = to_wide_fixed::<8>("AB");
        assert_eq!(buf.len(), 8);
        assert_eq!(&buf[..3], &[0x41, 0x42, 0]);
        assert!(buf[3..].iter().all(|&u| u == 0));
    }

    #[test]
    fn wide_fixed_truncates_overlong() {
        let long = "中".repeat(300);
        let buf = to_wide_fixed::<8>(&long);
        assert_eq!(buf.len(), 8);
        // 8 字符容量：正文截断到 7 个码元 + NUL（UTF-16 码元单位）。
        assert_eq!(buf[7], 0);
        assert_ne!(buf[0], 0);
    }

    #[test]
    fn wide_fixed_empty_stays_empty() {
        let buf = to_wide_fixed::<4>("");
        assert_eq!(buf, [0, 0, 0, 0]);
    }
}
