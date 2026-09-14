//! `emotion::sys_env`：情绪内核向平台层取「环境快照」的**端口**（S4-M1 新增）。
//!
//! ## 为什么需要这个端口
//!
//! S4-M1 的暂停累积需要两件事：**会话是否暂停**（锁屏 / 远程桌面 / 全屏前台）与
//! **用户是否在场**。两者都来自平台层（Win32 会话通知 / `GetLastInputInfo`），
//! 而 `dp-core` 按 C3 纪律**零时钟、零平台 API** —— 所以这里只定义**纯数据端口**：
//! `dp-core` 声明需要什么，`dp-app` 负责 Assembly 并注入（`02 §1.4` 单写者模型）。
//!
//! ## C9 合规
//!
//! 本端口**不新增任何 `windows` feature**、不引入第三方 crate、不产生任何网络能力。
//! 平台侧实现（`WTSRegisterSessionNotification` + `GetLastInputInfo`）由 `dp-app`
//! 既有能力组装；若平台能力在真机受限，可退化为本端口返回 `paused = false` 的
//! 空实现 [`NoSysEnv`]（台账 P1-2 允许的「降级 + 登记偏离」路径）。

use crate::emotion::TickEnv;

/// 情绪内核的环境来源端口（1Hz 调用；实现方须保证**无阻塞、无分配**）。
///
/// 热路径契约：`snapshot_env` 每业务 tick 调一次，实现应只做原子读 / 已缓存值拷贝。
pub trait SysEnv: Send {
    /// 组装本 tick 的环境快照。
    ///
    /// `now_local` 由调用方传入（来自 `WallClock::now_local()`），端口不自行取时（C3）。
    fn snapshot_env(&self, now_local: chrono::DateTime<chrono::Local>) -> TickEnv<'static>;
}

/// 空实现：会话永不暂停、用户视为在场（`presence_idle_ms = 0`）。
///
/// 用途：① 单测；② 平台会话检测能力不可用时的降级路径
/// （台账 P1-2 允许，但**必须在 `03 §3.2` 登记偏离**）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSysEnv;

impl SysEnv for NoSysEnv {
    fn snapshot_env(&self, now_local: chrono::DateTime<chrono::Local>) -> TickEnv<'static> {
        TickEnv { now_local, session_paused: false, preset_idle_ms: 0, ..TickEnv::default() }
    }
}

/// 轻量可配置实现：供 `dp-app` 在生产侧直接填充，也供测试构造场景。
///
/// 字段语义与 [`TickEnv`] 对应；`sat_here` = 用户在场（`false` → 走 `presence.factorAway`）。
#[derive(Debug, Default, Clone, Copy)]
pub struct StaticSysEnv {
    /// 会话暂停（锁屏 / 远程桌面 / 全屏前台）。
    pub session_paused: bool,
    /// 用户空闲毫秒（`GetLastInputInfo` 口径；`u64::MAX` 表示不可用）。
    pub preset_idle_ms: u64,
}

impl StaticSysEnv {
    /// 构造。
    pub fn new(session_paused: bool, preset_idle_ms: u64) -> Self {
        Self { session_paused, preset_idle_ms }
    }
}

impl SysEnv for StaticSysEnv {
    fn snapshot_env(&self, now_local: chrono::DateTime<chrono::Local>) -> TickEnv<'static> {
        TickEnv {
            now_local,
            session_paused: self.session_paused,
            preset_idle_ms: self.preset_idle_ms,
            ..TickEnv::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};

    fn t0() -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 14, 10, 0, 0).unwrap()
    }

    #[test]
    fn no_sys_env_is_never_paused() {
        let env = NoSysEnv.snapshot_env(t0());
        assert!(!env.session_paused);
        assert_eq!(env.preset_idle_ms, 0);
        assert_eq!(env.now_local, t0());
    }

    #[test]
    fn static_env_echoes_state() {
        let env = StaticSysEnv::new(true, 999_000).snapshot_env(t0());
        assert!(env.session_paused);
        assert_eq!(env.preset_idle_ms, 999_000);
    }
}
