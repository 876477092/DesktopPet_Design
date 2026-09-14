//! 感知服务（S2-M7，T-07；03 台账 §2 S2-M7 卡片）：光标 / 时间 / 系统 / 窗口枚举的内核侧类型与端口。
//!
//! 职责（对照 `02 §1.4` / `02 §4.4` / `01 §6.6` FR-6-1~4、`01 §6.10` FR-10-1）：
//!   - **事件总线**（[`bus`]）：perception → core 的 `bounded(64)` 有界队列，
//!     最新值优先、满则丢弃最旧（`02 §1.4`）；
//!   - **时间服务**（[`time`]）：[`time::WallClock`] 端口——C3 时间纪律下 dp-core 内
//!     唯一允许读真实墙钟的位置（`02 §4.4` / `02 §5.14`），附时段判定（本阶段 4 段，
//!     S7-M1 扩 6 段）；
//!   - **番茄钟**（[`pomodoro`]）：25/5min 工作-休息循环（FR-10-1，QI-03 并入），
//!     纯逻辑状态机：仅计时与事件（上层经 `pet://activity` 域广播），不做业务联动；
//!   - **共享类型**：[`PerceptionEvent`] 感知事件与 [`SystemSample`] / [`WindowBand`]
//!     等载荷类型。
//!
//! 架构硬约束：
//!   - **频率口径（`02 §1.4`）**：光标 20Hz（FR-6-1）/ 窗口枚举 0.5Hz = 2s 周期
//!     （FR-6-4）/ 系统 0.1Hz（FR-6-3）/ 前台全屏 2s 轮询（K-1）——轮询节拍由上层
//!     装配（S7-M3），本模块只定义事件载体与可调用的采样/状态机能力；
//!   - **坐标系（C6/RV-17）**：事件载荷一律 VDC（96-dpi 逻辑像素，可为负）；物理
//!     坐标由上层经 dp-platform `DisplayService::physical_to_vdc` 换算后注入；
//!   - **时间纪律（C3）**：除 [`time::SystemWallClock`]（墙钟端口的唯一实装）外本
//!     模块零时钟，全部计时由调用方注入 `now_ms: i64`，且一律**绝对时间锚定**
//!     （end_ms = 段起点 + 段长，严禁逐 tick 累加）；
//!   - **单次会话边界**：只做采样数据类型与端口；perception 线程创建与轮询循环归
//!     上层（S7-M3 装配），前台进程 hash 归 S7-M1，业务联动（情绪/需求/动作提交）
//!     一律不在本模块发生。

pub mod bus;
pub mod pomodoro;
pub mod time;

pub use bus::{PerceptionBus, PERCEPTION_CHANNEL_CAP};
pub use pomodoro::{Pomodoro, PomodoroCfg, PomodoroEvent, PomodoroPhase};
pub use time::{segment_of_hour, FakeWallClock, SystemWallClock, TimeSegment, WallClock};

use crate::motion::Vec2;

/// 感知事件（perception 线程 → core，频率口径 `02 §1.4`；载荷均为 VDC 或无坐标数据）。
#[derive(Clone, Debug, PartialEq)]
pub enum PerceptionEvent {
    /// 光标采样（20Hz，FR-6-1；VDC 坐标由上层经 dp-platform 换算注入）。
    Cursor {
        /// 光标位置（VDC）。
        pos: Vec2,
    },
    /// 窗口枚举（0.5Hz = 2s 周期，FR-6-4；标题栏 / 任务栏水平带，VDC）。
    Windows {
        /// 标题栏带列表（VDC）。
        titlebars: Vec<WindowBand>,
        /// 任务栏带列表（VDC）。
        taskbars: Vec<WindowBand>,
    },
    /// 系统状态（0.1Hz，FR-6-3）。
    System(SystemSample),
    /// 前台全屏状态（2s 轮询，K-1 三重校验结果）。
    Fullscreen {
        /// 是否处于前台全屏。
        on: bool,
        /// 全屏所在显示器（dp-platform `MonitorId(u64)` 原始值；未知为 `None`）。
        monitor_id: Option<u64>,
    },
}

/// 标题栏 / 任务栏水平带（VDC；`top` 为顶面 y）。
///
/// 独立轻量结构：S2-M6 `dp-core::motion::platform` 的 `PlatformBand` 为其消费侧
/// 对等类型，上层装配时按字段转换（并行开发解耦，字段同名直转）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowBand {
    /// 左边界（VDC，含）。
    pub left: f32,
    /// 顶面 y（VDC；宠物可站立的高度基准）。
    pub top: f32,
    /// 右边界（VDC，含）。
    pub right: f32,
}

/// 系统状态采样（FR-6-3：电量 / 负载 / 在场空闲；`None` = 该项本次不可用）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SystemSample {
    /// 电量状态（无电池 / 读取失败 / 百分比未知为 `None`）。
    pub battery: Option<BatteryState>,
    /// CPU 负载（0.0~1.0；首次采样无基线为 `None`）。
    pub cpu_load: Option<f32>,
    /// 键鼠空闲时长（`GetLastInputInfo` 口径；读取失败为 `None`）。
    pub presence_idle_ms: Option<u64>,
}

/// 电量状态（FR-6-3）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatteryState {
    /// 剩余电量百分比（0~100）。
    pub percent: u8,
    /// 是否接入外接电源（AC 在线）。
    pub charging: bool,
}
