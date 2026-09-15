#![cfg(windows)]

//! 原生对话框（`03 S5-M4`）——退出确认与存档异常提示。
//!
//! ## 为什么需要它
//! 两处需求**必须有一处原生 UI 承载**，且都不能依赖宠物窗口或设置窗口：
//!   1. **退出确认（`01 FR-1-10`）**：「退出前弹确认（含"保存进度"提示）」。退出动作由
//!      **托盘**触发，此时设置窗口可能从未打开（`visible:false`），宠物窗口可能是隐藏态，
//!      故不能复用 WebView 前端对话框；
//!   2. **存档异常提示（`02 §5 K-7`）**：「解析失败 → 隔离 `save.corrupt.<ts>.json` →
//!      默认档 + **原生提示**」。此时应用刚启动，任何 WebView 都还没就绪。
//!
//! 因此本模块用 Win32 `MessageBoxW` 提供最小原生对话框面（`Win32_UI_WindowsAndMessaging`
//! 已在 `windows` feature 面内，**零新增依赖**，C9 零网络）。
//!
//! ## 边界与纪律
//! - **阻塞语义**：`MessageBoxW` 是模态阻塞调用。本模块的两处调用点都在
//!   **非渲染/非 tick 线程**（托盘菜单事件回调 / `setup` 装配期），不阻塞 core-loop 或
//!   渲染帧循环；`flags` 一律不带 `MB_SERVICE_NOTIFICATION` 等前台抢占选项；
//! - **文案由调用方传入**（本模块零中文硬编码，便于 S5-M5 的 i18n 键值化）；
//! - **零时钟**（C3）：不做超时、不去抖；
//! - **失败降级**（`02 §7.4.2`）：对话框创建失败返回 [`DialogOutcome::Unavailable`]，
//!   调用方按「未确认」处理（危险动作保守不做），绝不 panic；
//! - **测试**：真实对话框在单测内**不弹出**（CI 无交互桌面）；只单测纯映射逻辑
//!   （[`DialogOutcome`] 与按钮 id 的对应关系）。

use windows::core::PCWSTR;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDCANCEL, IDNO, IDOK, IDYES, MB_DEFBUTTON2, MB_ICONINFORMATION,
    MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_YESNO, MESSAGEBOX_RESULT,
};

/// 对话框返回结果（把 Win32 按钮 id 归一化为三态，避免调用方理解 `MESSAGEBOX_RESULT`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogOutcome {
    /// 用户点击「确定 / 是」（肯定）。
    Confirmed,
    /// 用户点击「取消 / 否」（否定）。
    Dismissed,
    /// 对话框不可用（创建失败 / 无交互桌面）——调用方按**未确认**保守处理。
    Unavailable,
}

impl DialogOutcome {
    /// 是否为用户明确确认（`Unavailable` 不算确认）。
    #[must_use]
    pub fn is_confirmed(self) -> bool {
        matches!(self, Self::Confirmed)
    }

    /// 由 Win32 按钮 id 映射（纯函数，可单测）。
    #[must_use]
    pub fn from_button(result: MESSAGEBOX_RESULT) -> Self {
        match result {
            IDOK | IDYES => Self::Confirmed,
            IDCANCEL | IDNO => Self::Dismissed,
            // 未知 / 关闭（Esc）→ 否定（危险动作保守不做）。
            _ => Self::Dismissed,
        }
    }
}

/// 询问式确认（「确定 / 取消」或「是 / 否」二选一）。
///
/// - `title` / `body` 为**已本地化**的文案（调用方负责；本模块不内嵌文案）；
/// - `body` 中的 `\n` 会被 MessageBoxW 按换行渲染（调用方可用其分行写"保存进度"提示）；
/// - 默认按钮为**第二项**（`MB_DEFBUTTON2`）：回车默认落在「取消」上，符合"退出/重置"
///   这类破坏性动作的安全习惯。
#[must_use]
pub fn confirm(title: &str, body: &str) -> DialogOutcome {
    show(title, body, MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2)
}

/// 提示式对话框（仅「确定」；不返回确认语义，恒 [`DialogOutcome::Dismissed`]）。
///
/// 用于「存档已损坏并已自动恢复」这类**知会**信息（无选择项）。
pub fn notify(title: &str, body: &str) {
    let _ = show(title, body, MB_OK | MB_ICONWARNING);
}

/// 信息提示（`MB_ICONINFORMATION`；与 [`notify`] 同语义，供非异常类知会用）。
pub fn info(title: &str, body: &str) {
    let _ = show(title, body, MB_OK | MB_ICONINFORMATION);
}

// ---------------------------------------------------------------------------
// 内部实现
// ---------------------------------------------------------------------------

/// 弹出对话框并归一化结果（`Unavailable` 覆盖"没有交互桌面"的会话，如服务态启动）。
fn show(
    title: &str,
    body: &str,
    style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) -> DialogOutcome {
    let caption = to_wide(title);
    let text = to_wide(body);
    // SAFETY: 两个 PCWSTR 均由本函数栈上的 NUL 结尾缓冲区支撑，调用期间有效；
    // hwnd 传 HWND::default()（无父窗口）→ 对话框以桌面为父，不依赖任何 WebView。
    let result = unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            style,
        )
    };
    // 0 返回值 = 创建失败 / 无交互桌面 → Unavailable（保守：不当成确认）。
    if result.0 == 0 {
        return DialogOutcome::Unavailable;
    }
    DialogOutcome::from_button(result)
}

/// UTF-16（NUL 结尾）转换。
fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_ids_map_to_confirmed_or_dismissed() {
        assert_eq!(DialogOutcome::from_button(IDOK), DialogOutcome::Confirmed);
        assert_eq!(DialogOutcome::from_button(IDYES), DialogOutcome::Confirmed);
        assert_eq!(DialogOutcome::from_button(IDCANCEL), DialogOutcome::Dismissed);
        assert_eq!(DialogOutcome::from_button(IDNO), DialogOutcome::Dismissed);
    }

    #[test]
    fn unknown_button_is_treated_as_dismissed() {
        // 未知按钮（如超时/关闭）→ 否定：破坏性动作必须保守（绝不误判为确认）。
        assert_eq!(
            DialogOutcome::from_button(MESSAGEBOX_RESULT(9999)),
            DialogOutcome::Dismissed
        );
    }

    #[test]
    fn unavailable_is_not_a_confirmation() {
        assert!(!DialogOutcome::Unavailable.is_confirmed());
        assert!(DialogOutcome::Confirmed.is_confirmed());
        assert!(!DialogOutcome::Dismissed.is_confirmed());
    }
}
