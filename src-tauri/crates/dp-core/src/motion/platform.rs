//! 平台图（S2-M6，T-05 段 · 下）：站立平台图构建与光标避让收口（`02 §5` K-3）。
//!
//! 职责（`03 §2` S2-M6；FR-6-4 / FR-2-6）：
//!   - **四类平台节点**（`02 §5` K-3）：[`PlatformKind::DesktopBottom`]（显示器
//!     工作区底边，与 [`super::roam::DesktopFloor`] 同语义）/ [`PlatformKind::Taskbar`]
//!     / [`PlatformKind::WindowTitleBar`] / [`PlatformKind::ScreenEdge`]（垂直参照
//!     线，**不参与站立/落地判定**，为 FR-1-2 边缘吸附与上层沿边行走预留数据位）；
//!   - **2s 重建**（FR-6-4）：[`PlatformGraph::rebuild`] 由上层注入标题栏带与
//!     任务栏带（VDC；由 `dp-platform::win::winenum` 物理矩形换算，C6/RV-17），
//!     deadline 链**绝对锚定**——下一 deadline = 上一 deadline +
//!     [`REBUILD_INTERVAL_MS`]，与实际触发时刻无关（C3 红线：过冲不向后累积
//!     漂移，历史教训见 `motion` 模块文档）；
//!   - **StandSurface 端口**：落地判定 = 自上而下取 y 最小（top 最小/最高）且
//!     水平包含 `p.x` 者（K-3）；端口语义保持 S2-M5 物理的落地假设
//!     `clamp_to_stand(p).y = p.x 处应站立顶面`（登记）；无平台命中 → 回退桌底
//!     语义（同 `DesktopFloor::clamp_to_stand`，按 `MonitorGeom` 公开方法自建）；
//!   - **光标避让收口**（FR-2-6）：本图 + `avoid::CursorHeat`（150px 热区）+
//!     `roam::RoamSampler::decide_target` 联调闭合——采样目标不落热区、路径穿
//!     热区时 `avoid::detour_waypoint` 给出热区外绕行路标（见文件尾部联调单测）；
//!   - **优雅降级**（`02 §10` R2）：titlebars 为空（枚举失败或无窗口）→ 仅生成
//!     桌底 + 任务栏 + 边缘节点，`degraded = true`，不崩溃、仅桌底行走。
//!
//! 时间纪律（C3）：零时钟——`now_ms` 全部由调用方注入（单调毫秒）。
//! 坐标系（C6/RV-17）：一律 VDC（96-dpi 逻辑像素，原点虚拟桌面左上、可为负）。

use core::cmp::Ordering;

use super::roam::{MonitorGeom, StandSurface, STAND_EPS_PX};
use super::Vec2;

// ---------------------------------------------------------------------------
// 平台节点模型
// ---------------------------------------------------------------------------

/// 平台节点类型（`02 §5 K-3` 四类）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformKind {
    /// 桌底：显示器工作区底边（与 `roam::DesktopFloor` 同语义）。
    DesktopBottom,
    /// 任务栏顶面。
    Taskbar,
    /// 可见窗口标题栏带。
    WindowTitleBar,
    /// 屏幕工作区左右边缘（垂直参照线，不参与站立/落地判定，见 [`Platform`]）。
    ScreenEdge,
}

/// 站立平台 = VDC 水平顶面线段（落地面语义）。
///
/// `ScreenEdge` 退化为点线段：`top = work_bottom_vdc()`、`left = right = 边缘 x`
/// ——垂直线无顶面，不参与站立/落地判定（半开区间 `[left, right)` 对 `left ==
/// right` 恒为空集），仅作 FR-1-2 边缘吸附与沿边行走的数据位。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Platform {
    /// 节点类型。
    pub kind: PlatformKind,
    /// 顶面 y（VDC）。
    pub top: f32,
    /// 水平范围左界（含）。
    pub left: f32,
    /// 水平范围右界（不含）。
    pub right: f32,
}

/// 单条平台带（VDC；上层从 `dp-platform::win::winenum` 物理矩形换算注入，C6）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlatformBand {
    /// 带左缘 x。
    pub left: f32,
    /// 带顶面 y（站立面）。
    pub top: f32,
    /// 带右缘 x（不含）。
    pub right: f32,
}

/// 外部注入的重建输入（VDC；由上层从 winenum 物理矩形换算，C6）。
#[derive(Clone, Debug, Default)]
pub struct PlatformInputs {
    /// 可见窗口标题栏带（窗口外接矩形的 left/top/right；空 = 枚举失败或无窗口）。
    pub titlebars: Vec<PlatformBand>,
    /// 任务栏带（含顶面 y）。
    pub taskbars: Vec<PlatformBand>,
}

/// 平台图重建周期（`02 §5` K-3 / FR-6-4：每 2s 重建）。
pub const REBUILD_INTERVAL_MS: u64 = 2000;

// ---------------------------------------------------------------------------
// 平台图
// ---------------------------------------------------------------------------

/// 平台图（`02 §5` K-3）：站立平台节点集合 + 绝对锚定的重建节拍，
/// 实现 [`StandSurface`] 端口（S2-M5 物理落地判定经同一端口，自动兼容）。
pub struct PlatformGraph {
    /// 平台节点（rebuild 后按 top 升序稳定排序，同 top 按 left）。
    platforms: Vec<Platform>,
    /// 显示器几何（桌底兜底语义，由调用方注入并维护）。
    monitors: Vec<MonitorGeom>,
    /// 下一次重建 deadline（绝对锚定链头）。
    next_rebuild_ms: u64,
    /// 降级标志：上次重建 titlebars 为空（`02 §10` R2）。
    degraded: bool,
}

impl PlatformGraph {
    /// 构造平台图：初始节点 = 仅桌底（等价 `DesktopFloor` 语义，等价 R2 降级
    /// 形态），首次重建 deadline = `now_ms + REBUILD_INTERVAL_MS`（绝对锚定起点）。
    #[must_use]
    pub fn new(monitors: Vec<MonitorGeom>, now_ms: u64) -> Self {
        Self {
            platforms: desktop_bottoms(&monitors),
            monitors,
            next_rebuild_ms: now_ms.saturating_add(REBUILD_INTERVAL_MS),
            degraded: false,
        }
    }

    /// 是否到达重建节拍（`now_ms >= 下一 deadline`）。
    #[must_use]
    pub fn should_rebuild(&self, now_ms: u64) -> bool {
        now_ms >= self.next_rebuild_ms
    }

    /// 重建平台节点（FR-6-4）并推进 deadline 链。
    ///
    /// **绝对锚定（C3）**：下一 deadline = 上一 deadline + [`REBUILD_INTERVAL_MS`]，
    /// 与实际触发时刻 `now_ms` 无关——过冲不向后累积漂移；若上层单次迟到超过
    /// 一个周期，[`Self::should_rebuild`] 将立即再次为真，由上层轮询自然追赶节拍。
    /// `now_ms` 参数保留签名一致性（供上层调用统一口径），不参与 deadline 计算。
    ///
    /// 节点生成规则（K-3）：
    ///   1. `DesktopBottom`：每屏工作区底边线段（与 `DesktopFloor` 同语义）；
    ///   2. `Taskbar`：每条任务栏带；与某桌底 top 相同亦保留（任务栏顶面就是
    ///      常见站立面，不去重）；
    ///   3. `WindowTitleBar`：每条标题栏带；无效带（right ≤ left、NaN/inf）一律
    ///      丢弃（防御外部数据）；
    ///   4. `ScreenEdge`：每屏工作区左右边缘垂直参照节点（不参与站立判定）；
    ///   5. `titlebars` 为空 → `degraded = true`（R2：枚举失败不崩溃、仅桌底行走）。
    pub fn rebuild(&mut self, _now_ms: u64, inputs: PlatformInputs) {
        // ① 桌底节点（每屏工作区底边）。
        let mut platforms = desktop_bottoms(&self.monitors);
        // ② 任务栏节点（与桌底 top 相同亦保留，不去重）。
        for band in &inputs.taskbars {
            if let Some(p) = valid_platform(band, PlatformKind::Taskbar) {
                platforms.push(p);
            }
        }
        // ③ 标题栏节点（无效带防御丢弃）+ 降级标志（R2）。
        self.degraded = inputs.titlebars.is_empty();
        if !self.degraded {
            for band in &inputs.titlebars {
                if let Some(p) = valid_platform(band, PlatformKind::WindowTitleBar) {
                    platforms.push(p);
                }
            }
        }
        // ④ 边缘参照节点（不参与站立判定，FR-1-2 数据位）。
        platforms.extend(screen_edges(&self.monitors));
        // ⑤ top 升序稳定排序，同 top 按 left（NaN 已被 valid_platform 过滤，
        //    桌底/边缘来自 MonitorGeom 公开方法，partial_cmp 恒有值）。
        platforms.sort_by(|a, b| {
            a.top
                .partial_cmp(&b.top)
                .unwrap_or(Ordering::Equal)
                .then(a.left.partial_cmp(&b.left).unwrap_or(Ordering::Equal))
        });
        self.platforms = platforms;

        // ⑥ 绝对锚定（C3）：下一 deadline = 上一 deadline + 周期。
        self.next_rebuild_ms = self.next_rebuild_ms.saturating_add(REBUILD_INTERVAL_MS);
    }

    /// 平台节点只读快照（top 升序稳定序，同 top 按 left）。
    #[must_use]
    pub fn platforms(&self) -> &[Platform] {
        &self.platforms
    }

    /// 是否处于降级态（上次重建 titlebars 为空；`02 §10` R2）。
    #[must_use]
    pub fn degraded(&self) -> bool {
        self.degraded
    }

    /// 回退桌底语义的钳制（与 `roam::DesktopFloor::clamp_to_stand` 同逻辑；
    /// DesktopFloor 字段私有不可直接复用，按 `MonitorGeom` 公开方法自建）。
    fn floor_clamp(&self, p: Vec2) -> Vec2 {
        // 命中某屏工作区 x 区间（半开）→ x 钳制后落其底边。
        if let Some(m) = self
            .monitors
            .iter()
            .find(|m| p.x >= m.work_origin_vdc.x && p.x < m.work_right_vdc())
        {
            return Vec2::new(
                p.x.clamp(m.work_origin_vdc.x, m.work_right_vdc()),
                m.work_bottom_vdc(),
            );
        }
        // 未命中 → 工作区中心 x 最近者；无显示器 → 原样返回（防御，不 panic）。
        match self.monitors.iter().min_by(|a, b| {
            let da = (p.x - a.work_center().x).abs();
            let db = (p.x - b.work_center().x).abs();
            da.partial_cmp(&db).unwrap_or(Ordering::Equal)
        }) {
            Some(m) => Vec2::new(
                p.x.clamp(m.work_origin_vdc.x, m.work_right_vdc()),
                m.work_bottom_vdc(),
            ),
            None => p,
        }
    }
}

impl StandSurface for PlatformGraph {
    /// 合法站立点：存在 `kind != ScreenEdge` 的平台水平包含 `p.x`
    /// （`left ≤ p.x < right`）且 `|p.y − top| ≤ STAND_EPS_PX`。
    fn is_valid_stand(&self, p: Vec2) -> bool {
        self.platforms.iter().any(|pl| {
            pl.kind != PlatformKind::ScreenEdge
                && p.x >= pl.left
                && p.x < pl.right
                && (p.y - pl.top).abs() <= STAND_EPS_PX
        })
    }

    /// 端口语义（保持 S2-M5 物理的落地假设，登记）：`clamp_to_stand(p).y =
    /// p.x 处应站立顶面`。
    ///
    /// 落地判定（K-3）= 自上而下取 y 最小（top 最小/最高）且水平包含 `p.x` 的
    /// 非 ScreenEdge 平台 → 返回 `(p.x, top)`；无命中 → 回退桌底语义
    /// （[`Self::floor_clamp`]，同 `DesktopFloor`）；无显示器 → 原样返回。
    fn clamp_to_stand(&self, p: Vec2) -> Vec2 {
        self.platforms
            .iter()
            .filter(|pl| pl.kind != PlatformKind::ScreenEdge && p.x >= pl.left && p.x < pl.right)
            .min_by(|a, b| a.top.partial_cmp(&b.top).unwrap_or(Ordering::Equal))
            .map_or_else(|| self.floor_clamp(p), |pl| Vec2::new(p.x, pl.top))
    }
}

// ---------------------------------------------------------------------------
// 节点生成（纯函数）
// ---------------------------------------------------------------------------

/// 桌底节点：每屏工作区底边线段（x ∈ [work_origin_vdc.x, work_right_vdc())，
/// top = work_bottom_vdc()——与 `roam::DesktopFloor` 同语义（其字段私有，
/// 按 `MonitorGeom` 公开方法自建）。
fn desktop_bottoms(monitors: &[MonitorGeom]) -> Vec<Platform> {
    monitors
        .iter()
        .map(|m| Platform {
            kind: PlatformKind::DesktopBottom,
            top: m.work_bottom_vdc(),
            left: m.work_origin_vdc.x,
            right: m.work_right_vdc(),
        })
        .collect()
}

/// 边缘参照节点：每屏工作区左右边缘的垂直参照线（top = work_bottom_vdc()、
/// left = right = 边缘 x，即退化为点线段）。
///
/// 语义登记：不参与站立/落地判定（垂直线无顶面），为 FR-1-2 边缘吸附与
/// 上层沿边行走预留数据位。
fn screen_edges(monitors: &[MonitorGeom]) -> Vec<Platform> {
    monitors
        .iter()
        .flat_map(|m| {
            [
                Platform {
                    kind: PlatformKind::ScreenEdge,
                    top: m.work_bottom_vdc(),
                    left: m.work_origin_vdc.x,
                    right: m.work_origin_vdc.x,
                },
                Platform {
                    kind: PlatformKind::ScreenEdge,
                    top: m.work_bottom_vdc(),
                    left: m.work_right_vdc(),
                    right: m.work_right_vdc(),
                },
            ]
        })
        .collect()
}

/// 带校验（防御外部数据）：NaN/inf 任一分量非有限、或 right ≤ left → 一律丢弃。
fn valid_platform(band: &PlatformBand, kind: PlatformKind) -> Option<Platform> {
    if !band.left.is_finite() || !band.top.is_finite() || !band.right.is_finite() {
        return None;
    }
    if band.right <= band.left {
        return None;
    }
    Some(Platform { kind, top: band.top, left: band.left, right: band.right })
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::avoid::{detour_waypoint, CursorHeat, DETOUR_MARGIN_PX};
    use crate::motion::roam::{RoamRegion, RoamSampler};

    /// K-3 光标避让热区半径（FR-2-6 联调用，与 `RoamCfg` 默认口径一致）。
    const CURSOR_HEAT_RADIUS_PX: f32 = 150.0;

    /// 主屏：VDC (0,0) 1920×1080，工作区底边 1040（与 roam.rs tests 同构）。
    fn mon_a() -> MonitorGeom {
        MonitorGeom {
            id: 1,
            origin_vdc: Vec2::new(0.0, 0.0),
            size_vdc: Vec2::new(1920.0, 1080.0),
            work_origin_vdc: Vec2::new(0.0, 0.0),
            work_size_vdc: Vec2::new(1920.0, 1040.0),
            primary: true,
        }
    }

    /// 标题栏带：x ∈ [400, 1520)，顶面 y = 300（桌底上方 740px 处的窗口标题栏）。
    fn titlebar_band() -> PlatformBand {
        PlatformBand { left: 400.0, top: 300.0, right: 1520.0 }
    }

    /// 任务栏带：x ∈ [0, 1920)，顶面 y = 1040（与桌底同线的常见口径）。
    fn taskbar_band() -> PlatformBand {
        PlatformBand { left: 0.0, top: 1040.0, right: 1920.0 }
    }

    /// 单屏 + 一条标题栏 + 一条任务栏的重建图（dt 绝对锚定测试共用）。
    fn graph_with_bands(now_ms: u64) -> PlatformGraph {
        let mut g = PlatformGraph::new(vec![mon_a()], now_ms);
        g.rebuild(now_ms, PlatformInputs {
            titlebars: vec![titlebar_band()],
            taskbars: vec![taskbar_band()],
        });
        g
    }

    // -- 构造 / 重建节点生成 ------------------------------------------------------

    #[test]
    fn new_starts_desktop_only() {
        let g = PlatformGraph::new(vec![mon_a()], 1000);
        assert_eq!(g.platforms().len(), 1, "初始仅桌底（等价 DesktopFloor 语义）");
        let p = g.platforms()[0];
        assert_eq!(p.kind, PlatformKind::DesktopBottom);
        assert!((p.top - 1040.0).abs() < 1e-6, "top = 工作区底边");
        assert!((p.left - 0.0).abs() < 1e-6 && (p.right - 1920.0).abs() < 1e-6);
        assert!(!g.degraded(), "初始非降级");
        assert!(!g.should_rebuild(1000), "首次重建 deadline = now + 2000");
        assert!(g.should_rebuild(3000), "now == deadline → true");
    }

    #[test]
    fn rebuild_single_monitor_node_counts_and_coords() {
        let g = graph_with_bands(0);
        // 桌底 1 + 任务栏 1 + 标题栏 1 + 边缘 2 = 5。
        assert_eq!(g.platforms().len(), 5);
        // top 升序稳定排序：标题栏(300) → 桌底/任务栏/边缘(1040)。
        let kinds: Vec<PlatformKind> = g.platforms().iter().map(|p| p.kind).collect();
        assert_eq!(kinds[0], PlatformKind::WindowTitleBar);
        assert_eq!(kinds[1], PlatformKind::DesktopBottom, "同 top 按 left，left 相同保持生成序");
        assert_eq!(kinds[2], PlatformKind::Taskbar);
        assert_eq!(kinds[3], PlatformKind::ScreenEdge);
        assert_eq!(kinds[4], PlatformKind::ScreenEdge);

        // 标题栏节点坐标。
        let tb = &g.platforms()[0];
        assert!((tb.top - 300.0).abs() < 1e-6);
        assert!((tb.left - 400.0).abs() < 1e-6 && (tb.right - 1520.0).abs() < 1e-6);
        // 桌底节点坐标（与 DesktopFloor 同语义）。
        let db = &g.platforms()[1];
        assert!((db.top - 1040.0).abs() < 1e-6);
        assert!((db.left - 0.0).abs() < 1e-6 && (db.right - 1920.0).abs() < 1e-6);
        // 边缘节点：退化为点线段（left == right）、top = 工作区底边。
        let e1 = &g.platforms()[3];
        assert_eq!(e1.left, e1.right, "边缘退化为点线段");
        assert!((e1.left - 0.0).abs() < 1e-6, "左缘 x = 工作区左缘");
        let e2 = &g.platforms()[4];
        assert_eq!(e2.left, e2.right);
        assert!((e2.left - 1920.0).abs() < 1e-6, "右缘 x = 工作区右缘");
        assert!((e2.top - 1040.0).abs() < 1e-6, "edge top = 工作区底边");
    }

    #[test]
    fn same_top_bands_sorted_by_left() {
        let mut g = PlatformGraph::new(vec![mon_a()], 0);
        g.rebuild(
            2000,
            PlatformInputs {
                titlebars: vec![
                    PlatformBand { left: 800.0, top: 300.0, right: 1000.0 },
                    PlatformBand { left: 100.0, top: 300.0, right: 300.0 },
                    PlatformBand { left: 500.0, top: 200.0, right: 700.0 },
                ],
                taskbars: Vec::new(),
            },
        );
        let tbs: Vec<f32> = g
            .platforms()
            .iter()
            .filter(|p| p.kind == PlatformKind::WindowTitleBar)
            .map(|p| p.left)
            .collect();
        assert_eq!(tbs, vec![500.0, 100.0, 800.0], "top 200 在前，同 top=300 按 left 升序");
    }

    #[test]
    fn taskbar_sharing_floor_top_is_kept() {
        let g = graph_with_bands(0);
        // 任务栏顶面 y=1040 == 桌底 top → 不去重、保留节点（任务栏顶面即常见站立面）。
        assert_eq!(
            g.platforms().iter().filter(|p| p.kind == PlatformKind::Taskbar).count(),
            1
        );
        assert!(g.is_valid_stand(Vec2::new(100.0, 1040.0)), "任务栏顶面站立合法");
    }

    // -- 站立 / 钳制（K-3 落地语义） ------------------------------------------------

    #[test]
    fn clamp_prefers_highest_platform_within_range() {
        let g = graph_with_bands(0);
        // x=600 同时被标题栏(300)与桌底(1040)水平包含 → 取 top 最小（最高）者。
        let p = g.clamp_to_stand(Vec2::new(600.0, 900.0));
        assert!((p.y - 300.0).abs() < 1e-6, "应站在标题栏顶面：{p:?}");
        assert!((p.x - 600.0).abs() < 1e-6, "x 不变");
        assert!(g.is_valid_stand(p), "钳制点为合法站立点");
    }

    #[test]
    fn clamp_falls_back_to_floor_outside_titlebar_x() {
        let g = graph_with_bands(0);
        // x=1600 超出标题栏水平范围 [400, 1520) → 回退桌底。
        let p = g.clamp_to_stand(Vec2::new(1600.0, 900.0));
        assert!((p.y - 1040.0).abs() < 1e-6, "回退桌底：{p:?}");
        assert!(g.is_valid_stand(p));
    }

    #[test]
    fn screen_edge_not_standable_and_not_snapped() {
        let g = graph_with_bands(0);
        // 右缘 x=1920：桌底/任务栏 [0,1920) 不含、标题栏 [400,1520) 不含，
        // ScreenEdge 不参与站立（垂直线无顶面）→ 判定 false。
        assert!(!g.is_valid_stand(Vec2::new(1920.0, 1040.0)), "ScreenEdge 不提供站立点");
        // 处于标题栏高度但 x 在边缘之外 → 同样无平台水平包含 → false。
        assert!(!g.is_valid_stand(Vec2::new(1920.0, 300.0)));
        // 钳制不吸附到 ScreenEdge：边缘外的点走桌底回退语义（x 钳到工作区右缘 +
        // 落底边），与 DesktopFloor::clamp_to_stand 行为一致。
        let p = g.clamp_to_stand(Vec2::new(5000.0, 1040.0));
        assert!(
            (p.y - 1040.0).abs() < 1e-6 && (p.x - 1920.0).abs() < 1e-6,
            "钳制结果为桌底语义而非 ScreenEdge 节点：{p:?}"
        );
    }

    #[test]
    fn clamp_no_monitors_identity() {
        let mut g = PlatformGraph::new(Vec::new(), 0);
        g.rebuild(2000, PlatformInputs::default());
        let p = Vec2::new(100.0, 200.0);
        assert!(!g.is_valid_stand(p), "无显示器无合法站立点");
        assert_eq!(g.clamp_to_stand(p), p, "无显示器钳制为恒等（防御）");
    }

    // -- 降级（R2）与无效带防御 ------------------------------------------------------

    #[test]
    fn empty_titlebars_degrades_but_keeps_floor() {
        let mut g = PlatformGraph::new(vec![mon_a()], 0);
        g.rebuild(0, PlatformInputs {
            titlebars: Vec::new(),
            taskbars: vec![taskbar_band()],
        });
        assert!(g.degraded(), "titlebars 为空 → R2 降级标志");
        // 降级不崩：仍有桌底 + 任务栏 + 边缘，无标题栏节点。
        let kinds: Vec<PlatformKind> = g.platforms().iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds.iter().filter(|k| **k == PlatformKind::DesktopBottom).count(),
            1,
            "降级后桌底仍在"
        );
        assert!(kinds.iter().all(|k| *k != PlatformKind::WindowTitleBar));
        assert!(g.is_valid_stand(Vec2::new(960.0, 1040.0)), "降级后桌底仍可站立");
        let p = g.clamp_to_stand(Vec2::new(960.0, 900.0));
        assert!((p.y - 1040.0).abs() < 1e-6, "降级后仍钳到桌底");
    }

    #[test]
    fn degraded_resets_when_titlebars_return() {
        let mut g = PlatformGraph::new(vec![mon_a()], 0);
        g.rebuild(0, PlatformInputs::default());
        assert!(g.degraded());
        g.rebuild(2000, PlatformInputs {
            titlebars: vec![titlebar_band()],
            taskbars: Vec::new(),
        });
        assert!(!g.degraded(), "titlebars 恢复 → 降级解除");
        assert!(g.platforms().iter().any(|p| p.kind == PlatformKind::WindowTitleBar));
    }

    #[test]
    fn invalid_bands_are_filtered() {
        let mut g = PlatformGraph::new(vec![mon_a()], 0);
        g.rebuild(
            0,
            PlatformInputs {
                titlebars: vec![
                    titlebar_band(),                                             // 有效
                    PlatformBand { left: 500.0, top: 400.0, right: 500.0 }, // right == left
                    PlatformBand { left: 500.0, top: 400.0, right: 100.0 }, // right < left
                    PlatformBand { left: f32::NAN, top: 400.0, right: 800.0 },  // NaN
                    PlatformBand { left: 100.0, top: f32::INFINITY, right: 800.0 }, // inf
                ],
                taskbars: vec![PlatformBand { left: 0.0, top: f32::NEG_INFINITY, right: 100.0 }],
            },
        );
        let tbs: Vec<&Platform> = g
            .platforms()
            .iter()
            .filter(|p| p.kind == PlatformKind::WindowTitleBar)
            .collect();
        assert_eq!(tbs.len(), 1, "仅有效标题栏带保留");
        let tsk: Vec<&Platform> = g
            .platforms()
            .iter()
            .filter(|p| p.kind == PlatformKind::Taskbar)
            .collect();
        assert!(tsk.is_empty(), "无效任务栏带被丢弃");
    }

    // -- 重建节拍（边界 + 绝对锚定，C3） -----------------------------------------------

    #[test]
    fn should_rebuild_boundary_and_overshoot() {
        let g = PlatformGraph::new(vec![mon_a()], 0);
        // 边界：deadline = 2000；now = 1999 → false；now == 2000 → true。
        assert!(!g.should_rebuild(1999));
        assert!(g.should_rebuild(2000));
        assert!(g.should_rebuild(5000), "过冲后同样为真");
    }

    #[test]
    fn rebuild_deadline_chain_is_absolutely_anchored() {
        let mut g = PlatformGraph::new(vec![mon_a()], 0);
        // 首次 deadline = 2000；过冲到 now=9000 才触发 → 下一 deadline 仍 =
        // 上一 deadline(2000) + 2000 = 4000，不漂移到 9000+2000（C3）。
        g.rebuild(9000, PlatformInputs::default());
        assert_eq!(g.next_rebuild_ms, 4000, "next = 上一 deadline + 2000，过冲不漂移");
        assert!(g.should_rebuild(9000), "过冲已超一个周期 → 立即到期（上层轮询追赶）");

        // 连续追赶：每次 rebuild 单步推进一个周期，直至 deadline 超过 now。
        g.rebuild(9000, PlatformInputs::default());
        assert_eq!(g.next_rebuild_ms, 6000);
        g.rebuild(9000, PlatformInputs::default());
        g.rebuild(9000, PlatformInputs::default());
        assert_eq!(g.next_rebuild_ms, 10_000, "追赶至超过 now 后节拍恢复");
        assert!(!g.should_rebuild(9_999));
        assert!(g.should_rebuild(10_000));
    }

    // -- FR-2-6 光标避让联调闭合（图 + 150px 热区 + 采样器 + 绕行路标） ----------------

    /// 联调 ①：含标题栏平台的图 + 光标热区（中心在桌底、r=150）→
    /// `RoamSampler::decide_target` 的目标均不落在热区内且为合法站立点。
    #[test]
    fn integration_sampler_targets_never_in_cursor_heat() {
        let g = graph_with_bands(0);
        let region = RoamRegion::from_monitor_work(&mon_a());
        // 光标在桌底 x=600（y=1040 底边上），150px 热区覆盖该段桌底；
        // x ∈ [400,1520) 的采样点会被钳到标题栏（y=300，距热区 ≥740px）。
        let heat = CursorHeat::new(Vec2::new(600.0, 1040.0), CURSOR_HEAT_RADIUS_PX);
        let mut sampler = RoamSampler::new(7);
        for _ in 0..50 {
            let target = sampler
                .decide_target(&region, &g, Some(heat), &[])
                .expect("标题栏平台提供热区外采样空间，12 次采样内应命中");
            assert!(!heat.contains(target), "目标不得落在光标热区内：{target:?}");
            assert!(g.is_valid_stand(target), "目标为合法站立点：{target:?}");
            let on_titlebar = (target.y - 300.0).abs() < 1e-6;
            let on_floor = (target.y - 1040.0).abs() < 1e-6;
            assert!(on_titlebar || on_floor, "目标只应落在标题栏或桌底：{target:?}");
        }
    }

    /// 联调 ②：路径沿桌底（站立点取自平台图钳制）穿过 150px 光标热区 →
    /// `detour_waypoint` 给出热区外绕行路标（半径 + 裕量），证明热区几何与
    /// 新平台联调闭合。
    #[test]
    fn integration_detour_waypoint_bypasses_heat_on_floor() {
        let g = graph_with_bands(0);
        let heat = CursorHeat::new(Vec2::new(600.0, 1040.0), CURSOR_HEAT_RADIUS_PX);
        // 两个站立点均取自平台图钳制（x < 400 / x > 1520 → 桌底线上）。
        let from = g.clamp_to_stand(Vec2::new(300.0, 900.0));
        let to = g.clamp_to_stand(Vec2::new(1550.0, 900.0));
        assert!(
            (from.y - 1040.0).abs() < 1e-6 && (to.y - 1040.0).abs() < 1e-6,
            "两点均在桌底站立：{from:?} → {to:?}"
        );
        assert!(heat.segment_intersects(from, to), "前置：路径确实穿过热区");
        let wp = detour_waypoint(from, to, &heat).expect("穿过热区应有绕行路标");
        assert!(!heat.contains(wp), "绕行路标须在热区外：{wp:?}");
        assert!((wp.x - 600.0).abs() < 1e-4, "路标 x = 路径上距热区中心最近点 x");
        assert!(
            (wp.y - (1040.0 + CURSOR_HEAT_RADIUS_PX + DETOUR_MARGIN_PX)).abs() < 1e-4,
            "路标 y = 最近点 + 半径 + 裕量：{wp:?}"
        );
    }
}
