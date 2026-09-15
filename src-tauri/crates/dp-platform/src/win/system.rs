//! 系统状态感知实装（S2-M7，T-07 / FR-6-3）：电量 / CPU 负载 / 在场空闲采样。
//!
//! 职责（`01 §6.6` FR-6-3 / `02 §1.4` / 03 台账 S2-M7 卡片）：
//!   - [`battery_status`]：`GetSystemPowerStatus` 电量（AC 在线 = charging；
//!     百分比未知 255 → `None`）；
//!   - [`CpuLoadSampler`]：`GetSystemTimes` 两次采样差分求负载（首次无基线 →
//!     `None`；时间未前进 → `None` 并重置基线），差分归一为纯函数
//!     [`load_from_deltas`]（单测覆盖）；
//!   - [`last_input_idle_ms`]：`GetLastInputInfo` 键鼠空闲时长（`GetTickCount`
//!     口径 u32 回绕以 `wrapping_sub` 处理）。
//!
//! 边界：采样失败一律返回 `None`、跳过不重试（`02 §1.4`）；**前台进程 hash 归
//! S7-M1**（本模块不做）；采样节拍（0.1Hz）由上层装配（S7-M3）。[`BatteryState`]
//! 在本 crate 独立定义（dp-platform 不依赖 dp-core），上层装配时按字段转换为
//! `dp_core::perception::BatteryState`（字段同名直转）。

/// 电量状态（`GetSystemPowerStatus` 映射）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryState {
    /// 剩余电量百分比（0~100；未知 255 已在采样层滤为 `None`）。
    pub percent: u8,
    /// 是否接入外接电源（`ACLineStatus == 1`，AC 在线）。
    pub charging: bool,
}

/// 电量采样：AC 在线 = charging；百分比 > 100（255 = 未知）返回 `None`。
pub fn battery_status() -> Option<BatteryState> {
    imp::battery_status()
}

/// CPU 负载采样器（`GetSystemTimes` 两次采样差分；**首次采样返回 `None`** 以建立基线）。
///
/// 内部为 100ns 为单位的 FILETIME 计数差分；计数回绕按 `wrapping_sub` 处理。
#[derive(Clone, Copy, Debug)]
pub struct CpuLoadSampler {
    /// 上次采样 idle 计数（100ns 单位）。
    prev_idle: u64,
    /// 上次采样总量计数（kernel + user，100ns 单位）。
    prev_total: u64,
    /// 是否已建立基线（首次采样仅建立基线）。
    has_prev: bool,
}

impl CpuLoadSampler {
    /// 构造空采样器（无基线；首次 `sample` 仅建立基线并返回 `None`）。
    #[must_use]
    pub fn new() -> Self {
        Self { prev_idle: 0, prev_total: 0, has_prev: false }
    }

    /// 采样 CPU 负载（0.0~1.0）。
    ///
    /// 首次调用返回 `None`（建立基线）；时间未前进（总量无增长）返回 `None`
    /// 并以当前值重置基线；其余返回差分负载或差分非法（防御）时的 `None`。
    pub fn sample(&mut self) -> Option<f32> {
        let (idle, total) = imp::system_times()?;
        if !self.has_prev {
            self.prev_idle = idle;
            self.prev_total = total;
            self.has_prev = true;
            return None;
        }
        // 100ns 计数为回绕计数器：差分一律 wrapping_sub（模运算保证回绕差正确），
        // 异常差分交由 load_from_deltas 防御。
        let idle_d = idle.wrapping_sub(self.prev_idle);
        let total_d = total.wrapping_sub(self.prev_total);
        self.prev_idle = idle;
        self.prev_total = total;
        load_from_deltas(idle_d, total_d)
    }
}

impl Default for CpuLoadSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// 差分 → CPU 负载纯函数（单测入口）：`load = 1 - idle_d / total_d`。
///
/// 防御：`total_d == 0`（时间未前进）或 `idle_d > total_d`（差分非法）返回 `None`。
#[must_use]
pub fn load_from_deltas(idle_d: u64, total_d: u64) -> Option<f32> {
    if total_d == 0 || idle_d > total_d {
        return None;
    }
    Some(1.0 - idle_d as f32 / total_d as f32)
}

/// 键鼠空闲时长（`GetLastInputInfo`；`GetTickCount` 口径 u32 回绕用 `wrapping_sub`）。
pub fn last_input_idle_ms() -> Option<u64> {
    imp::last_input_idle_ms()
}

/// 当前进程内存占用（PrivateUsage，字节；`GetProcessMemoryInfo`）。
///
/// **S6-M1（T-16 段 · 上）**：`metrics` 的 `mem` 真源 + `check-mem.ps1` 同口径
/// （AC-40 峰值 ≤209MB / 红线 250MB / 告警 200MB，`02 §10.2 R12`）。
/// 失败（API 不可用）返回 `None`，由上层按「未知」跳过（`02 §7.4.2`）。
pub fn process_memory_bytes() -> Option<u64> {
    imp::process_memory_bytes()
}

/// 当前进程 CPU 占用采样器（`GetProcessTimes` 两次差分；**首次采样返回 `None`** 建基线）。
///
/// **S6-M1（T-16 段 · 上）**：输出为**单核归一百分比**（如任务管理器「CPU」列）——
/// `proc_time_delta / (墙钟差分 × 逻辑核心数) × 100`，与 AC-01「空闲 ≤3%、动画 ≤8%」
/// 同口径（0.0 为 0%，1.0 为占满 1 核）。差分以 100ns FILETIME 计数 `wrapping_sub` 处理回绕。
#[derive(Clone, Copy, Debug)]
pub struct ProcCpuSampler {
    /// 上次采样进程（kernel + user）计数（100ns）。
    prev_proc: u64,
    /// 上次采样墙钟（Instant，单调）。
    prev_wall: std::time::Instant,
    /// 逻辑核心数（`available_parallelism` 兜底 1）。
    cores: u32,
    /// 是否已建立基线。
    has_prev: bool,
}

impl ProcCpuSampler {
    /// 构造空采样器（无基线；首次 `sample` 仅建立基线并返回 `None`）。
    #[must_use]
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1)
            .max(1);
        Self { prev_proc: 0, prev_wall: std::time::Instant::now(), cores, has_prev: false }
    }

    /// 采样进程 CPU 占用（%，单核归一）。
    ///
    /// 首次调用返回 `None`（建立基线）；进程时间未前进（本次采样与上次零间隔）
    /// 返回 `None` 并重置基线；其余返回差分 CPU 或差分非法（防御）时的 `None`。
    pub fn sample(&mut self) -> Option<f32> {
        let proc = imp::process_times_100ns()?;
        let wall = std::time::Instant::now();
        if !self.has_prev {
            self.prev_proc = proc;
            self.prev_wall = wall;
            self.has_prev = true;
            return None;
        }
        let proc_d = proc.wrapping_sub(self.prev_proc);
        let wall_d = wall.saturating_duration_since(self.prev_wall);
        self.prev_proc = proc;
        self.prev_wall = wall;
        process_load_from_deltas(proc_d, wall_d, self.cores)
    }
}

impl Default for ProcCpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// 进程差分 → CPU 占用纯函数（单测入口）：
/// `proc_100ns / (wall_ns / 100 × cores) × 100`。
///
/// 防御：`wall` 为零 / `proc_d == 0` 时返回 `None`（无有效差分）。
#[must_use]
pub fn process_load_from_deltas(proc_100ns: u64, wall: std::time::Duration, cores: u32) -> Option<f32> {
    let wall_100ns = wall.as_nanos() as u64 / 100;
    if wall_100ns == 0 || proc_100ns == 0 {
        return None;
    }
    // 进程时间可能 > 墙钟×核数（多线程满载瞬时）→ 钳到 100%（单核口径上限）。
    let raw = proc_100ns as f64 / (wall_100ns as f64 * cores.max(1) as f64) * 100.0;
    Some(raw.min(100.0) as f32)
}

// ---------------------------------------------------------------------------
// Windows 实装
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    // 注意：`GetSystemTimes` 在 windows 0.61 实装于 `Win32::System::Threading`
    // （非 SystemInformation）。该 feature **已在 workspace `Cargo.toml` 的
    // windows features 最小集中显式声明**（并在依赖理由注释中登记了
    // 「API 位于 Threading 而非 SystemInformation」这一坑）——**勿删**，
    // 否则一旦脱离 tao 的 feature unification 传递、本模块即编译失败。
    // （历史注释曾写「未列入最小集、由 tao 传递启用」，与现状不符，已订正。）
    use windows::Win32::System::Threading::GetSystemTimes;
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

    use super::BatteryState;

    /// 电量采样。失败（API 失败 / 百分比未知 255）返回 `None`。
    pub(super) fn battery_status() -> Option<BatteryState> {
        let mut status = SYSTEM_POWER_STATUS::default();
        // Safety：`status` 为本函数持有的合法输出缓冲；`GetSystemPowerStatus`
        // 仅写入该缓冲，失败（Err）时内容不可信、按 `None` 跳过。
        unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
        // BatteryLifePercent 0~100，255 = 未知 → None（口径：>100 视为不可用）。
        if status.BatteryLifePercent > 100 {
            return None;
        }
        // ACLineStatus：1 = AC 在线（0 离线 / 255 未知，均按未充电处理）。
        Some(BatteryState {
            percent: status.BatteryLifePercent,
            charging: status.ACLineStatus == 1,
        })
    }

    /// `GetSystemTimes` 采样值（100ns 计数）：`(idle, kernel + user 总量)`。
    ///
    /// kernel 计数已包含 idle，故总量恒 ≥ idle；异常差分由 `load_from_deltas` 防御。
    pub(super) fn system_times() -> Option<(u64, u64)> {
        let mut idle = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let idle_ptr = &mut idle as *mut FILETIME;
        let kernel_ptr = &mut kernel as *mut FILETIME;
        let user_ptr = &mut user as *mut FILETIME;
        // Safety：三个 FILETIME 均为本函数持有的合法输出缓冲；`GetSystemTimes`
        // 成功后填充，失败（Err）时内容不可信、按 `None` 跳过。
        unsafe { GetSystemTimes(Some(idle_ptr), Some(kernel_ptr), Some(user_ptr)) }.ok()?;
        Some((
            filetime_u64(&idle),
            filetime_u64(&kernel).wrapping_add(filetime_u64(&user)),
        ))
    }

    /// 当前进程内存占用（PrivateUsage，字节）。
    ///
    /// **S6-M1**：`GetProcessMemoryInfo`（Psapi，`Win32_System_ProcessStatus` feature
    /// 已在 workspace `Cargo.toml` 最小集中显式登记，见依赖理由注释）。
    pub(super) fn process_memory_bytes() -> Option<u64> {
        use windows::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;
        // `PROCESS_MEMORY_COUNTERS_EX`（扩展结构，基结构为其前缀）才含 `PrivateUsage`
        // （进程私有提交内存，AC-40 口径）。SDK 层面 `GetProcessMemoryInfo` 以 cb
        // 决定填充量，EX 变体可经指针转换传入（基结构前缀兼容，布局合法）。
        let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
        let cb = core::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        // Safety：`counters` 为本函数持有的合法输出缓冲，cb 按 EX 尺寸填充；
        // `GetCurrentProcess` 返回伪句柄，无需关闭。
        let ptr = &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS;
        unsafe { GetProcessMemoryInfo(GetCurrentProcess(), ptr, cb) }.ok()?;
        Some(counters.PrivateUsage as u64)
    }

    /// 当前进程 CPU 时间（kernel + user，100ns 计数；`GetProcessTimes`）。
    pub(super) fn process_times_100ns() -> Option<u64> {
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let (c_ptr, e_ptr, k_ptr, u_ptr) =
            (&mut creation, &mut exit, &mut kernel, &mut user);
        // Safety：四个 FILETIME 均为本函数持有的合法输出缓冲；失败（Err）按 `None` 跳过。
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                c_ptr,
                e_ptr,
                k_ptr,
                u_ptr,
            )
        }
        .ok()?;
        Some(filetime_u64(&kernel).wrapping_add(filetime_u64(&user)))
    }

    /// FILETIME 高低位拼合 → u64（100ns 计数）。
    fn filetime_u64(ft: &FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    }

    /// 键鼠空闲时长（ms）。`dwTime` 为最后输入时刻（`GetTickCount` 口径）。
    pub(super) fn last_input_idle_ms() -> Option<u64> {
        let mut info = LASTINPUTINFO {
            cbSize: core::mem::size_of::<LASTINPUTINFO>() as u32,
            ..Default::default()
        };
        // Safety：`info` 为本函数持有的合法输出缓冲且 cbSize 已按 SDK 规定填充。
        if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
            return None;
        }
        // Safety：`GetTickCount` 无参数、无前置条件。
        let now = unsafe { GetTickCount() };
        // u32 回绕：wrapping_sub 后差值即空闲 ms（跨回绕仍正确）。
        Some(now.wrapping_sub(info.dwTime) as u64)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::BatteryState;

    /// 非 Windows 目标不提供系统采样（本项目仅 Windows；`win` 模块整体 cfg 门控）。
    pub(super) fn battery_status() -> Option<BatteryState> {
        None
    }

    /// 非 Windows 目标不提供系统采样。
    pub(super) fn system_times() -> Option<(u64, u64)> {
        None
    }

    /// 非 Windows 目标不提供系统采样。
    pub(super) fn last_input_idle_ms() -> Option<u64> {
        None
    }

    /// 非 Windows 目标不提供进程内存采样。
    pub(super) fn process_memory_bytes() -> Option<u64> {
        None
    }

    /// 非 Windows 目标不提供进程 CPU 采样。
    pub(super) fn process_times_100ns() -> Option<u64> {
        None
    }
}

// ---------------------------------------------------------------------------
// 单元测试（负载差分纯函数；Win32 直调不做真机断言，真机矩阵归 B7）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_from_deltas_normal_quarter_load() {
        // idle 300 / total 400 → 负载 0.25。
        let load = load_from_deltas(300, 400).unwrap();
        assert!((load - 0.25).abs() < 1e-6);
    }

    #[test]
    fn load_from_deltas_idle_exceeds_total_is_none() {
        assert!(load_from_deltas(401, 400).is_none(), "idle_d > total_d 防御");
    }

    #[test]
    fn load_from_deltas_zero_total_is_none() {
        assert!(load_from_deltas(0, 0).is_none(), "时间未前进防御");
    }

    #[test]
    fn load_from_deltas_all_idle_is_zero_load() {
        let load = load_from_deltas(400, 400).unwrap();
        assert!(load.abs() < 1e-6);
    }

    #[test]
    fn load_from_deltas_all_busy_is_full_load() {
        let load = load_from_deltas(0, 400).unwrap();
        assert!((load - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cpu_sampler_new_starts_without_baseline() {
        let sampler = CpuLoadSampler::new();
        assert!(!sampler.has_prev, "构造后无基线（首次 sample 仅建立基线）");
        assert_eq!(sampler.prev_idle, 0);
        assert_eq!(sampler.prev_total, 0);
    }

    // -----------------------------------------------------------------------
    // S6-M1：进程 CPU 差分纯函数（`process_load_from_deltas`）
    // -----------------------------------------------------------------------

    #[test]
    fn process_load_one_core_full_uses_one_core() {
        // 1 核：100ms 墙钟内进程消耗 100ms → 100%。
        let load = process_load_from_deltas(1_000_000, std::time::Duration::from_millis(100), 1)
            .unwrap();
        assert!((load - 100.0).abs() < 1e-3);
    }

    #[test]
    fn process_load_normalized_by_cores() {
        // 4 核：100ms 墙钟内进程消耗 100ms → 25%。
        let load = process_load_from_deltas(1_000_000, std::time::Duration::from_millis(100), 4)
            .unwrap();
        assert!((load - 25.0).abs() < 1e-3);
    }

    #[test]
    fn process_load_clamped_at_100_percent() {
        // 1 核：100ms 墙钟内进程消耗 200ms（多线程瞬时）→ 钳到 100%。
        let load = process_load_from_deltas(2_000_000, std::time::Duration::from_millis(100), 1)
            .unwrap();
        assert!((load - 100.0).abs() < 1e-3);
    }

    #[test]
    fn process_load_zero_wall_is_none() {
        assert!(process_load_from_deltas(1_000_000, std::time::Duration::ZERO, 1).is_none());
        assert!(process_load_from_deltas(0, std::time::Duration::from_millis(100), 1).is_none());
    }

    #[test]
    fn proc_cpu_sampler_new_starts_without_baseline() {
        let sampler = ProcCpuSampler::new();
        assert!(!sampler.has_prev, "构造后无基线");
        assert!(sampler.cores >= 1);
    }
}
