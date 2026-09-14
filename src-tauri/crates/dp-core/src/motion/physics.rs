//! S2-M5 物理：重力掉落 / 落地缓冲 / 平台消失即坠（T-05 段，`03 台账 §2`）。
//!
//! 职责与对照条目：
//!   - **重力掉落**（`01 §6.2` FR-2-4）：非站定状态按固定子步 [`SUB_STEP_MS`]
//!     （`02 §5` K-3）做半隐式欧拉积分（先速度后位置），高速下坠被切成 10ms
//!     子步，防止单步大 dt 穿透站立面；
//!   - **重力单一真源**（RV-18 / C7）：重力一律读
//!     [`InteractionCfg::gravity_px_per_sec2`]（settings `gravityPxPerSec2`，
//!     默认值定义于 config 模型），本模块**零重力常数**（含测试）；
//!     非法配置（非有限 / ≤ 0）防御回退 `InteractionCfg::default()`；
//!   - **左右边界反弹**（`02 §5` K-3）：反弹系数 [`WALL_BOUNCE_RESTITUTION`]，
//!     边界 x 区间由调用方注入（[`HorizontalBounds`]，即 K-3 伪代码
//!     `vd.left` / `vd.right` 口径）；
//!   - **平台消失即坠 / 落地**（AC-08）：站定中每 tick 校验站立面，不合法
//!     （关窗 / 平台消失）当帧起坠；下坠中 [`StandSurface::clamp_to_stand`]
//!     投影点合法且越过顶面即落地，产出 [`MotionEvent::Landed`]（`impact` =
//!     落地前 |vel.y|）。
//!
//! 对接契约（S2-M3 事件式解耦，物理模块不 import 仲裁器）：
//!   上层驱动循环（S7-M3）收到 `MotionEvent::Landed { impact }` 后，经
//!   `ActionArbiter::submit`（`dp-core/src/anim/arbiter.rs`，
//!   `ActionSource::Motion`）提交 **ACT-M-06 落地缓冲**（`01 §6.3` 动作表：
//!   8 帧 / 15fps 单次动作）；落地缓冲期间宠物 grounded 静止。物理只负责
//!   「何时落地 + 冲击多大」，不直接提交/打断动作。
//!
//! 落地端口语义假设：[`StandSurface::clamp_to_stand`](p) 返回「p.x 处应站立
//! 的顶面点」（返回 y = 顶面线）。`DesktopFloor` 与 S2-M6 PlatformGraph 均按
//! 此端口实现，落地判定自动兼容窗口标题栏平台。
//!
//! 架构硬约束：
//!   - **时间纪律（C3）**：模块内零时钟——所有时间由调用方注入单调毫秒
//!     `now_ms: u64`（首 tick dt = 0，仿 `MotionEngine` 的 `last_tick_ms`
//!     模式）；
//!   - **零新增依赖**：仅 std + 本 crate 已有类型（[`Vec2`] /
//!     [`StandSurface`] / [`InteractionCfg`] / [`MotionEvent`]）；
//!   - **与 `MotionEngine` 解耦**：物理为独立状态机，不感知漫游/行走逻辑，
//!     由上层驱动循环组合驱动。

use super::engine::MotionEvent;
use super::roam::StandSurface;
use super::Vec2;
use crate::config::model::InteractionCfg;

/// 左右反弹边界（VDC x 区间；`02 §5` K-3 伪代码 `vd.left` / `vd.right` 口径，
/// 由调用方注入——物理模块不感知虚拟桌面拓扑）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HorizontalBounds {
    /// 左边界（VDC x，含）。
    pub left: f32,
    /// 右边界（VDC x，含）。
    pub right: f32,
}

/// 固定积分子步长（毫秒，`02 §5` K-3）：下坠按 10ms 子步切片，防高速穿透。
pub const SUB_STEP_MS: u64 = 10;

/// 单 tick 允许推进的最大时长（毫秒）。
///
/// 长停顿（系统休眠/唤醒、主线程长阻塞、首帧卡顿）会让调用方注入的
/// `now_ms` 差值达到数十分钟甚至数小时，而 [`PhysicsEngine::tick`] 的
/// 子步数为 `dt / SUB_STEP_MS`（1 小时 ≈ 36 万次），既无物理意义也会
/// 卡死主线程。
///
/// 语义裁定：**长停顿按最大步长补做一次，不做历史重演**——宠物最多按
/// 1s 的下坠量继续运动，跨越的空白期不追补。这与 S2-M4/S2-M6 的
/// 「绝对时间锚定」不冲突：锚定保证的是 deadline 链不漂移，本常量限制
/// 的是单帧补做量。
///
/// 定值 1000ms 的依据：①子步数上限 `1000 / 10 = 100`，计算量可忽略，
/// 足以消灭「休眠唤醒 → 36 万子步卡死主线程」；②覆盖最长合理单帧——
/// 掉帧 / 首帧 / GC 停顿，且**不小于**默认重力下从屏幕顶落到底的耗时
/// （≈645ms），否则正常掉帧会被误伤成「本 tick 落不了地」。
pub const MAX_TICK_DT_MS: u64 = 1000;

/// 左右边界反弹系数（`02 §5` K-3 明确参数；非重力常数，允许常量化）。
pub const WALL_BOUNCE_RESTITUTION: f32 = 0.4;

/// 物理引擎（S2-M5）：重力掉落 / 左右反弹 / 落地判定。
///
/// 独立于 `MotionEngine` 的状态机：上层驱动循环（S7-M3）组合两者
/// （运动决策 + 物理），物理不感知漫游/行走逻辑。
#[derive(Debug)]
pub struct PhysicsEngine {
    /// 当前位置（VDC）。
    pos: Vec2,
    /// 当前速度（VDC px/s）。
    vel: Vec2,
    /// 是否站定（grounded 短路：站定时每 tick 仅校验站立面，不积分重力）。
    grounded: bool,
    /// 配置真源（RV-18：重力读 `gravity_px_per_sec2`，本模块零硬编码）。
    cfg: InteractionCfg,
    /// 左右反弹边界。
    bounds: HorizontalBounds,
    /// 上一次 tick 的 `now_ms`（None = 尚未 tick，首 tick dt = 0；C3 零时钟）。
    last_tick_ms: Option<u64>,
}

impl PhysicsEngine {
    /// 构造物理引擎。
    ///
    /// `grounded` 初始为 true——信任调用方给的 `pos` 是站立点；若该点实际
    /// 不合法，首个 tick 的站立面校验会自然起坠（防御性自愈）。
    pub fn new(pos: Vec2, cfg: InteractionCfg, bounds: HorizontalBounds) -> Self {
        Self { pos, vel: Vec2::ZERO, grounded: true, cfg, bounds, last_tick_ms: None }
    }

    /// 甩出构造入口（S3-M4）：以初速度直接构造**下坠态**引擎（抛物线飞行）。
    ///
    /// 与 [`PhysicsEngine::set_velocity`] 的区别：`set_velocity` 需调用方先确保
    /// 已处于下坠态（grounded 且站立面合法时，下一 tick 的站定分支会把速度
    /// 归零——见其文档），本构造函数直接以 `grounded = false` 起步，拖拽甩出
    /// 后**无需任何前置状态操作**。初速矢量（VDC px/s，y 向下为正）直接参与
    /// 半隐式欧拉积分与左右边界反弹（积分逻辑零改动）。
    ///
    /// `last_tick_ms = None`：首 tick dt = 0 无操作（C3，同 [`PhysicsEngine::new`]
    /// 口径），自第二个 tick 起按初速积分。
    ///
    /// 「甩出后 1.5s 内完成落地」约束（FR-4-6）由**调用方**钳制向上初速分量
    /// ——物理模块无屏幕高度概念（K-3 纯运动学口径），钳制落在持有显示器
    /// 几何的 dp-app 层（`THROW_MAX_FLIGHT_MS` 预算 + 闭式解钳制）。
    #[must_use]
    pub fn thrown(pos: Vec2, vel: Vec2, cfg: InteractionCfg, bounds: HorizontalBounds) -> Self {
        Self { pos, vel, grounded: false, cfg, bounds, last_tick_ms: None }
    }

    /// 当前位置（VDC）。
    #[must_use]
    pub const fn pos(&self) -> Vec2 {
        self.pos
    }

    /// 当前速度（VDC px/s）。
    #[must_use]
    pub const fn vel(&self) -> Vec2 {
        self.vel
    }

    /// 是否站定。
    #[must_use]
    pub const fn grounded(&self) -> bool {
        self.grounded
    }

    /// 更新左右反弹边界（显示器 / 虚拟桌面拓扑变更时调用）。
    pub fn set_bounds(&mut self, bounds: HorizontalBounds) {
        self.bounds = bounds;
    }

    /// 注入速度（整体覆盖当前速度）。
    ///
    /// S3-M4 拖拽甩出将复用此入口；本卡片测试的反弹/防穿透亦经此注入。
    /// 注意：grounded 且站立面合法时，下一 tick 站定分支会把速度归零
    /// （站定不滑移）——注入初速前应确保已处于下坠态。
    pub fn set_velocity(&mut self, vel: Vec2) {
        self.vel = vel;
    }

    /// 推进一个 tick（C3 零时钟：`now_ms` 由调用方注入，单调毫秒）。
    ///
    /// 语义（`02 §5` K-3）：
    /// 1. dt = 0（首 tick / 时钟回拨防御）→ 无操作，返回空；
    /// 2. 站定中：每 tick 校验站立面——仍合法则什么都不做（站定不累积重力，
    ///    速度保持零）；不合法（平台消失 / 关窗，AC-08）→ 置零初速转入下坠，
    ///    并继续落入本 tick 的积分（平台消失当帧就开始坠）；
    /// 3. 下坠：按 [`SUB_STEP_MS`] 固定子步积分，含左右边界反弹与落地判定；
    ///    落地产出 [`MotionEvent::Landed`] 后本 tick 剩余子步跳过。
    ///
    /// dt 上限：超过 [`MAX_TICK_DT_MS`] 的停顿被钳制（长停顿补做一次，
    /// 不重演历史），子步数因此恒 ≤ `MAX_TICK_DT_MS / SUB_STEP_MS`。
    pub fn tick(&mut self, now_ms: u64, surface: &dyn StandSurface) -> Vec<MotionEvent> {
        let mut events = Vec::new();
        let raw_dt_ms = self.last_tick_ms.map_or(0, |prev| now_ms.saturating_sub(prev));
        self.last_tick_ms = Some(now_ms);
        let dt_ms = raw_dt_ms.min(MAX_TICK_DT_MS);
        if dt_ms == 0 {
            return events;
        }
        if self.grounded {
            if surface.is_valid_stand(self.pos) {
                // 站定：不积分、不累积重力，速度保持零。
                self.vel = Vec2::ZERO;
                return events;
            }
            // 平台消失 / 关窗（AC-08）：自由落体起手，无初速，当帧起坠。
            self.grounded = false;
            self.vel = Vec2::ZERO;
        }
        self.integrate(dt_ms, surface, &mut events);
        events
    }

    /// 下坠积分（忠实 `02 §5` K-3 伪代码语义；先速度后位置的半隐式欧拉）。
    fn integrate(
        &mut self,
        dt_ms: u64,
        surface: &dyn StandSurface,
        events: &mut Vec<MotionEvent>,
    ) {
        let mut remain = dt_ms;
        while remain > 0 {
            let h = SUB_STEP_MS.min(remain);
            let dt = h as f32 / 1000.0;
            // 半隐式欧拉：先更新速度，再以新速度推进位置（手写分量运算，
            // Vec2 无 Mul——不新增向量依赖）。
            self.vel.y += self.effective_gravity() * dt;
            self.pos.x += self.vel.x * dt;
            self.pos.y += self.vel.y * dt;

            // 左右边界反弹（先 x 后落地检测，与 02 伪代码顺序一致）。
            if self.pos.x < self.bounds.left {
                self.pos.x = self.bounds.left;
                self.vel.x = self.vel.x.abs() * WALL_BOUNCE_RESTITUTION;
            }
            if self.pos.x > self.bounds.right {
                self.pos.x = self.bounds.right;
                self.vel.x = -self.vel.x.abs() * WALL_BOUNCE_RESTITUTION;
            }

            // 落地检测：`clamp_to_stand(p).y` = p.x 处应站立的顶面 y（端口
            // 语义假设见模块文档，S2-M6 PlatformGraph 实现同一端口后自动
            // 兼容标题栏平台）。`is_valid_stand(landing)` 前置检查 =「无任何
            // 合法站立面则永不落地继续坠」的防御（空 DesktopFloor 时钳制点
            // 非法，若不设防会立即「落在自己身上」）。
            let landing = surface.clamp_to_stand(self.pos);
            if surface.is_valid_stand(landing) && self.pos.y >= landing.y {
                // 冲击在置零前采样：落地前 |vel.y|（VDC px/s），上层据此经
                // 仲裁器提交 ACT-M-06 落地缓冲。
                let impact = self.vel.y.abs();
                // 只吸收 y（x 保留积分结果，反弹边界已管住 x；DesktopFloor
                // 钳制的 x 副作用不扭曲物理）。
                self.pos.y = landing.y;
                // 落地站住：vel.x 一并清零（02 伪码只清 vel.y；遗留水平速度
                // 会在下次「平台消失即坠」时产生意外横移，台账登记该决策）。
                self.vel = Vec2::ZERO;
                self.grounded = true;
                events.push(MotionEvent::Landed { impact });
                break; // 02 伪代码同款：本 tick 剩余子步跳过。
            }
            remain -= h;
        }
    }

    /// 有效重力（VDC px/s²）：单一真源 `cfg.gravity_px_per_sec2`（RV-18）；
    /// 非有限 / ≤ 0 → 防御回退默认配置值。
    fn effective_gravity(&self) -> f32 {
        let g = self.cfg.gravity_px_per_sec2;
        if g.is_finite() && g > 0.0 {
            g
        } else {
            InteractionCfg::default().gravity_px_per_sec2
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::motion::roam::{DesktopFloor, MonitorGeom, STAND_EPS_PX};
    use super::*;
    use std::cell::Cell;

    /// 主屏：VDC (0,0) 1920×1080，工作区底边 1040（与 engine.rs 测试夹具同款）。
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

    /// 兜底地面（工作区底边 1040）。
    fn floor_a() -> DesktopFloor {
        DesktopFloor::new(vec![mon_a()])
    }

    /// 全屏 x 反弹边界。
    fn bounds_a() -> HorizontalBounds {
        HorizontalBounds { left: 0.0, right: 1920.0 }
    }

    /// 从空中某点构造引擎并推进到下坠状态：首 tick dt=0 无操作，第二 tick
    /// 站立校验失败起坠（10ms 自由落体）。返回值已处于下坠中。
    fn falling_engine(start: Vec2, cfg: InteractionCfg, bounds: HorizontalBounds) -> PhysicsEngine {
        let mut eng = PhysicsEngine::new(start, cfg, bounds);
        eng.tick(1000, &floor_a());
        eng.tick(1010, &floor_a());
        eng
    }

    // -- 自由落体运动学（重力读配置真源，RV-18） ---------------------------------

    #[test]
    fn free_fall_velocity_linear_and_matches_config_gravity() {
        // 空地面（永不落地）：纯运动学。半隐式欧拉速度线性：vel.y ≈ g·t。
        let floor = DesktopFloor::new(Vec::new());
        let mut eng = falling_engine(Vec2::new(960.0, 500.0), InteractionCfg::default(), bounds_a());
        // 再推进 99 个 10ms tick → 总下坠 1s = 100 个子步。
        for i in 1..=99u64 {
            eng.tick(1010 + i * 10, &floor);
        }
        let g = InteractionCfg::default().gravity_px_per_sec2;
        let expect = g * 1.0;
        assert!(
            (eng.vel().y - expect).abs() < 1e-1,
            "vel.y 应为 g·t：{} vs {expect}",
            eng.vel().y
        );
        assert!(eng.pos().y > 600.0, "1s 自由落体位移显著");
    }

    #[test]
    fn gravity_config_change_scales_fall_displacement_ac08b() {
        // 两引擎同 dt 同起点：一个用默认配置，一个把重力改为默认的 1/5
        //（显式改值，不在物理代码出现重力字面量）。下落位移应成同比例
        // → 证明重力读配置而非硬编码（AC-08b）。
        let floor = DesktopFloor::new(Vec::new());
        let weak_cfg = InteractionCfg {
            gravity_px_per_sec2: InteractionCfg::default().gravity_px_per_sec2 / 5.0,
            ..Default::default()
        };
        let mut strong =
            falling_engine(Vec2::new(960.0, 500.0), InteractionCfg::default(), bounds_a());
        let mut weak = falling_engine(Vec2::new(960.0, 500.0), weak_cfg, bounds_a());
        for i in 1..=50u64 {
            let t = 1010 + i * 20;
            strong.tick(t, &floor);
            weak.tick(t, &floor);
        }
        let d_strong = strong.pos().y - 500.0;
        let d_weak = weak.pos().y - 500.0;
        assert!(d_strong > 0.0 && d_weak > 0.0, "两者均应下落：{d_strong} / {d_weak}");
        let ratio = d_strong / d_weak;
        assert!((ratio - 5.0).abs() < 1e-2, "位移比应等于重力比 5：{ratio}");
    }

    // -- dt 护栏（Q1 回归：长停顿不重演历史） ------------------------------------

    #[test]
    fn huge_dt_is_clamped_to_max_tick_not_replayed() {
        // 休眠/唤醒后注入的 dt 可达 1 小时 = 36 万子步。钳制语义 = 按
        // MAX_TICK_DT_MS 补做一次，因此与「连续推进 MAX_TICK_DT_MS」等价。
        let floor = DesktopFloor::new(Vec::new()); // 空地面：永不落地，纯运动学
        let start = Vec2::new(960.0, 500.0);
        let steps = MAX_TICK_DT_MS / SUB_STEP_MS;

        // A：正常连续推进（steps 次 × SUB_STEP_MS）。
        let mut a = falling_engine(start, InteractionCfg::default(), bounds_a());
        for i in 1..=steps {
            a.tick(1010 + i * SUB_STEP_MS, &floor);
        }

        // B：一次注入 1 小时的巨大 dt。
        let mut b = falling_engine(start, InteractionCfg::default(), bounds_a());
        b.tick(1010 + 3_600_000, &floor);

        let d_a = a.pos().y - start.y;
        let d_b = b.pos().y - start.y;
        assert!(
            (d_a - d_b).abs() < 1e-3,
            "巨大 dt 应钳到 {MAX_TICK_DT_MS}ms 与连续推进等价：{d_a} vs {d_b}"
        );
        // 有界性：未钳制时 1 小时自由落体是天文数字，钳制后仅是 1s 的下坠量。
        assert!(d_b.is_finite() && d_b < 100_000.0, "钳制后位移应有界，实际 {d_b}");
    }

    #[test]
    fn extreme_now_ms_jump_does_not_explode_substeps() {
        // u64 邻界跳变：raw_dt 达 u64::MAX 量级，钳制后与「连续推进上限」同等。
        let floor = DesktopFloor::new(Vec::new());
        let start = Vec2::new(960.0, 500.0);
        let steps = MAX_TICK_DT_MS / SUB_STEP_MS;

        let mut baseline = falling_engine(start, InteractionCfg::default(), bounds_a());
        for i in 1..=steps {
            baseline.tick(1010 + i * SUB_STEP_MS, &floor);
        }

        let mut eng = falling_engine(start, InteractionCfg::default(), bounds_a());
        eng.tick(u64::MAX, &floor);
        let d = eng.pos().y - start.y;
        assert!(d.is_finite(), "极端跳变后位移应有限，实际 {d}");
        assert!(
            (d - (baseline.pos().y - start.y)).abs() < 1e-3,
            "极端跳变应钳到 {MAX_TICK_DT_MS}ms：{d} vs {}",
            baseline.pos().y - start.y
        );
        // last_tick_ms 已推进到 u64::MAX，重复同一时刻 → dt=0 无操作。
        let before = eng.pos().y;
        eng.tick(u64::MAX, &floor);
        assert_eq!(eng.pos().y, before, "同一 now_ms 重复 tick：dt=0 无操作");
    }

    // -- 平台消失即坠 / 恢复落地（AC-08a） ---------------------------------------

    /// 可开关的测试桩站立面：模拟窗口开/关（关 → 无任何合法站立点）。
    struct ToggleFloor {
        open: Cell<bool>,
        floor_y: f32,
    }

    impl ToggleFloor {
        fn new(floor_y: f32) -> Self {
            Self { open: Cell::new(true), floor_y }
        }
    }

    impl StandSurface for ToggleFloor {
        fn is_valid_stand(&self, p: Vec2) -> bool {
            self.open.get() && (p.y - self.floor_y).abs() <= STAND_EPS_PX
        }

        fn clamp_to_stand(&self, p: Vec2) -> Vec2 {
            Vec2::new(p.x, self.floor_y)
        }
    }

    #[test]
    fn platform_disappear_falls_then_relands_ac08a() {
        let surface = ToggleFloor::new(1040.0);
        let mut eng = PhysicsEngine::new(
            Vec2::new(500.0, 1040.0),
            InteractionCfg::default(),
            bounds_a(),
        );
        // 站定：无事件、位置不变、速度为零。
        eng.tick(0, &surface);
        let events = eng.tick(100, &surface);
        assert!(events.is_empty(), "站定无事件");
        assert!(eng.grounded());
        assert!((eng.pos().y - 1040.0).abs() < 1e-3);
        // 关窗：站立面消失 → 当帧转入下坠（grounded 翻转，无落地事件）。
        surface.open.set(false);
        let events = eng.tick(200, &surface);
        assert!(events.is_empty(), "起坠 tick 无落地事件");
        assert!(!eng.grounded(), "站立面消失当帧起坠");
        // 无站立面期间持续下坠（已坠过原地面线）。
        for i in 1..=10u64 {
            eng.tick(200 + i * 100, &surface);
        }
        assert!(!eng.grounded());
        assert!(eng.pos().y > 1040.0, "应已坠过原地面线：{}", eng.pos().y);
        // 窗口恢复（平台重现）→ 落地产生 Landed，落点钳回地面线。
        surface.open.set(true);
        let events = eng.tick(1400, &surface);
        assert_eq!(events.len(), 1, "恢复站立面应恰产出一次落地事件");
        assert!(
            matches!(events[0], MotionEvent::Landed { impact } if impact > 0.0),
            "Landed 事件应携带正冲击"
        );
        assert!(eng.grounded());
        assert!((eng.pos().y - 1040.0).abs() < 1e-3, "落点钳回地面线");
    }

    // -- 落地精确性与冲击采样 -----------------------------------------------------

    #[test]
    fn landing_precision_on_desktop_floor() {
        let floor = floor_a();
        let mut eng = falling_engine(Vec2::new(960.0, 540.0), InteractionCfg::default(), bounds_a());
        // 一次推进 10s：中途必落地并 break（本 tick 剩余子步跳过）。
        let events = eng.tick(11_010, &floor);
        assert_eq!(events.len(), 1, "一次 tick 内完成落地");
        let MotionEvent::Landed { impact } = events[0] else {
            panic!("应为 Landed 事件：{events:?}");
        };
        assert!(eng.grounded());
        assert!((eng.pos().y - 1040.0).abs() < 1e-3, "落点精确等于工作区底边");
        assert!((eng.pos().x - 960.0).abs() < 1e-3, "x 保留积分结果");
        assert_eq!(eng.vel(), Vec2::ZERO, "落地后速度清零");
        // impact = 置零前 |vel.y|：按同一积分格式独立复算（g 取配置真源）。
        let g = InteractionCfg::default().gravity_px_per_sec2;
        let h = SUB_STEP_MS as f32 / 1000.0;
        let dist = 1040.0 - 540.0;
        let mut vel = 0.0f32;
        let mut travelled = 0.0f32;
        while travelled < dist {
            vel += g * h;
            travelled += vel * h;
        }
        assert!(vel > 0.0, "复算落地速度应为正");
        assert!(
            (impact - vel).abs() < 1e-1,
            "impact 应为落地前 |vel.y|：{impact} vs {vel}"
        );
    }

    // -- 落地后站定（grounded 短路，无重力累积） ----------------------------------

    #[test]
    fn grounded_stationary_no_events_no_gravity_accumulation() {
        let floor = floor_a();
        let mut eng = PhysicsEngine::new(
            Vec2::new(960.0, 1040.0),
            InteractionCfg::default(),
            bounds_a(),
        );
        let start = eng.pos();
        for i in 1..=50u64 {
            let events = eng.tick(i * 100, &floor);
            assert!(events.is_empty(), "站定无事件");
        }
        assert!(eng.grounded());
        assert_eq!(eng.vel(), Vec2::ZERO, "站定不累积重力");
        assert_eq!(eng.pos(), start, "位置不变");
    }

    // -- 子步防穿透（大 dt + 高速下坠） -------------------------------------------

    #[test]
    fn substep_integration_prevents_floor_penetration() {
        let floor = floor_a();
        // 注入 5000 px/s 纵向初速：单步 200ms 会穿透地面数百像素；
        // 10ms 子步必须精确停在地面线。
        let mut eng = falling_engine(Vec2::new(960.0, 540.0), InteractionCfg::default(), bounds_a());
        eng.set_velocity(Vec2::new(0.0, 5000.0));
        let events = eng.tick(11_010, &floor);
        assert!(
            events.iter().any(|e| matches!(e, MotionEvent::Landed { .. })),
            "大步推进内应落地"
        );
        assert!(eng.grounded());
        assert!((eng.pos().y - 1040.0).abs() < 1e-3, "落点精确在地面线，不越过");
        assert_eq!(eng.vel(), Vec2::ZERO);
    }

    // -- 左右边界反弹（`02 §5` K-3：0.4 反弹系数） ---------------------------------

    #[test]
    fn wall_bounce_reflects_with_restitution_left_wall() {
        let floor = floor_a();
        let mut eng = falling_engine(Vec2::new(500.0, 500.0), InteractionCfg::default(), bounds_a());
        eng.set_velocity(Vec2::new(-1000.0, 0.0));
        // 向左飞，撞左墙 x=0 反弹：vel.x 反号且模 × WALL_BOUNCE_RESTITUTION。
        let mut bounce_tick_ms = 0u64;
        for i in 1..=100u64 {
            let t = 1010 + i * 10;
            eng.tick(t, &floor);
            if eng.vel().x > 0.0 {
                bounce_tick_ms = t;
                break;
            }
        }
        assert!(bounce_tick_ms > 0, "100 tick 内应撞左墙反弹");
        assert!((eng.pos().x - 0.0).abs() < 1e-3, "撞墙点钳在左边界");
        let expect = 1000.0 * WALL_BOUNCE_RESTITUTION;
        assert!(
            (eng.vel().x - expect).abs() < 1e-2,
            "反弹速度 = 原模 × 反弹系数：{} vs {expect}",
            eng.vel().x
        );
        // 反弹方向正确：下一 tick 向右回弹。
        eng.tick(bounce_tick_ms + 10, &floor);
        assert!(eng.pos().x > 0.0, "反弹后应向右离开左边界：{}", eng.pos().x);
    }

    #[test]
    fn falling_x_clamped_within_bounds_right_wall() {
        let floor = floor_a();
        let mut eng =
            falling_engine(Vec2::new(1900.0, 500.0), InteractionCfg::default(), bounds_a());
        eng.set_velocity(Vec2::new(10_000.0, 0.0));
        // 向右飞越右边界 → x 钳在右边界且反弹向左。
        let mut bounced = false;
        for i in 1..=100u64 {
            eng.tick(1010 + i * 10, &floor);
            if eng.vel().x < 0.0 {
                bounced = true;
                break;
            }
        }
        assert!(bounced, "应撞右墙并反弹");
        assert!((eng.pos().x - 1920.0).abs() < 1e-3, "x 钳在右边界");
        let expect = 10_000.0 * WALL_BOUNCE_RESTITUTION;
        assert!(
            (eng.vel().x + expect).abs() < 1e-2,
            "反弹速度向左且模 × 反弹系数：{}",
            eng.vel().x
        );
        // 反弹方向正确：下一 tick 向左回弹，x 不再越界。
        eng.tick(1030, &floor);
        assert!(eng.pos().x < 1920.0, "反弹后应向左离开右边界：{}", eng.pos().x);
        assert!(eng.pos().x >= 0.0, "x 恒在边界区间内");
    }

    // -- 防御路径 -----------------------------------------------------------------

    #[test]
    fn invalid_gravity_falls_back_to_default_behavior() {
        // gravity ≤ 0 / NaN → 防御回退默认配置：与 default 引擎逐 tick 一致。
        let floor = DesktopFloor::new(Vec::new());
        let start = Vec2::new(960.0, 500.0);
        let mut reference = falling_engine(start, InteractionCfg::default(), bounds_a());
        let mut zero = falling_engine(
            start,
            InteractionCfg { gravity_px_per_sec2: 0.0, ..Default::default() },
            bounds_a(),
        );
        let mut negative = falling_engine(
            start,
            InteractionCfg { gravity_px_per_sec2: -5.0, ..Default::default() },
            bounds_a(),
        );
        let mut nan = falling_engine(
            start,
            InteractionCfg { gravity_px_per_sec2: f32::NAN, ..Default::default() },
            bounds_a(),
        );
        for i in 1..=50u64 {
            let t = 1010 + i * 20;
            reference.tick(t, &floor);
            zero.tick(t, &floor);
            negative.tick(t, &floor);
            nan.tick(t, &floor);
        }
        assert!((zero.pos().y - reference.pos().y).abs() < 1e-3, "gravity=0 回退默认");
        assert!((negative.pos().y - reference.pos().y).abs() < 1e-3, "gravity<0 回退默认");
        assert!((nan.pos().y - reference.pos().y).abs() < 1e-3, "NaN 回退默认");
        assert!((zero.vel().y - reference.vel().y).abs() < 1e-1, "速度同默认");
        assert!(!reference.grounded() && !zero.grounded(), "均持续下坠");
    }

    #[test]
    fn empty_floor_never_lands_and_emits_no_events() {
        // 空 DesktopFloor：无任何合法站立面 → 持续下坠永不落地、事件恒空
        //（若落地判定缺少 is_valid_stand(landing) 前置检查，钳制点会被误判
        // 落地——此测试守护该防御）。
        let floor = DesktopFloor::new(Vec::new());
        let mut eng = falling_engine(Vec2::new(960.0, 500.0), InteractionCfg::default(), bounds_a());
        let mut events_total = 0usize;
        for i in 1..=100u64 {
            events_total += eng.tick(1010 + i * 100, &floor).len();
        }
        assert_eq!(events_total, 0, "无站立面永不落地，事件恒空");
        assert!(!eng.grounded());
        assert!(eng.pos().y > 600.0, "持续下坠位移显著：{}", eng.pos().y);
        assert!(eng.vel().y > 0.0, "仍在下坠");
    }

    #[test]
    fn first_tick_zero_dt_and_clock_regression_noop() {
        let floor = floor_a();
        // 站立点：首 tick dt=0 无操作。
        let mut stand = PhysicsEngine::new(
            Vec2::new(960.0, 1040.0),
            InteractionCfg::default(),
            bounds_a(),
        );
        let events = stand.tick(0, &floor);
        assert!(events.is_empty());
        assert_eq!(stand.pos(), Vec2::new(960.0, 1040.0));
        assert_eq!(stand.vel(), Vec2::ZERO);
        assert!(stand.grounded());
        // 时钟回拨防御：dt 经 saturating_sub 归零 → 无操作。
        let events = stand.tick(50, &floor);
        assert!(events.is_empty());
        let events = stand.tick(30, &floor);
        assert!(events.is_empty(), "时钟回拨应无操作");
        assert_eq!(stand.pos(), Vec2::new(960.0, 1040.0));
        // 空中点：首 tick dt=0 不积分、不起坠（无操作语义）。
        let mut air =
            PhysicsEngine::new(Vec2::new(960.0, 500.0), InteractionCfg::default(), bounds_a());
        let events = air.tick(0, &floor);
        assert!(events.is_empty());
        assert_eq!(air.pos(), Vec2::new(960.0, 500.0));
        assert!(air.grounded(), "dt=0 不触发起坠转换");
    }

    // -- QA 独立验证补充探针（S2-M5 验证轮追加；实现零改动） --------------------
    // 以下 4 例为 QA 边界探查补充：现有 11 例未覆盖的 u64 邻界、长时程站定、
    // 退化边界区间、NaN 直接传播断言。全部只依赖公共行为，不白盒实现。

    #[test]
    fn probe_u64_boundary_clock_saturating() {
        // 理由：first_tick_zero_dt_and_clock_regression_noop 仅覆盖普通值域的
        // 回拨；本探针验证 u64 邻界（接近 u64::MAX）处单调推进正常积分、回拨
        // 仍走 saturating 归零无操作，不回绕不 panic（边界探查项：u64 邻界）。
        let floor = floor_a();
        let max = u64::MAX;
        // 站立点邻界推进：dt=10ms 正常，站定分支无事件。
        let mut stand = PhysicsEngine::new(
            Vec2::new(960.0, 1040.0),
            InteractionCfg::default(),
            bounds_a(),
        );
        let events = stand.tick(max - 20, &floor);
        assert!(events.is_empty(), "u64 邻界首 tick（dt=0）无操作");
        let events = stand.tick(max - 10, &floor);
        assert!(events.is_empty());
        let events = stand.tick(max, &floor);
        assert!(events.is_empty());
        assert!(stand.grounded());
        assert_eq!(stand.pos(), Vec2::new(960.0, 1040.0));
        // 邻界回拨：dt saturating 归零 → 无操作。
        let events = stand.tick(max - 30, &floor);
        assert!(events.is_empty(), "u64 邻界回拨应无操作");
        assert_eq!(stand.pos(), Vec2::new(960.0, 1040.0));
        // 空中点邻界推进：dt 正常积分（201 子步 = 2.01s，见下方速度断言）。
        let mut air =
            PhysicsEngine::new(Vec2::new(960.0, 500.0), InteractionCfg::default(), bounds_a());
        let no_floor = DesktopFloor::new(Vec::new());
        let events = air.tick(max - 20, &no_floor);
        assert!(events.is_empty(), "邻界首 tick dt=0 不起坠");
        assert!(air.grounded(), "dt=0 不触发起坠转换");
        air.tick(max - 10, &no_floor);
        air.tick(max, &no_floor);
        assert!(!air.grounded(), "空中点邻界正常推进应起坠");
        let g = InteractionCfg::default().gravity_px_per_sec2;
        let expected = g * (2.0 * SUB_STEP_MS as f32 / 1000.0);
        assert!(
            (air.vel().y - expected).abs() < 1e-1,
            "邻界推进速度应与普通值域一致：{} vs {expected}",
            air.vel().y
        );
    }

    #[test]
    fn probe_long_stationary_no_drift_10k_ticks() {
        // 理由：grounded_stationary_no_events_no_gravity_accumulation 仅 50 tick；
        // 本探针以 10_000 tick（模拟 ~17 分钟 @100ms/tick）验证站定分支长期
        // 零漂移、零事件、零速度累积（边界探查项：grounded 站定长期 tick 无漂移）。
        let floor = floor_a();
        let mut eng = PhysicsEngine::new(
            Vec2::new(960.0, 1040.0),
            InteractionCfg::default(),
            bounds_a(),
        );
        let start = eng.pos();
        for i in 1..=10_000u64 {
            let events = eng.tick(i * 100, &floor);
            assert!(events.is_empty(), "tick {i}：站定不应有事件");
        }
        assert!(eng.grounded());
        assert_eq!(eng.vel(), Vec2::ZERO, "站定不累积重力");
        assert_eq!(eng.pos(), start, "10k tick 站定零漂移");
    }

    #[test]
    fn probe_xbounds_degenerate_no_panic() {
        // 理由：HorizontalBounds 由调用方注入（set_bounds 随显示器拓扑变更），
        // 退化区间（left==right / left>right）属异常输入：物理为纯计算，不应
        // panic / 死循环 / 产生 NaN，位置与速度保持有限（边界探查项：XBounds 退化）。
        let floor = floor_a();
        // left == right：x 恒钳单点，反弹模按 0.4 逐子步衰减至 0。
        let mut point = PhysicsEngine::new(
            Vec2::new(500.0, 500.0),
            InteractionCfg::default(),
            HorizontalBounds { left: 100.0, right: 100.0 },
        );
        point.tick(0, &floor);
        point.tick(10, &floor);
        for i in 1..=50u64 {
            point.tick(10 + i * 10, &floor);
        }
        assert!(point.pos().x.is_finite() && point.pos().y.is_finite(), "单点区间位置有限");
        assert!(point.vel().x.is_finite() && point.vel().y.is_finite(), "单点区间速度有限");
        // left > right：钳制区间翻转，x 在 [right, left] 内振荡衰减，仍不 panic。
        let mut inverted = PhysicsEngine::new(
            Vec2::new(500.0, 500.0),
            InteractionCfg::default(),
            HorizontalBounds { left: 100.0, right: 50.0 },
        );
        inverted.tick(0, &floor);
        inverted.tick(10, &floor);
        for i in 1..=50u64 {
            inverted.tick(10 + i * 10, &floor);
        }
        assert!(inverted.pos().x.is_finite() && inverted.pos().y.is_finite(), "翻转区间位置有限");
        assert!(inverted.vel().x.is_finite() && inverted.vel().y.is_finite(), "翻转区间速度有限");
    }

    #[test]
    fn probe_nan_gravity_no_nan_propagation() {
        // 理由：invalid_gravity_falls_back_to_default_behavior 以「与 reference
        // 逐 tick 对照」间接验证 NaN 防御；本探针直断言 NaN gravity 不向
        // pos/vel 传播（is_finite），并覆盖长时程 201 个子步——effective_gravity
        // 在积分循环内逐子步重读 cfg 校验，防御须每子步生效而非仅构造时。
        let no_floor = DesktopFloor::new(Vec::new());
        let mut eng = falling_engine(
            Vec2::new(960.0, 500.0),
            InteractionCfg { gravity_px_per_sec2: f32::NAN, ..Default::default() },
            bounds_a(),
        );
        for i in 1..=200u64 {
            eng.tick(1010 + i * 10, &no_floor);
            assert!(
                eng.pos().x.is_finite() && eng.pos().y.is_finite(),
                "tick {i}：pos 出现 NaN 传播"
            );
            assert!(
                eng.vel().x.is_finite() && eng.vel().y.is_finite(),
                "tick {i}：vel 出现 NaN 传播"
            );
        }
        // 201 个子步（2.01s）后半隐式欧拉速度 = g·t（回退后的默认重力真源）。
        let g = InteractionCfg::default().gravity_px_per_sec2;
        let expected = g * (201.0 * SUB_STEP_MS as f32 / 1000.0);
        assert!(
            (eng.vel().y - expected).abs() < 1e-1,
            "NaN 防御后速度应按默认重力线性增长：{} vs {expected}",
            eng.vel().y
        );
    }

    // -- S3-M4 甩出构造入口（thrown）：初速方向抛物线 / 反弹不出屏 / 首 tick dt=0 --

    #[test]
    fn thrown_vx_positive_travels_right_monotonically() {
        // vx > 0 → x 单调增（无墙面反弹路径），抛物线向前飞并落地。
        let floor = floor_a();
        let mut eng = PhysicsEngine::thrown(
            Vec2::new(500.0, 500.0),
            Vec2::new(300.0, 0.0),
            InteractionCfg::default(),
            bounds_a(),
        );
        assert!(!eng.grounded(), "thrown 构造即下坠态（无需调用方先置）");
        assert_eq!(eng.vel(), Vec2::new(300.0, 0.0), "初速原样保留");
        let mut prev_x = eng.pos().x;
        let mut landed = false;
        for i in 1..=200u64 {
            let events = eng.tick(1_000 + i * 10, &floor);
            // 首 tick dt=0 只锚定不推进（x 恒等），故单调性断言用 >=（不降）；
            // vx>0 的净位移由落地后 `x > 500` 断言兜底，不依赖本处的严格递增。
            assert!(
                eng.pos().x >= prev_x,
                "x 应单调不降（vx>0 无反弹）：{} vs {prev_x}",
                eng.pos().x
            );
            prev_x = eng.pos().x;
            if !events.is_empty() {
                landed = true;
                break;
            }
        }
        assert!(landed, "向前抛出应落地");
        assert!(eng.pos().x > 500.0, "vx>0 → 落点在抛出点右侧：{}", eng.pos().x);
        assert!(eng.grounded());
    }

    #[test]
    fn thrown_vy_negative_rises_then_falls() {
        // vy < 0（上抛）→ 先升后降：最低 y 显著高于抛出点，最终落回地面线。
        let floor = floor_a();
        let start_y = 1000.0;
        let mut eng = PhysicsEngine::thrown(
            Vec2::new(960.0, start_y),
            Vec2::new(0.0, -800.0),
            InteractionCfg::default(),
            bounds_a(),
        );
        let mut min_y = start_y;
        let mut landed = false;
        for i in 1..=300u64 {
            let events = eng.tick(1_000 + i * 10, &floor);
            min_y = min_y.min(eng.pos().y);
            if !events.is_empty() {
                landed = true;
                break;
            }
        }
        assert!(landed, "上抛后应落回地面");
        // 上升高度 v²/2g ≈ 133px（v=800, g=配置真源 2400）→ min_y < start_y − 100。
        assert!(min_y < start_y - 100.0, "上抛应显著高于抛出点：min_y={min_y}");
        assert!((eng.pos().y - 1040.0).abs() < 1e-3, "落点钳回地面线");
        assert!(eng.grounded());
    }

    #[test]
    fn thrown_wall_bounce_keeps_x_within_bounds() {
        // 向左甩出：撞左墙 x=0 反弹（0.4 系数），全程 x 不越出边界区间。
        let floor = floor_a();
        let mut eng = PhysicsEngine::thrown(
            Vec2::new(100.0, 500.0),
            Vec2::new(-1500.0, 0.0),
            InteractionCfg::default(),
            bounds_a(),
        );
        let mut bounced = false;
        for i in 1..=200u64 {
            eng.tick(1_000 + i * 10, &floor);
            assert!(eng.pos().x >= 0.0 - 1e-3, "x 不得越出左边界：{}", eng.pos().x);
            assert!(eng.pos().x <= 1920.0 + 1e-3, "x 不得越出右边界：{}", eng.pos().x);
            if eng.vel().x > 0.0 {
                bounced = true;
                break;
            }
        }
        assert!(bounced, "向左甩出应撞左墙反弹");
        let expect = 1500.0 * WALL_BOUNCE_RESTITUTION;
        assert!(
            (eng.vel().x - expect).abs() < 1e-2,
            "反弹速度 = 原模 × 反弹系数：{} vs {expect}",
            eng.vel().x
        );
    }

    #[test]
    fn thrown_first_tick_zero_dt_keeps_initial_velocity() {
        // 首 tick dt=0：不积分、不触发起坠转换，初速原样保留（同 new 口径）。
        let floor = floor_a();
        let vel = Vec2::new(300.0, -800.0);
        let mut eng = PhysicsEngine::thrown(
            Vec2::new(960.0, 500.0),
            vel,
            InteractionCfg::default(),
            bounds_a(),
        );
        let events = eng.tick(1_000, &floor);
        assert!(events.is_empty(), "首 tick 无事件");
        assert_eq!(eng.pos(), Vec2::new(960.0, 500.0), "首 tick 不积分");
        assert_eq!(eng.vel(), vel, "首 tick 保留初速");
        assert!(!eng.grounded(), "dt=0 不改变下坠态");
    }
}
