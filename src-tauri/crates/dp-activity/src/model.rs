//! 外出活动核心模型（S8-M1，T-21 段 · 1/4；`02 §4.2` dp-activity/src/model.rs）。
//!
//! 承载内容（`02 §4.2` / §5.13）：
//!   - [`ActivityKind`]：打工 / 学习 / 旅游三类；
//!   - [`ActivityPhase`]：可派遣 → 准备出发 → 外出中 → 回归汇报 → 结算完成，
//!     另含 [`ActivityPhase::Aborted`]（时钟异常三次 / 存档损坏的保底出口）；
//!   - [`ActivityInstance`]：单次活动实例——`start_ms` / `end_ms` / `planned_ms`
//!     一律为 **UTC epoch 毫秒绝对量**（`02 §5 K-12`：`end_ms` 为绝对量，离线/跨
//!     时区不受影响）；`seed` 为确定性随机种子（随机事件 / 明信片抽取用）；
//!   - [`ActivityReward`]：结算产物（心币 / 技能点 / Mood / 亲密度 / 道具 ID 列表）。
//!
//! 边界登记（S8-M1~M4）：
//!   - **经济入账归 S8-M5**：本模块只**计算**收益数值并随 [`ActivityReward`] 输出，
//!     心币 / 道具的账本入账由 `dp-economy`（S8-M5）接管；S8-M5 前 dp-app 只
//!     展示结算结果，不入账（登记 §3.3 B22 ③ 的 `jobReward` 等乘子消费点）。
//!   - **技能等级维护归 S8-M5**：学习结算产出技能点数，技能等级 / 升级由经济侧
//!     `skills` 段接管。
//!   - **学费 / 旅行券 / 背包检查归 S8-M5**：S8-M1 的派遣前置检查只做已就绪维度
//!     （情绪 L4/L5、Energy、Satiety、Cleanliness、当日次数、无进行中活动、安静
//!     时段）；金币 / 道具校验随经济系统接入（`EconomyGate` 预留，`None` = 未接入）。

use serde::{Deserialize, Serialize};

/// 活动类别（`02 §4.2` 冻结词汇）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityKind {
    /// 打工（宠物离桌，明信片倒计时挂件；回归递工资袋）。
    Work,
    /// 学习（**桌面可见版**：宠物坐书桌写作业，循环 ACT-N-11）。
    Study,
    /// 旅游（宠物离桌，定时明信片；回归 ACT-N-14）。
    Travel,
}

impl ActivityKind {
    /// 展示名（前端 / 托盘悬浮文案用）。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Work => "打工",
            Self::Study => "学习",
            Self::Travel => "旅游",
        }
    }
}

/// 活动生命周期阶段（`02 §4.2` 冻结词汇，六态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityPhase {
    /// 可派遣（无进行中活动）。
    Idle,
    /// 准备出发（出发演出 ACT-N-09/ACT-N-12 播放中，3~5s；进入即阻塞落盘）。
    Preparing,
    /// 外出中（宠物离桌 / 桌面学习循环；`end_ms` 已落盘）。
    Running,
    /// 回归汇报（回归演出 ACT-N-10/ACT-N-14 播放中，4~5s；结算前写盘）。
    Returning,
    /// 结算完成（奖励入库且宠物回到桌面；结算后写盘）。
    Settled,
    /// 异常中止（时钟回拨三次 / 存档损坏 → 按已完成比例保底结算）。
    Aborted,
}

impl ActivityPhase {
    /// 是否处于「进行中」（有实例需每拍 tick；`Idle` / `Settled` 无实例）。
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Preparing | Self::Running | Self::Returning)
    }

    /// 是否「已结束待清场」（结算完成 / 异常中止，下一次 dispatch 前清除）。
    #[must_use]
    pub const fn is_finished(self) -> bool {
        matches!(self, Self::Settled | Self::Aborted)
    }

    /// 序列化键（存档 / 快照 `phase` 字段；与 serde lowercase 同值）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Returning => "returning",
            Self::Settled => "settled",
            Self::Aborted => "aborted",
        }
    }
}

/// 活动实例（`02 §4.2` 冻结字段；D 段存档 `activity` 的 Rust 镜像）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ActivityInstance {
    /// 实例 ID（单调自增 / 出发时刻，见 [`ActivityRuntime::dispatch`] 造号口径）。
    pub id: u64,
    /// 活动类别。
    pub kind: ActivityKind,
    /// 定义 ID（`W-01` / `CRS-01` / `TR-01`，`activities.json` 键）。
    pub def_id: String,
    /// 出发时刻（UTC epoch 毫秒，绝对量）。
    pub start_ms: i64,
    /// 计划结束时刻（UTC epoch 毫秒，绝对量；= start_ms + dur × 60_000）。
    pub end_ms: i64,
    /// 计划时长（毫秒；召回比例的分母）。
    pub planned_ms: i64,
    /// 确定性随机种子（= hash(id, def_id, start_ms)；事件表 / 明信片抽取用）。
    pub seed: u64,
    /// 明信片默认位置 = 出发前宠物坐标（VDC；D-2 约束 2；`None` = 未知 → 右下角默认）。
    pub origin_vdc: Option<(f32, f32)>,
    /// 已推送明信片数（旅游；`postcardIntervalMin` 调度）。
    pub postcards_sent: u8,
    /// 未推送的明信片到期时刻（队列；离线期间到期的一并汇入「旅行日记」）。
    pub postcards_due: Vec<i64>,
    /// 已抽取的随机事件（seed 确定性；离线补抽取时追记）。
    pub rolled_events: Vec<ActivityEventRoll>,
    /// 是否使用召回券（S8-M5 经济道具；本卡保留字段，入账随 S8-M5）。
    pub recall_ticket: bool,
    /// 深夜回归延后结算标记（D-1：23:00-05:00 不在场 → 延后至次日 07:00）。
    pub deferred_settle: bool,
}

impl Default for ActivityInstance {
    fn default() -> Self {
        Self {
            id: 0,
            kind: ActivityKind::Work,
            def_id: String::new(),
            start_ms: 0,
            end_ms: 0,
            planned_ms: 0,
            seed: 0,
            origin_vdc: None,
            postcards_sent: 0,
            postcards_due: Vec::new(),
            rolled_events: Vec::new(),
            recall_ticket: false,
            deferred_settle: false,
        }
    }
}

impl ActivityInstance {
    /// 已推进比例（`[0,1]`；`planned_ms ≤ 0` 防御 → 1.0）。
    #[must_use]
    pub fn progress_ratio(&self, now_ms: i64) -> f32 {
        if self.planned_ms <= 0 {
            return 1.0;
        }
        let elapsed = (now_ms - self.start_ms).max(0);
        (elapsed as f32 / self.planned_ms as f32).clamp(0.0, 1.0)
    }

    /// 剩余毫秒（已结束 → 0）。
    #[must_use]
    pub fn remaining_ms(&self, now_ms: i64) -> i64 {
        (self.end_ms - now_ms).max(0)
    }
}

/// 已抽取的随机事件（持久化记录，离线回归时不重复计）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ActivityEventRoll {
    /// 事件 ID（`WACT-E-01` / `TACT-E-01`…）。
    pub event_id: String,
    /// 抽取时刻（UTC 毫秒）。
    pub at_ms: i64,
    /// 事件权重（配置值，供审计）。
    pub weight: u32,
}

/// 召回类别（决定收益比例与惩罚口径；`02 §5.14` 异常表）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecallKind {
    /// 正常到期回归（ratio = 1.0）。
    Normal,
    /// 用户提前召回（ratio × 0.5；Mood−4 / P+6 / rough+0.15）。
    Early,
    /// 异常中止保底（时钟回拨三次 / 存档损坏；按已完成比例保底，无额外惩罚）。
    Abnormal,
}

/// 活动结算产物（`02 §5.14`：基础 → 修正 → 事件 → 召回比例）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ActivityReward {
    /// 心币收益（≥0；Work 主收益 / Travel 找零；S8-M5 前只计算不入账）。
    pub coin: i64,
    /// 技能点（Study 主收益；`floor(dur/30 × (0.85+0.3d) × mood 修正)`，最低 1）。
    pub skill_points: u32,
    /// Mood 净变化（回归应用：Work −2~+5 / Travel +25~35 / Study 0）。
    pub mood_delta: f32,
    /// 亲密度经验（Travel +40；Work / Study 0）。
    pub affinity_exp: f32,
    /// 清洁度变化（负 = 消耗；Travel −15）。
    pub cleanliness_delta: f32,
    /// 精力变化（负 = 消耗；出发时已预扣，结算时 0 或补扣）。
    pub energy_delta: f32,
    /// 道具产出 ID 列表（照片 / 纪念品 / 限定道具；背包入账归 S8-M5）。
    pub item_ids: Vec<String>,
    /// 冷落压力净变化（正常回归 P−20；提前召回 P+6；dp-app 应用）。
    pub neglect_delta: f32,
    /// 粗暴因子净变化（提前召回 +0.15；dp-app 应用）。
    pub rough_delta: f32,
    /// 深夜延后结算标记（D-1；dp-app 据此决定是否立即应用）。
    pub deferred: bool,
    /// 结算类别（审计）。
    pub kind: RecallKind,
}

impl Default for ActivityReward {
    fn default() -> Self {
        Self {
            coin: 0,
            skill_points: 0,
            mood_delta: 0.0,
            affinity_exp: 0.0,
            cleanliness_delta: 0.0,
            energy_delta: 0.0,
            item_ids: Vec::new(),
            neglect_delta: 0.0,
            rough_delta: 0.0,
            deferred: false,
            kind: RecallKind::Normal,
        }
    }
}

/// 派遣判定（`02 §4.2`：`Allow` / `Refuse { reason }`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchVerdict {
    /// 允许派遣。
    Allow,
    /// 拒绝派遣（原因展示给用户；AC-36：L4/L5 拒绝）。
    Refuse {
        /// 拒绝原因（中文可读，前端直接展示）。
        reason: String,
    },
}

/// 活动错误（`02 §7.4`；`#[non_exhaustive]` 防外部穷举破坏封闭性）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ActivityError {
    /// 前置校验不通过（含 L4/L5 / Energy / Satiety / Cleanliness / 当日次数 / 进行中）。
    #[error("派遣被拒绝：{0}")]
    Refused(String),
    /// 无进行中活动（召回 / 结算空调用）。
    #[error("当前没有进行中的活动")]
    NoActiveActivity,
    /// 活动已在准备 / 回归演出中（不可重复操作）。
    #[error("活动正在演出中（{0}），请稍候")]
    Busy(&'static str),
    /// 配置缺失（def_id 不在 activities.json 目录）。
    #[error("活动定义不存在：{0}")]
    UnknownDef(String),
    /// 结算上下文缺失（如学习结算需要勤奋 d，而配置未提供）。
    #[error("结算上下文缺失：{0}")]
    MissingContext(&'static str),
}

/// 活动结算所需的「修正乘子」输入（dp-app 从内核快照组装；C3 零时钟——时间全部
/// 由 [`crate::clock`] 的注入值提供）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SettleInputs {
    /// 当前 Mood（0~100；打工收益 ×1.15（≥80）/ ×0.8（<30）；学习效率 ×1.1（≥70）/ ×0.9）。
    pub mood: f32,
    /// 当前 Cleanliness（0~100；打工收益 ×0.6（<15））。
    pub cleanliness: f32,
    /// 当前 Satiety（0~100；打工收益 ×0.7（<20 且 ≥5）——RV-01 解除死亡负循环）。
    pub satiety: f32,
    /// 勤奋性格 d（0~100；归一化后进入 `jobReward` / 技能点公式 `0.85+0.3d`）。
    pub diligence: f32,
    /// 经济倍率档（casual/standard/diligent = 1.5/1.0/0.7；S8-M5 起消费）。
    pub economy_scale: f32,
    /// 时段（morning / forenoon / noon / afternoon / dusk / night；`jobs.modifiers` 用）。
    pub time_segment: &'static str,
    /// 是否在场（深夜回归 D-1 判定）。
    pub present: bool,
    /// 当前本地小时（23-05 深夜窗口判定；`0..=23`）。
    pub local_hour: u8,
    /// 随机事件抽取种子（= 实例 seed；确定性）。
    pub seed: u64,
    /// 随机事件抽取时刻（UTC 毫秒）。
    pub now_ms: i64,
    /// 提前召回惩罚：冷落压力增量 P+6（`02 §5.14` recallPenalty.neglectAdd）。
    pub recall_neglect_add: f32,
    /// 提前召回惩罚：粗暴因子增量 rough+0.15（`02 §5.14` recallPenalty.roughStep）。
    pub recall_rough_step: f32,
    /// 正常回归：冷落压力 P−20（`02 §5.15` 回归 | P−20）。
    pub regress_neglect_delta: f32,
}

/// 存档 D 段 `activity` 的承载形状（`02 §5 K-7`）。
///
/// 由 `dp-app` 在落盘 / 恢复时与 `dp-core::save::SaveFileV2.activity`
/// （`serde_json::Value`）互转——强类型住在活动 crate（依赖方向 dp-activity →
/// dp-core），dp-core 侧保持 Value 冻结（见 `save/schema.rs` S8-M1 口径）。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ActivitySave {
    /// 恢复用阶段（仅进行中阶段会被恢复）。
    pub phase: ActivityPhase,
    /// 实例本体（`None` = 无进行中活动）。
    pub instance: Option<ActivityInstance>,
}

impl Default for ActivitySave {
    fn default() -> Self {
        Self { phase: ActivityPhase::Idle, instance: None }
    }
}
