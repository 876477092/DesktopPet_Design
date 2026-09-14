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
}
