//! S3-M1 真机探针（`#[ignore]`）：低阶鼠标钩子高频压测（`02 §5.24 L-04`）。
//!
//! 目的（对应 `02 §5.24 L-04` 四指标中的真机部分）：
//!   - ③ **无 `LowLevelHooksTimeout` 卸载**：高频合成后断言 `is_installed()` 仍为真；
//!   - ④ **事件不丢失**：报告 sink 投递数 / 丢弃数（本探针 sink 不丢，恒 0）；
//!   - ①② CPU / 回调延迟 P99 由**外部工具 + 人工真机步骤**采集（见 [`print_manual_steps`]）。
//!
//! 纪律（照 `qa_s1m2_extra.rs` 范式）：
//!   - **早退 ≠ 通过**：任何环境阻塞（无桌面会话 / 钩子未装上 / 合成无效）一律打印
//!     `[SKIPPED]` 并提前返回——**不得伪造数据**；
//!   - 默认 `#[ignore]`：`cargo test` 不跑；真机经 `cargo test -p dp-platform -- --ignored` 触发。
//!
//! 本文件仅测试代码，不改任何生产代码。

#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dp_platform::win::hook::{HitTest, HookEvent, HookService, HookSink};

/// 目标合成频率（次 / 秒，`02 §5.24 L-04` 口径 1000）。
const TARGET_RATE_PER_SEC: u32 = 1_000;

/// 目标持续时长（秒，`02 §5.24 L-04` 口径 60）；经环境变量 `DP_HOOK_PROBE_SECS` 可缩短。
const DEFAULT_DURATION_SECS: u32 = 60;

/// 恒命中：强制投递（把 sink 压到极限，检验「无卸载 / 无丢失」）。
struct AlwaysHit;

impl HitTest for AlwaysHit {
    fn contains(&self, _x: i32, _y: i32) -> bool {
        true
    }
}

/// 计数 sink（回调线程调用；仅原子自增，零分配）。
struct CountSink {
    count: AtomicU64,
}

impl CountSink {
    fn new() -> Self {
        Self { count: AtomicU64::new(0) }
    }

    fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}

impl HookSink for CountSink {
    fn on_event(&self, _ev: HookEvent) {
        self.count.fetch_add(1, Ordering::Relaxed);
    }
}

/// 最小 `user32` FFI（仅测试用，不进入产品路径）。
mod ffi {
    use std::ffi::c_void;
    use std::os::raw::{c_int, c_uint};

    pub type Hwnd = *mut c_void;

    #[link(name = "user32")]
    extern "system" {
        pub fn CreateWindowExW(
            ex_style: c_uint,
            class: *const u16,
            name: *const u16,
            style: c_uint,
            x: c_int,
            y: c_int,
            w: c_int,
            h: c_int,
            parent: Hwnd,
            menu: Hwnd,
            instance: Hwnd,
            param: *mut c_void,
        ) -> Hwnd;
        pub fn DestroyWindow(hwnd: Hwnd) -> c_int;
        /// 合成鼠标输入（本测试仅用 MOVE；`dx/dy` 为相对位移，按 `LONG` 解释）。
        pub fn mouse_event(dwflags: c_uint, dx: c_uint, dy: c_uint, data: c_uint, extra: usize);
    }

    pub const WS_VISIBLE: c_uint = 0x1000_0000;
    pub const MOUSEEVENTF_MOVE: c_uint = 0x0001;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 临时原生窗口守卫（Drop 销毁）；创建失败返回 `None`（无桌面会话 → 跳过）。
struct TempWindow(ffi::Hwnd);

impl TempWindow {
    fn new() -> Option<Self> {
        let class = wide("STATIC");
        // 不变量：CreateWindowExW 各句柄参数允许为空（顶层无父窗口）。
        let hwnd = unsafe {
            ffi::CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                ffi::WS_VISIBLE,
                0,
                0,
                120,
                120,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if hwnd.is_null() {
            None
        } else {
            Some(TempWindow(hwnd))
        }
    }
}

impl Drop for TempWindow {
    fn drop(&mut self) {
        unsafe {
            ffi::DestroyWindow(self.0);
        }
    }
}

/// 按目标频率合成相对鼠标移动 `secs` 秒，返回合成次数。
///
/// 用 ±1 交错位移（净漂移 0，避免光标跑出屏幕）；每 `chunk` 次 `sleep` 一次近似节流。
fn synthesize_mouse_flood(rate_per_sec: u32, secs: u32) -> u64 {
    let chunk = 20u32;
    let chunks_per_sec = (rate_per_sec / chunk).max(1);
    let mut sent: u64 = 0;
    for _ in 0..secs {
        for _ in 0..chunks_per_sec {
            for i in 0..chunk {
                // ±1 交错：i 偶数 +1、奇数 -1（-1 以 u32 补码传，内核按 LONG 解释）。
                let dx: u32 = if i % 2 == 0 { 1 } else { (-1i32) as u32 };
                unsafe { ffi::mouse_event(ffi::MOUSEEVENTF_MOVE, dx, 0, 0, 0) };
                sent += 1;
            }
            // ~1000/s：chunk 次后睡 1ms（受 Windows 定时器粒度影响，仅近似节流）。
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    sent
}

/// 打印人工真机步骤（沙箱 / CI 不可自动化时使用；不得据此计入验收）。
fn print_manual_steps() {
    eprintln!("---- S3-M1 真实机验收人工步骤（`02 §5.24 L-04`） ----");
    eprintln!("1) 在**交互式桌面会话**下运行：");
    eprintln!("   cargo test -p dp-platform --test qa_s3m1_hook_probe -- --ignored --nocapture");
    eprintln!("   （可选：set DP_HOOK_PROBE_SECS=60 以固定 60s；不设亦默认 60s）");
    eprintln!("2) 钩子 CPU 增量（<1.5%，目标 <0.8%）：任务管理器 / Process Explorer 观察 DesktopPet 进程");
    eprintln!("   在压测期间相对空闲基线的增量；或扩展后续 `check-mem.ps1`（归 S6-M1）。");
    eprintln!("3) 回调延迟 P99 <50μs、max <200μs：需带 `QueryPerformanceCounter` 插桩的专用构建，");
    eprintln!("   或以 ETW / WPA 采集 `dp-hook` 线程的 CPU 采样；本探针的合成吞吐可作平均开销上界。");
    eprintln!("4) AC-10（穿透点击穿透 / 关闭恢复）：托盘切穿透 → 后窗可点；切回 → 宠物可点。");
    eprintln!("5) 全程确认无 `LowLevelHooksTimeout` 卸载（本探针以 `is_installed()` 断言在线）。");
    eprintln!("----------------------------------------------------------");
}

/// 读取时长（环境变量覆盖，便于快速冒烟；默认 60s）。
fn probe_duration_secs() -> u32 {
    std::env::var("DP_HOOK_PROBE_SECS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_DURATION_SECS)
}

#[test]
#[ignore = "真机探针：需交互式桌面会话与高频合成鼠标；CI / 沙箱会 [SKIPPED] 早退"]
fn probe_lowlevel_hook_under_high_frequency_mouse() {
    // 环境守卫：无交互式桌面会话 → 明确跳过（不得伪造数据）。
    let Some(win) = TempWindow::new() else {
        eprintln!("[SKIPPED] S3-M1 探针：无法创建原生窗口（无交互式桌面会话？）→ 真机高频压测未执行");
        print_manual_steps();
        return;
    };

    let sink = Arc::new(CountSink::new());
    // 保持 `CountSink` 具名句柄以读计数；注入钩子的用 `dyn HookSink`（显式 unsize）。
    let sink_for_hook: Arc<dyn HookSink> = sink.clone();
    // 两 gate 均 false（默认模式：不穿透、非全屏）→ 构造即装钩子。
    let svc = HookService::new(Arc::new(AlwaysHit), sink_for_hook, false, false);
    if !svc.is_installed() {
        eprintln!("[SKIPPED] S3-M1 探针：SetWindowsHookExW 未成功安装 → 真机高频压测未执行");
        svc.shutdown();
        drop(win);
        print_manual_steps();
        return;
    }

    let secs = probe_duration_secs();
    eprintln!("[S3-M1 探针] 开始：目标 {TARGET_RATE_PER_SEC}/s × {secs}s（钩子在线）");
    let start = Instant::now();
    let sent = synthesize_mouse_flood(TARGET_RATE_PER_SEC, secs);
    let elapsed = start.elapsed();
    let delivered = sink.count();

    let secs_f = elapsed.as_secs_f64().max(f64::EPSILON);
    let avg_us = elapsed.as_micros() as f64 / delivered.max(1) as f64;
    eprintln!(
        "[S3-M1 探针] 完成：合成 {sent} 次 / 投递 {delivered} 次 / 用时 {:.2}s / 投递速率 {:.0}/s / 平均回调开销上界 {:.2}μs",
        elapsed.as_secs_f64(),
        delivered as f64 / secs_f,
        avg_us
    );

    // 合成无效（本环境 mouse_event 未产生 LL 回调）→ 明确跳过，不得据「未卸载」误判通过。
    if delivered == 0 {
        eprintln!("[SKIPPED] S3-M1 探针：合成鼠标未产生钩子回调（本环境不支持）→ 真机高频压测未执行");
        svc.shutdown();
        drop(win);
        print_manual_steps();
        return;
    }

    // ③ 无 LowLevelHooksTimeout 卸载：跑完仍在线。
    assert!(
        svc.is_installed(),
        "高频 {secs}s 后钩子应仍在线（无 LowLevelHooksTimeout 静默卸载）"
    );
    // ④ 事件不丢失（本探针 sink 无丢弃；有界队列丢弃计数由 dp-app ChannelSink 覆盖）。
    eprintln!("[S3-M1 探针] 断言通过：is_installed()=true（③ 无卸载）；投递 {delivered}（④ 见 ChannelSink 丢弃计数）");

    svc.shutdown();
    assert!(!svc.is_installed(), "shutdown() 后应已完全卸载");
    drop(win);
    print_manual_steps();
}
