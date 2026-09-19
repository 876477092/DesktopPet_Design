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

use serde::{Deserialize, Serialize};
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

/// 托盘替代交互入口命令（**S7-M6** / `02 §5.23`：`pet_tray_coax(step)`）。
///
/// 语义：不可达期间（穿透 / 勿扰 / 已离家）桌面交互完全失效，`FR-11-12` 层 ② 要求
/// **托盘菜单**提供替代入口。托盘菜单项自身已投递（`tray_menu::apply_action`），
/// 本命令是给**前端**（情绪原因卡的「摸摸 / 喂食 / 洗澡」快捷按钮、设置页引导）用的
/// 等价入口——两者走同一条入站通道与同一内核接口，故行为完全一致。
///
/// `step` 参数只作**诊断标记**（`"coax"` / `"feed"` / `"bath"` / `"recall"`），
/// **不驱动状态机**：抚摸阶段推进一律由 `CoaxFlow` 自身状态决定（单一真源，
/// 避免前端传参与内核状态不一致时越级完成三部曲）。
///
/// # Errors
/// 入站通道未装配（纯逻辑模式 / core-loop 未起）时返回中文可读错误串。
#[tauri::command]
pub fn pet_tray_coax(app: AppHandle, step: String) -> Result<(), String> {
    use tauri::Manager;

    let channel = app
        .try_state::<crate::bridge::CoreInputChannel>()
        .ok_or_else(|| "core-loop 入站通道尚未装配，无法下发托盘交互".to_string())?;
    let input = match step.as_str() {
        "coax" => crate::bridge::CoreInput::TrayCoax,
        "feed" => crate::bridge::CoreInput::TrayFeed,
        "bath" => crate::bridge::CoreInput::TrayBath,
        "recall" => crate::bridge::CoreInput::RecallRunaway,
        other => return Err(format!("未知的托盘交互步骤：{other}")),
    };
    channel.push(input);
    Ok(())
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

// ---------------------------------------------------------------------------
// S8-M1/M3：活动命令（前端活动卡「出发 / 召回」按钮；`01 §6.13`）
// ---------------------------------------------------------------------------

/// 派遣外出活动（前端 `invoke('pet_dispatch', { kind, defId, durationMin })`）。
///
/// 只做**投递**：`CoreInput::ActivityDispatch` 交给 core-loop 逻辑档，前置校验
/// （唯一性 / AC-36 / RV-07/03/01 / 安静时段）在 core-loop 以**当时内核快照**执行；
/// 被拒时前端经 1Hz `pet://state` 快照的 `activity.phase` 保持 `idle` 感知。
///
/// # Errors
/// 入站通道尚未装配（纯逻辑模式 / core-loop 未起）时返回中文可读错误串。
#[tauri::command]
pub fn pet_dispatch(
    app: AppHandle,
    kind: String,
    def_id: String,
    duration_min: u32,
) -> Result<(), String> {
    use tauri::Manager;

    let channel = app
        .try_state::<crate::bridge::CoreInputChannel>()
        .ok_or_else(|| "core-loop 入站通道尚未装配，无法派遣活动".to_string())?;
    channel.push(crate::bridge::CoreInput::ActivityDispatch {
        kind,
        def_id,
        duration_min,
    });
    Ok(())
}

/// 提前召回进行中的活动（前端 `invoke('pet_recall')`）。
///
/// 只做**投递**：`CoreInput::ActivityRecall` 交给 core-loop（Running → Returning，
/// 收益按已完成比例 ×0.5 + P+6 + rough+0.15，`02 §5.14`）。
///
/// # Errors
/// 入站通道尚未装配时返回中文可读错误串。
#[tauri::command]
pub fn pet_recall(app: AppHandle) -> Result<(), String> {
    use tauri::Manager;

    let channel = app
        .try_state::<crate::bridge::CoreInputChannel>()
        .ok_or_else(|| "core-loop 入站通道尚未装配，无法召回活动".to_string())?;
    channel.push(crate::bridge::CoreInput::ActivityRecall);
    Ok(())
}

/// S8-M6：商城购买（前端 `invoke('pet_buy', { itemId, qty })`）。
///
/// 只做**投递**：`CoreInput::Purchase` 交给 core-loop 逻辑档，余额 / 限购 / 日顶 /
/// 失败冲正在 core-loop 以当时账本快照执行；结果经 1Hz `pet://state` 的 economy/inventory
/// 字段回传前端。
///
/// # Errors
/// 入站通道尚未装配时返回中文可读错误串。
#[tauri::command]
pub fn pet_buy(app: AppHandle, item_id: String, qty: u32) -> Result<(), String> {
    use tauri::Manager;

    let channel = app
        .try_state::<crate::bridge::CoreInputChannel>()
        .ok_or_else(|| "core-loop 入站通道尚未装配，无法购买".to_string())?;
    channel.push(crate::bridge::CoreInput::Purchase { item_id, qty });
    Ok(())
}

// ---------------------------------------------------------------------------
// S10-M1：桌面装饰摆放 / 取下（`01 FR-13-6`；5 槽）。
//
// 只投递：校验（槽位越界 / 背包拥有 / 落盘）在 core-loop 单写者线程内完成，
// 命令层不碰存档（与 `pet_buy` 同纪律）。
// ---------------------------------------------------------------------------

/// 设置页相册 Tab：把已拥有的摆件摆到桌面某槽（0..=4）。
#[tauri::command]
pub fn pet_decor_place(app: AppHandle, slot: u8, item_id: String) -> Result<(), String> {
    use tauri::Manager;
    let channel = app
        .try_state::<crate::bridge::CoreInputChannel>()
        .ok_or_else(|| "core-loop 入站通道尚未装配".to_string())?;
    channel.push(crate::bridge::CoreInput::DecorPlace { slot, item_id });
    Ok(())
}

/// 设置页相册 Tab：取下桌面某槽装饰。
#[tauri::command]
pub fn pet_decor_remove(app: AppHandle, slot: u8) -> Result<(), String> {
    use tauri::Manager;
    let channel = app
        .try_state::<crate::bridge::CoreInputChannel>()
        .ok_or_else(|| "core-loop 入站通道尚未装配".to_string())?;
    channel.push(crate::bridge::CoreInput::DecorRemove { slot });
    Ok(())
}

// ---------------------------------------------------------------------------
// S5-M4：设置命令（设置页 ⇄ 应用层设置状态；`01 FR-7` / `02 §7.6 pet://config`）
// ---------------------------------------------------------------------------

/// 读取当前**有效设置快照**（`settings.json` 出厂默认 ⊕ 存档 B 段用户值）。
///
/// 设置窗口挂载后第一件事即调用本命令；`pet://config` 事件只广播摘要（revision），
/// 窗口收到后据此**再拉一次**本命令（避免每次改动全量广播 ~1KB 载荷）。
///
/// # Errors
/// 设置状态尚未装配（core-loop 未起来 / 纯逻辑模式）时返回可读错误串：前端降级为
/// 内置默认 + 只读预览（`02 §7.4.2`），不阻断窗口显示。
#[tauri::command]
pub fn settings_get(app: AppHandle) -> Result<crate::bridge::SettingsSnapshot, String> {
    use tauri::Manager;
    let state = app
        .try_state::<crate::bridge::SettingsState>()
        .ok_or_else(|| "设置服务尚未装配".to_string())?;
    Ok(state.snapshot())
}

/// 应用设置补丁（**热更新的唯一写入点**）。
///
/// 执行顺序（重要，保证「10s 内生效」与「单写者」两条硬约束同时成立）：
///   1. 合并补丁进设置快照（含取值域夹紧）+ `revision += 1`；
///   2. **立即**落地应用层效果（窗口缩放/不透明度、置顶、穿透、音量/静音、自启）；
///   3. 把补丁入队交 core-loop（由其在业务档落到引擎与存档，并广播 `pet://config`）。
///
/// 第 2 步在此处同步完成的原因：这些设置项作用于**应用层**（窗口 / 音频总线 / 注册表），
/// 与 core-loop 无关；走 core-loop 只会平白引入一拍延迟。第 3 步留给 core-loop 的是
/// **内核侧**设置（敏感度 / 勿扰 / 感知 / 名字 / 提醒偏好）与**存档写入**。
///
/// # Errors
/// 设置服务未装配 / 补丁写入失败（锁中毒）时返回可读错误串。
#[tauri::command]
pub fn settings_apply(
    app: AppHandle,
    patch: crate::bridge::SettingsPatch,
) -> Result<crate::bridge::SettingsSnapshot, String> {
    use tauri::Manager;
    let state = app
        .try_state::<crate::bridge::SettingsState>()
        .ok_or_else(|| "设置服务尚未装配".to_string())?;
    let revision = state
        .apply(&patch)
        .ok_or_else(|| "设置状态写入失败（内部锁异常）".to_string())?;
    let snapshot = state.snapshot();
    apply_platform_effects(&app, &snapshot, revision);
    Ok(snapshot)
}

/// 重置全部数据（`01 FR-8-4`：二次确认后清档重建）。
///
/// 只**投递**给 core-loop（存档唯一写者）：core-loop 重置内存档 + 立刻落盘 → 重启进程，
/// 使 B/C/D 段全部回到出厂态（未来 S8 经济/背包/成就也一并清空，无需逐段实现重置）。
///
/// # Errors
/// 入站通道未装配时返回可读错误串。
#[tauri::command]
pub fn settings_reset_all(app: AppHandle) -> Result<(), String> {
    push_core_input(&app, crate::bridge::CoreInput::ResetAllData)
}

// ---------------------------------------------------------------------------
// S5-M4：存档命令（设置页「数据」Tab；`01 FR-8-2` / `02 §5 K-7`）
// ---------------------------------------------------------------------------

/// 存档操作命令（收口单一出口；未登记动作一律拒绝）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SaveCommand {
    /// 重置全部数据（清档重建 + 重启）。
    Reset,
    /// 导入指定备份档（`save.json.bak` / `save.corrupt.*.json`）。
    Import,
}

/// 「数据」Tab 的存档健康状态（`01 FR-8-2` 的**用户可见提示面**）。
///
/// 背景（S5-M1 裁定 ④）：`02 §5 K-7` 要求损坏档「重建默认档**并提示**」，而 S5-M1
/// 只做到「隔离 + 重建 + 日志」——`pet://` 事件面无存档状态事件、托盘无通知 API，
/// 提示必须有一处 UI 承载。本命令即为该承载面：设置页「数据」Tab 挂载时查询并展示。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveStatus {
    /// 存档主文件绝对路径（只读展示；用户可据此手工回捞）。
    pub path: String,
    /// 主档是否存在。
    pub exists: bool,
    /// 加载落点（`fresh` / `loaded` / `recoveredFromBak` / `isolatedCorrupt` /
    /// `isolatedFuture` / `migrationPending` / `unknown` / `unavailable`）。
    pub state: String,
    /// 是否健康（`false` → UI 必须显示提示条）。
    pub healthy: bool,
    /// 是否需要向用户提示（损坏 / 未来版本 / 待迁移）。
    pub needs_notice: bool,
    /// 当前是否允许写盘（`false` = v1 待迁移的保护态 / 存档未装配）。
    pub writable: bool,
    /// 是否已装配存档（`false` = `%APPDATA%` 解析失败等降级态）。
    pub available: bool,
    /// 最近一次落盘记录的墙钟毫秒（0 = 从未落盘）。
    pub last_seen_ms: i64,
    /// 候选备份清单（可导入者在前）。
    pub backups: Vec<crate::bridge::SaveBackup>,
}

/// 查询存档健康状态与可用备份（设置页「数据」Tab）。
///
/// 永不 `Err`：存档不可用时返回 `available = false` + `needs_notice = true` 的降级状态
/// （UI 仍能如实显示「存档不可用」，比抛错更有用）。
#[tauri::command]
#[must_use]
pub fn save_status(app: AppHandle) -> SaveStatus {
    use tauri::Manager;

    let Some(dir) = crate::bridge::resolve_save_dir(&app) else {
        return SaveStatus {
            path: String::new(),
            exists: false,
            state: "unavailable".to_string(),
            healthy: false,
            needs_notice: true,
            writable: false,
            available: false,
            last_seen_ms: 0,
            backups: Vec::new(),
        };
    };

    let main = dir.join(crate::bridge::SAVE_FILE_NAME);
    let exists = main.is_file();
    let backups = crate::bridge::list_save_backups(&dir);

    // 状态真源 = core-loop 装配期记录的加载落点（未装配 → `unknown`，按「需提示」处理）。
    let loaded = app.try_state::<crate::bridge::SaveStatusHandle>();
    let (state, healthy, needs_notice, writable, last_seen_ms) = match loaded.as_deref() {
        Some(handle) => {
            let info = handle.get();
            (info.state, info.healthy, info.needs_notice, info.writable, info.last_seen_ms)
        }
        None => ("unknown".to_string(), !exists, !exists, false, 0),
    };

    SaveStatus {
        path: main.to_string_lossy().to_string(),
        exists,
        state,
        healthy,
        needs_notice,
        writable,
        available: true,
        last_seen_ms,
        backups,
    }
}

/// 存档操作（`reset` 清档重建 / `import` 导入指定备份；两者均以「重启」收尾）。
///
/// `file` 仅 `import` 时需要：**只接受存档目录内的备份文件名**（第二道收口；
/// 白名单判定在 [`crate::bridge::resolve_backup_path`]）。
///
/// # Errors
/// 未登记命令 / 文件名不合法 / 入站通道未装配时返回可读错误串。
#[tauri::command]
pub fn save_command(
    app: AppHandle,
    command: SaveCommand,
    file: Option<String>,
) -> Result<(), String> {
    match command {
        SaveCommand::Reset => push_core_input(&app, crate::bridge::CoreInput::ResetAllData),
        SaveCommand::Import => {
            let Some(name) = file else {
                return Err("导入存档需要提供备份文件名".to_string());
            };
            let dir = crate::bridge::resolve_save_dir(&app)
                .ok_or_else(|| "存档目录不可用，无法导入".to_string())?;
            // 先做白名单 + 存在性校验（命令层即拒绝非法请求，不把脏输入交给 core-loop）。
            let _ = crate::bridge::resolve_backup_path(&dir, &name)?;
            push_core_input(&app, crate::bridge::CoreInput::ImportSave { file: name })
        }
    }
}

/// 投递入站指令（core-loop 未装配 → 可读错误；不 panic）。
fn push_core_input(app: &AppHandle, input: crate::bridge::CoreInput) -> Result<(), String> {
    use tauri::Manager;
    let channel = app
        .try_state::<crate::bridge::CoreInputChannel>()
        .ok_or_else(|| "core-loop 入站通道尚未装配，无法下发指令".to_string())?;
    channel.push(input);
    Ok(())
}

/// 装配期落地一次设置的应用层效果（`lib.rs` 的 `setup` 末尾调用）。
///
/// 存在的理由（`01 FR-7`「持久化」）：设置存在存档里，重启后若不在装配期重新落地，
/// 用户会看到「设置记住了但没生效」（体积/透明度/音量回到默认）。此函数把
/// [`apply_platform_effects`] 在启动时跑一遍，与「改设置时立即落地」共用同一实现。
///
/// 未装配 `SettingsState`（纯逻辑模式 / 平台层降级）→ 静默返回（`02 §7.4.2`）。
pub fn apply_startup_settings(app: &AppHandle) {
    use tauri::Manager;

    let Some(state) = app.try_state::<crate::bridge::SettingsState>() else {
        return;
    };
    let snapshot = state.snapshot();
    apply_platform_effects(app, &snapshot, snapshot.revision);
}

/// 应用层设置项的**立即生效**落地（窗口 / 音频 / 注册表）。
///
/// 非 Windows 目标为空实现（跨平台 `cargo check` 可编译；本项目仅 Windows 目标）。
#[cfg(not(windows))]
fn apply_platform_effects(
    _app: &AppHandle,
    _snapshot: &crate::bridge::SettingsSnapshot,
    _revision: u64,
) {
}

/// Windows 实现：逐项落地应用层设置（每一项独立降级，单项失败不影响其余项）。
#[cfg(windows)]
fn apply_platform_effects(
    app: &AppHandle,
    snapshot: &crate::bridge::SettingsSnapshot,
    revision: u64,
) {
    apply_window_effects(app, snapshot);
    apply_alpha_effect(app, snapshot);
    apply_audio_effect(app, snapshot);
    apply_autostart_effect(app, snapshot, revision);
}

/// ① 窗口：缩放（逻辑尺寸 + 用户缩放系数）、置顶、穿透。
#[cfg(windows)]
fn apply_window_effects(app: &AppHandle, snapshot: &crate::bridge::SettingsSnapshot) {
    use dp_platform::PlatformWindow;
    use dp_platform::traits::TopmostMode;
    use tauri::Manager;

    let Some(platform) = app.try_state::<crate::PetPlatform>() else {
        return;
    };
    let window = &platform.window;
    let logical = crate::BRIDGE_BASE_LOGICAL_SIZE * snapshot.scale_percent / 100;
    if let Err(err) = window.set_size_logical(logical, logical) {
        eprintln!("[dp-app] 设置热更新：窗口缩放失败（{err}）");
    }
    window.set_user_scale(snapshot.scale_percent as f32 / 100.0);
    let mode = crate::bridge::normalize_topmost_policy(&snapshot.always_on_top_policy)
        .unwrap_or("Always");
    let mode = match mode {
        "Never" => TopmostMode::Never,
        "BelowFullscreen" => TopmostMode::BelowFullscreen,
        _ => TopmostMode::Always,
    };
    if let Err(err) = window.set_topmost(mode) {
        eprintln!("[dp-app] 设置热更新：置顶策略失败（{err}）");
    }
    if let Err(err) = window.set_click_through(snapshot.click_through) {
        eprintln!("[dp-app] 设置热更新：穿透开关失败（{err}）");
    }
    crate::tray_menu::note_topmost(snapshot.always_on_top_policy == "Always");
}

/// ② 不透明度：走渲染帧 alpha（窗口不做分层 alpha，`02 §2.3` 定版口径）。
#[cfg(windows)]
fn apply_alpha_effect(app: &AppHandle, snapshot: &crate::bridge::SettingsSnapshot) {
    use tauri::Manager;

    if let Some(alpha) = app.try_state::<crate::bridge::FrameAlpha>() {
        alpha.set_percent(snapshot.opacity_percent, snapshot.opacity_min_percent);
    }
}

/// ③ 音频：音量 / 静音 / 勿扰（勿扰同时影响门控优先级，见 `dp_audio::resolve_play`）。
#[cfg(windows)]
fn apply_audio_effect(app: &AppHandle, snapshot: &crate::bridge::SettingsSnapshot) {
    use tauri::Manager;

    if let Some(bus) = app.try_state::<dp_audio::AudioBus>() {
        bus.set_settings(dp_audio::AudioSettings {
            master_volume_percent: snapshot.master_volume_percent,
            muted: snapshot.muted,
            do_not_disturb: snapshot.do_not_disturb,
            click_through: snapshot.click_through,
        });
    }
}

/// ④ 开机自启：真实写注册表；失败**不谎报**（把开关位回滚为用户可见的真实态）。
#[cfg(windows)]
fn apply_autostart_effect(
    app: &AppHandle,
    snapshot: &crate::bridge::SettingsSnapshot,
    revision: u64,
) {
    use tauri::Manager;

    let Some(state) = app.try_state::<crate::bridge::SettingsState>() else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("[dp-app] 设置热更新：取当前可执行文件路径失败，自启跳过");
        return;
    };
    match dp_platform::win::autostart::set_enabled(snapshot.autostart, &exe) {
        Ok(actual) => {
            let actual_on = actual.is_on();
            if actual_on != snapshot.autostart {
                eprintln!(
                    "[dp-app] 设置热更新：自启真实状态为 {actual:?}，回写设置快照（revision={revision}）"
                );
                let fix = crate::bridge::SettingsPatch {
                    autostart: Some(actual_on),
                    ..crate::bridge::SettingsPatch::default()
                };
                let _ = state.apply(&fix);
            }
        }
        Err(err) => {
            eprintln!("[dp-app] 设置热更新：写入自启注册表失败（{err}），开关回滚为关闭");
            let fix = crate::bridge::SettingsPatch {
                autostart: Some(false),
                ..crate::bridge::SettingsPatch::default()
            };
            let _ = state.apply(&fix);
        }
    }
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

    /// S5-M4：存档操作命令枚举同样是「收口单一出口」——未登记动作一律拒绝。
    #[test]
    fn save_command_deserializes_and_rejects_unknown() {
        assert_eq!(
            serde_json::from_str::<SaveCommand>("\"reset\"").expect("reset 应可解析"),
            SaveCommand::Reset
        );
        assert_eq!(
            serde_json::from_str::<SaveCommand>("\"import\"").expect("import 应可解析"),
            SaveCommand::Import
        );
        assert!(serde_json::from_str::<SaveCommand>("\"wipe\"").is_err(), "未登记存档命令必须拒绝");
    }

    /// S5-M4：设置补丁只接受**已定义设置项**；未知字段（拼写错误）被忽略而非报错，
    /// 且不会因为「拼错一个键」而整包失败（与配置加载同容错口径，R19）。
    #[test]
    fn settings_patch_parses_partial_and_ignores_unknown_fields() {
        let patch: crate::bridge::SettingsPatch = serde_json::from_str(
            "{\"scalePercent\":150,\"muted\":true,\"scalePercnet\":999,\"unknown\":1}",
        )
        .expect("脏字段应被忽略而非报错");
        assert_eq!(patch.scale_percent, Some(150));
        assert_eq!(patch.muted, Some(true));
        assert!(patch.opacity_percent.is_none());
        assert_eq!(patch.groups(), vec!["appearance", "audio"]);
    }

    /// S5-M4：空补丁是 no-op（不产生分组、不触发落盘）。
    #[test]
    fn empty_settings_patch_is_noop() {
        let patch = crate::bridge::SettingsPatch::default();
        assert!(patch.is_empty());
        assert!(patch.groups().is_empty());
    }

    /// S5-M4：`pet://config` 只广播摘要；分组名必须与 `settings.json` 顶层键同字面量。
    #[test]
    fn settings_patch_groups_use_config_section_names() {
        let known = ["pet", "appearance", "audio", "behavior", "interaction", "reminders"];
        let patch: crate::bridge::SettingsPatch = serde_json::from_str(
            "{\"name\":\"x\",\"opacityPercent\":80,\"masterVolumePercent\":10,\"muted\":false,\
              \"autoRoam\":false,\"clickThrough\":true,\"autostart\":true,\
              \"doNotDisturb\":true,\"alwaysOnTopPolicy\":\"Never\",\
              \"sensitivityValue\":1.3,\"catchphraseEnabled\":false,\
              \"catchphraseFrequency\":\"low\",\"clickFeedbackEnabled\":false,\
              \"activitySensing\":false,\"reminders\":{\"waterIntervalMin\":60}}",
        )
        .expect("完整补丁应可解析");
        let groups = patch.groups();
        assert_eq!(
            groups,
            vec!["pet", "appearance", "audio", "behavior", "interaction", "reminders"]
        );
        for group in groups {
            assert!(known.contains(&group), "未知分组名：{group}");
        }
    }
}
