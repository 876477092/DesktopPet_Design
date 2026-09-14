//! 番茄钟（S2-M7，T-07 / FR-10-1 `01 §6.10`，QI-03 并入）：25/5min 工作-休息循环状态机。
//!
//! 职责（03 台账 §2 S2-M7 卡片）：
//!   - **纯逻辑状态机**：[`Pomodoro`] 的 `start / pause / resume / skip / stop /
//!     tick`，仅计时与事件产出；**不做业务联动**（情绪 / 需求 / 动作提交一律不碰）；
//!   - **事件广播**：状态机事件由上层经 `pet://activity` 域广播（C8：无需新增事件名）；
//!   - **勿扰静默（FR-10-1）**：`tick(now_ms, dnd=true)` 仅抑制提醒类事件
//!     （[`PomodoroEvent::WorkDone`] / [`PomodoroEvent::RestDone`]），阶段推进不受
//!     影响——「勿扰时段静默 = 提醒被抑制，而非停表」；
//!   - **绝对时间锚定（C3）**：`rest_end = work_end + rest_ms`、下一工作段
//!     `end = rest_end + work_ms`（锚自上一阶段的**理论边界**链接），与 tick 实际
//!     到达时刻（过冲量）无关；严禁逐 tick 累加（历史教训：过冲逐 tick 累加曾导致
//!     AC 失败）；
//!   - **时钟回拨防御**：`tick` 发现 `now_ms < last_tick_ms` → 记录（刷新基线）
//!     但不推进完成判定；`end_ms` 为绝对量保持不变（与 `02 §5.14` ClockAnomaly
//!     精神一致；本阶段不做异常枚举）。
//!
//! 边界：本模块零时钟（C3）——所有时刻由调用方注入 `now_ms: i64`；`dnd`（勿扰）
//! 判定由上层（S5-M5 提醒偏好 / S7 装配）传入，此处只消费布尔量。

/// 默认工作段时长：25min（FR-10-1）。
pub const WORK_MS_DEFAULT: i64 = 25 * 60 * 1000;

/// 默认休息段时长：5min（FR-10-1）。
pub const REST_MS_DEFAULT: i64 = 5 * 60 * 1000;

/// 番茄钟配置（时长毫秒）；非法值（≤0）在构造时钳到默认 25 / 5。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PomodoroCfg {
    /// 工作段时长（ms）。
    pub work_ms: i64,
    /// 休息段时长（ms）。
    pub rest_ms: i64,
}

impl PomodoroCfg {
    /// 构造配置；非法值（≤0）钳到默认 25 / 5（`03` S2-M7 卡片）。
    #[must_use]
    pub fn new(work_ms: i64, rest_ms: i64) -> Self {
        Self {
            work_ms: if work_ms > 0 { work_ms } else { WORK_MS_DEFAULT },
            rest_ms: if rest_ms > 0 { rest_ms } else { REST_MS_DEFAULT },
        }
    }
}

impl Default for PomodoroCfg {
    fn default() -> Self {
        Self { work_ms: WORK_MS_DEFAULT, rest_ms: REST_MS_DEFAULT }
    }
}

/// 番茄钟阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PomodoroPhase {
    /// 未开始 / 已停止。
    Idle,
    /// 工作段进行中。
    Working,
    /// 休息段进行中。
    Resting,
    /// 已暂停（`pause` 冻结剩余量，`resume` 回原阶段）。
    Paused,
}

/// 状态机事件（不含持久化；由上层经 `pet://activity` 域广播，C8）。
///
/// `cycle` 为**工作段轮次序号（1 起）**：首次 `start` 记 1；休息段结束 / 跳过后
/// 进入下一工作段时 +1；工作段内手动跳过不变（该段未完成，其后休息仍导向
/// 下一序号）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PomodoroEvent {
    /// 开始番茄钟（Idle → Working，cycle 记 1）。
    Started,
    /// 进入 Working / Resting（阶段起点绝对锚定；`resting` 标识目标阶段，
    /// `cycle`：进入休息 = 刚结束的工作段序号，进入工作 = 新工作段序号）。
    PhaseStarted {
        /// 是否进入休息段。
        resting: bool,
        /// 相关工作段序号。
        cycle: u32,
    },
    /// 工作段**自然**完成（提醒挂点：结束气泡；cycle = 刚完成的工作段序号）。
    WorkDone {
        /// 完成的工作段序号。
        cycle: u32,
    },
    /// 休息段**自然**完成（cycle = 所属工作段序号）。
    RestDone {
        /// 所属的工作段序号。
        cycle: u32,
    },
    /// 已暂停。
    Paused,
    /// 已恢复（回原阶段）。
    Resumed,
    /// 手动跳过当前阶段（`from_resting` = 跳过前是否处于休息段）。
    Skipped {
        /// 跳过前是否处于休息段。
        from_resting: bool,
    },
    /// 已停止（回 Idle，清态）。
    Stopped,
}

/// 番茄钟状态机（纯逻辑；零时钟，`now_ms` 由调用方逐次注入）。
#[derive(Clone, Copy, Debug)]
pub struct Pomodoro {
    /// 配置（构造时已钳制非法值）。
    cfg: PomodoroCfg,
    /// 当前阶段。
    phase: PomodoroPhase,
    /// 当前阶段（Working / Resting）的**绝对**结束时刻（Unix ms）；
    /// 锚定量，与 tick 实际到达时刻无关。
    end_ms: i64,
    /// Paused 时冻结的剩余毫秒（resume 以此为基重算 end_ms）。
    paused_remaining_ms: i64,
    /// Paused 前所处阶段（`false` = Working / `true` = Resting）——resume 恢复原阶段。
    paused_resting: bool,
    /// 工作段轮次序号（1 起；语义见 [`PomodoroEvent`] 文档）。
    cycle: u32,
    /// 最近一次 tick 的时刻（时钟回拨检测基线）。
    last_tick_ms: i64,
    /// `last_tick_ms` 基线是否有效（首个 tick / start 后为真）。
    has_tick: bool,
}

impl Pomodoro {
    /// 构造空闲状态机（phase = Idle，cycle = 0）；非法配置在此统一钳制。
    #[must_use]
    pub fn new(cfg: PomodoroCfg) -> Self {
        // 防御性钳制：字段为 pub，允许绕过 `PomodoroCfg::new` 直构，这里归一兜底。
        let cfg = PomodoroCfg::new(cfg.work_ms, cfg.rest_ms);
        Self {
            cfg,
            phase: PomodoroPhase::Idle,
            end_ms: 0,
            paused_remaining_ms: 0,
            paused_resting: false,
            cycle: 0,
            last_tick_ms: 0,
            has_tick: false,
        }
    }

    /// 开始番茄钟（Idle → Working，cycle 记 1，工作段绝对锚定 `[now, now + work_ms]`）。
    /// 非 Idle 时忽略并返回 `None`（重复 start 不重置进度）。
    pub fn start(&mut self, now_ms: i64) -> Option<PomodoroEvent> {
        if self.phase != PomodoroPhase::Idle {
            return None;
        }
        self.phase = PomodoroPhase::Working;
        self.cycle = 1;
        self.end_ms = now_ms + self.cfg.work_ms;
        self.paused_remaining_ms = 0;
        self.paused_resting = false;
        self.last_tick_ms = now_ms;
        self.has_tick = true;
        Some(PomodoroEvent::Started)
    }

    /// 暂停（Working / Resting → Paused，冻结剩余毫秒）；Idle / Paused 忽略。
    pub fn pause(&mut self, now_ms: i64) -> Option<PomodoroEvent> {
        match self.phase {
            PomodoroPhase::Working | PomodoroPhase::Resting => {
                self.paused_resting = self.phase == PomodoroPhase::Resting;
                self.paused_remaining_ms = (self.end_ms - now_ms).max(0);
                self.phase = PomodoroPhase::Paused;
                Some(PomodoroEvent::Paused)
            }
            _ => None,
        }
    }

    /// 恢复（Paused → 原阶段；`end = now + 冻结剩余`，剩余量守恒）；非 Paused 忽略。
    pub fn resume(&mut self, now_ms: i64) -> Option<PomodoroEvent> {
        if self.phase != PomodoroPhase::Paused {
            return None;
        }
        self.end_ms = now_ms + self.paused_remaining_ms;
        self.phase = if self.paused_resting { PomodoroPhase::Resting } else { PomodoroPhase::Working };
        // 与 skip 同源：本方法以 now_ms 重建锚点，须刷新回拨判定基线。
        self.last_tick_ms = now_ms;
        self.has_tick = true;
        Some(PomodoroEvent::Resumed)
    }

    /// 手动跳过当前阶段：
    ///   - Working → Resting：cycle 不变、**不产 `WorkDone`**（该段未自然完成）；
    ///   - Resting → Working：cycle + 1（进入下一工作段）；
    ///   - Idle / Paused：无动作，返回空。
    ///
    /// 基线同步：本方法以 `now_ms` 重建绝对锚点 `end_ms`，故必须一并刷新
    /// `last_tick_ms` / `has_tick`——否则后续 [`Self::tick`] 会把
    /// 「now_ms < 旧基线」误判为时钟回拨，白白跳过一次完成判定。
    pub fn skip(&mut self, now_ms: i64) -> Vec<PomodoroEvent> {
        match self.phase {
            PomodoroPhase::Working => {
                self.phase = PomodoroPhase::Resting;
                self.end_ms = now_ms + self.cfg.rest_ms;
                self.last_tick_ms = now_ms;
                self.has_tick = true;
                vec![
                    PomodoroEvent::Skipped { from_resting: false },
                    PomodoroEvent::PhaseStarted { resting: true, cycle: self.cycle },
                ]
            }
            PomodoroPhase::Resting => {
                self.cycle += 1;
                self.phase = PomodoroPhase::Working;
                self.end_ms = now_ms + self.cfg.work_ms;
                self.last_tick_ms = now_ms;
                self.has_tick = true;
                vec![
                    PomodoroEvent::Skipped { from_resting: true },
                    PomodoroEvent::PhaseStarted { resting: false, cycle: self.cycle },
                ]
            }
            PomodoroPhase::Idle | PomodoroPhase::Paused => Vec::new(),
        }
    }

    /// 停止（任意 → Idle，清态：cycle 归零、锚点与冻结量清零）；已 Idle 时忽略。
    pub fn stop(&mut self) -> Option<PomodoroEvent> {
        if self.phase == PomodoroPhase::Idle {
            return None;
        }
        self.phase = PomodoroPhase::Idle;
        self.end_ms = 0;
        self.paused_remaining_ms = 0;
        self.paused_resting = false;
        self.cycle = 0;
        Some(PomodoroEvent::Stopped)
    }

    /// 每 tick 推进一次阶段判定；`dnd`（勿扰）= `true` 时抑制 `WorkDone` /
    /// `RestDone` 提醒事件（阶段迁移照常——勿扰时段静默语义，注释登记）。
    ///
    /// 每次至多推进一个阶段迁移；过冲较大时由后续 tick 逐阶段追进（锚点链为
    /// 绝对量，追进结果与时序无关）。时钟回拨（`now_ms < last_tick_ms`）→
    /// 记录（刷新基线）但不推进完成判定，`end_ms` 绝对量不变。
    pub fn tick(&mut self, now_ms: i64, dnd: bool) -> Vec<PomodoroEvent> {
        // 时钟回拨防御（C3 / `02 §5.14` ClockAnomaly 精神，本阶段不做异常枚举）：
        // end_ms 为绝对锚定量保持不变，仅刷新基线并跳过本次完成判定。
        if self.has_tick && now_ms < self.last_tick_ms {
            self.last_tick_ms = now_ms;
            return Vec::new();
        }
        self.last_tick_ms = now_ms;
        self.has_tick = true;

        let mut events: Vec<PomodoroEvent> = Vec::new();
        match self.phase {
            PomodoroPhase::Working if now_ms >= self.end_ms => {
                // 工作段自然完成（提醒挂点：结束气泡）。
                let work_end = self.end_ms;
                let cycle = self.cycle;
                events.push(PomodoroEvent::WorkDone { cycle });
                // 绝对锚定（核心不变量）：rest_end = work_end + rest_ms，
                // 与 tick 过冲量无关；严禁 now + rest_ms 逐次累加。
                self.phase = PomodoroPhase::Resting;
                self.end_ms = work_end + self.cfg.rest_ms;
                events.push(PomodoroEvent::PhaseStarted { resting: true, cycle });
            }
            PomodoroPhase::Resting if now_ms >= self.end_ms => {
                // 休息段自然完成 → cycle + 1 回 Working；
                // 下一工作段锚自 rest_end 链接（非 now）。
                let rest_end = self.end_ms;
                let cycle = self.cycle;
                events.push(PomodoroEvent::RestDone { cycle });
                self.cycle += 1;
                self.phase = PomodoroPhase::Working;
                self.end_ms = rest_end + self.cfg.work_ms;
                events.push(PomodoroEvent::PhaseStarted { resting: false, cycle: self.cycle });
            }
            _ => {}
        }

        // 勿扰时段静默（FR-10-1）：仅抑制提醒类事件（WorkDone / RestDone），
        // 阶段迁移与 PhaseStarted 不受影响。
        if dnd {
            events.retain(|ev| {
                !matches!(ev, PomodoroEvent::WorkDone { .. } | PomodoroEvent::RestDone { .. })
            });
        }
        events
    }

    /// 当前阶段。
    #[must_use]
    pub fn phase(&self) -> PomodoroPhase {
        self.phase
    }

    /// 当前工作段轮次序号（1 起；Idle / 停止后为 0）。
    #[must_use]
    pub fn cycle(&self) -> u32 {
        self.cycle
    }

    /// 剩余毫秒（Paused 返回冻结值；Working / Resting 为 `end_ms - now` 钳非负；Idle 为 0）。
    #[must_use]
    pub fn remaining_ms(&self, now_ms: i64) -> i64 {
        match self.phase {
            PomodoroPhase::Paused => self.paused_remaining_ms,
            PomodoroPhase::Working | PomodoroPhase::Resting => (self.end_ms - now_ms).max(0),
            PomodoroPhase::Idle => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试（FR-10-1：25/5 循环、暂停 / 跳过、绝对锚定不漂移、勿扰、回拨防御）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_enters_working_with_cycle_one() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        assert_eq!(p.phase(), PomodoroPhase::Idle);
        assert_eq!(p.start(1_000), Some(PomodoroEvent::Started));
        assert_eq!(p.phase(), PomodoroPhase::Working);
        assert_eq!(p.cycle(), 1);
        assert_eq!(p.remaining_ms(1_000), WORK_MS_DEFAULT, "工作段绝对锚定 [now, now+work]");
    }

    #[test]
    fn start_is_ignored_when_not_idle() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        assert_eq!(p.start(0), Some(PomodoroEvent::Started));
        assert_eq!(p.start(5_000), None, "Working 时重复 start 忽略");
        assert_eq!(p.cycle(), 1, "重复 start 不得重置进度");
        assert_eq!(p.pause(1_000), Some(PomodoroEvent::Paused));
        assert_eq!(p.start(5_000), None, "Paused 时 start 亦忽略");
    }

    #[test]
    fn tick_before_any_transition_emits_nothing() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        assert!(p.tick(0, false).is_empty(), "Idle 时 tick 为空");
        p.start(1_000);
        assert!(p.tick(2_000, false).is_empty(), "工作段未到点 tick 为空");
        assert_eq!(p.phase(), PomodoroPhase::Working);
    }

    #[test]
    fn tick_at_work_end_transitions_to_resting() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        p.start(0);
        let events = p.tick(WORK_MS_DEFAULT, false); // 恰好到点
        assert_eq!(
            events,
            vec![
                PomodoroEvent::WorkDone { cycle: 1 },
                PomodoroEvent::PhaseStarted { resting: true, cycle: 1 },
            ]
        );
        assert_eq!(p.phase(), PomodoroPhase::Resting);
    }

    #[test]
    fn tick_after_overshoot_keeps_rest_anchor_absolute() {
        // 绝对锚定不漂移核心用例：rest_end = work_end + rest_ms（非 now + rest_ms）。
        let mut p = Pomodoro::new(PomodoroCfg::default());
        p.start(0);
        let work_end = WORK_MS_DEFAULT;
        let overshoot = 37i64;
        let events = p.tick(work_end + overshoot, false);
        assert_eq!(events.len(), 2, "过冲不影响事件序列");
        assert_eq!(p.phase(), PomodoroPhase::Resting);
        assert_eq!(
            p.remaining_ms(work_end + overshoot),
            REST_MS_DEFAULT - overshoot,
            "rest_end = work_end + rest_ms，过冲量不落入剩余量"
        );
    }

    #[test]
    fn rest_completion_returns_to_working_with_next_cycle() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        p.start(0);
        p.tick(WORK_MS_DEFAULT + 10, false); // 进入休息，rest_end = work_end + rest
        let rest_end = WORK_MS_DEFAULT + REST_MS_DEFAULT;
        let events = p.tick(rest_end + 10, false);
        assert_eq!(
            events,
            vec![
                PomodoroEvent::RestDone { cycle: 1 },
                PomodoroEvent::PhaseStarted { resting: false, cycle: 2 },
            ]
        );
        assert_eq!(p.phase(), PomodoroPhase::Working);
        assert_eq!(p.cycle(), 2);
        // 下一工作段锚自 rest_end 链接：end = rest_end + work_ms（非 tick 到达时刻）。
        assert_eq!(
            p.remaining_ms(rest_end + 10),
            WORK_MS_DEFAULT - 10,
            "下一工作段绝对锚定不随 tick 时刻漂移"
        );
    }

    #[test]
    fn pause_freezes_remaining() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        p.start(0);
        let at = 10 * 60 * 1000; // 进行 10min
        assert_eq!(p.pause(at), Some(PomodoroEvent::Paused));
        assert_eq!(p.phase(), PomodoroPhase::Paused);
        assert_eq!(p.remaining_ms(at), WORK_MS_DEFAULT - at, "冻结剩余 15min");
        // Paused 下时间照常流逝，冻结量不变。
        assert_eq!(p.remaining_ms(at + 3 * 60 * 1000), WORK_MS_DEFAULT - at);
    }

    #[test]
    fn resume_restores_phase_and_reanchors() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        p.start(0);
        let at = 10 * 60 * 1000;
        p.pause(at);
        let resumed_at = at + 5 * 60 * 1000;
        assert_eq!(p.resume(resumed_at), Some(PomodoroEvent::Resumed));
        assert_eq!(p.phase(), PomodoroPhase::Working, "回原阶段（暂停前为 Working）");
        assert_eq!(p.remaining_ms(resumed_at), WORK_MS_DEFAULT - at, "剩余量守恒后随时间递减");
    }

    #[test]
    fn pause_resume_remaining_conserved() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        p.start(0);
        let at = 7_777;
        let before = p.remaining_ms(at);
        p.pause(at);
        p.resume(at); // 同一时刻暂停即恢复
        assert_eq!(p.remaining_ms(at), before, "pause/resume 剩余量守恒");
    }

    #[test]
    fn pause_from_idle_returns_none() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        assert_eq!(p.pause(0), None);
        assert_eq!(p.resume(0), None, "非 Paused 时 resume 忽略");
    }

    #[test]
    fn skip_from_working_enters_rest_without_workdone() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        p.start(0);
        let events = p.skip(1_000);
        assert_eq!(
            events,
            vec![
                PomodoroEvent::Skipped { from_resting: false },
                PomodoroEvent::PhaseStarted { resting: true, cycle: 1 },
            ],
            "工作段手动跳过：不产 WorkDone"
        );
        assert_eq!(p.phase(), PomodoroPhase::Resting);
        assert_eq!(p.cycle(), 1, "跳过工作段 cycle 不变");
        assert_eq!(p.remaining_ms(1_000), REST_MS_DEFAULT);
    }

    #[test]
    fn skip_from_resting_enters_next_work_cycle() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        p.start(0);
        p.skip(0); // → Resting
        let events = p.skip(1_000);
        assert_eq!(
            events,
            vec![
                PomodoroEvent::Skipped { from_resting: true },
                PomodoroEvent::PhaseStarted { resting: false, cycle: 2 },
            ]
        );
        assert_eq!(p.phase(), PomodoroPhase::Working);
        assert_eq!(p.cycle(), 2, "跳过休息 → 下一工作段 cycle+1");
        assert_eq!(p.remaining_ms(1_000), WORK_MS_DEFAULT);
    }

    #[test]
    fn skip_from_idle_or_paused_is_empty() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        assert!(p.skip(0).is_empty(), "Idle 时跳过为空");
        p.start(0);
        p.pause(0);
        assert!(p.skip(1_000).is_empty(), "Paused 时跳过为空");
        assert_eq!(p.phase(), PomodoroPhase::Paused);
    }

    #[test]
    fn stop_resets_state_and_is_idempotent() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        assert_eq!(p.stop(), None, "Idle 时 stop 忽略");
        p.start(0);
        assert_eq!(p.stop(), Some(PomodoroEvent::Stopped));
        assert_eq!(p.phase(), PomodoroPhase::Idle);
        assert_eq!(p.cycle(), 0, "停止后清态");
        assert_eq!(p.remaining_ms(999_999), 0);
        assert_eq!(p.stop(), None, "重复 stop 幂等");
    }

    #[test]
    fn dnd_suppresses_reminders_but_phase_advances() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        p.start(0);
        // 勿扰：WorkDone 被抑制，PhaseStarted（迁移）保留。
        let events = p.tick(WORK_MS_DEFAULT, true);
        assert_eq!(events, vec![PomodoroEvent::PhaseStarted { resting: true, cycle: 1 }]);
        assert_eq!(p.phase(), PomodoroPhase::Resting, "阶段推进不受勿扰影响");
        // 休息完成同样只抑制 RestDone。
        let events = p.tick(WORK_MS_DEFAULT + REST_MS_DEFAULT, true);
        assert_eq!(events, vec![PomodoroEvent::PhaseStarted { resting: false, cycle: 2 }]);
        // 对照：勿扰关闭、第 2 工作段未到点时无事件（状态机照常运行）。
        assert!(p.tick(2_000_000, false).is_empty());
    }

    #[test]
    fn reminders_present_without_dnd() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        p.start(0);
        let events = p.tick(WORK_MS_DEFAULT, false);
        assert!(events.contains(&PomodoroEvent::WorkDone { cycle: 1 }));
    }

    #[test]
    fn cfg_invalid_values_clamp_to_default() {
        let cfg = PomodoroCfg::new(-1, 0);
        assert_eq!(cfg, PomodoroCfg::default(), "构造钳制：非法值 → 默认 25/5");
        // 直构（绕过 PomodoroCfg::new）也在 Pomodoro::new 兜底钳制。
        let mut p = Pomodoro::new(PomodoroCfg { work_ms: -1, rest_ms: 0 });
        p.start(0);
        assert_eq!(p.remaining_ms(0), WORK_MS_DEFAULT);
    }

    #[test]
    fn tick_clock_rollback_does_not_advance() {
        let mut p = Pomodoro::new(PomodoroCfg::default());
        p.start(1_000_000);
        let work_end = 1_000_000 + WORK_MS_DEFAULT;
        assert!(p.tick(work_end, false).len() == 2, "正常到点推进");
        // 回拨 tick：仅记录（刷新基线），不推进完成判定。
        assert!(p.tick(work_end - 100_000, false).is_empty(), "回拨 tick 无事件");
        assert_eq!(p.phase(), PomodoroPhase::Resting, "阶段不受回拨影响");
        // end_ms 为绝对量保持不变（休息段剩余按原 rest_end 计算）。
        assert_eq!(
            p.remaining_ms(work_end - 100_000),
            REST_MS_DEFAULT + 100_000,
            "回拨不改锚点：rest_end = work_end + rest_ms"
        );
        // 回拨后恢复正常推进（基线已刷新）。
        let rest_end = work_end + REST_MS_DEFAULT;
        assert_eq!(p.tick(rest_end, false).len(), 2, "后续正常 tick 照常推进");
    }

    #[test]
    fn remaining_ms_is_zero_when_idle() {
        let p = Pomodoro::new(PomodoroCfg::default());
        assert_eq!(p.remaining_ms(1_234_567), 0);
    }
}
