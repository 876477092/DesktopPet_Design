//! QA 独立探针（S3-M0 运行时装配层验证，QA 严过关 2026-09-13）。
//!
//! 目的：以**公共 API**（`CoreLoopState` / `ScheduleGrid`）驱动装配层，补现有
//! `coreloop::tests`（crate 内单元测试）**未覆盖**的边界，独立佐证：
//!   - GWT-3：单 tick 注入极大 dt → 有界、不挂起、位置有限（装配层入口，非仅引擎层）；
//!   - 时钟回拨（`now_ms` 倒退）→ `saturating_sub` 防御，无 panic、位置不变；
//!   - 拓扑变更（增屏 / 删空）经 `apply_monitors` → 不 panic、位置有限；
//!   - E1：三档 deadline 绝对锚定「第 k 次 == start + k×间隔」（从 crate 外独立复核）。
//!
//! 本文件仅测试代码，不改任何生产代码。

#![cfg(windows)]

use dp_app::coreloop::{advance_seed, CoreCfg, CoreLoopState, Phase, ScheduleGrid};
use dp_core::anim::ActionCatalog;
use dp_core::config::model::{EmotionConfig, InteractionCfg, NeedsConfig, RoamCfg};
use dp_core::motion::{MonitorGeom, Vec2};

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

/// 左侧副屏：VDC x ∈ [-1920, 0)。
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

fn roam_cfg() -> RoamCfg {
    RoamCfg {
        pace_options: dp_core::config::model::RoamCfg::default().pace_options,
        pace: 1.0,
        decision_interval_sec: [5, 30],
        cursor_avoid_radius_px: 150,
        walk_speed_px_per_sec: 60.0,
    }
}

fn landless_state(now_ms: u64) -> CoreLoopState {
    // 空显示器 → 无站立面 → 首 tick 即 Roam→Fall（可达性靠注入非空相，§6-7）。
    CoreLoopState::new(
        Vec2::new(960.0, 500.0),
        Vec::new(),
        CoreCfg {
            roam_cfg: roam_cfg(),
            interaction_cfg: InteractionCfg::default(),
            catalog: ActionCatalog::default(),
            emotion: EmotionConfig::default(),
            needs: NeedsConfig::default(),
            // S4-M5 / S4-M6 起 `CoreCfg` 追加台词库与音效总线，S5-M1 起追加存档门面；
            // 本探针只关心装配层三档 / 相机器 / 时钟回拨，故显式取
            // 「无台词 / 无音效 / 无存档」默认形态。
            ..CoreCfg::default()
        },
        7,
        now_ms,
    )
}

// -- GWT-3：装配层入口的极大 dt 护栏 -----------------------------------------

#[test]
fn gwt3_extreme_dt_through_logic_tick_is_bounded_and_does_not_hang() {
    let mut state = landless_state(0);
    let _ = state.logic_tick(50);
    assert_eq!(state.phase(), Phase::Fall, "空屏 → 起坠相");

    // 注入接近 u64::MAX 的时间跳变（模拟休眠 / 时间跳变）。
    let out = state.logic_tick(u64::MAX / 2);
    assert!(
        out.pos.x.is_finite() && out.pos.y.is_finite(),
        "极大 dt 后位置仍有限：{:?}",
        out.pos
    );
    // 该次下坠量必须有界（引擎钳 MAX_TICK_DT_MS=1000ms，不应跨越半个 u64）。
    assert!(
        out.pos.y < 500.0 + 100_000.0,
        "单 tick 下坠量应有界，实际 y={}",
        out.pos.y
    );

    // 跳变后继续正常推进，不得挂起 / 不得 panic。
    let out2 = state.logic_tick(u64::MAX / 2 + 50);
    assert!(out2.pos.x.is_finite() && out2.pos.y.is_finite());
}

// -- 时钟回拨：saturating_sub 防御 -------------------------------------------

#[test]
fn clock_rollback_is_safe_no_panic_and_pos_unchanged() {
    let mut state = landless_state(1_000);
    let before = state.logic_tick(1_050);
    assert_eq!(state.phase(), Phase::Fall);

    // now_ms 大幅回拨：dt 走 saturating_sub → 0 → 引擎 no-op。
    let after = state.logic_tick(10);
    assert!(after.pos.x.is_finite() && after.pos.y.is_finite());
    assert!(after.submitted.is_none(), "回拨不产生仲裁提交");
    // dt=0 → 位置不前进（物理首 tick/回拨防御）。
    assert!(
        (after.pos.y - before.pos.y).abs() < f32::EPSILON,
        "回拨 tick 位置不应变化：before={:?} after={:?}",
        before.pos,
        after.pos
    );
}

// -- 拓扑变更路径：增屏 / 删空 -------------------------------------------------

#[test]
fn topology_change_paths_do_not_panic_and_keep_pos_finite() {
    let mut state = CoreLoopState::new(
        Vec2::new(960.0, 1040.0),
        vec![mon_a()],
        CoreCfg {
            roam_cfg: roam_cfg(),
            interaction_cfg: InteractionCfg::default(),
            catalog: ActionCatalog::default(),
            emotion: EmotionConfig::default(),
            needs: NeedsConfig::default(),
            // S4-M5 / S4-M6 起 `CoreCfg` 追加台词库与音效总线，S5-M1 起追加存档门面；
            // 本探针只关心装配层三档 / 相机器 / 时钟回拨，故显式取
            // 「无台词 / 无音效 / 无存档」默认形态。
            ..CoreCfg::default()
        },
        7,
        0,
    );
    let _ = state.logic_tick(50);
    assert_eq!(state.phase(), Phase::Roam, "合法站立面上保持漫游相");

    // 增屏（拓扑变更：集合语义命中）。
    state.apply_monitors(vec![mon_a(), mon_b()], 100);
    let _ = state.logic_tick(100);
    assert!(state.pos().x.is_finite() && state.pos().y.is_finite());

    // 删空（站立面全消失 → 换相 Fall，bounds 无界防御）。
    state.apply_monitors(Vec::new(), 150);
    let out = state.logic_tick(150);
    assert_eq!(state.phase(), Phase::Fall, "删空显示器 → 站立面消失 → Fall");
    assert!(out.pos.x.is_finite() && out.pos.y.is_finite());

    // 同几何重复快照不触发重建（幂等，不 panic）。
    let snapshot = vec![mon_a()];
    state.apply_monitors(snapshot.clone(), 200);
    let _ = state.logic_tick(200);
    assert!(state.pos().x.is_finite() && state.pos().y.is_finite());
}

// -- E1：三档 deadline 绝对锚定（从 crate 外独立复核） -----------------------

#[test]
fn schedule_grid_deadline_is_start_plus_k_interval() {
    let mut g = ScheduleGrid::new(1_000, 16, 50, 1_000);
    assert_eq!(g.next_logic(), 1_050, "首个 logic deadline = start + 1×50");
    assert_eq!(g.next_biz(), 2_000, "首个 biz deadline = start + 1×1000");

    // 连续 10 轮、每轮迟到 30ms（过冲 < 间隔）→ 每轮恰推进一档，锚点不漂移。
    for k in 1..=10u64 {
        let fired = g.advance(1_000 + k * 50 + 30);
        assert!(fired.logic, "第 {k} 轮 logic 到期");
    }
    assert_eq!(
        g.next_logic(),
        1_000 + 11 * 50,
        "第 11 次 deadline 仍 == start + 11×间隔（过冲不累积）"
    );
}

// -- seed_k 递推：换相重建须用递推种子（非固定值） --------------------------

#[test]
fn seed_advance_is_recursive_and_non_constant() {
    let s0 = 42u64;
    let s1 = advance_seed(s0, 1_000);
    let s2 = advance_seed(s1, 2_000);
    assert_ne!(s1, s0);
    assert_ne!(s2, s1, "两次重建不得得到同一种子");
    // 同输入确定性（可复现）。
    assert_eq!(advance_seed(s0, 1_000), s1, "同输入应确定");
}
