//! QA 独立补充测试（S1-M2，QA 严过关新增，**不改动工程师生产代码**）。
//!
//! 目的：补工程师单测未覆盖的边界与错误路径，并对「需要真实 HWND 才能验证」的
//! `ensure_styles` 幂等性 / `set_topmost(Never)` 清位 / `set_click_through` 往返
//! 做真机级断言（自建临时窗口，不依赖 DesktopPet 进程）。
//!
//! ⚠️ 本文件由 QA 添加；`desired_ex_style` 为私有函数，无法从外部 crate 调用，
//! 故其位运算语义通过 `ensure_styles` 的真机读回间接覆盖。

#![cfg(windows)]

use std::ffi::c_void;
use std::sync::Arc;

use dp_platform::win::window::{covers, is_shell_class_name, style_has_frame};
use dp_platform::{
    topmost_plan, DisplayService, FullscreenWatch, MonitorId, MonitorInfo, PlatformWindow, RectI,
    TopmostMode, Vec2, WatchAction, WinPlatformWindow, ZOrder, RESTORE_DELAY_MS,
};

// ---------------------------------------------------------------------------
// 补充：全屏迟滞状态机精确边界（2.99s / 3.0s / 3.01s）
// ---------------------------------------------------------------------------

#[test]
fn hysteresis_exact_boundaries_2s99_3s00_3s01() {
    // 观测：t=0 全屏，t=1000 全屏（last=1000），t=2000 退出全屏
    let mut w = FullscreenWatch::new(TopmostMode::BelowFullscreen);
    assert_eq!(w.on_poll(0, true), WatchAction::Hide);
    assert_eq!(w.on_poll(1_000, true), WatchAction::None);

    // 退出后：距最近全屏观测 1000 的差值
    assert_eq!(RESTORE_DELAY_MS, 3_000);
    // 3999ms - 1000ms = 2999 < 3000 → 不恢复
    assert_eq!(w.on_poll(3_999, false), WatchAction::None);
    assert!(w.is_hidden_for_fullscreen(), "2.99s 不应恢复");
    // 4000ms - 1000ms = 3000 == 3000 → 恢复（>= 边界）
    assert_eq!(w.on_poll(4_000, false), WatchAction::Show);
    assert!(!w.is_hidden_for_fullscreen(), "3.0s 边界应恢复");
    // 再轮询不重复动作
    assert_eq!(w.on_poll(4_001, false), WatchAction::None);
}

#[test]
fn hysteresis_short_fullscreen_does_not_misfire_restore() {
    // 全屏只持续 200ms（跨两个轮询之间），退出后未满 3s 不得恢复
    let mut w = FullscreenWatch::new(TopmostMode::BelowFullscreen);
    assert_eq!(w.on_poll(10_000, true), WatchAction::Hide); // last=10000
    assert_eq!(w.on_poll(10_200, false), WatchAction::None); // 200ms 后退出
    assert_eq!(w.on_poll(12_999, false), WatchAction::None); // 距 10000 为 2999ms
    assert!(w.is_hidden_for_fullscreen());
    assert_eq!(w.on_poll(13_000, false), WatchAction::Show); // 距 10000 为 3000ms
}

#[test]
fn watch_never_mode_returns_none_even_when_fullscreen() {
    let mut w = FullscreenWatch::new(TopmostMode::Never);
    for t in [0, 1_000, 5_000, 100_000] {
        assert_eq!(w.on_poll(t, true), WatchAction::None);
    }
    assert!(!w.is_hidden_for_fullscreen());
    assert_eq!(w.plan().zorder, ZOrder::NoTopmost);
}

// ---------------------------------------------------------------------------
// 补充：TopmostMode 三态（含 Never 恒 NoTopmost，覆盖工程师未显式断言的组合）
// ---------------------------------------------------------------------------

#[test]
fn topmost_plan_never_always_notopmost_regardless_of_state() {
    for fs in [false, true] {
        for hidden in [false, true] {
            assert_eq!(topmost_plan(TopmostMode::Never, fs, hidden).zorder, ZOrder::NoTopmost);
            assert_eq!(topmost_plan(TopmostMode::Never, fs, hidden).visible, None);
            assert_eq!(topmost_plan(TopmostMode::Always, fs, hidden).zorder, ZOrder::Topmost);
            assert_eq!(topmost_plan(TopmostMode::Always, fs, hidden).visible, None);
        }
    }
}

// ---------------------------------------------------------------------------
// 补充：covers 容差边界 / style_has_frame / shell 类名
// ---------------------------------------------------------------------------

#[test]
fn covers_tolerance_boundary_exact() {
    let m = RectI::new(0, 0, 1920, 1080);
    // 以容差 4 为例：每边差 <=4 通过，差 5 不通过
    assert!(covers(RectI::new(4, 4, 1916, 1076), m, 4));
    assert!(!covers(RectI::new(5, 0, 1920, 1080), m, 4));
    assert!(!covers(RectI::new(0, 0, 1915, 1080), m, 4));
    // tolerance=0 时任何缩进都不覆盖
    assert!(!covers(RectI::new(1, 0, 1920, 1080), m, 0));
}

#[test]
fn style_has_frame_uses_caption_or_thickframe_only() {
    const WS_CAPTION: u32 = 0x00C0_0000;
    const WS_THICKFRAME: u32 = 0x0004_0000;
    const WS_POPUP: u32 = 0x8000_0000;
    assert!(style_has_frame(WS_CAPTION));
    assert!(style_has_frame(WS_THICKFRAME));
    assert!(style_has_frame(WS_POPUP | WS_THICKFRAME));
    assert!(!style_has_frame(WS_POPUP));
    // 全屏视频常见：仅 WS_POPUP|WS_VISIBLE
    assert!(!style_has_frame(WS_POPUP | 0x1000_0000));
}

#[test]
fn shell_class_name_membership() {
    for n in ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"] {
        assert!(is_shell_class_name(n), "{n} 应视为壳层");
    }
    assert!(!is_shell_class_name("Tauri Window"));
    assert!(!is_shell_class_name("Progman2"));
}

// ---------------------------------------------------------------------------
// 补充：MonitorInfo 换算（负原点 + 非 1 缩放）纯函数，含 scale 防御
// ---------------------------------------------------------------------------

fn mk(id: u64, ox: f32, oy: f32, scale: f32, rc: RectI) -> MonitorInfo {
    MonitorInfo {
        id: MonitorId(id),
        origin_vdc: Vec2::new(ox, oy),
        scale,
        rc_monitor: rc,
        rc_work: rc,
        device_id: format!("DISPLAY{id}"),
        primary: id == 1,
    }
}

#[test]
fn monitor_roundtrip_negative_origin_and_scale() {
    // 右上副屏：物理 (1920, -1080)-(3840, 0)，scale=2.0 → origin_vdc=(960, -540)
    let m = mk(9, 960.0, -540.0, 2.0, RectI::new(1920, -1080, 3840, 0));
    for v in [
        Vec2::new(960.0, -540.0),
        Vec2::new(1000.0, -400.0),
        Vec2::new(1900.0, -100.0),
    ] {
        let spc = m.vdc_to_spc(v);
        let back = m.spc_to_vdc(spc);
        assert!((back.x - v.x).abs() < 1e-3 && (back.y - v.y).abs() < 1e-3, "{v:?}");
        let (px, py) = m.vdc_to_physical(v);
        let b2 = m.physical_to_vdc(px, py);
        assert!((b2.x - v.x).abs() < 1e-3 && (b2.y - v.y).abs() < 1e-3, "{v:?}");
    }
    assert_eq!(m.vdc_width(), 960.0); // 1920px / 2.0
    assert_eq!(m.vdc_height(), 540.0); // 1080px / 2.0
}

#[test]
fn size_logical_to_px_uses_user_and_monitor_scale_and_defends_invalid() {
    let m = mk(1, 0.0, 0.0, 1.25, RectI::new(0, 0, 1920, 1080));
    assert_eq!(m.size_logical_to_px(128, 1.0), 160); // 128*1.0*1.25
    assert_eq!(m.size_logical_to_px(128, 2.0), 320);
    // 非法 userScale 退化为 1.0；非法 monitor scale 亦退化（用 safe_scale）
    assert_eq!(m.size_logical_to_px(128, f32::NAN), 160);
    assert_eq!(m.size_logical_to_px(128, 0.0), 160);
    assert_eq!(m.size_logical_to_px(128, -3.0), 160);
    assert_eq!(m.size_logical_to_px(0, 1.0), 1); // 至少 1px
}

#[test]
fn display_service_injection_and_lookup() {
    let a = mk(1, 0.0, 0.0, 1.5, RectI::new(0, 0, 1920, 1080));
    let b = mk(2, -1920.0, 0.0, 1.0, RectI::new(-1920, 0, 0, 1080));
    let svc = DisplayService::from_monitors(vec![a.clone(), b.clone()]);
    assert_eq!(svc.monitors().len(), 2);
    assert_eq!(svc.monitor_by_id(MonitorId(2)).unwrap().origin_vdc.x, -1920.0);
    assert_eq!(svc.primary().unwrap().id, MonitorId(1));
    // monitor_at 命中负坐标副屏
    assert_eq!(svc.monitor_at(Vec2::new(-100.0, 100.0)).id, MonitorId(2));
    // 无 primary 标记时 primary() 取第一个
    let c = mk(3, 0.0, 0.0, 1.0, RectI::new(0, 0, 100, 100));
    let svc2 = DisplayService::from_monitors(vec![c]);
    assert_eq!(svc2.primary().unwrap().id, MonitorId(3));
}

// ---------------------------------------------------------------------------
// 补充：真机 DisplayService::detect()/refresh() —— 仅断言不 panic / 降级语义
// ---------------------------------------------------------------------------

#[test]
fn display_service_detect_and_refresh_do_not_panic() {
    let svc = DisplayService::detect();
    let list = svc.monitors();
    // refresh 应当成功（失败会被降级为空，此处允许空但不允许 panic）
    let r = svc.refresh();
    assert!(r.is_ok(), "refresh 失败：{r:?}");
    let list2 = svc.monitors();
    if !list2.is_empty() {
        for m in &list2 {
            assert!(m.scale.is_finite() && m.scale > 0.0, "scale 非法：{m:?}");
            assert!(m.origin_vdc.x.is_finite() && m.origin_vdc.y.is_finite());
            assert!(m.rc_monitor.width() > 0 && m.rc_monitor.height() > 0);
        }
    }
    eprintln!("[qa] detect={} monitors, after refresh={} monitors", list.len(), list2.len());
}

// ---------------------------------------------------------------------------
// 真机 HWND：ensure_styles 幂等 / set_topmost(Never) 清位 / click_through 往返
// ---------------------------------------------------------------------------

mod ffi {
    use super::c_void;
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
        pub fn SetForegroundWindow(hwnd: Hwnd) -> c_int;
        pub fn GetForegroundWindow() -> Hwnd;
        pub fn IsWindowVisible(hwnd: Hwnd) -> c_int;
        pub fn SetWindowPos(
            hwnd: Hwnd,
            after: Hwnd,
            x: c_int,
            y: c_int,
            cx: c_int,
            cy: c_int,
            flags: c_uint,
        ) -> c_int;
    }

    pub const SWP_SHOWWINDOW: u32 = 0x0040;
    pub const HWND_TOP: Hwnd = std::ptr::null_mut();

    pub const GWL_EXSTYLE: c_int = -20;
    pub const GWL_STYLE: c_int = -16;

    pub const WS_POPUP: u32 = 0x8000_0000;
    pub const WS_CAPTION: u32 = 0x00C0_0000;
    pub const WS_VISIBLE: u32 = 0x1000_0000;
    pub const WS_SYSMENU: u32 = 0x0008_0000;
    pub const WS_MINIMIZEBOX: u32 = 0x0002_0000;
    pub const WS_MAXIMIZEBOX: u32 = 0x0001_0000;
    pub const WS_CLIPSIBLINGS: u32 = 0x0400_0000;

    pub const WS_EX_TOPMOST: u32 = 0x0000_0008;
    pub const WS_EX_TRANSPARENT: u32 = 0x0000_0020;
    pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    pub const WS_EX_APPWINDOW: u32 = 0x0004_0000;
    pub const WS_EX_LAYERED: u32 = 0x0008_0000;
    pub const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
    pub const WS_EX_NOREDIRECTIONBITMAP: u32 = 0x0020_0000;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct TempWindow(ffi::Hwnd);

impl TempWindow {
    fn new() -> Self {
        let class = wide("STATIC");
        // 初始扩展样式：模拟 tao 默认（APPWINDOW + WINDOWEDGE 等既有位）。
        // 基础样式模拟 tao pet 实测 0x14CB0000（含 WS_CAPTION、无 WS_POPUP）。
        let ex = ffi::WS_EX_APPWINDOW | 0x0000_0100 /*WINDOWEDGE*/;
        let base = ffi::WS_VISIBLE
            | ffi::WS_CAPTION
            | ffi::WS_SYSMENU
            | ffi::WS_MINIMIZEBOX
            | ffi::WS_MAXIMIZEBOX
            | ffi::WS_CLIPSIBLINGS;
        let hwnd = unsafe {
            ffi::CreateWindowExW(
                ex,
                class.as_ptr(),
                std::ptr::null(),
                base,
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
        assert!(!hwnd.is_null(), "CreateWindowExW 失败（环境无桌面？）");
        TempWindow(hwnd)
    }

    fn ex_style(&self) -> u32 {
        unsafe { ffi::GetWindowLongPtrW(self.0, ffi::GWL_EXSTYLE) as u32 }
    }

    fn style(&self) -> u32 {
        unsafe { ffi::GetWindowLongPtrW(self.0, ffi::GWL_STYLE) as u32 }
    }
}

impl Drop for TempWindow {
    fn drop(&mut self) {
        unsafe {
            ffi::DestroyWindow(self.0);
        }
    }
}

fn attach(tw: &TempWindow) -> WinPlatformWindow {
    WinPlatformWindow::new(tw.0 as isize, Arc::new(DisplayService::detect())).expect("attach")
}

#[test]
fn ensure_styles_is_idempotent_and_sets_required_clears_forbidden() {
    let tw = TempWindow::new();
    let w = attach(&tw);

    // 第一次补写
    w.ensure_styles().expect("ensure_styles #1");
    let s1 = tw.ex_style();
    assert_ne!(s1 & ffi::WS_EX_LAYERED, 0, "LAYERED 未置位");
    assert_ne!(s1 & ffi::WS_EX_TOOLWINDOW, 0, "TOOLWINDOW 未置位");
    assert_ne!(s1 & ffi::WS_EX_NOACTIVATE, 0, "NOACTIVATE 未置位");
    assert_eq!(s1 & ffi::WS_EX_NOREDIRECTIONBITMAP, 0, "NOREDIRECTIONBITMAP 应清除");
    assert_eq!(s1 & ffi::WS_EX_APPWINDOW, 0, "APPWINDOW 应清除");

    // 幂等：连续第二次结果完全一致（不来回翻转位）
    w.ensure_styles().expect("ensure_styles #2");
    let s2 = tw.ex_style();
    assert_eq!(s1, s2, "ensure_styles 非幂等：{s1:#010X} -> {s2:#010X}");
    w.ensure_styles().expect("ensure_styles #3");
    assert_eq!(tw.ex_style(), s2, "ensure_styles 第三次仍变化");
}

#[test]
fn topmost_never_clears_and_always_restores_topmost_bit() {
    let tw = TempWindow::new();
    let w = attach(&tw);

    w.set_topmost(TopmostMode::Always).expect("always");
    assert_ne!(tw.ex_style() & ffi::WS_EX_TOPMOST, 0, "Always 未置 TOPMOST");

    // ensure_styles 后 TOPMOST 必须保留（不被抹掉）
    w.ensure_styles().expect("ensure");
    assert_ne!(tw.ex_style() & ffi::WS_EX_TOPMOST, 0, "ensure_styles 抹掉了 TOPMOST");

    w.set_topmost(TopmostMode::Never).expect("never");
    assert_eq!(tw.ex_style() & ffi::WS_EX_TOPMOST, 0, "Never 未清除 TOPMOST");

    w.set_topmost(TopmostMode::Always).expect("always again");
    assert_ne!(tw.ex_style() & ffi::WS_EX_TOPMOST, 0, "恢复 Always 失败");
}

#[test]
fn click_through_toggle_roundtrip_restores_original_bit() {
    let tw = TempWindow::new();
    let w = attach(&tw);
    w.ensure_styles().expect("ensure");
    let base = tw.ex_style();
    assert_eq!(base & ffi::WS_EX_TRANSPARENT, 0, "初始不应穿透");

    w.set_click_through(true).expect("ct on");
    let on = tw.ex_style();
    assert_ne!(on & ffi::WS_EX_TRANSPARENT, 0, "开启穿透未置位");
    // 其它关键位保持
    assert_eq!(on & ffi::WS_EX_LAYERED, base & ffi::WS_EX_LAYERED);
    assert_eq!(on & ffi::WS_EX_TOOLWINDOW, base & ffi::WS_EX_TOOLWINDOW);
    assert_eq!(on & ffi::WS_EX_NOACTIVATE, base & ffi::WS_EX_NOACTIVATE);
    assert!(w.is_click_through());

    w.set_click_through(false).expect("ct off");
    let off = tw.ex_style();
    assert_eq!(off & ffi::WS_EX_TRANSPARENT, 0, "关闭穿透未摘除");
    // 往返后回到原状（除 TRelATED 之外完全一致）
    assert_eq!(off, base, "穿透往返后样式位未复原：{base:#010X} -> {off:#010X}");
    assert!(!w.is_click_through());
}

#[test]
fn ensure_styles_does_not_rewrite_base_style() {
    // 佐证「WS_POPUP 偏离」：ensure_styles 只动扩展样式，基础样式保持原样。
    let tw = TempWindow::new();
    let base_before = tw.style();
    let w = attach(&tw);
    w.ensure_styles().expect("ensure");
    w.set_topmost(TopmostMode::Always).expect("topmost");
    assert_eq!(tw.style(), base_before, "基础样式被改写");
    // 基础样式仍含 WS_CAPTION、且无 WS_POPUP（说明未强改基础样式，仅维护扩展样式位）
    assert_ne!(tw.style() & ffi::WS_CAPTION, 0);
    assert_eq!(tw.style() & ffi::WS_POPUP, 0);
}

// ---------------------------------------------------------------------------
// 真机：AC-11 端到端 —— 合成「无边框全屏窗口」→ 检测 → BelowFullscreen 隐藏/恢复
// ---------------------------------------------------------------------------

/// 造一个覆盖主屏、无标题栏/无粗边框的全屏 WS_POPUP 并置前台。
fn make_fullscreen_foreground(rc: RectI) -> ffi::Hwnd {
    let class = wide("STATIC");
    let hwnd = unsafe {
        ffi::CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            ffi::WS_POPUP,
            rc.left,
            rc.top,
            rc.width(),
            rc.height(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert!(!hwnd.is_null(), "创建合成全屏窗口失败");
    unsafe {
        ffi::SetWindowPos(
            hwnd,
            ffi::HWND_TOP,
            rc.left,
            rc.top,
            rc.width(),
            rc.height(),
            ffi::SWP_SHOWWINDOW,
        );
        ffi::SetForegroundWindow(hwnd);
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    hwnd
}

/// 环境守卫：当前前台全屏源（三重校验口径）是否恰为本测试自建的弹窗。
///
/// AC-11（Q3 收紧）：只有当「检测到的全屏窗口 = 自己刚创建并置前台的弹窗」时，
/// 判定结果才可信；其他全屏窗口在场或前台被抢占时一律交由调用方跳过。
fn foreground_is_own_popup(popup: ffi::Hwnd) -> bool {
    let fg = unsafe { ffi::GetForegroundWindow() };
    fg == popup
}

/// 环境守卫：当前桌面是否不存在任何全屏窗口（`foreground_fullscreen_monitor()` 为 `None`）。
fn no_fullscreen_present() -> bool {
    dp_platform::foreground_fullscreen_monitor().is_none()
}

/// AC-11 真机端到端（BelowFullscreen 全屏隐藏 → 迟滞恢复，Q3 收紧后口径）。
///
/// 判别纪律（04 检查清单 Q3）：
///   - **全屏源必须 = 本测试自建的弹窗**：`SetForegroundWindow` 后校验前台确为本弹窗，
///     否则进入断言路径的一切判定都不可信；
///   - **早退 ≠ 通过**：任何环境阻塞（外部全屏窗口在场 / 前台受限 / 无主显示器）
///     一律打印 `[SKIPPED]` 标记并提前返回——输出中凡出现 `[SKIPPED]` 即表示
///     本条链路**未在真机被验证**（官方 ignored 语义等价物），不得据 ok 计入验收；
///   - Hide/Show 的生产语义（迟滞状态机）由 `window.rs` 纯逻辑单测全量覆盖，
///     本用例只负责真机链路冒烟。
#[test]
fn ac11_below_fullscreen_end_to_end_on_real_window() {
    let svc = DisplayService::detect();
    let Some(mon) = svc.primary() else {
        eprintln!("[SKIPPED] AC-11：无主显示器（无桌面会话？），真机端到端验证未执行");
        return;
    };

    // 前置守卫：桌面已存在其他全屏窗口 → 环境不隔离，跳过而非误失败。
    if let Some(other) = dp_platform::foreground_fullscreen_monitor() {
        eprintln!(
            "[SKIPPED] AC-11：测试启动前桌面已有外部全屏窗口（monitor={other:?}），环境不隔离，真机端到端验证未执行"
        );
        return;
    }

    // 被测窗口（模拟 pet）
    let tw = TempWindow::new();
    let w = attach(&tw);
    w.set_topmost(TopmostMode::BelowFullscreen).expect("below-fullscreen");

    // ① 合成全屏并置前台；前台非本测试弹窗（SetForegroundWindow 受限）→ 明确跳过
    let popup = make_fullscreen_foreground(mon.rc_monitor);
    if !foreground_is_own_popup(popup) {
        eprintln!(
            "[SKIPPED] AC-11：SetForegroundWindow 受限（前台非本测试自建弹窗），全屏隐藏→恢复链路未验证"
        );
        unsafe {
            ffi::DestroyWindow(popup);
        }
        return;
    }
    let detected = dp_platform::foreground_fullscreen_monitor();
    assert_eq!(detected, Some(mon.id), "全屏检测未识别合成全屏窗口");

    // ② 第一次轮询 → Hide，且窗口真的被隐藏、取消置顶
    assert_eq!(w.poll_fullscreen(1_000).expect("poll1"), WatchAction::Hide);
    assert!(w.is_hidden_for_fullscreen(), "应进入全屏隐藏态");
    assert_eq!(unsafe { ffi::IsWindowVisible(tw.0) }, 0, "窗口未隐藏(SW_HIDE 未生效)");
    assert_eq!(tw.ex_style() & ffi::WS_EX_TOPMOST, 0, "进入全屏后应取消置顶");

    // ③ 退出全屏（销毁合成全屏窗口），迟滞未满不恢复
    unsafe {
        ffi::DestroyWindow(popup);
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    // 环境守卫：迟滞观察期内出现外部全屏窗口 → 后续判定不可信，跳过剩余断言。
    if !no_fullscreen_present() {
        eprintln!(
            "[SKIPPED] AC-11：迟滞期桌面出现外部全屏窗口，恢复断言未执行（Hide 阶段已验证）"
        );
        return;
    }
    assert_eq!(w.poll_fullscreen(2_000).expect("poll2"), WatchAction::None, "迟滞未满不应恢复");
    assert!(w.is_hidden_for_fullscreen());

    // ④ 距最近全屏观测(1000)满 3s → Show，窗口恢复显示 + 恢复置顶
    // 环境守卫：恢复断言前再次确认无外部全屏窗口（Q3-① 误失败根因）。
    if !no_fullscreen_present() {
        eprintln!(
            "[SKIPPED] AC-11：恢复断言前桌面出现外部全屏窗口，恢复断言未执行（Hide 阶段已验证）"
        );
        return;
    }
    assert_eq!(w.poll_fullscreen(4_000).expect("poll4"), WatchAction::Show);
    assert!(!w.is_hidden_for_fullscreen(), "应退出隐藏态");
    assert_ne!(unsafe { ffi::IsWindowVisible(tw.0) }, 0, "窗口未恢复显示");
    assert_ne!(tw.ex_style() & ffi::WS_EX_TOPMOST, 0, "恢复后应重新置顶");
}
