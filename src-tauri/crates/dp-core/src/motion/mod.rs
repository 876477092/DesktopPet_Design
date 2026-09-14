//! 运动模块（S2-M4 决策引擎 + S2-M5 物理，T-05）。
//!
//! 职责（03 台账 §2 S2-M4；对照 T-05、`01 §6.2` FR-2-1/2/5/6/7、`02 §5` K-3）：
//!   - **漫游决策**（[`roam`]）：每 5~30s 决策一次（按 `RoamCfg::pace` 缩放，
//!     pace 越大越活泼 → 间隔越短）；`decide_target` 最多 12 次采样：范围内
//!     随机取点 → 站立面校验 → 光标热区跳过 → 禁区跳过 → 全失败返回 `None`
//!     （上层回退 idle）；
//!   - **站立面端口**（[`roam::StandSurface`]）：最小端口 + `DesktopFloor`
//!     兜底实现（工作区底边行走）；S2-M6 PlatformGraph 将实现同一端口；
//!   - **光标热区**（[`avoid`]）：150px 热区 `contains` + tangent 绕行纯函数；
//!   - **跨屏**（AC-13，不瞬移）：目标在另一显示器时路径沿边缘插值——先水平/
//!     垂直走到两屏 VDC 交界（另一轴钳到重叠区间），再跨到目标屏；位置由
//!     `tick` 按 `walk_speed_px_per_sec` 逐步推进，绝无 SetWindowPos 跳变；
//!   - **显示器变更迁移**（FR-1-4）：引擎保存 NDC + monitorId 快照；当前屏
//!     还在 → NDC 重算 VDC；当前屏消失 → 2s 内（deadline = 变更时刻 +
//!     2000ms，绝对锚定）迁移到最近显示器工作区中心。
//!
//! 架构硬约束：
//!   - **坐标系（C6/RV-17）**：内核一律 VDC（96-dpi 逻辑像素，原点为虚拟桌面
//!     左上、可为负）。dp-core 不依赖 dp-platform：显示器几何以自有轻量
//!     [`roam::MonitorGeom`] 由调用方从 `dp_platform::win::display::MonitorInfo`
//!     换算注入（换算公式见其文档注释）；写窗口位置仍经 `MonitorInfo::
//!     {vdc_to_spc, vdc_to_physical}` / `DisplayService::monitor_at` 换算；
//!   - **时间纪律（C3）**：模块内零时钟——禁止 `SystemTime`/`Utc::now()`/
//!     `Instant`，所有时间由调用方注入单调毫秒 `now_ms: u64`。定时一律
//!     **绝对时间锚定**：deadline 链基于「上一 deadline + 间隔」，严禁以实际
//!     触发时刻累加（历史教训：过冲逐 tick 累加导致 AC 失败）；
//!   - **零新增依赖**：随机数用内置可播种 PRNG（splitmix64，u64 状态 →
//!     f32 [0,1)），种子由调用方注入，测试可复现；
//!   - **与 S2-M3 仲裁器解耦**：引擎只输出决策/移动状态与 [`engine::MotionEvent`]，
//!     不直接提交/打断动作；动作下发由上层驱动循环（S7-M3）统一走仲裁器；
//!   - **物理（S2-M5，[`physics`]）**：重力掉落 / 左右边界反弹 / 落地判定由
//!     独立 [`PhysicsEngine`] 实现（上层驱动循环 S7-M3 组合决策与物理）；
//!     重力单一真源 `InteractionCfg::gravityPxPerSec2`（RV-18），落地产出
//!     `MotionEvent::Landed` 交上层经仲裁器提交 ACT-M-06 落地缓冲；
//!   - **平台图**（[`platform`]，S2-M6）：每 2s（FR-6-4）重建四类站立平台节点
//!     （`DesktopBottom` / `Taskbar` / `WindowTitleBar` / `ScreenEdge`，`02 §5`
//!     K-3）并实现 [`roam::StandSurface`] 端口——落地 = 自上而下取 y 最小且
//!     水平包含者；标题栏枚举失败 → 仅桌底优雅降级（`02 §10` R2）；数据源在
//!     `dp-platform::win::winenum`，重建 deadline 链绝对锚定（C3）；
//!   - **边界登记**：PlatformGraph（窗口标题栏平台）属 S2-M6，本模块仅预留
//!     `StandSurface` 端口（物理落地判定亦经该端口，S2-M6 实现后自动兼容）；
//!     `BehaviorCfg::auto_roam` 开关门控归上层（引擎被驱动时即视为开启）。

pub mod avoid;
pub mod engine;
pub mod physics;
pub mod platform;
pub mod roam;

pub use avoid::{detour_direction, detour_waypoint, CursorHeat, DETOUR_MARGIN_PX};
pub use engine::{MotionEngine, MotionEvent, MIGRATION_DEADLINE_MS};
pub use physics::{HorizontalBounds, PhysicsEngine, SUB_STEP_MS, WALL_BOUNCE_RESTITUTION};
pub use platform::{
    Platform, PlatformBand, PlatformGraph, PlatformInputs, PlatformKind, REBUILD_INTERVAL_MS,
};
pub use roam::{
    decision_interval_ms, DesktopFloor, ForbiddenZone, MonitorGeom, RoamRegion, RoamSampler,
    StandSurface, SplitMix64, MAX_SAMPLES, STAND_EPS_PX,
};

/// 内核轻量二维向量（C6/RV-17：VDC 96-dpi 逻辑像素，可为负）。
///
/// dp-core 为纯逻辑内核、不依赖 dp-platform（见 crate 文档），故运动模块自带
/// 最小向量类型；dp-app 侧与 `dp_platform::traits::Vec2` 字段同名可直接互转。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    /// X 分量（VDC 逻辑像素）。
    pub x: f32,
    /// Y 分量（VDC 逻辑像素）。
    pub y: f32,
}

impl Vec2 {
    /// 零向量。
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    /// 构造向量。
    #[inline]
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// 分量缩放。
    #[inline]
    #[must_use]
    pub fn scale(self, k: f32) -> Vec2 {
        Vec2::new(self.x * k, self.y * k)
    }

    /// 欧氏长度。
    #[inline]
    #[must_use]
    pub fn length(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    /// 到另一点的欧氏距离。
    #[inline]
    #[must_use]
    pub fn distance(self, other: Vec2) -> f32 {
        (self - other).length()
    }

    /// 到另一点的欧氏距离平方（比较用，免开方）。
    #[inline]
    #[must_use]
    pub fn distance_sq(self, other: Vec2) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        dx * dx + dy * dy
    }
}

impl core::ops::Add for Vec2 {
    type Output = Vec2;

    #[inline]
    fn add(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl core::ops::Sub for Vec2 {
    type Output = Vec2;

    #[inline]
    fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl core::ops::Div<f32> for Vec2 {
    type Output = Vec2;

    #[inline]
    fn div(self, k: f32) -> Vec2 {
        Vec2::new(self.x / k, self.y / k)
    }
}
