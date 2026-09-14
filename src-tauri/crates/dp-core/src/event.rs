//! `event`：事件总线映射与 `pet://state` 快照投影（**S4-M2**，`02 §7.6` / §4.3）。
//!
//! ## 模块职责（三层分离，单一真源）
//!
//! ```text
//!   ① 算法事件层   emotion::engine::EmotionEvent         （内核语义，非 Tauri 依赖）
//!   ② 线上事件层   event::WireEvent + EVENT_STATE/…      （`pet://<域>` 名 + 载荷）
//!   ③ 传输层       dp-app::coreloop（app.emit）          （唯一 emit 点）
//! ```
//!
//! 本模块承担 ②，即「算法事件 → 线上事件」的**纯函数映射**。`dp-core` **不依赖
//! Tauri**（C8：事件名真源在内核，emit 动作在壳），故此处只产出「事件名 + 载荷」
//! 二元组，由 `dp-app` 逐个 `emit`。
//!
//! ## 事件名白名单（C8，`02 §7.6` 已登记，**本卡不新增任何事件名**）
//!
//! - [`EVENT_STATE`] = `pet://state`：1Hz 全量 [`PetSnapshotV2`]（属性面板 / 原因卡数据源）。
//! - [`EVENT_EMOTION`] = `pet://emotion`：**变更时** 阶段迁移详情 [`EmotionWire`]。
//!
//! 其余 `pet://` 域（`needs` / `activity` / `economy` / `bubble` / …）各有归口卡
//! （T-18 / M7+M8 / S4-M5），**本卡不产**，避免越界登记。
//!
//! ## 与 `02 §4.3` 的一致性
//!
//! [`PetSnapshotV2`] 为 `02 §4.3` TypeScript 接口的 Rust 侧镜像（同名字段，
//! serde `camelCase`）。**七字段因子**在 S4-M1 为「未引入」的显式表达（恒 `1.0`），
//! S7-M4 接入真实七因子求解器后此处自动获得真实值——本模块的投影签名不变。
//!
//! 时间纪律（C3）：本模块**零时钟**，`date` 等时间信息由调用方注入。

use serde::{Deserialize, Serialize};

use crate::emotion::coax::{CoaxFailReason, CoaxStep};
use crate::emotion::engine::{ColdReason, EmotionEngine, EmotionEvent};
use crate::emotion::EmotionState;
use crate::perception::WallClock;

// ---------------------------------------------------------------------------
// 事件名常量（C8：`02 §7.6` 已登记；新增事件必须先登记再启用）
// ---------------------------------------------------------------------------

/// `pet://state` 事件名（`02 §7.6`：core → 设置窗口，载荷 `PetSnapshotV2`，1Hz）。
///
/// 前端对应 `src/shared/ipc.ts` 的 `PET_EVENT.STATE`（同源字符串，C8）。
pub const EVENT_STATE: &str = "pet://state";

/// `pet://emotion` 事件名（`02 §7.6`：core → 全部，载荷 `{from,to,reason,p,level}`，
/// 变更时）。前端对应 `PET_EVENT.EMOTION`。
pub const EVENT_EMOTION: &str = "pet://emotion";

/// `pet://coax` 事件名（`02 §7.6`：core → 宠物窗口，载荷 [`CoaxWire`]，变更时；
/// **S4-M3 登记 2026-09-14**）。前端对应 `PET_EVENT.COAX`。
pub const EVENT_COAX: &str = "pet://coax";

/// `CoaxWire` 载荷版本（v1；与前端 `COAX_CMD_VERSION` 同源）。
pub const COAX_CMD_VERSION: u32 = 1;

/// 快照载荷版本（`02 §4.3` `v: 2`；与前端 `PetSnapshotV2.v` 同源）。
pub const SNAPSHOT_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// ② 线上事件：`pet://` 名 + 载荷（传输层仅做 emit）
// ---------------------------------------------------------------------------

/// 一条待广播的线上事件（事件名 + JSON 载荷）。
///
/// `payload` 为 `serde_json::Value`（而非泛型）的原因：一个 tick 产出的多类事件
/// 载荷类型不同，异构收集需要统一载体；`dp-core` 已将 `serde_json` 作为既有依赖
/// （配置加载用），**不新增依赖**（C9）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireEvent {
    /// `pet://<域>` 事件名（**必须**取自本模块常量 / `02 §7.6` 白名单）。
    pub event: &'static str,
    /// 事件载荷（camelCase 线上格式，由各载荷结构 `Serialize` 产出）。
    pub payload: serde_json::Value,
}

impl WireEvent {
    /// 以载荷构造（`payload` 序列化失败时降级为 `null`，绝不 panic——`02 §7.4.2`）。
    #[must_use]
    pub fn new<T: Serialize>(event: &'static str, payload: &T) -> Self {
        Self {
            event,
            payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
        }
    }
}

// ---------------------------------------------------------------------------
// `pet://emotion` 载荷（`02 §4.3`：`{from,to,reason,p,level}`）
// ---------------------------------------------------------------------------

/// 冷落原因线上格式（`02 §4.3` `ColdReason`；serde 驼峰）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColdReasonWire {
    /// 自然消气。
    NaturalCool,
    /// 道歉三部曲。
    Coax,
    /// 离线补偿。
    Offline,
    /// 常规累积。
    Accumulate,
}

impl From<ColdReason> for ColdReasonWire {
    fn from(value: ColdReason) -> Self {
        match value {
            ColdReason::NaturalCool => Self::NaturalCool,
            ColdReason::Coax => Self::Coax,
            ColdReason::Offline => Self::Offline,
            ColdReason::Accumulate => Self::Accumulate,
        }
    }
}

/// `pet://emotion` 载荷（`02 §4.3` 冻结五字段 + 展示态扩展）。
///
/// `from` / `to` 为变化前后档位；`reason` 为冷落原因；`p` 为当前 P；`level` 为
/// 生效档位（= `to`，单独冗余便于前端免解析 `to`）。
///
/// **展示态扩展**（`state` 字段）非 `02 §4.3` 契约字段，但为 `#[serde(default)]`
/// 的可选附加项，携带 `EmotionState` 供前端做情绪表现切换（如 `SleepyHint`）；
/// 缺省时前端按 `null` 处理，**不破坏契约**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EmotionWire {
    /// 变化前档位。
    pub from: u8,
    /// 变化后档位。
    pub to: u8,
    /// 冷落原因。
    pub reason: ColdReasonWire,
    /// 当前 P。
    pub p: f32,
    /// 生效档位（= `to`）。
    pub level: u8,
    /// 展示态（扩展字段；`02 §4.3` 未登记但前向兼容）。
    pub state: Option<EmotionStateWire>,
}

impl Default for EmotionWire {
    fn default() -> Self {
        Self {
            from: 0,
            to: 0,
            reason: ColdReasonWire::Accumulate,
            p: 0.0,
            level: 0,
            state: None,
        }
    }
}

/// `EmotionState` 的线上镜像（serde 驼峰；与 `02 §4.3` 枚举名同构）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EmotionStateWire {
    /// 待机。
    Idle,
    /// 开心。
    Happy,
    /// 好奇。
    Curious,
    /// 无聊。
    Bored,
    /// 委屈。
    Aggrieved,
    /// 生闷气。
    Sulking,
    /// 生气。
    Angry,
    /// 离家出走。
    Runaway,
    /// 想念。
    Longing,
    /// 困。
    Sleepy,
    /// 睡着。
    Asleep,
    /// 兴奋。
    Excited,
    /// 外出。
    Outing,
    /// 困倦提示。
    SleepyHint,
}

impl From<EmotionState> for EmotionStateWire {
    fn from(value: EmotionState) -> Self {
        match value {
            EmotionState::Idle => Self::Idle,
            EmotionState::Happy => Self::Happy,
            EmotionState::Curious => Self::Curious,
            EmotionState::Bored => Self::Bored,
            EmotionState::Aggrieved => Self::Aggrieved,
            EmotionState::Sulking => Self::Sulking,
            EmotionState::Angry => Self::Angry,
            EmotionState::Runaway => Self::Runaway,
            EmotionState::Longing => Self::Longing,
            EmotionState::Sleepy => Self::Sleepy,
            EmotionState::Asleep => Self::Asleep,
            EmotionState::Excited => Self::Excited,
            EmotionState::Outing => Self::Outing,
            EmotionState::SleepyHint => Self::SleepyHint,
        }
    }
}

// ---------------------------------------------------------------------------
// `pet://coax` 载荷（S4-M3；进度环 + 离家态，`02 §7.6` 登记）
// ---------------------------------------------------------------------------

/// 三部曲子状态线上镜像（`02 §4.3` `CoaxStep`；serde 驼峰）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CoaxStepWire {
    /// 未进行。
    Idle,
    /// 呼唤。
    Call,
    /// 抚摸。
    Stroke,
    /// 比心窗。
    Heart,
    /// 离家演出中。
    Runaway,
    /// 已离家。
    Away,
}

impl From<CoaxStep> for CoaxStepWire {
    fn from(value: CoaxStep) -> Self {
        match value {
            CoaxStep::Idle => Self::Idle,
            CoaxStep::Call => Self::Call,
            CoaxStep::Stroke => Self::Stroke,
            CoaxStep::Heart => Self::Heart,
            CoaxStep::Runaway => Self::Runaway,
            CoaxStep::Away => Self::Away,
        }
    }
}

/// 失败原因线上镜像（`02 §4.3` `CoaxFailReason`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CoaxFailReasonWire {
    /// 被打断。
    Interrupted,
    /// 超时。
    Timeout,
    /// 中途放弃。
    Abandoned,
}

impl From<CoaxFailReason> for CoaxFailReasonWire {
    fn from(value: CoaxFailReason) -> Self {
        match value {
            CoaxFailReason::Interrupted => Self::Interrupted,
            CoaxFailReason::Timeout => Self::Timeout,
            CoaxFailReason::Abandoned => Self::Abandoned,
        }
    }
}

/// `pet://coax` 载荷（进度环 + 离家态 + 成败标志；单一事件承载三部曲全部表现态）。
///
/// 字段语义（前端 `parseCoaxCmd` 逐项对齐）：
/// - `active`：进度环是否可见（`step ∈ {call, stroke, heart}`）；
/// - `away`：是否离家（`true` 时宠物窗口应隐藏；找回 / `force_lower` 后回落 `false`）；
/// - `step` / `ratio`：子状态与进度环比例 0..=1；
/// - `succeeded`：本帧为「三部曲完成」（配合 `ACT-E-06`，进度环隐藏）；
/// - `reason`：本帧为「三部曲失败」时的原因。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CoaxWire {
    /// 载荷结构版本（恒 [`COAX_CMD_VERSION`]）。
    pub version: u32,
    /// 进度环是否可见。
    pub active: bool,
    /// 是否离家（窗口应隐藏）。
    pub away: bool,
    /// 子状态。
    pub step: CoaxStepWire,
    /// 进度环比例 0..=1。
    pub ratio: f32,
    /// 是否三部曲完成。
    pub succeeded: bool,
    /// 失败原因（成功 / 进行中为 `None`）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<CoaxFailReasonWire>,
}

impl Default for CoaxWire {
    fn default() -> Self {
        Self {
            version: COAX_CMD_VERSION,
            active: false,
            away: false,
            step: CoaxStepWire::Idle,
            ratio: 0.0,
            succeeded: false,
            reason: None,
        }
    }
}

// ---------------------------------------------------------------------------
// `PetSnapshotV2`（`02 §4.3` TS 接口的 Rust 侧镜像）
// ---------------------------------------------------------------------------

/// 六维数值投影（`02 §4.3` `values`）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ValuesSnapshot {
    /// 心情 0~100。
    pub mood: f32,
    /// 精力 0~100。
    pub energy: f32,
    /// 派生展示 Boredom 0~100（RV-02）。
    pub boredom: f32,
    /// 饱食度 0~100。
    pub satiety: f32,
    /// 清洁度 0~100。
    pub cleanliness: f32,
    /// 亲密度等级 Lv1~10。
    pub affinity_level: u32,
    /// 亲密度当前级内经验。
    pub affinity_exp: f32,
    /// 升级所需经验（`100 × level`；满级为 0）。
    pub affinity_exp_next: f32,
}

impl Default for ValuesSnapshot {
    fn default() -> Self {
        Self {
            mood: 0.0,
            energy: 0.0,
            boredom: 0.0,
            satiety: 0.0,
            cleanliness: 0.0,
            affinity_level: 1,
            affinity_exp: 0.0,
            affinity_exp_next: 0.0,
        }
    }
}

/// 七因子投影（`02 §4.3` `neglect.factors`）。
///
/// `product` 为原始乘积（未加敏感度），与各分项的关系由 `02 §4.2` 定义。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FactorsSnapshot {
    /// 在场因子。
    pub presence: f32,
    /// 忙碌因子。
    pub busyness: f32,
    /// 性格因子。
    pub personality: f32,
    /// 节律因子。
    pub rhythm: f32,
    /// 需求因子。
    pub needs: f32,
    /// 粗暴交互因子。
    pub rough: f32,
    /// 自适应因子。
    pub adapt: f32,
    /// 原始乘积。
    pub product: f32,
}

impl Default for FactorsSnapshot {
    fn default() -> Self {
        Self {
            presence: 1.0,
            busyness: 1.0,
            personality: 1.0,
            rhythm: 1.0,
            needs: 1.0,
            rough: 1.0,
            adapt: 1.0,
            product: 1.0,
        }
    }
}

/// 敏感度投影（`02 §4.3` `neglect.sensitivity`）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SensitivitySnapshot {
    /// 用户档位值（0.7 / 1.0 / 1.3）。
    pub value: f32,
}

impl Default for SensitivitySnapshot {
    fn default() -> Self {
        Self { value: 1.0 }
    }
}

/// 冷落压力投影（`02 §4.3` `neglect`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NeglectSnapshot {
    /// 当前 P。
    pub p: f32,
    /// 当前封顶。
    pub cap: f32,
    /// 生效档位 0..=5。
    pub level: u8,
    /// 生效速率（含敏感度）。
    pub rate_per_min: f32,
    /// 七因子。
    pub factors: FactorsSnapshot,
    /// 敏感度。
    pub sensitivity: SensitivitySnapshot,
    /// 原因卡条目（原因卡归 S7；本卡只留空列表占位以保证契约形状）。
    pub reasons: Vec<NeglectReasonWire>,
}

impl Default for NeglectSnapshot {
    fn default() -> Self {
        Self {
            p: 0.0,
            cap: 0.0,
            level: 0,
            rate_per_min: 0.0,
            factors: FactorsSnapshot::default(),
            sensitivity: SensitivitySnapshot::default(),
            reasons: Vec::new(),
        }
    }
}

/// 原因卡条目（`02 §4.3` `NeglectReason`；S7 填充，本卡保持结构形状）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NeglectReasonWire {
    /// 因子键。
    pub factor_key: String,
    /// 展示文案。
    pub label: String,
    /// 权重。
    pub weight: f32,
    /// 方向。
    pub dir: NeglectReasonDirWire,
    /// 建议（可选）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub advice: Option<String>,
}

/// 原因方向（`02 §4.3` `dir: 'faster' | 'slower'`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NeglectReasonDirWire {
    /// 加速冷落。
    Faster,
    /// 减缓冷落。
    Slower,
}

/// 性格投影（`02 §4.3` `personality`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersonalitySnapshot {
    /// 性格描述文本。
    pub text: String,
    /// 剩余重掷次数。
    pub reroll_left: u32,
    /// 是否可重掷。
    pub can_reroll: bool,
}

/// 经济投影（`02 §4.3` `economy`；真值归 S8 经济系统，本卡留形状）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EconomySnapshot {
    /// 心币余额。
    pub coin: i64,
    /// 今日已赚。
    pub today_earned: i64,
    /// 每日上限。
    pub daily_cap: i64,
}

/// `pet://state` 载荷（`02 §4.3` `PetSnapshotV2`）。
///
/// 字段顺序 / 命名与 TS 接口逐项对应；`#[serde(default)]` 保证新增字段前向兼容
/// （旧前端忽略未知字段，缺字段取默认）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PetSnapshotV2 {
    /// 契约版本（恒 [`SNAPSHOT_VERSION`]）。
    pub v: u32,
    /// 六维数值。
    pub values: ValuesSnapshot,
    /// 冷落压力。
    pub neglect: NeglectSnapshot,
    /// 性格。
    pub personality: PersonalitySnapshot,
    /// 活动快照（归 S8；本卡恒 `null`）。
    pub activity: Option<serde_json::Value>,
    /// 经济。
    pub economy: EconomySnapshot,
    /// 背包（归 S8；本卡恒空）。
    pub inventory: Vec<InventoryItemWire>,
    /// 技能（归 S8/S9；本卡恒空）。
    pub skills: std::collections::BTreeMap<String, SkillSnapshot>,
    /// 展示态（扩展字段；便于属性面板一并渲染当前情绪）。
    pub state: EmotionStateWire,
}

impl Default for PetSnapshotV2 {
    fn default() -> Self {
        Self {
            v: SNAPSHOT_VERSION,
            values: ValuesSnapshot::default(),
            neglect: NeglectSnapshot::default(),
            personality: PersonalitySnapshot::default(),
            activity: None,
            economy: EconomySnapshot::default(),
            inventory: Vec::new(),
            skills: std::collections::BTreeMap::new(),
            state: EmotionStateWire::Idle,
        }
    }
}

/// 背包条目（`02 §4.3` `inventory[{itemId,qty}]`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryItemWire {
    /// 物品 ID。
    pub item_id: String,
    /// 数量。
    pub qty: u32,
}

/// 技能条目（`02 §4.3` `skills[<id>]`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SkillSnapshot {
    /// 等级。
    pub level: u32,
    /// 技能点。
    pub points: u32,
}

// ---------------------------------------------------------------------------
// 投影：`EmotionEngine` → `PetSnapshotV2`（纯函数，零时钟）
// ---------------------------------------------------------------------------

/// 由情绪内核当前状态投影 1Hz 快照（`02 §4.3`）。
///
/// 本函数是**唯一**的 `pet://state` 载荷生产者；调用方（`dp-app` 业务档）每 tick
/// 调用一次并 `emit`。所有时间无关（C3），不读时钟、无副作用。
///
/// 七因子口径：S4-M1 内核只引入 `presence` × `interaction` 两项，其余五项恒 `1.0`
/// （「未引入」的显式表达）；`product` 取内核当前原始乘积。S7-M4 接入后无需改本函数。
///
/// `personality_text` / `reroll_left` 由调用方注入（S7 性格面板；本卡默认空 / 0），
/// 保持本函数对 `EmotionEngine` 的单一依赖。
#[must_use]
pub fn project_snapshot(engine: &EmotionEngine<'_>, personality_text: &str, reroll_left: u32) -> PetSnapshotV2 {
    let values = &engine.state.values;
    let d = &engine.cfg().dimensions;
    let exp_next = if values.affinity_level >= d.affinity.max_level {
        0.0
    } else {
        100.0 * values.affinity_level as f32
    };
    let factors = engine.factors_snapshot();
    PetSnapshotV2 {
        v: SNAPSHOT_VERSION,
        values: ValuesSnapshot {
            mood: values.mood,
            energy: values.energy,
            boredom: engine.boredom_display(),
            satiety: values.satiety,
            cleanliness: values.cleanliness,
            affinity_level: values.affinity_level,
            affinity_exp: values.affinity_exp,
            affinity_exp_next: exp_next,
        },
        neglect: NeglectSnapshot {
            p: engine.neglect.p,
            cap: engine.neglect.cap,
            level: engine.neglect.level,
            rate_per_min: engine.neglect.rate_per_min,
            factors,
            sensitivity: SensitivitySnapshot { value: engine.sensitivity.value },
            reasons: Vec::new(),
        },
        personality: PersonalitySnapshot {
            text: personality_text.to_string(),
            reroll_left,
            can_reroll: reroll_left > 0,
        },
        activity: None,
        economy: EconomySnapshot::default(),
        inventory: Vec::new(),
        skills: std::collections::BTreeMap::new(),
        state: engine.state.emotion.into(),
    }
}

/// 由 `WallClock` 与内核状态生成完整 1Hz 快照（便捷封装；时间经端口注入，C3）。
#[must_use]
pub fn snapshot_now(
    engine: &EmotionEngine<'_>,
    _wall: &dyn WallClock,
    personality_text: &str,
    reroll_left: u32,
) -> PetSnapshotV2 {
    project_snapshot(engine, personality_text, reroll_left)
}

// ---------------------------------------------------------------------------
// 映射：`EmotionEvent` → 线上事件（纯函数）
// ---------------------------------------------------------------------------

/// 把内核算法事件映射为线上事件（`02 §7.6` 白名单内，**不新增事件名**）。
///
/// 映射口径（`02 §6.2` 时序的 S4-M2/S4-M3 承接面）：
///
/// | 算法事件 | 线上事件 | 说明 |
/// |---|---|---|
/// | [`EmotionEvent::ColdLevelChanged`] | `pet://emotion` | 阶段迁移详情 |
/// | [`EmotionEvent::CoaxProgress`] | `pet://coax` | 进度环 / 子状态（含离家态） |
/// | [`EmotionEvent::CoaxSucceeded`] | `pet://coax` | 完成标志（`succeeded=true`） |
/// | [`EmotionEvent::CoaxFailed`] | `pet://coax` | 失败原因 |
/// | [`EmotionEvent::ValuesChanged`] | `pet://state` | 数值变化（1Hz 快照同源，此处**不重复发**） |
/// | [`EmotionEvent::ForceAction`] | —（无线上事件） | 走动作仲裁，非事件面 |
/// | [`EmotionEvent::PersistNow`] | —（无线上事件） | 存盘请求归 S5 存档层 |
///
/// 返回 `None` 表示该事件不产出线上广播（这是**大多数**事件的常态，避免 1Hz 无谓
/// 广播）。`ValuesChanged` 特判为 `None` 的原因：`pet://state` 由
/// [`project_snapshot`] 每 tick 全量投影（设置窗口只关心最新态，逐条数值事件会
/// 造成 1Hz 冗余广播）。
#[must_use]
pub fn wire_for_event(ev: &EmotionEvent, engine: &EmotionEngine<'_>) -> Option<WireEvent> {
    match ev {
        EmotionEvent::ColdLevelChanged { from, to, reason, .. } => {
            let payload = EmotionWire {
                from: *from,
                to: *to,
                reason: (*reason).into(),
                p: engine.neglect.p,
                level: *to,
                state: Some(engine.state.emotion.into()),
            };
            Some(WireEvent::new(EVENT_EMOTION, &payload))
        }
        EmotionEvent::CoaxProgress { ratio, step } => {
            let payload = CoaxWire {
                version: COAX_CMD_VERSION,
                active: step.ring_visible(),
                away: *step == CoaxStep::Away,
                step: (*step).into(),
                ratio: *ratio,
                succeeded: false,
                reason: None,
            };
            Some(WireEvent::new(EVENT_COAX, &payload))
        }
        EmotionEvent::CoaxSucceeded { .. } => {
            let payload = CoaxWire {
                version: COAX_CMD_VERSION,
                active: false,
                away: false,
                step: CoaxStepWire::Idle,
                ratio: 0.0,
                succeeded: true,
                reason: None,
            };
            Some(WireEvent::new(EVENT_COAX, &payload))
        }
        EmotionEvent::CoaxFailed { reason } => {
            let payload = CoaxWire {
                version: COAX_CMD_VERSION,
                active: false,
                away: false,
                step: CoaxStepWire::Idle,
                ratio: 0.0,
                succeeded: false,
                reason: Some((*reason).into()),
            };
            Some(WireEvent::new(EVENT_COAX, &payload))
        }
        EmotionEvent::ValuesChanged { .. }
        | EmotionEvent::ForceAction { .. }
        | EmotionEvent::PersistNow => None,
    }
}

/// 把一批算法事件映射为线上事件列表（保序；过滤不产出方）。
#[must_use]
pub fn wire_for_events(events: &[EmotionEvent], engine: &EmotionEngine<'_>) -> Vec<WireEvent> {
    events.iter().filter_map(|ev| wire_for_event(ev, engine)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{EmotionConfig, NeedsConfig};
    use crate::emotion::engine::NeglectPressure;

    fn engine() -> EmotionEngine<'static> {
        let cfg: &'static EmotionConfig = Box::leak(Box::new(EmotionConfig::default()));
        let needs: &'static NeedsConfig = Box::leak(Box::new(NeedsConfig::default()));
        EmotionEngine::new(cfg, needs)
    }

    #[test]
    fn event_names_match_doc_whitelist() {
        assert_eq!(EVENT_STATE, "pet://state");
        assert_eq!(EVENT_EMOTION, "pet://emotion");
        assert_eq!(SNAPSHOT_VERSION, 2);
    }

    #[test]
    fn snapshot_projects_core_fields() {
        let e = engine();
        let snap = project_snapshot(&e, "有点黏人", 3);
        assert_eq!(snap.v, 2);
        assert_eq!(snap.values.mood, e.state.values.mood);
        assert_eq!(snap.values.affinity_level, e.state.values.affinity_level);
        // 满级前 expNext = 100 × level
        assert_eq!(snap.values.affinity_exp_next, 100.0 * snap.values.affinity_level as f32);
        assert_eq!(snap.neglect.sensitivity.value, e.sensitivity.value);
        assert_eq!(snap.personality.text, "有点黏人");
        assert_eq!(snap.personality.reroll_left, 3);
        assert!(snap.personality.can_reroll);
        assert!(snap.activity.is_none());
        assert!(snap.inventory.is_empty());
        assert!(snap.skills.is_empty());
    }

    #[test]
    fn snapshot_serializes_to_camel_case_contract() {
        let e = engine();
        let snap = project_snapshot(&e, "", 0);
        let json = serde_json::to_value(&snap).expect("快照可序列化");
        // 契约字段名逐项核对（`02 §4.3` TS 接口）
        assert!(json.get("v").is_some());
        assert!(json["values"].get("affinityLevel").is_some());
        assert!(json["values"].get("affinityExpNext").is_some());
        assert!(json["neglect"].get("ratePerMin").is_some());
        assert!(json["neglect"].get("factors").is_some());
        assert!(json["neglect"]["factors"].get("product").is_some());
        assert!(json["neglect"].get("sensitivity").is_some());
        assert!(json["neglect"].get("reasons").is_some());
        assert!(json["economy"].get("todayEarned").is_some());
        assert!(json["economy"].get("dailyCap").is_some());
        // 无 snake_case 泄漏
        assert!(json["values"].get("affinity_level").is_none());
    }

    #[test]
    fn cold_level_changed_maps_to_emotion_wire() {
        let mut e = engine();
        e.neglect = NeglectPressure { p: 42.5, cap: 120.0, level: 3, ..NeglectPressure::default() };
        e.state.emotion = crate::emotion::EmotionState::Angry;
        let ev = EmotionEvent::ColdLevelChanged {
            from: 2,
            to: 3,
            mood_delta: -4.0,
            redirected: false,
            reason: ColdReason::Accumulate,
        };
        let wire = wire_for_event(&ev, &e).expect("阶段迁移应产出线上事件");
        assert_eq!(wire.event, EVENT_EMOTION);
        assert_eq!(wire.payload["from"], 2);
        assert_eq!(wire.payload["to"], 3);
        assert_eq!(wire.payload["reason"], "accumulate");
        assert_eq!(wire.payload["level"], 3);
        assert_eq!(wire.payload["state"], "angry");
        let p = wire.payload["p"].as_f64().expect("p 为数值");
        assert!((p - 42.5).abs() < 1e-6);
    }

    #[test]
    fn values_changed_and_force_action_produce_no_wire_event() {
        let mut e = engine();
        e.state.emotion = crate::emotion::EmotionState::Idle;
        let values = EmotionEvent::ValuesChanged {
            mood: 50.0,
            energy: 80.0,
            boredom: 10.0,
            satiety: 60.0,
            cleanliness: 70.0,
        };
        assert!(wire_for_event(&values, &e).is_none());
        let force = EmotionEvent::ForceAction { action_id: "ACT-T-07".to_string(), priority: 7 };
        assert!(wire_for_event(&force, &e).is_none());
        assert!(wire_for_event(&EmotionEvent::PersistNow, &e).is_none());
    }

    #[test]
    fn wire_for_events_preserves_order_and_filters() {
        let mut e = engine();
        e.neglect.level = 2;
        let events = vec![
            EmotionEvent::ValuesChanged {
                mood: 50.0,
                energy: 80.0,
                boredom: 10.0,
                satiety: 60.0,
                cleanliness: 70.0,
            },
            EmotionEvent::ColdLevelChanged {
                from: 1,
                to: 2,
                mood_delta: -3.0,
                redirected: false,
                reason: ColdReason::Accumulate,
            },
            EmotionEvent::PersistNow,
        ];
        let wires = wire_for_events(&events, &e);
        assert_eq!(wires.len(), 1, "仅阶段迁移产出线上事件");
        assert_eq!(wires[0].event, EVENT_EMOTION);
    }

    /// 七因子投影自洽（`02 §4.3`）：`product` 必须等于各因子连乘。
    ///
    /// 前端属性面板据 `factors` 逐项展示并可能反算校验，若两者不自洽会产生
    /// 「原因卡权重与 P 增速对不上」的观感缺陷。
    #[test]
    fn factors_snapshot_product_is_consistent_with_components() {
        let e = engine();
        let f = e.factors_snapshot();
        let expected = f.presence * f.busyness * f.personality * f.rhythm * f.needs * f.rough * f.adapt;
        assert!(
            (f.product - expected).abs() < 1e-5,
            "product={} 与因子连乘={} 不自洽",
            f.product,
            expected
        );
        // S4-M1 未引入的因子必须显式为 1.0（不是 0，避免前端显示成「因子归零」）
        assert_eq!(f.busyness, 1.0);
        assert_eq!(f.personality, 1.0);
        assert_eq!(f.rhythm, 1.0);
        assert_eq!(f.needs, 1.0);
        assert_eq!(f.rough, 1.0);
    }

    #[test]
    fn cold_reason_wire_round_trips_all_variants() {        for (src, want) in [
            (ColdReason::NaturalCool, "naturalCool"),
            (ColdReason::Coax, "coax"),
            (ColdReason::Offline, "offline"),
            (ColdReason::Accumulate, "accumulate"),
        ] {
            let wire = ColdReasonWire::from(src);
            assert_eq!(serde_json::to_value(wire).unwrap(), serde_json::json!(want));
        }
    }

    #[test]
    fn emotion_state_wire_covers_all_variants() {
        // 遍历内核全部 14 个展示态，确保映射链完整（无遗漏变体）
        for st in [
            EmotionState::Idle,
            EmotionState::Happy,
            EmotionState::Curious,
            EmotionState::Bored,
            EmotionState::Aggrieved,
            EmotionState::Sulking,
            EmotionState::Angry,
            EmotionState::Runaway,
            EmotionState::Longing,
            EmotionState::Sleepy,
            EmotionState::Asleep,
            EmotionState::Excited,
            EmotionState::Outing,
            EmotionState::SleepyHint,
        ] {
            let wire = EmotionStateWire::from(st);
            let json = serde_json::to_value(wire).expect("展示态可序列化");
            assert!(json.is_string());
        }
    }
}
