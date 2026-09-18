//! `dp-app` 窗口自检与显示变更监督线程（S1-M4，T-02 段 · 下；**S6-M1/M2 扩充**）。
//!
//! 职责（仅此五项，其余一律不在此模块）：
//!   1. **30s 自检**（`02 §5 K-1`「30s 自检重设」，风险 R1）：每 30 tick（≈30s）调用
//!      [`dp_platform::WinPlatformWindow::self_check`] 补回扩展样式位 / 置顶位，并把结果经
//!      `pet://window/selfcheck` 广播；失败计数累加入日志。
//!   2. **显示变更恢复**（FR-1-4 拔屏兜底）：每 tick `DisplayService::refresh()` 取显示器快照，
//!      与上一次比较（[`topology_changed`]）；有变化即 `self_check()` + `ensure_on_screen()`，
//!      保证拔屏后窗口 ≤2s 内迁到剩余屏。
//!   3. **全屏轮询调度**：每 tick 调 [`dp_platform::WinPlatformWindow::poll_fullscreen`]，
//!      驱动 `BelowFullscreen` 三态的隐藏 / 恢复（`02 §5 K-1`）。
//!   4. **性能度量与降级**（S6-M1，T-16 段 · 上）：每 5 tick（≈5s）采样
//!      内存（`GetProcessMemoryInfo`）/ CPU（`GetProcessTimes` 差分）/ 帧率
//!      （`FrameWatchdog` 发布计数差分）→ [`dp_core::metrics::DegradeController`] 决策
//!      （K-8：>200MB 卸插槽纹理、>225MB 切 FrameRenderer+LRU 32MB、全屏/隐身/电池降帧）
//!      → 应用档位（`FrameTierHandle`）→ 发射 `pet://perf`（`02 §7.6`：`{fps,cpu,mem,level}`，5s）。
//!   5. **渲染自愈与存档主动提示**（S6-M2，T-16 段 · 下）：每 5 tick 采样
//!      「最近 5s 是否有帧回执」（`FrameWatchdog` 计数差分），连续 3 次无回执 →
//!      ① 阻塞落盘（`CoreInput::FlushSave` + 3s 回执超时）；② 重建宠物 WebView 渲染器
//!      （`reload()`，WebView2 渲染器重建，不重建进程、不丢状态）；③ 连续 3 次自愈失败 →
//!      托盘气泡 + `app.restart()`。另：启动后按 `SaveStatusHandle.needs_notice`
//!      托盘气泡提示存档异常（AC-14「损坏档并提示」收口）。
//!
//! ⚠️ **本文件自门禁 Windows 目标**：内部引用 `crate::PetPlatform`（`#[cfg(windows)]`），
//! 故以 `#![cfg(windows)]` 自门禁；装配方（`lib.rs`，阶段 2）只需 `pub mod supervisor;`
//! 即可，无需再套 `#[cfg]`。
//!
//! ## C3 时间口径裁定（单调钟）
//! 本模块的时间读取**一律**用 [`std::time::Instant`]（**单调钟**）：
//! `now_ms = start.elapsed().as_millis()`。**严禁** `SystemTime` / `Utc::now()` 之类裸读墙钟——
//! 墙钟会被 NTP 校时 / 用户改表 / DST 拨动，导致迟滞状态机的 `now_ms` 回跳或跳跃。
//! `Instant` 单调不减，满足全屏迟滞（`RESTORE_DELAY_MS`）与 tick 计数的稳定推进。
//! 降级策略的滞回同样**只用 tick 计数**（`dp_core::metrics` 纯逻辑，零时钟）。
//!
//! ## 跨模块硬约束
//! 事件 `pet://window/selfcheck`（`{ok, missing_ex_style, tick}`）为 S1-M4 新增，已登记于
//! `02 §7.6`。载荷用元组 `(bool, u32, u64)` 直出，**不新增 `serde` 直接依赖**
//! （避免改动 `Cargo.toml`）。`pet://perf`（S6-M1）事件名早已登记 `02 §7.6`，
//! 载荷为 `dp_core::metrics::PerfWire`（序列化后 emit），同样零新增依赖。

#![cfg(windows)]

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dp_core::config::DegradeCfg;
use dp_core::metrics::{DegradeController, DegradeEnv};
use dp_platform::{MonitorInfo, WatchAction};
use tauri::{AppHandle, Emitter, Manager};

/// 监督线程轮询周期（毫秒）——**绝对锚定网格的步长**。
///
/// 取 **1000ms** ⇒ 满足 AC-11「退出全屏 3s 内恢复」的「① 轮询间隔 ≤1s」方案
/// （收口 `03 §3.3 B5`，推演见 `gate/s1-m4/engineer-selfcheck.md`）。
///
/// 每轮按「第 k 个 tick 的 deadline = `start + k × 本常量`」绝对锚定休眠
/// （见 [`sleep_until_tick`]），**不随单次 `sleep` 过冲累加漂移**。
pub const TICK_INTERVAL_MS: u64 = 1_000;

/// 自检间隔（tick 数）：每 30 tick ≈ 30s 一次（`02 §5 K-1`）。
pub const SELF_CHECK_EVERY_TICKS: u64 = 30;

/// 自检事件名（`02 §7.6`）。
pub const SELFCHECK_EVENT: &str = "pet://window/selfcheck";

/// S6-M1/M2：性能采样与看门狗采样间隔（tick 数）——每 5 tick ≈ 5s
/// （`02 §7.6` `pet://perf` 频率；K-8 看门狗采样窗同为 5s）。
pub const PERF_EVERY_TICKS: u64 = 5;

/// S6-M2：渲染看门狗判定阈值——连续 3 次（每次 5s）有帧发但无回执 → 自愈（K-8）。
pub const WATCHDOG_MISS_LIMIT: u32 = 3;

/// S6-M2：自愈失败阈值——连续 3 次自愈失败 → 重启进程 + 托盘提示（K-8）。
pub const HEAL_FAIL_LIMIT: u32 = 3;

/// S6-M2：阻塞落盘等待超时（core-loop 收到 `FlushSave` 后立即落盘，3s 极宽裕；
/// 超时仅防死等，不阻断自愈）。
const FLUSH_TIMEOUT: Duration = Duration::from_secs(3);

/// 显示器拓扑是否发生变化（纯函数，单测覆盖）。
///
/// 比较口径（**集合语义**，与列表顺序无关）：两张快照的显示器 `id` 集合一致，
/// 且每个 `id` 对应的 `rc_monitor` / `rc_work` / `primary` 均相等 → `false`（无变化）；
/// 否则 `true`（增 / 删 / 改）。
#[must_use]
pub fn topology_changed(prev: &[MonitorInfo], now: &[MonitorInfo]) -> bool {
    // 数量不同必有增删（id 唯一，量相等才逐项比对）。
    if prev.len() != now.len() {
        return true;
    }
    prev.iter().any(|p| match now.iter().find(|n| n.id == p.id) {
        None => true,
        Some(n) => {
            p.rc_monitor != n.rc_monitor || p.rc_work != n.rc_work || p.primary != n.primary
        }
    })
}

/// 启动监督线程（`dp-supervisor`；由 `dp-app` 装配点在 `setup` 阶段调用一次）。
///
/// 返回线程句柄，供优雅退出模块（后续）决定是否 join；本模块自身不 join、不阻塞主线程。
///
/// 注：**故意不标 `#[must_use]`**，以便装配点直接 `supervisor::spawn(app.handle().clone());`
/// 丢弃句柄时不会触发 `unused_must_use`（在 `-D warnings` 下会致构建失败）。
pub fn spawn(app: AppHandle) -> JoinHandle<()> {
    let builder = std::thread::Builder::new().name("dp-supervisor".to_string());
    // 不变量：线程启动失败（资源耗尽等）不得 panic 主线程 → 降级为一个「空 JoinHandle」
    // （派发一个立即结束的线程返回句柄），交由上层日志感知。
    match builder.spawn(move || run_loop(app)) {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("[dp-app] supervisor 线程启动失败，降级为空监督：{err}");
            std::thread::spawn(|| {})
        }
    }
}

/// 监督循环体（在 `dp-supervisor` 线程内运行）。
///
/// **绝对时间锚定**：第 k 个 tick 的目标（deadline）时刻 = `start + k × TICK_INTERVAL_MS`，
/// 每轮工作结束后睡到该 deadline。以**单调钟锚定固定网格**，可**消除固定 `sleep` 的单次
/// 过冲逐 tick 累加**——Windows 默认定时器粒度约 15.6ms，`sleep(1000)` 实测约 1000+δ，
/// 固定循环会把 δ 累加进网格（tick 网格退化为 `k×(1000+δ)`），使 AC-11 恢复时延漂移为
/// `3000+3δ`（最坏 ≈3045ms）；锚定后仅残留**单次**调度抖动 ε ⇒ `R−E ≤ 3000ms + ε`。
fn run_loop(app: AppHandle) {
    // 时间来源：单调钟 `Instant`（C3「单调节拍可直读」口径，2026-09-12 复核裁定
    // 见 `dp-core/src/lib.rs` 文件头）。起点即线程启动时刻。
    let start = Instant::now();
    let mut prev_monitors: Vec<MonitorInfo> = Vec::new();
    let mut tick: u64 = 0;
    let mut ticks_since_self_check: u64 = 0;
    let mut fail_count: u64 = 0;

    // S6-M1：性能采样与降级状态（每 5 tick 收敛一次；CPU 采样器持上次差分点）。
    let mut perf_tick: u64 = 0;
    let mut cpu_sampler = dp_platform::ProcCpuSampler::new();
    let mut controller = DegradeController::new();
    let degrade_cfg = degrade_cfg(&app);

    // S6-M2：渲染看门狗状态（prev 计数 + 连续未回执次数 + 自愈失败次数）。
    let mut watchdog = WatchdogState::default();
    // AC-14 收口：存档异常「托盘气泡主动提示」只提示一次（启动后首次就绪时）。
    let mut save_notice_shown = false;

    loop {
        tick = tick.wrapping_add(1);
        let now_ms = start.elapsed().as_millis() as i64;

        // 平台层尚未装配（启动竞态）时本轮跳过工作，绝不 panic（`02 §7.4.2`）。
        // 注意：跳过分支同样走到循环末尾的绝对锚定休眠，保持网格不漂移。
        if let Some(state) = app.try_state::<crate::PetPlatform>() {
            let pet: &crate::PetPlatform = &state;

            // ① 刷新并比较显示器拓扑（refresh 失败用缓存降级）。
            let display = pet.platform.display();
            if let Err(err) = display.refresh() {
                eprintln!("[dp-app] supervisor 显示器 refresh 降级：{err}");
            }
            let monitors = display.monitors();

            // ② 拓扑变化 → 立即自检 + 迁移兜底（FR-1-4：拔屏 ≤2s 内迁移）+ 写日志。
            if topology_changed(&prev_monitors, &monitors) {
                eprintln!(
                    "[dp-app] supervisor 检测到显示变更（{} → {} 屏），执行自检与迁移兜底",
                    prev_monitors.len(),
                    monitors.len()
                );
                match pet.window.self_check() {
                    Ok(report) => eprintln!(
                        "[dp-app] supervisor 显示变更后自检：ok={} missing_ex_style={:#010X} topmost_ok={}",
                        report.ok, report.missing_ex_style, report.topmost_ok
                    ),
                    Err(err) => eprintln!("[dp-app] supervisor 显示变更后自检降级：{err}"),
                }
                match pet.window.ensure_on_screen(&monitors) {
                    Ok(Some(vdc)) => eprintln!(
                        "[dp-app] supervisor 拔插屏迁移：窗口已迁至 VDC ({:.1}, {:.1})",
                        vdc.x, vdc.y
                    ),
                    Ok(None) => {}
                    Err(err) => eprintln!("[dp-app] supervisor 迁移兜底降级：{err}"),
                }
            }
            prev_monitors = monitors;

            // ③ 全屏轮询（驱动 BelowFullscreen 三态；now_ms 为单调钟毫秒）。
            // S3-M1：进入全屏隐藏态 → 钩子**完全卸载**（降载，`02 §5 K-2`）；退出 → 重挂。
            // 时延随本 1s 网格 ≤1s（降载而非硬实时要求，设计补充 §5.2）。
            match pet.window.poll_fullscreen(now_ms) {
                Ok(action) => {
                    if let Some(svc) =
                        app.try_state::<std::sync::Arc<dp_platform::win::hook::HookService>>()
                    {
                        svc.set_fullscreen_hidden(matches!(action, WatchAction::Hide));
                    }
                }
                Err(err) => eprintln!("[dp-app] supervisor 全屏轮询降级：{err}"),
            }

            // ③' 钩子心跳接线：K-2「10s 心跳探测/丢失重挂」（2026-09-13，B13-②）。
            // 本网格 1s，密于 K-2 的 10s 要求；每 tick 先快照再补装，`ensure_installed`
            // 幂等且内部经 refresh() 守两 gate——穿透 / 全屏隐藏态为停用 no-op，绝不误装。
            if let Some(svc) =
                app.try_state::<std::sync::Arc<dp_platform::win::hook::HookService>>()
            {
                let was_installed = svc.is_installed();
                let installed = svc.ensure_installed();
                if !was_installed && installed {
                    eprintln!("[dp-app] supervisor 钩子丢失重挂成功（K-2 心跳）");
                }
            }

            // ④ 每 ≈30s：自检 + 事件广播 + 失败计数（`02 §5 K-1`）。
            ticks_since_self_check = ticks_since_self_check.saturating_add(1);
            if ticks_since_self_check >= SELF_CHECK_EVERY_TICKS {
                ticks_since_self_check = 0;
                periodic_self_check(pet, &app, tick, &mut fail_count);
            }

            // ④' S6-M2（AC-14 收口）：存档异常主动提示——core-loop 载档后写
            // `SaveStatusHandle`，`needs_notice=true`（损坏隔离 / 备份恢复 / 未来档）
            // 时托盘气泡提示一次（日志版已在 coreloop::log_save_outcome，S6-M2 补
            // 用户可见面）。状态就绪后置 `save_notice_shown`，不重复弹。
            if !save_notice_shown {
                if let Some(status) = app.try_state::<crate::bridge::SaveStatusHandle>() {
                    let info = status.get();
                    if info.state != "unknown" {
                        save_notice_shown = true;
                        if info.needs_notice {
                            show_save_notice(&app, &info.state);
                        }
                    }
                }
            }

            // ⑤ S6-M1/M2：性能度量 + 降级 + 发射 `pet://perf` + 渲染看门狗自愈
            // （每 5 tick ≈ 5s；K-8 采样窗）。
            perf_tick = perf_tick.wrapping_add(1);
            if perf_tick >= PERF_EVERY_TICKS {
                perf_tick = 0;
                perf_and_watchdog_tick(
                    pet, &app, &mut cpu_sampler, &mut controller, &degrade_cfg, &mut watchdog,
                );
            }
        }

        // 绝对锚定：睡到本 tick 的 deadline（消除过冲累加漂移）；越过 deadline 则直接进入下一轮。
        sleep_until_tick(start, tick);
    }
}

/// S6-M2：渲染看门狗状态机（计数差分采样，纯内存无时钟）。
#[derive(Debug, Default)]
struct WatchdogState {
    /// 上次采样的「已发布帧」计数（`FrameWatchdog` 全局计数差分用）。
    prev_sent: u64,
    /// 上次采样的「已回执帧」计数。
    prev_acked: u64,
    /// 连续「有帧发但无回执」的采样窗数。
    miss: u32,
    /// 连续自愈失败次数（回执恢复后清零）。
    heal_fail: u32,
}

/// 读取降级策略配置：`animation.json.degrade`（经与 bridge 同源的配置目录候选链），
/// 读取 / 解析失败降级为 `DegradeCfg::default()`（与 animation.json 同源，`02 §5 K-8`）。
fn degrade_cfg(app: &AppHandle) -> DegradeCfg {
    use std::path::Path;
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(root) = app.path().resource_dir() {
        candidates.push(root.join("resources").join("config"));
        candidates.push(root.join("config"));
    }
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("resources")
            .join("config"),
    );
    for dir in candidates {
        if !dir.join("actions.json").is_file() {
            continue;
        }
        return match dp_core::config::ConfigService::load_all(&dir) {
            Ok((bundle, _warnings)) => bundle.animation.degrade,
            Err(err) => {
                eprintln!("[dp-app] supervisor 读取 animation.json.degrade 降级为默认：{err}");
                DegradeCfg::default()
            }
        };
    }
    DegradeCfg::default()
}

/// S6-M1/M2：单个采样窗（≈5s）的度量 → 降级 → 发射 + 渲染看门狗判定。
fn perf_and_watchdog_tick(
    pet: &crate::PetPlatform,
    app: &AppHandle,
    cpu_sampler: &mut dp_platform::ProcCpuSampler,
    controller: &mut DegradeController,
    cfg: &DegradeCfg,
    watchdog: &mut WatchdogState,
) {
    // —— 度量（S6-M1；进程内存/CPU 采样失败降级为 0，不阻断）——
    let mem_mb = dp_platform::process_memory_bytes()
        .map(|b| b as f32 / (1024.0 * 1024.0))
        .unwrap_or(0.0);
    let cpu = cpu_sampler.sample().unwrap_or(0.0);
    let (sent, acked) = app
        .try_state::<crate::bridge::FrameWatchdog>()
        .map(|w| w.sample())
        .unwrap_or((0, 0));
    let sent_delta = sent.saturating_sub(watchdog.prev_sent);
    let acked_delta = acked.saturating_sub(watchdog.prev_acked);
    watchdog.prev_sent = sent;
    watchdog.prev_acked = acked;
    // 帧率 = 本窗发布帧数 / 窗长 5s（发射口径，`02 §7.6`：fps）。
    let fps = sent_delta as f32 / PERF_EVERY_TICKS as f32;

    // —— 降级（S6-M1；优先级 全屏/隐身 > 电池 > 内存，K-8）——
    let fullscreen = pet.window.is_hidden_for_fullscreen();
    let hidden = fullscreen || !pet_window_visible(app);
    let battery = dp_platform::battery_status()
        .map(|b| !b.charging)
        .unwrap_or(false);
    let env = DegradeEnv {
        mem_mb,
        fullscreen,
        hidden,
        battery,
    };
    let decision = controller.decide(&env, cfg);
    if let Some(tier) = decision.tier_override {
        if let Some(tier_handle) = app.try_state::<crate::bridge::FrameTierHandle>() {
            let current = tier_handle.get();
            if current != tier {
                eprintln!(
                    "[dp-app] supervisor 降帧切档：{:?} → {:?}（level={:?}）",
                    current, tier, decision.level
                );
                tier_handle.set(tier);
            }
        }
    }
    // 动作只记决策（`UnloadSlotTextures`/`PhysicsPrimaryOnly`/`FrameRendererLru` 的
    // **执行**归 S9 渲染后端；S6-M1 卡边界：不改渲染后端）。
    if !decision.actions.is_empty() {
        eprintln!(
            "[dp-app] supervisor 降级动作决策（执行归 S9）：{:?}",
            decision.actions
        );
    }

    // —— 发射 `pet://perf`（`02 §7.6`：{fps,cpu,mem,level}，5s）——
    let sample = dp_core::metrics::PerfSample { fps, cpu, mem_mb };
    let wire = dp_core::metrics::PerfWire::from_sample(&sample, decision.level);
    if let Err(err) = app.emit(dp_core::event::EVENT_PERF, &wire) {
        eprintln!(
            "[dp-app] supervisor 广播 {} 降级：{err}",
            dp_core::event::EVENT_PERF
        );
    }

    // —— 渲染看门狗（S6-M2，K-8：连续 3 次 5s 窗「有帧发无回执」→ 自愈）——
    match watchdog_step(sent_delta, acked_delta, watchdog) {
        WatchdogStep::None => {}
        WatchdogStep::Heal => self_heal_renderer(app),
        WatchdogStep::Restart => {
            eprintln!(
                "[dp-app] supervisor 连续 {HEAL_FAIL_LIMIT} 次自愈失败：重启进程 + 托盘提示"
            );
            show_tray_balloon(
                app,
                "桌面宠物渲染异常",
                "连续多次自动恢复失败，正在重启桌面宠物…",
            );
            app.restart();
        }
    }
}

/// S6-M2：渲染看门狗状态机（纯函数，单测覆盖）。
///
/// 输入本采样窗（≈5s）的帧发布 / 回执差分与旧状态，输出新状态与动作：
/// - 本窗无帧发（`sent_delta == 0`）→ 播放器未推帧（无图集/暂停），**不是渲染异常**，
///   清空连续 miss 并观察；
/// - 本窗有回执 → 渲染健康；若此前在自愈则计一次成功（`heal_fail` 清零）；
/// - 本窗有帧发但无回执 → `miss + 1`；连续 [`WATCHDOG_MISS_LIMIT`] 次 →
///   触发自愈（`Heal`，`heal_fail + 1`）；连续 [`HEAL_FAIL_LIMIT`] 次自愈失败 →
///   `Restart`（重启进程 + 托盘提示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchdogStep {
    /// 观察 / 正常（不动作）。
    None,
    /// 触发一次渲染自愈（K-8 步骤①+②）。
    Heal,
    /// 连续自愈失败达阈值：重启进程 + 托盘提示。
    Restart,
}

#[must_use]
fn watchdog_step(sent_delta: u64, acked_delta: u64, state: &mut WatchdogState) -> WatchdogStep {
    if sent_delta == 0 {
        // 播放器未推帧（无图集 / 暂停中）→ 不是渲染异常，重置 miss 不触发。
        state.miss = 0;
        return WatchdogStep::None;
    }
    if acked_delta > 0 {
        // 回执恢复：若此前在自愈，记一次成功并清零失败计数。
        if state.heal_fail > 0 {
            eprintln!("[dp-app] supervisor 渲染回执恢复（自愈成功）");
        }
        state.miss = 0;
        state.heal_fail = 0;
        return WatchdogStep::None;
    }
    state.miss = state.miss.saturating_add(1);
    if state.miss < WATCHDOG_MISS_LIMIT {
        return WatchdogStep::None;
    }
    state.miss = 0;
    state.heal_fail = state.heal_fail.saturating_add(1);
    eprintln!(
        "[dp-app] supervisor 渲染看门狗：连续 {WATCHDOG_MISS_LIMIT} 窗无帧回执 \
         （sent_delta={sent_delta} acked_delta={acked_delta}），执行自愈 #{}",
        state.heal_fail
    );
    if state.heal_fail >= HEAL_FAIL_LIMIT {
        WatchdogStep::Restart
    } else {
        WatchdogStep::Heal
    }
}

/// 宠物窗口当前是否可见（Tauri 查询；失败降级为「可见」，不误触隐身降帧）。
fn pet_window_visible(app: &AppHandle) -> bool {
    app.get_webview_window(crate::PET_WINDOW_LABEL)
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(true)
}

/// S6-M2：渲染自愈（K-8 步骤①+②）。
///
/// ① 阻塞落盘：`CoreInputChannel` 投递 `FlushSave`，等回执 ≤3s——保证「重建渲染前
/// 存档已落盘」，渲染异常期间**不丢档**（`02 §5 K-8`）。
/// ② 重建宠物 WebView：以 `webview.reload()` 重建 WebView2 渲染器（官方机制：导航即
/// 重建渲染器进程，渲染进程已死时自动拉起）。**不重建应用进程、不丢状态**（存档在
/// Rust 侧，前端纯展示）——稳定 Tauri 2.11 无 `add_child` / `WebviewBuilder::build`
/// （unstable / crate 私有），全窗口重建需 ExitRequested 守卫 + unsafe 状态置换，
/// 风险/收益失衡，故采用渲染器级重建（满足 AC「渲染线程已死 → 自动重启渲染且存档不丢」）。
fn self_heal_renderer(app: &AppHandle) {
    // ① 阻塞落盘。
    if let Some(channel) = app.try_state::<crate::bridge::CoreInputChannel>() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        channel.push(crate::bridge::CoreInput::FlushSave { ack: tx });
        match rx.recv_timeout(FLUSH_TIMEOUT) {
            Ok(()) => eprintln!("[dp-app] supervisor 自愈前存档已落盘（回执）"),
            Err(_) => eprintln!(
                "[dp-app] supervisor 自愈前落盘回执超时（{FLUSH_TIMEOUT:?}），继续自愈"
            ),
        }
    } else {
        eprintln!("[dp-app] supervisor 自愈：CoreInputChannel 未注册，跳过落盘");
    }

    // ② 重建渲染器。
    match app.get_webview_window(crate::PET_WINDOW_LABEL) {
        Some(webview_window) => match webview_window.reload() {
            Ok(()) => eprintln!("[dp-app] supervisor 已重建宠物 WebView 渲染器（reload）"),
            Err(err) => eprintln!("[dp-app] supervisor 重建宠物 WebView 失败：{err}"),
        },
        None => eprintln!("[dp-app] supervisor 自愈：宠物窗口不存在，跳过重建"),
    }
}

/// 托盘气泡（S6-M2）：宿主窗口取 pet HWND；气泡不可用时降级为日志（`02 §7.4.2`）。
fn show_tray_balloon(app: &AppHandle, title: &str, body: &str) {
    let hwnd = app
        .try_state::<crate::PetPlatform>()
        .map(|pet| pet.window.hwnd())
        .unwrap_or(0);
    if hwnd == 0 || !dp_platform::win::tray_balloon::show_balloon(hwnd, title, body) {
        eprintln!("[dp-app] supervisor 托盘气泡不可用（降级为日志）：{title} {body}");
    }
}

/// S6-M2（AC-14 收口）：存档异常主动提示——`needs_notice` 状态经托盘气泡告知用户
/// （损坏档已隔离重建 / 备份恢复 / 未来版本档），随后可到设置页「数据」Tab 回捞原档。
fn show_save_notice(app: &AppHandle, state: &str) {
    let body = match state {
        "isolatedCorrupt" => "检测到存档损坏，已隔离并重建默认档；原档可在设置「数据」页回捞",
        "recoveredFromBak" => "存档主档异常，已从备份自动恢复",
        "isolatedFuture" => "检测到更高版本存档，已隔离并重建默认档；原档可在设置「数据」页查看",
        _ => "存档状态异常，请到设置「数据」页查看详情",
    };
    show_tray_balloon(app, "桌面宠物存档提示", body);
}

/// 第 `tick` 个 tick 相对 `start` 的 deadline 偏移（纯函数，单测覆盖）。
///
/// 网格间隔恒为 [`TICK_INTERVAL_MS`]；用 `saturating_mul` 防 `tick` 极大时溢出。
#[inline]
#[must_use]
fn tick_offset(tick: u64) -> Duration {
    Duration::from_millis(tick.saturating_mul(TICK_INTERVAL_MS))
}

/// 睡到第 `tick` 个 tick 的**绝对锚定** deadline（`start + tick_offset(tick)`）。
///
/// 若本轮工作耗时已越过 deadline → `checked_duration_since` 返回 `None`，
/// 则**不补偿、不忙等**（禁止 `while Instant::now() < deadline {}` 空转占 CPU），
/// 直接返回进入下一轮（退化为尽力节拍，绝不阻塞后续 tick 的锚定）。
fn sleep_until_tick(start: Instant, tick: u64) {
    let deadline = start + tick_offset(tick);
    if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        std::thread::sleep(remaining);
    }
}

/// 执行一次周期性自检，并广播 `pet://window/selfcheck` 事件。
///
/// 失败（`ok == false` 或 `Err`）累加 `fail_count` 并写日志（`02 §5 K-1`：失败计数入日志）。
/// 广播失败仅降级为日志，不影响监督循环。
fn periodic_self_check(
    pet: &crate::PetPlatform,
    app: &AppHandle,
    tick: u64,
    fail_count: &mut u64,
) {
    match pet.window.self_check() {
        Ok(report) => {
            if !report.ok {
                *fail_count += 1;
                eprintln!(
                    "[dp-app] supervisor 周期自检未通过（累计 {fail_count} 次）：missing_ex_style={:#010X} topmost_ok={}",
                    report.missing_ex_style, report.topmost_ok
                );
            }
            // 广播：载荷为元组 (ok, missing_ex_style, tick)，不新增 serde 直接依赖。
            if let Err(err) = app.emit(
                SELFCHECK_EVENT,
                (report.ok, report.missing_ex_style, tick),
            ) {
                eprintln!("[dp-app] supervisor 广播 {SELFCHECK_EVENT} 降级：{err}");
            }
        }
        Err(err) => {
            *fail_count += 1;
            eprintln!("[dp-app] supervisor 周期自检降级（累计 {fail_count} 次）：{err}");
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试（纯逻辑，不依赖真机窗口）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dp_platform::{MonitorId, RectI, Vec2};

    /// 构造测试用显示器描述（`primary` 约定 id==1 为主屏）。
    fn mk(
        id: u64,
        ox: f32,
        oy: f32,
        scale: f32,
        rc_monitor: RectI,
        rc_work: RectI,
    ) -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(id),
            origin_vdc: Vec2::new(ox, oy),
            scale,
            rc_monitor,
            rc_work,
            device_id: format!("DISPLAY{id}"),
            primary: id == 1,
        }
    }

    #[test]
    fn topology_changed_detects_add_remove_modify_and_same() {
        let a = mk(
            1,
            0.0,
            0.0,
            1.0,
            RectI::new(0, 0, 1920, 1080),
            RectI::new(0, 0, 1920, 1040),
        );
        let b = mk(
            2,
            -1920.0,
            0.0,
            1.0,
            RectI::new(-1920, 0, 0, 1080),
            RectI::new(-1920, 0, 0, 1040),
        );

        // 同：完全一致 / 顺序无关（集合语义）
        assert!(!topology_changed(
            std::slice::from_ref(&a),
            std::slice::from_ref(&a)
        ));
        assert!(!topology_changed(
            &[a.clone(), b.clone()],
            &[b.clone(), a.clone()]
        ));
        // 双空 → 同
        assert!(!topology_changed(&[], &[]));

        // 增：多一台
        assert!(topology_changed(
            std::slice::from_ref(&a),
            &[a.clone(), b.clone()]
        ));
        // 删：少一台（拔屏）
        assert!(topology_changed(
            &[a.clone(), b.clone()],
            std::slice::from_ref(&a)
        ));

        // 改 rc_work（任务栏变化）
        let a_work = mk(
            1,
            0.0,
            0.0,
            1.0,
            RectI::new(0, 0, 1920, 1080),
            RectI::new(0, 0, 1920, 1000),
        );
        assert!(topology_changed(
            std::slice::from_ref(&a),
            std::slice::from_ref(&a_work)
        ));

        // 改 rc_monitor（分辨率 / 缩放变化）
        let a_res = mk(
            1,
            0.0,
            0.0,
            1.25,
            RectI::new(0, 0, 2560, 1440),
            RectI::new(0, 0, 2560, 1400),
        );
        assert!(topology_changed(
            std::slice::from_ref(&a),
            std::slice::from_ref(&a_res)
        ));

        // 改 primary（主屏迁移）
        let mut a_not_primary = a.clone();
        a_not_primary.primary = false;
        assert!(topology_changed(
            std::slice::from_ref(&a),
            std::slice::from_ref(&a_not_primary)
        ));
    }

    #[test]
    fn monotonic_now_ms_is_non_negative_and_non_decreasing() {
        // 单调钟语义：elapsed 非负且不回跳（C3）。
        let start = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
        let a = start.elapsed().as_millis() as i64;
        let b = start.elapsed().as_millis() as i64;
        assert!(a > 0, "sleep 2ms 后 elapsed 应 > 0：a={a}");
        assert!(b >= a, "单调钟不应回跳：a={a} b={b}");
    }

    #[test]
    fn tick_offset_builds_drift_free_absolute_grid() {
        // 绝对锚定网格：第 k 个 deadline 偏移恒为 k×TICK_INTERVAL_MS，
        // 不随任何单次 sleep 过冲累加（这是消除累加漂移的核心不变量）。
        assert_eq!(TICK_INTERVAL_MS, 1_000);
        assert_eq!(tick_offset(0), Duration::ZERO);
        assert_eq!(tick_offset(1), Duration::from_millis(1_000));
        assert_eq!(tick_offset(30), Duration::from_millis(30_000));

        let mut prev = tick_offset(0);
        for k in 1..=600u64 {
            let cur = tick_offset(k);
            assert!(cur > prev, "deadline 偏移应严格递增：k={k}");
            assert_eq!(
                cur - prev,
                Duration::from_millis(TICK_INTERVAL_MS),
                "相邻 tick 间隔应恒为 TICK_INTERVAL_MS（无累加漂移）：k={k}"
            );
            prev = cur;
        }

        // 极大 tick 不 panic（saturating_mul 防溢出）。
        assert!(tick_offset(u64::MAX) == Duration::from_millis(u64::MAX));
    }

    // -----------------------------------------------------------------------
    // S6-M1/M2：常量与看门狗状态机（纯逻辑）
    // -----------------------------------------------------------------------

    #[test]
    fn s6_constants_match_k8_spec() {
        // K-8：5s 采样窗 / 连续 3 次无回执自愈 / 连续 3 次自愈失败重启。
        assert_eq!(PERF_EVERY_TICKS, 5);
        assert_eq!(WATCHDOG_MISS_LIMIT, 3);
        assert_eq!(HEAL_FAIL_LIMIT, 3);
    }

    #[test]
    fn watchdog_healthy_flow_never_heals() {
        let mut s = WatchdogState::default();
        // 有帧发且有回执 → 连续观察，无动作。
        for _ in 0..10 {
            assert_eq!(watchdog_step(5, 5, &mut s), WatchdogStep::None);
        }
        assert_eq!(s.miss, 0);
        assert_eq!(s.heal_fail, 0);
    }

    #[test]
    fn watchdog_no_publish_is_not_anomaly() {
        let mut s = WatchdogState { miss: 2, ..Default::default() };
        // 播放器未推帧（无图集/暂停）：sent_delta == 0 → 永远不触发，且清空累积 miss。
        for _ in 0..10 {
            assert_eq!(watchdog_step(0, 0, &mut s), WatchdogStep::None);
        }
        assert_eq!(s.miss, 0, "无推帧应重置 miss，不判渲染异常");
        assert_eq!(s.heal_fail, 0);
    }

    #[test]
    fn watchdog_three_miss_windows_trigger_heal() {
        let mut s = WatchdogState::default();
        // 连续 2 窗「有帧发无回执」→ 观察；第 3 窗 → Heal。
        assert_eq!(watchdog_step(5, 0, &mut s), WatchdogStep::None);
        assert_eq!(s.miss, 1);
        assert_eq!(watchdog_step(5, 0, &mut s), WatchdogStep::None);
        assert_eq!(s.miss, 2);
        assert_eq!(watchdog_step(5, 0, &mut s), WatchdogStep::Heal);
        assert_eq!(s.miss, 0, "触发自愈后 miss 重置");
        assert_eq!(s.heal_fail, 1);
    }

    #[test]
    fn watchdog_receipt_resume_clears_fail_counter() {
        let mut s = WatchdogState { miss: 2, heal_fail: 1, ..Default::default() }; // 此前已自愈过一次
        // 回执恢复 → miss 与 heal_fail 全部清零（自愈成功）。
        assert_eq!(watchdog_step(5, 5, &mut s), WatchdogStep::None);
        assert_eq!(s.miss, 0);
        assert_eq!(s.heal_fail, 0);
    }

    #[test]
    fn watchdog_three_heal_failures_restart() {
        let mut s = WatchdogState::default();
        // 每次自愈 = 连续 3 个无回执窗；3 次自愈失败 → Restart（第 9 个无回执窗）。
        for heal_idx in 1..=2u32 {
            assert_eq!(watchdog_step(5, 0, &mut s), WatchdogStep::None);
            assert_eq!(watchdog_step(5, 0, &mut s), WatchdogStep::None);
            assert_eq!(
                watchdog_step(5, 0, &mut s),
                WatchdogStep::Heal,
                "第 {heal_idx} 次自愈应在第 3 个无回执窗触发"
            );
            assert_eq!(s.heal_fail, heal_idx);
            assert_eq!(s.miss, 0);
        }
        assert_eq!(s.heal_fail, 2);
        assert_eq!(watchdog_step(5, 0, &mut s), WatchdogStep::None);
        assert_eq!(watchdog_step(5, 0, &mut s), WatchdogStep::None);
        assert_eq!(
            watchdog_step(5, 0, &mut s),
            WatchdogStep::Restart,
            "连续 3 次自愈失败应触发重启"
        );
        assert_eq!(s.heal_fail, 3);
        assert_eq!(s.miss, 0);
    }
}
