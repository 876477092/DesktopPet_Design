//! `dp-app` 窗口自检与显示变更监督线程（S1-M4，T-02 段 · 下）。
//!
//! 职责（**仅此三项**，其余一律不在此模块）：
//!   1. **30s 自检**（`02 §5 K-1`「30s 自检重设」，风险 R1）：每 30 tick（≈30s）调用
//!      [`dp_platform::WinPlatformWindow::self_check`] 补回扩展样式位 / 置顶位，并把结果经
//!      `pet://window/selfcheck` 广播；失败计数累加入日志。
//!   2. **显示变更恢复**（FR-1-4 拔屏兜底）：每 tick `DisplayService::refresh()` 取显示器快照，
//!      与上一次比较（[`topology_changed`]）；有变化即 `self_check()` + `ensure_on_screen()`，
//!      保证拔屏后窗口 ≤2s 内迁到剩余屏。
//!   3. **全屏轮询调度**：每 tick 调 [`dp_platform::WinPlatformWindow::poll_fullscreen`]，
//!      驱动 `BelowFullscreen` 三态的隐藏 / 恢复（`02 §5 K-1`）。
//!
//! ⚠️ **范围边界**：渲染异常自愈（5s 帧回执看门狗）与优雅退出属**后续模块**，
//! **不在本模块范围**。
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
//!
//! ## 跨模块硬约束
//! 事件 `pet://window/selfcheck`（`{ok, missing_ex_style, tick}`）为 S1-M4 新增，已登记于
//! `02 §7.6`。载荷用元组 `(bool, u32, u64)` 直出，**不新增 `serde` 直接依赖**
//! （避免改动 `Cargo.toml`）。

#![cfg(windows)]

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
        }

        // 绝对锚定：睡到本 tick 的 deadline（消除过冲累加漂移）；越过 deadline 则直接进入下一轮。
        sleep_until_tick(start, tick);
    }
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
}
