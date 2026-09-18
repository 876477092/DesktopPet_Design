//! 活动状态机迁移与派遣前置检查（S8-M1，T-21 段 · 1/4；`02 §5.13` / §5.15）。
//!
//! 纯函数设计（可单测）：所有迁移都是 `(phase, 事件) → 新 phase` 的确定性映射；
//! 派遣前置检查 [`dispatch_precheck`] 只读输入快照并返回 [`DispatchVerdict`]，不
//! 持有状态——由 [`crate::ActivityRuntime`] 组装调用。
//!
//! 口径（`01 §6.13.1` / `02 §5.14` / RV-07 / RV-03）：
//!   - **AC-36**：冷落阶段 L4 生气 / L5 离家出走 → 拒绝派遣（「她在生气，先哄好
//!     她吧」）；原「离家出走强制召回并中断活动」死分支已删除（活动期间 P 冻结
//!     ⇒ 不可能新触 L5，且 L4/L5 禁止派遣，两者不重叠）；
//!   - Energy < 20（RV-03 统一阈值）拒绝打工 / 学习 / 旅游；
//!   - Satiety < 5 拒绝打工 / 学习 / 旅游（RV-01：`[5,20)` 仍可打工且收益 ×0.7）；
//!   - Cleanliness < 15 拒绝**学习**（打工可去，仅收益 ×0.6）；
//!   - 打工当日次数 < 3（`dailyJobLimit`）；
//!   - 安静时段（23:00-05:00）不派遣（`quietHours`）；
//!   - 无进行中活动（唯一性）。

use crate::model::{ActivityKind, ActivityPhase, DispatchVerdict};

/// 派遣前置检查输入（dp-app 从内核快照组装；全部只读）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DispatchCheck {
    /// 当前冷落阶段（0~5；L4/L5 拒绝派遣，AC-36）。
    pub neglect_level: u32,
    /// 当前精力（0~100；<20 拒绝）。
    pub energy: f32,
    /// 当前饱食度（0~100；<5 拒绝）。
    pub satiety: f32,
    /// 当前清洁度（0~100；学习 <15 拒绝）。
    pub cleanliness: f32,
    /// 今日已打工次数（打工 < `dailyJobLimit`）。
    pub today_job_count: u32,
    /// 是否已有进行中活动。
    pub active: bool,
    /// 当前本地小时（0..=23；安静时段不派遣）。
    pub local_hour: u8,
    /// 安静时段配置（from / to 本地小时；默认 23/5）。
    pub quiet_from_hour: u8,
    /// 安静时段配置（to）。
    pub quiet_to_hour: u8,
    /// 打工日限（`activities.json.activity.dailyJobLimit`）。
    pub daily_job_limit: u32,
}

impl DispatchCheck {
    /// 默认检查器（安静时段 23-05、打工日限 3；与 `activities.json` 内置默认一致）。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            neglect_level: 0,
            energy: 100.0,
            satiety: 70.0,
            cleanliness: 85.0,
            today_job_count: 0,
            active: false,
            local_hour: 12,
            quiet_from_hour: 23,
            quiet_to_hour: 5,
            daily_job_limit: 3,
        }
    }
}

/// 派遣前置检查（纯函数；`01 §6.13.1` 唯一性 + 前置校验 + RV-07/03/01）。
///
/// 经济校验（学费 / 旅行券 / 背包道具）随 S8-M5 接入：本函数不消费金币 / 道具，
/// 由 [`crate::ActivityRuntime::dispatch`] 的 `EconomyGate` 预留（`None` = 未接入）。
#[must_use]
pub fn dispatch_precheck(kind: ActivityKind, check: &DispatchCheck) -> DispatchVerdict {
    if check.active {
        return DispatchVerdict::Refuse {
            reason: "心心正在忙呢，等这次活动结束吧".to_string(),
        };
    }
    if check.neglect_level >= 4 {
        return DispatchVerdict::Refuse {
            reason: "她在生气，先哄好她吧".to_string(),
        };
    }
    if check.energy < 20.0 {
        return DispatchVerdict::Refuse {
            reason: "心心没力气出门了，让她休息一下吧".to_string(),
        };
    }
    if check.satiety < 5.0 {
        return DispatchVerdict::Refuse {
            reason: "心心饿得走不动了，先喂点吃的吧".to_string(),
        };
    }
    if kind == ActivityKind::Study && check.cleanliness < 15.0 {
        return DispatchVerdict::Refuse {
            reason: "心心太脏了没法上课，先洗个澡吧".to_string(),
        };
    }
    if kind == ActivityKind::Work && check.today_job_count >= check.daily_job_limit {
        return DispatchVerdict::Refuse {
            reason: "今天打工次数已经用完啦（3 次/日）".to_string(),
        };
    }
    if in_quiet_hours(check.local_hour, check.quiet_from_hour, check.quiet_to_hour) {
        return DispatchVerdict::Refuse {
            reason: "现在是深夜（23:00-05:00），让心心好好休息吧".to_string(),
        };
    }
    DispatchVerdict::Allow
}

/// 安静时段判定（跨零点区间：from=23, to=5 → [23,24) ∪ [0,5)）。
#[must_use]
pub const fn in_quiet_hours(hour: u8, from_hour: u8, to_hour: u8) -> bool {
    if from_hour <= to_hour {
        hour >= from_hour && hour < to_hour
    } else {
        hour >= from_hour || hour < to_hour
    }
}

/// 状态机迁移事件（`02 §5.13` 状态图）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    /// 用户确认派遣且前置校验通过（Idle → Preparing）。
    Dispatch,
    /// 出发演出播完且已阻塞落盘（Preparing → Running）。
    Departed,
    /// 时长届满（Running → Returning）。
    TimeUp,
    /// 用户提前召回（Running → Returning）。
    Recall,
    /// 时钟异常三次 / 存档损坏（Running → Aborted）。
    Anomaly,
    /// 回归演出播完（Returning → Settled）。
    ReportDone,
    /// 结算完成清场（Settled / Aborted → Idle，实例清除）。
    Clear,
}

/// 状态机迁移（纯函数）：非法迁移返回原阶段（不 panic，防御性收口）。
#[must_use]
pub const fn transition(phase: ActivityPhase, event: Transition) -> ActivityPhase {
    match (phase, event) {
        (ActivityPhase::Idle, Transition::Dispatch) => ActivityPhase::Preparing,
        (ActivityPhase::Preparing, Transition::Departed) => ActivityPhase::Running,
        (ActivityPhase::Running, Transition::TimeUp | Transition::Recall) => {
            ActivityPhase::Returning
        }
        (ActivityPhase::Running, Transition::Anomaly) => ActivityPhase::Aborted,
        (ActivityPhase::Returning, Transition::ReportDone) => ActivityPhase::Settled,
        (ActivityPhase::Settled | ActivityPhase::Aborted, Transition::Clear) => {
            ActivityPhase::Idle
        }
        _ => phase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_check() -> DispatchCheck {
        DispatchCheck::new()
    }

    #[test]
    fn allows_valid_dispatch() {
        assert_eq!(dispatch_precheck(ActivityKind::Work, &ok_check()), DispatchVerdict::Allow);
        assert_eq!(dispatch_precheck(ActivityKind::Study, &ok_check()), DispatchVerdict::Allow);
        assert_eq!(dispatch_precheck(ActivityKind::Travel, &ok_check()), DispatchVerdict::Allow);
    }

    #[test]
    fn refuses_when_active_activity() {
        let c = DispatchCheck { active: true, ..ok_check() };
        assert!(matches!(
            dispatch_precheck(ActivityKind::Work, &c),
            DispatchVerdict::Refuse { .. }
        ));
    }

    #[test]
    fn refuses_at_l4_l5_ac36() {
        for level in [4, 5] {
            let c = DispatchCheck { neglect_level: level, ..ok_check() };
            let v = dispatch_precheck(ActivityKind::Travel, &c);
            assert!(
                matches!(v, DispatchVerdict::Refuse { .. }),
                "L{level} 必须拒绝派遣（AC-36）"
            );
            let DispatchVerdict::Refuse { reason } = v else { unreachable!() };
            assert!(reason.contains("生气"), "提示先哄好她：{reason}");
        }
        let c = DispatchCheck { neglect_level: 3, ..ok_check() };
        assert_eq!(dispatch_precheck(ActivityKind::Travel, &c), DispatchVerdict::Allow, "L3 可派遣");
    }

    #[test]
    fn refuses_when_energy_below_20() {
        for e in [0.0, 19.9] {
            let c = DispatchCheck { energy: e, ..ok_check() };
            assert!(matches!(
                dispatch_precheck(ActivityKind::Work, &c),
                DispatchVerdict::Refuse { .. }
            ));
        }
        let c = DispatchCheck { energy: 20.0, ..ok_check() };
        assert_eq!(dispatch_precheck(ActivityKind::Work, &c), DispatchVerdict::Allow, "≥20 放行");
    }

    #[test]
    fn refuses_when_satiety_below_5_rv01() {
        let c = DispatchCheck { satiety: 4.9, ..ok_check() };
        assert!(matches!(
            dispatch_precheck(ActivityKind::Work, &c),
            DispatchVerdict::Refuse { .. }
        ));
        // RV-01：Satiety ∈ [5,20) 仍可打工（收益 ×0.7 由结算侧处理）。
        let c = DispatchCheck { satiety: 10.0, ..ok_check() };
        assert_eq!(dispatch_precheck(ActivityKind::Work, &c), DispatchVerdict::Allow);
    }

    #[test]
    fn refuses_study_when_dirty_but_work_allowed() {
        let c = DispatchCheck { cleanliness: 10.0, ..ok_check() };
        assert!(matches!(
            dispatch_precheck(ActivityKind::Study, &c),
            DispatchVerdict::Refuse { .. }
        ));
        assert_eq!(
            dispatch_precheck(ActivityKind::Work, &c),
            DispatchVerdict::Allow,
            "脏可打工（收益 ×0.6 由结算侧处理）"
        );
    }

    #[test]
    fn enforces_daily_job_limit() {
        let c = DispatchCheck { today_job_count: 3, ..ok_check() };
        assert!(matches!(
            dispatch_precheck(ActivityKind::Work, &c),
            DispatchVerdict::Refuse { .. }
        ));
        let c = DispatchCheck { today_job_count: 2, ..ok_check() };
        assert_eq!(dispatch_precheck(ActivityKind::Work, &c), DispatchVerdict::Allow);
    }

    #[test]
    fn enforces_quiet_hours() {
        let c = DispatchCheck { local_hour: 23, ..ok_check() };
        assert!(matches!(
            dispatch_precheck(ActivityKind::Work, &c),
            DispatchVerdict::Refuse { .. }
        ));
        let c = DispatchCheck { local_hour: 3, ..ok_check() };
        assert!(matches!(
            dispatch_precheck(ActivityKind::Travel, &c),
            DispatchVerdict::Refuse { .. }
        ));
        let c = DispatchCheck { local_hour: 6, ..ok_check() };
        assert_eq!(dispatch_precheck(ActivityKind::Travel, &c), DispatchVerdict::Allow);
    }

    #[test]
    fn quiet_hours_cross_midnight_semantics() {
        assert!(in_quiet_hours(23, 23, 5));
        assert!(in_quiet_hours(4, 23, 5));
        assert!(!in_quiet_hours(5, 23, 5));
        assert!(!in_quiet_hours(12, 23, 5));
        // 非跨零点区间（如 12-14）语义正确。
        assert!(in_quiet_hours(13, 12, 14));
        assert!(!in_quiet_hours(11, 12, 14));
    }

    #[test]
    fn transition_map_is_exhaustive_and_defensive() {
        use ActivityPhase::*;
        use Transition::*;
        assert_eq!(transition(Idle, Dispatch), Preparing);
        assert_eq!(transition(Preparing, Departed), Running);
        assert_eq!(transition(Running, TimeUp), Returning);
        assert_eq!(transition(Running, Recall), Returning);
        assert_eq!(transition(Running, Anomaly), Aborted);
        assert_eq!(transition(Returning, ReportDone), Settled);
        assert_eq!(transition(Settled, Clear), Idle);
        assert_eq!(transition(Aborted, Clear), Idle);
        // 非法迁移防御：原阶段不变（不 panic）。
        assert_eq!(transition(Idle, Departed), Idle);
        assert_eq!(transition(Preparing, Recall), Preparing);
        assert_eq!(transition(Running, Dispatch), Running);
        assert_eq!(transition(Idle, Anomaly), Idle);
        assert_eq!(transition(Settled, Dispatch), Settled);
    }

    #[test]
    fn phase_active_and_finished_helpers() {
        use ActivityPhase::*;
        assert!(Preparing.is_active());
        assert!(Running.is_active());
        assert!(Returning.is_active());
        assert!(!Idle.is_active());
        assert!(!Settled.is_active());
        assert!(!Aborted.is_active());
        assert!(Settled.is_finished());
        assert!(Aborted.is_finished());
        assert!(!Running.is_finished());
    }
}
