//! dp-activity：外出活动（打工 / 课程 / 旅游）运行时（S8-M1/M2，T-21 段 · 1/4 + 2/4）。
//!
//! 架构（`02 §5.13`）：本 crate 持有**纯状态机 + 确定性结算**，不直接读写
//! `dp-core` 的 `EmotionEngine` / `Economy`；与内核的交互全部经 dp-app 的
//! [`ActivityOutcome`] 消费通道：
//!
//! ```text
//! dp-app (coreloop)
//!    │  dispatch(kind, def, dur, now, check) / recall / tick / confirm_*
//!    ▼
//! ActivityRuntime ──► ActivityOutcome（Dispatched / PostcardDue / TimeUp /
//!    │                        Returned / Aborted）
//!    │
//!    ├─ machine.rs  状态机迁移（纯函数）
//!    ├─ clock.rs    WallClock 端口 + 时钟跟踪
//!    ├─ anomaly.rs  时钟异常检测（回拨/前跳/DST/跨时区）
//!    ├─ settle.rs   收益结算（基础→修正→事件→召回比例）
//!    ├─ events.rs   随机事件确定性抽取
//!    └─ postcard.rs 明信片调度 + 旅行日记（S8-M3）
//! ```
//!
//! 边界登记（S8-M1~M4）：经济入账 / 技能升级 / 学费与旅行券扣款归 **S8-M5**，
//! 本模块只计算与判定，`economy_gate` 预留（`None` = 未接入，跳过经济校验）。

pub mod anomaly;
pub mod clock;
pub mod events;
pub mod machine;
pub mod model;
pub mod postcard;
pub mod settle;

use crate::anomaly::{AnomalyCfg, AnomalyTracker, ClockVerdict};
use crate::clock::{ClockTracker, is_deep_night, segment_of_hour};
use crate::machine::{dispatch_precheck, transition, DispatchCheck, Transition};
use crate::model::{
    ActivityError, ActivityInstance, ActivityKind, ActivityPhase, ActivityReward, DispatchVerdict,
    RecallKind, SettleInputs,
};
use dp_core::config::model::ActivityGlobalCfg;

/// 活动运行时门面（`02 §5.13` `ActivityRuntime`）。
#[derive(Debug, Clone)]
pub struct ActivityRuntime {
    /// 活动全局配置（`activities.json` 的 `activity` 段）。
    cfg: ActivityGlobalCfg,
    /// 当前实例（`None` = Idle / Settled 清场后）。
    current: Option<ActivityInstance>,
    /// 当前阶段。
    phase: ActivityPhase,
    /// 时钟跨拍跟踪（回拨 / DST 检测输入）。
    clock: ClockTracker,
    /// 异常跟踪（回拨计数）。
    anomaly: AnomalyTracker,
    /// 时钟异常配置（内置默认；测试可注入）。
    anomaly_cfg: AnomalyCfg,
}

impl ActivityRuntime {
    /// 以活动全局配置构建运行时（初始 Idle）。
    #[must_use]
    pub fn new(cfg: ActivityGlobalCfg) -> Self {
        Self {
            cfg,
            current: None,
            phase: ActivityPhase::Idle,
            clock: ClockTracker::default(),
            anomaly: AnomalyTracker::default(),
            anomaly_cfg: AnomalyCfg::default(),
        }
    }

    /// 从存档恢复（重启 / 离线回归；`02 §5 K-7` D 段 `activity`）。
    ///
    /// `phase` 恢复口径：存档只存进行中（Preparing / Running / Returning）实例；
    /// 其余阶段（Idle / Settled / Aborted）按无实例处理。
    ///
    /// 接收 `cfg`：恢复后的实例在 tick / 结算时仍按定义 ID 查岗位 / 课程 / 旅游配置
    /// （`C7` 配置单源；`restore` 不得悄悄退回默认配置）。
    #[must_use]
    pub fn restore(
        cfg: ActivityGlobalCfg,
        saved: Option<ActivityInstance>,
        phase: ActivityPhase,
    ) -> Self {
        let mut rt = Self::new(cfg);
        if let (Some(inst), true) = (saved, phase.is_active()) {
            rt.current = Some(inst);
            rt.phase = phase;
        }
        rt
    }

    /// 当前实例（`None` = 无进行中活动）。
    #[must_use]
    pub fn current(&self) -> Option<&ActivityInstance> {
        self.current.as_ref()
    }

    /// 当前阶段。
    #[must_use]
    pub const fn phase(&self) -> ActivityPhase {
        self.phase
    }

    /// 是否有进行中活动（Preparing / Running / Returning）。
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.phase.is_active()
    }

    /// 可派遣判定（对外只读；`01 §6.13.1` 唯一性 + AC-36 + RV-07/03/01 + 安静时段）。
    #[must_use]
    pub fn can_dispatch(&self, kind: ActivityKind, check: &DispatchCheck) -> DispatchVerdict {
        if self.is_running() {
            return DispatchVerdict::Refuse {
                reason: "心心正在忙呢，等这次活动结束吧".to_string(),
            };
        }
        dispatch_precheck(kind, check)
    }

    /// 派遣（Idle → Preparing）：前置校验 → 查定义 → 建实例（`end_ms` 为绝对量）。
    ///
    /// `origin_vdc`：明信片默认位置 = 出发前宠物坐标（D-2 约束 2；`None` = 右下角默认）。
    ///
    /// 经济闸门（S8-M5）：学费 / 旅行券 / 背包道具校验随经济系统接入，本阶段跳过。
    pub fn dispatch(
        &mut self,
        kind: ActivityKind,
        def_id: &str,
        dur_min: u32,
        now_ms: i64,
        origin_vdc: Option<(f32, f32)>,
        check: &DispatchCheck,
    ) -> Result<ActivityInstance, ActivityError> {
        if self.is_running() {
            return Err(ActivityError::Refused("心心正在忙呢，等这次活动结束吧".to_string()));
        }
        match dispatch_precheck(kind, check) {
            DispatchVerdict::Allow => {}
            DispatchVerdict::Refuse { reason } => return Err(ActivityError::Refused(reason)),
        }
        // 查定义（jobs / courses / trips）。
        let dur_min = match kind {
            ActivityKind::Work => {
                let job = self
                    .cfg
                    .jobs
                    .iter()
                    .find(|j| j.id == def_id)
                    .ok_or_else(|| ActivityError::UnknownDef(def_id.to_string()))?;
                if !job.duration_options.contains(&dur_min) {
                    return Err(ActivityError::Refused(format!(
                        "该岗位没有 {dur_min} 分钟的档位"
                    )));
                }
                dur_min
            }
            ActivityKind::Study => {
                let course = self
                    .cfg
                    .courses
                    .iter()
                    .find(|c| c.id == def_id)
                    .ok_or_else(|| ActivityError::UnknownDef(def_id.to_string()))?;
                if !course.duration_options.contains(&dur_min) {
                    return Err(ActivityError::Refused(format!(
                        "该课程没有 {dur_min} 分钟的档位"
                    )));
                }
                dur_min
            }
            ActivityKind::Travel => {
                let trip = self
                    .cfg
                    .trips
                    .iter()
                    .find(|t| t.id == def_id)
                    .ok_or_else(|| ActivityError::UnknownDef(def_id.to_string()))?;
                trip.duration_min
            }
        };
        if dur_min > self.cfg.max_duration_min {
            return Err(ActivityError::Refused(format!(
                "单次活动最长 {max} 分钟",
                max = self.cfg.max_duration_min
            )));
        }
        let planned_ms = i64::from(dur_min) * 60_000;
        let mut inst = ActivityInstance {
            id: now_ms as u64,
            kind,
            def_id: def_id.to_string(),
            start_ms: now_ms,
            end_ms: now_ms.saturating_add(planned_ms),
            planned_ms,
            seed: mix_seed(now_ms as u64, def_id),
            origin_vdc,
            postcards_sent: 0,
            postcards_due: Vec::new(),
            rolled_events: Vec::new(),
            recall_ticket: false,
            deferred_settle: false,
        };
        // 旅游：初始化明信片队列（首张 = start + interval；后续每 interval 一张直到 end）。
        if kind == ActivityKind::Travel {
            if let Some(trip) = self.cfg.trips.iter().find(|t| t.id == def_id) {
                let interval_ms = i64::from(trip.postcard_interval_min.max(1)) * 60_000;
                let mut due = Vec::new();
                let mut at = inst.start_ms.saturating_add(interval_ms);
                while at <= inst.end_ms {
                    due.push(at);
                    at = at.saturating_add(interval_ms);
                }
                inst.postcards_due = due;
            }
        }
        self.clock.reset();
        self.anomaly.reset_backward();
        self.current = Some(inst.clone());
        self.phase = transition(self.phase, Transition::Dispatch);
        Ok(inst)
    }

    /// 出发演出完成（Preparing → Running）：阻塞落盘点在 dp-app。
    pub fn confirm_departed(&mut self) -> Result<(), ActivityError> {
        if self.phase != ActivityPhase::Preparing {
            return Err(ActivityError::Busy("出发演出"));
        }
        self.phase = transition(self.phase, Transition::Departed);
        Ok(())
    }

    /// 用户提前召回（Running → Returning；结算在回归演出完成后）。
    pub fn recall(&mut self) -> Result<(), ActivityError> {
        if self.phase != ActivityPhase::Running {
            return Err(if self.phase.is_active() {
                ActivityError::Busy("准备 / 回归演出")
            } else {
                ActivityError::NoActiveActivity
            });
        }
        self.phase = transition(self.phase, Transition::Recall);
        Ok(())
    }

    /// 每拍 tick（1Hz）：时钟观察 → 异常判定 → 明信片调度 → 到期检测 → 事件输出。
    ///
    /// 时间全部来自注入的 `now_ms` / `offset_sec`（C3；`WallClock` 端口取值）。
    pub fn tick(&mut self, now_ms: i64, offset_sec: i32, out: &mut Vec<ActivityOutcome>) {
        if self.current.is_none() {
            return;
        }
        let delta = self.clock.observe(now_ms, offset_sec);
        let verdict = self.anomaly.classify(delta, &self.anomaly_cfg);
        match verdict {
            ClockVerdict::BackwardAbort => {
                // 回拨三次 → 保底结算并清场。
                let reward = self.abort(now_ms);
                out.push(ActivityOutcome::Aborted(reward));
                return;
            }
            ClockVerdict::Backward { .. } => return, // elapsed=0、end_ms 不变
            _ => {}
        }
        // 明信片调度（travel；离线期间到期的一并汇入旅行日记）。
        self.poll_postcards(now_ms, out);
        // 到期检测（含深夜延后 D-1）。
        if let Some(inst) = self.current.as_ref() {
            if now_ms >= inst.end_ms {
                self.start_return(now_ms, offset_sec, out);
            }
        }
    }

    /// 到期（或召回后回归演出完成）→ 结算（Returning → Settled）。
    ///
    /// `inputs` 组装见 [`SettleInputs`]；`recall_kind` 由 dp-app 按召回来源传入
    /// （正常到期 Normal / 用户召回 Early）。
    pub fn confirm_reported(
        &mut self,
        inputs: &SettleInputs,
        recall_kind: RecallKind,
    ) -> Result<ActivityReward, ActivityError> {
        if self.phase != ActivityPhase::Returning {
            return Err(ActivityError::Busy("非回归阶段"));
        }
        let Some(inst) = self.current.as_ref() else {
            return Err(ActivityError::NoActiveActivity);
        };
        let now_ms = inputs.now_ms;
        let ratio = inst.progress_ratio(now_ms);
        let reward = settle::settle(
            inst.kind,
            self.job_of(inst),
            self.course_of(inst),
            self.trip_of(inst),
            (inst.planned_ms / 60_000) as u32,
            inputs,
            ratio,
            recall_kind,
        );
        let mut reward = reward;
        reward.deferred = inst.deferred_settle;
        self.phase = transition(self.phase, Transition::ReportDone);
        Ok(reward)
    }

    /// 异常中止保底结算（Running → Aborted；已完成比例，不再额外打折）。
    pub fn abort(&mut self, now_ms: i64) -> ActivityReward {
        let Some(inst) = self.current.as_ref() else {
            return ActivityReward::default();
        };
        let ratio = inst.progress_ratio(now_ms);
        let inputs = SettleInputs {
            seed: inst.seed,
            now_ms,
            ..SettleInputs::default()
        };
        let mut reward = settle::settle(
            inst.kind,
            self.job_of(inst),
            self.course_of(inst),
            self.trip_of(inst),
            (inst.planned_ms / 60_000) as u32,
            &inputs,
            ratio,
            RecallKind::Abnormal,
        );
        reward.deferred = inst.deferred_settle;
        self.phase = transition(self.phase, Transition::Anomaly);
        self.clear();
        reward
    }

    /// 清场（Settled / Aborted → Idle）：实例清除（结算结果已由调用方消费）。
    pub fn clear(&mut self) {
        self.current = None;
        self.phase = transition(self.phase, Transition::Clear);
        self.anomaly.reset_backward();
    }

    /// 活动全局配置（只读；dp-app 结算惩罚参数 / 前端展示用）。
    #[must_use]
    pub const fn cfg(&self) -> &ActivityGlobalCfg {
        &self.cfg
    }

    /// 当前实例的出发消耗（`cost.energy/cleanliness`；dp-app 前置扣减用；
    /// 旅游无前置数值消耗，返回 `None`——经济费用归 S8-M5）。
    #[must_use]
    pub fn current_cost(&self) -> Option<&dp_core::config::model::CostCfg> {
        let inst = self.current.as_ref()?;
        match inst.kind {
            ActivityKind::Work => self.job_of(inst).map(|j| &j.cost),
            ActivityKind::Study => self.course_of(inst).map(|c| &c.cost),
            ActivityKind::Travel => None,
        }
    }

    /// 当前实例的演出动作引用（出发 / 回归 / 桌面循环 / 明信片；S8-M4 演出接线）。
    #[must_use]
    pub fn action_ids(&self) -> Option<&dp_core::config::model::ActionRefCfg> {
        let inst = self.current.as_ref()?;
        match inst.kind {
            ActivityKind::Work => self.job_of(inst).map(|j| &j.action_ids),
            ActivityKind::Study => self.course_of(inst).map(|c| &c.action_ids),
            ActivityKind::Travel => self.trip_of(inst).map(|t| &t.action_ids),
        }
    }

    /// 当前明信片是否已到期且未推送（S8-M3 前端轮询用）。
    #[must_use]
    pub fn postcard_due(&self, now_ms: i64) -> bool {
        self.current
            .as_ref()
            .map(|i| i.postcards_due.iter().any(|due| *due <= now_ms))
            .unwrap_or(false)
    }

    /// 明信片调度（旅游：`postcardIntervalMin` 到点推送；离线补抽汇入队列）。
    fn poll_postcards(&mut self, now_ms: i64, out: &mut Vec<ActivityOutcome>) {
        let Some(inst) = self.current.as_mut() else { return };
        if inst.kind != ActivityKind::Travel || inst.postcards_due.is_empty() {
            return;
        }
        // 取出全部已到期明信片（离线期间到期的一并汇入旅行日记）。
        let mut due_now = Vec::new();
        inst.postcards_due.retain(|due| {
            if *due <= now_ms {
                due_now.push(*due);
                false
            } else {
                true
            }
        });
        for at in due_now {
            inst.postcards_sent = inst.postcards_sent.saturating_add(1);
            out.push(ActivityOutcome::PostcardDue { at_ms: at, instance: inst.clone() });
        }
    }

    /// 到期 → 回归演出（Running → Returning）；深夜窗口内延后（D-1）。
    fn start_return(&mut self, now_ms: i64, offset_sec: i32, out: &mut Vec<ActivityOutcome>) {
        let Some(inst) = self.current.as_mut() else { return };
        // 本地小时：now_ms + offset → 推算（精确换算交给 dp-app；此处用偏移粗算）。
        let local_hour = local_hour_from(now_ms, offset_sec);
        if is_deep_night(local_hour) && !inst.deferred_settle {
            // 深夜到期 → 延后至次日 07:00（deferUntilHour）。
            let defer_until_hour = self.cfg.quiet_settlement.defer_until_hour;
            inst.deferred_settle = true;
            inst.end_ms = now_ms.saturating_add(deferred_until_ms(local_hour, defer_until_hour));
            // 深夜延后期间不推明信片（suppressPostcard）。
            inst.postcards_due.clear();
            return;
        }
        self.phase = transition(self.phase, Transition::TimeUp);
        out.push(ActivityOutcome::TimeUp(inst.clone()));
    }

    /// 定义查找（结算用）。
    fn job_of(&self, inst: &ActivityInstance) -> Option<&dp_core::config::model::JobCfg> {
        self.cfg.jobs.iter().find(|j| j.id == inst.def_id)
    }

    fn course_of(&self, inst: &ActivityInstance) -> Option<&dp_core::config::model::CourseCfg> {
        self.cfg.courses.iter().find(|c| c.id == inst.def_id)
    }

    fn trip_of(&self, inst: &ActivityInstance) -> Option<&dp_core::config::model::TripCfg> {
        self.cfg.trips.iter().find(|t| t.id == inst.def_id)
    }
}

/// 活动事件（dp-app 消费通道）。
#[derive(Clone, Debug, PartialEq)]
pub enum ActivityOutcome {
    /// 派遣成功（Preparing；dp-app 播出发演出 ACT-N-09/12）。
    Dispatched(ActivityInstance),
    /// 明信片到点（旅游；dp-app 推明信片挂件）。
    PostcardDue { at_ms: i64, instance: ActivityInstance },
    /// 时长届满（Running → Returning；dp-app 播回归演出 ACT-N-10/14）。
    TimeUp(ActivityInstance),
    /// 结算完成（Returning → Settled；dp-app 应用收益）。
    Returned(ActivityReward),
    /// 异常中止（Aborted；保底结算）。
    Aborted(ActivityReward),
}

/// 便捷时段（与 `clock::segment_of_hour` 一致；SettleInputs 组装用）。
#[must_use]
pub fn time_segment_of(local_hour: u8) -> &'static str {
    segment_of_hour(local_hour)
}

/// 由 UTC 毫秒 + 偏移秒粗算本地小时（0..=23；仅用于深夜窗口 / 时段粗判）。
#[must_use]
pub fn local_hour_from(now_ms: i64, offset_sec: i32) -> u8 {
    let local = now_ms.saturating_add(i64::from(offset_sec) * 1000);
    let hours = local.div_euclid(3_600_000);
    hours.rem_euclid(24) as u8
}

/// 深夜延后：从 `hour` 到下一个 `defer_until_hour` 的毫秒数。
#[must_use]
pub fn deferred_until_ms(hour: u8, defer_until_hour: u8) -> i64 {
    let mut diff = i64::from(defer_until_hour).saturating_sub(i64::from(hour));
    if diff < 0 {
        diff += 24;
    }
    if diff == 0 {
        diff = 24;
    }
    diff * 3_600_000
}

/// 实例种子：确定性混合（id / def_id / start_ms）。
#[must_use]
pub fn mix_seed(id: u64, def_id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ id;
    for b in def_id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::DispatchCheck;
    use crate::model::DispatchVerdict;
    use dp_core::config::model::ActivityGlobalCfg;

    fn cfg() -> ActivityGlobalCfg {
        serde_json::from_str(
            r#"{
                "maxConcurrent": 1, "tickSource": "wallClock", "maxDurationMin": 240,
                "quietHours": { "from": "23:00", "to": "05:00", "deferSettlement": "auto" },
                "quietSettlement": { "mute": true, "style": "night", "noPenalty": true,
                                     "deferUntilHour": 7, "replay": "simple", "suppressPostcard": true },
                "recallPenalty": { "rewardRatio": 0.5, "mood": -4, "neglectAdd": 6, "roughStep": 0.15 },
                "dailyJobLimit": 3, "coinDailyCap": 350,
                "economyScale": { "casual": 1.5, "standard": 1.0, "diligent": 0.7 },
                "jobs": [ { "id": "W-01", "name": "咖啡店店员", "icon": "", "wagePerMinute": 1.2,
                    "durationOptions": [15, 30, 60],
                    "unlock": { "affinityLevel": 0, "skills": {} },
                    "cost": { "energy": 12, "cleanliness": 12 },
                    "modifiers": [], "events": [],
                    "actionIds": { "depart": "ACT-N-09", "return": "ACT-N-10", "deskLoop": "", "postcard": "" },
                    "sound": { "depart": null, "return": null } } ],
                "courses": [],
                "trips": [ { "id": "TR-01", "name": "海边", "durationMin": 120,
                    "cost": { "coin": 120, "ticketItem": "ticket_travel" },
                    "unlock": { "affinityLevel": 0, "skills": {} }, "postcardIntervalMin": 40,
                    "rewards": { "photo": "photo_sea_01", "souvenir": "item_shell", "mood": 25,
                                 "affinityExp": 40, "cleanliness": -15 },
                    "weatherTable": [],
                    "actionIds": { "depart": "ACT-N-12", "return": "ACT-N-14", "deskLoop": "", "postcard": "ACT-N-13" },
                    "sound": { "depart": null, "return": null } } ],
                "postcard": { "sizePx": [160, 220], "position": "bottom-right",
                              "draggable": true, "closable": true }
            }"#,
        )
        .expect("cfg 应可反序列化")
    }

    fn check() -> DispatchCheck {
        DispatchCheck::new()
    }

    fn t0() -> i64 {
        1_800_000_000_000 // 固定 UTC 毫秒
    }

    #[test]
    fn dispatch_work_creates_instance_with_absolute_end() {
        let mut rt = ActivityRuntime::new(cfg());
        let inst = rt
            .dispatch(ActivityKind::Work, "W-01", 30, t0(), Some((0.6, 0.8)), &check())
            .expect("派遣应成功");
        assert_eq!(inst.def_id, "W-01");
        assert_eq!(inst.planned_ms, 30 * 60_000);
        assert_eq!(inst.end_ms, t0() + 30 * 60_000, "end_ms 为绝对量");
        assert_eq!(rt.phase(), ActivityPhase::Preparing);
        assert!(rt.is_running());
        rt.confirm_departed().expect("出发确认应成功");
        assert_eq!(rt.phase(), ActivityPhase::Running);
    }

    #[test]
    fn dispatch_refuses_when_active() {
        let mut rt = ActivityRuntime::new(cfg());
        let _ = rt.dispatch(ActivityKind::Work, "W-01", 15, t0(), None, &check());
        let err = rt.dispatch(ActivityKind::Travel, "TR-01", 120, t0() + 1, None, &check());
        assert!(matches!(err, Err(ActivityError::Refused(_))), "进行中拒绝再派遣：{err:?}");
    }

    #[test]
    fn dispatch_unknown_def_errors() {
        let mut rt = ActivityRuntime::new(cfg());
        let err = rt.dispatch(ActivityKind::Work, "W-99", 30, t0(), None, &check());
        assert!(matches!(err, Err(ActivityError::UnknownDef(_))));
    }

    #[test]
    fn dispatch_travel_uses_fixed_duration() {
        let mut rt = ActivityRuntime::new(cfg());
        let inst = rt
            .dispatch(ActivityKind::Travel, "TR-01", 999, t0(), None, &check())
            .expect("旅游时长取配置固定值");
        assert_eq!(inst.planned_ms, 120 * 60_000);
    }

    #[test]
    fn tick_timeup_emits_return_flow() {
        let mut rt = ActivityRuntime::new(cfg());
        let _ = rt.dispatch(ActivityKind::Work, "W-01", 15, t0(), None, &check());
        rt.confirm_departed().unwrap();
        let mut out = Vec::new();
        rt.tick(t0() + 15 * 60_000 + 1000, 28_800, &mut out);
        assert!(out.iter().any(|o| matches!(o, ActivityOutcome::TimeUp(_))), "到期应触发回归：{out:?}");
        assert_eq!(rt.phase(), ActivityPhase::Returning);
    }

    #[test]
    fn confirm_reported_settles_and_clears() {
        let mut rt = ActivityRuntime::new(cfg());
        let _ = rt.dispatch(ActivityKind::Work, "W-01", 15, t0(), None, &check());
        rt.confirm_departed().unwrap();
        let mut out = Vec::new();
        rt.tick(t0() + 15 * 60_000 + 1000, 28_800, &mut out);
        let inputs = SettleInputs {
            mood: 50.0,
            cleanliness: 80.0,
            satiety: 70.0,
            diligence: 60.0,
            economy_scale: 1.0,
            time_segment: time_segment_of(14),
            present: true,
            local_hour: 14,
            seed: 42,
            now_ms: t0() + 15 * 60_000 + 1000,
            recall_neglect_add: 6.0,
            recall_rough_step: 0.15,
            regress_neglect_delta: -20.0,
        };
        let reward = rt.confirm_reported(&inputs, RecallKind::Normal).expect("结算应成功");
        assert!(reward.coin >= 0);
        assert_eq!(reward.kind, RecallKind::Normal);
        assert_eq!(rt.phase(), ActivityPhase::Settled);
        rt.clear();
        assert_eq!(rt.phase(), ActivityPhase::Idle);
        assert!(rt.current().is_none());
    }

    #[test]
    fn early_recall_path() {
        let mut rt = ActivityRuntime::new(cfg());
        let _ = rt.dispatch(ActivityKind::Work, "W-01", 60, t0(), None, &check());
        rt.confirm_departed().unwrap();
        rt.recall().expect("召回应成功");
        assert_eq!(rt.phase(), ActivityPhase::Returning);
        let inputs = SettleInputs {
            mood: 50.0,
            cleanliness: 80.0,
            satiety: 70.0,
            diligence: 60.0,
            economy_scale: 1.0,
            time_segment: "afternoon",
            present: true,
            local_hour: 14,
            seed: 42,
            now_ms: t0() + 30 * 60_000,
            recall_neglect_add: 6.0,
            recall_rough_step: 0.15,
            regress_neglect_delta: -20.0,
        };
        let reward = rt.confirm_reported(&inputs, RecallKind::Early).expect("结算应成功");
        assert_eq!(reward.kind, RecallKind::Early);
        assert_eq!(reward.neglect_delta, 6.0, "提前召回 P+6");
        assert!((reward.rough_delta - 0.15).abs() < 1e-6, "提前召回 rough+0.15");
    }

    #[test]
    fn backward_clock_holds_end_time() {
        let mut rt = ActivityRuntime::new(cfg());
        let _ = rt.dispatch(ActivityKind::Work, "W-01", 15, t0(), None, &check());
        rt.confirm_departed().unwrap();
        let mut out = Vec::new();
        // 第一次回拨：elapsed=0、end_ms 不变。
        rt.tick(t0() + 10_000, 28_800, &mut out);
        rt.tick(t0() + 5_000, 28_800, &mut out);
        assert!(!out.iter().any(|o| matches!(o, ActivityOutcome::TimeUp(_))), "回拨不应到期");
        assert_eq!(rt.current().unwrap().end_ms, t0() + 15 * 60_000);
        // 第三次回拨 → 保底中止。
        rt.tick(t0() - 5_000, 28_800, &mut out);
        rt.tick(t0() - 10_000, 28_800, &mut out);
        assert!(out.iter().any(|o| matches!(o, ActivityOutcome::Aborted(_))), "三次回拨应中止：{out:?}");
        assert_eq!(rt.phase(), ActivityPhase::Idle, "中止后清场");
    }

    #[test]
    fn travel_postcard_scheduling() {
        let mut rt = ActivityRuntime::new(cfg());
        let _ = rt.dispatch(ActivityKind::Travel, "TR-01", 120, t0(), None, &check());
        rt.confirm_departed().unwrap();
        // 首张明信片在 start + 40min 到期。
        let mut out = Vec::new();
        rt.tick(t0() + 40 * 60_000, 28_800, &mut out);
        assert!(
            out.iter().any(|o| matches!(o, ActivityOutcome::PostcardDue { .. })),
            "40min 明信片应到点：{out:?}"
        );
    }

    #[test]
    fn deep_night_defer_semantics() {
        assert!(is_deep_night(23));
        assert_eq!(deferred_until_ms(23, 7), 8 * 3_600_000, "23:00 到次日 07:00 = 8h");
        assert_eq!(deferred_until_ms(3, 7), 4 * 3_600_000, "03:00 到 07:00 = 4h");
        assert_eq!(deferred_until_ms(7, 7), 24 * 3_600_000, "07:00 整 → 次日 07:00");
        // 深夜到期 → 延后：end_ms 被推到深夜外且 deferred_settle=true。
        let mut rt = ActivityRuntime::new(cfg());
        let _ = rt.dispatch(ActivityKind::Work, "W-01", 15, t0(), None, &check());
        rt.confirm_departed().unwrap();
        let mut out = Vec::new();
        rt.tick(t0() + 15 * 60_000, 28_800, &mut out);
        let hour = local_hour_from(t0() + 15 * 60_000, 28_800);
        if is_deep_night(hour) {
            let inst = rt.current().unwrap();
            assert!(inst.deferred_settle, "深夜到期应延后");
            assert_eq!(inst.end_ms, t0() + 15 * 60_000 + deferred_until_ms(hour, 7));
        } else {
            assert!(out.iter().any(|o| matches!(o, ActivityOutcome::TimeUp(_))), "非深夜正常到期");
        }
    }

    #[test]
    fn restore_from_save_recovers_running() {
        let mut rt = ActivityRuntime::new(cfg());
        let inst = rt
            .dispatch(ActivityKind::Work, "W-01", 30, t0(), None, &check())
            .expect("派遣应成功");
        rt.confirm_departed().unwrap();
        let restored = ActivityRuntime::restore(cfg(), Some(inst.clone()), ActivityPhase::Running);
        assert_eq!(restored.phase(), ActivityPhase::Running);
        assert_eq!(restored.current().unwrap().end_ms, t0() + 30 * 60_000);
    }

    #[test]
    fn can_dispatch_uses_precheck() {
        let rt = ActivityRuntime::new(cfg());
        assert_eq!(rt.can_dispatch(ActivityKind::Work, &check()), DispatchVerdict::Allow);
        let mut c = check();
        c.neglect_level = 5;
        assert!(matches!(
            rt.can_dispatch(ActivityKind::Work, &c),
            DispatchVerdict::Refuse { .. }
        ), "L5 拒绝派遣（AC-36）");
    }
}
