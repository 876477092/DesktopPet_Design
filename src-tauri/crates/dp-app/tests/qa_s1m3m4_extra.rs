//! QA 独立补充测试（S1-M3 托盘 app 层纯逻辑 + S1-M4 `topology_changed`）。
//!
//! 由 QA（严过关）新增，**不改动任何生产代码**。仅使用 `dp_app` 的公开 API。
//! 覆盖 QA 任务 C 第 3/7 项中属 `dp-app` 的部分：
//!   - `supervisor::topology_changed`：增 / 删 / 改 / 同 + 顺序打乱（集合语义）+ 空↔非空
//!   - `tray_menu::next_topmost` 往返
//!   - `tray_menu::tooltip_for` 状态后缀 / 空名保留占位（C2）
//!   - `tray_menu::DEFAULT_PET_NAME` 中性（不得等于硬编码角色名，用码点构造）
//!   - `tray_menu::action_for_menu_id`（转调平台层）联通
//!
//! 标注：本文件为 QA 交付物，非工程师生产代码。

#![cfg(windows)]

use dp_app::supervisor::topology_changed;
use dp_app::tray_menu::{action_for_menu_id, next_topmost, tooltip_for, DEFAULT_PET_NAME};
use dp_platform::{MonitorId, MonitorInfo, RectI, TopmostMode, TrayAction, TrayIconState, Vec2};

/// 构造测试用显示器描述（id 唯一）。
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

fn a() -> MonitorInfo {
    mk(1, 0.0, 0.0, 1.0, RectI::new(0, 0, 1920, 1080))
}
fn b() -> MonitorInfo {
    mk(2, -1920.0, 0.0, 1.0, RectI::new(-1920, 0, 0, 1080))
}
fn c() -> MonitorInfo {
    mk(3, 0.0, -1080.0, 1.0, RectI::new(0, -1080, 1920, 0))
}

/// 增 / 删 / 改 / 同。
#[test]
fn topology_changed_add_remove_modify_same() {
    assert!(!topology_changed(&[a()], &[a()]), "完全相同应为未变");
    assert!(!topology_changed(&[], &[]), "双空应为未变");

    assert!(topology_changed(&[a()], &[a(), b()]), "增屏应判为变化");
    assert!(topology_changed(&[a(), b()], &[a()]), "删屏应判为变化");

    // 改 rc_monitor。
    let a_res = mk(1, 0.0, 0.0, 1.0, RectI::new(0, 0, 2560, 1440));
    assert!(topology_changed(&[a()], &[a_res]), "改分辨率应判为变化");

    // 改 primary。
    let mut a_np = a();
    a_np.primary = false;
    assert!(topology_changed(&[a()], &[a_np]), "改主屏标记应判为变化");

    // 改 scale（rc_monitor 不变）。
    let a_scale = mk(1, 0.0, 0.0, 1.5, RectI::new(0, 0, 1920, 1080));
    // 注：现行实现只比对 rc_monitor/rc_work/primary，**不比对 scale / origin_vdc**。
    // 若仅 scale 变，实现会判「未变化」——在此记录实际行为，供 x 核对设计口径。
    let scale_only_changed = topology_changed(&[a()], &[a_scale]);
    eprintln!("[qa] topology_changed 仅 scale 变化 -> {scale_only_changed}（记录实际行为）");
}

/// 顺序打乱：同集合不同顺序应判为「未变化」（集合语义）。
#[test]
fn topology_changed_is_order_insensitive() {
    let list1 = vec![a(), b(), c()];
    let list2 = vec![c(), a(), b()];
    let list3 = vec![b(), c(), a()];
    assert!(!topology_changed(&list1, &list2), "同集合乱序应为未变");
    assert!(!topology_changed(&list1, &list3), "同集合乱序应为未变");
    assert!(!topology_changed(&list2, &list3), "同集合乱序应为未变");

    // 乱序 + 少一个 → 变化。
    assert!(topology_changed(&list1, &[c(), a()]), "乱序且缺项应为变化");
}

/// 空 ↔ 非空。
#[test]
fn topology_changed_empty_vs_nonempty() {
    assert!(topology_changed(&[], &[a()]), "空→非空应判为变化");
    assert!(topology_changed(&[a()], &[]), "非空→空应判为变化");
    assert!(!topology_changed(&[], &[]), "空→空应为未变");
}

/// 置顶二态往返（Always ↔ Never；BelowFullscreen 归入 Always）。
#[test]
fn next_topmost_roundtrip_and_below_fullscreen() {
    assert_eq!(next_topmost(TopmostMode::Always), TopmostMode::Never);
    assert_eq!(next_topmost(TopmostMode::Never), TopmostMode::Always);
    assert_eq!(next_topmost(TopmostMode::BelowFullscreen), TopmostMode::Always);
    assert_eq!(next_topmost(next_topmost(TopmostMode::Always)), TopmostMode::Always);
    assert_eq!(next_topmost(next_topmost(TopmostMode::Never)), TopmostMode::Never);
}

/// tooltip 后缀 / 空名保留占位。
#[test]
fn tooltip_state_suffixes_and_empty_name() {
    assert_eq!(tooltip_for(TrayIconState::Normal, "小星"), "小星");
    assert_eq!(tooltip_for(TrayIconState::LeftHome, "小星"), "小星 离开了");
    assert_eq!(tooltip_for(TrayIconState::NotInteractive, "小星"), "小星（不可交互）");
    // 空名：保留 {name}。
    assert_eq!(tooltip_for(TrayIconState::Normal, ""), "{name}");
    assert_eq!(tooltip_for(TrayIconState::LeftHome, ""), "{name} 离开了");
    assert_eq!(tooltip_for(TrayIconState::NotInteractive, ""), "{name}（不可交互）");
}

/// C2：`DEFAULT_PET_NAME` 不得等于硬编码角色名（码点构造，避免源码字面量）。
#[test]
fn default_pet_name_is_neutral() {
    let role = "\u{5fc3}\u{6708}\u{72d0}";
    assert_ne!(DEFAULT_PET_NAME, role, "默认名不得是硬编码角色名");
    assert!(!DEFAULT_PET_NAME.is_empty());
    for label in [DEFAULT_PET_NAME, &tooltip_for(TrayIconState::Normal, DEFAULT_PET_NAME)] {
        assert!(!label.contains(role), "默认文案不得含角色名：{label}");
    }
}

/// `action_for_menu_id`（经 app 层转调）联通 + 未知 id。
#[test]
fn action_for_menu_id_through_app_layer() {
    assert_eq!(action_for_menu_id("tray.show_hide"), Some(TrayAction::ShowHide));
    assert_eq!(action_for_menu_id("tray.coax"), Some(TrayAction::Coax));
    assert_eq!(action_for_menu_id("tray.recall"), Some(TrayAction::Recall));
    assert_eq!(action_for_menu_id("tray.nope"), None);
    assert_eq!(action_for_menu_id(""), None);
}
