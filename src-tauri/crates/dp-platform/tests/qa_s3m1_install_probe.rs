//! S3-M1 QA **最小安装探针**（**纯测试代码，不改任何生产代码**）。
//!
//! 目的：在**本沙箱**里实测 `SetWindowsHookExW(WH_MOUSE_LL, .., NULL, 0)` 到底
//! **装成功还是失败**，并以 `GetLastError` 给出失败原因；再把结果与
//! `HookService::new(...).is_installed()` 对照，判断「哪些验收在本环境真的被证明、
//! 哪些因装不上而被跳过」。**不得把 skipped 当 passed。**
//!
//! 该探针**不是** `#[ignore]`：它刻意在普通 `cargo test` 中执行，以便逐轮留证
//! 「本环境的安装能力」；即使装不上也只是**报告**（打印），不使套件变红——
//! 因为「本沙箱是否支持 LL 钩子」不是被测产品代码的属性。

#![cfg(windows)]

use std::ffi::c_void;
use std::os::raw::{c_int, c_uint};

use dp_platform::win::hook::{HitTest, HookEvent, HookService, HookSink};

// ---------------------------------------------------------------------------
// 最小 user32 / kernel32 FFI（仅探针用，不进入产品路径）
// ---------------------------------------------------------------------------

mod ffi {
    use super::*;

    pub type Hhook = *mut c_void;
    pub type Hmodule = *mut c_void;
    pub type Hwnd = *mut c_void;
    pub type HookProc = Option<unsafe extern "system" fn(c_int, usize, isize) -> isize>;

    /// 对齐的 MSG 裸缓冲（x64 下 MSG ≈ 48B）。
    #[repr(C, align(8))]
    pub struct RawMsg(pub [u8; 64]);

    #[link(name = "user32")]
    extern "system" {
        pub fn SetWindowsHookExW(id: c_int, lpfn: HookProc, hmod: Hmodule, tid: c_uint) -> Hhook;
        pub fn UnhookWindowsHookEx(hhk: Hhook) -> c_int;
        pub fn PeekMessageW(
            msg: *mut RawMsg,
            hwnd: Hwnd,
            min: c_uint,
            max: c_uint,
            remove: c_uint,
        ) -> c_int;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetCurrentThreadId() -> c_uint;
        pub fn GetLastError() -> c_uint;
    }

    /// `WH_MOUSE_LL`
    pub const WH_MOUSE_LL: c_int = 14;
    /// `PM_NOREMOVE`
    pub const PM_NOREMOVE: c_uint = 0x0000;
}

/// 空转回调（探针期间极少数真实事件可能触发；不做任何事、直接放行）。
unsafe extern "system" fn noop_hook_proc(code: c_int, wp: usize, lp: isize) -> isize {
    // 规范：负 nCode 必须放行。此处无下层句柄，直接返回 0（放行）。
    let _ = (code, wp, lp);
    0
}

/// 在一个自带消息队列的线程内尝试安装 `WH_MOUSE_LL`，返回 `Result<(), errno>`。
fn try_install_raw() -> Result<(), u32> {
    // 建消息队列（PostThreadMessageW 前提；此处仅为让环境与生产一致）。
    let mut msg = ffi::RawMsg([0u8; 64]);
    unsafe {
        let _ = ffi::PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, ffi::PM_NOREMOVE);
    }
    let tid = unsafe { ffi::GetCurrentThreadId() };
    eprintln!("[INSTALL-PROBE] 线程 id={tid}，已建消息队列，尝试 SetWindowsHookExW(WH_MOUSE_LL,..,NULL,0)");
    let hhk =
        unsafe { ffi::SetWindowsHookExW(ffi::WH_MOUSE_LL, Some(noop_hook_proc), std::ptr::null_mut(), 0) };
    if hhk.is_null() {
        let err = unsafe { ffi::GetLastError() };
        return Err(err);
    }
    eprintln!("[INSTALL-PROBE] 安装成功 hhk={hhk:?}（将立即卸载）");
    unsafe {
        let _ = ffi::UnhookWindowsHookEx(hhk);
    }
    Ok(())
}

/// 恒命中 / 计数 sink（与生产 `HookService` 接线同口径）。
struct AlwaysHit;
impl HitTest for AlwaysHit {
    fn contains(&self, _x: i32, _y: i32) -> bool {
        true
    }
}
struct CountSink;
impl HookSink for CountSink {
    fn on_event(&self, _ev: HookEvent) {}
}

/// 探针 1：裸 FFI 最小安装（判定本沙箱 OS 能力）。
#[test]
fn probe_raw_setwindowshookexw_wh_mouse_ll() {
    let result = try_install_raw();
    match result {
        Ok(()) => eprintln!("[INSTALL-PROBE] 结论：本沙箱 **可以** 安装 WH_MOUSE_LL（裸 FFI 成功）"),
        Err(err) => eprintln!(
            "[INSTALL-PROBE] 结论：本沙箱 **无法** 安装 WH_MOUSE_LL（裸 FFI 失败，GetLastError={err}）"
        ),
    }
    // 真断言：失败时 GetLastError 必非 0（区分「装不上」与「无错误却返回空」）。
    if let Err(err) = result {
        assert_ne!(err, 0, "SetWindowsHookExW 返回空但 GetLastError=0（异常）");
    }
}

/// 探针 2：经**生产** `HookService` 走一遍，报告 `is_installed()`（与探针 1 对照）。
#[test]
fn probe_hook_service_install_state() {
    let svc = HookService::new(
        std::sync::Arc::new(AlwaysHit),
        std::sync::Arc::new(CountSink),
        false,
        false,
    );
    let installed = svc.is_installed();
    eprintln!("[INSTALL-PROBE] HookService::new(ct=false, fsh=false).is_installed() = {installed}");
    // 降级不崩：无论装上与否，调用都不得 panic；shutdown 幂等。
    svc.shutdown();
    assert!(!svc.is_installed(), "shutdown() 后必须为未安装态（幂等卸载）");
    svc.shutdown();
}
