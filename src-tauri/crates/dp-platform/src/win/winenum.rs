//! 窗口枚举（S2-M6，T-05 段 · 下 / FR-6-4）：可见窗口标题栏候选枚举。
//!
//! 职责（`03 §2` S2-M6；`02 §5` K-3 平台图数据源）：
//!   - 全量枚举可见窗口的标题栏候选 [`TitlebarWindow`]（物理屏幕坐标 SPC），
//!     供上层经 DPI 换算（C6/RV-17）注入 `dp_core::motion::platform::PlatformInputs`；
//!   - 过滤规则六条（纯函数 [`titlebar_candidate`] 承载，Win32 薄层只取值）：
//!     可见 / 非最小化 / 非壳层 / 无 `WS_EX_TOOLWINDOW` 位 / 标题非空 /
//!     无 DWM cloaked（UWP 幽灵窗口跳过）；
//!   - 降级口径（`02 §10` R2）：整体枚举失败 → 返回已收集部分或空 `Vec`，
//!     不 panic、不返回 `Err`；单窗口属性读取失败 → 仅跳过该窗口。
//!
//! Win32 边界（风格参照 `win::window`）：
//!   - 矩形读取 `DWMWA_EXTENDED_FRAME_BOUNDS` 优先（规避 Win11 阴影边框差异），
//!     失败回退 `GetWindowRect`；标题读取上限 256 个 UTF-16 码元（候选用途足够）；
//!   - `EnumWindows` 回调为 `unsafe extern "system" fn` + `lparam` 裸指针传
//!     `&mut Vec`，回调内只做取值与 push（轻量，无阻塞操作）；windows 0.61 的
//!     `EnumWindows` wrapper 仅暴露 closure 形式，此处以 non-capturing 闭包
//!     转发到 `extern "system" fn` 等价实现；
//!   - 整体失败判定：`EnumWindows` 返回 FALSE 且 `GetLastError() !=
//!     ERROR_NO_MORE_FILES`（非「无更多窗口」的正常终止）→ 视为整体失败；
//!     两种情况降级行为一致（见上，R2）。
//!
//! ⚠️ 本文件不依赖 serde / tracing（dp-platform 仅 thiserror + windows，C9）。

use windows::core::BOOL;
use windows::Win32::Foundation::{GetLastError, ERROR_NO_MORE_FILES, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongPtrW, GetWindowRect, GetWindowTextW, IsIconic, IsWindowVisible,
    GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

use super::display::RectI;
use super::window::is_shell_window;

// ---------------------------------------------------------------------------
// 候选模型 + 纯判定函数
// ---------------------------------------------------------------------------

/// 可见窗口标题栏候选（物理屏幕坐标，SPC）。
#[derive(Clone, Debug, PartialEq)]
pub struct TitlebarWindow {
    /// 窗口句柄（`HWND` 指针值，仅本次会话内有效）。
    pub hwnd: isize,
    /// 窗口标题（`GetWindowTextW`，非空）。
    pub title: String,
    /// 窗口外接矩形（虚拟桌面物理像素）：`DWMWA_EXTENDED_FRAME_BOUNDS` 优先，
    /// 失败回退 `GetWindowRect`（与 `win::window` 同款口径）。
    pub rect: RectI,
}

/// 标题栏平台候选判定（纯函数，参数取自 Win32 属性读取；单测不依赖 Win32 会话）。
///
/// 六条过滤规则（`03 §2` S2-M6 / `02 §5` K-3 平台图数据源）：任一不满足 →
/// 非候选。`ex_style` 只关心 `WS_EX_TOOLWINDOW` 位，其余位不影响判定。
#[must_use]
pub fn titlebar_candidate(
    visible: bool,
    iconic: bool,
    shell: bool,
    ex_style: u32,
    title_len: usize,
    cloaked: bool,
) -> bool {
    visible
        && !iconic
        && !shell
        && (ex_style & WS_EX_TOOLWINDOW.0) == 0
        && title_len > 0
        && !cloaked
}

// ---------------------------------------------------------------------------
// 枚举入口（FR-6-4：2s 周期，供 PlatformGraph 重建）
// ---------------------------------------------------------------------------

/// 枚举可见窗口标题栏候选（FR-6-4：2s 周期，供 PlatformGraph 重建）。
///
/// 降级口径（`02 §10` R2）：整体枚举失败 → 返回已收集部分或空 `Vec`，不
/// panic、不返回 `Err`；单窗口属性读取失败 → 仅跳过该窗口。
pub fn enumerate_titlebar_windows() -> Vec<TitlebarWindow> {
    let mut out: Vec<TitlebarWindow> = Vec::new();
    // 不变量：裸指针随 lparam 注入回调；EnumWindows 同步单线程执行，枚举期间
    // out 不被其他代码触碰，指针在回调期间有效。
    let out_ptr = &mut out as *mut Vec<TitlebarWindow>;
    // 不变量：回调恒返回 TRUE（不提前终止），整体失败走 R2 降级口径；
    // lparam 裸指针随双参 API 注入，回调内只做取值与 push（轻量）。
    let enum_result = unsafe { EnumWindows(Some(titlebar_enum_proc), LPARAM(out_ptr as isize)) };

    // 整体失败判定（口径见模块文档）：回调恒返回 TRUE、不提前终止，Err 仅源于
    // 系统级失败；GetLastError 的 ERROR_NO_MORE_FILES（正常终止）判定随 wrapper
    // 消耗错误码不复可得，两种情况降级行为一致（返回已收集部分），此处仅读取留档。
    if let Err(_err) = enum_result {
        let _terminated_normally = unsafe { GetLastError() } == ERROR_NO_MORE_FILES;
        let _ = _terminated_normally;
    }
    out
}

/// `EnumWindows` 回调（`unsafe extern "system" fn` + lparam 裸指针）。
///
/// 轻量契约：只做单窗口取值与 push，不做任何阻塞 / 重操作；恒返回 `TRUE`
/// （不提前终止枚举，整体失败走 R2 降级口径）。
unsafe extern "system" fn titlebar_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // 安全性：lparam 由调用方（[`enumerate_titlebar_windows`]）注入 `&mut Vec`
    // 裸指针，枚举为同步单线程调用，指针在回调期间有效。
    let out = &mut *(lparam.0 as *mut Vec<TitlebarWindow>);
    collect_titlebar(hwnd, out);
    BOOL(1)
}

/// 单窗口候选收集（回调体）：过滤判定 + 矩形读取；任一属性读取失败仅跳过该窗口。
///
/// # Safety
/// `hwnd` 须为 EnumWindows 传入的有效窗口句柄；本函数只读，不产生副作用。
unsafe fn collect_titlebar(hwnd: HWND, out: &mut Vec<TitlebarWindow>) {
    let visible = IsWindowVisible(hwnd).as_bool();
    let iconic = IsIconic(hwnd).as_bool();
    let shell = is_shell_window(hwnd);
    let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    let title = window_title(hwnd);
    let cloaked = dwm_cloaked(hwnd);

    // 过滤判定抽成纯函数（`titlebar_candidate`），Win32 薄层只取值。
    if !titlebar_candidate(visible, iconic, shell, ex_style, title.chars().count(), cloaked) {
        return;
    }

    // 窗口外接矩形：DWM 扩展边框优先，失败回退 GetWindowRect；再失败跳过该窗口。
    let Some(rect) = dwm_extended_bounds(hwnd).or_else(|| window_rect(hwnd)) else {
        return;
    };
    out.push(TitlebarWindow { hwnd: hwnd.0 as isize, title, rect });
}

// ---------------------------------------------------------------------------
// Win32 取值薄层（只读）
// ---------------------------------------------------------------------------

/// 读取窗口标题（失败返回空串）。
fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // 不变量：GetWindowTextW 返回拷贝的 UTF-16 码元数（不含结尾 null），上限 buf 长度。
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// 读取 DWM cloaked 状态（UWP 幽灵窗口判定；DWM 不可用视为未 cloaked）。
fn dwm_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // 不变量：出参 u32 与 DWMWA_CLOAKED 要求的类型大小一致。
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            core::mem::size_of::<u32>() as u32,
        )
    };
    hr.is_ok() && cloaked != 0
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

/// 读取窗口矩形（DWM 不可用时的兜底）；失败返回 `None`。
///
/// # Safety
/// `hwnd` 须为有效窗口句柄。
unsafe fn window_rect(hwnd: HWND) -> Option<RectI> {
    let mut rect = RECT::default();
    GetWindowRect(hwnd, &mut rect).ok()?;
    Some(RectI::new(rect.left, rect.top, rect.right, rect.bottom))
}

// ---------------------------------------------------------------------------
// 单元测试（纯逻辑部分，不依赖真机窗口）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- titlebar_candidate 六条规则（`03 §2` S2-M6 过滤条件） ------------------

    #[test]
    fn titlebar_candidate_accepts_clean_window() {
        // 全干净基线：可见 / 非最小化 / 非壳层 / 无工具窗口位 / 标题非空 / 未 cloaked。
        assert!(titlebar_candidate(true, false, false, 0, 4, false));
        // 其他扩展样式位（此处取 WS_EX_TOPMOST=0x8）不误伤判定。
        assert!(titlebar_candidate(true, false, false, 0x0000_0008, 1, false));
    }

    #[test]
    fn titlebar_candidate_rejects_invisible_window() {
        assert!(!titlebar_candidate(false, false, false, 0, 4, false));
    }

    #[test]
    fn titlebar_candidate_rejects_iconic_window() {
        assert!(!titlebar_candidate(true, true, false, 0, 4, false));
    }

    #[test]
    fn titlebar_candidate_rejects_shell_window() {
        assert!(!titlebar_candidate(true, false, true, 0, 4, false));
    }

    #[test]
    fn titlebar_candidate_rejects_toolwindow_bit() {
        // WS_EX_TOOLWINDOW 位置位 → 排除（工具窗 / 宠物自身窗口等）。
        assert!(!titlebar_candidate(true, false, false, WS_EX_TOOLWINDOW.0, 4, false));
    }

    #[test]
    fn titlebar_candidate_rejects_empty_title() {
        assert!(!titlebar_candidate(true, false, false, 0, 0, false));
    }

    #[test]
    fn titlebar_candidate_rejects_cloaked_uwp_ghost() {
        // DWM cloaked（UWP 幽灵窗口）→ 排除。
        assert!(!titlebar_candidate(true, false, false, 0, 4, true));
    }
}
