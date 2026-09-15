//! `dp-app`：Tauri 应用层库 crate。
//!
//! 本文件是**装配点**而非实现处：S1-M2 起把 `tauri.conf.json` 声明的 `pet` 窗口
//! 接上 `dp_platform` 的 `PlatformWindow`（透明 / 置顶三态 / 全屏 / 穿透）。
//!
//! 增量扩展顺序（保持本文件为装配点）：
//!   1. `commands` / `bridge`：IPC 出口与事件桥接；
//!   2. `coreloop`：启动 `core-loop` 执行体（`std::thread`，唯一状态写者）；
//!   3. `tray_menu`（S1-M3）/ `supervisor`（S1-M4）：**已接线**——托盘在 `setup` 主线程
//!      装配（FR-1-10 / FR-11-12），监督线程在 `attach_pet_window` 成功后后台启动
//!      （30s 自检 / 全屏轮询 / 显示变更恢复）。
//!
//! 事件出口：`pet://tray`（S1-M3）、`pet://window/selfcheck`（S1-M4，`02 §7.6`）、
//! `pet://frame`（S2-M2 动作播放器，`bridge.rs`）。

// S1-M3 / S1-M4 模块（`pub`：供主理人 / QA 从外部引用做联通性核查）。
// 注：两文件各自以 `#![cfg(windows)]` 自门禁，外层 `#[cfg(windows)]` 为二次保险。
#[cfg(windows)]
pub mod supervisor;
#[cfg(windows)]
pub mod tray_menu;

// S2-M1：`pet://` 事件桥接（RenderFrameCmd v1 + 帧冒烟发射器 + atlas_png 命令）。
// 桥接层不依赖 Win32 / PetPlatform，无需 cfg 门禁。
pub mod bridge;

// S3-M1：钩子事件投递口（std 有界队列 `ChannelSink`）。零新增依赖；引用 `dp-platform/win/*`，
// 故以 `#[cfg(windows)]` 门控（文件内另有 `#![cfg(windows)]` 自门禁）。
#[cfg(windows)]
pub mod hook_sink;

// S3-M2：HIT_LATEST 像素命中数据源与双实现（FrameCursor / MaskStore / MaskHitSource /
// HitLatestHandle / HitFeed）。核心判定、构建器与 `HitFeed` 跨平台可测；仅 `HitTest`
// 覆写实现 windows 门控（文件内 `#[cfg(windows)]`），故模块本身不加 cfg。
pub mod hit_latest;

// S3-M3：手势消费端（钩子队列 drain → InputEvent → 手势状态机 → 意图日志+计数）。
// 引用 `dp-platform/win/*`，`#[cfg(windows)]` 门控（文件内另有自门禁）。
#[cfg(windows)]
pub mod interaction_consumer;

// S3-M0：core-loop 运行时装配层（三档 tick + 五引擎装配 + 端口注入 + 感知接线）。
// 文件以 `#![cfg(windows)]` 自门禁（引用 `PetPlatform` 与 `dp-platform/win/*`）。
#[cfg(windows)]
pub mod coreloop;
// S3-M0：装配层适配器（`MonitorGeom` / `PlatformBand` / `PetBBoxHandle` 端口注入；
// 纯逻辑换算跨平台，Windows 换算局部门控）。
pub mod ports;

// S3-M6：前端 invoke 命令统一收口（menu_command；模块不加整体 cfg——
// `generate_handler!` 注册需无条件编译，Windows 专属实现下沉到函数级 cfg 分支）。
pub mod commands;

/// 启动 Tauri 运行时。
///
/// 窗口集合（`pet` / `settings`）来自 `tauri.conf.json` 的 `app.windows`；
/// `run()` 在 `setup` 阶段把 `pet` 窗口挂到平台窗口层（`ensure_styles` → 置顶）。
///
/// ⚠️ `generate_context!` 的配置路径必须显式给出：该宏默认取
/// `$CARGO_MANIFEST_DIR/tauri.conf.json`（即 `crates/dp-app/tauri.conf.json`），
/// 而本工作区的配置位于 `src-tauri/` 根。相对路径按 `CARGO_MANIFEST_DIR` 解析，
/// 故 `../../tauri.conf.json` 稳定指向 `src-tauri/tauri.conf.json`。
pub fn run() {
    let builder = tauri::Builder::default()
        // S5-M4（R17）：**单实例锁**——二次启动唤醒已运行实例，不产生第二份存档写入。
        // 必须是**第一个**注册的插件（插件文档要求：单实例检查需先于其它插件初始化）；
        // 唤醒动作 = 恢复宠物窗口可见 + 置顶（用户最可能"找不到"宠物）。
        // C9：本机命名互斥体 / 窗口消息实现，零网络（登记见根 `Cargo.toml`）。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            eprintln!("[dp-app] 检测到二次启动：唤醒已运行实例（单实例锁，R17）");
            if let Err(err) = crate::tray_menu::set_pet_visible(app, true) {
                eprintln!("[dp-app] 唤醒已运行实例失败（降级忽略）：{err}");
            }
        }))
        // S2-M1：最小命令出口——前端经 invoke 读取图集 PNG 字节
        // （自定义 command，不走 asset 协议网络面，C9）。
        // S3-M6：menu_command 统一收口（右键菜单「隐藏」等命令的唯一出口）。
        // S4-M3/M4：`bridge::atlas_png`（图集字节）、`commands::pet_emotion_command`
        //（设置页「重置情绪」/「找回」兜底）。
        // S5-M4：设置读取 / 热更新 / 重置、存档健康状态与导入（设置页「数据」Tab）。
        .invoke_handler(tauri::generate_handler![
            bridge::atlas_png,
            commands::menu_command,
            commands::pet_emotion_command,
            commands::settings_get,
            commands::settings_apply,
            commands::settings_reset_all,
            commands::save_status,
            commands::save_command,
            // S6-M2：前端帧绘制回执（渲染看门狗计数源，`02 §5 K-8`）。
            bridge::frame_receipt
        ]);

    // 仅 Windows 落平台窗口层（本项目仅 Windows 目标）。
    #[cfg(windows)]
    let builder = builder.setup(|app| {
        setup_platform_layer(app);
        Ok(())
    });

    builder
        .run(tauri::generate_context!("../../tauri.conf.json"))
        .expect("启动 DesktopPet 运行时失败");
}

/// pet 窗口的平台封装（供后续模块取用：S1-M3 托盘切换置顶/穿透、S1-M4 30s 自检与全屏轮询）。
#[cfg(windows)]
pub struct PetPlatform {
    /// pet 窗口的平台窗口句柄封装。
    pub window: dp_platform::WinPlatformWindow,
    /// 平台入口（持有 `DisplayService`，供坐标换算 / 显示器变更后 `refresh`）。
    pub platform: dp_platform::WinPlatform,
}

/// 宠物窗口的**基准逻辑尺寸**（`Q-B`：逻辑 128×128，导出 2x 256×256）。
///
/// `01 FR-7-2` 的缩放百分比即相对本基准：`logical = BASE × scalePercent / 100`。
/// 常量放在装配层（`lib.rs`）：它是「窗口尺寸」这一平台概念的单一真源，`dp-core` 的
/// 运动引擎只处理 VDC 坐标、不关心窗口像素尺寸（`02 §7.2` 坐标系约定）。
pub const BRIDGE_BASE_LOGICAL_SIZE: u32 = 128;

/// 宠物窗口标签（`tauri.conf.json` `app.windows[0].label`；S6-M2 自愈重建查询用）。
pub(crate) const PET_WINDOW_LABEL: &str = "pet";

/// 解析音效资源目录（`<resource_dir>/resources/assets/audio` 优先，dev 兜底工程根
/// 相对路径；**C1：无盘符字面量**，与 [`bridge::resolve_atlas_dir`] 同候选链范式）。
///
/// 找不到时返回工程根候选路径（不 panic）：`dp-audio` 播放失败只记日志降级
/// （`02 §7.4.2`），不会阻断启动。
#[cfg(windows)]
fn resolve_audio_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    use std::path::Path;
    use tauri::Manager;

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(root) = app.path().resource_dir() {
        candidates.push(root.join("resources").join("assets").join("audio"));
        candidates.push(root.join("assets").join("audio"));
    }
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("assets")
            .join("audio"),
    );
    candidates
        .iter()
        .find(|c| c.join("emotion_hum.ogg").is_file())
        .cloned()
        .unwrap_or_else(|| candidates.last().cloned().unwrap_or_default())
}

/// 在 `setup` 阶段装配平台窗口层。
///
/// 遵循 `02 §7.4.2`「平台调用失败一律降级不崩溃」：失败只打印告警，保证应用仍能启动。
#[cfg(windows)]
fn setup_platform_layer(app: &mut tauri::App) {
    if let Err(err) = attach_pet_window(app) {
        eprintln!("[dp-app] 平台窗口层装配降级：{err}");
    }
}

#[cfg(windows)]
fn attach_pet_window(app: &mut tauri::App) -> Result<(), String> {
    use std::sync::Arc;

    use dp_platform::traits::TopmostMode;
    use dp_platform::{PlatformWindow, WinPlatform};
    use tauri::Manager;

    let pet = app
        .get_webview_window("pet")
        .ok_or_else(|| "未找到 pet 窗口（请检查 tauri.conf.json 的 app.windows）".to_string())?;

    // 取原生句柄：Tauri 的 `hwnd()` 返回 windows crate 的 `HWND`，取其指针值即可。
    let hwnd = pet.hwnd().map_err(|e| format!("取 pet 窗口句柄失败：{e}"))?.0 as isize;

    let platform = WinPlatform::new();
    let window = platform.attach(hwnd).map_err(|e| e.to_string())?;

    // 穿透双写：注册 Tauri 侧 `set_ignore_cursor_events` 回调（SG-M1 定版口径）。
    let pet_for_hook = pet.clone();
    window.set_cursor_events_hook(Arc::new(move |on: bool| {
        if let Err(e) = pet_for_hook.set_ignore_cursor_events(on) {
            eprintln!("[dp-app] set_ignore_cursor_events({on}) 失败：{e}");
        }
    }));

    // 创建参数落地：补齐扩展样式位（含 WS_EX_LAYERED 分层语义）+ topmost + 任务栏二次保险。
    window.ensure_styles().map_err(|e| e.to_string())?;
    window
        .set_topmost(TopmostMode::Always)
        .map_err(|e| e.to_string())?;

    // 先注册平台对象：下述托盘装配与监督线程均经 `app.state::<PetPlatform>()` / 跨线程
    // `try_state::<PetPlatform>()` 取用，故顺序不可反（必须先 manage）。
    app.manage(PetPlatform { window, platform });

    // S3-M0：core-loop 运行时装配层（三档 tick + 五引擎装配 + 端口注入）。
    // 感知总线（单生产者语义：感知线程 offer、core-loop drain）与 bbox 句柄先注册，
    // 供 core-loop 与 S3-M2 句柄后续取用；`SystemWallClock` 注入点在 core-loop 内首建。
    let bus =
        Arc::new(dp_core::perception::PerceptionBus::<dp_core::perception::PerceptionEvent>::new());
    let bbox = ports::PetBBoxHandle::new();
    app.manage(bbox.clone());

    // S3-M2：HIT_LATEST 组合句柄（粗筛复用 PetBBoxHandle 同一原子槽 + 帧掩码真三态）。
    // 先 manage 再装配 core-loop / 播放器 / 钩子（三者经 app 状态取用，顺序不可反）。
    let hit_latest = hit_latest::HitLatestHandle::new(bbox.clone());
    app.manage(hit_latest.clone());

    // S5-M4：渲染帧不透明度句柄（`01 FR-7-6`）——帧播放器每帧读、设置热更新每改即写。
    // 必须在 `bridge::spawn_frame_player` 之前 manage（播放器启动时取用）。
    app.manage(bridge::FrameAlpha::default());

    // S6-M2：帧绘制回执看门狗（`02 §5 K-8`）——播放器每帧 sent、前端 `frame_receipt`
    // 每回执 acked；supervisor 每 5s 采样判渲染自愈。**必须先 manage 再 spawn 播放器**
    // （播放器启动时取用句柄；未注册 → 播放器照常发布，仅自愈不触发）。
    app.manage(bridge::FrameWatchdog::default());

    // S5-M4：存档健康状态句柄——由 `coreloop::build_state` 载档时写入，
    // 设置页「数据」Tab 经 `save_status` 命令读取（不新增 `pet://` 事件，见 bridge 文档）。
    app.manage(bridge::SaveStatusHandle::default());

    // S4 前清障 B15-④：播放指令通道（core-loop 发播侧 ⇄ 播放器线程消费侧共享；
    // 须先 manage 再装配 core-loop 与播放器，两侧 try_state 取同一对象）。
    app.manage(bridge::PlaybackChannel::default());

    // S4-M4：core-loop 入站指令通道（设置页「重置情绪」/ 托盘「把心月狐找回来」→
    // core-loop 逻辑档；与 PlaybackChannel 同为进程内反向通道，非 C8 事件面）。
    app.manage(bridge::CoreInputChannel::default());

    // S4-M6：音效总线（audio 线程：rodio 播放 + 门控/队列上限 8）。
    // 资源目录复用与配置目录同源的候选链（C1 无盘符字面量）；设置快照由 core-loop
    // 装配期从 `settings.json` 同步一次（`coreloop::build_state`），热更新归 S5-M4。
    // 无音频设备时 `dp-audio` 内部降级静默后端（不阻断启动，`02 §7.4.2`）。
    let audio = dp_audio::AudioBus::spawn(
        dp_audio::AudioSettings::default(),
        resolve_audio_dir(app.handle()),
    );
    app.manage(audio);

    // S3-M2：掩码库后台一次性构建（`dp-mask-build` 线程读图集+PNG → MaskStore；
    // 失败降级 bbox 回退，不阻断启动）。图集目录复用 bridge 同一候选链（C1）。
    match bridge::resolve_atlas_dir(app.handle()) {
        Some(dir) => {
            hit_latest::spawn_mask_build(dir, hit_latest.store_slot());
        }
        None => {
            eprintln!("[dp-app] hit_latest 未发现图集目录，掩码库不构建（命中保持 bbox 回退）");
        }
    }

    // S3-M1/M3：钩子事件队列（Arc 双持：HookService 投递侧 + core-loop 手势消费 drain 侧）。
    let sink = Arc::new(hook_sink::ChannelSink::new(hook_sink::DEFAULT_HOOK_QUEUE_CAPACITY));

    // S3-M0：core-loop 装配（hit_latest 做 render 档 bbox 写者；手势消费端在 spawn 内构造）。
    // 口径同 `supervisor::spawn`（故意不标 #[must_use]），句柄以下划线具名承接避免未用告警。
    let _coreloop_handle =
        coreloop::spawn(app.handle().clone(), bus, hit_latest.clone(), sink.clone());

    // S3-M1：全局低阶鼠标钩子（`WH_MOUSE_LL`）装配 + 穿透兜底卸载。
    // - sink：std 有界队列（非阻塞、零分配、零新增依赖；S3-M3 起与 core-loop 共享）；
    // - hit：注入 `HitLatestHandle`（S3-M2 覆写 `HitTest::outcome` 三态；粗筛真源仍 ports.rs，
    //   `PetBBoxHandle` 实现不覆写 outcome，作为二态回退保留）；
    // - 初始两 gate 取窗口当前态（通常均 false → 构造即装钩子）；
    // - 经 `set_click_through_observer` 与既有穿透双写同挂点：任何人切穿透 → 钩子随之装/卸；
    // - `app.manage(hook_svc)` 供 supervisor 全屏降载取用（`Arc<HookService>`）。
    {
        // 内层作用域收口 `app.state::<PetPlatform>()` 的借用，`app.manage` 前释放（避免重叠借用）。
        let hook_svc = {
            let win = app.state::<PetPlatform>();
            let svc = Arc::new(dp_platform::win::hook::HookService::new(
                Arc::new(hit_latest.clone()),
                sink.clone(),
                win.window.is_click_through(),
                win.window.is_hidden_for_fullscreen(),
            ));
            let svc_for_click_through = Arc::clone(&svc);
            win.window.set_click_through_observer(Arc::new(move |on: bool| {
                svc_for_click_through.set_click_through(on);
            }));
            svc
        };
        app.manage(hook_svc);
    }

    // S1-M3：托盘装配（FR-1-10 / FR-11-12）。此刻在 `setup` 主线程内（`install` 要求）；
    // 遵循 `02 §7.4.2`：失败只降级告警，不阻断启动。
    if let Err(err) = tray_menu::install(app.handle()) {
        eprintln!("[dp-app] 托盘装配降级：{err}");
    }

    // S1-M4：启动监督线程（30s 自检 / 全屏轮询 / 显示变更恢复）。后台运行、不 join；
    // `_handle` 以下划线开头具名承接，避免 `-D warnings` 下的 unused 告警。
    let _handle = supervisor::spawn(app.handle().clone());

    // S5-M4：把设置快照（配置 ⊕ 存档 B 段）的**应用层效果**落地一次（窗口缩放/透明度、
    // 置顶、穿透、音量、自启），保证重启后 UI 记忆的设置真的生效（FR-7「持久化」）。
    // 装配顺序：`coreloop::spawn` 已 manage `SettingsState`，故此处必然可读。
    commands::apply_startup_settings(app.handle());

    // S2-M2：动作播放器（`dp-core::anim` 目录 + 轮播 + fps 档位 + 镜像规则；
    // 替换 S2-M1 冒烟发射器）。仅当图集可用时启动；后台运行、不 join
    // （spawn 有意不标 #[must_use]，见 bridge.rs）。
    bridge::spawn_frame_player(app.handle().clone());

    Ok(())
}
