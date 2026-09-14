//! 全局低阶鼠标钩子（`WH_MOUSE_LL`）、穿透/全屏兜底与幂等装卸（S3-M1 / T-08 段 · 上）。
//!
//! 依据：`02 §5 K-2`（像素级点击热区判定 / 双命中源抽象）、`02 §5.24 L-04`
//! （高频鼠标压测四指标）、`03 §2 S3-M1 卡片`、
//! `gate/arch-audit/2026-09-13-S3M1-实现设计补充.md`（线程模型 / 跨 crate bbox /
//! 事件投递 / 穿透触发 / 降载 / swallow / features / 验收边界），以及
//! `gate/arch-audit/2026-09-13-S3M2M3-实现设计.md`（S3-M2：命中数据源换
//! `HIT_LATEST` 三态句柄 + 回调三态路由，裁定①②）。
//!
//! ## 线程模型（设计补充 §1.2）
//! 专用「`dp-hook`」线程内：`GetCurrentThreadId` → `PeekMessageW(PM_NOREMOVE)` **先建消息队列**
//! → 写线程 TLS 上下文 → `SetWindowsHookExW(WH_MOUSE_LL, proc, None /*hMod=NULL*/, 0)`
//! → `GetMessageW` **常驻消息泵**（`WM_APP_UNHOOK` → `PostQuitMessage` 退出）
//! → 泵退出后 `UnhookWindowsHookEx` + 清 TLS。卸载从任意线程经
//! `PostThreadMessageW(tid, WM_APP_UNHOOK)` → `join`；**Unhook 在钩子线程内、泵退出后执行**，
//! 防与 in-flight 回调竞争。**常驻消息泵 + 回调 O(1)** 即「无 `LowLevelHooksTimeout` 卸载」
//! 的充要工程条件。
//!
//! ## 回调纪律（`02 §5 K-2`；S3-M2 起三态路由）
//! `mouse_hook_proc` 内**仅**读线程 TLS → 解析 `MSLLHOOKSTRUCT.pt` → `hit.outcome(x, y)`
//! 一次三态判定 → Miss 放行；Move 投递（吞否恒由 [`SWALLOW_MOVE`] 决定，裁定①）；
//! 按钮/滚轮仅 [`HitOutcome::Hit`] 吞掉并投递，Hover 放行（裁定②）。**禁** `Mutex` /
//! 日志 / `format!` / 任何 IO / 堆分配（只允许原子读 + 原子计数 + 有界 `try_send`）。
//!
//! ## 坐标系（设计补充 §6.1）
//! `MSLLHOOKSTRUCT.pt` 为**物理像素（虚拟桌面坐标）**；bbox 由 `window_physical_rect()`
//! （`GetWindowRect`，同为物理像素虚拟桌面）写入 → **同一坐标系，零 VDC 换算**。
//!
//! ## 边界（红线）
//! 不实装 `HitSource`（归 S3-M2）；不做命中掩码 / `bit_at`；不新增 `pet://` 事件（C8）；
//! 零网络（C9）；`dp-platform` **零新增依赖**（只定义端口 / 队列在 `dp-app`）；
//! `SetWindowsHookExW` 传 `hMod = NULL`（LL 钩子规范，无需 `GetModuleHandleW`）。

use std::sync::{Arc, Mutex};

#[cfg(windows)]
use std::cell::RefCell;
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
#[cfg(windows)]
use windows::Win32::System::Threading::GetCurrentThreadId;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, MSLLHOOKSTRUCT, MSG, PeekMessageW,
    PostQuitMessage, PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    PM_NOREMOVE, WH_MOUSE_LL, WM_APP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

/// `WM_MOUSEMOVE` 命中 bbox 时是否**吞**事件（返回 1）。
///
/// **裁定：`false`（只投递内部事件、`CallNextHookEx` 放行）——偏离 `02 §5 K-2` 字面
/// 「命中即吞」**，理由与出处见 `设计补充 §6.2`：① 吞掉 `WM_MOUSEMOVE` 会打断被系统
/// capture 的拖拽/框选（拖拽源窗口收不到移动消息）；② FR-4-11 只约束「点击」不落空，
/// 未约束移动；③ 悬停能力由「投递内部事件」满足，无需吞移动。
///
/// 若主理人要求严格照 `02 §5 K-2` 字面「命中即吞」，**只需把本常量置 `true`**（一行回退开关）。
#[cfg(windows)]
const SWALLOW_MOVE: bool = false;

/// 唤醒 / 停机消息泵的自定义消息（`WM_APP + 1`，避免与系统消息冲突）。
#[cfg(windows)]
const WM_APP_UNHOOK: u32 = WM_APP + 1;

/// 等待钩子线程回报 `tid` / 安装结果的超时（毫秒）。正常路径二者皆瞬时返回。
#[cfg(windows)]
const HOOK_SIGNAL_TIMEOUT_MS: u64 = 2_000;

// ---------------------------------------------------------------------------
// 事件与端口（平台侧，跨平台可编译）
// ---------------------------------------------------------------------------

/// 鼠标按键（POD）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    /// 左键。
    Left,
    /// 右键。
    Right,
    /// 中键。
    Middle,
    /// 侧键 1。
    X1,
    /// 侧键 2。
    X2,
}

/// 低阶鼠标钩子捕获的内部事件（`Copy` POD，**物理像素（虚拟桌面）**，无堆分配）。
///
/// 命名刻意区别于 `02 K-2` 的 `InputEvent`（core 侧交互事件，S3-M3）：平台侧用 `HookEvent`，
/// 由 S3-M3 再做 `HookEvent → InputEvent` 映射，避免跨层同名 / 越权。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookEvent {
    /// 移动。
    Move { x: i32, y: i32 },
    /// 按下。
    Down { button: MouseButton, x: i32, y: i32 },
    /// 抬起。
    Up { button: MouseButton, x: i32, y: i32 },
    /// 滚轮（`delta` 为 `WHEEL_DELTA` 的整数倍，正=远离用户）。
    Wheel { delta: i32, x: i32, y: i32 },
}

/// 事件投递口（端口）。回调内调用，**必须非阻塞 / 零分配 / 零 IO**。
pub trait HookSink: Send + Sync {
    /// 投递一个命中事件（实现方不得阻塞 / 分配 / 抛错）。
    fn on_event(&self, ev: HookEvent);
}

/// 三态命中判定结果（S3-M2 裁定5 + 主理人修正：`dp-platform` 侧仅此枚举与
/// 默认退化实现，真三态语义由 `dp-app::hit_latest::HitLatestHandle` 覆写产出）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitOutcome {
    /// 未命中（窗口内透明区）→ 回调放行、不投递（含 Move+Miss，裁定①）。
    Miss,
    /// 仅次级热区命中（悬停有效、点击落主判定）→ Move 投递、按钮/滚轮放行。
    Hover,
    /// 主热区命中（不透明像素）→ 按钮/滚轮吞掉并投递、Move 投递。
    Hit,
}

/// 命中判定端口（只读）。`dp-platform` 只持只读 trait 对象，**不含第二份矩形**。
pub trait HitTest: Send + Sync {
    /// 物理像素点是否落在命中粗筛矩形内（半开区间 `[left, right) × [top, bottom)`）。
    ///
    /// 二态粗筛口径（S3-M1 既有语义，判定式与 `dp-app::ports::PetBBoxHandle`
    /// 逐位同式）；三态精确判定见 [`HitTest::outcome`]。
    fn contains(&self, x: i32, y: i32) -> bool;

    /// 三态命中判定（S3-M2 主理人修正）：默认退化为二态——[`Self::contains`]
    /// 为真 → [`HitOutcome::Hit`]，否则 [`HitOutcome::Miss`]（保证既有
    /// `PetBBoxHandle` 实现**零改动零回归**）；具备次级热区语义的实现
    /// （`HitLatestHandle`）覆写本方法返回真三态。
    fn outcome(&self, x: i32, y: i32) -> HitOutcome {
        if self.contains(x, y) {
            HitOutcome::Hit
        } else {
            HitOutcome::Miss
        }
    }
}

/// 半开区间矩形包含的**参考判定式**：`x ∈ [left, right) && y ∈ [top, bottom)`。
///
/// 与 `dp-app::ports::PetBBoxHandle::contains` **逐位同式**（唯一真源仍在 `dp-app`）；此处仅作
/// 可单测的纯函数与语义文档载体（设计补充 §6.1），**不持有任何矩形状态**。
/// S3-M2 换 `HIT_LATEST` 时**判定式不变、只换数据源**。
#[inline]
#[must_use]
pub fn rect_contains(left: i32, top: i32, right: i32, bottom: i32, x: i32, y: i32) -> bool {
    x >= left && x < right && y >= top && y < bottom
}

// ---------------------------------------------------------------------------
// 后端抽象（使幂等状态机可在无头环境单测，设计补充 §8.1）
// ---------------------------------------------------------------------------

/// 钩子线程 + OS 原语的抽象端口。
///
/// 抽象粒度 = 「一次**完整的装 / 卸动作**」：生产实装 [`WinHookBackend`] 内部起专用线程跑
/// `GetMessageW` 消息泵 + `SetWindowsHookExW`；测试用 `FakeBackend` 计数。
/// **两个方法均须幂等**（已在目标态时为 no-op），使 [`HookService`] 的幂等状态机可无头单测。
pub trait HookBackend: Send + Sync {
    /// 安装低阶钩子（内部起线程 + 建消息队列 + 装钩子）。返回是否安装成功（降级返回 `false`）。
    /// **幂等**：已安装时 no-op 并返回 `true`。
    fn install(&self) -> bool;
    /// 卸载低阶钩子（停泵 → `UnhookWindowsHookEx` → `join`）。**幂等**：未安装时 no-op。
    fn uninstall(&self);
    /// 当前是否处于**已安装**态。
    fn is_installed(&self) -> bool;
}

// ---------------------------------------------------------------------------
// HookService：装 / 卸幂等状态机（设计补充 §1.3 / §4.2 / §5.1）
// ---------------------------------------------------------------------------

/// 低阶鼠标钩子服务。
///
/// 两个 gate 求值启停：`want = !click_through && !fullscreen_hidden`。**仅状态转变时**
/// 调 [`HookBackend::install`] / [`HookBackend::uninstall`]（二者内部再作幂等守卫）。
pub struct HookService {
    backend: Arc<dyn HookBackend>,
    runtime: Mutex<GateState>,
}

/// 两个 gate 的当前取值。
struct GateState {
    click_through: bool,
    fullscreen_hidden: bool,
}

impl HookService {
    /// 生产构造：按两个 gate 求值启停（安装失败**降级为「未安装」+ 告警，不 panic**，
    /// `02 §7.4.2`）。
    #[cfg(windows)]
    #[must_use]
    pub fn new(
        hit: Arc<dyn HitTest>,
        sink: Arc<dyn HookSink>,
        click_through: bool,
        fullscreen_hidden: bool,
    ) -> Self {
        Self::with_backend(
            Arc::new(WinHookBackend::new(hit, sink)),
            click_through,
            fullscreen_hidden,
        )
    }

    /// 注入后端的构造（测试 / 备选实装；生产用 [`HookService::new`]）。构造即 `refresh()`。
    #[must_use]
    pub fn with_backend(
        backend: Arc<dyn HookBackend>,
        click_through: bool,
        fullscreen_hidden: bool,
    ) -> Self {
        let svc = Self {
            backend,
            runtime: Mutex::new(GateState { click_through, fullscreen_hidden }),
        };
        svc.refresh();
        svc
    }

    /// gate①：穿透态（`true` = 穿透 → 卸载钩子，`01 §6.4` / `02 §5 K-2`）。
    pub fn set_click_through(&self, on: bool) {
        if self.set_gate(|g| &mut g.click_through, on) {
            self.refresh();
        }
    }

    /// gate②：全屏隐藏态（`true` = 隐藏 → 卸载钩子；降载）。
    pub fn set_fullscreen_hidden(&self, on: bool) {
        if self.set_gate(|g| &mut g.fullscreen_hidden, on) {
            self.refresh();
        }
    }

    /// 幂等重挂：gate 允许且未安装时重装，返回调用后是否处于已安装态。
    ///
    /// 供 K-2「心跳探测 / 丢失重挂」在 supervisor 网格上调用（设计补充 §5.3，预留）。
    pub fn ensure_installed(&self) -> bool {
        self.refresh();
        self.is_installed()
    }

    /// 当前是否已安装（供探测 / 自检）。
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.backend.is_installed()
    }

    /// 应用退出：等价 `disable()`（幂等）。
    pub fn shutdown(&self) {
        self.disable();
    }

    /// 更新某 gate；返回是否**发生变化**（无变化 → 不触发 `refresh`）。
    fn set_gate<F>(&self, pick: F, on: bool) -> bool
    where
        F: FnOnce(&mut GateState) -> &mut bool,
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
            !g.click_through && !g.fullscreen_hidden
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
// Win32 生产实装（设计补充 §1.2 / §6）
// ---------------------------------------------------------------------------

/// 钩子线程回报消息。
#[cfg(windows)]
enum ThreadMsg {
    /// 线程已建立消息队列，回报其线程 id（保证安装失败时调用方仍能正确停机）。
    Tid(u32),
    /// 安装结果（`true` = 成功常驻；`false` = 降级退出）。
    Installed(bool),
}

/// 回调热路径所用的线程 TLS 上下文（先例：`WH_MOUSE_LL` 回调总在**安装线程**派发）。
///
/// 用 TLS 而非 `Mutex`：回调内 `borrow()` **零锁、零分配**（设计补充 §3.2）。
#[cfg(windows)]
struct HookCtx {
    hit: Arc<dyn HitTest>,
    sink: Arc<dyn HookSink>,
}

#[cfg(windows)]
thread_local! {
    /// 当前线程的钩子上下文（仅 `dp-hook` 安装线程写入，回调只读）。
    static HOOK_CTX: RefCell<Option<HookCtx>> = const { RefCell::new(None) };
}

/// 低阶鼠标钩子回调（`HOOKPROC`）。
///
/// **热路径纪律**：负 `code` 直接放行；否则仅读 TLS → `hit.outcome` 一次三态判定
/// （Miss 放行；Move 投递；按钮/滚轮仅 Hit）→ 命中时 `sink.on_event` 投递，
/// 按钮/滚轮 + Hit 返回 1 吞掉。**不做**任何 `Mutex` / 日志 / `format!` / IO / 分配。
///
/// # Safety
/// 由系统在安装线程上下文中调用；`lparam` 指向合法 `MSLLHOOKSTRUCT`（系统保证）。
#[cfg(windows)]
unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // 规范：nCode < 0 必须原样放行，不得处理。
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    // lParam = *const MSLLHOOKSTRUCT（含物理像素 pt）。
    let info = lparam.0 as *const MSLLHOOKSTRUCT;
    if info.is_null() {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let pt = (*info).pt;
    let mouse_data = (*info).mouseData;
    let message = wparam.0 as u32;

    // 三态路由（S3-M2 裁定①②⑤；QA Bug#1 修复）：`outcome` 一次判定——
    // Miss → 放行不投递；Move + Hit/Hover → 投递（吞否恒由 SWALLOW_MOVE 决定）；
    // 按钮/滚轮仅 Hit → 吞+投递，Hover/Miss → 放行（点击落主判定）。
    let swallow = HOOK_CTX.with(|cell| {
        let cell = cell.borrow();
        let Some(ctx) = cell.as_ref() else {
            return false;
        };
        let outcome = ctx.hit.outcome(pt.x, pt.y);
        if outcome == HitOutcome::Miss {
            return false;
        }
        let Some((ev, _base)) = event_from_message(message, pt.x, pt.y, mouse_data) else {
            return false;
        };
        let is_move = matches!(ev, HookEvent::Move { .. });
        let hit_swallow = if is_move { SWALLOW_MOVE } else { outcome == HitOutcome::Hit };
        if is_move || hit_swallow {
            ctx.sink.on_event(ev);
        }
        hit_swallow
    });

    if swallow {
        LRESULT(1)
    } else {
        CallNextHookEx(None, code, wparam, lparam)
    }
}

/// 由 Win32 鼠标消息构造内部事件（物理像素）。
///
/// 返回 `(事件, 类别基础吞标志)`；`None` = 非本模块关注的消息（放行）。
/// 基础标志仅为消息类别口径（Move=`SWALLOW_MOVE`、按钮/滚轮恒 `true`），
/// **生产路由以 `HitTest::outcome` 三态重判**（见 `mouse_hook_proc`）：
/// 按钮/滚轮仅 `Hit` 吞、Hover 放行；Move 吞否恒由 [`SWALLOW_MOVE`] 决定。
#[cfg(windows)]
#[must_use]
fn event_from_message(message: u32, x: i32, y: i32, mouse_data: u32) -> Option<(HookEvent, bool)> {
    let ev = match message {
        WM_MOUSEMOVE => (HookEvent::Move { x, y }, SWALLOW_MOVE),
        WM_LBUTTONDOWN => (HookEvent::Down { button: MouseButton::Left, x, y }, true),
        WM_LBUTTONUP => (HookEvent::Up { button: MouseButton::Left, x, y }, true),
        WM_RBUTTONDOWN => (HookEvent::Down { button: MouseButton::Right, x, y }, true),
        WM_RBUTTONUP => (HookEvent::Up { button: MouseButton::Right, x, y }, true),
        WM_MBUTTONDOWN => (HookEvent::Down { button: MouseButton::Middle, x, y }, true),
        WM_MBUTTONUP => (HookEvent::Up { button: MouseButton::Middle, x, y }, true),
        WM_XBUTTONDOWN => (HookEvent::Down { button: xbutton(mouse_data), x, y }, true),
        WM_XBUTTONUP => (HookEvent::Up { button: xbutton(mouse_data), x, y }, true),
        WM_MOUSEWHEEL => (HookEvent::Wheel { delta: wheel_delta(mouse_data), x, y }, true),
        _ => return None,
    };
    Some(ev)
}

/// `WM_XBUTTON*` 的 `mouseData` 高位字 → 侧键（1 = `XBUTTON1`，2 = `XBUTTON2`）。
#[cfg(windows)]
#[must_use]
fn xbutton(mouse_data: u32) -> MouseButton {
    match (mouse_data >> 16) as u16 {
        2 => MouseButton::X2,
        _ => MouseButton::X1,
    }
}

/// `WM_MOUSEWHEEL` 的 `mouseData` 高位字 → 带符号滚轮增量。
#[cfg(windows)]
#[must_use]
fn wheel_delta(mouse_data: u32) -> i32 {
    ((mouse_data >> 16) & 0xFFFF) as u16 as i16 as i32
}

/// `dp-hook` 线程主体（设计补充 §1.2 步骤 ①~⑥）。
#[cfg(windows)]
fn hook_thread_main(hit: Arc<dyn HitTest>, sink: Arc<dyn HookSink>, tx: std::sync::mpsc::Sender<ThreadMsg>) {
    // ① 线程 id（供调用方 PostThreadMessageW 唤醒 / 停机）。
    let thread_id = unsafe { GetCurrentThreadId() };
    let _ = tx.send(ThreadMsg::Tid(thread_id));

    // ② 先建线程消息队列（PostThreadMessageW 对无队列线程会失败）。
    let mut msg = MSG::default();
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };

    // ③ 写线程 TLS 上下文（回调只读）。
    HOOK_CTX.with(|cell| *cell.borrow_mut() = Some(HookCtx { hit, sink }));

    // ④ 安装低阶鼠标钩子。
    //    hMod = NULL：LL 钩子回调由系统在安装线程上下文内派发，无需模块句柄（设计补充 §7）。
    let hook = match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), None, 0) } {
        Ok(h) => h,
        Err(err) => {
            // 降级不 panic：清 TLS、通知失败、线程退出。
            eprintln!("[dp-platform] SetWindowsHookExW(WH_MOUSE_LL) 失败，降级：{err}");
            HOOK_CTX.with(|cell| *cell.borrow_mut() = None);
            let _ = tx.send(ThreadMsg::Installed(false));
            return;
        }
    };
    let _ = tx.send(ThreadMsg::Installed(true));

    // ⑤ 常驻消息泵：回调在本线程派发；泵在线 + 回调 O(1) ⇒ 无 LowLevelHooksTimeout 卸载。
    loop {
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 == 0 {
            break; // WM_QUIT
        }
        if ret.0 == -1 {
            eprintln!("[dp-platform] GetMessageW 取消息失败，钩子线程降级退出");
            break;
        }
        if msg.message == WM_APP_UNHOOK {
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
    HOOK_CTX.with(|cell| *cell.borrow_mut() = None);
}

/// 后端运行期状态。
#[cfg(windows)]
struct BackendRuntime {
    thread: Option<std::thread::JoinHandle<()>>,
    thread_id: u32,
    installed: bool,
}

/// Win32 生产后端：内部持有 `dp-hook` 线程句柄与安装态。
#[cfg(windows)]
struct WinHookBackend {
    hit: Arc<dyn HitTest>,
    sink: Arc<dyn HookSink>,
    runtime: Mutex<BackendRuntime>,
}

#[cfg(windows)]
impl WinHookBackend {
    fn new(hit: Arc<dyn HitTest>, sink: Arc<dyn HookSink>) -> Self {
        Self {
            hit,
            sink,
            runtime: Mutex::new(BackendRuntime { thread: None, thread_id: 0, installed: false }),
        }
    }

    /// 停机一个（可能残留的）钩子线程：发 `WM_APP_UNHOOK` → `join`。
    fn stop_thread(thread: std::thread::JoinHandle<()>, thread_id: u32) {
        if thread_id != 0 {
            // 安全：tid 来自本进程 dp-hook 线程，其消息队列已在 step ② 建好。
            let _ = unsafe { PostThreadMessageW(thread_id, WM_APP_UNHOOK, WPARAM(0), LPARAM(0)) };
        }
        let _ = thread.join();
    }
}

#[cfg(windows)]
impl HookBackend for WinHookBackend {
    fn install(&self) -> bool {
        // ① 幂等检查 + 取出可能残留的线程句柄（锁内快速完成，勿跨 join 持锁）。
        let residual = {
            let mut rt = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            if rt.installed {
                return true; // 幂等：已装再 enable → no-op
            }
            let r = rt.thread.take().map(|h| (h, rt.thread_id));
            rt.thread_id = 0;
            r
        };
        if let Some((thread, tid)) = residual {
            Self::stop_thread(thread, tid);
        }

        // ② 起钩子线程（失败降级，不 panic）。
        let (tx, rx) = std::sync::mpsc::channel::<ThreadMsg>();
        let hit = Arc::clone(&self.hit);
        let sink = Arc::clone(&self.sink);
        let handle = match std::thread::Builder::new()
            .name("dp-hook".to_string())
            .spawn(move || hook_thread_main(hit, sink, tx))
        {
            Ok(h) => h,
            Err(err) => {
                eprintln!("[dp-platform] 钩子线程启动失败，降级：{err}");
                return false;
            }
        };

        // ③ 等 tid（保证即使安装失败也能正确停机）。
        let thread_id = match rx.recv_timeout(Duration::from_millis(HOOK_SIGNAL_TIMEOUT_MS)) {
            Ok(ThreadMsg::Tid(tid)) => tid,
            _ => {
                eprintln!("[dp-platform] 钩子线程未回报线程 id，降级");
                let _ = handle.join();
                return false;
            }
        };

        // ④ 等安装结果。
        match rx.recv_timeout(Duration::from_millis(HOOK_SIGNAL_TIMEOUT_MS)) {
            Ok(ThreadMsg::Installed(true)) => {
                let mut rt = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
                rt.thread = Some(handle);
                rt.thread_id = thread_id;
                rt.installed = true;
                true
            }
            Ok(_) => {
                // 安装失败（degraded）：线程已退出，join 回收。
                eprintln!("[dp-platform] WH_MOUSE_LL 安装失败，降级（未安装）");
                let _ = handle.join();
                false
            }
            Err(_) => {
                // 超时：无法确认 → 视为未安装（降级），但保留句柄供后续停机（避免悬挂线程）。
                eprintln!("[dp-platform] WH_MOUSE_LL 安装信号超时，降级");
                let mut rt = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
                rt.thread = Some(handle);
                rt.thread_id = thread_id;
                rt.installed = false;
                false
            }
        }
    }

    fn uninstall(&self) {
        let stopped = {
            let mut rt = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            rt.installed = false;
            let tid = rt.thread_id;
            rt.thread_id = 0;
            rt.thread.take().map(|h| (h, tid))
        };
        if let Some((thread, tid)) = stopped {
            // 泵退出 + Unhook 均在钩子线程内完成（step ⑥），join 保证卸载完整。
            Self::stop_thread(thread, tid);
        }
    }

    fn is_installed(&self) -> bool {
        self.runtime.lock().unwrap_or_else(|e| e.into_inner()).installed
    }
}

#[cfg(windows)]
impl Drop for WinHookBackend {
    fn drop(&mut self) {
        // 兜底：后端析构时停机任何残留螺纹（正常路径已由 uninstall 停机）。
        self.uninstall();
    }
}

// ---------------------------------------------------------------------------
// 单元测试（状态机幂等 / 包含数学 / POD / 坐标不变量；沙箱无头可跑）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// 计数后端：把装 / 卸映射为原子计数，验证状态机与幂等（无头环境可跑）。
    #[derive(Default)]
    struct FakeBackend {
        installs: AtomicU32,
        uninstalls: AtomicU32,
        installed: AtomicBool,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self::default()
        }

        fn installs(&self) -> u32 {
            self.installs.load(Ordering::Relaxed)
        }

        fn uninstalls(&self) -> u32 {
            self.uninstalls.load(Ordering::Relaxed)
        }
    }

    impl HookBackend for FakeBackend {
        fn install(&self) -> bool {
            if self.installed.swap(true, Ordering::Relaxed) {
                return true; // 幂等：已装 → no-op
            }
            self.installs.fetch_add(1, Ordering::Relaxed);
            true
        }

        fn uninstall(&self) {
            if !self.installed.swap(false, Ordering::Relaxed) {
                return; // 幂等：未装 → no-op
            }
            self.uninstalls.fetch_add(1, Ordering::Relaxed);
        }

        fn is_installed(&self) -> bool {
            self.installed.load(Ordering::Relaxed)
        }
    }

    /// 以 FakeBackend 构造服务（构造即 `refresh()`）。
    fn svc(backend: &Arc<FakeBackend>, ct: bool, fsh: bool) -> HookService {
        let shared: Arc<FakeBackend> = Arc::clone(backend);
        let backend: Arc<dyn HookBackend> = shared; // 显式 unsize 强制
        HookService::with_backend(backend, ct, fsh)
    }

    // -- gate 两两组合四种 ------------------------------------------------------

    #[test]
    fn gates_both_false_installs_on_construction() {
        let b = Arc::new(FakeBackend::new());
        let s = svc(&b, false, false);
        assert!(s.is_installed(), "两 gate 均 false → 构造即装");
        assert_eq!(b.installs(), 1);
        assert_eq!(b.uninstalls(), 0);
    }

    #[test]
    fn gate_click_through_true_suppresses_install() {
        let b = Arc::new(FakeBackend::new());
        let s = svc(&b, true, false);
        assert!(!s.is_installed(), "穿透态 → 不装");
        assert_eq!(b.installs(), 0);
    }

    #[test]
    fn gate_fullscreen_hidden_true_suppresses_install() {
        let b = Arc::new(FakeBackend::new());
        let s = svc(&b, false, true);
        assert!(!s.is_installed(), "全屏隐藏态 → 不装");
        assert_eq!(b.installs(), 0);
    }

    #[test]
    fn both_gates_true_suppresses_install() {
        let b = Arc::new(FakeBackend::new());
        let s = svc(&b, true, true);
        assert!(!s.is_installed());
        assert_eq!(b.installs(), 0);
    }

    // -- 幂等：enable / disable 重入 no-op --------------------------------------

    #[test]
    fn set_click_through_reentry_is_noop() {
        let b = Arc::new(FakeBackend::new());
        let s = svc(&b, false, false); // 已装
        s.set_click_through(true);
        assert!(!s.is_installed());
        assert_eq!(b.uninstalls(), 1);
        s.set_click_through(true); // 重入 → no-op
        assert_eq!(b.uninstalls(), 1, "disable 重入不应重复卸载");

        s.set_click_through(false);
        assert!(s.is_installed());
        assert_eq!(b.installs(), 2);
        s.set_click_through(false); // 重入 → no-op
        assert_eq!(b.installs(), 2, "enable 重入不应重复安装");
    }

    #[test]
    fn set_fullscreen_hidden_reentry_is_noop() {
        let b = Arc::new(FakeBackend::new());
        let s = svc(&b, false, false);
        s.set_fullscreen_hidden(true);
        assert!(!s.is_installed());
        assert_eq!(b.uninstalls(), 1);
        s.set_fullscreen_hidden(true);
        assert_eq!(b.uninstalls(), 1);
        s.set_fullscreen_hidden(false);
        assert!(s.is_installed());
        assert_eq!(b.installs(), 2);
    }

    #[test]
    fn ensure_installed_is_idempotent_when_already_installed() {
        let b = Arc::new(FakeBackend::new());
        let s = svc(&b, false, false);
        assert!(s.ensure_installed(), "已装 → 幂等 no-op");
        assert_eq!(b.installs(), 1, "重挂不得重复安装");
    }

    #[test]
    fn ensure_installed_reinstalls_after_external_stop() {
        // 模拟系统静默卸载（K-2 心跳场景）：后端被判未装 → ensure 重挂。
        let b = Arc::new(FakeBackend::new());
        let s = svc(&b, false, false);
        b.uninstall(); // 外部停机（backend 视角）
        assert!(!s.is_installed());
        assert!(s.ensure_installed());
        assert_eq!(b.installs(), 2);
    }

    // -- gate 组合转变 -----------------------------------------------------------

    #[test]
    fn combined_gate_transitions_are_consistent() {
        let b = Arc::new(FakeBackend::new());
        let s = svc(&b, false, false); // 已装
        s.set_click_through(true); // want=false → 卸载
        assert!(!s.is_installed());

        s.set_fullscreen_hidden(true); // want 仍 false → disable 重入 no-op
        assert_eq!(b.uninstalls(), 1);
        assert!(!s.is_installed());

        s.set_click_through(false); // ct=false 但 fsh=true → want 仍 false → 不重装
        assert!(!s.is_installed());
        assert_eq!(b.installs(), 1, "另一 gate 仍挡时不得重装");

        s.set_fullscreen_hidden(false); // want=true → 重装
        assert!(s.is_installed());
        assert_eq!(b.installs(), 2);
        assert_eq!(b.uninstalls(), 1);
    }

    #[test]
    fn shutdown_stops_and_is_idempotent() {
        let b = Arc::new(FakeBackend::new());
        let s = svc(&b, false, false);
        s.shutdown();
        assert!(!s.is_installed());
        assert_eq!(b.uninstalls(), 1);
        s.shutdown(); // 幂等
        assert_eq!(b.uninstalls(), 1);
    }

    // -- 矩形包含（表驱动：四角 / 边界 / 半开区间 / 退化） -----------------------

    #[test]
    fn rect_contains_table_half_open_and_degenerate() {
        // 正常矩形 [10,20)×[30,40)
        assert!(rect_contains(10, 30, 20, 40, 10, 30), "左上闭");
        assert!(rect_contains(10, 30, 20, 40, 19, 39), "右下减一闭");
        assert!(rect_contains(10, 30, 20, 40, 15, 35), "内点");
        assert!(!rect_contains(10, 30, 20, 40, 20, 30), "右开");
        assert!(!rect_contains(10, 30, 20, 40, 10, 40), "下开");
        assert!(!rect_contains(10, 30, 20, 40, 9, 30), "左外");
        assert!(!rect_contains(10, 30, 20, 40, 25, 35), "右外");
        assert!(!rect_contains(10, 30, 20, 40, 10, 29), "上外");
        assert!(!rect_contains(10, 30, 20, 40, 15, 45), "下外");

        // 退化：零面积 / 反向（left ≥ right 或 top ≥ bottom）→ 恒 false，不 panic。
        assert!(!rect_contains(10, 30, 10, 40, 10, 35), "零宽");
        assert!(!rect_contains(10, 30, 20, 30, 15, 30), "零高");
        assert!(!rect_contains(10, 40, 20, 30, 15, 35), "top>bottom 反向");
        assert!(!rect_contains(20, 30, 10, 40, 15, 35), "left>right 反向");
        assert!(!rect_contains(0, 0, 0, 0, 0, 0), "全零（未刷新 bbox）");

        // 多屏负坐标域（左侧副屏）：半开区间在负区间同样成立。
        assert!(rect_contains(-1920, 0, 0, 1080, -1, 0), "负域左上闭");
        assert!(!rect_contains(-1920, 0, 0, 1080, 0, 0), "负域右开");
        assert!(!rect_contains(-1920, 0, 0, 1080, -1921, 0), "负域左外");
    }

    // -- 三态 outcome 默认退化（S3-M2 主理人修正：默认实现零回归） ----------------

    /// 固定矩形命中桩（验证 trait 默认 outcome 退化为二态）。
    struct FixedRect(bool);

    impl HitTest for FixedRect {
        fn contains(&self, _x: i32, _y: i32) -> bool {
            self.0
        }
    }

    #[test]
    fn outcome_defaults_to_two_state_from_contains() {
        assert_eq!(FixedRect(true).outcome(0, 0), HitOutcome::Hit, "contains=true → Hit");
        assert_eq!(FixedRect(false).outcome(0, 0), HitOutcome::Miss, "contains=false → Miss");
        // 默认实现永不产出 Hover（真三态由 HitLatestHandle 覆写，dp-app 侧单测）。
    }

    #[test]
    fn hit_outcome_is_copy_pod() {
        fn assert_copy<T: Copy + Eq + std::fmt::Debug>() {}
        assert_copy::<HitOutcome>();
        assert_ne!(HitOutcome::Hover, HitOutcome::Hit);
        assert_ne!(HitOutcome::Miss, HitOutcome::Hover);
    }

    // -- HookEvent POD 语义 ------------------------------------------------------

    #[test]
    fn hook_event_and_button_are_pod_copy() {
        fn assert_copy<T: Copy>() {}
        fn assert_eq_ord<T: Eq + PartialEq>() {}
        assert_copy::<HookEvent>();
        assert_copy::<MouseButton>();
        assert_eq_ord::<HookEvent>();
        assert_eq_ord::<MouseButton>();

        let a = HookEvent::Down { button: MouseButton::Left, x: 10, y: 20 };
        let b = a; // Copy（非移动）
        assert_eq!(a, b, "Copy 后仍可读原值");
        assert_ne!(a, HookEvent::Up { button: MouseButton::Left, x: 10, y: 20 });
        assert_ne!(MouseButton::Left, MouseButton::Right);
    }

    // -- 坐标不变量 --------------------------------------------------------------

    #[test]
    fn coordinate_is_physical_pixel_verbatim_no_vdc_conversion() {
        // 不变量：`MSLLHOOKSTRUCT.pt`（物理像素，虚拟桌面）与
        // `GetWindowRect`（`window_physical_rect`，同为物理像素虚拟桌面）坐标系一致，
        // 钩子侧**零 VDC 换算**——事件坐标原样透传（不做缩放 / 偏移）。
        let phys = (-1234_i32, 567_i32);
        let ev = HookEvent::Move { x: phys.0, y: phys.1 };
        match ev {
            HookEvent::Move { x, y } => assert_eq!((x, y), phys, "坐标原样透传"),
            _ => unreachable!(),
        }
    }

    // -- 消息 → 事件分类（Win32 常量相关，仅在 windows 下编译） -------------------

    #[cfg(windows)]
    #[test]
    fn event_from_message_classifies_and_swallow_flags() {
        let mv = event_from_message(WM_MOUSEMOVE, 5, 6, 0).expect("MOVE 应被识别");
        assert_eq!(mv.0, HookEvent::Move { x: 5, y: 6 });
        assert!(!mv.1, "WM_MOUSEMOVE 默认不吞（SWALLOW_MOVE=false）");

        let down = event_from_message(WM_LBUTTONDOWN, 1, 2, 0).expect("LBUTTONDOWN");
        assert_eq!(down.0, HookEvent::Down { button: MouseButton::Left, x: 1, y: 2 });
        assert!(down.1, "按钮命中应吞");

        let up = event_from_message(WM_RBUTTONUP, -3, 4, 0).expect("RBUTTONUP");
        assert_eq!(up.0, HookEvent::Up { button: MouseButton::Right, x: -3, y: 4 });
        assert!(up.1);

        let wheel = event_from_message(WM_MOUSEWHEEL, 3, 4, 0x0078_0000).expect("WHEEL");
        assert_eq!(wheel.0, HookEvent::Wheel { delta: 120, x: 3, y: 4 });
        assert!(wheel.1, "滚轮命中应吞");

        let wheel_neg = event_from_message(WM_MOUSEWHEEL, 0, 0, 0xFF88_0000).expect("WHEEL-");
        assert_eq!(wheel_neg.0, HookEvent::Wheel { delta: -120, x: 0, y: 0 });

        let x2 = event_from_message(WM_XBUTTONDOWN, 0, 0, 0x0002_0000).expect("XBUTTON2");
        assert_eq!(x2.0, HookEvent::Down { button: MouseButton::X2, x: 0, y: 0 });
        assert!(x2.1);

        let x1 = event_from_message(WM_XBUTTONUP, 0, 0, 0x0001_0000).expect("XBUTTON1");
        assert_eq!(x1.0, HookEvent::Up { button: MouseButton::X1, x: 0, y: 0 });

        assert!(event_from_message(0xFFFF, 0, 0, 0).is_none(), "无关消息放行");
    }

    // -- QA 探针（清单#1）：回调三态路由矩阵（直调 mouse_hook_proc + TLS 注入） ----
    //
    // 生产路由真身是 `mouse_hook_proc`（TLS 上下文 + Win32 消息分派），既有用例只
    // 覆盖 `event_from_message` 与 trait 默认实现；本组探针把回调本体作为被测对象，
    // 以「固定三态桩 + 事件记录桩」直调回调，逐格核对裁定5 路由矩阵。

    /// 固定三态命中桩（镜像 `HitLatestHandle` 口径：contains = outcome != Miss）。
    #[cfg(windows)]
    struct QaFixedOutcome(HitOutcome);

    #[cfg(windows)]
    impl HitTest for QaFixedOutcome {
        fn contains(&self, _x: i32, _y: i32) -> bool {
            self.0 != HitOutcome::Miss
        }
        fn outcome(&self, _x: i32, _y: i32) -> HitOutcome {
            self.0
        }
    }

    /// 事件记录桩 sink（仅测试线程使用，非回调热路径）。
    #[cfg(windows)]
    struct QaRecordingSink {
        events: Mutex<Vec<HookEvent>>,
    }

    #[cfg(windows)]
    impl HookSink for QaRecordingSink {
        fn on_event(&self, ev: HookEvent) {
            self.events.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
        }
    }

    #[cfg(windows)]
    impl QaRecordingSink {
        fn snapshot(&self) -> Vec<HookEvent> {
            self.events.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    /// 注入 TLS 上下文执行 `f`，结束后清除（回调读 TLS 的调用线程 = 本测试线程）。
    #[cfg(windows)]
    fn qa_with_ctx(hit: Arc<dyn HitTest>, sink: Arc<dyn HookSink>, f: impl FnOnce()) {
        HOOK_CTX.with(|cell| *cell.borrow_mut() = Some(HookCtx { hit, sink }));
        f();
        HOOK_CTX.with(|cell| *cell.borrow_mut() = None);
    }

    /// 以给定 Win32 鼠标消息直调钩子回调（`MSLLHOOKSTRUCT` 零值初始化后写 pt/mouseData）。
    #[cfg(windows)]
    fn qa_fire(message: u32, x: i32, y: i32, mouse_data: u32) -> LRESULT {
        use windows::Win32::Foundation::POINT;
        // Safety：栈上 POD 结构，全零位型合法；被测回调只读 pt / mouseData。
        let mut info: MSLLHOOKSTRUCT = unsafe { std::mem::zeroed() };
        info.pt = POINT { x, y };
        info.mouseData = mouse_data;
        unsafe {
            mouse_hook_proc(
                0,
                WPARAM(message as usize),
                LPARAM(&info as *const MSLLHOOKSTRUCT as isize),
            )
        }
    }

    #[cfg(windows)]
    #[test]
    fn qa_probe_hook_button_hit_is_swallowed_and_delivered() {
        let sink = Arc::new(QaRecordingSink { events: Mutex::new(Vec::new()) });
        let hit: Arc<dyn HitTest> = Arc::new(QaFixedOutcome(HitOutcome::Hit));
        qa_with_ctx(hit, Arc::clone(&sink) as Arc<dyn HookSink>, || {
            assert_eq!(qa_fire(WM_LBUTTONDOWN, 5, 6, 0).0, 1, "按钮+Hit → 吞（返回 1）");
        });
        assert_eq!(
            sink.snapshot(),
            vec![HookEvent::Down { button: MouseButton::Left, x: 5, y: 6 }],
            "按钮+Hit → 投递 Down"
        );
    }

    #[cfg(windows)]
    #[test]
    fn qa_probe_hook_move_hit_and_hover_delivered_not_swallowed() {
        // 裁定①：Move 按 !=Miss 投递；SWALLOW_MOVE=false 语义不变（不吞）。
        for outcome in [HitOutcome::Hit, HitOutcome::Hover] {
            let sink = Arc::new(QaRecordingSink { events: Mutex::new(Vec::new()) });
            let hit: Arc<dyn HitTest> = Arc::new(QaFixedOutcome(outcome));
            qa_with_ctx(hit, Arc::clone(&sink) as Arc<dyn HookSink>, || {
                let r = qa_fire(WM_MOUSEMOVE, 7, 8, 0);
                assert_ne!(r.0, 1, "Move+{outcome:?} 不吞（SWALLOW_MOVE=false 语义不变）");
            });
            assert_eq!(
                sink.snapshot(),
                vec![HookEvent::Move { x: 7, y: 8 }],
                "Move+{outcome:?} → 投递（!=Miss 即投递，裁定①）"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn qa_probe_hook_miss_never_delivers_any_message_class() {
        // 裁定①：Move+Miss 不投递；裁定②：按钮/滚轮+Miss 放行不投递。
        let sink = Arc::new(QaRecordingSink { events: Mutex::new(Vec::new()) });
        let hit: Arc<dyn HitTest> = Arc::new(QaFixedOutcome(HitOutcome::Miss));
        qa_with_ctx(hit, Arc::clone(&sink) as Arc<dyn HookSink>, || {
            for (msg, data) in
                [(WM_MOUSEMOVE, 0u32), (WM_LBUTTONDOWN, 0), (WM_RBUTTONUP, 0), (WM_MOUSEWHEEL, 0x0078_0000)]
            {
                let r = qa_fire(msg, 9, 9, data);
                assert_ne!(r.0, 1, "Miss 一律放行不吞（msg={msg:#x}）");
            }
        });
        assert!(sink.snapshot().is_empty(), "Miss 不投递任何事件类");
    }

    #[cfg(windows)]
    #[test]
    fn qa_probe_hook_without_tls_ctx_and_negative_code_pass_through() {
        // TLS 未注入（钩子线程未初始化/已清理）→ 放行、不投递、不 panic。
        HOOK_CTX.with(|cell| *cell.borrow_mut() = None);
        assert_ne!(qa_fire(WM_LBUTTONDOWN, 1, 2, 0).0, 1, "无 ctx 放行");

        // nCode < 0 规范性放行：不读 TLS、不投递。
        let sink = Arc::new(QaRecordingSink { events: Mutex::new(Vec::new()) });
        let hit: Arc<dyn HitTest> = Arc::new(QaFixedOutcome(HitOutcome::Hit));
        qa_with_ctx(hit, Arc::clone(&sink) as Arc<dyn HookSink>, || {
            let r = unsafe { mouse_hook_proc(-1, WPARAM(WM_LBUTTONDOWN as usize), LPARAM(0)) };
            assert_ne!(r.0, 1, "nCode<0 放行");
        });
        assert!(sink.snapshot().is_empty(), "nCode<0 不投递");
    }

    #[cfg(windows)]
    #[test]
    fn qa_probe_hook_bbox_only_impl_zero_regression_via_default_outcome() {
        // PetBBoxHandle 不覆写 outcome（默认二态退化 contains?Hit:Miss）：
        // S3-M1「过粗筛按钮吞+投递」零回归（卡片「切换 HitSource 实现行为一致」回退面）。
        let sink = Arc::new(QaRecordingSink { events: Mutex::new(Vec::new()) });
        let hit: Arc<dyn HitTest> = Arc::new(FixedRect(true));
        qa_with_ctx(hit, Arc::clone(&sink) as Arc<dyn HookSink>, || {
            assert_eq!(qa_fire(WM_LBUTTONDOWN, 3, 4, 0).0, 1, "二态实现按钮命中吞+投递（S3-M1 不回归）");
        });
        assert_eq!(sink.snapshot().len(), 1, "二态实现投递 Down");
    }

    /// **QA 探针（清单#1 / 裁定5）**：按钮/滚轮 + Hover 必须放行（不吞不投递，
    /// 「点击落主判定」）。
    ///
    /// 修复验收（QA 报告 Bug#1 闭环）：`mouse_hook_proc` 已按 `HitTest::outcome`
    /// 三态路由——按钮/滚轮 + Hover 放行不投递，仅 Hit 吞+投递；本探针为常驻
    /// 回归用例（曾于 commit 20ebe3f 以 `#[ignore]` 标注，修复后移除）。
    #[cfg(windows)]
    #[test]
    fn qa_probe_hook_button_and_wheel_hover_must_pass_through() {
        for (msg, data, name) in
            [(WM_LBUTTONDOWN, 0u32, "左键 Down"), (WM_MOUSEWHEEL, 0x0078_0000, "滚轮")]
        {
            let sink = Arc::new(QaRecordingSink { events: Mutex::new(Vec::new()) });
            let hit: Arc<dyn HitTest> = Arc::new(QaFixedOutcome(HitOutcome::Hover));
            qa_with_ctx(hit, Arc::clone(&sink) as Arc<dyn HookSink>, || {
                let r = qa_fire(msg, 5, 6, data);
                assert_ne!(r.0, 1, "{name}+Hover → 放行不吞（裁定5：点击落主判定）");
            });
            assert!(
                sink.snapshot().is_empty(),
                "{name}+Hover → 不投递（悬停有效、点击落主判定）"
            );
        }
    }
}
