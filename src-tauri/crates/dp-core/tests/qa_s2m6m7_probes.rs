//! QA 独立探针测试（S2-M6 平台图 + S2-M7 感知服务，回归验证）。
//!
//! 由 QA（严过关）新增，**不改动任何生产代码**。仅使用 `dp-core` 公开 API，
//! 目标：在工程师自测之外，独立复现并尝试证伪其结论（验收口径逐条探针）。
//!
//! 覆盖点（对应 QA 任务第 4 条）：
//!   P1 番茄钟计时精度（FR-10-1）：25min 工作段 tick 链后 WorkDone 触发时刻
//!      与 work_end 偏差 < 1s；休息 / 第二工作段 deadline 链同理；
//!   P2 番茄钟勿扰（FR-10-4）：dnd=true 抑制 WorkDone/RestDone 但阶段推进照常
//!      （整双周期跑查 + 按tick 开关混合）；
//!   P3 PerceptionBus 溢出：塞入 >64 条消息，最旧被丢弃、最新可达、不 panic；
//!   P4 segment_of_hour 边界：hour=5/11/17/23/4/22 归段（23 必须归 Night）；
//!   P5 PlatformGraph 混合平台带（taskbar top ≠ 桌底 top 的强区分场景）：
//!      clamp_to_stand 落到正确节点；ScreenEdge 永不被选为站立面；
//!   P6 PlatformGraph 降级（R2）：titlebars 空 → degraded=true 且仍有桌底可站，
//!      恢复注入后降级解除。
//!
//! 标注：本文件为 QA 交付物，非工程师生产代码。

use dp_core::motion::{
    MonitorGeom, PlatformBand, PlatformGraph, PlatformInputs, PlatformKind, StandSurface, Vec2,
};
use dp_core::perception::pomodoro::{Pomodoro, PomodoroCfg, PomodoroEvent, PomodoroPhase};
use dp_core::perception::{segment_of_hour, PerceptionBus, TimeSegment, PERCEPTION_CHANNEL_CAP};

// ---------------------------------------------------------------------------
// 公共构造
// ---------------------------------------------------------------------------

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

/// 绝对时间锚点（任意固定值；C3 探针不读真实时钟）。
const T0: i64 = 1_700_000_000_000;

/// 事件谓词：是否为指定事件。
fn is_work_done(evs: &[PomodoroEvent], cycle: u32) -> bool {
    evs.iter().any(|e| matches!(e, PomodoroEvent::WorkDone { cycle: c } if *c == cycle))
}

fn is_rest_done(evs: &[PomodoroEvent], cycle: u32) -> bool {
    evs.iter().any(|e| matches!(e, PomodoroEvent::RestDone { cycle: c } if *c == cycle))
}

// ---------------------------------------------------------------------------
// P1 番茄钟计时精度（FR-10-1）：tick 链触发时刻与理论边界偏差 < 1s
// ---------------------------------------------------------------------------

/// 700ms 步进 tick 链（1_500_000 非 700 整数倍 → 触发必带非零过冲，
/// 可真实验证「偏差 <1s」而非恰好 0）。跑满两个工作段 + 一个休息段。
#[test]
fn qa_pomodoro_workdone_fires_within_1s_of_theoretical_end() {
    let mut p = Pomodoro::new(PomodoroCfg::default());
    assert_eq!(p.start(T0), Some(PomodoroEvent::Started));

    // 理论边界（绝对锚定链）：
    //   work_end_1 = T0 + 25min；rest_end = work_end_1 + 5min；work_end_2 = rest_end + 25min。
    let work_ms = 25 * 60 * 1000i64;
    let rest_ms = 5 * 60 * 1000i64;
    let work_end_1 = T0 + work_ms;
    let rest_end = work_end_1 + rest_ms;
    let work_end_2 = rest_end + work_ms;

    let step = 700i64;
    let mut t = T0;
    let mut saw_work1 = false;
    let mut saw_rest1 = false;
    let mut saw_work2 = false;
    // 上界：全部事件理论最晚触发 + 1min 余量。
    while t < work_end_2 + 60 * 1000 {
        t += step;
        let evs = p.tick(t, false);
        if !saw_work1 && is_work_done(&evs, 1) {
            saw_work1 = true;
            let lateness = t - work_end_1;
            assert!(
                (0..1000).contains(&lateness),
                "FR-10-1：WorkDone(cycle=1) 触发时刻与 work_end 偏差须 <1s，实际 {lateness}ms"
            );
            assert_eq!(
                evs.iter().position(|e| matches!(e, PomodoroEvent::PhaseStarted { resting: true, cycle: 1 })),
                Some(1),
                "WorkDone 与同 tick 的 PhaseStarted(resting) 成对且顺序正确"
            );
            assert_eq!(p.phase(), PomodoroPhase::Resting);
        }
        if !saw_rest1 && is_rest_done(&evs, 1) {
            saw_rest1 = true;
            let lateness = t - rest_end;
            assert!(
                (0..1000).contains(&lateness),
                "RestDone(cycle=1) 偏差须 <1s，实际 {lateness}ms"
            );
            assert_eq!(p.cycle(), 2, "休息自然完成 → 下一工作段 cycle+1");
        }
        if !saw_work2 && is_work_done(&evs, 2) {
            saw_work2 = true;
            let lateness = t - work_end_2;
            assert!(
                (0..1000).contains(&lateness),
                "FR-10-1：WorkDone(cycle=2) 触发时刻与理论边界偏差须 <1s，实际 {lateness}ms"
            );
        }
    }
    assert!(saw_work1 && saw_rest1 && saw_work2, "三段自然完成事件均应触发");
}

// ---------------------------------------------------------------------------
// P2 番茄钟勿扰（FR-10-4）：抑制提醒但阶段照常推进
// ---------------------------------------------------------------------------

/// 全程 dnd=true 跑两个完整工作-休息循环：零提醒事件、阶段迁移齐全、cycle 正确。
#[test]
fn qa_pomodoro_dnd_full_cycle_suppresses_reminders_but_advances() {
    let mut p = Pomodoro::new(PomodoroCfg::default());
    p.start(T0);
    let work_ms = 25 * 60 * 1000i64;
    let rest_ms = 5 * 60 * 1000i64;

    let mut all: Vec<PomodoroEvent> = Vec::new();
    let mut t = T0;
    // 两个整循环；<=：含第二次休息自然完成的那个 tick（PhaseStarted{false,3} 产生点）。
    let horizon = T0 + 2 * (work_ms + rest_ms);
    while t <= horizon {
        t += 1000;
        all.extend(p.tick(t, true));
    }
    // 提醒类事件全部被抑制。
    assert!(
        !all.iter().any(|e| matches!(e, PomodoroEvent::WorkDone { .. } | PomodoroEvent::RestDone { .. })),
        "dnd=true 不得出现 WorkDone/RestDone：{all:?}"
    );
    // 阶段推进照常：四段迁移事件齐全。
    for (resting, cycle) in [(true, 1u32), (false, 2), (true, 2), (false, 3)] {
        assert!(
            all.iter().any(|e| matches!(e, PomodoroEvent::PhaseStarted { resting: r, cycle: c } if *r == resting && *c == cycle)),
            "缺少 PhaseStarted(resting={resting}, cycle={cycle})：{all:?}"
        );
    }
    assert_eq!(p.cycle(), 3, "两个整循环后应进入第 3 工作段");
    assert_eq!(p.phase(), PomodoroPhase::Working);

    // 勿扰关闭后，当前工作段完成时提醒恢复（抑制是按 tick 生效，非粘性）。
    let mut found = false;
    while t < horizon + 2 * work_ms {
        t += 1000;
        let evs = p.tick(t, false);
        if is_work_done(&evs, 3) {
            found = true;
            break;
        }
    }
    assert!(found, "dnd 恢复 false 后 WorkDone 应正常产出");
}

/// 混合勿扰：工作段结束 tick 开勿扰（WorkDone 抑制），休息段结束 tick 关勿扰
/// （RestDone 产出）——验证 dnd 按 tick 取值、阶段推进两种情况下都不受影响。
#[test]
fn qa_pomodoro_dnd_per_tick_mixed() {
    let mut p = Pomodoro::new(PomodoroCfg::default());
    p.start(T0);
    let work_ms = 25 * 60 * 1000i64;
    let rest_ms = 5 * 60 * 1000i64;

    // 工作段自然完成，勿扰开：WorkDone 被抑制、迁移保留。
    let evs = p.tick(T0 + work_ms, true);
    assert_eq!(evs, vec![PomodoroEvent::PhaseStarted { resting: true, cycle: 1 }]);
    assert_eq!(p.phase(), PomodoroPhase::Resting, "勿扰不影响阶段推进");
    // 休息段自然完成，勿扰关：RestDone 正常产出。
    let evs = p.tick(T0 + work_ms + rest_ms, false);
    assert!(
        is_rest_done(&evs, 1),
        "dnd=false 时 RestDone 应产出：{evs:?}"
    );
    assert_eq!(p.cycle(), 2);
    assert_eq!(p.phase(), PomodoroPhase::Working);
}

// ---------------------------------------------------------------------------
// P3 PerceptionBus：>64 条消息 → 丢最旧、新消息可达、不 panic
// ---------------------------------------------------------------------------

#[test]
fn qa_bus_overflow_100_messages_drops_oldest_keeps_newest() {
    let bus: PerceptionBus<u64> = PerceptionBus::new();
    let total = 100u64; // > PERCEPTION_CHANNEL_CAP(64)
    for i in 0..total {
        bus.offer(i);
    }
    assert_eq!(bus.len(), PERCEPTION_CHANNEL_CAP, "容量恒为 64");
    let mut out = Vec::new();
    bus.drain(&mut out);
    assert_eq!(out.len(), PERCEPTION_CHANNEL_CAP);
    // 最旧的 total-cap 条（0..=35）被丢弃；保留区间严格连续 FIFO。
    assert_eq!(out[0], total - PERCEPTION_CHANNEL_CAP as u64, "最旧的应被丢弃");
    assert_eq!(out[PERCEPTION_CHANNEL_CAP - 1], total - 1, "最新一条必须可达");
    for (i, v) in out.iter().enumerate() {
        assert_eq!(*v, total - PERCEPTION_CHANNEL_CAP as u64 + i as u64, "顺序连续");
    }

    // 溢出 + drain 之后，新消息继续可达（不 panic）。
    bus.offer(1_000);
    bus.offer(1_001);
    let mut again = Vec::new();
    bus.drain(&mut again);
    assert_eq!(again, vec![1_000, 1_001]);
}

/// 恰好 cap 条 → 全保留；cap+1 条 → 仅丢 1 条最旧（边界精确性）。
#[test]
fn qa_bus_overflow_boundary_exact_cap_and_cap_plus_one() {
    let bus: PerceptionBus<u64> = PerceptionBus::new();
    for i in 0..PERCEPTION_CHANNEL_CAP as u64 {
        bus.offer(i);
    }
    let mut out = Vec::new();
    bus.drain(&mut out);
    assert_eq!(out.len(), PERCEPTION_CHANNEL_CAP, "恰好满载不丢弃");
    assert_eq!(out[0], 0);

    let bus2: PerceptionBus<u64> = PerceptionBus::new();
    for i in 0..(PERCEPTION_CHANNEL_CAP + 1) as u64 {
        bus2.offer(i);
    }
    let mut out2 = Vec::new();
    bus2.drain(&mut out2);
    assert_eq!(out2.len(), PERCEPTION_CHANNEL_CAP);
    assert_eq!(out2[0], 1, "第 65 条入队时恰丢弃最旧的 0 号");
    assert_eq!(out2[PERCEPTION_CHANNEL_CAP - 1], PERCEPTION_CHANNEL_CAP as u64);
}

// ---------------------------------------------------------------------------
// P4 segment_of_hour 边界归段
// ---------------------------------------------------------------------------

#[test]
fn qa_segment_of_hour_required_boundaries() {
    // （hour, 期望段）——含任务指定的 5/11/17/23/4/22 六点。
    let cases = [
        (5u8, TimeSegment::Morning),
        (11, TimeSegment::Daytime),
        (17, TimeSegment::Evening),
        (23, TimeSegment::Night), // 23 必须归 Night（傍晚为 [17,23) 左闭右开）
        (4, TimeSegment::Night),
        (22, TimeSegment::Evening),
    ];
    for (hour, expect) in cases {
        assert_eq!(segment_of_hour(hour), expect, "hour={hour} 归段错误");
    }
}

// ---------------------------------------------------------------------------
// P5 PlatformGraph 混合平台带：taskbar top ≠ 桌底 top 的强区分场景
// ---------------------------------------------------------------------------

/// 任务栏带 top=1000、桌底 1040、标题栏 [400,1520) top=500：
/// 同一 x 被多个平台包含时取 top 最小者；ScreenEdge 永不参与判定。
#[test]
fn qa_platform_mixed_bands_clamp_lands_on_correct_node() {
    let mut g = PlatformGraph::new(vec![mon_a()], 0);
    g.rebuild(0, PlatformInputs {
        titlebars: vec![PlatformBand { left: 400.0, top: 500.0, right: 1520.0 }],
        taskbars: vec![PlatformBand { left: 0.0, top: 1000.0, right: 1920.0 }],
    });
    // 节点数：桌底 1 + 任务栏 1 + 标题栏 1 + 边缘 2 = 5，四类齐全。
    assert_eq!(g.platforms().len(), 5);
    let kinds: Vec<PlatformKind> = g.platforms().iter().map(|p| p.kind).collect();
    for k in [
        PlatformKind::DesktopBottom,
        PlatformKind::Taskbar,
        PlatformKind::WindowTitleBar,
        PlatformKind::ScreenEdge,
    ] {
        assert!(kinds.contains(&k), "缺少 {k:?} 节点");
    }
    assert!(!g.degraded());

    // ① x=600：标题栏(500) 与任务栏(1000) 与桌底(1040) 都水平包含 → 取 top 最小的标题栏。
    let p = g.clamp_to_stand(Vec2::new(600.0, 700.0));
    assert_eq!(p, Vec2::new(600.0, 500.0), "应落到标题栏 top=500：{p:?}");
    assert!(g.is_valid_stand(p));

    // ② x=200：仅任务栏(1000)+桌底(1040) 包含 → 任务栏（1000 < 1040）。
    let p = g.clamp_to_stand(Vec2::new(200.0, 700.0));
    assert_eq!(p, Vec2::new(200.0, 1000.0), "应落到任务栏 top=1000：{p:?}");
    assert!(g.is_valid_stand(p));

    // ③ x=1800：任务栏区间内、标题栏外 → 任务栏。
    let p = g.clamp_to_stand(Vec2::new(1800.0, 700.0));
    assert_eq!(p, Vec2::new(1800.0, 1000.0));
    assert!(g.is_valid_stand(p));

    // ④ 标题栏半开区间右端 x=1520：不含标题栏 → 落任务栏。
    let p = g.clamp_to_stand(Vec2::new(1520.0, 700.0));
    assert_eq!(p, Vec2::new(1520.0, 1000.0), "右端点属半开区间外：{p:?}");

    // ⑤ ScreenEdge 永不成为站立面：x=1920（右缘）→ 桌底回退语义钳到 (1920, 1040)。
    let p = g.clamp_to_stand(Vec2::new(1920.0, 700.0));
    assert_eq!(p, Vec2::new(1920.0, 1040.0), "不得吸附 ScreenEdge，应回退桌底：{p:?}");
    assert!(!g.is_valid_stand(Vec2::new(1920.0, 1000.0)), "右缘点任何高度都非合法站立点");
}

// ---------------------------------------------------------------------------
// P6 PlatformGraph 降级（R2）：titlebars 空 → degraded=true 且仍有桌底可站
// ---------------------------------------------------------------------------

#[test]
fn qa_platform_empty_titlebars_degraded_but_floor_still_standable() {
    let mut g = PlatformGraph::new(vec![mon_a()], 0);
    g.rebuild(0, PlatformInputs {
        titlebars: Vec::new(),
        taskbars: vec![PlatformBand { left: 0.0, top: 1000.0, right: 1920.0 }],
    });
    assert!(g.degraded(), "titlebars 为空 → R2 降级标志");
    // 不崩溃：桌底 + 任务栏 + 边缘仍在，仅无标题栏。
    let kinds: Vec<PlatformKind> = g.platforms().iter().map(|p| p.kind).collect();
    assert!(kinds.contains(&PlatformKind::DesktopBottom), "降级后桌底仍在");
    assert!(kinds.contains(&PlatformKind::Taskbar), "降级后任务栏仍在");
    assert!(!kinds.contains(&PlatformKind::WindowTitleBar));
    // 桌底可站：x 在任务栏外（本例任务栏横贯全屏，改用 y 判定任务栏上站立）。
    assert!(g.is_valid_stand(Vec2::new(960.0, 1040.0)), "降级后桌底可站");
    assert!(g.is_valid_stand(Vec2::new(960.0, 1000.0)), "降级后任务栏顶仍可站");
    // 钳制回退桌底语义。
    let p = g.clamp_to_stand(Vec2::new(960.0, 700.0));
    assert_eq!(p, Vec2::new(960.0, 1000.0), "钳到最高的可用平台（任务栏 1000）：{p:?}");
    let p = g.clamp_to_stand(Vec2::new(5_000.0, 700.0));
    assert_eq!(p, Vec2::new(1920.0, 1040.0), "区间外回退桌底并钳 x：{p:?}");

    // 恢复注入 titlebars → 降级解除。
    g.rebuild(2000, PlatformInputs {
        titlebars: vec![PlatformBand { left: 400.0, top: 500.0, right: 1520.0 }],
        taskbars: Vec::new(),
    });
    assert!(!g.degraded(), "titlebars 恢复 → 降级解除");
    assert!(g.platforms().iter().any(|pl| pl.kind == PlatformKind::WindowTitleBar));
}
