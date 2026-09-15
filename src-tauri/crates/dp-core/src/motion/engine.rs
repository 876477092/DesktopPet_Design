//! 运动决策引擎状态机（S2-M4，T-05）。
//!
//! 职责（03 台账 §2 S2-M4）：
//!   - **决策调度**：每 5~30s（按 pace 缩放）决策一次漫游目标；deadline 链
//!     **绝对锚定**——第 k 次 deadline = 第 k−1 次 deadline + 间隔_k，不以实际
//!     触发时刻为基准（C3 红线：tick 迟到的过冲不向后累积漂移）；
//!   - **行走推进**：路径按 `walk_speed_px_per_sec` 逐 tick 步进（位置由 tick
//!     推进，绝无 SetWindowPos 跳变，AC-13 / FR-2-1）；
//!   - **跨屏**（AC-13）：目标在另一显示器时，路径先水平/垂直走到两屏 VDC
//!     交界（另一轴钳到两屏重叠区间内，[`seam_waypoint`]），再跨到目标屏；
//!   - **显示器变更迁移**（FR-1-4）：保存 NDC + monitorId 快照；当前屏还在 →
//!     NDC 重算 VDC 平移；当前屏消失 → deadline = 变更时刻 + 2000ms（绝对
//!     锚定）迁移到最近显示器工作区中心；
//!   - **活动范围约束**：决策采样范围 = 当前屏工作区 ∪ 相邻屏
//!     （[`RoamRegion::from_current_and_adjacent`]）。
//!
//! 解耦边界（S2-M3 仲裁器）：引擎只输出 [`MotionEvent`]（目标决策/到达/回退
//! idle/迁移完成），不直接提交/打断动作；动作下发由上层驱动循环（S7-M3）
//! 统一走仲裁器。`BehaviorCfg::auto_roam` 开关门控归上层。
//!
//! 时间纪律（C3）：内部零时钟——所有时间由调用方注入单调毫秒 `now_ms: u64`，
//! tick 间 dt 由相邻两次 `now_ms` 差得出（调用方保证单调）。

use super::avoid::{detour_waypoint, CursorHeat};
use super::roam::{
    decision_interval_ms, DesktopFloor, MonitorGeom, RoamRegion, RoamSampler, SplitMix64,
    StandSurface,
};
use super::Vec2;
use crate::config::model::RoamCfg;

/// 拔屏迁移时限（`01 §6.2` FR-1-4：2s 内出现于剩余屏；
/// deadline = 变更时刻 + 2000ms，绝对锚定）。
pub const MIGRATION_DEADLINE_MS: u64 = 2000;

/// pacing 随机流种子偏移（与采样器随机流解耦，避免序列相关）。
const PACING_SEED_OFFSET: u64 = 0x517C_C1B7_2722_0A95;

/// 运动事件（与 S2-M3 仲裁器解耦：引擎只产出事件，动作下发归上层 S7-M3）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MotionEvent {
    /// 决策出新目标。`cross_monitor` = 目标在另一显示器（上层可据此请求
    /// 跨屏行走的动作表现）。
    TargetDecided {
        /// 目标点（VDC）。
        target: Vec2,
        /// 是否跨屏目标。
        cross_monitor: bool,
    },
    /// 到达目标（本次漫游段结束）。
    Arrived {
        /// 到达点（VDC）。
        pos: Vec2,
    },
    /// 本轮决策无合法目标 → 上层回退 idle。
    IdleFallback,
    /// 显示器变更迁移完成（FR-1-4）。
    Migrated {
        /// 迁移落点（VDC，最近显示器工作区中心）。
        to: Vec2,
    },
    /// 落地（S2-M5 物理）。`impact` = 落地前一子步纵向速度绝对值（VDC px/s），
    /// 上层据此请求 ACT-M-06 落地缓冲（经仲裁器 submit，物理不直接提交动作）。
    Landed {
        /// 落地冲击（落地前 |vel.y|，VDC px/s）。
        impact: f32,
    },
}

/// 运动决策引擎状态机。
#[derive(Debug)]
pub struct MotionEngine {
    /// 当前显示器几何列表（调用方经 `on_monitors_changed` 维护）。
    monitors: Vec<MonitorGeom>,
    /// 站立面（兜底地面，随 monitors 重建）。
    floor: DesktopFloor,
    /// 漫游配置（`02 K-3` 单一真源）。
    cfg: RoamCfg,
    /// 决策间隔随机流（与采样器流分离）。
    pacing_rng: SplitMix64,
    /// 漫游采样器（含独立随机流）。
    sampler: RoamSampler,
    /// 当前位置（VDC，可为负）。
    pos: Vec2,
    /// 当前所在显示器 id。
    current_monitor_id: u64,
    /// NDC（0~1 归一）+ monitorId 快照的 NDC 分量（FR-1-4）。
    home_ndc: Vec2,
    /// 下一次决策 deadline（绝对锚定链头）。
    next_decision_ms: u64,
    /// 行走路标队列（空 = 静止）。
    waypoints: Vec<Vec2>,
    /// 上一次 tick 的 `now_ms`（None = 尚未 tick，首 tick dt = 0）。
    last_tick_ms: Option<u64>,
    /// 拔屏迁移 deadline（None = 无迁移挂起）。
    migration_deadline_ms: Option<u64>,
    /// 光标位置（VDC；None = 未知/不避让），由调用方每 tick 注入。
    cursor_pos: Option<Vec2>,
}

impl MotionEngine {
    /// 构造引擎。
    ///
    /// `pos` 为初始位置（VDC），构造时钳制到站立面；`monitors` 为初始显示器
    /// 几何；`cfg` 为漫游配置（`RoamCfg` 单一真源）；`seed` 注入 PRNG（测试
    /// 可复现）；`now_ms` 为启动时刻（首个决策 deadline = 启动时刻 + 首个
    /// 间隔，绝对锚定起点）。
    #[must_use]
    pub fn new(
        pos: Vec2,
        monitors: Vec<MonitorGeom>,
        cfg: RoamCfg,
        seed: u64,
        now_ms: u64,
    ) -> Self {
        let floor = DesktopFloor::new(monitors.clone());
        let mut engine = Self {
            monitors,
            floor,
            cfg,
            pacing_rng: SplitMix64::new(seed.wrapping_add(PACING_SEED_OFFSET)),
            sampler: RoamSampler::new(seed),
            pos,
            current_monitor_id: 0,
            home_ndc: Vec2::new(0.5, 0.5),
            next_decision_ms: now_ms,
            waypoints: Vec::new(),
            last_tick_ms: None,
            migration_deadline_ms: None,
            cursor_pos: None,
        };
        if let Some(m) = engine.monitor_at(pos) {
            engine.current_monitor_id = m.id;
        }
        engine.pos = engine.floor.clamp_to_stand(pos);
        engine.sync_home();
        let first_interval = decision_interval_ms(&mut engine.pacing_rng, &engine.cfg);
        engine.next_decision_ms = now_ms.saturating_add(first_interval);
        engine
    }

    // -- 只读查询 ---------------------------------------------------------------

    /// 当前位置（VDC）。
    #[must_use]
    pub const fn pos(&self) -> Vec2 {
        self.pos
    }

    /// 当前所在显示器 id。
    #[must_use]
    pub const fn current_monitor_id(&self) -> u64 {
        self.current_monitor_id
    }

    /// NDC 快照（FR-1-4 调试视图；0~1 归一于当前显示器矩形）。
    #[must_use]
    pub const fn home_ndc(&self) -> Vec2 {
        self.home_ndc
    }

    /// 下一次决策 deadline（绝对锚定链头；测试/调试视图）。
    #[must_use]
    pub const fn next_decision_ms(&self) -> u64 {
        self.next_decision_ms
    }

    /// 拔屏迁移 deadline（None = 无迁移挂起；测试/调试视图）。
    #[must_use]
    pub const fn migration_deadline_ms(&self) -> Option<u64> {
        self.migration_deadline_ms
    }

    /// 是否行走中。
    #[must_use]
    pub fn is_walking(&self) -> bool {
        !self.waypoints.is_empty()
    }

    // -- 输入注入 -----------------------------------------------------------------

    /// 注入光标位置（VDC；None = 未知/不避让）。由调用方每 tick 前更新。
    pub fn set_cursor(&mut self, pos: Option<Vec2>) {
        self.cursor_pos = pos;
    }

    /// 显示器变更（FR-1-4，`WM_DISPLAYCHANGE` 等路径）。
    ///
    /// ① 当前显示器还在：以 NDC 快照重算 VDC（分辨率/缩放变更平移），并钳制
    ///    到新站立面，不迁移；
    /// ② 当前显示器消失：挂起迁移 deadline = `now_ms + 2000ms`（绝对锚定），
    ///    到期由 [`MotionEngine::tick`] 迁移到最近显示器工作区中心。
    ///
    /// 两种情形均清空行走路标（路标可能落在已消失/已变形的显示器上）。
    pub fn on_monitors_changed(&mut self, new_monitors: Vec<MonitorGeom>, now_ms: u64) {
        self.monitors = new_monitors;
        self.floor = DesktopFloor::new(self.monitors.clone());
        self.waypoints.clear();
        let still_present =
            self.monitors.iter().any(|m| m.id == self.current_monitor_id);
        if still_present {
            self.migration_deadline_ms = None;
            // NDC 重算 VDC（快照基 = 显示器矩形）。
            if let Some(m) = self.monitor_by_id(self.current_monitor_id).copied() {
                self.pos = m.ndc_to_vdc(self.home_ndc);
                self.pos = self.floor.clamp_to_stand(self.pos);
            }
        } else {
            // ② FR-1-4：deadline = 变更时刻 + 2000ms，绝对锚定。
            self.migration_deadline_ms = Some(now_ms.saturating_add(MIGRATION_DEADLINE_MS));
        }
        self.sync_home();
    }

    /// 上层主动指派目标（如测试注入、后续上层直控）。
    ///
    /// 走同一跨屏路径规划（[`MotionEngine::plan_path`]），不产事件（主动指令
    /// 无需决策事件）。返回 `false` 表示无显示器、无法规划。
    pub fn walk_to(&mut self, target: Vec2) -> bool {
        if self.monitors.is_empty() {
            return false;
        }
        let target = self.floor.clamp_to_stand(target);
        self.waypoints = self.plan_path(self.pos, target, None);
        true
    }

    // -- 每 tick 推进 -------------------------------------------------------------

    /// 推进一帧（dt 由相邻两次 `now_ms` 差得出）。
    ///
    /// 顺序：拔屏迁移到期检查 → 行走推进 → 决策到期检查。
    pub fn tick(&mut self, now_ms: u64) -> Vec<MotionEvent> {
        let mut events = Vec::new();
        let dt_ms = self.last_tick_ms.map_or(0, |prev| now_ms.saturating_sub(prev));
        self.last_tick_ms = Some(now_ms);
        self.try_migrate(now_ms, &mut events);
        self.advance_walk(dt_ms, &mut events);
        self.maybe_decide(now_ms, &mut events);
        self.sync_home();
        events
    }

    /// 拔屏迁移到期处理（FR-1-4）。
    fn try_migrate(&mut self, now_ms: u64, events: &mut Vec<MotionEvent>) {
        let Some(deadline) = self.migration_deadline_ms else {
            return;
        };
        if now_ms < deadline {
            return;
        }
        self.migration_deadline_ms = None;
        // 最近显示器（VDC 矩形中心欧氏距离最近，与 display.rs monitor_at 同口径）。
        if let Some(target) = self.nearest_monitor(self.pos).copied() {
            self.pos = target.work_center();
            self.current_monitor_id = target.id;
            self.waypoints.clear();
            events.push(MotionEvent::Migrated { to: self.pos });
        }
        // 无显示器（防御）：deadline 已清，保持原位等待下一轮变更。
        self.sync_home();
    }

    /// 行走推进：按 `walk_speed_px_per_sec` 消耗路标队列（逐 tick 步进，无跳变）。
    fn advance_walk(&mut self, dt_ms: u64, events: &mut Vec<MotionEvent>) {
        if self.waypoints.is_empty() {
            return;
        }
        // 速度防御：非有限 / ≤ 0 → 回退内置默认（60 VDC px/s）。
        let fallback = RoamCfg::default().walk_speed_px_per_sec;
        let speed = self.cfg.walk_speed_px_per_sec;
        let speed = if speed.is_finite() && speed > 0.0 { speed } else { fallback };
        let mut budget = speed * dt_ms as f32 / 1000.0;
        while budget > 0.0 {
            let Some(wp) = self.waypoints.first().copied() else {
                break;
            };
            let dist = self.pos.distance(wp);
            if dist <= budget || dist <= 1e-3 {
                self.pos = wp;
                budget -= dist.min(budget);
                self.waypoints.remove(0);
            } else {
                let dir = (wp - self.pos) / dist;
                self.pos = self.pos + dir.scale(budget);
                budget = 0.0;
            }
        }
        if self.waypoints.is_empty() {
            events.push(MotionEvent::Arrived { pos: self.pos });
        }
    }

    /// 决策到期处理（绝对锚定 deadline 链）。
    ///
    /// 到期先推进 deadline 链（`deadline += 间隔`，与实际触发时刻无关——tick
    /// 迟到的过冲不向后累积），再视状态决策：迁移等待期 / 行走中跳过本轮
    /// 决策（链已推进，节奏不被行走时长拖变形）。
    fn maybe_decide(&mut self, now_ms: u64, events: &mut Vec<MotionEvent>) {
        if now_ms < self.next_decision_ms {
            return;
        }
        let interval = decision_interval_ms(&mut self.pacing_rng, &self.cfg);
        self.next_decision_ms = self.next_decision_ms.saturating_add(interval);
        if self.migration_deadline_ms.is_some() || !self.waypoints.is_empty() {
            return;
        }
        let Some(cur) = self.monitor_by_id(self.current_monitor_id).copied() else {
            events.push(MotionEvent::IdleFallback);
            return;
        };
        // 活动范围约束（02 K-3）：当前屏工作区 ∪ 相邻屏（S2-M4 默认全工作区）。
        let region = RoamRegion::from_current_and_adjacent(&cur, &self.monitors);
        let heat = self
            .cursor_pos
            .map(|p| CursorHeat::new(p, self.cfg.cursor_avoid_radius_px as f32));
        match self.sampler.decide_target(&region, &self.floor, heat, &[]) {
            Some(target) => {
                let cross = self.monitor_at(target).is_some_and(|m| m.id != cur.id);
                self.waypoints = self.plan_path(self.pos, target, heat);
                events.push(MotionEvent::TargetDecided { target, cross_monitor: cross });
            }
            None => events.push(MotionEvent::IdleFallback),
        }
    }

    // -- 路径规划 ---------------------------------------------------------------

    /// 路径规划：同屏直走（路径穿光标热区时插入绕行路标，落点须为合法站立点
    /// 才采用）；跨屏经 [`seam_waypoint`] 边界插值（AC-13）。
    fn plan_path(&self, from: Vec2, to: Vec2, heat: Option<CursorHeat>) -> Vec<Vec2> {
        let Some(a) = self.monitor_at(from).copied() else {
            return vec![to];
        };
        let Some(b) = self.monitor_at(to).copied() else {
            return vec![to];
        };
        if a.id == b.id {
            if let Some(h) = heat {
                if let Some(wp) = detour_waypoint(from, to, &h) {
                    if self.floor.is_valid_stand(wp) {
                        return vec![wp, to];
                    }
                }
            }
            return vec![to];
        }
        vec![seam_waypoint(&a, &b, from), to]
    }

    // -- 几何辅助 ---------------------------------------------------------------

    /// 为 VDC 点选择显示器：命中（半开）优先，未命中取矩形中心最近者
    /// （与 `display.rs select_monitor_at` 同口径）。
    fn monitor_at(&self, p: Vec2) -> Option<&MonitorGeom> {
        if let Some(hit) = self.monitors.iter().find(|m| m.contains_vdc(p)) {
            return Some(hit);
        }
        self.nearest_monitor(p)
    }

    /// 按显示器 id 查找。
    fn monitor_by_id(&self, id: u64) -> Option<&MonitorGeom> {
        self.monitors.iter().find(|m| m.id == id)
    }

    /// 距 VDC 点矩形中心最近的显示器。
    fn nearest_monitor(&self, p: Vec2) -> Option<&MonitorGeom> {
        self.monitors.iter().min_by(|a, b| {
            let da = a.center_vdc().distance_sq(p);
            let db = b.center_vdc().distance_sq(p);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    /// 以当前位置刷新 NDC 快照（FR-1-4）。
    fn sync_home(&mut self) {
        self.home_ndc = self
            .monitor_by_id(self.current_monitor_id)
            .map_or(self.home_ndc, |m| m.vdc_to_ndc(self.pos));
    }
}

/// 两屏 VDC 交界路标（AC-13 边缘插值）。
///
/// 语义：目标在另一显示器时，先水平/垂直走到两屏交界，再跨到目标屏——
///   - 水平相邻（y 区间重叠）：交界 x = 两对向边中点（相邻屏恰为共享边），
///     Y 钳到两屏 y 重叠区间内；
///   - 垂直相邻（x 区间重叠）：交界 y 同理，X 钳到两屏 x 重叠区间内；
///   - 斜对角（防御兜底）：取两屏对向边中点组合。
#[must_use]
pub(crate) fn seam_waypoint(a: &MonitorGeom, b: &MonitorGeom, from: Vec2) -> Vec2 {
    let ox_min = a.origin_vdc.x.max(b.origin_vdc.x);
    let ox_max = a.right_vdc().min(b.right_vdc());
    let oy_min = a.origin_vdc.y.max(b.origin_vdc.y);
    let oy_max = a.bottom_vdc().min(b.bottom_vdc());
    let overlap_x = ox_max > ox_min;
    let overlap_y = oy_max > oy_min;
    if overlap_x && overlap_y {
        // 显示器矩形重叠（异常叠屏，防御）：无需边界点，直走即可。
        return from;
    }
    if overlap_y {
        // 水平相邻：交界 x = 对向边（a 右缘 / b 左缘，反向亦然）中点。
        let seam_x = if b.origin_vdc.x >= a.right_vdc() {
            (a.right_vdc() + b.origin_vdc.x) / 2.0
        } else {
            (b.right_vdc() + a.origin_vdc.x) / 2.0
        };
        return Vec2::new(seam_x, from.y.clamp(oy_min, oy_max));
    }
    if overlap_x {
        // 垂直相邻：交界 y = 对向边（a 下缘 / b 上缘，反向亦然）中点。
        let seam_y = if b.origin_vdc.y >= a.bottom_vdc() {
            (a.bottom_vdc() + b.origin_vdc.y) / 2.0
        } else {
            (b.bottom_vdc() + a.origin_vdc.y) / 2.0
        };
        return Vec2::new(from.x.clamp(ox_min, ox_max), seam_y);
    }
    // 斜对角（防御兜底）：两屏对向边中点组合。
    let seam_x = (a.right_vdc() + b.origin_vdc.x) / 2.0;
    let seam_y = (a.bottom_vdc() + b.origin_vdc.y) / 2.0;
    Vec2::new(seam_x, seam_y)
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 主屏：VDC (0,0) 1920×1080，工作区底边 1040。
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

    /// 左侧副屏：VDC x ∈ [-1920, 0)（负坐标口径）。
    fn mon_b() -> MonitorGeom {
        MonitorGeom {
            id: 2,
            origin_vdc: Vec2::new(-1920.0, 0.0),
            size_vdc: Vec2::new(1920.0, 1080.0),
            work_origin_vdc: Vec2::new(-1920.0, 0.0),
            work_size_vdc: Vec2::new(1920.0, 1040.0),
            primary: false,
        }
    }

    /// 测试配置工厂。
    fn cfg(interval_sec: [u64; 2], pace: f32, speed: f32) -> RoamCfg {
        RoamCfg {
            pace_options: crate::config::model::RoamCfg::default().pace_options,
            pace,
            decision_interval_sec: interval_sec,
            cursor_avoid_radius_px: 150,
            walk_speed_px_per_sec: speed,
        }
    }

    // -- 决策间隔绝对锚定（C3 红线：无累积漂移） ---------------------------------

    #[test]
    fn decision_deadlines_absolutely_anchored_no_drift() {
        // 固定 1s 间隔；每 tick 迟到 500ms 触发。绝对锚定下 deadline 链恒为
        // 1000, 2000, …；若按「实际触发时刻 + 间隔」累加则会漂移到 11500。
        let mut eng = MotionEngine::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a(), mon_b()],
            cfg([1, 1], 1.0, 1_000_000.0),
            42,
            0,
        );
        assert_eq!(eng.next_decision_ms(), 1000, "首个 deadline = 启动 + 1s");
        let mut decided = 0usize;
        for i in 1..=10u64 {
            let events = eng.tick(i * 1000 + 500);
            decided += events
                .iter()
                .filter(|e| matches!(e, MotionEvent::TargetDecided { .. }))
                .count();
        }
        assert_eq!(decided, 10, "每次迟到触发均应完成一次决策");
        assert_eq!(eng.next_decision_ms(), 11000, "deadline 链绝对锚定，无迟到漂移");
    }

    #[test]
    fn pace_scales_first_decision_deadline() {
        // pace 2.0 → 间隔减半；pace 0.5 → 间隔加倍（roam.rs 缩放方向的引擎侧验证）。
        let fast = MotionEngine::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            cfg([2, 2], 2.0, 60.0),
            7,
            0,
        );
        assert_eq!(fast.next_decision_ms(), 1000, "pace 2.0 → 2s 间隔减半为 1s");
        let slow = MotionEngine::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            cfg([2, 2], 0.5, 60.0),
            7,
            0,
        );
        assert_eq!(slow.next_decision_ms(), 4000, "pace 0.5 → 2s 间隔加倍为 4s");
    }

    // -- 跨屏（AC-13）：路径经过边界、逐 tick 步进无瞬移 ---------------------------

    #[test]
    fn cross_monitor_walk_passes_boundary_without_teleport() {
        // 主屏 (0..1920) → 左副屏 (-1920..0)，交界 x = 0。
        let mut eng = MotionEngine::new(
            Vec2::new(1000.0, 1040.0),
            vec![mon_a(), mon_b()],
            cfg([5, 30], 1.0, 1000.0),
            42,
            0,
        );
        assert!(eng.walk_to(Vec2::new(-960.0, 1040.0)), "双屏下 walk_to 应成功");
        let mut positions = Vec::new();
        let mut arrived = false;
        for i in 1..=30u64 {
            for e in eng.tick(i * 100) {
                if matches!(e, MotionEvent::Arrived { .. }) {
                    arrived = true;
                }
            }
            positions.push(eng.pos());
        }
        assert!(arrived, "30 tick 内应到达目标");
        assert!(
            (eng.pos().x + 960.0).abs() < 1e-3 && (eng.pos().y - 1040.0).abs() < 1e-3,
            "终点为目标点"
        );
        // AC-13：中间采样点必须经过两屏 VDC 交界（x = 0）。
        assert!(
            positions.iter().any(|p| p.x.abs() < 1e-3),
            "跨屏路径必须经过边界 x=0"
        );
        // 无瞬移：相邻采样点位移 ≤ speed × dt（1000 px/s × 100ms = 100px）。
        for w in positions.windows(2) {
            let d = w[0].distance(w[1]);
            assert!(d <= 100.0 + 1e-2, "逐 tick 步进无瞬移：d={d}");
        }
    }

    #[test]
    fn seam_waypoint_horizontal_adjacency_clamps_y_into_overlap() {
        // 主屏 → 左副屏：交界 x = 0（共享边），Y 钳到重叠区间 [0, 1080]。
        let wp = seam_waypoint(&mon_a(), &mon_b(), Vec2::new(1000.0, 1040.0));
        assert!(wp.x.abs() < 1e-6, "交界 x = 两屏共享边：{}", wp.x);
        assert!((wp.y - 1040.0).abs() < 1e-6, "Y 保持两屏重叠区间内的行走线");
        // 反向：副屏 → 主屏。
        let wp = seam_waypoint(&mon_b(), &mon_a(), Vec2::new(-1000.0, 500.0));
        assert!(wp.x.abs() < 1e-6, "反向交界 x 仍为共享边");
        assert!((wp.y - 500.0).abs() < 1e-6);
    }

    #[test]
    fn seam_waypoint_vertical_adjacency_crosses_at_shared_edge() {
        // 上方副屏：VDC y ∈ [-1080, 0)，与主屏 x 区间重叠 → 垂直相邻。
        let mut top = mon_a();
        top.id = 3;
        top.origin_vdc = Vec2::new(0.0, -1080.0);
        top.work_origin_vdc = Vec2::new(0.0, -1080.0);
        top.work_size_vdc = Vec2::new(1920.0, 1040.0);
        let wp = seam_waypoint(&mon_a(), &top, Vec2::new(500.0, 1040.0));
        assert!(wp.y.abs() < 1e-6, "交界 y = 两屏共享边（主屏上缘 0）：{}", wp.y);
        assert!((wp.x - 500.0).abs() < 1e-6, "X 钳到两屏 x 重叠区间");
    }

    // -- 显示器变更迁移（FR-1-4） --------------------------------------------------

    #[test]
    fn monitor_removed_migrates_to_nearest_within_2s() {
        let mut eng = MotionEngine::new(
            Vec2::new(-960.0, 1040.0),
            vec![mon_a(), mon_b()],
            cfg([5, 30], 1.0, 60.0),
            42,
            0,
        );
        assert_eq!(eng.current_monitor_id(), 2, "初始在左副屏（负坐标）");
        // 拔掉所在屏：deadline = 变更时刻 + 2000ms（绝对锚定）。
        eng.on_monitors_changed(vec![mon_a()], 1000);
        assert_eq!(eng.migration_deadline_ms(), Some(3000));
        // 2s 内不迁移。
        let events = eng.tick(2000);
        assert!(
            events.iter().all(|e| !matches!(e, MotionEvent::Migrated { .. })),
            "2s 内不得提前迁移"
        );
        assert!((eng.pos().x + 960.0).abs() < 1e-3, "迁移前位置不变");
        // 到期迁移到剩余屏工作区中心。
        let events = eng.tick(3000);
        assert!(events.iter().any(|e| matches!(e, MotionEvent::Migrated { .. })));
        assert!(
            (eng.pos().x - 960.0).abs() < 1e-3 && (eng.pos().y - 520.0).abs() < 1e-3,
            "迁移到剩余屏（主屏）工作区中心"
        );
        assert_eq!(eng.current_monitor_id(), 1);
        assert_eq!(eng.migration_deadline_ms(), None, "迁移完成清 deadline");
    }

    #[test]
    fn monitor_geometry_change_recomputes_vdc_from_ndc() {
        let mut eng = MotionEngine::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            cfg([5, 30], 1.0, 60.0),
            42,
            0,
        );
        let ndc = eng.home_ndc();
        assert!((ndc.x - 0.5).abs() < 1e-6);
        assert!((ndc.y - 1040.0 / 1080.0).abs() < 1e-6, "NDC 快照基于显示器矩形");
        // 当前屏还在（分辨率变更 1920×1080 → 1280×720）：NDC 重算 VDC，不迁移。
        let mut changed = mon_a();
        changed.size_vdc = Vec2::new(1280.0, 720.0);
        changed.work_size_vdc = Vec2::new(1280.0, 700.0);
        eng.on_monitors_changed(vec![changed], 1000);
        assert_eq!(eng.migration_deadline_ms(), None, "当前显示器还在 → 不迁移");
        assert!((eng.pos().x - 640.0).abs() < 1e-2, "NDC 重算 VDC 平移");
        assert!((eng.pos().y - 700.0).abs() < 1e-2, "重算后钳制到新工作区底边");
        assert_eq!(eng.current_monitor_id(), 1);
    }

    // -- VDC 负坐标（左侧副屏） ---------------------------------------------------

    #[test]
    fn negative_vdc_left_monitor_stand_and_walk() {
        let mut eng = MotionEngine::new(
            Vec2::new(-960.0, 900.0),
            vec![mon_b()],
            cfg([5, 30], 1.0, 1000.0),
            42,
            0,
        );
        assert_eq!(eng.current_monitor_id(), 2, "负坐标点归属左副屏");
        assert!((eng.pos().y - 1040.0).abs() < 1e-3, "构造时钳制到工作区底边");
        assert!(eng.walk_to(Vec2::new(-1800.0, 900.0)));
        for i in 1..=20u64 {
            let _ = eng.tick(i * 100);
        }
        assert!(
            (eng.pos().x + 1800.0).abs() < 1e-3 && (eng.pos().y - 1040.0).abs() < 1e-3,
            "负坐标 VDC 行走终点正确"
        );
    }

    // -- 光标热区决策避让（K-3） ---------------------------------------------------

    #[test]
    fn decision_rejects_targets_inside_cursor_heat() {
        let mut eng = MotionEngine::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            cfg([1, 1], 1.0, 60.0),
            7,
            0,
        );
        eng.set_cursor(Some(Vec2::new(960.0, 1040.0)));
        let events = eng.tick(1500);
        let target = events.iter().find_map(|e| match e {
            MotionEvent::TargetDecided { target, .. } => Some(*target),
            _ => None,
        });
        let target = target.expect("空旷地面应决策出目标");
        let heat = CursorHeat::new(Vec2::new(960.0, 1040.0), 150.0);
        assert!(!heat.contains(target), "K-3：目标不得落在光标热区内");
    }

    #[test]
    fn decision_emits_idle_fallback_when_no_stand_surface() {
        let mut eng =
            MotionEngine::new(Vec2::new(0.0, 0.0), Vec::new(), cfg([1, 1], 1.0, 60.0), 7, 0);
        let events = eng.tick(1500);
        assert!(
            events.iter().any(|e| matches!(e, MotionEvent::IdleFallback)),
            "无显示器/无站立面 → IdleFallback（上层回退 idle）"
        );
        // 事件式解耦：引擎不直接提交/打断动作（S2-M3 边界）——由返回值表达。
    }

    // -- 行走防御与 walk_to 边界 ---------------------------------------------------

    #[test]
    fn degenerate_speed_falls_back_to_default_and_walk_to_without_monitors_fails() {
        // 速度非法（0）→ 回退默认 60 px/s；仍能逐 tick 到达近处目标。
        let mut eng = MotionEngine::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            cfg([5, 30], 1.0, 0.0),
            42,
            0,
        );
        assert!(eng.walk_to(Vec2::new(1000.0, 1040.0)));
        for i in 1..=30u64 {
            let _ = eng.tick(i * 100);
        }
        assert!((eng.pos().x - 1000.0).abs() < 1e-3, "默认速度下 30 tick 内到达");

        // 无显示器 → walk_to 失败。
        let mut bare =
            MotionEngine::new(Vec2::new(0.0, 0.0), Vec::new(), cfg([5, 30], 1.0, 60.0), 42, 0);
        assert!(!bare.walk_to(Vec2::new(10.0, 10.0)), "无显示器 walk_to 返回 false");
    }
}
