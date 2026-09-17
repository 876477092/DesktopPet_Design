//! 全局低阶键盘钩子（`WH_KEYBOARD_LL`）· **只计数**（S7-M1，T-20 / `02 §5.6`）。
//!
//! 依据：`02 §5.6`「击键强度 = `WH_KEYBOARD_LL` **只计数**（+`CallNextHookEx`，永不吞键），
//! 事件驱动（≈5/s），单次成本 ~2μs，CPU 增量 <0.01%」；`01 §6.8` FR-8-5 隐私声明；
//! `03 §2 S7-M1` 要点 3/4（一键关闭 → 纯时间模型；穿透模式键盘钩子卸载）。
//!
//! ## 与 [`super::hook`]（`WH_MOUSE_LL`）的关系
//!
//! 职责互斥、互不依赖：本模块只做「按键动作计数」，`hook.rs` 做「命中三态路由 + 吞事件」。
//! **隐私红线 ⑤**（`02 §5.6`）要求「穿透模式鼠标 + 键盘钩子一并卸载」，由**装配层**
//! （`dp-app`）对两个服务同时下 gate —— 本模块不反向依赖 `hook.rs`（保持职责单一，
//! 且两者线程 / 生命周期完全独立）。
//!
//! ## 隐私红线（`01 §6.8` FR-8-5 / `02 §5.6` 隐私设计 ①⑥）
//!
//! 回调内**不解析 `KBDLLHOOKSTRUCT`、不读 `vkCode`**——按键内容从不进入本进程内存；
//! 只对「按下」类消息做一次原子自增（[`KeyCounters`]）。日志层面同样只暴露计数，
//! 无任何键名 / 键值 / 文本。
//!
//! ## 线程模型（镜像 `hook.rs` 已验证结构）
//!
//! 专用「`dp-keyhook`」线程内：`GetCurrentThreadId` → `PeekMessageW(PM_NOREMOVE)`
//! **先建消息队列** → 写线程 TLS 上下文 → `SetWindowsHookExW(WH_KEYBOARD_LL, …, None, 0)`
//! → `GetMessageW` **常驻消息泵**（`WM_APP+2` → `PostQuitMessage` 退出）→ 泵退出后
//! `UnhookWindowsHookEx` + 清 TLS。卸载从任意线程经 `PostThreadMessageW` → `join`。
//! **常驻消息泵 + 回调 O(1)** 即「无 `LowLevelHooksTimeout` 卸载」的充要工程条件。
//!
//! 边界：不实装按键内容解析；不新增 `pet://` 事件（C8）；零网络（C9）；
//! `dp-platform` **零新增依赖 / 零新增 feature**（复用既有 `Win32_UI_WindowsAndMessaging`）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(windows)]
use std::sync::mpsc;
#[cfg(windows)]
use std::thread::JoinHandle;

/// 键击计数器（**只计数**：不保存键值 / 键序 / 文本）。
///
/// 原子量以支持「钩子线程自增 + 任意线程读取」；`Relaxed` 足够——计数器只用于
/// 速率估算（`02 §5.6`「≈5/s 事件驱动」），不与其它内存构成同步关系。
#[derive(Debug, Default)]
pub struct KeyCounters {
    /// 累计「按下」类事件数（单调不减）。
    strokes: AtomicU64,
}

impl KeyCounters {
    /// 空计数器。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 计数 +1（**仅由钩子回调调用**；热路径零分配、零锁）。
    #[inline]
    pub fn bump(&self) {
        let _ = self.strokes.fetch_add(1, Ordering::Relaxed);
    }

    /// 累计计数（供强度采样器取差分）。
    #[inline]
    #[must_use]
    pub fn total(&self) -> u64 {
        self.strokes.load(Ordering::Relaxed)
    }
}

/// 键盘钩子后端端口（抽象粒度 = 一次完整的装 / 卸动作）。
///
/// 与 [`super::hook::HookBackend`] 同构：生产实装 [`WinKeyHookBackend`] 内部起专用线程；
/// 测试用假后端计数。**两个方法均须幂等**（已在目标态时为 no-op）。
pub trait KeyHookBackend: Send + Sync {
    /// 安装低阶键盘钩子。返回是否安装成功（降级返回 `false`）。
    /// **幂等**：已安装时 no-op 并返回 `true`。
    fn install(&self) -> bool;
    /// 卸载低阶键盘钩子。**幂等**：未安装时 no-op。
    fn uninstall(&self);
    /// 当前是否处于**已安装**态。
    fn is_installed(&self) -> bool;
}

/// 键盘钩子服务（装 / 卸幂等状态机）。
///
/// 两个 gate 求值启停：`want = activity_sensing && !click_through`。
/// **仅状态转变时**才调后端（后端内部再作幂等守卫）。
/// - `activity_sensing = false`（`settings.privacy.activitySensing`）→ 卸载，
///   配合 `dp-app` 的 `presence` 恒在场口径，整体退化为**纯时间模型**（`02 §5.6` ④）；
/// - `click_through = true`（穿透模式）→ 卸载（`02 §5.6` ⑤）。
pub struct KeyHookService {
    backend: Arc<dyn KeyHookBackend>,
    counters: Arc<KeyCounters>,
    runtime: Mutex<KeyGate>,
}

/// 两个 gate 的当前取值。
#[derive(Debug, Clone, Copy)]
struct KeyGate {
    /// 活动感知开关（隐私）。
    activity_sensing: bool,
    /// 穿透态。
    click_through: bool,
}

impl KeyHookService {
    /// 生产构造（安装失败**降级为「未安装」+ 告警，不 panic**，`02 §7.4.2`）。
    #[cfg(windows)]
    #[must_use]
    pub fn new(activity_sensing: bool, click_through: bool) -> Self {
        let counters = Arc::new(KeyCounters::new());
        Self::with_backend(
            Arc::new(WinKeyHookBackend::new(Arc::clone(&counters))),
            counters,
            activity_sensing,
            click_through,
        )
    }

    /// 注入后端 / 计数器（测试 / 备选实装）。构造即 `refresh()`。
    #[must_use]
    pub fn with_backend(
        backend: Arc<dyn KeyHookBackend>,
        counters: Arc<KeyCounters>,
        activity_sensing: bool,
        click_through: bool,
    ) -> Self {
        let svc = Self {
            backend,
            counters,
            runtime: Mutex::new(KeyGate { activity_sensing, click_through }),
        };
        svc.refresh();
        svc
    }

    /// gate①：活动感知开关（`false` = 隐私关闭 → 卸载钩子）。
    pub fn set_activity_sensing(&self, on: bool) {
        if self.set_gate(|g| &mut g.activity_sensing, on) {
            self.refresh();
        }
    }

    /// gate②：穿透态（`true` = 穿透 → 卸载钩子）。
    pub fn set_click_through(&self, on: bool) {
        if self.set_gate(|g| &mut g.click_through, on) {
            self.refresh();
        }
    }

    /// 当前是否已安装。
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.backend.is_installed()
    }

    /// 活动感知开关当前取值（`false` = 隐私关闭：调用方应视为「无感知信息」并不再采样）。
    ///
    /// 供 `dp-app` 感知线程在采样前判定（避免「关了还在后台查前台进程」）与
    /// [`crate::traits::ActivitySensing`] 实装使用。
    #[must_use]
    pub fn is_activity_sensing(&self) -> bool {
        self.runtime.lock().unwrap_or_else(|e| e.into_inner()).activity_sensing
    }

    /// 键击计数器（共享句柄；**只含计数**）。
    #[must_use]
    pub fn counters(&self) -> &Arc<KeyCounters> {
        &self.counters
    }

    /// 应用退出：等价 `disable()`（幂等）。
    pub fn shutdown(&self) {
        self.disable();
    }

    /// 更新某 gate；返回是否**发生变化**。
    fn set_gate<F>(&self, pick: F, on: bool) -> bool
    where
        F: FnOnce(&mut KeyGate) -> &mut bool,
    {
        let mut g = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        let slot = pick(&mut g);
        if *slot == on {
            return false;
        }
        *slot = on;
        true
    }

    /// 按两 gate 求值期望态并启 / 停（幂等）。
    fn refresh(&self) {
        let want = {
            let g = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            g.activity_sensing && !g.click_through
        };
        if want {
            self.enable();
        } else {
            self.disable();
        }
    }

    /// 幂等启用：已装 → no-op。
    fn enable(&self) {
        if self.backend.is_installed() {
            return;
        }
        let _ = self.backend.install();
    }

    /// 幂等停用：未装 → no-op。
    fn disable(&self) {
        if !self.backend.is_installed() {
            return;
        }
        self.backend.uninstall();
    }
}

// ---------------------------------------------------------------------------
// Win32 生产实装
// ---------------------------------------------------------------------------

/// 钩子线程回报消息。
#[cfg(windows)]
enum KeyThreadMsg {
    /// 线程已建立消息队列，回报其线程 id。
    Tid(u32),
    /// 安装结果（`true` = 成功常驻；`false` = 降级退出）。
    Installed(bool),
}

/// 唤醒 / 停机消息泵的自定义消息（`WM_APP + 2`，与鼠标钩子 `WM_APP + 1` 区分）。
#[cfg(windows)]
const WM_APP_KEY_UNHOOK: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

/// 等待钩子线程回报 `tid` / 安装结果的超时（毫秒）。
#[cfg(windows)]
const KEY_HOOK_SIGNAL_TIMEOUT_MS: u64 = 2_000;

/// 回调热路径所用的线程 TLS 上下文（`WH_KEYBOARD_LL` 回调总在**安装线程**派发）。
///
/// 用 TLS 而非 `Mutex`：回调内 `borrow()` **零锁、零分配**。
/// 上下文**只含计数器**——无键值、无缓冲、无 sink。
#[cfg(windows)]
struct KeyCtx {
    counters: Arc<KeyCounters>,
}

#[cfg(windows)]
thread_local! {
    /// 当前线程的键盘钩子上下文（仅 `dp-keyhook` 安装线程写入，回调只读）。
    static KEY_CTX: std::cell::RefCell<Option<KeyCtx>> =
        const { std::cell::RefCell::new(None) };
}

/// 低阶键盘钩子回调（`HOOKPROC`）。
///
/// **热路径纪律**：负 `code` 直接放行；否则仅读 TLS → 对「按下」类消息原子自增
/// （`WM_KEYDOWN` / `WM_SYSKEYDOWN`）→ **恒 `CallNextHookEx` 放行，永不吞键**
/// （`02 §5.6`）。**不解析 `KBDLLHOOKSTRUCT`**（按键内容不进内存），
/// 不做 `Mutex` / 日志 / `format!` / IO / 分配。
///
/// # Safety
///
/// 由系统在安装线程上下文中调用；`lparam` 指向合法 `KBDLLHOOKSTRUCT`，本实现
/// **不解引用**它（只按 `wparam` 的消息码计数），故无别名 / 越界风险。
#[cfg(windows)]
unsafe extern "system" fn key_hook_proc(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, WM_KEYDOWN, WM_SYSKEYDOWN};

    if code >= 0 {
        let message = wparam.0 as u32;
        if message == WM_KEYDOWN || message == WM_SYSKEYDOWN {
            KEY_CTX.with(|cell| {
                if let Some(ctx) = cell.borrow().as_ref() {
                    ctx.counters.bump();
                }
            });
        }
    }
    // 永不吞键：无论计数与否一律放行（`02 §5.6`）。
    CallNextHookEx(None, code, wparam, lparam)
}

/// `dp-keyhook` 线程主体（与鼠标钩子线程同构：建队列 → 写 TLS → 装钩子 → 泵 → 卸）。
#[cfg(windows)]
fn key_hook_thread_main(counters: Arc<KeyCounters>, tx: mpsc::Sender<KeyThreadMsg>) {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, PeekMessageW, PostQuitMessage, SetWindowsHookExW,
        TranslateMessage, UnhookWindowsHookEx, MSG, PM_NOREMOVE, WH_KEYBOARD_LL,
    };

    // ① 线程 id（供调用方 PostThreadMessageW 唤醒 / 停机）。
    let thread_id = unsafe { GetCurrentThreadId() };
    let _ = tx.send(KeyThreadMsg::Tid(thread_id));

    // ② 先建线程消息队列（PostThreadMessageW 对无队列线程会失败）。
    let mut msg = MSG::default();
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };

    // ③ 写线程 TLS 上下文（回调只读）。
    KEY_CTX.with(|cell| *cell.borrow_mut() = Some(KeyCtx { counters }));

    // ④ 安装低阶键盘钩子（hMod = NULL：LL 钩子规范）。
    let hook = match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_hook_proc), None, 0) } {
        Ok(h) => h,
        Err(err) => {
            eprintln!("[dp-platform] SetWindowsHookExW(WH_KEYBOARD_LL) 失败，降级：{err}");
            KEY_CTX.with(|cell| *cell.borrow_mut() = None);
            let _ = tx.send(KeyThreadMsg::Installed(false));
            return;
        }
    };
    let _ = tx.send(KeyThreadMsg::Installed(true));

    // ⑤ 常驻消息泵。
    loop {
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 == 0 {
            break; // WM_QUIT
        }
        if ret.0 == -1 {
            eprintln!("[dp-platform] GetMessageW 取消息失败，键盘钩子线程降级退出");
            break;
        }
        if msg.message == WM_APP_KEY_UNHOOK {
            unsafe { PostQuitMessage(0) };
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // ⑥ 泵退出后再卸载 + 清 TLS（防与 in-flight 回调竞争）。
    let _ = unsafe { UnhookWindowsHookEx(hook) };
    KEY_CTX.with(|cell| *cell.borrow_mut() = None);
}

/// 后端运行期状态。
#[cfg(windows)]
struct KeyBackendRuntime {
    thread: Option<JoinHandle<()>>,
    thread_id: u32,
    installed: bool,
}

/// Win32 生产后端：内部持有 `dp-keyhook` 线程句柄与安装态。
#[cfg(windows)]
struct WinKeyHookBackend {
    counters: Arc<KeyCounters>,
    runtime: Mutex<KeyBackendRuntime>,
}

#[cfg(windows)]
impl WinKeyHookBackend {
    fn new(counters: Arc<KeyCounters>) -> Self {
        Self {
            counters,
            runtime: Mutex::new(KeyBackendRuntime {
                thread: None,
                thread_id: 0,
                installed: false,
            }),
        }
    }

    /// 停机一个（可能残留的）钩子线程：发自定义卸载消息 → `join`。
    fn stop_thread(thread: JoinHandle<()>, thread_id: u32) {
        if thread_id != 0 {
            // SAFETY: tid 来自本进程 dp-keyhook 线程，其消息队列已建好。
            let _ = unsafe {
                windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    thread_id,
                    WM_APP_KEY_UNHOOK,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                )
            };
        }
        let _ = thread.join();
    }
}

#[cfg(windows)]
impl KeyHookBackend for WinKeyHookBackend {
    fn install(&self) -> bool {
        let mut rt = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        if rt.installed {
            return true;
        }
        let (tx, rx) = mpsc::channel::<KeyThreadMsg>();
        let counters = Arc::clone(&self.counters);
        let builder = std::thread::Builder::new().name("dp-keyhook".to_string());
        let handle = match builder.spawn(move || key_hook_thread_main(counters, tx)) {
            Ok(h) => h,
            Err(err) => {
                eprintln!("[dp-platform] dp-keyhook 线程启动失败，降级：{err}");
                return false;
            }
        };

        // 等回报 tid（保证安装失败时也能正确停机）。
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(KEY_HOOK_SIGNAL_TIMEOUT_MS);
        let mut thread_id = 0u32;
        let mut installed = false;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(KeyThreadMsg::Tid(tid)) => thread_id = tid,
                Ok(KeyThreadMsg::Installed(ok)) => {
                    installed = ok;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        if installed {
            rt.thread = Some(handle);
            rt.thread_id = thread_id;
            rt.installed = true;
            true
        } else {
            Self::stop_thread(handle, thread_id);
            false
        }
    }

    fn uninstall(&self) {
        let mut rt = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        if !rt.installed {
            return;
        }
        if let Some(thread) = rt.thread.take() {
            Self::stop_thread(thread, rt.thread_id);
        }
        rt.thread_id = 0;
        rt.installed = false;
    }

    fn is_installed(&self) -> bool {
        self.runtime.lock().unwrap_or_else(|e| e.into_inner()).installed
    }
}

// ---------------------------------------------------------------------------
// 单元测试（幂等状态机 / 计数语义；不装真实钩子——同 `hook.rs` 既有口径）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

    /// 测试假后端：计数装 / 卸调用，可注入「安装失败」以覆盖降级路径。
    #[derive(Debug)]
    struct FakeBackend {
        installed: AtomicBool,
        installs: AtomicUsize,
        uninstalls: AtomicUsize,
        fail: AtomicBool,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                installed: AtomicBool::new(false),
                installs: AtomicUsize::new(0),
                uninstalls: AtomicUsize::new(0),
                fail: AtomicBool::new(false),
            }
        }
    }

    impl KeyHookBackend for FakeBackend {
        fn install(&self) -> bool {
            if self.fail.load(AtomicOrdering::Relaxed) {
                return false;
            }
            self.installs.fetch_add(1, AtomicOrdering::Relaxed);
            self.installed.store(true, AtomicOrdering::Relaxed);
            true
        }

        fn uninstall(&self) {
            self.uninstalls.fetch_add(1, AtomicOrdering::Relaxed);
            self.installed.store(false, AtomicOrdering::Relaxed);
        }

        fn is_installed(&self) -> bool {
            self.installed.load(AtomicOrdering::Relaxed)
        }
    }

    fn service(
        sensing: bool,
        click_through: bool,
    ) -> (Arc<KeyHookService>, Arc<KeyCounters>, Arc<FakeBackend>) {
        let backend = Arc::new(FakeBackend::new());
        let counters = Arc::new(KeyCounters::new());
        let svc = Arc::new(KeyHookService::with_backend(
            Arc::clone(&backend) as Arc<dyn KeyHookBackend>,
            Arc::clone(&counters),
            sensing,
            click_through,
        ));
        (svc, counters, backend)
    }

    #[test]
    fn counters_are_monotonic_and_only_counts() {
        let c = KeyCounters::new();
        assert_eq!(c.total(), 0);
        c.bump();
        c.bump();
        assert_eq!(c.total(), 2);
        assert_eq!(c.total(), 2, "读取无副作用");
    }

    #[test]
    fn installs_when_sensing_on_and_not_click_through() {
        let (svc, _, backend) = service(true, false);
        assert!(svc.is_installed(), "感知开 + 非穿透 → 已安装");
        assert_eq!(backend.installs.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn privacy_off_unloads_hook() {
        let (svc, _, backend) = service(true, false);
        svc.set_activity_sensing(false);
        assert!(!svc.is_installed(), "隐私关闭必须卸载键盘钩子（`02 §5.6` ④）");
        assert_eq!(backend.uninstalls.load(AtomicOrdering::Relaxed), 1);
        // 幂等：重复关闭不产生额外卸载。
        svc.set_activity_sensing(false);
        assert_eq!(backend.uninstalls.load(AtomicOrdering::Relaxed), 1);
        // 重新打开 → 重装。
        svc.set_activity_sensing(true);
        assert!(svc.is_installed());
        assert_eq!(backend.installs.load(AtomicOrdering::Relaxed), 2);
    }

    #[test]
    fn click_through_unloads_and_restores_hook() {
        let (svc, _, backend) = service(true, false);
        svc.set_click_through(true);
        assert!(!svc.is_installed(), "穿透模式必须卸载键盘钩子（`02 §5.6` ⑤）");
        svc.set_click_through(false);
        assert!(svc.is_installed());
        assert_eq!(backend.installs.load(AtomicOrdering::Relaxed), 2);
    }

    #[test]
    fn both_gates_off_keep_hook_unloaded_across_transitions() {
        let (svc, _, backend) = service(true, false);
        svc.set_activity_sensing(false);
        svc.set_click_through(true);
        assert!(!svc.is_installed());
        // 仅解除穿透但隐私仍关 → 不得重装（避免「隐私关了又偷偷装回来」）。
        svc.set_click_through(false);
        assert!(!svc.is_installed(), "隐私仍关时不得重装");
        assert_eq!(backend.installs.load(AtomicOrdering::Relaxed), 1);
        // 隐私重新打开且非穿透 → 安装。
        svc.set_activity_sensing(true);
        assert!(svc.is_installed());
    }

    #[test]
    fn install_failure_degrades_to_not_installed_without_panic() {
        let backend = Arc::new(FakeBackend::new());
        backend.fail.store(true, AtomicOrdering::Relaxed);
        let counters = Arc::new(KeyCounters::new());
        let svc = KeyHookService::with_backend(
            Arc::clone(&backend) as Arc<dyn KeyHookBackend>,
            counters,
            true,
            false,
        );
        assert!(!svc.is_installed(), "安装失败降级为未安装（不 panic）");
        // 未安装态下的卸载 / 停机均为 no-op。
        svc.shutdown();
        assert_eq!(backend.uninstalls.load(AtomicOrdering::Relaxed), 0);
    }

    #[test]
    fn shutdown_unloads_installed_hook_and_is_idempotent() {
        let (svc, counters, backend) = service(true, false);
        assert!(svc.is_installed());
        svc.shutdown();
        assert!(!svc.is_installed());
        svc.shutdown();
        assert_eq!(backend.uninstalls.load(AtomicOrdering::Relaxed), 1, "停机幂等");
        // 计数器句柄在停机后仍可安全读取（只读原子）。
        assert_eq!(svc.counters().total(), counters.total());
    }
}
