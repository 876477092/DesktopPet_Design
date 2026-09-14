#![cfg(windows)]

//! 托盘菜单装配（`02 §5 K-9`；`01 FR-1-10 / FR-11-12`）——**S1-M3 交付物**。
//!
//! ## 职责边界（本模块**只提供事件通道**）
//!
//! 本模块用 **Tauri 2 内置托盘 API**（[`tauri::tray::TrayIconBuilder`] +
//! [`tauri::menu`]，底层即 Win32 `Shell_NotifyIcon`，符合 `02 §5 K-9`）装配托盘菜单、
//! 图标与 tooltip，并把用户操作落地为：
//!   1. **真实生效**：置顶翻转 / 穿透开关 / 显示隐藏（直接作用于
//!      [`crate::PetPlatform`] 的平台窗口层）；
//!   2. **事件广播**：`pet://tray`（载荷 `{"action": "<action 字符串>"}`，`02 §7.6`）。
//!
//! **不做**的事（严守单次会话边界，对齐 `03` 卡片）：
//!   - `CoaxFlow` 完整闭环（摸摸 / 喂食 / 洗澡 / 找回来的状态机）在 **S7-M6**；
//!     本模块只把 `coax / feed / bath / recall` 事件送达，不解释其语义。
//!   - 穿透下 `WH_MOUSE_LL` 全局钩子的卸载与重挂属 **S3-M1**；本模块只切 `WS_EX_TRANSPARENT`
//!     侧（经 `WinPlatformWindow::set_click_through` 的双写口径）。
//!   - 退出前的存档确认属 **S5-M4**（此前误记 S1-M4，2026-09-13 按 N5 修正）；优雅退出
//!     已随 N1 接线（卸钩子 + `app.exit(0)`，见 `apply_action`）。
//!
//! ## 硬约束
//!   - **C2**：**禁止硬编码角色名**。本模块使用中性占位常量 [`DEFAULT_PET_NAME`]，
//!     真实角色名由 `resources/config/character.json`（S5-M3）接入后替换；
//!     所有含名字的文案经 [`dp_platform::tray::fmt_label`] 走 `{name}` 占位。
//!   - **C3**：本模块无时间逻辑。**C9**：零网络，托盘不引入任何网络能力。
//!   - **图标加载方式**：`Image::from_bytes(include_bytes!(...))`（编译期内嵌 + 运行时
//!     经 `image-ico` 特性解析 `.ico`；见 `Cargo.toml` 的 tauri 特性）。「不可交互」角标
//!     版图标不在 S1-M3 交付物内，故复用普通图标 + tooltip 后缀表达。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use dp_platform::tray::{
    action_for_menu_id as platform_action_for_menu_id, fmt_label, icon_for_state, menu_spec,
    TrayAction, TrayIconState, TrayMenuItem, TrayMenuState, ID_BATH, ID_COAX, ID_FEED, ID_QUIT,
    ID_RECALL, ID_SHOW_HIDE,
};
// `PlatformWindow` trait 提供 `set_topmost` / `set_click_through` / `set_visible`。
use dp_platform::{PlatformWindow, TopmostMode};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::PetPlatform;

/// 托盘图标的唯一 id（供 `tray_by_id` 定位，供 [`set_state`] 切换图标）。
const TRAY_ID: &str = "pet-tray";

/// 角色名占位默认值（**C2：禁止硬编码角色名**）。
///
/// 正式角色名来自 `resources/config/character.json`（S5-M3 接入）；在此之前，托盘标签与
/// tooltip 统一使用中性占位「宠物」，接入后替换即可（无需改动模板本身）。
pub const DEFAULT_PET_NAME: &str = "宠物";

/// tooltip 的名字模板（C2：名字经 `{name}` 占位，运行时替换）。
const NAME_TEMPLATE: &str = "{name}";

/// 普通态图标字节（编译期内嵌，运行时不走文件系统）。
const ICON_BYTES_NORMAL: &[u8] = include_bytes!("../../../icons/tray.ico");
/// 离家出走（L5）灰色图标字节（编译期内嵌）。
const ICON_BYTES_GRAY: &[u8] = include_bytes!("../../../icons/tray-gray.ico");

/// `ShowHide` 动作的可见态镜像（`AtomicBool`，无锁）。
///
/// 平台窗口层未暴露「读取可见态」的接口，故在 app 层维护一份镜像；初值 `true`
/// （`setup` 阶段已把 pet 窗口置为可见）。
static PET_VISIBLE: AtomicBool = AtomicBool::new(true);

/// 置顶态镜像（`true` = `TopmostMode::Always`）。
///
/// 初值 `true`：`setup` 阶段 `attach_pet_window` 已 `set_topmost(TopmostMode::Always)`。
static TOPMOST_ON: AtomicBool = AtomicBool::new(true);

// ---------------------------------------------------------------------------
// 纯函数（可单测，不依赖 Tauri 运行时）
// ---------------------------------------------------------------------------

/// 置顶开关的下一态（纯函数，供 [`TrayAction::ToggleTopmost`] 使用）。
///
/// 托盘置顶开关是**二态**语义：`Always` ↔ `Never`；`BelowFullscreen` 视为「未常驻置顶」，
/// 切回 `Always`。
#[must_use]
pub fn next_topmost(cur: TopmostMode) -> TopmostMode {
    match cur {
        TopmostMode::Always => TopmostMode::Never,
        TopmostMode::Never | TopmostMode::BelowFullscreen => TopmostMode::Always,
    }
}

/// 菜单项 id → 动作（转调平台层，保持单一事实来源）。
#[must_use]
pub fn action_for_menu_id(id: &str) -> Option<TrayAction> {
    platform_action_for_menu_id(id)
}

/// 组装 tooltip：`{name}` 美化名 + 状态后缀。
#[must_use]
pub fn tooltip_for(state: TrayIconState, pet_name: &str) -> String {
    let base = fmt_label(NAME_TEMPLATE, pet_name);
    match state {
        TrayIconState::Normal => base,
        // L5 离家出走：`{name} 离开了`。
        TrayIconState::LeftHome => format!("{base} 离开了"),
        // 不可交互（穿透 / 钩子卸载）：以 tooltip 后缀表达角标语义（角标图标不在本模块交付物内）。
        TrayIconState::NotInteractive => format!("{base}（不可交互）"),
    }
}

/// 广播 `pet://tray` 事件。
///
/// 载荷为对象 `{"action": "<action 字符串>"}`（`02 §7.6`）。使用
/// `HashMap<&str, &str>`：`serde` 已对 `HashMap` 实现 `Serialize`，无需新增直接依赖。
pub fn emit_tray_action(app: &AppHandle, action: TrayAction) -> Result<(), String> {
    let mut payload: HashMap<&str, &str> = HashMap::new();
    payload.insert("action", action.payload_action());
    app.emit(action.event_name(), payload)
        .map_err(|e| format!("广播 {} 事件失败：{e}", action.event_name()))
}

// ---------------------------------------------------------------------------
// 安装
// ---------------------------------------------------------------------------

/// 安装托盘图标与菜单（由 `lib.rs` 的 `setup` 阶段调用）。
///
/// 图标：`Image::from_bytes(include_bytes!("../../../icons/tray.ico"))`。
/// 左键单击 → 显示 / 隐藏；右键 → 弹出菜单（`.show_menu_on_left_click(false)`）。
pub fn install(app: &AppHandle) -> Result<(), String> {
    let state = current_menu_state(app);
    let items = menu_spec(state, DEFAULT_PET_NAME);
    let menu = build_menu(app, &items)?;
    let icon = load_icon(TrayIconState::Normal)?;

    // `build` 会把托盘注册进 Tauri 资源表并返回可克隆句柄；此处无需保留句柄。
    let _tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip(tooltip_for(TrayIconState::Normal, DEFAULT_PET_NAME))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu_event(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| on_tray_icon_event(tray.app_handle(), &event))
        .build(app)
        .map_err(|e| format!("创建托盘图标失败：{e}"))?;

    Ok(())
}

/// 切换托盘图标态与 tooltip（供后续模块 S7-M6 在 L5 / 穿透时调用）。
pub fn set_state(app: &AppHandle, state: TrayIconState) -> Result<(), String> {
    let tray = app
        .tray_by_id(TRAY_ID)
        .ok_or_else(|| "托盘图标尚未安装（请先调用 install）".to_string())?;
    let icon = load_icon(state)?;
    tray.set_icon(Some(icon))
        .map_err(|e| format!("切换托盘图标失败：{e}"))?;
    tray.set_tooltip(Some(tooltip_for(state, DEFAULT_PET_NAME)))
        .map_err(|e| format!("切换托盘提示失败：{e}"))?;
    Ok(())
}

/// 显示 / 隐藏 pet 窗口的**统一写点**（S3-M6 托盘协同收口）。
///
/// 托盘 `ShowHide` 与右键菜单 `menu_command{hide}` 均经此函数落窗口可见态，
/// 保证 [`PET_VISIBLE`] 镜像与真实窗口态一致（镜像漂移会让托盘按钮语义反转）。
///
/// # Errors
/// 平台窗口操作失败时返回中文可读错误串（调用方降级日志，镜像不更新）。
pub fn set_pet_visible(app: &AppHandle, visible: bool) -> Result<(), String> {
    let window = &app.state::<PetPlatform>().window;
    window
        .set_visible(visible)
        .map_err(|e| format!("设置窗口可见性({visible})失败：{e}"))?;
    PET_VISIBLE.store(visible, Ordering::Relaxed);
    Ok(())
}

// ---------------------------------------------------------------------------
// 内部实现
// ---------------------------------------------------------------------------

/// 读取当前菜单状态（穿透态取自平台窗口层；置顶态取自本地镜像）。
fn current_menu_state(app: &AppHandle) -> TrayMenuState {
    TrayMenuState {
        click_through: app.state::<PetPlatform>().window.is_click_through(),
        // L5 离家出走态由 S7-M6 经 `set_state` / 后续接口驱动；本模块起步为 `false`。
        left_home: false,
        topmost_on: TOPMOST_ON.load(Ordering::Relaxed),
    }
}

/// 菜单分组序号（用于插入分隔符：兜底三项 / 六项基础 / 退出 / L5 找回）。
fn menu_group(id: &str) -> u8 {
    match id {
        ID_COAX | ID_FEED | ID_BATH => 0,
        ID_QUIT => 2,
        ID_RECALL => 3,
        _ => 1,
    }
}

/// 由菜单规格装配 Tauri 菜单（分组间插入 `PredefinedMenuItem::separator`）。
fn build_menu(app: &AppHandle, items: &[TrayMenuItem]) -> Result<Menu<tauri::Wry>, String> {
    let menu = Menu::new(app).map_err(|e| format!("创建托盘菜单失败：{e}"))?;

    let mut prev_group: Option<u8> = None;
    for item in items {
        let group = menu_group(item.id);
        if let Some(prev) = prev_group {
            if prev != group {
                let sep = PredefinedMenuItem::separator(app)
                    .map_err(|e| format!("创建菜单分隔符失败：{e}"))?;
                menu.append(&sep).map_err(|e| format!("追加菜单分隔符失败：{e}"))?;
            }
        }

        let entry = MenuItem::with_id(app, item.id, item.label.as_str(), item.enabled, None::<&str>)
            .map_err(|e| format!("创建菜单项 {} 失败：{e}", item.id))?;
        menu.append(&entry)
            .map_err(|e| format!("追加菜单项 {} 失败：{e}", item.id))?;

        prev_group = Some(group);
    }

    Ok(menu)
}

/// 加载指定图标态的图标（编译期内嵌字节，运行时经 `image-ico` 解析）。
fn load_icon(state: TrayIconState) -> Result<Image<'static>, String> {
    Image::from_bytes(icon_bytes(state))
        .map_err(|e| format!("加载托盘图标失败（{}）：{e}", icon_for_state(state)))
}

/// 图标态 → 内嵌图标字节。
fn icon_bytes(state: TrayIconState) -> &'static [u8] {
    if icon_for_state(state) == "tray-gray.ico" {
        ICON_BYTES_GRAY
    } else {
        ICON_BYTES_NORMAL
    }
}

/// 菜单项事件处理：先真实生效，再广播 `pet://tray`。
fn on_menu_event(app: &AppHandle, id: &str) {
    let Some(action) = action_for_menu_id(id) else {
        // 未知 id（分隔符等非动作项）：忽略。
        return;
    };
    apply_action(app, action);
    if let Err(err) = emit_tray_action(app, action) {
        eprintln!("[dp-app] 托盘事件广播失败（{id}）：{err}");
    }
}

/// 把托盘动作落到平台窗口层（真实生效）；其余动作仅广播（见文件头职责边界）。
fn apply_action(app: &AppHandle, action: TrayAction) {
    let window = &app.state::<PetPlatform>().window;
    match action {
        TrayAction::ToggleTopmost => {
            let next = next_topmost(current_topmost());
            TOPMOST_ON.store(matches!(next, TopmostMode::Always), Ordering::Relaxed);
            if let Err(err) = window.set_topmost(next) {
                eprintln!("[dp-app] 托盘切换置顶失败：{err}");
            }
        }
        TrayAction::ToggleClickThrough => {
            let next = !window.is_click_through();
            if let Err(err) = window.set_click_through(next) {
                eprintln!("[dp-app] 托盘切换穿透失败：{err}");
            }
        }
        TrayAction::ShowHide => {
            let next = !PET_VISIBLE.load(Ordering::Relaxed);
            // 统一写点收口（S3-M6）：与 menu_command{hide} 共用 set_pet_visible，
            // 保证 PET_VISIBLE 镜像一致。
            if let Err(err) = set_pet_visible(app, next) {
                eprintln!("[dp-app] 托盘显示/隐藏失败：{err}");
            }
        }
        // N1（2026-09-13）：退出为真实生效动作——先卸载全局钩子线程（`shutdown`
        // 幂等，S3-M1 起钩子常驻，不卸载会残留全局鼠标钩子），再优雅退出。
        // 当前 S5-M1 存档尚未引入，无「退出前存档确认」流程，直接退出（存档确认
        // 属 S5-M4；此前误记为 S1-M4，已按 N5 检查报告修正归属）。
        TrayAction::Quit => {
            if let Some(svc) =
                app.try_state::<std::sync::Arc<dp_platform::win::hook::HookService>>()
            {
                svc.shutdown();
            }
            app.exit(0);
        }
        // S4-M4：L5 离家 → 「把心月狐找回来」走回。经入站通道交给 core-loop 逻辑档
        // 落地（单线程 Actor 口径，避免跨线程直改内核状态）。
        TrayAction::Recall => {
            if let Some(channel) = app.try_state::<crate::bridge::CoreInputChannel>() {
                channel.push(crate::bridge::CoreInput::RecallRunaway);
            } else {
                eprintln!("[dp-app] 托盘找回：core-loop 入站通道尚未装配，降级忽略");
            }
        }
        // 设置 / 关于的 UI 在 S5；摸摸 / 喂食 / 洗澡的完整闭环在 S7-M6。
        // 本模块只负责把事件送达（见文件头「职责边界」）。
        TrayAction::Settings
        | TrayAction::About
        | TrayAction::Coax
        | TrayAction::Feed
        | TrayAction::Bath => {}
    }
}

/// 当前置顶态（读本地镜像）。
fn current_topmost() -> TopmostMode {
    if TOPMOST_ON.load(Ordering::Relaxed) {
        TopmostMode::Always
    } else {
        TopmostMode::Never
    }
}

/// 左键单击 → 显示 / 隐藏（右键由 `.show_menu_on_left_click(false)` 交给菜单）。
///
/// 说明：Win32 下 `WM_LBUTTONDOWN` / `WM_LBUTTONUP` 均会产生 `TrayIconEvent::Click`，
/// 这里只取 `button_state == Up`（一次物理点击恰触发一次），与 Tauri 官方示例口径一致。
fn on_tray_icon_event(app: &AppHandle, event: &TrayIconEvent) {
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        on_menu_event(app, ID_SHOW_HIDE);
    }
}

// ---------------------------------------------------------------------------
// 单元测试（纯函数部分，不依赖 Tauri 运行时）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_topmost_toggles_and_round_trips() {
        assert_eq!(next_topmost(TopmostMode::Always), TopmostMode::Never);
        assert_eq!(next_topmost(TopmostMode::Never), TopmostMode::Always);
        // BelowFullscreen 视为「非常驻置顶」，切回常驻置顶。
        assert_eq!(next_topmost(TopmostMode::BelowFullscreen), TopmostMode::Always);

        // 往返：Always → Never → Always。
        assert_eq!(
            next_topmost(next_topmost(TopmostMode::Always)),
            TopmostMode::Always
        );
        // 往返：Never → Always → Never。
        assert_eq!(
            next_topmost(next_topmost(TopmostMode::Never)),
            TopmostMode::Never
        );
    }

    #[test]
    fn menu_id_to_action_to_payload_chain() {
        // 覆盖全部十个菜单项（含 L5 追加的 recall）。
        let state = TrayMenuState { click_through: true, left_home: true, topmost_on: true };
        let items = menu_spec(state, DEFAULT_PET_NAME);
        assert_eq!(items.len(), 10, "L5 态应产出 10 项");

        for item in &items {
            let action = action_for_menu_id(item.id)
                .unwrap_or_else(|| panic!("id 未映射为动作：{}", item.id));
            assert_eq!(action.event_name(), "pet://tray");
            assert!(
                !action.payload_action().is_empty(),
                "载荷动作字符串不得为空：{}",
                item.id
            );
        }

        // 未知 id → None。
        assert!(action_for_menu_id("tray.none").is_none());
        assert!(action_for_menu_id("").is_none());
    }

    #[test]
    fn tooltip_reflects_state_and_suffixes() {
        assert_eq!(tooltip_for(TrayIconState::Normal, "小星"), "小星");
        assert_eq!(tooltip_for(TrayIconState::LeftHome, "小星"), "小星 离开了");
        assert_eq!(
            tooltip_for(TrayIconState::NotInteractive, "小星"),
            "小星（不可交互）"
        );

        // 无角色名来源时保留 `{name}` 占位（C2：绝不回退到硬编码角色名）。
        assert_eq!(tooltip_for(TrayIconState::Normal, ""), "{name}");
        assert_eq!(tooltip_for(TrayIconState::LeftHome, ""), "{name} 离开了");
    }

    #[test]
    fn default_pet_name_is_a_neutral_placeholder() {
        // C2 回归：占位默认值不得等于硬编码角色名（用码点构造，避免字面量）。
        let role_name = "\u{5fc3}\u{6708}\u{72d0}";
        assert_ne!(DEFAULT_PET_NAME, role_name);
        assert!(!DEFAULT_PET_NAME.is_empty());
    }
}
