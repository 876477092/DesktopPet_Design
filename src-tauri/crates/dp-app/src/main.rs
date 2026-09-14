//! `DesktopPet` 二进制入口（薄入口）。
//!
//! 本文件**只启动 Tauri 运行时**，不做任何窗口操控、不承载业务逻辑：
//! 两个窗口（`pet` / `settings`）由 `src-tauri/tauri.conf.json` 的 `app.windows`
//! 声明生成；pet 窗口的透明 / 置顶 / 全屏能力由 `dp_app::run()` 在 `setup`
//! 阶段经 `dp-platform` 的 `PlatformWindow` 落地（S1-M2）。
//!
//! 后续扩展点（按 `02 §3` 文件清单增量扩展，勿整体推翻本文件）：
//!   - `commands.rs`  ：`invoke` 命令出口（renderer / settings → core）
//!   - `bridge.rs`    ：`pet://` 事件桥接（core → 前端，`02 §7.6`）
//!   - `coreloop.rs`  ：`core-loop` 执行体（`02 §1.4`）
//!   - `tray_menu.rs` ：托盘菜单（`02 §5 K-1`，S1-M3）
//!   - `supervisor.rs`：异常自愈与优雅退出（S1-M4）
//!
//! 硬约束提醒：
//!   - C1 禁止盘符字面量；C3 一切时间读取必须经 `WallClock` 端口（本文件无时间逻辑）；
//!   - C9 零对外连接——`capabilities/default.json` 权限全关，且不得引入网络依赖。

// Release 下隐藏控制台窗口（Windows GUI 应用）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 运行时启动失败即终止：骨架阶段刻意不做降级，避免掩盖配置错误。
    dp_app::run();
}
