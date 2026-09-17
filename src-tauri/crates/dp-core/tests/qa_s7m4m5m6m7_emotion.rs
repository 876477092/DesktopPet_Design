//! S7-M5 / S7-M6 / S7-M7 验收用例：情绪模型算例标定、确定性、不变量与三层死锁防护。
//!
//! 对应台账 `03 §2` 的三张卡：
//!   - **S7-M5**：Mood 惯性 / L0~L5 阶段状态机 / 自然消气（仅 L3→L2）/ 深夜重定向；
//!   - **S7-M6**：敏感度（FR-11-11）/ 三层死锁防护（FR-11-12）；
//!   - **S7-M7**：集成 + 离线分块 + 算例校验（`02 §9.2` 必测不变量）。
//!
//! ## 标定口径（`01 §6.11.10` / §9.2）
//!
//! AC-16 统一在「**情绪敏感度 = 1.0 + 粘人度 50±5**」下验收，容差 ±10%。
//! 本文件的用例一律用 [`Personality::from_cfg`]（粘人 0.50 / 脾气 0.55）钉住性格，
//! 使典型场景七因子乘积 = 1.0（`tips`：`personalityFactor = 0.6+0.8×0.5 = 1.0`，
//! `adaptFactor = sqrt(25/25) = 1.0`）。
//!
//! AC-16 的度量口径：**「P 首次达到阈值的时刻」**（`01 §6.11.5` 算例 2 明确
//! 「忽略 60s 升级确认期」），故断言分两段：① P 越阈时刻 ∈ 标称 ±10%；
//! ② 档位迁移发生在越阈后 `confirm.upSec` 之内（不跳级、不早迁）。

use chrono::{Local, TimeZone};

use dp_core::emotion::coax::{CoaxStep, TRAY_COAX_STROKE_TAPS};
use dp_core::emotion::neglect::level_for;
use dp_core::emotion::{
    EmotionEngine, EmotionEvent, EmotionState, InteractionPolicy, Personality, TickEnv,
};
use dp_core::config::model::{EmotionConfig, NeedsConfig};
use dp_core::perception::ActivitySample;

/// 摸鱼白名单里的进程类别哈希（用例注入用；测试哈希，非真实进程）。
const SLACK_HASH: u64 = 0xB1B1_2026;

/// 本地时刻（用例统一用 14:00：非深夜、非用餐、非预热 → `rhythmFactor = 1.0`）。
fn at(hour: u32, minute: u32) -> chrono::DateTime<Local> {
    Local
        .with_ymd_and_hms(2026, 9, 17, hour, minute, 0)
        .single()
        .expect("有效本地时刻")
}

/// 构造「典型场景」环境：用户一直在电脑前、轻度使用、不理她。
///
/// 第一个参数为**时间点占位**（`TickEnv` 不含绝对时间字段，时间点由 `tick_1s` 给出）；
/// 保留该形参是为了让调用处的「时刻 / 空闲」两个语义显式并列，便于阅读。
fn env_day(_now_ms: i64, idle_ms: u64) -> TickEnv<'static> {
    TickEnv {
        now_local: at(14, 0),
        preset_idle_ms: idle_ms,
        satiety: 70.0,
        cleanliness: 85.0,
        ..TickEnv::default()
    }
}

/// 钉住性格的引擎（粘人 50 / 脾气 55 → 典型场景乘积 1.0）。
fn engine_typical<'a>(cfg: &'a EmotionConfig, needs: &'a NeedsConfig) -> EmotionEngine<'a> {
    let mut e = EmotionEngine::new(cfg, needs);
    e.set_personality(Personality::from_cfg(&cfg.personality));
    e
}

/// 制造关系降温：连续 4 个「日期」各推 2 拍，触发 `daily_roll` 的降温计数。
///
/// 走**真实日切路径**（`state.today` 变化 → `AdaptationState::roll_day`），
/// 不直接改写 `AdaptationState` 内部字段——保证测的是集成行为而非字段。
fn force_cooling(e: &mut EmotionEngine<'_>, cfg: &EmotionConfig) {
    let mut t = e.state.last_tick_ms;
    for day in 1..=4u32 {
        let dt = Local
            .with_ymd_and_hms(2026, 10, day, 14, 0, 0)
            .single()
            .expect("本地时刻");
        for _ in 0..2 {
            t += 1_000;
            let mut env = env_day(t, 0);
            env.now_local = dt;
            let _ = e.tick_1s(t, env);
        }
    }
    assert!(
        e.adapt().is_cooling(&cfg.adapt),
        "连续低互动日切后应进入关系降温（cool_days={}）",
        e.adapt().cool_days()
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// S7-M7 · AC-16：典型场景 5/15/30/60/120 分钟依次 L1~L5（±10%）
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ac16_typical_scenario_hits_five_thresholds_in_order() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    // 首拍建基线（不累积）
    let _ = e.tick_1s(0, env_day(0, 0));
    assert!(
        (e.factors().product - 1.0).abs() < 2e-3,
        "典型场景乘积应 ≈1.0（needs 因子随饱食度微漂），实际 {}",
        e.factors().product
    );

    let t = &cfg.thresholds;
    // 逐档测量：每个阈值在 ±10% 内首次达标
    let mut events = Vec::new();
    for (idx, (label, threshold, want_min)) in [
        ("L1", t.l1 as f32, 5.0f32),
        ("L2", t.l2 as f32, 15.0),
        ("L3", t.l3 as f32, 30.0),
        ("L4", t.l4 as f32, 60.0),
        ("L5", t.l5 as f32, 120.0),
    ]
    .into_iter()
    .enumerate()
    {
        let level_seen = u8::try_from(idx).expect("档位序号 < 6");
        let tol = want_min * 0.10;
        // 从当前 P 起继续推进，直到越过该阈值
        let mut reached = None;
        let mut now = e.state.last_tick_ms;
        while now < 130 * 60_000 {
            now += 1_000;
            let mut ev = env_day(now, 0);
            ev.now_local = at(14, 0);
            events.extend(e.tick_1s(now, ev).events);
            if e.neglect.p >= threshold {
                reached = Some(now);
                break;
            }
        }
        let got_min = reached.expect("应能达到阈值") as f32 / 60_000.0;
        assert!(
            (got_min - want_min).abs() <= tol,
            "AC-16 {label}：标称 {want_min}min（±{tol}），实测 {got_min:.2}min"
        );
        // 档位迁移须在越阈后 upSec 内发生（不跳级 → 只允许 +1）
        let deadline = reached.expect("阈值时刻") + (cfg.confirm.up_sec as i64) * 1_000;
        while e.neglect.level < level_seen + 1 && e.state.last_tick_ms < deadline + 2_000 {
            let n = e.state.last_tick_ms + 1_000;
            let mut ev = env_day(n, 0);
            ev.now_local = at(14, 0);
            events.extend(e.tick_1s(n, ev).events);
        }
        assert_eq!(
            e.neglect.level,
            level_seen + 1,
            "AC-16 {label}：越阈后应在 upSec 内逐级迁移（不跳级）"
        );
    }

    // 每层一次 `ColdLevelChanged`，且 `|Δlevel| == 1`、`moodDelta` 逐层恰执行一次（不变量 ④）
    let changes: Vec<(u8, u8, f32)> = events
        .iter()
        .filter_map(|ev| match ev {
            EmotionEvent::ColdLevelChanged { from, to, mood_delta, .. } => Some((*from, *to, *mood_delta)),
            _ => None,
        })
        .collect();
    assert_eq!(changes.len(), 5, "五层各一条：{changes:?}");
    for (from, to, _) in &changes {
        assert_eq!(to.abs_diff(*from), 1, "绝不跳级：{from}→{to}");
    }
    for (idx, (_, to, delta)) in changes.iter().enumerate() {
        let want = cfg.levels[*to as usize].mood_delta as f32 * e.personality().mood_delta_scale(&cfg.personality);
        assert!(
            (delta - want).abs() < 1e-3,
            "L{to} 的 moodDelta 应恰为 {want}，实际 {delta}（第 {idx} 条）"
        );
        assert!(*delta <= 0.0, "升级扣减不得为正");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// S7-M7 · AC-17：持续编码 2h → P 封顶 L1（不进入委屈）
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ac17_deep_busyness_caps_at_l1_for_two_hours() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let deep = ActivitySample { key_kps: Some(cfg.busyness.kps_deep_threshold + 0.5), ..ActivitySample::default() };
    let env = TickEnv { activity: Some(deep), ..env_day(0, 0) };
    let _ = e.tick_1s(0, env);

    // 2 小时 = 7200 拍
    let mut t = 0i64;
    while t < 120 * 60_000 {
        t += 1_000;
        let mut ev = env;
        ev.now_local = at(14, 0);
        let _ = e.tick_1s(t, ev);
    }
    assert_eq!(e.busyness_level(), dp_core::emotion::BusynessLevel::Deep);
    assert!(
        (e.neglect.p - cfg.busyness.cap_deep as f32).abs() < 1e-3,
        "P 应封顶于 capDeep={}，实际 {}",
        cfg.busyness.cap_deep,
        e.neglect.p
    );
    assert!(e.neglect.level <= 1, "AC-17：深度忙碌 2h 不得进入 L2 及以上，实际 L{}", e.neglect.level);
}

// ═══════════════════════════════════════════════════════════════════════════
// S7-M7 · AC-18 / AC-37：离线边界（3h / 4h / 5h / 24h / 100h 一律不离家）
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ac18_offline_three_hours_is_mild_not_angry() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let _ = e.tick_1s(0, env_day(0, 0));
    let outcome = e.offline_compensate(3 * 3_600_000, env_day(0, u64::MAX));
    assert_eq!(outcome, dp_core::emotion::OfflineOutcome::ColdApplied(e.neglect.level));
    // 02 §5.5：3h → P = 0.05 × 180 = 9 → L1 无聊
    assert!((e.neglect.p - 9.0).abs() < 0.6, "3h 离线 P 应 ≈9，实际 {}", e.neglect.p);
    assert_eq!(e.neglect.level, 1, "AC-18：3h 离线应为 L1 无聊（不生气）");
}

/// **AC-37 离线补偿边界（`01 §11.3` 2026-09-17 订正后口径）**。
///
/// 离线期间 `presenceFactor = 0.05`、其余六因子恒 1.0（`02 §5.5`），故
/// `P = 0.05 × 离线分钟数`，档位按 §6.5.2 阈值 5/15/30/60/120 判定：
///
/// | 离线 | P | 期望档位 |
/// |---|---|---|
/// | 3h | 9 | L1 无聊 |
/// | 4h | 12 | L1（临界 L2，未越阈） |
/// | 5h | 15 | L2 委屈 |
/// | 24h | 72 | L4 上限 |
/// | 100h | 72（`maxSimSteps` 截断） | L4 |
///
/// 断言**逐档精确值**而非仅 `level <= 4` —— 原实现只锁「不离家」，锁不住 AC-37 的档位口径；
/// 同时保留 RV-16「任意时长 `level <= 4`」的红线断言。
#[test]
fn ac37_offline_boundaries_p_and_levels() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    // (离线小时数, 期望 P, 期望档位)
    let cases = [(3i64, 9.0f32, 1u8), (4, 12.0, 1), (5, 15.0, 2), (24, 72.0, 4), (100, 72.0, 4)];
    for (hours, want_p, want_level) in cases {
        let mut e = engine_typical(&cfg, &needs);
        let _ = e.tick_1s(0, env_day(0, 0));
        let _ = e.offline_compensate(hours * 3_600_000, env_day(0, u64::MAX));
        assert!(
            (e.neglect.p - want_p).abs() < 0.6,
            "AC-37：{hours}h 离线 P 应 ≈{want_p}，实际 {}",
            e.neglect.p
        );
        assert_eq!(
            e.neglect.level, want_level,
            "AC-37：{hours}h 离线应为 L{want_level}（实际 L{}，P={}）",
            e.neglect.level, e.neglect.p
        );
        // RV-16 第一 / 二道保险：任意时长绝不到 L5（不触发离家出走）
        assert!(
            e.neglect.level <= 4,
            "AC-37 / RV-16：{hours}h 离线绝不离家，实际 L{}",
            e.neglect.level
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// S7-M7 · AC-19：前台视频 40min → L3 + 摸鱼专属提示触发面
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ac19_slack_foreground_triggers_level3_and_dedicated_hint() {
    let mut cfg = EmotionConfig::default();
    cfg.busyness.slack_apps = vec![SLACK_HASH.to_string()];
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let slack = ActivitySample { foreground_hash: Some(SLACK_HASH), ..ActivitySample::default() };
    let env = TickEnv { activity: Some(slack), ..env_day(0, 0) };
    // 首拍只建基线（不求解因子），故需第二拍才能读到忙碌档
    let _ = e.tick_1s(0, env);
    let _ = e.tick_1s(1_000, env);
    assert_eq!(e.busyness_level(), dp_core::emotion::BusynessLevel::Slack);

    let mut t = 1_000i64;
    let mut events = Vec::new();
    while t < 45 * 60_000 {
        t += 1_000;
        let mut ev = env;
        ev.now_local = at(14, 0);
        events.extend(e.tick_1s(t, ev).events);
    }
    // 速率 1.2/min：25min → P=30（L3），40min → P=48
    assert!(e.neglect.p > 40.0 && e.neglect.p < 60.0, "40min 摸鱼 P 应 ≈48，实际 {}", e.neglect.p);
    assert_eq!(e.neglect.level, 3, "AC-19：40min 摸鱼应进入 L3 生闷气");

    let hints: Vec<(u32, String)> = events
        .iter()
        .filter_map(|ev| match ev {
            EmotionEvent::SlackLinger { minutes, pool } => Some((*minutes, pool.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(hints.len(), 1, "每档期只触发一次：{hints:?}");
    assert!(hints[0].0 >= 40, "应在 40min 后才触发，实际 {}min", hints[0].0);
    assert_eq!(
        hints[0].1, cfg.levels[3].line_pool,
        "池键应取当前档位的 linePool（配置驱动）"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// S7-M7 · AC-20：深夜 P 达 L3 → 改播困倦催睡，绝不播生气动作
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ac20_night_redirects_to_sleepy_hint() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let night = |ms: i64| TickEnv { now_local: at(2, 0), ..env_day(ms, 0) };
    let _ = e.tick_1s(0, night(0));

    // 深夜 rhythm = 0.35 → 累积更慢；直接推到 L3 阈值之上（最小侵入）
    let mut t = 0i64;
    let mut events = Vec::new();
    while e.neglect.p < cfg.thresholds.l3 as f32 + 1.0 {
        t += 1_000;
        events.extend(e.tick_1s(t, night(t)).events);
        assert!(t < 600 * 60_000, "深夜速率过低，用例预算不足");
    }
    // 继续推进过确认期
    let mut t2 = t;
    while e.neglect.level < 3 && t2 < t + 400_000 {
        t2 += 1_000;
        events.extend(e.tick_1s(t2, night(t2)).events);
    }
    assert_eq!(e.neglect.level, 3);
    assert_eq!(e.state.emotion, EmotionState::SleepyHint, "AC-20：深夜绝不进入 Angry");

    let changed = events.iter().find_map(|ev| match ev {
        EmotionEvent::ColdLevelChanged { to: 3, redirected, .. } => Some(*redirected),
        _ => None,
    });
    assert_eq!(changed, Some(true), "深夜档位迁移须标记 redirected");

    // 重定向动作 = 配置的 nightRedirectActionId（默认 ACT-I-02 打哈欠），不是 L3 的生气动作
    let acts: Vec<(String, u8)> = events
        .iter()
        .filter_map(|ev| match ev {
            EmotionEvent::ForceAction { action_id, priority } => Some((action_id.clone(), *priority)),
            _ => None,
        })
        .collect();
    assert!(
        acts.iter().any(|(id, _)| *id == cfg.rhythm.night_redirect_action_id),
        "深夜应播催睡动作 {}，实际 {acts:?}",
        cfg.rhythm.night_redirect_action_id
    );
    assert!(
        !acts.iter().any(|(id, _)| cfg.levels[3].idle_pool.contains(id)),
        "深夜绝不播 L3 生气动作池：{acts:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// S7-M5 · 自然消气（D-4 收紧：仅 L3→L2，四条件全满足）
// ═══════════════════════════════════════════════════════════════════════════

/// 把引擎送到 L3（P 越 L3 阈值 + 过确认期）。
fn drive_to_l3(e: &mut EmotionEngine<'_>, cfg: &EmotionConfig) {
    e.neglect.p = cfg.thresholds.l3 as f32 + 0.5;
    let mut t = e.state.last_tick_ms;
    let mut guard = 0;
    while e.neglect.level < 3 && guard < 200 {
        t += 1_000;
        guard += 1;
        let _ = e.tick_1s(t, env_day(t, 0));
    }
    assert_eq!(e.neglect.level, 3, "应到达 L3");
}

#[test]
fn natural_cool_only_l3_to_l2_with_four_conditions() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let _ = e.tick_1s(0, env_day(0, 0));
    drive_to_l3(&mut e, &cfg);

    // 条件未满足（无正向交互）→ 不降
    let mut t = e.state.last_tick_ms;
    for _ in 0..120 {
        t += 1_000;
        let _ = e.tick_1s(t, env_day(t, 0));
    }
    assert_eq!(e.neglect.level, 3, "缺正向交互不得自然消气");

    // 补齐三条：P < 15 持续 60s + 期间正向交互 ≥3 + 近 1h 无负向
    e.neglect.p = 10.0;
    for i in 0..cfg.confirm.natural_min_positive {
        let _ = e.on_interaction(true, t + i as i64 * 100);
    }
    let mut saw_natural = false;
    let mut events = Vec::new();
    // 预算 = 保持窗口 60s + 降档确认期 30s + 余量
    let budget = cfg.confirm.natural_hold_sec + cfg.confirm.down_sec + 30;
    for _ in 0..budget {
        t += 1_000;
        events.extend(e.tick_1s(t, env_day(t, 0)).events);
    }
    for ev in &events {
        if let EmotionEvent::ColdLevelChanged { from: 3, to: 2, reason, mood_delta, .. } = ev {
            saw_natural = true;
            assert_eq!(*reason, dp_core::emotion::ColdReason::NaturalCool);
            assert_eq!(*mood_delta, 0.0, "R18-4：降级路径不得产生负向扣减");
        }
    }
    assert!(saw_natural, "四条件满足应触发 L3→L2 自然消气");
    assert_eq!(e.neglect.level, 2);
    // `01 §6.11.8.1`：自然消气结算把 Mood 抬到 `naturalMoodFloor`（45）。容差 0.05：
    // 同一拍在其后的 `step_mood` 里还会走一次自然衰减（drain + decayPerMin ≈ 0.01/min）。
    assert!(
        e.state.values.mood >= cfg.confirm.natural_mood_floor as f32 - 0.05,
        "自然消气后 Mood 应 ≥ {}（实际 {}）",
        cfg.confirm.natural_mood_floor,
        e.state.values.mood
    );
    // 不播 ACT-E-06（与 CoaxFlow 的差异是产品底线）
    assert!(
        !events.iter().any(|ev| matches!(ev, EmotionEvent::ForceAction { action_id, .. } if action_id == "ACT-E-06")),
        "自然消气绝不播 ACT-E-06"
    );
    assert!(
        events.iter().any(|ev| matches!(ev, EmotionEvent::ForceAction { action_id, .. } if *action_id == cfg.confirm.natural_cool_action_id)),
        "应播配置的自然消气动作 {}",
        cfg.confirm.natural_cool_action_id
    );
}

#[test]
fn natural_cool_never_applies_to_l4_or_l5() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let _ = e.tick_1s(0, env_day(0, 0));
    for level in [4u8, 5] {
        e.neglect.level = level;
        e.neglect.p = 10.0;
        e.neglect.pending_level = level;
        e.neglect.pending_since_ms = None;
        e.state.emotion = EmotionState::from_cfg_name(&cfg.levels[level as usize].emotion);
        for _ in 0..cfg.confirm.natural_min_positive {
            let _ = e.on_interaction(true, e.state.last_tick_ms + 1_000);
        }
        let mut t = e.state.last_tick_ms;
        let mut events = Vec::new();
        for _ in 0..200 {
            t += 1_000;
            events.extend(e.tick_1s(t, env_day(t, 0)).events);
        }
        assert_eq!(e.neglect.level, level, "L{level} 不开放自然回退（产品红线）");
        assert!(
            !events.iter().any(|ev| matches!(
                ev,
                EmotionEvent::ColdLevelChanged { reason: dp_core::emotion::ColdReason::NaturalCool, .. }
            )),
            "L{level} 不得出现 NaturalCool 事件"
        );
    }
}

#[test]
fn natural_cool_blocked_during_relation_cooling() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let _ = e.tick_1s(0, env_day(0, 0));
    drive_to_l3(&mut e, &cfg);
    assert_eq!(e.neglect.level, 3);
    force_cooling(&mut e, &cfg);
    // 全程沿用「降温当天」的本地日期：`daily_roll` 只在**日期变化**时结算降温计数，
    // 若这里换回 09-17 会立刻结算 10-04 这一天（含刚补的正向交互）→ 降温被清零，
    // 那测的就不是「降温期是否关闭自然消气」而是「降温能否恢复」了。
    let cooling_day = Local
        .with_ymd_and_hms(2026, 10, 4, 14, 0, 0)
        .single()
        .expect("本地时刻");
    let env_cool = |ms: i64| TickEnv { now_local: cooling_day, ..env_day(ms, 0) };
    e.neglect.p = 10.0;
    for _ in 0..cfg.confirm.natural_min_positive {
        let _ = e.on_interaction(true, e.state.last_tick_ms + 1_000);
    }
    let mut t = e.state.last_tick_ms;
    let mut events = Vec::new();
    for _ in 0..(cfg.confirm.natural_hold_sec + cfg.confirm.down_sec + 120) {
        t += 1_000;
        events.extend(e.tick_1s(t, env_cool(t)).events);
    }
    assert!(e.adapt().is_cooling(&cfg.adapt), "关系降温应保持生效");
    assert_eq!(e.neglect.level, 3, "降温期关闭自然消气通道");
    assert!(!events.iter().any(|ev| matches!(
        ev,
        EmotionEvent::ColdLevelChanged { reason: dp_core::emotion::ColdReason::NaturalCool, .. }
    )));
}

// ═══════════════════════════════════════════════════════════════════════════
// S7-M6 · AC-34：敏感度三档成比例、阈值不变、极端组合被钳制
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ac34_sensitivity_scales_rate_only() {
    let needs = NeedsConfig::default();
    let mut base = None;
    for value in [0.7f32, 1.0, 1.3] {
        let mut cfg2 = EmotionConfig::default();
        cfg2.sensitivity.value = value;
        let mut e = engine_typical(&cfg2, &needs);
        let _ = e.tick_1s(0, env_day(0, 0));
        let _ = e.tick_1s(1_000, env_day(1_000, 0));
        let rate = e.neglect.rate_per_min;
        assert!((rate - value).abs() < 5e-3, "敏感度 {value} → rate 应 ≈{value}，实际 {rate}");
        if (value - 1.0).abs() < 1e-6 {
            base = Some(rate);
        }
        // 阶段阈值不变（单一真源 = emotion.json.thresholds，与敏感度无关）
        assert_eq!(level_for(cfg2.thresholds.l1 as f32, &cfg2.thresholds), 1);
        assert_eq!(level_for(cfg2.thresholds.l2 as f32, &cfg2.thresholds), 2);
        assert_eq!(level_for(cfg2.thresholds.l5 as f32, &cfg2.thresholds), 5);
    }
    assert!(base.is_some());
}

#[test]
fn ac34_extreme_combination_is_clamped() {
    // 超黏人 1.3 × 粘人度 100 → 1.4 × 1.3 = 1.82 → 钳到 rateClamp.max = 1.6
    let mut cfg = EmotionConfig::default();
    cfg.sensitivity.value = 1.3;
    let needs = NeedsConfig::default();
    let mut e = EmotionEngine::new(&cfg, &needs);
    e.set_personality(Personality { clingy: 1.0, ..Personality::from_cfg(&cfg.personality) });
    let _ = e.tick_1s(0, env_day(0, 0));
    let _ = e.tick_1s(1_000, env_day(1_000, 0));
    assert!(
        (e.neglect.rate_per_min - cfg.sensitivity.rate_clamp.max).abs() < 1e-4,
        "极端组合应钳到 {}，实际 {}",
        cfg.sensitivity.rate_clamp.max,
        e.neglect.rate_per_min
    );
    // 下限侧：慢热 0.7 × 低粘人度 → 仍在钳制区间内
    let mut cfg2 = EmotionConfig::default();
    cfg2.sensitivity.value = 0.5;
    let mut e2 = EmotionEngine::new(&cfg2, &needs);
    e2.set_personality(Personality { clingy: 0.0, ..Personality::from_cfg(&cfg2.personality) });
    let _ = e2.tick_1s(0, env_day(0, 0));
    let _ = e2.tick_1s(1_000, env_day(1_000, 0));
    assert!(e2.neglect.rate_per_min >= cfg2.sensitivity.rate_clamp.min - 1e-6);
    assert!(e2.neglect.rate_per_min <= cfg2.sensitivity.rate_clamp.max + 1e-6);
}

// ═══════════════════════════════════════════════════════════════════════════
// S7-M6 · AC-35：穿透 + L5 → 经托盘完成三部曲，不永久卡死、无负向扣减
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ac35_tray_coax_rescues_l5_while_unreachable() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    // 不可达（穿透开启）+ 已达 L5
    let unavailable = TickEnv {
        interaction_available: false,
        ..env_day(0, 0)
    };
    let _ = e.tick_1s(0, unavailable);
    e.neglect.level = 5;
    e.neglect.p = cfg.thresholds.l5 as f32 + 5.0;
    e.state.emotion = EmotionState::Runaway;
    let mut t = 1_000;
    let mut events = Vec::new();
    events.extend(e.tick_1s(t, unavailable).events);
    assert_eq!(e.neglect.level, 5);

    // ① 不可达时 presenceFactor = 0.05，P 不归零（层 ①）
    let p_before = e.neglect.p;
    for _ in 0..5 {
        t += 1_000;
        let _ = e.tick_1s(t, unavailable);
    }
    assert!((e.factors().presence - 0.05).abs() < 1e-6, "层 ①：不可达 presenceFactor = 0.05");
    assert!(e.factors().product > 0.0, "P 不得归零（避免「开穿透 = 永不生气」作弊）");
    assert!(e.neglect.p >= p_before - 1e-6, "不可达期间 P 不应被清空");

    // ② 经托盘完成完整三部曲：呼唤 → 5× 抚摸 → 比心
    let mut all = Vec::new();
    all.extend(e.coax_tray_tap(t)); // Runaway/Away → Recall（走回）
    t += 1_000;
    assert!(matches!(e.coax_step(), CoaxStep::Idle | CoaxStep::Call));
    all.extend(e.coax_tray_tap(t)); // Idle → Call
    assert_eq!(e.coax_step(), CoaxStep::Call);
    for _ in 0..TRAY_COAX_STROKE_TAPS {
        t += 500;
        all.extend(e.coax_tray_tap(t));
    }
    assert_eq!(e.coax_step(), CoaxStep::Heart, "5 格抚摸后应进入比心窗");
    t += 500;
    all.extend(e.coax_tray_tap(t)); // Heart → 完成
    assert!(
        all.iter().any(|ev| matches!(ev, EmotionEvent::CoaxSucceeded { .. })),
        "经托盘应能完成三部曲：{all:?}"
    );

    // ③ L5 不永久卡死：档位回落（coax_target_level 降 2 级）且 Mood 有兜底
    assert_eq!(e.neglect.level, 3, "L5 完成后应降至 L3");
    assert!(e.state.values.mood >= cfg.coax.recover_mood_floor as f32 - 1e-3);
    assert!(!e.is_runaway_away());

    // ④ 降级路径无任何负向扣减（R18-4 红线）
    for ev in &all {
        if let EmotionEvent::ColdLevelChanged { mood_delta, .. } = ev {
            assert_eq!(*mood_delta, 0.0, "降级 / 哄好路径不得扣 Mood：{ev:?}");
        }
    }
    // ⑤ 失败 / 超时分支同样不得扣减
    let mut ev2 = Vec::new();
    for _ in 0..200 {
        t += 1_000;
        ev2.extend(e.tick_1s(t, unavailable).events);
    }
    for ev in &ev2 {
        if let EmotionEvent::CoaxFailed { .. } = ev {
            assert!(e.neglect.p >= 0.0);
        }
    }
}

#[test]
fn layer3_unreachable_fallback_only_l4_to_l3() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let unavailable = TickEnv { interaction_available: false, ..env_day(0, 0) };
    let _ = e.tick_1s(0, unavailable);
    e.neglect.level = 4;
    e.neglect.p = 9.0;
    e.neglect.pending_level = 4;
    e.neglect.pending_since_ms = None;
    e.state.emotion = EmotionState::Angry;

    // 层 ③ 条件：P<10 持续 180s + 近 2h 无负向
    let mut t = 1_000;
    let mut events = Vec::new();
    for _ in 0..400 {
        t += 1_000;
        e.neglect.p = 9.0;
        events.extend(e.tick_1s(t, unavailable).events);
    }
    assert_eq!(e.neglect.level, 3, "不可达时 L4 应经层 ③ 兜底降到 L3");
    assert!(events.iter().any(|ev| matches!(ev, EmotionEvent::ColdLevelChanged { from: 4, to: 3, .. })));

    // L5 同条件绝不降（产品红线）
    e.neglect.level = 5;
    e.neglect.p = 9.0;
    t += 1_000;
    let mut e2_events = Vec::new();
    for _ in 0..400 {
        t += 1_000;
        e.neglect.p = 9.0;
        e2_events.extend(e.tick_1s(t, unavailable).events);
    }
    assert_eq!(e.neglect.level, 5, "L5 任何情况不开放自然回退");
    assert!(!e2_events.iter().any(|ev| matches!(ev, EmotionEvent::ColdLevelChanged { from: 5, .. })));
}

// ═══════════════════════════════════════════════════════════════════════════
// S7-M7 · 不变量（`02 §9.2` ② ③ ⑤ ⑥ ⑪ ⑫）
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn invariant_p_monotonic_and_never_exceeds_cap() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let _ = e.tick_1s(0, env_day(0, 0));
    let mut prev = e.neglect.p;
    let mut t = 0i64;
    while t < 60 * 60_000 {
        t += 1_000;
        let _ = e.tick_1s(t, env_day(t, 0));
        assert!(e.neglect.p >= prev - 1e-6, "无 relief 时 P 单调不减（t={t}）");
        assert!(e.neglect.p <= e.neglect.cap + 1e-4, "P ≤ P_Cap");
        prev = e.neglect.p;
    }
}

#[test]
fn invariant_confirm_window_blocks_premature_transition() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let _ = e.tick_1s(0, env_day(0, 0));
    // 直接压在 L3 阈值之上：确认期内（<60s）不得迁移
    e.neglect.p = cfg.thresholds.l3 as f32 + 1.0;
    let mut t = 0i64;
    let mut events = Vec::new();
    // 确认期起计于首拍（t = 1s），故 t = upSec×1000 时尚未满 upSec
    for _ in 0..cfg.confirm.up_sec {
        t += 1_000;
        events.extend(e.tick_1s(t, env_day(t, 0)).events);
    }
    assert!(
        !events.iter().any(|ev| matches!(ev, EmotionEvent::ColdLevelChanged { .. })),
        "确认期未满不得升级"
    );
    assert_eq!(e.neglect.level, 0);
    // 满确认期后按「逐层结算、绝不跳级」推进：level 一次到位，但事件逐级且每层恰一次
    t += 1_000;
    events.extend(e.tick_1s(t, env_day(t, 0)).events);
    assert_eq!(e.neglect.level, 3, "满 {}s 后结算到 P 对应档位", cfg.confirm.up_sec);
    let steps: Vec<(u8, u8)> = events
        .iter()
        .filter_map(|ev| match ev {
            EmotionEvent::ColdLevelChanged { from, to, .. } => Some((*from, *to)),
            _ => None,
        })
        .collect();
    assert_eq!(steps, vec![(0, 1), (1, 2), (2, 3)], "绝不跳级：逐层各一条事件");
}

#[test]
fn invariant_mood_inertia_matches_tau() {
    // τ_down = 20s：3τ = 60s 内完成 −20 阶跃的 95%
    let mut cfg = EmotionConfig::default();
    cfg.dimensions.mood.decay_per_min = 0.0;
    cfg.dimensions.mood.decay_per_min_active = 0.0;
    cfg.mood.drain_coef = 0.0;
    cfg.sensitivity.value = 1.0;
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let _ = e.tick_1s(0, env_day(0, 0));
    e.state.values.mood = 60.0;
    let down = TickEnv { event_delta: -20.0, ..env_day(0, 0) };
    let _ = e.tick_1s(60_000, down);
    let want = 60.0 - 20.0 * (1.0 - (-60.0f32 / 20.0).exp());
    assert!(
        (e.state.values.mood - want).abs() < 0.4,
        "τ_down=20s 的一次 60s 步应实现 −20 阶跃的 95%：期望 {want}，实际 {}",
        e.state.values.mood
    );

    // τ_up = 90s：3τ = 270s 内完成 +20 阶跃的 95%
    let mut e2 = engine_typical(&cfg, &needs);
    let _ = e2.tick_1s(0, env_day(0, 0));
    e2.state.values.mood = 20.0;
    let up = TickEnv { event_delta: 20.0, ..env_day(0, 0) };
    let _ = e2.tick_1s(270_000, up);
    let want_up = 20.0 + 20.0 * (1.0 - (-270.0f32 / 90.0).exp());
    assert!(
        (e2.state.values.mood - want_up).abs() < 0.4,
        "τ_up=90s 的一次 270s 步应实现 +20 阶跃的 95%：期望 {want_up}，实际 {}",
        e2.state.values.mood
    );
}

#[test]
fn invariant_adapt_and_exp_ranges() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let e = engine_typical(&cfg, &needs);
    let t_exp0 = e.personality().t_exp0(&cfg.adapt);
    assert!((t_exp0 - 25.0).abs() < 1e-6, "T_exp0 = 30 − 10×0.5 = 25");
    for p in [0.0f32, 25.0, 100.0, 1000.0] {
        let mut a = dp_core::emotion::AdaptationState::default();
        a.on_interval("2026-09-17", p, &cfg.adapt);
        let _ = a.roll_day("2026-09-18", t_exp0, &cfg.adapt);
        let f = a.factor(t_exp0, &cfg.adapt);
        assert!((cfg.adapt.factor_min..=cfg.adapt.factor_max).contains(&f), "adaptFactor 越界 {f}");
        assert!(
            (cfg.adapt.t_min as f32..=cfg.adapt.t_max as f32).contains(&a.t_exp()),
            "T_exp 越界 {}",
            a.t_exp()
        );
    }
    // 日变化 ≤10%
    let mut a = dp_core::emotion::AdaptationState::default();
    let before = a.t_exp();
    a.on_interval("2026-09-17", 1.0, &cfg.adapt);
    let _ = a.roll_day("2026-09-18", t_exp0, &cfg.adapt);
    assert!(
        (a.t_exp() - before).abs() <= before * cfg.adapt.max_daily_change + 1e-6,
        "T_exp 单日变化超 ±{}",
        cfg.adapt.max_daily_change
    );
    // P2-2：`rateClamp` 作用于**乘性灵敏度增益**而非原始因子乘积。
    // 若误把钳制加在乘积上，离线速率会被抬到 0.5/min（10 倍）→ 3h 离线 P = 90；
    // 正确口径为 `0.05 × 180 × value.clamp(0.5,1.6)`，即与敏感度**成线性**。
    for (value, want) in [(1.0f32, 9.0f32), (0.5, 4.5), (1.3, 11.7)] {
        let mut e2 = engine_typical(&cfg, &needs);
        e2.set_sensitivity_value(value);
        let _ = e2.tick_1s(0, env_day(0, 0));
        let _ = e2.offline_compensate(3 * 3_600_000, env_day(0, u64::MAX));
        assert!(
            (e2.neglect.p - want).abs() < 0.6,
            "P2-2：敏感度 {value} → 3h 离线 P 应 ≈{want}（线性），实际 {}",
            e2.neglect.p
        );
        assert!(e2.neglect.p < 20.0, "钳制下限绝不可作用于原始乘积（否则 P→90）");
    }
}

#[test]
fn invariant_effective_rate_within_clamp() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = engine_typical(&cfg, &needs);
    let _ = e.tick_1s(0, env_day(0, 0));
    let mut t = 0i64;
    while t < 10 * 60_000 {
        t += 1_000;
        let _ = e.tick_1s(t, env_day(t, 0));
        let r = e.neglect.rate_per_min;
        assert!(r.is_finite() && r >= 0.0, "速率非法：{r}");
    }
    // 常规路径（离场 / 不可达之外）应在钳制区间内
    assert!((cfg.sensitivity.rate_clamp.min..=cfg.sensitivity.rate_clamp.max).contains(&e.neglect.rate_per_min));
}

// ═══════════════════════════════════════════════════════════════════════════
// S7-M7 · 因子组合矩阵（解析解对照）+ 确定性 + 离线分块等价
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn factor_matrix_matches_analytic_solution() {
    let base = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut checked = 0;
    // presence(2) × busyness(3 档取值) × rhythm(6 段各取一个代表时刻) × needs(2) × rough(2) = 144 组
    for away in [false, true] {
        for kps in [0.0f32, 1.2, 3.0] {
            for hour in [6u32, 10, 12, 14, 18, 23] {
                for hungry in [false, true] {
                    for rough in [false, true] {
                        let cfg = base.clone();
                        let mut e = engine_typical(&cfg, &needs);
                        // 注意：负向事件时间戳必须 >0（0 是「从未负向」哨兵）。
                        if rough {
                            let _ = e.on_interaction_kind(dp_core::interaction::InteractionKind::Tickle, 1_000);
                        }
                        let activity = if kps <= 0.0 {
                            None
                        } else {
                            Some(ActivitySample { key_kps: Some(kps), ..ActivitySample::default() })
                        };
                        let mut env = TickEnv {
                            now_local: at(hour, 0),
                            preset_idle_ms: if away { u64::MAX } else { 0 },
                            satiety: if hungry { 20.0 } else { 70.0 },
                            cleanliness: 85.0,
                            activity,
                            ..TickEnv::default()
                        };
                        let _ = e.tick_1s(0, env);
                        // 推进 5 分钟后（不含首拍）的 P 与解析解比对
                        let mut t = 0i64;
                        let mut sum_product = 0.0f32;
                        let mut n = 0u32;
                        while t < 5 * 60_000 {
                            t += 1_000;
                            env.now_local = at(hour, 0);
                            let _ = e.tick_1s(t, env);
                            sum_product += e.neglect.rate_per_min;
                            n += 1;
                        }
                        // 解析解：ΔP = Σ rate × (1/60) 分钟
                        let expected: f32 = sum_product / 60.0;
                        let tol = (expected.abs() * 0.01).max(0.02);
                        assert!(
                            (e.neglect.p - expected).abs() <= tol,
                            "解析解对照失配（away={away} kps={kps} hour={hour} hungry={hungry} rough={rough}）：\
                             引擎 P={} 解析={expected}（n={n}）",
                            e.neglect.p
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked >= 144, "组合覆盖不足：{checked}");
}

#[test]
fn same_input_sequence_yields_identical_output() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut runs = Vec::new();
    for _ in 0..2 {
        let mut e = engine_typical(&cfg, &needs);
        let mut trace = Vec::new();
        let mut t = 0i64;
        while t < 40 * 60_000 {
            t += 1_000;
            let mut env = env_day(t, if t % 5_000 < 2_500 { 0 } else { 200_000 });
            env.now_local = at(14, 0);
            env.event_delta = if t % 30_000 == 0 { 3.0 } else { 0.0 };
            let out = e.tick_1s(t, env);
            trace.push((e.neglect.p, e.neglect.level, e.state.values.mood, out.events.len()));
        }
        runs.push(trace);
    }
    assert_eq!(runs[0], runs[1], "相同输入序列两次运行必须完全一致（确定性）");
}

#[test]
fn offline_chunk_additivity_within_half_percent() {
    // 「分块推进与逐步模拟差 <0.5%」（`02 §9.2` ⑩）在本引擎里的可控等价形式：
    // 离线推进的**步长固定**（`offline.stepSec`），故唯一可自由切分的维度是**调用次数**。
    // 这里验证 `补偿(180min)` 与 `补偿(90min) + 补偿(90min)` 的 P 差 <0.5%
    // ——即「分块推进」与「一次推到底」等价（逐步模拟的口径差异见 `tick_offline_step`
    // 的因子说明：离线是 presence 单因子口径，不走在线感知）。
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e1 = engine_typical(&cfg, &needs);
    let _ = e1.tick_1s(0, env_day(0, 0));
    let _ = e1.offline_compensate(180 * 60_000, env_day(0, u64::MAX));

    let mut e2 = engine_typical(&cfg, &needs);
    let _ = e2.tick_1s(0, env_day(0, 0));
    let _ = e2.offline_compensate(90 * 60_000, env_day(0, u64::MAX));
    let _ = e2.offline_compensate(90 * 60_000, env_day(0, u64::MAX));

    let diff = (e1.neglect.p - e2.neglect.p).abs();
    assert!(
        diff <= e2.neglect.p * 0.005 + 1e-3,
        "离线分块可加性差应 <0.5%：一次={} 两次={}",
        e1.neglect.p,
        e2.neglect.p
    );
    assert_eq!(e1.neglect.level, e2.neglect.level, "分块不得改变档位结果");
}

// ═══════════════════════════════════════════════════════════════════════════
// S7-M4 · 首建性格随机只发生一次，且在出厂区间内
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn initial_personality_roll_happens_once_within_factory_window() {
    let cfg = EmotionConfig::default();
    let needs = NeedsConfig::default();
    let mut e = EmotionEngine::new(&cfg, &needs);
    assert!(!e.personality_rolled());
    let _ = e.tick_1s(0, env_day(0, 0));
    assert!(e.personality_rolled());
    let first = e.personality();
    assert!((0.45..=0.55).contains(&first.clingy), "首建粘人度应在 45~55：{}", first.clingy);
    let _ = e.tick_1s(60_000, env_day(60_000, 0));
    assert_eq!(e.personality(), first, "首建随机只发生一次");
    // 出厂区间保证 AC-16 容差：personalityFactor ∈ [0.96, 1.04]
    let f = e.personality().factor(&cfg.personality);
    assert!((0.96..=1.04).contains(&f), "出厂 personalityFactor 越出 AC-16 容差：{f}");
}

#[test]
fn interaction_policy_projects_settings_values() {
    // 层 ③ 兜底开关：等级下限 ≤4 才算开放
    let mut p = InteractionPolicy::default();
    assert!(p.unreachable_fallback_enabled());
    p.unreachable_floor_level = 5;
    assert!(!p.unreachable_fallback_enabled(), "5 = 完全关闭兜底");
    p.unreachable_floor_level = 4;
    assert!(p.unreachable_fallback_enabled());
}
