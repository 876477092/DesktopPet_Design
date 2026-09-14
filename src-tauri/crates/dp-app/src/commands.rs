//! `dp-app/src/commands.rs` —— 前端 invoke 命令统一收口（S3-M6，T-10 段 · 下）。
//!
//! 职责：右键菜单命令（`menu_command`）的路由与执行——前端「隐藏」菜单项经
//! `invoke('menu_command', { command: 'hide' })` 到达此处；现阶段唯一命令为
//! `hide`（执行：隐藏 pet 窗口）。其余八项菜单当前灰化（`01 §8.2`），不可达
//! 即不产命令；后续里程碑新菜单命令**一律**在本枚举登记后扩展，前端不得
//! 直接 invoke 平台窗口命令（收口单一出口）。
//!
//! ## 跨模块硬约束
//!   - **托盘协同**：隐藏执行统一走 [`crate::tray_menu::set_pet_visible`]（与托盘
//!     `ShowHide` 同一写点），保证 `PET_VISIBLE` 可见态镜像一致；
//!   - **C8**：命令执行零新增 `pet://` 事件（可见态变化的前端感知归后续里程碑）；
//!   - **C3 / C9**：零时钟、零网络。
//!
//! 模块**不加**整体 `#[cfg(windows)]`：`generate_handler!` 注册需无条件编译；
//! Windows 专属实现下沉到 [`hide_pet_window`] 的 cfg 分支（非 Windows 目标返回
//! 可读错误，保证 `cargo check` 跨平台可编译）。

use serde::Deserialize;
use tauri::AppHandle;

/// 右键菜单命令枚举（S3-M6；新增命令在此登记，勿在前端自创字符串）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MenuCommand {
    /// 隐藏宠物窗口（`01 §8.2` 菜单「隐藏」项；复用既有窗口能力）。
    Hide,
}

/// 右键菜单命令（前端 `invoke('menu_command', { command })`）。
///
/// # Errors
/// 命令执行失败（平台窗口操作失败 / 非 Windows 目标）返回中文可读错误串
/// （`02 §7.4.3`；前端降级日志，不 panic）。
#[tauri::command]
pub fn menu_command(app: AppHandle, command: MenuCommand) -> Result<(), String> {
    match command {
        MenuCommand::Hide => hide_pet_window(&app),
    }
}

/// 隐藏 pet 窗口（托盘可见态镜像统一收口；Windows 实现）。
#[cfg(windows)]
fn hide_pet_window(app: &AppHandle) -> Result<(), String> {
    use tauri::Manager;

    // 平台窗口层装配前置校验（未装配即失败，不 panic）；实际隐藏走托盘统一写点。
    if app.try_state::<crate::PetPlatform>().is_none() {
        return Err("平台窗口层尚未装配，无法隐藏窗口".to_string());
    }
    crate::tray_menu::set_pet_visible(app, false)
}

/// 非 Windows 目标占位（命令仍注册、调用返回可读错误，保证跨平台可编译）。
#[cfg(not(windows))]
fn hide_pet_window(_app: &AppHandle) -> Result<(), String> {
    Err("menu_command 仅在 Windows 目标下可用".to_string())
}

// ---------------------------------------------------------------------------
// S4-M4：情绪兜底命令（设置页「重置情绪」/ 托盘「把心月狐找回来」）
// ---------------------------------------------------------------------------

/// 情绪兜底命令枚举（S4-M4；新增命令在此登记，勿在前端自创字符串）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EmotionCommand {
    /// 重置情绪（`force_lower` 兜底，`02 §5.23` R18：L5 强制解除）。
    Reset,
    /// 把角色找回来（L5 离家 → 走回，`01 §6.5.2`）。
    Recall,
}

/// 情绪兜底命令（前端 `invoke('pet_emotion_command', { command })`）。
///
/// 只做**投递**：把指令经 [`crate::bridge::CoreInputChannel`] 送给 core-loop 逻辑档
/// 落地（core-loop 单线程 Actor 口径，避免跨线程直改内核状态）。
///
/// # Errors
/// 入站通道尚未装配时返回中文可读错误串（`02 §7.4.3`；前端降级日志，不 panic）。
#[tauri::command]
pub fn pet_emotion_command(app: AppHandle, command: EmotionCommand) -> Result<(), String> {
    use tauri::Manager;

    let channel = app
        .try_state::<crate::bridge::CoreInputChannel>()
        .ok_or_else(|| "core-loop 入站通道尚未装配，无法下发情绪命令".to_string())?;
    channel.push(match command {
        EmotionCommand::Reset => crate::bridge::CoreInput::ResetEmotion,
        EmotionCommand::Recall => crate::bridge::CoreInput::RecallRunaway,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_command_deserializes_lowercase_and_rejects_unknown() {
        assert_eq!(
            serde_json::from_str::<MenuCommand>("\"hide\"").expect("小写应可解析"),
            MenuCommand::Hide
        );
        assert!(serde_json::from_str::<MenuCommand>("\"Hide\"").is_err(), "严格小写");
        assert!(
            serde_json::from_str::<MenuCommand>("\"unknown\"").is_err(),
            "未登记命令必须拒绝（收口单一出口）"
        );
    }

    #[test]
    fn emotion_command_deserializes_camel_case_and_rejects_unknown() {
        assert_eq!(
            serde_json::from_str::<EmotionCommand>("\"reset\"").expect("小写应可解析"),
            EmotionCommand::Reset
        );
        assert_eq!(
            serde_json::from_str::<EmotionCommand>("\"recall\"").expect("小写应可解析"),
            EmotionCommand::Recall
        );
        assert!(
            serde_json::from_str::<EmotionCommand>("\"reboot\"").is_err(),
            "未登记情绪命令必须拒绝（收口单一出口）"
        );
    }
}
