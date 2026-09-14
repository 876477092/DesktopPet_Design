//! Windows 托盘菜单的**契约与纯逻辑**（`02 §5 K-9` / `02 §5.23`；`01 FR-1-10 / FR-11-12`）。
//!
//! 本文件**只承载托盘菜单的可单测部分**：动作枚举、图标态、菜单项规格、`id ↔ 动作`
//! 双向映射与 `{name}` 标签格式化。**不直接调用** `Shell_NotifyIcon`——真正的托盘落点是
//! `dp-app/src/tray_menu.rs`（Tauri 2 内置 `TrayIconBuilder`，底层即 `Shell_NotifyIcon`，
//! 符合 `02 §5 K-9`），本层保持「零 Win32 依赖、天然可单测」。
//!
//! 关键约束：
//!   - **C2：禁止硬编码角色名**。所有含角色名的标签一律走 `{name}` 占位符，运行时经
//!     [`fmt_label`] 替换；调用方传空串时保留 `{name}` 原样（交由上层兜底）。
//!   - **C3：本模块无时间逻辑**（不读取任何系统时钟，符合 C3 红线）。
//!   - **单次会话边界（S1-M3）**：只做托盘 UI 与事件通道，**不做** `CoaxFlow` 状态机
//!     （托盘输入的完整闭环在 S7-M6）。
//!
//! 图标说明：本模块交付物只含 `tray.ico`（普通）与 `tray-gray.ico`（离家出走）。
//! 设计文档提到的「**不可交互**」**角标版图标不在本模块交付物内**，故 [`icon_for_state`]
//! 对 [`TrayIconState::NotInteractive`] **复用 `tray.ico`**，角标语义改由 **tooltip 后缀
//! `（不可交互）`** 表达（见 `dp-app/src/tray_menu.rs` 的 tooltip 拼装）。

// ---------------------------------------------------------------------------
// 动作
// ---------------------------------------------------------------------------

/// 托盘菜单动作（`02 §7.6` `pet://tray` 的 `{action}` 取值来源）。
///
/// 十项动作 = **六项基础**（[`TrayAction::ShowHide`] / [`TrayAction::ToggleTopmost`] /
/// [`TrayAction::ToggleClickThrough`] / [`TrayAction::Settings`] / [`TrayAction::About`] /
/// [`TrayAction::Quit`]）+ **四项替代交互入口**（[`TrayAction::Coax`] / [`TrayAction::Feed`] /
/// [`TrayAction::Bath`] / [`TrayAction::Recall`]，其中 `Recall` 仅 L5 出现）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    /// 显示 / 隐藏宠物窗口。
    ShowHide,
    /// 置顶开关（`Always` ↔ `Never` 翻转）。
    ToggleTopmost,
    /// 鼠标穿透开关。
    ToggleClickThrough,
    /// 打开设置面板。
    Settings,
    /// 关于对话框。
    About,
    /// 退出应用。
    Quit,
    /// **FR-11-12 托盘替代入口**：摸摸（等价 CoaxFlow 呼唤 / 抚摸输入）。
    Coax,
    /// **FR-11-12 托盘替代入口**：喂食。
    Feed,
    /// **FR-11-12 托盘替代入口**：洗澡。
    Bath,
    /// **L5 离家出走**：把角色找回来。
    Recall,
}

impl TrayAction {
    /// 全部动作（用于遍历 / 双向完备性断言）。
    pub const ALL: [TrayAction; 10] = [
        TrayAction::ShowHide,
        TrayAction::ToggleTopmost,
        TrayAction::ToggleClickThrough,
        TrayAction::Settings,
        TrayAction::About,
        TrayAction::Quit,
        TrayAction::Coax,
        TrayAction::Feed,
        TrayAction::Bath,
        TrayAction::Recall,
    ];

    /// 动作所属的事件名（`02 §7.6`：托盘事件统一为 `pet://tray`）。
    #[must_use]
    pub fn event_name(&self) -> &'static str {
        "pet://tray"
    }

    /// 事件载荷中的 `action` 字符串（小写下划线口径，`02 §7.6`）。
    #[must_use]
    pub fn payload_action(&self) -> &'static str {
        match self {
            TrayAction::ShowHide => "show_hide",
            TrayAction::ToggleTopmost => "toggle_topmost",
            TrayAction::ToggleClickThrough => "toggle_click_through",
            TrayAction::Settings => "settings",
            TrayAction::About => "about",
            TrayAction::Quit => "quit",
            TrayAction::Coax => "coax",
            TrayAction::Feed => "feed",
            TrayAction::Bath => "bath",
            TrayAction::Recall => "recall",
        }
    }
}

// ---------------------------------------------------------------------------
// 图标态
// ---------------------------------------------------------------------------

/// 托盘图标态（`01 §9.3` / `02 §5 K-9`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayIconState {
    /// 普通态（`tray.ico`）。
    Normal,
    /// 离家出走 / L5（`tray-gray.ico`，灰色版）。
    LeftHome,
    /// 不可交互态（穿透 / 钩子卸载，`01 FR-11-12`）：复用 `tray.ico`，
    /// 「不可交互」经 tooltip 后缀 `（不可交互）` 表达（角标版图标不在本模块交付物内）。
    NotInteractive,
}

impl Default for TrayIconState {
    /// 默认普通态。
    fn default() -> Self {
        TrayIconState::Normal
    }
}

// ---------------------------------------------------------------------------
// 菜单状态与菜单项
// ---------------------------------------------------------------------------

/// 生成菜单规格所需的运行时状态快照。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrayMenuState {
    /// 当前是否处于鼠标穿透态（决定兜底三项是否置顶，`01 FR-11-12`）。
    pub click_through: bool,
    /// 当前是否处于 L5 离家出走态（决定是否追加「把{name}找回来」）。
    pub left_home: bool,
    /// 当前是否处于置顶态（决定置顶切换项的文案）。
    pub topmost_on: bool,
}

/// 托盘菜单项规格（纯数据，供 app 层装配为 Tauri 菜单）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayMenuItem {
    /// 菜单项 id（`tray.*` 命名空间；见本文件 `ID_*` 常量）。
    pub id: &'static str,
    /// 展示文案（已按 `{name}` → 角色名替换）。
    pub label: String,
    /// 是否可点击。
    pub enabled: bool,
    /// 是否可见。
    pub visible: bool,
    /// 是否应置顶展示（`01 FR-11-12`：穿透不可达时兜底三项置顶）。
    pub top_priority: bool,
}

// ---------------------------------------------------------------------------
// 菜单项 id 常量（`menu_spec` 与 `action_for_menu_id` 双向引用的唯一来源）
// ---------------------------------------------------------------------------

/// 显示 / 隐藏。
pub const ID_SHOW_HIDE: &str = "tray.show_hide";
/// 置顶切换。
pub const ID_TOGGLE_TOPMOST: &str = "tray.toggle_topmost";
/// 穿透开关。
pub const ID_TOGGLE_CLICK_THROUGH: &str = "tray.toggle_click_through";
/// 设置。
pub const ID_SETTINGS: &str = "tray.settings";
/// 关于。
pub const ID_ABOUT: &str = "tray.about";
/// 退出。
pub const ID_QUIT: &str = "tray.quit";
/// 摸摸（FR-11-12 托盘替代入口）。
pub const ID_COAX: &str = "tray.coax";
/// 喂食（FR-11-12 托盘替代入口）。
pub const ID_FEED: &str = "tray.feed";
/// 洗澡（FR-11-12 托盘替代入口）。
pub const ID_BATH: &str = "tray.bath";
/// 把角色找回来（仅 L5）。
pub const ID_RECALL: &str = "tray.recall";

/// 摸摸项文案模板（C2：角色名走 `{name}` 占位）。
const TPL_COAX: &str = "❤ 摸摸{name}";
/// 找回来项文案模板（C2：角色名走 `{name}` 占位）。
const TPL_RECALL: &str = "把{name}找回来";

// ---------------------------------------------------------------------------
// 纯函数
// ---------------------------------------------------------------------------

/// 把标签模板中的 `{name}` 替换为角色名。
///
/// **C2 口径**：调用方传空串（尚无角色名来源，如配置未加载）时，**保留 `{name}` 占位
/// 原样返回**，绝不回退到任何硬编码角色名。
#[must_use]
pub fn fmt_label(template: &str, pet_name: &str) -> String {
    if pet_name.is_empty() {
        return template.to_string();
    }
    template.replace("{name}", pet_name)
}

/// 按运行时状态生成托盘菜单规格。
///
/// 产出顺序（`01 FR-1-10` / `FR-11-12`）：
///   1. **兜底三项**（FR-11-12 第 2 层，常驻；`top_priority` 由 `state.click_through` 决定）：
///      `tray.coax` / `tray.feed` / `tray.bath`；
///   2. **六项基础**：`tray.show_hide` / `tray.toggle_topmost` / `tray.toggle_click_through` /
///      `tray.settings` / `tray.about` / `tray.quit`；
///   3. **L5 追加**：`state.left_home == true` 时追加 `tray.recall`。
///
/// 普通态共 **9 项**，L5 态共 **10 项**；所有项 `enabled = true`（L5 时仍全部可达）。
#[must_use]
pub fn menu_spec(state: TrayMenuState, pet_name: &str) -> Vec<TrayMenuItem> {
    // 兜底三项是否置顶：穿透（不可达）时置顶展示（`01 FR-11-12`）。
    let fallback_top = state.click_through;

    let mut items = Vec::with_capacity(if state.left_home { 10 } else { 9 });

    // ① 兜底三项（FR-11-12 第 2 层；常驻）。
    items.push(TrayMenuItem {
        id: ID_COAX,
        label: fmt_label(TPL_COAX, pet_name),
        enabled: true,
        visible: true,
        top_priority: fallback_top,
    });
    items.push(TrayMenuItem {
        id: ID_FEED,
        label: fmt_label("🍙 喂食", pet_name),
        enabled: true,
        visible: true,
        top_priority: fallback_top,
    });
    items.push(TrayMenuItem {
        id: ID_BATH,
        label: fmt_label("🛁 洗澡", pet_name),
        enabled: true,
        visible: true,
        top_priority: fallback_top,
    });

    // ② 六项基础（FR-1-10）。
    items.push(TrayMenuItem {
        id: ID_SHOW_HIDE,
        // 可见态不易从平台层无损获取，按 `03` 卡片口径固定为「显示 / 隐藏」。
        label: fmt_label("显示 / 隐藏", pet_name),
        enabled: true,
        visible: true,
        top_priority: false,
    });
    items.push(TrayMenuItem {
        id: ID_TOGGLE_TOPMOST,
        label: fmt_label(
            if state.topmost_on { "取消置顶" } else { "置顶" },
            pet_name,
        ),
        enabled: true,
        visible: true,
        top_priority: false,
    });
    items.push(TrayMenuItem {
        id: ID_TOGGLE_CLICK_THROUGH,
        label: fmt_label(
            if state.click_through { "关闭穿透" } else { "开启穿透" },
            pet_name,
        ),
        enabled: true,
        visible: true,
        top_priority: false,
    });
    items.push(TrayMenuItem {
        id: ID_SETTINGS,
        label: fmt_label("⚙ 设置…", pet_name),
        enabled: true,
        visible: true,
        top_priority: false,
    });
    items.push(TrayMenuItem {
        id: ID_ABOUT,
        label: fmt_label("关于", pet_name),
        enabled: true,
        visible: true,
        top_priority: false,
    });
    items.push(TrayMenuItem {
        id: ID_QUIT,
        label: fmt_label("✕ 退出", pet_name),
        enabled: true,
        visible: true,
        top_priority: false,
    });

    // ③ L5 离家出走：追加「把{name}找回来」（其余项保持可达）。
    if state.left_home {
        items.push(TrayMenuItem {
            id: ID_RECALL,
            label: fmt_label(TPL_RECALL, pet_name),
            enabled: true,
            visible: true,
            top_priority: false,
        });
    }

    items
}

/// 菜单项 id → 动作的映射（**与 [`menu_spec`] 的 id 集合双向完备**）。
///
/// 未知 id（如分隔符）返回 `None`。
#[must_use]
pub fn action_for_menu_id(id: &str) -> Option<TrayAction> {
    let action = match id {
        ID_SHOW_HIDE => TrayAction::ShowHide,
        ID_TOGGLE_TOPMOST => TrayAction::ToggleTopmost,
        ID_TOGGLE_CLICK_THROUGH => TrayAction::ToggleClickThrough,
        ID_SETTINGS => TrayAction::Settings,
        ID_ABOUT => TrayAction::About,
        ID_QUIT => TrayAction::Quit,
        ID_COAX => TrayAction::Coax,
        ID_FEED => TrayAction::Feed,
        ID_BATH => TrayAction::Bath,
        ID_RECALL => TrayAction::Recall,
        _ => return None,
    };
    Some(action)
}

/// 图标态 → 图标文件名（相对 `src-tauri/icons/`）。
///
/// `NotInteractive` **复用 `tray.ico`**：角标版图标不在本模块交付物内，角标语义由
/// app 层 tooltip 后缀 `（不可交互）` 表达。
#[must_use]
pub fn icon_for_state(state: TrayIconState) -> &'static str {
    match state {
        TrayIconState::Normal => "tray.ico",
        TrayIconState::LeftHome => "tray-gray.ico",
        TrayIconState::NotInteractive => "tray.ico",
    }
}

// ---------------------------------------------------------------------------
// 单元测试（纯逻辑，不依赖真机）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// 普通态样本：非穿透、非 L5、置顶中。
    fn normal_state() -> TrayMenuState {
        TrayMenuState { click_through: false, left_home: false, topmost_on: true }
    }

    /// 穿透态样本：穿透开启（不可达兜底置顶）。
    fn click_through_state() -> TrayMenuState {
        TrayMenuState { click_through: true, left_home: false, topmost_on: true }
    }

    /// L5 态样本：离家出走。
    fn left_home_state() -> TrayMenuState {
        TrayMenuState { click_through: false, left_home: true, topmost_on: true }
    }

    #[test]
    fn menu_spec_item_counts_and_top_priority() {
        // 普通态：9 项，兜底三项不置顶。
        let normal = menu_spec(normal_state(), "小星");
        assert_eq!(normal.len(), 9, "普通态应为 9 项");
        for item in &normal {
            assert!(!item.top_priority, "普通态不应有置顶项：{}", item.id);
            assert!(item.enabled, "所有项均应 enabled：{}", item.id);
            assert!(item.visible, "所有项均应 visible：{}", item.id);
        }

        // 穿透态：9 项，且仅兜底三项置顶。
        let ct = menu_spec(click_through_state(), "小星");
        assert_eq!(ct.len(), 9, "穿透态应为 9 项");
        let top_ids: BTreeSet<&str> =
            ct.iter().filter(|i| i.top_priority).map(|i| i.id).collect();
        assert_eq!(
            top_ids,
            BTreeSet::from([ID_COAX, ID_FEED, ID_BATH]),
            "穿透态仅兜底三项应置顶"
        );

        // L5 态：10 项（追加 recall），且最后一项为 recall。
        let l5 = menu_spec(left_home_state(), "小星");
        assert_eq!(l5.len(), 10, "L5 态应为 10 项");
        assert_eq!(l5.last().map(|i| i.id), Some(ID_RECALL));

        // 穿透 + L5：10 项，兜底三项仍置顶。
        let both = menu_spec(
            TrayMenuState { click_through: true, left_home: true, topmost_on: false },
            "小星",
        );
        assert_eq!(both.len(), 10);
        assert_eq!(both.iter().filter(|i| i.top_priority).count(), 3);
    }

    #[test]
    fn menu_spec_order_matches_design() {
        let l5 = menu_spec(left_home_state(), "小星");
        let ids: Vec<&str> = l5.iter().map(|i| i.id).collect();
        assert_eq!(
            ids,
            vec![
                ID_COAX,
                ID_FEED,
                ID_BATH,
                ID_SHOW_HIDE,
                ID_TOGGLE_TOPMOST,
                ID_TOGGLE_CLICK_THROUGH,
                ID_SETTINGS,
                ID_ABOUT,
                ID_QUIT,
                ID_RECALL,
            ]
        );
    }

    #[test]
    fn menu_spec_labels_reflect_state() {
        let topmost_on = menu_spec(normal_state(), "小星");
        let topmost_item = topmost_on.iter().find(|i| i.id == ID_TOGGLE_TOPMOST).unwrap();
        assert_eq!(topmost_item.label, "取消置顶");

        let topmost_off = menu_spec(
            TrayMenuState { click_through: false, left_home: false, topmost_on: false },
            "小星",
        );
        let topmost_item = topmost_off.iter().find(|i| i.id == ID_TOGGLE_TOPMOST).unwrap();
        assert_eq!(topmost_item.label, "置顶");

        let ct = menu_spec(click_through_state(), "小星");
        let ct_item = ct.iter().find(|i| i.id == ID_TOGGLE_CLICK_THROUGH).unwrap();
        assert_eq!(ct_item.label, "关闭穿透");

        let coax = ct.iter().find(|i| i.id == ID_COAX).unwrap();
        assert_eq!(coax.label, "❤ 摸摸小星");
    }

    #[test]
    fn action_for_menu_id_maps_all_nine_plus_recall() {
        let pairs = [
            (ID_SHOW_HIDE, TrayAction::ShowHide),
            (ID_TOGGLE_TOPMOST, TrayAction::ToggleTopmost),
            (ID_TOGGLE_CLICK_THROUGH, TrayAction::ToggleClickThrough),
            (ID_SETTINGS, TrayAction::Settings),
            (ID_ABOUT, TrayAction::About),
            (ID_QUIT, TrayAction::Quit),
            (ID_COAX, TrayAction::Coax),
            (ID_FEED, TrayAction::Feed),
            (ID_BATH, TrayAction::Bath),
            (ID_RECALL, TrayAction::Recall),
        ];
        for (id, action) in pairs {
            assert_eq!(action_for_menu_id(id), Some(action), "id 映射错误：{id}");
        }

        // 未知 id / 空串 → None。
        assert_eq!(action_for_menu_id("tray.unknown"), None);
        assert_eq!(action_for_menu_id(""), None);
        assert_eq!(action_for_menu_id("separator"), None);
    }

    #[test]
    fn action_for_menu_id_is_bidirectionally_complete_with_menu_spec() {
        // menu_spec 产出的每个 id 都能映射回动作。
        let produced_ids: BTreeSet<&str> = menu_spec(left_home_state(), "小星")
            .iter()
            .map(|i| i.id)
            .collect();
        let mapped_ids: BTreeSet<&str> = TrayAction::ALL
            .iter()
            .filter_map(|a| menu_id_for_action(*a))
            .collect();
        assert_eq!(
            produced_ids, mapped_ids,
            "menu_spec 的 id 集合与 action_for_menu_id 的映射集合必须一致"
        );

        // 每个产出的 id 都能解析为动作。
        for item in menu_spec(left_home_state(), "小星") {
            assert!(action_for_menu_id(item.id).is_some(), "id 未映射：{}", item.id);
        }
    }

    /// 反向查找：动作 → 菜单 id（仅供本测试做双向完备性断言）。
    fn menu_id_for_action(action: TrayAction) -> Option<&'static str> {
        [
            ID_SHOW_HIDE,
            ID_TOGGLE_TOPMOST,
            ID_TOGGLE_CLICK_THROUGH,
            ID_SETTINGS,
            ID_ABOUT,
            ID_QUIT,
            ID_COAX,
            ID_FEED,
            ID_BATH,
            ID_RECALL,
        ]
        .into_iter()
        .find(|id| action_for_menu_id(id) == Some(action))
    }

    #[test]
    fn payload_action_strings_are_exact() {
        assert_eq!(TrayAction::ShowHide.payload_action(), "show_hide");
        assert_eq!(TrayAction::ToggleTopmost.payload_action(), "toggle_topmost");
        assert_eq!(
            TrayAction::ToggleClickThrough.payload_action(),
            "toggle_click_through"
        );
        assert_eq!(TrayAction::Settings.payload_action(), "settings");
        assert_eq!(TrayAction::About.payload_action(), "about");
        assert_eq!(TrayAction::Quit.payload_action(), "quit");
        assert_eq!(TrayAction::Coax.payload_action(), "coax");
        assert_eq!(TrayAction::Feed.payload_action(), "feed");
        assert_eq!(TrayAction::Bath.payload_action(), "bath");
        assert_eq!(TrayAction::Recall.payload_action(), "recall");

        // 事件名统一。
        for action in TrayAction::ALL {
            assert_eq!(action.event_name(), "pet://tray");
        }

        // 载荷字符串两两不同（可作为唯一键）。
        let payloads: BTreeSet<&str> =
            TrayAction::ALL.iter().map(|a| a.payload_action()).collect();
        assert_eq!(payloads.len(), TrayAction::ALL.len());
    }

    #[test]
    fn icon_for_state_mapping() {
        assert_eq!(icon_for_state(TrayIconState::Normal), "tray.ico");
        assert_eq!(icon_for_state(TrayIconState::LeftHome), "tray-gray.ico");
        // 角标版图标不在本模块交付物内：NotInteractive 复用 tray.ico。
        assert_eq!(icon_for_state(TrayIconState::NotInteractive), "tray.ico");
        assert_eq!(TrayIconState::default(), TrayIconState::Normal);
    }

    #[test]
    fn fmt_label_replaces_or_keeps_placeholder() {
        assert_eq!(fmt_label("❤ 摸摸{name}", "小星"), "❤ 摸摸小星");
        assert_eq!(fmt_label("把{name}找回来", "小星"), "把小星找回来");
        // 无占位符的模板原样返回。
        assert_eq!(fmt_label("🍙 喂食", "小星"), "🍙 喂食");
        // 空角色名 → 保留 {name} 占位（C2：绝不回退到硬编码角色名）。
        assert_eq!(fmt_label("❤ 摸摸{name}", ""), "❤ 摸摸{name}");
        assert_eq!(fmt_label("把{name}找回来", ""), "把{name}找回来");
    }

    #[test]
    fn menu_labels_never_hardcode_role_name() {
        // 用码点构造角色名进行断言，避免本测试源码出现字面量（与 C2 静态扫描口径一致）。
        let role_name = "\u{5fc3}\u{6708}\u{72d0}";

        // pet_name 传空：全部标签保留 {name} 占位，不得出现字面角色名。
        for state in [normal_state(), click_through_state(), left_home_state()] {
            for item in menu_spec(state, "") {
                assert!(
                    !item.label.contains(role_name),
                    "菜单标签不得硬编码角色名：{}",
                    item.label
                );
                assert!(!item.label.is_empty(), "菜单标签不得为空：{}", item.id);
            }
        }

        // 含占位符的两项在空名下必须保留 {name}。
        let empty = menu_spec(left_home_state(), "");
        let coax = empty.iter().find(|i| i.id == ID_COAX).unwrap();
        assert!(coax.label.contains("{name}"));
        let recall = empty.iter().find(|i| i.id == ID_RECALL).unwrap();
        assert!(recall.label.contains("{name}"));
    }
}
