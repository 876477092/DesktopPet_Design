//! 光标感知实装（S2-M7，T-07 / FR-6-1）：`GetCursorPos` 采样（20Hz 频率口径）。
//!
//! 职责（`01 §6.6` FR-6-1 / `02 §1.4` / 03 台账 S2-M7 卡片）：
//!   - [`read_cursor_physical`]：物理屏幕坐标采样（虚拟桌面物理像素，含负坐标副屏）；
//!   - [`read_cursor_vdc`]：采样并经 [`DisplayService`] 换算为 VDC（96-dpi 逻辑像素）；
//!   - [`cursor_speed_px_per_sec`]：两采样点间的光标速度纯函数（px/s，单测覆盖）。
//!
//! 边界：**轮询节拍（20Hz）由上层装配**（S7-M3 perception 线程），本模块只提供
//! 可调用采样函数；采样失败一律返回 `None`、跳过不重试（`02 §1.4`），调用方据
//! 空值自行降级。

use super::display::DisplayService;
use crate::traits::Vec2;

/// `GetCursorPos` 采样（虚拟桌面物理像素坐标；含负坐标副屏）。
///
/// 失败返回 `None`（采样失败跳过不重试，`02 §1.4`）。
pub fn read_cursor_physical() -> Option<(i32, i32)> {
    imp::read_cursor_physical()
}

/// 采样光标并经 `DisplayService` 换算为 VDC（96-dpi 逻辑像素）。
#[must_use]
pub fn read_cursor_vdc(display: &DisplayService) -> Option<Vec2> {
    let (x, y) = read_cursor_physical()?;
    Some(display.physical_to_vdc(x, y))
}

/// 两采样点间的光标速度（px/s；坐标口径由调用方保证，建议 VDC）。
///
/// `dt <= 0`（时间未前进 / 倒流）返回 `None`；两点重合返回 `0.0`（静止）。
#[must_use]
pub fn cursor_speed_px_per_sec(prev: Vec2, prev_ms: i64, cur: Vec2, cur_ms: i64) -> Option<f32> {
    // saturating_sub：任意 i64 入参组合（含 i64::MIN / i64::MAX）都不在
    // debug 构建下溢出 panic，与 arbiter.rs / window.rs 的饱和口径一致。
    let dt_ms = cur_ms.saturating_sub(prev_ms);
    if dt_ms <= 0 {
        return None;
    }
    let dist = (cur - prev).length();
    Some(dist / (dt_ms as f32 / 1000.0))
}

// ---------------------------------------------------------------------------
// Windows 实装
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    /// `GetCursorPos` 采样。失败（API 返回 Err）返回 `None`，跳过不重试。
    pub(super) fn read_cursor_physical() -> Option<(i32, i32)> {
        let mut pt = POINT::default();
        // Safety：`pt` 为本函数持有的合法输出缓冲；`GetCursorPos` 仅写入该缓冲，
        // 失败（Err）时缓冲内容不可信、按 `None` 跳过。
        unsafe { GetCursorPos(&mut pt) }.ok()?;
        Some((pt.x, pt.y))
    }
}

#[cfg(not(windows))]
mod imp {
    /// 非 Windows 目标不提供光标采样（本项目仅 Windows；`win` 模块整体 cfg 门控）。
    pub(super) fn read_cursor_physical() -> Option<(i32, i32)> {
        None
    }
}

// ---------------------------------------------------------------------------
// 单元测试（速度纯函数；Win32 直调不做真机断言，真机矩阵归 B7）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_speed_normal_computes_px_per_sec() {
        // 3-4-5 直角三角形：位移 500px / 2s = 250 px/s。
        let speed = cursor_speed_px_per_sec(Vec2::ZERO, 1_000, Vec2::new(300.0, 400.0), 3_000);
        assert!((speed.unwrap() - 250.0).abs() < 1e-4);
    }

    #[test]
    fn cursor_speed_zero_dt_is_none() {
        assert!(cursor_speed_px_per_sec(Vec2::ZERO, 1_000, Vec2::new(1.0, 1.0), 1_000).is_none());
    }

    #[test]
    fn cursor_speed_negative_dt_is_none() {
        assert!(cursor_speed_px_per_sec(Vec2::new(5.0, 5.0), 2_000, Vec2::ZERO, 1_000).is_none());
    }

    #[test]
    fn cursor_speed_stationary_is_zero() {
        let p = Vec2::new(12.0, -34.0);
        let speed = cursor_speed_px_per_sec(p, 1_000, p, 1_600).unwrap();
        assert!(speed.abs() < 1e-6);
    }
}
