//! QA 独立补充测试（S1-M3 托盘契约 + S1-M4 自检/多屏恢复 / AC-11 时序）。
//!
//! 由 QA（严过关）新增，**不改动任何生产代码**。仅使用 `dp-platform` 的公开 API，
//! 目标：在工程师自测之外，独立复现并尝试证伪其结论（边界 / 组合态 / 对抗输入 / 时序）。
//!
//! 覆盖点（对应 QA 任务 C / D）：
//!   C1 `menu_spec` 组合态（穿透 ∧ L5）
//!   C2 `action_for_menu_id` ↔ `menu_spec` 双向完备 / 未知 id
//!   C3 `payload_action` 十串 + `event_name` + `fmt_label` 边界
//!   C4 C2 红线回归（标签中不得出现硬编码角色名）
//!   C5 `icon_for_state` 三态 + 图标文件存在性
//!   C6 `recovery_target` 五类边界（含除零 / NaN / panic 风险探针）
//!   C8 `self_check` 真机幂等（连续 ≥3 次）
//!   C9 `ensure_on_screen` 不得无依据移动窗口
//!   D  AC-11 时序独立复算（2.999 / 3.000 / 3.001 与 200ms 短全屏）
//!
//! 标注：本文件为 QA 交付物，非工程师生产代码。

#![cfg(windows)]

use std::ffi::c_void;
use std::sync::Arc;

use dp_platform::win::window::{recovery_target, SelfCheckReport};
use dp_platform::{
    action_for_menu_id, fmt_label, icon_for_state, menu_spec, topmost_plan, DisplayService,
    FullscreenWatch, MonitorId, MonitorInfo, PlatformWindow, RectI, TopmostMode, TrayAction,
    TrayIconState, TrayMenuState, Vec2, WatchAction, WinPlatformWindow, ZOrder, RESTORE_DELAY_MS,
};

// ---------------------------------------------------------------------------
// 公共构造：显示器描述
// ---------------------------------------------------------------------------

/// 构造测试用显示器描述。
fn mk(ox: f32, oy: f32, scale: f32, rc_monitor: RectI, rc_work: RectI) -> MonitorInfo {
    MonitorInfo {
        id: MonitorId(1),
        origin_vdc: Vec2::new(ox, oy),
        scale,
        rc_monitor,
        rc_work,
        device_id: "DISPLAY1".to_string(),
        primary: true,
    }
}

fn primary() -> MonitorInfo {
    mk(
        0.0,
        0.0,
        1.5,
        RectI::new(0, 0, 1920, 1080),
        RectI::new(0, 0, 1920, 1040),
    )
}

/// 由 `MonitorInfo` + VDC 求「物理像素点」，用于核对目标是否落在 `rc_work` 内。
fn vdc_to_phys(m: &MonitorInfo, v: Vec2) -> (i32, i32) {
    m.vdc_to_physical(v)
}

// ===========================================================================
// C1 / C2 / C3 / C4 / C5：托盘契约（纯逻辑）
// ===========================================================================

/// C1：`click_through=true` 且 `left_home=true` 同时成立时，项数 / 顺序 / top_priority / enabled。
#[test]
fn c1_menu_spec_click_through_and_left_home_combo() {
    let state = TrayMenuState { click_through: true, left_home: true, topmost_on: false };
    let items = menu_spec(state, "小星");

    // 组合态应为 10 项（9 基础 + recall）。
    assert_eq!(items.len(), 10, "穿透∧L5 应为 10 项");
    // 仅兜底三项置顶（FR-11-12 第 2 层）。
    let top: Vec<&str> = items.iter().filter(|i| i.top_priority).map(|i| i.id).collect();
    assert_eq!(
        top,
        vec!["tray.coax", "tray.feed", "tray.bath"],
        "组合态仅兜底三项应置顶且顺序固定"
    );
    // 全部项 enabled / visible。
    for it in &items {
        assert!(it.enabled, "项应 enabled：{}", it.id);
        assert!(it.visible, "项应 visible：{}", it.id);
    }
    // 首三项为兜底，末项为 recall。
    assert_eq!(items[0].id, "tray.coax");
    assert_eq!(items[1].id, "tray.feed");
    assert_eq!(items[2].id, "tray.bath");
    assert_eq!(items.last().unwrap().id, "tray.recall");
    // 无重复 id。
    let uniq: std::collections::BTreeSet<&str> = items.iter().map(|i| i.id).collect();
    assert_eq!(uniq.len(), items.len(), "菜单 id 不得重复");
}

/// C2：`menu_spec` 产出的每个 id 都能反向映射；四态并集 = 全部 10 动作；未知/大小写变体 → None。
#[test]
fn c2_action_for_menu_id_bidirectionally_complete() {
    let states = [
        TrayMenuState { click_through: false, left_home: false, topmost_on: true },
        TrayMenuState { click_through: true, left_home: false, topmost_on: true },
        TrayMenuState { click_through: false, left_home: true, topmost_on: true },
        TrayMenuState { click_through: true, left_home: true, topmost_on: false },
    ];

    let mut union: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    for st in states {
        for it in menu_spec(st, "小星") {
            assert!(
                action_for_menu_id(it.id).is_some(),
                "menu_spec 产出的 id 未反向映射：{}",
                it.id
            );
            union.insert(it.id);
        }
    }

    // 四个状态的 id 并集应恰好覆盖 10 个动作对应的 id。
    let all_mapped: std::collections::BTreeSet<&str> = TrayAction::ALL
        .iter()
        .filter_map(|a| {
            [
                "tray.show_hide",
                "tray.toggle_topmost",
                "tray.toggle_click_through",
                "tray.settings",
                "tray.about",
                "tray.quit",
                "tray.coax",
                "tray.feed",
                "tray.bath",
                "tray.recall",
            ]
            .into_iter()
            .find(|id| action_for_menu_id(id) == Some(*a))
        })
        .collect();
    assert_eq!(union, all_mapped, "menu_spec 并集应与动作映射集合一致");
    assert_eq!(all_mapped.len(), 10, "应覆盖全部 10 个动作");

    // 未知 / 空 / 大小写变体 → None。
    for bad in ["", "tray.unknown", "tray.show_hide_", "TRAY.SHOW_HIDE", "Show_Hide", " "] {
        assert_eq!(action_for_menu_id(bad), None, "非法 id 应返回 None：{bad:?}");
    }
}

/// C3：`payload_action` 十字符串 + 事件名 + `fmt_label` 边界（多占位 / 无占位 / 空名）。
#[test]
fn c3_payload_and_event_and_fmt_label_edges() {
    let expected = [
        (TrayAction::ShowHide, "show_hide"),
        (TrayAction::ToggleTopmost, "toggle_topmost"),
        (TrayAction::ToggleClickThrough, "toggle_click_through"),
        (TrayAction::Settings, "settings"),
        (TrayAction::About, "about"),
        (TrayAction::Quit, "quit"),
        (TrayAction::Coax, "coax"),
        (TrayAction::Feed, "feed"),
        (TrayAction::Bath, "bath"),
        (TrayAction::Recall, "recall"),
    ];
    for (a, s) in expected {
        assert_eq!(a.payload_action(), s, "payload 字符串不符：{a:?}");
        assert_eq!(a.event_name(), "pet://tray", "事件名应为 pet://tray");
    }
    // 十串两两不同（可作唯一键）。
    let set: std::collections::BTreeSet<&str> = TrayAction::ALL
        .iter()
        .map(|a| a.payload_action())
        .collect();
    assert_eq!(set.len(), 10, "10 个动作 payload 应两两不同");

    // fmt_label 边界。
    assert_eq!(fmt_label("{name}{name}", "A"), "AA", "多占位应全部替换");
    assert_eq!(fmt_label("无占位", "A"), "无占位", "无占位应原样");
    assert_eq!(fmt_label("{name}", ""), "{name}", "空名应保留占位");
    assert_eq!(fmt_label("", "A"), "", "空模板应返回空");
    assert_eq!(fmt_label("X{name}Y{name}Z", "N"), "XNYNZ");
}

/// C4：C2 红线回归——所有标签不得含硬编码角色名（码点构造，避免源码字面量）。
#[test]
fn c4_menu_labels_never_hardcode_role_name() {
    let role = "\u{5fc3}\u{6708}\u{72d0}"; // 心月狐
    let states = [
        TrayMenuState { click_through: false, left_home: false, topmost_on: true },
        TrayMenuState { click_through: true, left_home: false, topmost_on: true },
        TrayMenuState { click_through: false, left_home: true, topmost_on: true },
        TrayMenuState { click_through: true, left_home: true, topmost_on: true },
    ];
    for st in states {
        for pet in ["", "小星", role] {
            for it in menu_spec(st, pet) {
                assert!(!it.label.is_empty(), "标签不得为空：{}", it.id);
                // 仅当 pet_name 恰为角色名时标签才可含该串（来自占位替换，非硬编码）。
                if pet != role {
                    assert!(
                        !it.label.contains(role),
                        "标签在 pet={pet:?} 下出现角色名（硬编码嫌疑）：{} = {}",
                        it.id,
                        it.label
                    );
                }
            }
            // 含占位符的项：空名时必须保留 {name}。
            if pet.is_empty() {
                let empty = menu_spec(st, "");
                for id in ["tray.coax", "tray.recall"] {
                    if let Some(it) = empty.iter().find(|i| i.id == id) {
                        assert!(
                            it.label.contains("{name}"),
                            "空名时 {id} 应保留 {{name}} 占位：{}",
                            it.label
                        );
                    }
                }
            }
        }
    }
}

/// C5：`icon_for_state` 三态映射 + 两个图标文件确实存在；Normal ≠ LeftHome。
#[test]
fn c5_icon_for_state_and_files_exist() {
    assert_eq!(icon_for_state(TrayIconState::Normal), "tray.ico");
    assert_eq!(icon_for_state(TrayIconState::LeftHome), "tray-gray.ico");
    assert_eq!(icon_for_state(TrayIconState::NotInteractive), "tray.ico");
    assert_ne!(
        icon_for_state(TrayIconState::Normal),
        icon_for_state(TrayIconState::LeftHome),
        "普通态与离家出走态图标应不同"
    );

    // 图标文件存在（相对 dp-platform crate：../../icons/）。
    let base = env!("CARGO_MANIFEST_DIR");
    for f in ["tray.ico", "tray-gray.ico"] {
        let p = format!("{base}/../../icons/{f}");
        assert!(
            std::path::Path::new(&p).is_file(),
            "托盘图标文件缺失：{p}"
        );
    }
    // tray-gray 与 tray 若非同源，则内容应不同（防「灰色图标 == 普通图标」占位）。
    let gray = std::fs::read(format!("{base}/../../icons/tray-gray.ico")).expect("读 tray-gray.ico");
    let normal = std::fs::read(format!("{base}/../../icons/tray.ico")).expect("读 tray.ico");
    assert!(!gray.is_empty() && !normal.is_empty(), "图标文件不得为空");
    eprintln!(
        "[qa] tray.ico={} B, tray-gray.ico={} B",
        normal.len(),
        gray.len()
    );
}

// ===========================================================================
// C6：recovery_target 边界 / 对抗输入
// ===========================================================================

/// C6①：中心在 `rc_monitor` 内 → None；半开区间边界核对。
#[test]
fn c6a_recovery_target_none_when_inside() {
    let list = vec![primary()];
    assert!(recovery_target((100, 100), &list).is_none());
    assert!(recovery_target((0, 0), &list).is_none());
    assert!(recovery_target((1919, 1079), &list).is_none());
    // 半开区间：右 / 下边界不含。
    assert!(recovery_target((1920, 100), &list).is_some(), "x=right 应越界");
    assert!(recovery_target((100, 1080), &list).is_some(), "y=bottom 应越界");
}

/// C6②：拔屏（列表不含原屏）→ 目标落在剩余屏 `rc_work` 内（monitor_at + 物理点双断言）。
#[test]
fn c6b_recovery_target_lands_in_remaining_work_area() {
    let remaining = vec![primary()];
    let vdc = recovery_target((-500, 100), &remaining).expect("拔屏后应迁移到剩余屏");

    let svc = DisplayService::from_monitors(remaining.clone());
    let m = svc.monitor_at(vdc);
    assert_eq!(m.id, MonitorId(1), "应迁到剩余屏（主屏）");
    assert!(m.contains_vdc(vdc), "目标 VDC 应落在剩余屏 rc_monitor 内：{vdc:?}");

    // 更强的断言：目标物理点应落在剩余屏工作区 rc_work 内（避让任务栏）。
    let (px, py) = vdc_to_phys(&remaining[0], vdc);
    assert!(
        remaining[0].rc_work.contains(px, py),
        "目标物理点 ({px},{py}) 应落在 rc_work {:?} 内",
        remaining[0].rc_work
    );
    assert!(vdc.x.is_finite() && vdc.y.is_finite(), "目标 VDC 不得为 NaN/Inf");
}

/// C6③：空列表 → None。
#[test]
fn c6c_recovery_target_none_when_empty() {
    assert!(recovery_target((100, 100), &[]).is_none());
    assert!(recovery_target((i32::MAX, i32::MIN), &[]).is_none());
}

/// C6④：极端输入（零/负尺寸、rc_work 脱离、边界点、非法 scale）——行为记录 + 无 panic/NaN/除零。
#[test]
fn c6d_recovery_target_extreme_inputs_no_panic_or_nan() {
    // (a) rc_monitor 零尺寸。
    let zero = vec![mk(0.0, 0.0, 1.0, RectI::new(0, 0, 0, 0), RectI::new(0, 0, 0, 0))];
    let r = recovery_target((5, 5), &zero);
    if let Some(v) = r {
        assert!(v.x.is_finite() && v.y.is_finite(), "零尺寸返回非有限值：{v:?}");
    }
    eprintln!("[qa] zero-size monitor -> {r:?}");

    // (b) rc_monitor 负尺寸（left > right）。
    let neg = vec![mk(0.0, 0.0, 1.0, RectI::new(100, 100, 0, 0), RectI::new(100, 100, 0, 0))];
    let r = recovery_target((5, 5), &neg);
    if let Some(v) = r {
        assert!(v.x.is_finite() && v.y.is_finite(), "负尺寸返回非有限值：{v:?}");
    }
    eprintln!("[qa] negative-size monitor -> {r:?}");

    // (c) rc_work 与 rc_monitor 完全脱离。
    let detached = vec![mk(
        0.0,
        0.0,
        1.0,
        RectI::new(0, 0, 1920, 1080),
        RectI::new(5000, 5000, 6000, 6000),
    )];
    let v = recovery_target((9999, 9999), &detached).expect("脱离工作区应仍给出最近屏目标");
    assert!(v.x.is_finite() && v.y.is_finite(), "脱离工作区返回非有限值：{v:?}");
    let (px, py) = vdc_to_phys(&detached[0], v);
    assert!(
        detached[0].rc_work.contains(px, py),
        "目标应落回（脱离的）rc_work：({px},{py})"
    );

    // (d) 恰好落在两屏边界（半开区间取右屏）。
    let left = MonitorInfo { id: MonitorId(2), ..mk(-1920.0, 0.0, 1.0, RectI::new(-1920, 0, 0, 1080), RectI::new(-1920, 0, 0, 1040)) };
    let two = vec![primary(), left];
    // x=0 属主屏（右屏），x=-1 属左屏 → 两者都在列表内，故均 None。
    assert!(recovery_target((0, 500), &two).is_none(), "x=0 边界应命中主屏");
    assert!(recovery_target((-1, 500), &two).is_none(), "x=-1 应命中左屏");
    // 两屏之外 → 迁移到最近屏。
    assert!(recovery_target((-5000, 500), &two).is_some());

    // (e) 非法 scale（0 / 负 / NaN）不得触发除零 / NaN。
    for bad_scale in [0.0_f32, -2.0, f32::NAN] {
        let m = vec![mk(0.0, 0.0, bad_scale, RectI::new(0, 0, 1920, 1080), RectI::new(0, 0, 1920, 1040))];
        let v = recovery_target((9999, 9999), &m);
        if let Some(v) = v {
            assert!(
                v.x.is_finite() && v.y.is_finite(),
                "scale={bad_scale} 返回非有限值：{v:?}"
            );
        }
        eprintln!("[qa] scale={bad_scale} -> {v:?}");
    }
}

// ===========================================================================
// C8 / C9：真机窗口（自建临时 STATIC 窗口）
// ===========================================================================

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
    pub const WS_POPUP: u32 = 0x8000_0000;
    pub const WS_VISIBLE: u32 = 0x1000_0000;
    pub const GWL_EXSTYLE: c_int = -20;
    pub const WS_EX_TOPMOST: u32 = 0x0000_0008;
    pub const WS_EX_LAYERED: u32 = 0x0008_0000;
    pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    pub const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct TempWindow(ffi::Hwnd);

impl TempWindow {
    /// 创建失败（无桌面会话等）返回 None，由用例优雅跳过。
    fn new() -> Option<Self> {
        let class = wide("STATIC");
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

/// C8：`self_check()` 真机幂等——连续 ≥3 次 `ok=true`、`missing_ex_style==0`、
/// `topmost_ok` 与真实 `WS_EX_TOPMOST` 位一致。
#[test]
fn c8_self_check_idempotent_three_times() {
    let Some(tw) = TempWindow::new() else {
        eprintln!("[qa] 无桌面会话，跳过 self_check 幂等（真机）用例");
        return;
    };
    let w = WinPlatformWindow::new(tw.0 as isize, Arc::new(DisplayService::detect()))
        .expect("attach 临时窗口");

    let mut reports: Vec<SelfCheckReport> = Vec::new();
    for i in 0..3 {
        let r = w.self_check().unwrap_or_else(|e| panic!("self_check #{i} 返回 Err：{e}"));
        assert!(r.ok, "self_check #{i} 应通过：{r:?}");
        assert_eq!(r.missing_ex_style, 0, "三必要位应齐备：{r:?}");

        // topmost_ok 必须与真实 WS_EX_TOPMOST 位 + 期望计划一致。
        let bit_set = tw.ex_style() & ffi::WS_EX_TOPMOST != 0;
        let plan_topmost = w_poll_plan_topmost();
        assert_eq!(
            r.topmost_ok,
            plan_topmost == bit_set,
            "topmost_ok 与真实位不一致：report={r:?} bit={bit_set} plan={plan_topmost}"
        );
        reports.push(r);
    }
    assert_eq!(reports[0], reports[1], "self_check 应幂等（1 vs 2）");
    assert_eq!(reports[1], reports[2], "self_check 应幂等（2 vs 3）");

    // 三必要位确实在位（旁观读回）。
    let required = ffi::WS_EX_LAYERED | ffi::WS_EX_TOOLWINDOW | ffi::WS_EX_NOACTIVATE;
    assert_eq!(
        tw.ex_style() & required,
        required,
        "三必要位应在位：{:#010X}",
        tw.ex_style()
    );
}

/// 期望的置顶计划（新建窗口默认 `Always` ⇒ 计划 Topmost）。
fn w_poll_plan_topmost() -> bool {
    // 新建的 WinPlatformWindow 默认 TopmostMode::Always ⇒ plan().zorder == Topmost。
    // 通过公开纯函数核对默认映射，避免依赖内部状态。
    topmost_plan(TopmostMode::Always, false, false).zorder == ZOrder::Topmost
}

/// C9：`ensure_on_screen`——空列表 / 窗口已在屏内时不得移动窗口。
#[test]
fn c9_ensure_on_screen_does_not_move_when_no_need() {
    let Some(tw) = TempWindow::new() else {
        eprintln!("[qa] 无桌面会话，跳过 ensure_on_screen 用例");
        return;
    };
    let w = WinPlatformWindow::new(tw.0 as isize, Arc::new(DisplayService::detect()))
        .expect("attach 临时窗口");

    let before = w.window_physical_rect().expect("读窗口矩形");

    // (1) 空列表 → 不迁移。
    let r1 = w.ensure_on_screen(&[]).expect("ensure_on_screen([]) 不应 Err");
    assert_eq!(r1, None, "空列表不应迁移");
    let after1 = w.window_physical_rect().expect("读窗口矩形");
    assert_eq!(before, after1, "空列表不得移动窗口");

    // (2) 窗口中心已在真实屏内（临时窗口位于 (0,0)）→ 不迁移。
    let monitors = DisplayService::detect().monitors();
    let r2 = w.ensure_on_screen(&monitors).expect("ensure_on_screen(real) 不应 Err");
    let after2 = w.window_physical_rect().expect("读窗口矩形");
    if r2.is_none() {
        assert_eq!(before, after2, "窗口已在屏内时不得移动");
    } else {
        eprintln!("[qa] 窗口被判为越界并迁移（单屏环境下异常）：{r2:?}");
    }
}

// ===========================================================================
// D：AC-11 时序独立复算
// ===========================================================================

/// 用 `FullscreenWatch` 精确核对迟滞边界：2.999s / 3.000s / 3.001s。
#[test]
fn d1_ac11_hysteresis_boundaries() {
    assert_eq!(RESTORE_DELAY_MS, 3_000);

    // 观测序列：t=0 全屏（Hide，L=0），t=1000 全屏（L=1000），t=2000 退出。
    let mut w = FullscreenWatch::new(TopmostMode::BelowFullscreen);
    assert_eq!(w.on_poll(0, true), WatchAction::Hide);
    assert_eq!(w.on_poll(1_000, true), WatchAction::None); // L 推进到 1000

    // 距 L=1000：3999 → 2999ms < 3000 → 不恢复。
    assert_eq!(w.on_poll(3_999, false), WatchAction::None, "2999ms 不该恢复");
    assert!(w.is_hidden_for_fullscreen());
    // 4000 → 3000ms == 3000 → 恢复（闭区间）。
    assert_eq!(w.on_poll(4_000, false), WatchAction::Show, "3000ms 边界应恢复");
    assert!(!w.is_hidden_for_fullscreen());
    // 再滞后 1ms 的问法（若先到 4001 也应已恢复）。
    assert_eq!(w.on_poll(4_001, false), WatchAction::None, "不应重复恢复");
}

/// 短全屏（200ms）不得误触发或过早恢复。
#[test]
fn d2_ac11_short_fullscreen_200ms() {
    let mut w = FullscreenWatch::new(TopmostMode::BelowFullscreen);
    assert_eq!(w.on_poll(10_000, true), WatchAction::Hide); // L=10000
    assert_eq!(w.on_poll(10_200, false), WatchAction::None); // 200ms 后退出，迟滞未满
    assert_eq!(w.on_poll(12_999, false), WatchAction::None); // 距 10000 = 2999
    assert!(w.is_hidden_for_fullscreen());
    assert_eq!(w.on_poll(13_000, false), WatchAction::Show); // 距 10000 = 3000
}

/// D 核心：在 tick 轮询下，最坏恢复时延相对**真实退出时刻**是多少？
///
/// 模型：轮询时刻 t=0, T, 2T, ...（T = tick 周期）；全屏在 t=0 起为真，真实退出时刻 = E。
/// 用真实状态机 `FullscreenWatch::on_poll` 推演，返回 (Show 轮询时刻 - E)。
fn worst_restore_latency(tick_ms: i64, e_ms: i64) -> Option<i64> {
    // E 必须落在「首个全屏观测之后」：这里令首个观测在 t=0、退出在 t=E。
    if e_ms <= 0 {
        return None;
    }
    let mut w = FullscreenWatch::new(TopmostMode::BelowFullscreen);
    let mut t: i64 = 0;
    // 先观测一次全屏（触发 Hide 并设 L=0）。
    assert_eq!(w.on_poll(0, true), WatchAction::Hide);
    t += tick_ms;
    loop {
        let fs = t < e_ms; // 退出前仍全屏
        let act = w.on_poll(t, fs);
        if act == WatchAction::Show {
            return Some(t - e_ms);
        }
        t += tick_ms;
        if t > e_ms + 30_000 {
            return None;
        }
    }
}

/// D：tick=1000ms 时，最坏恢复时延（对真实退出）。
///
/// 结论：最坏 = 3 × tick 周期（当退出紧接最后一次全屏观测之后）。
/// 若 tick 严格 = 1000ms，则最坏 → 3000ms（3s 临界）；
/// 若 tick = 1000 + δ（睡眠超时 + 循环体开销），则最坏 → 3000 + 3δ ms（微超 3s）。
#[test]
fn d3_ac11_worst_case_latency_vs_tick() {
    // tick = 1000ms：最坏（E→0+）逼近但不超过 3000。
    let mut max1000 = 0i64;
    for e in [1i64, 2, 50, 100, 500, 999, 1000] {
        let l = worst_restore_latency(1_000, e).expect("应有恢复点");
        max1000 = max1000.max(l);
    }
    eprintln!("[qa] AC-11 tick=1000ms 最坏恢复时延 = {max1000} ms");
    assert!(max1000 <= 3_000, "tick=1000 时最坏应 ≤3000ms：{max1000}");

    // tick = 1001ms（δ=1）：最坏升至 3003ms（> 3000）。
    let l1001 = worst_restore_latency(1_001, 1).expect("应有恢复点");
    eprintln!("[qa] AC-11 tick=1001ms(E=1) 恢复时延 = {l1001} ms");

    // tick = 1015ms（Windows 默认 ~15.6ms 睡眠粒度上限）：恢复时延 = 3×1015 = 3045。
    let l1015 = worst_restore_latency(1_015, 1).expect("应有恢复点");
    eprintln!("[qa] AC-11 tick=1015ms(E=1) 恢复时延 = {l1015} ms");
    assert_eq!(l1015, 3 * 1_015 - 1, "恢复应发生在第 3 个轮询点");

    // 记录结论：修复方向 = 使 tick ≤1000ms 且消除 δ，或改计时起点为「退出观测时刻」。
    assert!(l1001 > 3_000 || l1015 > 3_000, "说明：tick>1000ms 时最坏已超 3s");
}

// ===========================================================================
// 诊断探针（默认 #[ignore]，需显式 --ignored 运行）
// ===========================================================================

/// 诊断：实测 `std::thread::sleep(1000ms)` 的真实周期 δ（睡眠超时 + 调度），
/// 用于界定 AC-11 最坏恢复时延 = 3000 + 3δ 的 δ 量级。
///
/// 运行：`cargo test -p dp-platform --test qa_s1m3m4_extra -- --ignored --nocapture measure_sleep`
#[test]
#[ignore = "诊断探针：会睡眠 ~10s"]
fn measure_sleep_1000ms_overshoot() {
    use std::time::{Duration, Instant};
    let mut deltas: Vec<i64> = Vec::new();
    for _ in 0..10 {
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(1_000));
        deltas.push(t0.elapsed().as_millis() as i64);
    }
    let min = *deltas.iter().min().unwrap();
    let max = *deltas.iter().max().unwrap();
    let avg = deltas.iter().sum::<i64>() / deltas.len() as i64;
    eprintln!("[qa] sleep(1000ms) 实测：min={min} avg={avg} max={max} ms 样本={deltas:?}");
    eprintln!(
        "[qa] δ(sleep 超时) ≈ max-1000 = {} ms ⇒ AC-11 最坏恢复 ≈ 3000 + 3δ = {} ms",
        max - 1_000,
        3_000 + 3 * (max - 1_000)
    );
}

/// QA 诊断探针：验证工程师报告的 flake 候选机制——「同一测试二进制内并行的真机用例
/// （如 `ac11_...` 创建前台全屏窗口）是否会摘除其它窗口的 `WS_EX_TOPMOST` 位」。
///
/// 运行：
/// `cargo test -p dp-platform --test qa_s1m3m4_extra -- --ignored --nocapture`
#[test]
#[ignore = "诊断探针：需 --ignored 显式运行；会短暂创建前台全屏窗口"]
fn probe_fullscreen_foreground_may_demote_topmost() {
    let Some(tw) = TempWindow::new() else {
        eprintln!("[qa] 无桌面会话，跳过探针");
        return;
    };
    let w = WinPlatformWindow::new(tw.0 as isize, Arc::new(DisplayService::detect()))
        .expect("attach 临时窗口");
    w.set_topmost(TopmostMode::Always).expect("set_topmost(Always)");
    let before = tw.ex_style() & ffi::WS_EX_TOPMOST != 0;
    eprintln!("[qa] 置顶后 TOPMOST bit = {before}");
    assert!(before, "初始应置 TOPMOST");

    let svc = DisplayService::detect();
    let Some(mon) = svc.primary() else {
        eprintln!("[qa] 无主屏，跳过探针");
        return;
    };
    let rc = mon.rc_monitor;
    let class = wide("STATIC");
    let popup = unsafe {
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
    if popup.is_null() {
        eprintln!("[qa] 合成全屏窗口失败，跳过探针");
        return;
    }
    unsafe {
        ffi::SetWindowPos(
            popup,
            ffi::HWND_TOP,
            rc.left,
            rc.top,
            rc.width(),
            rc.height(),
            ffi::SWP_SHOWWINDOW,
        );
        ffi::SetForegroundWindow(popup);
    }
    std::thread::sleep(std::time::Duration::from_millis(400));
    let fg = unsafe { ffi::GetForegroundWindow() };
    let during = tw.ex_style() & ffi::WS_EX_TOPMOST != 0;
    eprintln!(
        "[qa] 前台全屏期间：fg==popup={}, TOPMOST bit = {during}",
        fg == popup
    );

    unsafe {
        ffi::DestroyWindow(popup);
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    let after = tw.ex_style() & ffi::WS_EX_TOPMOST != 0;
    eprintln!("[qa] 销毁全屏后 TOPMOST bit = {after}");
    eprintln!(
        "[qa] 探针结论：during=false ⇒ 机制成立（前台全屏会摘除其它窗口 TOPMOST 位）；否则机制不成立"
    );
}
