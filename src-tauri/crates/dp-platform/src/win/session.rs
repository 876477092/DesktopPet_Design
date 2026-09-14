//! `dp-platform::win::session`：Windows 会话状态检测（锁屏 / 远程桌面 / 活跃）。
//!
//! ## 归属与职责
//!
//! S4-M1 卡面「关键实现要点 3：锁屏/远程会话暂停累积」的**平台侧能力**（台账行 1534
//! P1-2 裁定「前置能力『锁屏/会话检测』」）。本模块只回答一个问题：
//! **当前会话是否应暂停情绪 P 累积**（锁屏 / 远程桌面会话 / 控制台已断开）。
//!
//! ## C9 合规（零网络 + feature 登记）
//!
//! 本模块为**新增能力**，按 C9 口径在 `src-tauri/Cargo.toml` 根清单登记后增补
//! `Win32_System_RemoteDesktop`（`WTSRegisterSessionNotification` /
//! `WTSQuerySessionInformationW`）。该 feature 只调用本机 `wtsapi32.dll`，
//! **不引入任何网络能力**，CSP / capabilities 不变。
//!
//! ## 降级路径（P1-2 允许）
//!
//! 真机受限时（例如非交互式会话、`wtsapi32` 不可用），本模块的
//! [`SessionWatcher::is_paused`] 退化为**基于 `GetLastInputInfo` 的 idle 近似**
//! （见 [`SessionWatcher::paused_by_idle_fallback`]），并按 C9 口径登记偏离。
//! 该退化不改变上层接口：`dp-app` 只需读 `is_paused()`。
//!
//! ## 线程模型
//!
//! `WTSRegisterSessionNotification` 需要接收 `WM_WTSSESSION_CHANGE` 的窗口句柄。
//! 为避免与宠物窗口耦合，本模块**不注册窗口消息**，而是采用**轮询查询**
//! （`WTSQuerySessionInformationW(WTSConnectState)`），由 `dp-app` 的系统感知档
//! （0.1Hz，`02 §1.4`）驱动，无新增线程、无消息泵、无 `hMod` 依赖。

#![cfg(windows)]

use windows::core::PWSTR;
use windows::Win32::System::RemoteDesktop::{
    WTSFreeMemory, WTSQuerySessionInformationW, WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION,
    WTS_INFO_CLASS,
};

/// 会话状态（对应 `WTS_CONNECTSTATE_CLASS` 的关键子集）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// 活跃（`WTSActive` / `WTSConnected`）：用户正在使用本机控制台。
    Active,
    /// 已连接但非活跃（`WTSDisconnected`）：典型 = 锁屏 / 远程桌面接管 / 切换用户。
    Disconnected,
    /// 其他（`WTSListen` / `WTSShadow` / `WTSIdle` / `WTSReset` / `WTSDown` / `WTSInit`）。
    Other,
    /// 查询失败（API 不可用 / 权限不足）。
    Unknown,
}

impl SessionState {
    /// 是否应暂停情绪 P 累积（F10 / FR-1-10）。
    ///
    /// `Unknown` **不暂停**（保守：宁可照常累积，也不因检测失败而永久冻结情绪）。
    #[inline]
    pub fn should_pause(self) -> bool {
        matches!(self, Self::Disconnected)
    }
}

/// 查询当前会话状态（本机 `WTS_CURRENT_SESSION`）。
///
/// 失败时返回 [`SessionState::Unknown`]，不 panic（R19：平台失败降级不崩）。
pub fn query_session_state() -> SessionState {
    let mut buf = PWSTR::null();
    let mut bytes: u32 = 0;
    // SAFETY: 按 Win32 API 契约传入有效出参指针；成功后 buf 由 WTSFreeMemory 释放。
    let ok = unsafe {
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            WTS_CURRENT_SESSION,
            WTS_INFO_CLASS(0), // WTSConnectState == 0（`WTS_INFO_CLASS` 首项）
            &mut buf,
            &mut bytes,
        )
    };
    if ok.is_err() || buf.is_null() || bytes < std::mem::size_of::<u32>() as u32 {
        return SessionState::Unknown;
    }
    // SAFETY: WTSConnectState 返回值为 WTS_CONNECTSTATE_CLASS（u32 枚举）单值。
    let raw = unsafe { *buf.0.cast::<u32>() };
    // SAFETY: buf 由系统分配，必须用 WTSFreeMemory 释放。
    unsafe { WTSFreeMemory(buf.0.cast()) };
    classify(raw)
}

/// `WTS_CONNECTSTATE_CLASS` 原始值 → [`SessionState`]。
///
/// 显式比对数值而非依赖 `windows` crate 的常量名，避免版本间命名差异。
/// 值域参考 Win32 `WTS_CONNECTSTATE_CLASS`：
/// 0=WTSActive / 1=WTSConnected / 2=WTSConnectQuery / 3=WTSShadow /
/// 4=WTSDisconnected / 5=WTSIdle / 6=WTSListen / 7=WTSReset / 8=WTSDown / 9=WTSInit。
fn classify(raw: u32) -> SessionState {
    match raw {
        0 | 1 => SessionState::Active,
        4 => SessionState::Disconnected,
        2 | 3 | 5 | 6 | 7 | 8 | 9 => SessionState::Other,
        _ => SessionState::Unknown,
    }
}

/// 会话状态观察器：轮询 + 迟滞，输出「是否暂停」的稳定判定。
///
/// 迟滞的必要性：锁屏瞬间/解锁瞬间的会话状态可能瞬时抖动（`WTSActive` ↔
/// `WTSDisconnected`），直接透传会让 `EmotionEngine` 的暂停窗口反复开关。
#[derive(Debug, Clone)]
pub struct SessionWatcher {
    /// 已确认的暂停态。
    paused: bool,
    /// 待确认的新状态起始单调时刻（毫秒）。
    candidate_since_ms: Option<u64>,
    /// 迟滞时长（毫秒）。
    hysteresis_ms: u64,
    /// 是否允许 idle 近似降级。
    idle_fallback_enabled: bool,
    /// idle 近似阈值（毫秒）。
    idle_fallback_threshold_ms: u64,
}

impl Default for SessionWatcher {
    fn default() -> Self {
        Self::new(1_500, true, 30 * 60 * 1000)
    }
}

impl SessionWatcher {
    /// 构造。
    ///
    /// - `hysteresis_ms`：状态切换的确认迟滞；
    /// - `idle_fallback_enabled`：平台查询失败时是否退化为 idle 近似；
    /// - `idle_fallback_threshold_ms`：idle 近似的空闲阈值（默认 30min，
    ///   与 `offline.graceMin` 对齐——单纯不碰键鼠 30min 不必然是锁屏，
    ///   故该降级**只在平台查询失败时启用**）。
    pub fn new(
        hysteresis_ms: u64,
        idle_fallback_enabled: bool,
        idle_fallback_threshold_ms: u64,
    ) -> Self {
        Self {
            paused: false,
            candidate_since_ms: None,
            hysteresis_ms,
            idle_fallback_enabled,
            idle_fallback_threshold_ms,
        }
    }

    /// 只读：当前是否判定为暂停（供 1Hz 业务档读取）。
    #[inline]
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// 只读：最近一次采样的原始会话状态（观测 / 日志用）。
    #[inline]
    pub fn hysteresis_ms(&self) -> u64 {
        self.hysteresis_ms
    }

    /// 推进一次采样。
    ///
    /// `now_ms` / `idle_ms` 由调用方注入（C3：本模块不自取时间；
    /// `now_ms` 用单调毫秒即可，判据只涉及差值）。返回本次是否发生暂停态迁移。
    pub fn observe(&mut self, now_ms: u64, idle_ms: Option<u64>) -> bool {
        let raw = query_session_state();
        let want = if raw.should_pause() {
            true
        } else if raw == SessionState::Unknown {
            self.paused_by_idle_fallback(idle_ms)
        } else {
            false
        };
        self.apply(want, now_ms)
    }

    /// 纯函数形式推进（供单测注入状态，不触碰平台 API）。
    pub fn observe_state(&mut self, state: SessionState, now_ms: u64, idle_ms: Option<u64>) -> bool {
        let want = if state.should_pause() {
            true
        } else if state == SessionState::Unknown {
            self.paused_by_idle_fallback(idle_ms)
        } else {
            false
        };
        self.apply(want, now_ms)
    }

    /// 平台查询不可用时的 idle 近似（P1-2 允许的降级路径）。
    fn paused_by_idle_fallback(&self, idle_ms: Option<u64>) -> bool {
        if !self.idle_fallback_enabled {
            return false;
        }
        matches!(idle_ms, Some(v) if v >= self.idle_fallback_threshold_ms)
    }

    /// 迟滞状态机：目标态变化需持续 `hysteresis_ms` 才被采纳。
    fn apply(&mut self, want: bool, now_ms: u64) -> bool {
        if want == self.paused {
            self.candidate_since_ms = None;
            return false;
        }
        let since = *self.candidate_since_ms.get_or_insert(now_ms);
        if now_ms.saturating_sub(since) >= self.hysteresis_ms {
            self.paused = want;
            self.candidate_since_ms = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_should_pause() {
        assert!(SessionState::Disconnected.should_pause());
    }

    #[test]
    fn active_and_other_and_unknown_should_not_pause() {
        // Unknown 保守不暂停：避免检测失败导致情绪永久冻结
        assert!(!SessionState::Active.should_pause());
        assert!(!SessionState::Other.should_pause());
        assert!(!SessionState::Unknown.should_pause());
    }

    #[test]
    fn classify_maps_connect_state_values() {
        assert_eq!(classify(0), SessionState::Active); // WTSActive
        assert_eq!(classify(1), SessionState::Active); // WTSConnected
        assert_eq!(classify(4), SessionState::Disconnected); // WTSDisconnected
        assert_eq!(classify(5), SessionState::Other); // WTSIdle
        assert_eq!(classify(6), SessionState::Other); // WTSListen
        assert_eq!(classify(99), SessionState::Unknown);
    }

    #[test]
    fn watcher_requires_hysteresis_before_pausing() {
        let mut w = SessionWatcher::new(1_500, false, 0);
        assert!(!w.is_paused());
        // 立刻切到 Disconnected：迟滞未满 → 不采纳
        assert!(!w.observe_state(SessionState::Disconnected, 0, None));
        assert!(!w.is_paused());
        assert!(!w.observe_state(SessionState::Disconnected, 1_000, None));
        assert!(!w.is_paused());
        // 跨过 1500ms → 采纳
        assert!(w.observe_state(SessionState::Disconnected, 1_500, None));
        assert!(w.is_paused());
    }

    #[test]
    fn watcher_drops_candidate_when_target_flaps_back() {
        let mut w = SessionWatcher::new(1_500, false, 0);
        w.observe_state(SessionState::Disconnected, 0, None);
        assert!(!w.is_paused());
        // 抖动回 Active → 候选清零，不采纳
        assert!(!w.observe_state(SessionState::Active, 800, None));
        assert!(!w.is_paused());
        // 再次 Disconnected 需重新起计 1500ms
        assert!(!w.observe_state(SessionState::Disconnected, 900, None));
        assert!(!w.observe_state(SessionState::Disconnected, 2_000, None));
        assert!(!w.is_paused(), "应重新起计而非沿用旧起点");
        assert!(w.observe_state(SessionState::Disconnected, 2_400, None));
        assert!(w.is_paused());
    }

    #[test]
    fn watcher_unpause_also_needs_hysteresis() {
        let mut w = SessionWatcher::new(1_000, false, 0);
        w.observe_state(SessionState::Disconnected, 0, None);
        w.observe_state(SessionState::Disconnected, 1_000, None);
        assert!(w.is_paused());
        // 解锁：先不采纳
        assert!(!w.observe_state(SessionState::Active, 2_000, None));
        assert!(w.is_paused());
        assert!(w.observe_state(SessionState::Active, 3_000, None));
        assert!(!w.is_paused());
    }

    #[test]
    fn watcher_is_idempotent_when_state_is_stable() {
        let mut w = SessionWatcher::new(1_000, false, 0);
        w.observe_state(SessionState::Disconnected, 0, None);
        w.observe_state(SessionState::Disconnected, 1_000, None);
        assert!(w.is_paused());
        // 稳定暂停中反复采样 → 无迁移
        for t in 2_000..10_000 {
            assert!(!w.observe_state(SessionState::Disconnected, t, None));
        }
        assert!(w.is_paused());
    }

    #[test]
    fn idle_fallback_only_applies_on_unknown_state() {
        let mut w = SessionWatcher::new(0, true, 30 * 60 * 1000);
        // Active + 超长 idle（用户只是没动键鼠）→ 不暂停
        assert!(!w.observe_state(SessionState::Active, 0, Some(u64::MAX)));
        assert!(!w.is_paused());
        // Unknown + 超长 idle → 降级判暂停
        assert!(w.observe_state(SessionState::Unknown, 100, Some(40 * 60 * 1000)));
        assert!(w.is_paused());
    }

    #[test]
    fn idle_fallback_can_be_disabled() {
        let mut w = SessionWatcher::new(0, false, 0);
        assert!(!w.observe_state(SessionState::Unknown, 0, Some(u64::MAX)));
        assert!(!w.is_paused());
    }

    #[test]
    fn unknown_without_idle_info_does_not_pause() {
        let mut w = SessionWatcher::new(0, true, 1000);
        assert!(!w.observe_state(SessionState::Unknown, 0, None));
        assert!(!w.is_paused());
    }

    #[test]
    fn query_never_panics_on_this_machine() {
        // 真机（或沙箱）下调用必须返回确定值、不 panic（R19）
        let s = query_session_state();
        assert!(matches!(
            s,
            SessionState::Active | SessionState::Disconnected | SessionState::Other | SessionState::Unknown
        ));
    }
}
