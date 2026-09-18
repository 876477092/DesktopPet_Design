//! `emotion::engine`：情绪 tick 编排内核（`02 §5 K-5` / §5.1~5.5；S4-M1 骨架 + S7-M4~M7 合入）。
//!
//! ## 时间纪律（C3）
//!
//! 本模块**零时钟**：所有时间点由调用方注入 `now_ms: i64`（墙钟毫秒，口径与
//! `dp-app` 的 `WallClock::now_ms()` 一致）。`now_local` 仅用于时段判定
//! （深夜重定向 / 日切），也由调用方经 [`TickEnv`] 注入。
//!
//! ## tick 六步结构（`02 §5.2`）
//!
//! ```text
//! 0.   暂停判定（锁屏 / 远程桌面 / 全屏前台 / 演出中）→ 冻结 P 且不跳变
//! 0.5  需求推进（S7-M2）+ 耦合求解（S7-M3）
//! 1.   七因子固定顺序求解（S7-M4；emotion::solver::FactorSolver）
//! 2.   ΔP 累积 + 忙碌档位封顶（12 / 28 / 120；外出期间冻结）
//! 3.   自然消气计量（仅 L3 维护；S7-M5）+ 摸鱼档期计量（AC-19）
//! 4.   阶段结算（60s 升级 / 30s 回退 / 不跳级 / 自然消气 / 深夜重定向）
//! 5.   Mood 一阶低通惯性（τ 变坏 20s、变好 90s）
//! 6.   日切（自适应基线滚动 + 自然消气复位）
//! ```
//!
//! ## 敏感度（FR-11-11）与 P2-2 裁定
//!
//! `effectiveRate = 七因子乘积 × sensitivityFactor`，按 `rateClamp` 钳制（默认 0.5~1.6）。
//! **P2-2 裁定（2026-09-15）**：离线补偿（§5.5 / RV-16）与 P 封顶判定一律使用
//! **unclamped raw 速率**（[`Sensitivity::effective_rate_raw`]）；clamped 的
//! [`Sensitivity::effective_rate`] 仅用于实时 tick 累积速率缩放。
//! 单一裁决点在 [`rate_from`]（`raw < clamp.min` 时走 raw 通道）。
//!
//! ## 禁止顺手改动
//!
//! 不改七因子**顺序**（破坏确定性，`02 §9.2`）；不改 P 阶段阈值（单一真源 `emotion.json`）；
//! 不为过测而调阈值。`EmotionEvent` 是**内核算法的返回值**，不是 Tauri 事件。
//!
//! ## S7-M6 增量（敏感度 + 交互死锁三层防护）
//!
//!   - FR-11-11 敏感度：`set_sensitivity_value` 钳 `[rateClamp.min, rateClamp.max]`；
//!   - FR-11-12 层 ①：不可达 → `presenceFactor = unavailablePresenceFactor`（**不归零**）；
//!   - FR-11-12 层 ②：托盘替代入口（`coax_tray_stroke` / `coax_tray_heart` / `recall` /
//!     喂食 / 洗澡），**仍须走完整三部曲**（产品底线不动）；
//!   - FR-11-12 层 ③：不可达时 `P<10` 持续 180s + 近 2h 无负向 → **仅 L4→L3**（L5 永不）。

use chrono::{Datelike, Local};

use crate::config::model::{EmotionLevelCfg, MoodDimCfg};
use crate::needs::coupling::{CouplingOutput, CouplingSolver, CouplingSnapshot};
use crate::needs::{NeedsEnv, NeedsOutcome, NeedsSystem};
use crate::perception::time::{minute_of_day, TimeRhythm};
use crate::perception::ActivitySample;
use crate::emotion::adapt::AdaptationState;
use crate::emotion::busyness::BusynessLevel;
use crate::emotion::coax::{
    CoaxEffect, CoaxFailReason, CoaxFlow, CoaxInput, CoaxStep, COAX_MIN_LEVEL,
    COAX_SUCCESS_ACTION, COAX_SUCCESS_PRIORITY,
};
use crate::emotion::neglect::{self, NaturalCoolMeter, UnreachableMeter};
use crate::emotion::personality::Personality;
use crate::emotion::rough::RoughTracker;
use crate::emotion::solver::{FactorInputs, FactorSet, FactorSolver, InteractionPolicy, SolverCtx};
use crate::interaction::router::InteractionKind;
use crate::save::schema::EmotionSave;
use crate::state::PetState;

/// 情绪状态机展示态（`02 §4.3`，14 值冻结词表）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EmotionState {
    /// 平静待机。
    #[default]
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
    /// 想念（离线补偿 >4h 分支）。
    Longing,
    /// 困倦。
    Sleepy,
    /// 睡眠。
    Asleep,
    /// 兴奋。
    Excited,
    /// 外出中。
    Outing,
    /// 深夜困倦提示（深夜重定向，PRD B-4）。
    SleepyHint,
}

impl EmotionState {
    /// 由 `emotion.json.levels[].emotion` 字符串解析（配置驱动，S7-M5 只换判定内核）。
    ///
    /// 未知字符串回退 [`EmotionState::Idle`]（配置容错，不 panic）。
    pub fn from_cfg_name(name: &str) -> Self {
        match name {
            "Idle" => Self::Idle,
            "Happy" => Self::Happy,
            "Curious" => Self::Curious,
            "Bored" => Self::Bored,
            "Aggrieved" => Self::Aggrieved,
            "Sulking" => Self::Sulking,
            "Angry" => Self::Angry,
            "Runaway" => Self::Runaway,
            "Longing" => Self::Longing,
            "Sleepy" => Self::Sleepy,
            "Asleep" => Self::Asleep,
            "Excited" => Self::Excited,
            "Outing" => Self::Outing,
            "SleepyHint" => Self::SleepyHint,
            _ => Self::Idle,
        }
    }
}

/// 冷落等级变化的原因（`02 §4.3` `ColdReason`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ColdReason {
    /// 自然消气通道（仅 L3→L2）。
    NaturalCool,
    /// 道歉三部曲（S4-M3）。
    Coax,
    /// 离线补偿。
    Offline,
    /// 常规 P 累积。
    Accumulate,
}

/// 交互缓解类别（`emotion.json.relief`；`02 §5.7` FR-11-7，S4-M3 落地）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReliefKind {
    /// 悬停（冷却 60s）。
    Hover,
    /// 单击（冷却 3s）。
    Click,
    /// 双击（冷却 30s）。
    DoubleClick,
    /// 抚摸（无冷却，但受 `strokeMaxPerWindow` 窗口上限约束）。
    Stroke,
    /// 喂食。
    Feed,
    /// 洗澡。
    Bath,
    /// 玩耍（轨迹彩蛋）。
    Play,
}

/// 缓解冷却与抚摸窗口计数（`relief.cooldownSec` / `strokeMaxPerWindow`；S4-M3）。
#[derive(Clone, Debug, Default)]
struct ReliefTracker {
    /// 四类带冷却交互的最近一次生效时刻（下标 = [`ReliefTracker::slot`]）。
    last_ms: [Option<i64>; 4],
    /// 抚摸窗口内的生效时刻（超窗剔除）。
    stroke_window: Vec<i64>,
}

impl ReliefTracker {
    /// 类别 → 冷却槽（无冷却类别返回 `None`）。
    const fn slot(kind: ReliefKind) -> Option<usize> {
        match kind {
            ReliefKind::Hover => Some(0),
            ReliefKind::Click => Some(1),
            ReliefKind::DoubleClick => Some(2),
            ReliefKind::Stroke => Some(3),
            ReliefKind::Feed | ReliefKind::Bath | ReliefKind::Play => None,
        }
    }
}

/// 情绪内核产出的算法事件（**非** Tauri 事件；C8 登记归 S4-M2）。
#[derive(Clone, Debug, PartialEq)]
pub enum EmotionEvent {
    /// 阶段变化（不跳级：逐层各产一条）。
    ColdLevelChanged {
        /// 变化前档位。
        from: u8,
        /// 变化后档位。
        to: u8,
        /// 本层 Mood 扣减（升级为负值；**降级恒为 0**，R18-4 红线）。
        mood_delta: f32,
        /// 是否深夜重定向（改播困倦提示）。
        redirected: bool,
        /// 原因。
        reason: ColdReason,
    },
    /// 强制动作下发（阶段进入 ≥L2 或自然消气）。
    ForceAction {
        /// 动作 ID（`ACT-` 前缀，RV-12）。
        action_id: String,
        /// 优先级下限。
        priority: u8,
    },
    /// 六维展示值变化（1Hz 快照投影用；含派生 Boredom）。
    ValuesChanged {
        /// 心情。
        mood: f32,
        /// 精力。
        energy: f32,
        /// 派生展示 Boredom。
        boredom: f32,
        /// 饱食度。
        satiety: f32,
        /// 清洁度。
        cleanliness: f32,
    },
    /// 道歉三部曲进度 / 子状态（S4-M3；`02 §4.3` `CoaxProgress`）。
    CoaxProgress {
        /// 进度环比例 0..=1。
        ratio: f32,
        /// 子状态（含 L5 离家段 `Runaway` / `Away`）。
        step: CoaxStep,
    },
    /// 道歉三部曲完成（S4-M3；`02 §4.3` `CoaxSucceeded`）。
    CoaxSucceeded {
        /// 完成后的 Mood（已应用 `recoverMoodFloor` 兜底）。
        mood: f32,
    },
    /// 道歉三部曲失败（S4-M3；`02 §4.3` `CoaxFailed`）。
    CoaxFailed {
        /// 失败原因。
        reason: CoaxFailReason,
    },
    /// 关系降温事件（S7-M4；`02 §5.4` `AdaptEvent`）。
    ///
    /// 等级 1 = 连续 `coolDays` 天日均有效互动不足（关闭自然消气 + `P × coolMultiplier`）；
    /// 等级 2 = 连续 `zeroDays` 天几乎零互动（回归冷淡台词 / 停主动求助归 S7-M8）。
    RelationCooling {
        /// 降温等级（1 / 2）。
        level: u8,
    },
    /// 摸鱼专属提示（S7-M5 / **AC-19**）：前台播放视频等摸鱼应用持续 `slackLingerSec`
    /// （默认 40min）且已进入 L3+ → 触发「你明明在看屏幕…却不看我」专属台词面。
    ///
    /// **台词内容归 S7-M8**：`lines.json` 现无「摸鱼专属」池，本事件只承载**触发时机 +
    /// 建议池键**（复用 L3 `sulking` 池），由 `dp-app` 走常规气泡通道。
    SlackLinger {
        /// 已持续摸鱼的分钟数。
        minutes: u32,
        /// 建议台词池键（`levels[level].linePool`，配置驱动，代码内不写字面量）。
        pool: String,
    },
    /// 交互可达性迁移（S7-M6 / FR-11-12 可解释性）：进入 / 退出「不可达」。
    ///
    /// `available == false` = 进入不可达（穿透 / 钩子卸载 / 勿扰）；`true` = 恢复。
    /// 引导文案（「心心知道你现在点不到我，不闹你啦~」等）由 `dp-app` 依 `level` 生成，
    /// 台词内容归 S7-M8。
    InteractionReachability {
        /// 迁移后的可达性。
        available: bool,
        /// 迁移时的档位（文案分级用：`P ≥ 30` 时提示托盘入口）。
        level: u8,
    },
    /// 亲密度升级（S8-M1/M2：活动结算的亲和经验跨过 `100 × level` 门槛）。
    ///
    /// 只承载等级结果（前端提示用）；数值变化同时会带一条 [`EmotionEvent::ValuesChanged`]。
    /// **不产线上事件**（`wire_for_events` 过滤），快照的 `affinityLevel` 已反映。
    AffinityLevelUp {
        /// 升级后的亲密度等级。
        level: u32,
    },
    /// 需要落盘（阶段变化 / 数值跨界）。
    PersistNow,
}

/// 单 tick 结果：算法事件列表 + 暂停累积观察（P2-2 裁定：起止时间戳入状态）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TickOutcome {
    /// 算法事件。
    pub events: Vec<EmotionEvent>,
    /// 本 tick 是否发生了暂停态迁移（进入 / 退出）——调用方据此决定是否记日志 / 存盘。
    pub pause_changed: bool,
}

/// 冷落压力 P（`02 §4.2 neglect.rs`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NeglectPressure {
    /// 加权等效分钟 0..=cap。
    pub p: f32,
    /// 当前封顶（busyness 决定：12 / 28 / 120）。
    pub cap: f32,
    /// 已生效阶段 0..=5。
    pub level: u8,
    /// 确认期中的目标阶段。
    pub pending_level: u8,
    /// 确认期起始时刻（`None` = 无待定）。
    pub pending_since_ms: Option<i64>,
    /// 生效速率（含 sensitivity），供「情绪原因卡」。
    pub rate_per_min: f32,
}

/// 敏感度（FR-11-11 / `02 §4.2`）。
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Sensitivity {
    /// 用户档位值（0.7 / 1.0 / 1.3）。
    pub value: f32,
    /// 速率钳制区间。
    pub rate_clamp: RateClamp,
}

/// 速率钳制区间（默认 `{0.5, 1.6}`）。
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RateClamp {
    /// 下限。
    pub min: f32,
    /// 上限。
    pub max: f32,
}

impl Default for RateClamp {
    fn default() -> Self {
        Self { min: 0.5, max: 1.6 }
    }
}

impl Sensitivity {
    /// 生效速率：`raw × value` 后按 `rateClamp` 钳制（`02 §4.2` 字面口径）。
    ///
    /// ⚠️ **S4-M1 裁定（待主理人核准，登记见模块文档「已知偏离」）**：
    /// 本方法**不用于**「离场」路径。原因：`presence.factorAway = 0.05` 低于
    /// `rateClamp.min = 0.5`，直接钳制会把离场速率抬到 0.5/min（**10 倍**），
    /// 与 ① PRD「不在场时 P 增长极慢」的设计意图、② `02 §5.5` RV-16 冻结算例
    /// （`0.05 × 180min = 9 → L1`、`0.05 × 1440min = 72 < 120` 永不触 L5）**同时冲突**。
    /// 按 PRD §6.11.11 的钳制**立法本意**（「防『超黏人 1.3 × 粘人度 100 → 1.82』的
    /// 极端**上溢**组合」），钳制应作用于**乘性灵敏度增益**而非原始因子乘积；
    /// 故离场/不可达等低速率路径改走 [`Self::effective_rate_raw`]。
    #[inline]
    pub fn effective_rate(&self, raw: f32) -> f32 {
        (raw * self.value).clamp(self.rate_clamp.min, self.rate_clamp.max)
    }

    /// 未钳制的乘性生效速率：`raw × value`（仅对 `value` 做区间保护）。
    ///
    /// 用于 `raw` 本身可合法取到 `rateClamp.min` 以下的路径（离场 0.05 / 不可达 0.05），
    /// 保证 `02 §5.5` 冻结算例与 RV-16 封顶推导成立（见 [`Self::effective_rate`] 说明）。
    #[inline]
    pub fn effective_rate_raw(&self, raw: f32) -> f32 {
        let v = self.value.clamp(self.rate_clamp.min, self.rate_clamp.max);
        (raw * v).max(0.0)
    }
}

impl Default for Sensitivity {
    fn default() -> Self {
        Self { value: 1.0, rate_clamp: RateClamp::default() }
    }
}

/// 每 tick 由调用方组装的不可变环境快照（`02 §4.2`）。
///
/// 七因子的全部原始输入都在此承载：在场（`preset_idle_ms`）、忙碌（`activity`）、
/// 需求（`satiety` / `cleanliness`）、时段（`now_local`）、粗暴 / 自适应 / 性格
/// （内核内部计量器），以及 FR-11-12 的 `interaction_available`。
#[derive(Clone, Copy, Debug)]
pub struct TickEnv<'a> {
    /// 本地时间（时段判定 / 日切；由调用方经 `WallClock::now_local()` 取，C3）。
    pub now_local: chrono::DateTime<Local>,
    /// 会话暂停：锁屏 / 远程桌面 / 全屏前台（F10 / FR-1-10）。
    pub session_paused: bool,
    /// 演出类动作进行中（S4-M1 未接，保留字段供 S3-M0 驱动循环填充）。
    pub performing: bool,
    /// 外出活动中（P 冻结，`emotion.json.activity.freezeNeglectWhileOut`）。
    pub activity_running: bool,
    /// 本 tick 交互净增益（Mood 加项；S4-M1 为 0，S4-M2 起由事件总线喂入）。
    pub event_delta: f32,
    /// 用户空闲毫秒（在场判定：≥ `presence.awayThresholdSec` 判离场 / hysteresis 迟滞）。
    pub preset_idle_ms: u64,
    /// 交互可达（穿透 / 钩子卸载 / 勿扰时为 false，FR-11-12 三层防护 ①）。
    pub interaction_available: bool,
    /// 需求快照：饱食度。
    pub satiety: f32,
    /// 需求快照：清洁度。
    pub cleanliness: f32,
    /// 活动感知采样（S7-M1 载荷；`None` = 感知关闭 / 不可用 ⇒ 忙碌档退化轻度）。
    ///
    /// **隐私**：载荷只含计数类强度与进程类别哈希，本内核不接触任何内容型数据。
    pub activity: Option<ActivitySample>,
    /// 本拍是否发生负向事件（甩出 / 戳痒 / 打断）——自然消气与兜底窗口的中断输入。
    pub negative_happened: bool,
    /// 生命周期占位（保持 `TickEnv<'a>` 与 `02` 签名同构；`busyness` 原始量已由
    /// `activity` 承载，本字段仅为**签名兼容**而保留，不参与任何判定）。
    pub _marker: core::marker::PhantomData<&'a ()>,
}

impl<'a> Default for TickEnv<'a> {
    fn default() -> Self {
        Self {
            now_local: Local::now(),
            session_paused: false,
            performing: false,
            activity_running: false,
            event_delta: 0.0,
            preset_idle_ms: 0,
            interaction_available: true,
            satiety: 70.0,
            cleanliness: 85.0,
            activity: None,
            negative_happened: false,
            _marker: core::marker::PhantomData,
        }
    }
}

/// 生成生效速率：`raw` 低于 `rateClamp.min` 时走未钳制通道（离场 / 不可达路径）。
///
/// **单一裁决点**：全模块只从这里取速率，避免两条路径各写一遍钳制口径。
/// 理由见 [`Sensitivity::effective_rate`] 的裁定说明。
#[inline]
fn rate_from(sensitivity: &Sensitivity, raw: f32) -> f32 {
    if raw < sensitivity.rate_clamp.min {
        sensitivity.effective_rate_raw(raw)
    } else {
        sensitivity.effective_rate(raw)
    }
}

/// 情绪内核（`02 §4.1` `EmotionEngine`）。
///
/// 持有：配置（借用）、六维数值、P 与阶段、七因子求解器与四组计量器（性格 / 粗暴 /
/// 自适应 / 两条保持窗口）、需求系统、耦合求解器、道歉三部曲。
/// **不持有**：台词 / 气泡（归 S4-M5 + S7-M8）与事件总线（归 S4-M2）。
pub struct EmotionEngine<'c> {
    cfg: &'c crate::config::model::EmotionConfig,
    needs_cfg: &'c crate::config::model::NeedsConfig,
    /// 六维数值 + 暂停窗口 + 情绪展示态。
    pub state: PetState,
    /// 冷落压力与阶段。
    pub neglect: NeglectPressure,
    /// 敏感度。
    pub sensitivity: Sensitivity,
    /// 本拍七因子快照（S7-M4；供快照投影 / 原因卡）。
    factors: FactorSet,
    /// 七因子求解器（S7-M4：在场迟滞 + 忙碌平滑 + 节律预热）。
    solver: FactorSolver,
    /// 五维隐藏性格（S7-M4；首建随机 + 存档持久化）。
    personality: Personality,
    /// 性格是否已随过（首次创建随机完成标记；存档段 C `personalityRolled`）。
    personality_rolled: bool,
    /// 粗暴对待计量（S7-M4；存档段 C `emotion.rough`）。
    rough: RoughTracker,
    /// 自适应基线（S7-M4；存档段 C `emotion.adapt`）。
    adapt: AdaptationState,
    /// 自然消气计量（S7-M5；仅 L3→L2）。
    natural_cool: NaturalCoolMeter,
    /// 交互不可达时的 L4→L3 兜底计量（S7-M6；L5 永不开放）。
    unreachable: UnreachableMeter,
    /// 交互可达性策略（S7-M6；`settings.json.interaction` 的配置投影）。
    interaction: InteractionPolicy,
    /// 上一拍交互可达性（迁移检测 → `InteractionReachability` 事件；`None` = 尚未判定）。
    last_interaction_available: Option<bool>,
    /// 摸鱼档起始时刻（AC-19；绝对锚定，`None` = 未在摸鱼）。
    slack_since_ms: Option<i64>,
    /// 摸鱼专属台词本**档期内**是否已触发（避免每拍重复播报）。
    slack_hint_fired: bool,
    /// 上次 tick 的墙钟毫秒（`None` = 尚未 tick）。
    last_tick_ms: Option<i64>,
    /// 上次 tick 注入的本地时间（**不读时钟**，仅缓存上游经 `WallClock` 取到的值；
    /// 供深夜重定向判定使用，C3）。
    last_now_local: chrono::DateTime<Local>,
    /// 离线补偿期间是否处于「想念」展示（供快照）。
    longing: bool,
    /// 今日净正向交互数（自然消气条件②的计量）。
    positive_interactions: u32,
    /// 最近一次正向交互时刻（自适应基线间隔采样用；`None` = 尚无前驱）。
    last_interaction_ms: Option<i64>,
    /// 道歉三部曲状态机（S4-M3 / S4-M4）。
    coax: CoaxFlow,
    /// 交互缓解冷却与抚摸窗口（S4-M3）。
    relief: ReliefTracker,
    /// 需求系统（S7-M2：自然变化 / 事件扣减 / 冷却记账；六维数值真源仍是 `state.values`）。
    needs: NeedsSystem,
    /// 一日节律表（S7-M1：6 段时段 + 用餐窗口；配置驱动，零分配）。
    rhythm: TimeRhythm,
    /// 耦合矩阵求解器（S7-M3；`None` = 构建期环检测未过 → 全中性输出，不崩）。
    coupling: Option<CouplingSolver<'c>>,
    /// 本拍耦合输出（S3 阶段消费面：Mood 衰减 / 速度 / 收益 / 派遣门禁）。
    coupling_out: CouplingOutput<'c>,
    /// 本拍需求推进产出（跨档判定 → `pet://needs`，由 `dp-app` 读后 emit）；
    /// `None` = 尚未推进过（首个业务档之前）。
    last_needs: Option<NeedsOutcome>,
}

/// 香味 Buff 期间的 Mood 衰减倍率（`01 §6.12.3`：洗护用品免冷却 + 12min 内 ×0.8）。
///
/// 口径说明（C7 登记）：`needs.json` 当前**无**该键（文档只给固定倍数），故以本常量
/// 承载；若后续配置化，应同时改为从 `needs.json.bath` 读取（届时删除本常量）。
const SENT_BUFF_MOOD_DECAY_MUL: f32 = 0.8;

/// 构建耦合求解器（S7-M3）。
///
/// 构建期环检测失败 → `None` + 告警（降级不崩，`02 §7.4.2`）：此后
/// [`EmotionEngine::coupling`] 恒为全中性输出（所有乘子 1.0、无拒绝），
/// 即「矩阵不可用时退回无耦合」，绝不 panic。
/// 正常路径下 `ConfigService::load_all` 已在装载期拒绝成环配置，本函数是第二道保险。
fn build_coupling<'c>(
    needs_cfg: &'c crate::config::model::NeedsConfig,
) -> Option<CouplingSolver<'c>> {
    match CouplingSolver::build(&needs_cfg.coupling) {
        Ok(solver) => Some(solver),
        Err(err) => {
            tracing::warn!("耦合矩阵构建失败，退化为无耦合：{err}");
            None
        }
    }
}

impl<'c> EmotionEngine<'c> {
    /// 以配置构造（C7：数值全部来自 `emotion.json` / `needs.json`）。
    pub fn new(
        cfg: &'c crate::config::model::EmotionConfig,
        needs_cfg: &'c crate::config::model::NeedsConfig,
    ) -> Self {
        let p_cap = cfg.busyness.cap_free as f32;
        Self {
            cfg,
            needs_cfg,
            state: PetState::from_cfg(cfg, needs_cfg),
            neglect: NeglectPressure { p: 0.0, cap: p_cap, level: 0, pending_level: 0, pending_since_ms: None, rate_per_min: 0.0 },
            sensitivity: Sensitivity {
                value: cfg.sensitivity.value,
                rate_clamp: RateClamp { min: cfg.sensitivity.rate_clamp.min, max: cfg.sensitivity.rate_clamp.max },
            },
            factors: FactorSet::default(),
            solver: FactorSolver::new(&cfg.busyness),
            personality: Personality::from_cfg(&cfg.personality),
            personality_rolled: false,
            rough: RoughTracker::default(),
            adapt: AdaptationState::default(),
            natural_cool: NaturalCoolMeter::new(),
            unreachable: UnreachableMeter::new(),
            interaction: InteractionPolicy::default(),
            last_interaction_available: None,
            slack_since_ms: None,
            slack_hint_fired: false,
            last_tick_ms: None,
            last_now_local: chrono::Local::now(),
            longing: false,
            positive_interactions: 0,
            last_interaction_ms: None,
            coax: CoaxFlow::new(),
            relief: ReliefTracker::default(),
            needs: NeedsSystem::new(),
            rhythm: TimeRhythm::from_cfg(&cfg.rhythm),
            coupling: build_coupling(needs_cfg),
            coupling_out: CouplingOutput::default(),
            last_needs: None,
        }
    }

    /// 由存档恢复（**S5-M2「补偿接入」**，`02 §5 K-7` 段 A + §6.1 启动时序）。
    ///
    /// 与 [`Self::new`] 的差别：整体接管 `state` / `neglect` / `sensitivity` / `last_tick_ms`，
    /// 并按 `emotion.*` 段整体恢复 S7-M4 的四组计量器（性格 / 粗暴 / 自适应 / 展示态）。
    ///
    /// **调用时机是硬约束**：必须在**首个 [`Self::tick_1s`] 之前**调用（`dp-app` 在
    /// `build_state` 内、core-loop 线程启动前完成）。原因：`tick_1s` 在
    /// `last_tick_ms == None` 时会「以当下为起点并直接返回」（首拍不计 dt），若先跑首拍，
    /// 离线时长会被压缩成 1 秒，[`Self::offline_compensate`] 随后只补 0 步 —— 补偿静默失效。
    ///
    /// `sensitivity` 由存档单独传入（FR-11-11 用户档位），`longing` 展示态按
    /// `state.emotion == Longing` 回填，使「关机 >4h 后重启」直接呈现想念而不必等下一拍。
    pub fn restore(
        cfg: &'c crate::config::model::EmotionConfig,
        needs_cfg: &'c crate::config::model::NeedsConfig,
        state: PetState,
        neglect: NeglectPressure,
        sensitivity: Sensitivity,
        last_tick_ms: i64,
    ) -> Self {
        let mut engine = Self::new(cfg, needs_cfg);
        engine.state = state;
        engine.state.last_tick_ms = last_tick_ms;
        engine.neglect = neglect;
        engine.sensitivity = sensitivity;
        engine.last_tick_ms = Some(last_tick_ms);
        engine.longing = engine.state.emotion == EmotionState::Longing;
        engine
    }

    /// 由存档 `emotion.*` 段恢复 S7-M4 的四组计量器（**追加式 API**，不破坏既有 `restore`）。
    ///
    /// 与 [`Self::restore`] 的分工：本方法只管**后加的因子侧状态**（性格 / 粗暴 / 自适应 /
    /// 展示态），六维数值与 P / 敏感度仍由 `restore` 接管。调用时机同样是**首个 tick 之前**。
    ///
    /// `personalityRolled == false`（全新档 / 老档升级）时**不**立即随机：留给首个
    /// [`Self::tick_1s`] 用注入的 `now_ms` 作种子，保证「首建随机只发生一次」且可复现。
    pub fn restore_factors(&mut self, save: &EmotionSave) {
        let mut p = Personality {
            clingy: save.personality.clingy,
            curiosity: save.personality.curiosity,
            temper: save.personality.temper,
            courage: save.personality.courage,
            diligence: save.personality.diligence,
        };
        p.clamp();
        self.personality = p;
        self.personality_rolled = save.personality_rolled;
        self.rough = RoughTracker::from_save(&save.rough, &self.cfg.rough);
        self.adapt = AdaptationState::from_save(&save.adapt);
    }

    /// 只读：配置引用。
    #[inline]
    pub fn cfg(&self) -> &'c crate::config::model::EmotionConfig {
        self.cfg
    }

    /// 只读：需求配置引用。
    #[inline]
    pub fn needs_cfg(&self) -> &'c crate::config::model::NeedsConfig {
        self.needs_cfg
    }

    /// 只读：需求系统（S7-M2；冷却时间戳与分档缓存）。
    #[inline]
    pub const fn needs(&self) -> &NeedsSystem {
        &self.needs
    }

    /// 可写：需求系统（供上层在道具 / 事件后回写冷却时间戳）。
    #[inline]
    pub fn needs_mut(&mut self) -> &mut NeedsSystem {
        &mut self.needs
    }

    /// 只读：一日节律表（S7-M1；时段 + 用餐窗口）。
    #[inline]
    pub const fn rhythm(&self) -> &TimeRhythm {
        &self.rhythm
    }

    /// 只读：本拍耦合输出（S7-M3；S3 阶段消费面）。
    #[inline]
    pub const fn coupling(&self) -> &CouplingOutput<'c> {
        &self.coupling_out
    }

    /// 只读：本拍需求推进产出（`None` = 尚未推进；`band_changed` = 需发 `pet://needs`）。
    #[inline]
    pub const fn last_needs(&self) -> Option<NeedsOutcome> {
        self.last_needs
    }

    /// 移动 / 动作速度乘子（S7-M3 C-12：`Energy < 20` → ×0.8）。
    ///
    /// 与 [`Self::needs`] 的分档 `speed_mul`（`satiety < 20` → ×0.85）**相乘**后使用；
    /// 消费点 = `MotionEngine::set_speed_mul`（core-loop 每 logic tick 注入）。
    #[inline]
    pub fn speed_mul(&self) -> f32 {
        self.coupling_out.speed_mul
    }

    /// 派遣门禁（S7-M3 / AC-30 / AC-31）：**矩阵级**拒绝判定（`true` = 允许）。
    ///
    /// 调用方应与 [`crate::needs::bands::BandEffects::denies_dispatch`]（分档级拒绝）
    /// 取**并集**（两处配置语义一致，见 `01 §6.12.4` 与 §6.12.2/3 对照）。
    #[must_use]
    pub fn dispatch_verdict(&self, kind: crate::needs::DispatchKind) -> bool {
        match &self.coupling {
            Some(solver) => solver.dispatch_verdict(kind, &self.coupling_out).is_allowed(),
            None => true,
        }
    }

    /// 只读：是否处于想念展示。
    #[inline]
    pub fn is_longing(&self) -> bool {
        self.longing
    }

    /// 只读：今日净正向交互数（自然消气计量，消费归 S4-M2/S4-M3）。
    #[inline]
    pub fn positive_interactions(&self) -> u32 {
        self.positive_interactions
    }

    /// 只读：自然消气保持窗口已持续毫秒（`None` = 未在计时；诊断 / 单测）。
    #[inline]
    #[must_use]
    pub fn natural_cool_hold_ms(&self, now_ms: i64) -> Option<i64> {
        self.natural_cool.hold_elapsed_ms(now_ms)
    }

    /// 只读：自然消气窗口内正向交互数（诊断 / 单测）。
    #[inline]
    #[must_use]
    pub fn natural_cool_positives(&self) -> usize {
        self.natural_cool.positive_count()
    }

    /// 只读：阶段定义（按 [`NeglectPressure::level`] 取，越界回退 L0）。
    pub fn current_level_cfg(&self) -> &EmotionLevelCfg {
        let idx = (self.neglect.level as usize).min(self.cfg.levels.len().saturating_sub(1));
        &self.cfg.levels[idx]
    }

    /// Boredom 派生展示量（`min(100, P×100/l5)`；RV-02 冻结）。
    #[inline]
    pub fn boredom_display(&self) -> f32 {
        crate::state::PetValues::boredom_display(self.neglect.p, self.cfg.thresholds.l5 as f32)
    }

    /// 只读：本拍七因子快照（S7-M4）。
    #[inline]
    pub const fn factors(&self) -> FactorSet {
        self.factors
    }

    /// 只读：五维隐藏性格（S7-M4）。
    #[inline]
    pub const fn personality(&self) -> Personality {
        self.personality
    }

    /// 只读：性格是否已随机（首建随机完成标记）。
    #[inline]
    pub const fn personality_rolled(&self) -> bool {
        self.personality_rolled
    }

    /// 设置五维性格（S7-M4：首建随机的显式覆盖口；S10 设置页重掷入口）。
    ///
    /// 调用即视为「已随过」（`personality_rolled = true`），避免紧随其后的首个 tick
    /// 再把值覆盖掉；非有限维按 `0.5` 兜底并整体钳 `[0,1]`（见 `Personality::clamp`）。
    pub fn set_personality(&mut self, personality: Personality) {
        let mut p = personality;
        p.clamp();
        self.personality = p;
        self.personality_rolled = true;
    }

    /// 只读：粗暴对待计量（S7-M4）。
    #[inline]
    pub const fn rough(&self) -> RoughTracker {
        self.rough
    }

    /// 只读：自适应基线（S7-M4）。
    #[inline]
    pub const fn adapt(&self) -> &AdaptationState {
        &self.adapt
    }

    /// 只读：交互可达性策略（S7-M6）。
    #[inline]
    pub const fn interaction_policy(&self) -> InteractionPolicy {
        self.interaction
    }

    /// 热更新交互可达性策略（S7-M6；`dp-app` 从 `settings.json.interaction` 注入）。
    ///
    /// 仅接管**数值口径**（不可达在场因子 / 层 ③ 阈值与兜底等级）；`available` 由每拍
    /// [`TickEnv::interaction_available`] 如实喂入，避免两处真源。
    pub fn set_interaction_policy(&mut self, policy: InteractionPolicy) {
        self.interaction = InteractionPolicy { available: self.interaction.available, ..policy };
    }

    /// 只读：关系降温乘子（`01 §6.11.7`；非降温期 1.0）。
    #[inline]
    pub const fn cool_multiplier(&self) -> f32 {
        self.factors.cool_mul
    }

    /// 只读：错过 / 未错过——本拍是否处于「开机宽限」窗口（诊断用）。
    #[inline]
    pub const fn in_warmup(&self) -> bool {
        self.factors.warmup
    }

    /// 只读：忙碌档位（S7-M4；诊断日志只记档位，`02 §5.6` ⑥）。
    #[inline]
    pub const fn busyness_level(&self) -> BusynessLevel {
        self.factors.busyness_level
    }

    /// 只读：展示态（`02 §4.3`；`pet://state` 投影用）。
    #[inline]
    pub fn emotion_state(&self) -> EmotionState {
        self.state.emotion
    }

    /// 七因子投影（`02 §4.3` `neglect.factors`，S4-M2 起供 `pet://state`）。
    ///
    /// **S7-M4 口径**：七槽全部为**真实因子**（固定顺序见
    /// [`crate::emotion::solver::FACTOR_ORDER`]），`product` 与七槽乘积**严格自洽**
    /// ——前端由 `product == presence × busyness × … × adapt` 反算时无需特殊分支。
    ///
    /// 两点刻意的不一致（均为既有契约，不是缺陷）：
    ///   - **敏感度不进七槽**（FR-11-11：它是独立乘子 `effectiveRate = 乘积 × 敏感度`）；
    ///   - **关系降温乘子不进 `product`**（`02 §9.2` 不变量 ⑥ 要求 `adapt ∈ [0.7,1.3]`，
    ///     且 `product` 必须与七槽自洽 ⇒ 降温乘子单列 [`Self::cool_multiplier`]）。
    #[must_use]
    pub fn factors_snapshot(&self) -> crate::event::FactorsSnapshot {
        crate::event::FactorsSnapshot {
            presence: self.factors.presence,
            busyness: self.factors.busyness,
            personality: self.factors.personality,
            rhythm: self.factors.rhythm,
            needs: self.factors.needs,
            rough: self.factors.rough,
            adapt: self.factors.adapt,
            product: self.factors.product,
        }
    }

    /// 1Hz 快照投影（`02 §4.3` `PetSnapshotV2`）。
    ///
    /// 时间无关（C3）；`personality_text` / `reroll_left` 由调用方注入（S7 性格面板，
    /// 本卡默认空 / 0）。实装在 [`crate::event::project_snapshot`]（投影归 S4-M2 的
    /// `event` 模块，内核只暴露数据）。
    #[must_use]
    pub fn snapshot(&self) -> crate::event::PetSnapshotV2 {
        crate::event::project_snapshot(self, "", 0)
    }

    /// 阶段阈值比较值（`02 §5.3`：`P_eff = P / threshold_scale`）。
    ///
    /// `threshold_scale = 1.2 − 0.4 × temper`，`temper=0.5` 时精确等于 1.0，
    /// 保证典型场景对齐 5/15/30/60/120（S7-M4 起由 [`Personality`] 提供）。
    #[inline]
    fn threshold_scale(&self) -> f32 {
        self.personality.threshold_scale(&self.cfg.personality)
    }

    /// Mood 扣减幅度缩放（`02 §5.3`：`mood_delta_scale = 0.8 + 0.4 × temper`）。
    #[inline]
    fn mood_delta_scale(&self) -> f32 {
        self.personality.mood_delta_scale(&self.cfg.personality)
    }

    /// 阶段判定：`P_eff` → 目标档位（`02 §5.3` 冻结阈值表；实装在 `neglect`）。
    #[inline]
    fn level_for(&self, p_eff: f32) -> u8 {
        neglect::level_for(p_eff, &self.cfg.thresholds)
    }

    /// 首建性格随机（S7-M4）：`personalityRolled == false` 时以注入 `now_ms` 为种子。
    ///
    /// 只随机**粘人度**（`clingyInitMin~Max` = 45~55），保证出厂 `personalityFactor ∈
    /// [0.96, 1.04]`，与 AC-16 的 ±10% 容差自洽；重掷（放开全区间）归 S10 设置页。
    fn ensure_personality_rolled(&mut self, now_ms: i64) {
        if self.personality_rolled {
            return;
        }
        self.personality = Personality::roll_initial(now_ms.max(0) as u64, &self.cfg.personality);
        self.personality_rolled = true;
    }

    /// 1Hz 业务 tick（`02 §5.2` 六步结构的 S4-M1 实装）。
    ///
    /// 返回值：[`TickOutcome`]（算法事件 + 暂停迁移标志）。
    pub fn tick_1s(&mut self, now_ms: i64, env: TickEnv<'_>) -> TickOutcome {
        // 暂停观察（P2-2：起止时间戳入状态；幂等）
        let pause_changed = self.state.pause.observe(env.session_paused, now_ms);

        // 0.1) 首建性格随机（S7-M4）。**必须在首拍早退之前**：首拍只建基线不求解因子，
        //      若放在其后，「全新安装的首拍」不会随机，性格会拖到第 2 拍才定。
        //      种子取注入的 `now_ms`（C3 不读时钟），且「只发生一次」（标记随存档持久化）。
        self.ensure_personality_rolled(now_ms);

        let Some(last) = self.last_tick_ms else {
            // 首拍：建立基线，不产生任何累积（避免把启动前的空闲算成冷落）。
            self.last_tick_ms = Some(now_ms);
            self.last_now_local = env.now_local;
            self.state.last_tick_ms = now_ms;
            return TickOutcome { events: vec![], pause_changed };
        };
        let dt_ms = (now_ms - last).max(0);
        if dt_ms == 0 {
            return TickOutcome { events: vec![], pause_changed };
        }
        self.last_tick_ms = Some(now_ms);
        self.state.last_tick_ms = now_ms;
        self.last_now_local = env.now_local;
        let dt_min = dt_ms as f32 / 60_000.0;

        // ── 0) 暂停：锁屏 / 远程桌面 / 全屏前台 / 演出类动作 ────────────────
        // 清掉确认期计时（`02 §5.2`）后直接返回：**P 不累积、阶段不跳变**，
        // 这正是验收「锁定屏幕 30min → 累积暂停，解锁后不跳变」的实现口径。
        if env.session_paused || env.performing {
            self.neglect.rate_per_min = 0.0;
            self.neglect.pending_since_ms = None;
            self.neglect.pending_level = self.neglect.level;
            // 暂停期间不维护预热窗口（宽限语义只对「刚回到电脑前」成立）。
            self.solver.clear_warmup();
            return TickOutcome { events: vec![], pause_changed };
        }

        // ── 0.5) 需求推进（S7-M2：六维自然变化 + 分档）+ 耦合求解（S7-M3）──────
        // 顺序说明：需求先推进（S2 阶段属性变化），再取**耦合快照**做 S1 求耦合，
        // 本拍后续的 Mood 衰减（S3 阶段）即用本拍矩阵结果——与 `02 §5.10`
        // 「S0 快照 → S1 求耦合 → S2 属性变化 → S3 Mood/Affinity」同序。
        // 用餐窗口取自 `emotion.json.rhythm.mealWindows`（07-09 / 11-13 / 17-19）。
        let minute = minute_of_day(&env.now_local);
        let needs_env = NeedsEnv::new(
            now_ms,
            self.rhythm.is_meal_window(minute),
            env.activity_running,
            matches!(self.state.emotion, EmotionState::Sleepy),
        );
        self.last_needs =
            Some(self.needs.tick(&mut self.state.values, self.needs_cfg, dt_min, &needs_env));
        let snapshot = CouplingSnapshot::from(&self.state.values);
        self.coupling_out = match self.coupling.as_mut() {
            Some(solver) => solver.evaluate(&snapshot, now_ms),
            None => CouplingOutput::default(),
        };

        // ── 1) 七因子固定顺序求解（S7-M4，`02 §5.2`；`solver` 内各步只读本拍快照）──
        let mut policy = self.interaction;
        // FR-11-12 层 ①：可达性**每拍如实喂入**（single source = TickEnv）。
        policy.available = env.interaction_available;
        let inputs = FactorInputs {
            idle_ms: env.preset_idle_ms,
            activity: env.activity.as_ref(),
            satiety: self.state.values.satiety,
            cleanliness: self.state.values.cleanliness,
            personality: &self.personality,
            rough: &self.rough,
            adapt: &self.adapt,
        };
        let ctx = SolverCtx {
            presence_cfg: &self.cfg.presence,
            busyness_cfg: &self.cfg.busyness,
            needs_cfg: &self.cfg.needs,
            personality_cfg: &self.cfg.personality,
            rhythm_cfg: &self.cfg.rhythm,
            rhythm_table: &self.rhythm,
            rough_cfg: &self.cfg.rough,
            adapt_cfg: &self.cfg.adapt,
            interaction: policy,
        };
        self.factors = self.solver.solve(now_ms, &env.now_local, &inputs, &ctx);
        // 速率 = 七因子乘积 × 降温乘子 × 敏感度（FR-11-11；钳制见 `rate_from`）。
        let raw = self.factors.product * self.factors.cool_mul;
        self.neglect.rate_per_min = rate_from(&self.sensitivity, raw);

        // ── 2) ΔP 累积 + 忙碌档位封顶（12 / 28 / 120；外出期间冻结）──────────
        if !env.activity_running {
            let rate = self.neglect.rate_per_min;
            neglect::accrue(&mut self.neglect, dt_min, rate, self.factors.busyness_cap);
        }

        // ── 3) 可达性迁移（S7-M6 可解释性：进入 / 退出不可达各报一次）──────────
        let mut events: Vec<EmotionEvent> = Vec::new();
        if self.last_interaction_available != Some(env.interaction_available) {
            self.last_interaction_available = Some(env.interaction_available);
            // 恢复可交互：清掉层 ③ 兜底的保持窗口（条件已不适用）。
            if env.interaction_available {
                self.unreachable.reset();
            }
            events.push(EmotionEvent::InteractionReachability {
                available: env.interaction_available,
                level: self.neglect.level,
            });
        }

        // ── 3.5) 摸鱼专属提示（AC-19）：摸鱼档持续 ≥`slackLingerSec` 且已 L3+ ──
        events.extend(self.slack_linger_tick(now_ms));

        // ── 4) 阶段结算（60s 升级 / 30s 回退 / 不跳级 / 自然消气 / 深夜重定向）──
        events.extend(self.settle_level(now_ms, env.negative_happened));

        // ── 4.5) 三部曲阶段同步（S4-M3/S4-M4：L5 离家演出触发 / 档位回落清空）──
        // 必须在 `settle_level` 之后：本 tick 刚进入 L5 时要立刻开始离家演出计时。
        let level_now = self.neglect.level;
        let coax_effects = self.coax.sync_level(level_now, now_ms);
        events.extend(self.apply_coax_effects(coax_effects));

        // ── 5) Mood 一阶低通惯性 ────────────────────────────────────────────
        events.extend(self.step_mood(dt_min, env.event_delta, now_ms));

        // ── 6) 日切（自适应滚动 / 自然消气计量与正向计数的重置）──────────────
        events.extend(self.daily_roll(&env));

        let events = dedup_events(events);
        if !events.is_empty() {
            let mut e = events;
            e.push(EmotionEvent::PersistNow);
            return TickOutcome { events: e, pause_changed };
        }
        TickOutcome { events, pause_changed }
    }

    /// 摸鱼专属提示（**AC-19**）：摸鱼档持续 ≥`busyness.slackLingerSec`（默认 40min）
    /// 且档位已达 L3+ → 每档期触发**一次** [`EmotionEvent::SlackLinger`]。
    ///
    /// 池键取当前档位的 `levels[].linePool`（配置驱动，代码内不写字面量）；
    /// 台词内容归 S7-M8（`lines.json` 现无「摸鱼专属」池）。
    fn slack_linger_tick(&mut self, now_ms: i64) -> Vec<EmotionEvent> {
        if self.factors.busyness_level != BusynessLevel::Slack {
            self.slack_since_ms = None;
            self.slack_hint_fired = false;
            return vec![];
        }
        let since = *self.slack_since_ms.get_or_insert(now_ms);
        if self.slack_hint_fired || self.neglect.level < 3 {
            return vec![];
        }
        let need_ms = (self.cfg.busyness.slack_linger_sec as i64).saturating_mul(1000);
        if now_ms - since < need_ms {
            return vec![];
        }
        self.slack_hint_fired = true;
        let minutes = u32::try_from((now_ms - since) / 60_000).unwrap_or(u32::MAX);
        vec![EmotionEvent::SlackLinger {
            minutes,
            pool: self.current_level_cfg().line_pool.clone(),
        }]
    }

    /// 阶段结算（`02 §5.3` 冻结口径）：确认期 + 不跳级 + 逐层扣 Mood +
    /// 自然消气通道（仅 L3→L2，S7-M5）+ 不可达兜底（仅 L4→L3，S7-M6）。
    ///
    /// ## 地板口径（三条并存，互不覆盖）
    ///
    ///   1. **正常可交互**（`confirm.naturalFloorLevel`，默认 3）：L3 及以上不自然回退，
    ///      L4/L5 必须走 [`CoaxFlow`]（S4-M4 已锁）；
    ///   2. **自然消气**（S7-M5）：`level == 3` 且四条件全满足 → 允许降到 L2；
    ///   3. **不可达兜底**（S7-M6，层 ③）：`interaction_available == false` 且
    ///      `P<10` 持续 `unreachableHoldSec` + 近 `unreachableNoNegativeSec` 无负向
    ///      → 允许 **L4→L3**（`unreachableNaturalFloorLevel = 4`；**L5 永不**）。
    fn settle_level(&mut self, now_ms: i64, negative_happened: bool) -> Vec<EmotionEvent> {
        let p_eff = self.neglect.p / self.threshold_scale();
        let mut target = self.level_for(p_eff);
        let cooling = self.adapt.is_cooling(&self.cfg.adapt);

        // 自然消气计量（仅 L3 时维护，`02 §5.2` 第 3 步）。
        if self.neglect.level == 3 {
            self.natural_cool
                .observe(now_ms, self.neglect.p, &self.cfg.confirm, negative_happened);
        } else {
            self.natural_cool.reset();
        }
        // 不可达兜底计量（仅不可达时维护）。
        let fallback_open = self.interaction.unreachable_fallback_enabled()
            && !self.factors.interaction_available;
        if fallback_open {
            let below = self.neglect.p.is_finite()
                && self.neglect.p < self.unreachable.p_threshold;
            self.unreachable.observe(now_ms, below, negative_happened);
        } else {
            self.unreachable.reset();
        }

        // 逐层结算前的**原始目标**（未经任何地板修正）——②③ 两条通道的放行条件都要
        // 依据它判断「P 是否真的已落到目标档之下」，不能用修正后的 `target`（会被地板
        // 抬回当前档，导致条件恒假）。
        let raw_target = target;

        // ① **L3+ 强制地板**（`01 §6.5.2` / §6.11.8.1）：L3 生闷气 / L4 生气 / L5 离家
        //    出走**都不开放「无条件自然回退」**——L3 只能经自然消气通道降到 L2，
        //    L4/L5 必须走 CoaxFlow（L4 另开一层不可达兜底）。L1/L2 不受本地板约束，
        //    由 P 自然下降回退（`01 §6.11.8` 状态回退 ①）。
        if target < self.neglect.level && self.neglect.level >= COAX_MIN_LEVEL {
            target = self.neglect.level;
        }
        // ② **自然消气通道**（S7-M5 / `01 §6.11.8.1`）：**仅 L3→L2**；
        //    `naturalFloorLevel` 设 4/5 即完全关闭该通道（与 PRD 的开关语义一致）。
        let natural_floor = self.cfg.confirm.natural_floor_level;
        let natural_cool = self.neglect.level == 3
            && raw_target < 3
            && natural_floor <= 3
            && self
                .natural_cool
                .satisfied(now_ms, self.neglect.p, cooling, &self.cfg.confirm);
        if natural_cool {
            target = 2;
        }
        // ③ **不可达兜底**（S7-M6 / `02 §5.23` 第 3 层）：**仅 L4→L3**；
        //    L5 任何情况不开放（`level == 4` 的硬条件即产品红线）。
        let unreachable_fallback = !self.factors.interaction_available
            && self.neglect.level == 4
            && raw_target < 4
            && fallback_open
            && self.unreachable.satisfied(
                now_ms,
                self.interaction.unreachable_hold_sec,
                self.interaction.unreachable_no_negative_sec,
            );
        if unreachable_fallback {
            target = 3;
        }

        if target == self.neglect.level {
            self.neglect.pending_since_ms = None;
            self.neglect.pending_level = self.neglect.level;
            return vec![];
        }

        let need_ms = neglect::confirm_ms(self.neglect.level, target, &self.cfg.confirm);

        // 目标变更 → 重新起计（幂等：同一目标保持同一起点）
        if self.neglect.pending_level != target {
            self.neglect.pending_level = target;
            self.neglect.pending_since_ms = Some(now_ms);
            return vec![];
        }
        let Some(since) = self.neglect.pending_since_ms else {
            self.neglect.pending_since_ms = Some(now_ms);
            return vec![];
        };
        if now_ms - since < need_ms {
            return vec![];
        }
        self.neglect.pending_since_ms = None;

        let mut out = Vec::new();
        let scale = self.mood_delta_scale();
        // 逐层结算，绝不跳级
        while self.neglect.level != target {
            let up = target > self.neglect.level;
            let next = if up { self.neglect.level + 1 } else { self.neglect.level - 1 };
            let Some(def) = self.cfg.levels.get(next as usize).cloned() else {
                // 配置档位缺失：保守停在当前档，不 panic
                self.neglect.pending_level = self.neglect.level;
                break;
            };
            let mut delta = 0.0f32;
            if up {
                // 仅升级扣减；降级不扣不退还（R18-4：降级路径不得产生负向扣减）
                delta = def.mood_delta as f32 * scale;
                if delta > 0.0 {
                    delta = 0.0;
                }
                self.state.values.mood = (self.state.values.mood + delta).clamp(0.0, 100.0);
            }
            // 深夜重定向（B-4 / AC-20）：深夜不升级为生气，改播困倦催睡。
            let redirected = up
                && def.level >= self.cfg.rhythm.night_redirect_from_level
                && self.factors.night;
            if def.mood_lock_max > 0 {
                self.state.values.mood = self.state.values.mood.min(def.mood_lock_max as f32);
            }
            self.state.emotion = if redirected {
                EmotionState::SleepyHint
            } else {
                EmotionState::from_cfg_name(&def.emotion)
            };
            let reason = if natural_cool {
                ColdReason::NaturalCool
            } else {
                ColdReason::Accumulate
            };
            out.push(EmotionEvent::ColdLevelChanged {
                from: self.neglect.level,
                to: next,
                mood_delta: delta,
                redirected,
                reason,
            });
            if up && def.level >= 2 {
                if redirected {
                    // AC-20：改用配置的深夜重定向动作（默认 ACT-I-02 打哈欠）。
                    let action = self.cfg.rhythm.night_redirect_action_id.clone();
                    if !action.is_empty() {
                        out.push(EmotionEvent::ForceAction {
                            action_id: action,
                            priority: self.cfg.rhythm.night_redirect_priority.min(255) as u8,
                        });
                    }
                } else if let Some(first) = def.idle_pool.first() {
                    out.push(EmotionEvent::ForceAction {
                        action_id: first.clone(),
                        priority: def.priority_floor.min(255) as u8,
                    });
                }
            }
            self.neglect.level = next;
        }
        self.neglect.pending_level = self.neglect.level;

        // 自然消气结算（`01 §6.11.8.1`）：Mood 抬到 `naturalMoodFloor`（45）+ 配置的
        // 自然消气动作（默认 `ACT-T-04`），**不播 `ACT-E-06`**（与 CoaxFlow 的差异是
        // 产品底线：三部曲仍是最优解——更快、Mood 更高、有进度环与仪式感）。
        if natural_cool {
            let floor_mood = self.cfg.confirm.natural_mood_floor as f32;
            self.state.values.mood = self.state.values.mood.max(floor_mood);
            self.natural_cool.reset();
            if !self.cfg.confirm.natural_cool_action_id.is_empty() {
                out.push(EmotionEvent::ForceAction {
                    action_id: self.cfg.confirm.natural_cool_action_id.clone(),
                    priority: self.cfg.confirm.natural_cool_priority.min(255) as u8,
                });
            }
        }
        // 不可达兜底结算：仅当降级目标为 L3（L4→L3），复位计量避免重复触发。
        if unreachable_fallback {
            self.unreachable.reset();
        }
        out
    }

    /// Mood 一阶低通惯性（`02 §5 K-5` `step_mood`；S7-M5）。
    ///
    /// ```text
    /// MoodTarget = Mood − decayPerMin×Δt − drain(P)×Δt + Σ eventΔ
    /// drain(P)   = moodDrainCoef × (P / drainRefP)^drainExp
    /// Mood       = Mood + (MoodTarget − Mood) × α,  α = 1 − exp(−Δt/τ)
    /// ```
    ///
    /// **`decayPerMin` 两档口径（S7-M5 裁定）**：`02 §5.7` 给 `decayPerMin = -1.0` /
    /// `decayPerMinActive = -0.5`，`01 §6.11.3` 注为「互动活跃期 0.5」。本卡取
    /// **「互动活跃」= 用户在场且交互可达**（`presence_here && interaction_available`）：
    /// 人在电脑前陪着 → 衰减慢（−0.5/min）；人不在 / 不可交互 → 衰减快（−1.0/min）。
    /// 该判定用到的两个布尔均来自本拍因子快照，不引入新感知量。
    fn step_mood(&mut self, dt_min: f32, event_delta: f32, now_ms: i64) -> Vec<EmotionEvent> {
        let m: &MoodDimCfg = &self.cfg.dimensions.mood;
        let ref_p = self.cfg.mood.drain_ref_p;
        let p_ratio = if ref_p > 0.0 { (self.neglect.p / ref_p).max(0.0) } else { 0.0 };
        let drain = self.cfg.mood.drain_coef * p_ratio.powf(self.cfg.mood.drain_exp);
        // 互动活跃期（在场 + 可达）→ `decayPerMinActive`；否则 `decayPerMin`（S7-M5）。
        let active = self.factors.presence_here && self.factors.interaction_available;
        let decay = if active {
            m.decay_per_min_active.abs()
        } else {
            m.decay_per_min.abs()
        };
        // S7-M3：Mood 衰减乘子由**耦合矩阵**给出（C-01/C-02/C-06 取 max + 5s 平滑，
        // `02 §5.10` S3 阶段）。
        let needs_mul = self.coupling_out.mood_decay_mul;
        // S7-M2：香味 Buff（`01 §6.12.3`：洗护用品 12min 内 Mood 衰减 ×0.8）。
        let scent_mul = if self.needs.scent_buff_active(now_ms) { SENT_BUFF_MOOD_DECAY_MUL } else { 1.0 };
        let needs_mul = needs_mul * scent_mul;
        let target = self.state.values.mood - decay * needs_mul * dt_min - drain * dt_min + event_delta;
        let tau = if target < self.state.values.mood {
            self.cfg.inertia.tau_down_sec as f32
        } else {
            self.cfg.inertia.tau_up_sec as f32
        };
        let alpha = if tau > 0.0 { 1.0 - (-(dt_min * 60.0) / tau).exp() } else { 1.0 };
        let before = self.state.values.mood;
        self.state.values.mood = (before + (target - before) * alpha).clamp(m.min, m.max);
        self.state.values.clamp_to_cfg(self.cfg, self.needs_cfg);
        if (self.state.values.mood - before).abs() > f32::EPSILON {
            vec![EmotionEvent::ValuesChanged {
                mood: self.state.values.mood,
                energy: self.state.values.energy,
                boredom: self.boredom_display(),
                satiety: self.state.values.satiety,
                cleanliness: self.state.values.cleanliness,
            }]
        } else {
            vec![]
        }
    }

    /// 日切：自适应基线滚动（S7-M4）+ 自然消气计量与正向计数重置（S7-M5）。
    fn daily_roll(&mut self, env: &TickEnv<'_>) -> Vec<EmotionEvent> {
        let today = date_key(&env.now_local);
        if self.state.today.is_empty() {
            self.state.today = today;
            return vec![];
        }
        if self.state.today != today {
            self.state.today = today.clone();
            self.positive_interactions = 0;
            self.longing = false;
            // S7-M4：自适应基线日切（`T_exp` 滚动 + 关系降温判定）。
            let t_exp0 = self.personality.t_exp0(&self.cfg.adapt);
            return self
                .adapt
                .roll_day(&today, t_exp0, &self.cfg.adapt)
                .into_iter()
                .map(|ev| match ev {
                    crate::emotion::adapt::AdaptEvent::RelationCooling(level) => {
                        EmotionEvent::RelationCooling { level }
                    }
                })
                .collect();
        }
        vec![]
    }

    /// 交互回调（最简记账：正向交互计数 + 自适应基线的间隔采样）。
    ///
    /// 完整缓解表（`relief.hover/click/…`）与冷却（`cooldownSec`）见
    /// [`Self::on_interaction_kind`] / [`Self::relief_for`]。
    pub fn on_interaction(&mut self, positive: bool, now_ms: i64) -> TickOutcome {
        self.last_tick_ms.get_or_insert(now_ms);
        if positive {
            self.observe_positive_interaction(now_ms);
        }
        TickOutcome::default()
    }

    /// 正向交互统一记账（S7-M4/S7-M5 单一入口）：计数 + 自适应间隔采样 + 自然消气窗口计数。
    ///
    /// **间隔采样口径**：相邻两次正向交互的间隔（分钟）送给 `AdaptationState::on_interval`，
    /// 由后者按 `adapt.outlierHours` 剔除离群段（>8h）；首次交互无前驱 → 只锚定不采样。
    fn observe_positive_interaction(&mut self, now_ms: i64) {
        self.positive_interactions = self.positive_interactions.saturating_add(1);
        self.natural_cool.observe_positive(now_ms);
        let today = date_key(&self.last_now_local);
        self.adapt.observe_interaction(&today);
        if let Some(prev) = self.last_interaction_ms {
            let interval_min = (now_ms - prev).max(0) as f32 / 60_000.0;
            self.adapt.on_interval(&today, interval_min, &self.cfg.adapt);
        }
        self.last_interaction_ms = Some(now_ms);
    }

    // -----------------------------------------------------------------------
    // S4-M3 / S4-M4：交互缓解表 + 道歉三部曲 + force_lower
    // -----------------------------------------------------------------------

    /// 设置「轻松模式」（`01 §6.5.2`；降低抚摸门槛至 `easyModeStrokeSec`）。
    /// 设置项接线归 S5；此处只透传。
    pub fn set_easy_coax_mode(&mut self, on: bool) {
        self.coax.set_easy_mode(on);
    }

    /// 热更新「情绪敏感度」（`01 FR-7-9 / FR-11-11`；S5-M4 设置热更新入口）。
    ///
    /// 语义与边界：
    /// - 传入值按 `02 §5.2` 的 `rateClamp` **夹紧**到 `[min, max]` 后写入 `sensitivity.value`，
    ///   使「实时 tick 速率缩放」即刻生效（`rate_from` 在每次 tick 重新求值，无需重建引擎）；
    /// - 夹紧只作用于**实时**通道；离线补偿窗口与 P 封顶一律走 unclamped raw 速率
    ///   （`01` P2-2 裁定 / `02 §5.2`），故敏感度调低**不会**削弱离线恢复与封顶上限；
    /// - 非有限值（`NaN` / `inf`）忽略，保持原值（脏输入不破坏内核状态）。
    pub fn set_sensitivity_value(&mut self, value: f32) {
        if !value.is_finite() {
            return;
        }
        let clamped = value.clamp(self.sensitivity.rate_clamp.min, self.sensitivity.rate_clamp.max);
        self.sensitivity.value = clamped;
    }

    /// 只读：三部曲子状态（`pet://coax` 投影 / 诊断用）。
    #[inline]
    #[must_use]
    pub fn coax_step(&self) -> CoaxStep {
        self.coax.step()
    }

    /// 只读：进度环比例。
    #[inline]
    #[must_use]
    pub fn coax_progress(&self) -> f32 {
        self.coax.progress()
    }

    /// 只读：是否已离家（窗口应隐藏）。
    #[inline]
    #[must_use]
    pub fn is_runaway_away(&self) -> bool {
        self.coax.is_away()
    }

    /// 交互缓解（`emotion.json.relief.*` + `cooldownSec` + `strokeMaxPerWindow`，FR-11-7）。
    ///
    /// 返回本次**实际可用**的 P 缓解量；冷却未过 / 抚摸超窗 → `0.0`（并**不**刷新冷却，
    /// 避免「连点把冷却一直顶住」）。调用方负责从 P 中扣减（见 [`Self::apply_relief`]）。
    pub fn relief_for(&mut self, kind: ReliefKind, now_ms: i64) -> f32 {
        let r = &self.cfg.relief;
        let (value, cooldown_sec) = match kind {
            ReliefKind::Hover => (r.hover, r.cooldown_sec.hover),
            ReliefKind::Click => (r.click, r.cooldown_sec.click),
            ReliefKind::DoubleClick => (r.double_click, r.cooldown_sec.double_click),
            ReliefKind::Stroke => (r.stroke, r.cooldown_sec.stroke),
            ReliefKind::Feed => (r.feed, 0),
            ReliefKind::Bath => (r.bath, 0),
            ReliefKind::Play => (r.play, 0),
        };
        // 抚摸窗口上限（`strokeMaxPerWindow` / `strokeWindowSec`）：先判后记，超窗直接失效。
        if kind == ReliefKind::Stroke {
            let win_ms = (r.stroke_window_sec as i64).saturating_mul(1_000).max(0);
            self.relief.stroke_window.retain(|t| now_ms - *t < win_ms);
            if r.stroke_max_per_window > 0
                && self.relief.stroke_window.len() >= r.stroke_max_per_window as usize
            {
                return 0.0;
            }
            self.relief.stroke_window.push(now_ms);
        }
        if let Some(slot) = ReliefTracker::slot(kind) {
            if cooldown_sec > 0 {
                if let Some(last) = self.relief.last_ms[slot] {
                    if now_ms - last < (cooldown_sec as i64).saturating_mul(1_000) {
                        return 0.0;
                    }
                }
            }
            self.relief.last_ms[slot] = Some(now_ms);
        }
        value as f32
    }

    /// 从 P 中扣减缓解量（钳 `[0, cap]`）。
    fn apply_relief(&mut self, amount: f32) {
        if amount <= 0.0 {
            return;
        }
        self.neglect.p = (self.neglect.p - amount).clamp(0.0, self.neglect.cap);
    }

    /// 交互意图统一入口（S4-M3 / S7-M4~M6）：缓解表 → 三部曲推进 → 正面 / 负面记账。
    ///
    /// `InteractionKind` 到三条通路的映射（唯一映射点，避免上层重复分支）：
    ///   - 缓解：Hover/Click/DoubleClick/Stroke/Feed/Bath、轨迹彩蛋 Circle/Line/Zigzag → Play；
    ///   - 三部曲：Click → 呼唤；DoubleClick → 比心；Tickle/Throw → **打断**（负向）；
    ///     `TrayCoax` → 呼唤（`02 §5.23` 第 2 层托盘替代入口）；
    ///   - 记账：有缓解语义的交互计入正向（自然消气条件② + 自适应间隔采样，S7-M4/M5）；
    ///     Tickle/Throw 计入**负向**（`rough` 计数 + 两条保持窗口中断，S7-M4/M6）。
    pub fn on_interaction_kind(&mut self, kind: InteractionKind, now_ms: i64) -> Vec<EmotionEvent> {
        self.last_tick_ms.get_or_insert(now_ms);
        let relief_kind = match kind {
            InteractionKind::Hover => Some(ReliefKind::Hover),
            InteractionKind::Click => Some(ReliefKind::Click),
            InteractionKind::DoubleClick => Some(ReliefKind::DoubleClick),
            InteractionKind::Stroke => Some(ReliefKind::Stroke),
            InteractionKind::Feed => Some(ReliefKind::Feed),
            InteractionKind::Bath => Some(ReliefKind::Bath),
            InteractionKind::Circle | InteractionKind::Line | InteractionKind::Zigzag => {
                Some(ReliefKind::Play)
            }
            _ => None,
        };
        let mut out = Vec::new();
        if kind == InteractionKind::Tickle || kind == InteractionKind::Throw {
            // B-6：负向事件 → `rough` 计数 + 两条保持窗口中断。
            self.rough.observe_negative(now_ms, &self.cfg.rough);
            self.natural_cool.observe(now_ms, self.neglect.p, &self.cfg.confirm, true);
            self.unreachable.observe(now_ms, false, true);
        }
        if let Some(rk) = relief_kind {
            let relief = self.relief_for(rk, now_ms);
            self.apply_relief(relief);
            self.observe_positive_interaction(now_ms);
        }
        // S7-M6 / `02 §5.23` M-05：托盘「喂食 / 洗澡」= 等效一次喂食 / 洗澡
        // （除 P 缓解外，还要落到六维数值，否则「穿透时宠物长期挨饿变脏」）。
        match kind {
            InteractionKind::Feed => {
                self.apply_need_delta(crate::emotion::need_keys::SATIETY, self.cfg.relief.feed as f32, now_ms);
            }
            InteractionKind::Bath => {
                self.apply_need_delta(crate::emotion::need_keys::CLEANLINESS, self.cfg.relief.bath as f32, now_ms);
                self.needs.mark_bath(self.needs_cfg, now_ms);
            }
            _ => {}
        }

        let coax_input = match kind {
            InteractionKind::Click | InteractionKind::TrayCoax => Some(CoaxInput::Call),
            InteractionKind::DoubleClick => Some(CoaxInput::Heart),
            InteractionKind::Tickle | InteractionKind::Throw => Some(CoaxInput::Negative),
            _ => None,
        };
        if let Some(input) = coax_input {
            out.extend(self.coax_input(input, now_ms));
        }
        out
    }

    /// 需求数值增量（S7-M6 托盘喂食 / 洗澡出口；`needs` 侧负责 clamp 与跨档事件）。
    ///
    /// 六维数值真源仍是 [`PetState::values`]（S7-M2 口径），此处只经 `NeedsSystem`
    /// 的道具出口写入，避免第二真源。
    fn apply_need_delta(&mut self, key: &str, delta: f32, now_ms: i64) {
        let outcome = match key {
            crate::emotion::need_keys::SATIETY => {
                self.needs.add_satiety(&mut self.state.values, self.needs_cfg, delta)
            }
            crate::emotion::need_keys::CLEANLINESS => {
                self.needs.add_cleanliness(&mut self.state.values, self.needs_cfg, delta)
            }
            _ => return,
        };
        // 道具出口也可能跨档（喂饱 → 分档变化）→ 供 `dp-app` 发 `pet://needs`。
        if outcome.band_changed {
            self.last_needs = Some(outcome);
        }
        let _ = now_ms;
    }

    /// 三部曲离散输入（`03 §2` 三部曲 / `02 §5.23` 托盘输入）。
    pub fn coax_input(&mut self, input: CoaxInput, now_ms: i64) -> Vec<EmotionEvent> {
        let effects = self.coax.on_input(input, now_ms, &self.cfg.coax);
        self.apply_coax_effects(effects)
    }

    /// 三部曲 20Hz 推进（连续抚摸累计 / 比心窗 / 离家演出计时）。
    ///
    /// `stroke_active` = 光标当前是否处于抚摸态（`dp-app` `InteractionConsumer::is_stroking`）。
    pub fn coax_stroke_tick(&mut self, now_ms: i64, stroke_active: bool) -> Vec<EmotionEvent> {
        let effects = self.coax.tick(now_ms, stroke_active, &self.cfg.coax);
        self.apply_coax_effects(effects)
    }

    /// 托盘「摸摸」累计一格（`02 §5.23` 第 2 层）。
    pub fn coax_tray_stroke(&mut self, now_ms: i64) -> Vec<EmotionEvent> {
        self.coax_input(CoaxInput::TrayStroke, now_ms)
    }

    /// **托盘替代入口单点分派**（S7-M6 / `01 FR-11-12` 第 2 层）。
    ///
    /// 托盘只有一个「❤ 摸摸{name}」项常驻，其**语义随三部曲阶段推进**（菜单项文案由
    /// `dp-app` 依据 [`Self::coax_step`] 渲染：呼唤阶段显示「摸摸」、比心阶段显示「比心」）：
    ///
    ///   - `Idle` → [`CoaxInput::Call`]（呼唤）；
    ///   - `Call` / `Stroke` → [`CoaxInput::TrayStroke`]（累计一次抚摸，满 `TRAY_COAX_STROKE_TAPS` 次进比心）；
    ///   - `Heart` → [`CoaxInput::Heart`]（完成比心 → 三部曲成功）；
    ///   - `Runaway` / `Away` → [`CoaxInput::Recall`]（L5 离家 → 先走回）。
    ///
    /// **产品底线**：经托盘编辑的仍是**完整道歉三部曲**（呼唤 → 抚摸 → 比心），
    /// 不因「不可达」而降低难度（`01 FR-11-12` 🚫 条）。
    pub fn coax_tray_tap(&mut self, now_ms: i64) -> Vec<EmotionEvent> {
        let input = match self.coax.step() {
            // 已呼唤（或抚摸中）→ 每次累计一格抚摸（满 `TRAY_COAX_STROKE_TAPS` 进比心窗）。
            CoaxStep::Call | CoaxStep::Stroke => CoaxInput::TrayStroke,
            CoaxStep::Heart => CoaxInput::Heart,
            CoaxStep::Runaway | CoaxStep::Away => CoaxInput::Recall,
            // 未开始 → 首击 = 呼唤（`01 FR-11-12`「点托盘项 = 呼唤」）。
            CoaxStep::Idle => CoaxInput::Call,
        };
        self.coax_input(input, now_ms)
    }

    /// 托盘替代入口的单次抚摸（`02 §5.23`：抚摸 = 完整一次抚摸，relief 15 / Mood +8）。
    ///
    /// 与 [`Self::coax_tray_stroke`] 的差别：本方法走**通用缓解表**（`relief.stroke` +
    /// `strokeMaxPerWindow` 窗口上限 + 正向记账），用于「不可达且未在哄好流程中」的
    /// 日常摸摸；`coax_tray_stroke` 只推进三部曲进度。
    pub fn tray_stroke(&mut self, now_ms: i64) -> Vec<EmotionEvent> {
        self.on_interaction_kind(InteractionKind::Stroke, now_ms)
    }

    /// 托盘「喂食」（S7-M6 / `02 §5.23` M-05）：等效一次喂食（`relief.feed` + 饱食度恢复）。
    pub fn tray_feed(&mut self, now_ms: i64) -> Vec<EmotionEvent> {
        self.on_interaction_kind(InteractionKind::Feed, now_ms)
    }

    /// 托盘「洗澡」（S7-M6 / `02 §5.23` M-05）：等效一次洗澡（`relief.bath` + 清洁度恢复）。
    pub fn tray_bath(&mut self, now_ms: i64) -> Vec<EmotionEvent> {
        self.on_interaction_kind(InteractionKind::Bath, now_ms)
    }

    /// L5 找回（托盘「把心月狐找回来」→ 走回，仍需完成三部曲）。
    pub fn coax_recall(&mut self, now_ms: i64) -> Vec<EmotionEvent> {
        self.coax_input(CoaxInput::Recall, now_ms)
    }

    /// `force_lower` 兜底（`02 §5.23` R18）：托盘 / 设置「重置情绪」强制解除 L5。
    ///
    /// 语义：清空三部曲与离家态、`P = 0`、档位回 L0、Mood 抬到 `recoverMoodFloor`。
    pub fn force_lower(&mut self, _now_ms: i64) -> Vec<EmotionEvent> {
        let cleared = self.coax.force_lower();
        let mut out = self.apply_coax_effects(cleared);
        let from = self.neglect.level;
        self.neglect.p = 0.0;
        self.neglect.level = 0;
        self.neglect.pending_level = 0;
        self.neglect.pending_since_ms = None;
        // S7-M5/M6：两条保持窗口与摸鱼档期一并复位（兜底语义 = 完全回到初始）。
        self.natural_cool.reset();
        self.unreachable.reset();
        self.slack_since_ms = None;
        self.slack_hint_fired = false;
        self.state.emotion = EmotionState::Idle;
        let floor = self.cfg.coax.recover_mood_floor as f32;
        if self.state.values.mood < floor {
            self.state.values.mood = floor;
        }
        if from != 0 {
            out.push(EmotionEvent::ColdLevelChanged {
                from,
                to: 0,
                mood_delta: 0.0,
                redirected: false,
                reason: ColdReason::Coax,
            });
        }
        out.push(EmotionEvent::PersistNow);
        out
    }

    /// S8-M1/M2：活动结算 / 前置消耗的**数值净变化**应用（`02 §5.15` 数值面）。
    ///
    /// 入参 [`crate::state::ActivityDeltas`]（由 `dp-app` 从 `dp-activity::ActivityReward`
    /// 翻译，`dp-core` 不依赖活动 crate）；本方法只落地数值：
    ///   - Mood / Energy / Cleanliness：加后整体 `clamp_to_cfg`（配置区间，C7）；
    ///   - 亲密度经验：`PetValues::add_affinity_exp`（满 `100 × level` 升级，上限 maxLevel）；
    ///   - P：与 coax relief / penalty 同口径（clamp `[0, cap]`）；
    ///   - rough：`rough_step > 0` 时 `observe_negative`（`02 §5.1` F7 / B-6）。
    ///
    /// **经济入账 / 技能升级 / 学费与旅行券扣款归 S8-M5**，本方法不落地；也不需要
    /// `PersistNow`（存档脏位由 core-loop 侧统一按「有活动结算」置位）。
    pub fn apply_activity_deltas(
        &mut self,
        deltas: &crate::state::ActivityDeltas,
        now_ms: i64,
    ) -> Vec<EmotionEvent> {
        let mut out = Vec::new();
        if deltas.mood != 0.0 {
            self.state.values.mood += deltas.mood;
        }
        if deltas.energy != 0.0 {
            self.state.values.energy += deltas.energy;
        }
        if deltas.cleanliness != 0.0 {
            self.state.values.cleanliness += deltas.cleanliness;
        }
        if deltas.affinity_exp != 0.0 {
            let gained = self
                .state
                .values
                .add_affinity_exp(deltas.affinity_exp, self.cfg);
            if gained > 0 {
                out.push(EmotionEvent::AffinityLevelUp { level: self.state.values.affinity_level });
            }
        }
        // 数值变化不单独产 `ValuesChanged`：1Hz 全量快照（`pet://state`）每拍投影，
        // 且 `wire_for_events` 对 `ValuesChanged` 本就不产线上事件——避免重复。
        if deltas.neglect_p_delta != 0.0 {
            self.neglect.p =
                (self.neglect.p + deltas.neglect_p_delta).clamp(0.0, self.neglect.cap);
        }
        if deltas.rough_step > 0.0 {
            self.rough.observe_negative(now_ms, &self.cfg.rough);
        }
        if deltas.mood != 0.0 || deltas.energy != 0.0 || deltas.cleanliness != 0.0 {
            self.state.values.clamp_to_cfg(self.cfg, self.needs_cfg);
        }
        out
    }

    /// 把 `CoaxEffect` 翻译为算法事件并落地状态变更（P / 档位 / Mood / 动作）。
    fn apply_coax_effects(&mut self, effects: Vec<CoaxEffect>) -> Vec<EmotionEvent> {
        let mut out = Vec::new();
        for effect in effects {
            match effect {
                CoaxEffect::Progress { ratio, step } => {
                    out.push(EmotionEvent::CoaxProgress { ratio, step });
                }
                CoaxEffect::MoodGain(gain) => {
                    let m = &self.cfg.dimensions.mood;
                    self.state.values.mood = (self.state.values.mood + gain).clamp(m.min, m.max);
                }
                CoaxEffect::Succeeded => {
                    // `01 §6.5.2`：完成一次道歉三部曲 → P relief 60（`emotion.json.relief.coax`）。
                    let relief = self.cfg.relief.coax as f32;
                    self.neglect.p = (self.neglect.p - relief).clamp(0.0, self.neglect.cap);
                    let from = self.neglect.level;
                    let to = crate::emotion::coax::coax_target_level(from);
                    if to != from {
                        self.neglect.level = to;
                        self.neglect.pending_level = to;
                        self.neglect.pending_since_ms = None;
                        if let Some(def) = self.cfg.levels.get(to as usize).cloned() {
                            self.state.emotion = EmotionState::from_cfg_name(&def.emotion);
                            if def.mood_lock_max > 0 {
                                self.state.values.mood =
                                    self.state.values.mood.min(def.mood_lock_max as f32);
                            }
                        }
                        out.push(EmotionEvent::ColdLevelChanged {
                            from,
                            to,
                            mood_delta: 0.0,
                            redirected: false,
                            reason: ColdReason::Coax,
                        });
                    }
                    // `01 §6.11.8`：哄好瞬时兜底 `Mood = max(Mood, 50)`。
                    let floor = self.cfg.coax.recover_mood_floor as f32;
                    if self.state.values.mood < floor {
                        self.state.values.mood = floor;
                    }
                    out.push(EmotionEvent::CoaxSucceeded {
                        mood: self.state.values.mood,
                    });
                    // `01 §6.5.2` 步骤 3：播 ACT-E-06 哄好破涕为笑。
                    out.push(EmotionEvent::ForceAction {
                        action_id: COAX_SUCCESS_ACTION.to_string(),
                        priority: COAX_SUCCESS_PRIORITY,
                    });
                    out.push(EmotionEvent::PersistNow);
                }
                CoaxEffect::Failed(reason) => {
                    // B-6：打断正在进行的道歉三部曲额外 `P + rough.interruptPenalty`。
                    if reason == CoaxFailReason::Interrupted {
                        let penalty = self.cfg.rough.interrupt_penalty as f32;
                        self.neglect.p = (self.neglect.p + penalty).clamp(0.0, self.neglect.cap);
                    }
                    out.push(EmotionEvent::CoaxFailed { reason });
                }
            }
        }
        out
    }

    /// 离线补偿（`02 §5.5`；RV-16：分块推进 + `min(4)` 封顶）。
    ///
    /// 三分支：`JustLeft`（≤`graceMin`）/ `ColdApplied`（`graceMin`~`longingMin`）/
    /// `Longing`（>`longingMin`）。
    pub fn offline_compensate(&mut self, away_ms: i64, env: TickEnv<'_>) -> OfflineOutcome {
        let step_ms = (self.cfg.offline.step_sec as i64).max(1) * 1000;
        let max_steps = self.cfg.offline.max_sim_steps as i64;
        let steps = (away_ms / step_ms).clamp(0, max_steps);
        let start = self.last_tick_ms.unwrap_or(0);
        let mut t = start;
        for _ in 0..steps {
            t += step_ms;
            let mut e = env;
            // 离线期间：视为不在场（idle 远超阈值），且**不继承**暂停态
            e.preset_idle_ms = u64::MAX;
            e.session_paused = false;
            e.performing = false;
            e.event_delta = 0.0;
            // 直接推进内部状态（跳过 pause 观察：离线不算会话暂停）
            self.tick_offline_step(t, e);
        }
        // ★ S5-M2 收口（RV-16 的**第三道保险**）：把时钟锚点推到**真实当下** `start + away_ms`。
        //
        // 为什么必须做：`steps` 被 `maxSimSteps`（1440 = 24h）截断，若只把锚点留在
        // `start + steps × stepSec`，则 >24h 的残留间隙会在**首个 `tick_1s`** 被当成
        // 一次 Δt 全额计入（`tick_1s` 不钳 dt）——ΔP = 残留分钟 × 0.05 远超 cap，
        // P 直冲 120 = L5 阈值，于是「离线永不离家出走」被绕开。此处一次吞掉全部残留，
        // 使首个 tick 的 Δt 归零（`dt_ms == 0` 早退，零累积）。
        //
        // 对 ≤24h 的离线，模拟终点与 `start + away_ms` 最多差一个步长余数（<60s，忽略不计），
        // 故此改动不改变 `02 §5.5` 的 P 算例（3h / 4h / 5h / 24h 逐项不变）。
        let anchor = start.saturating_add(away_ms.max(0));
        self.last_tick_ms = Some(anchor);
        self.state.last_tick_ms = anchor;

        // RV-16 双保险：任何离线时长一律封顶 L4，绝不触发离家出走
        self.neglect.level = self.neglect.level.min(4);
        self.neglect.pending_level = self.neglect.level;
        self.neglect.pending_since_ms = None;

        let grace_ms = self.cfg.offline.grace_min as i64 * 60_000;
        let longing_ms = self.cfg.offline.longing_min as i64 * 60_000;
        if away_ms > longing_ms {
            self.longing = true;
            self.state.emotion = EmotionState::Longing;
            OfflineOutcome::Longing(self.neglect.level)
        } else if away_ms <= grace_ms {
            OfflineOutcome::JustLeft
        } else {
            OfflineOutcome::ColdApplied(self.neglect.level)
        }
    }

    /// 离线分块内的单步推进（不触碰 pause 窗口；`02 §5.5` 的 `tick_1s` 等价体）。
    ///
    /// ## 因子口径（S7-M4 登记，与 S5-M2 已交付行为**逐字一致**）
    ///
    /// 离线步的七因子取「**presence = `factorAway`（0.05），其余六项恒 1.0**」，封顶取
    /// `busyness.capFree`（120）。理由：
    ///
    ///   1. `02 §5.5` 的四个离线边界算例（3h/4h/5h/24h）与 AC-18 / `02 §6.1` 的
    ///      「P = 0.05 × 分钟数」推导，**明确以「其余因子 1.0」为前置**；
    ///   2. 离线期间**没有任何感知样本**（`busyness` 恒轻度、`rough` 无负向事件），
    ///      且模拟步不推进 `now_local`，若套用 `rhythm` 会因时段而扰动可复算性；
    ///   3. 离线语义是「用户不在场」，不是「用户在忙 / 现在是深夜」——
    ///      用 `presence` 单因子表达最忠实。
    ///
    /// 这同时保证 RV-16 的**第二道保险**（`P = 0.05 × 离线分钟数`：24h → 72 < 120）
    /// 与封顶 L4 的推导成立。
    fn tick_offline_step(&mut self, now_ms: i64, env: TickEnv<'_>) {
        let Some(last) = self.last_tick_ms else {
            self.last_tick_ms = Some(now_ms);
            return;
        };
        let dt_ms = (now_ms - last).max(0);
        if dt_ms == 0 {
            return;
        }
        self.last_tick_ms = Some(now_ms);
        let dt_min = dt_ms as f32 / 60_000.0;
        // 离线因子口径：仅 presence 生效（不可达时与「离场」同档，不归零）。
        let presence = if env.interaction_available {
            self.cfg.presence.factor_away
        } else {
            self.interaction.unavailable_presence_factor
        };
        self.factors = FactorSet {
            presence,
            presence_here: false,
            interaction_available: env.interaction_available,
            ..FactorSet::default()
        };
        self.factors.recompute_product();
        self.neglect.rate_per_min = rate_from(&self.sensitivity, self.factors.product);
        let rate = self.neglect.rate_per_min;
        let cap = self.cfg.busyness.cap_free as f32;
        neglect::accrue(&mut self.neglect, dt_min, rate, cap);
        let _ = self.settle_level(now_ms, false);
        let _ = self.step_mood(dt_min, 0.0, now_ms);
        // 离线期间不推进日切（`02 §5.5`：日切用真实 `now_local` 处理跨日）
    }
}

/// 离线补偿结果（`02 §4.3`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfflineOutcome {
    /// ≤ `offline.graceMin`：刚离开，不衰减。
    JustLeft,
    /// `graceMin` ~ `longingMin`：冷落已生效。
    ColdApplied(u8),
    /// > `longingMin`：想念展示。
    Longing(u8),
}

/// `YYYY-MM-DD` 日期键（日切用；不读时钟，纯格式投影）。
fn date_key(dt: &chrono::DateTime<Local>) -> String {
    format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
}

/// 相同 (from,to) 的阶段事件去重（避免同一 tick 内重复播报）。
fn dedup_events(events: Vec<EmotionEvent>) -> Vec<EmotionEvent> {
    let mut out: Vec<EmotionEvent> = Vec::with_capacity(events.len());
    for ev in events {
        if let EmotionEvent::ColdLevelChanged { to, .. } = ev {
            let dup = out.iter().any(|e| matches!(e, EmotionEvent::ColdLevelChanged { to: t, .. } if *t == to));
            if dup {
                continue;
            }
        }
        out.push(ev);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{EmotionConfig, NeedsConfig};
    use crate::emotion::coax::RUNAWAY_PERFORMANCE_MS;
    use chrono::{Local, TimeZone};

    const MIN: i64 = 60_000;

    /// 固定本地时间（**非深夜** 10:00，避免深夜重定向干扰常规断言）。
    fn day() -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 14, 10, 0, 0).unwrap()
    }

    /// 固定本地时间（深夜 02:00，用于深夜重定向断言）。
    fn night() -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 14, 2, 0, 0).unwrap()
    }

    fn env(now: chrono::DateTime<Local>) -> TickEnv<'static> {
        TickEnv { now_local: now, ..TickEnv::default() }
    }

    /// 在场 + 不休眠 + 非外出的默认环境。
    fn env_here(now: chrono::DateTime<Local>) -> TickEnv<'static> {
        TickEnv { now_local: now, preset_idle_ms: 0, ..TickEnv::default() }
    }

    /// 空转 `n` 次 tick（每步 1s），返回累计事件。
    fn run(e: &mut EmotionEngine<'_>, start_ms: i64, n: u64, mk: impl Fn(i64) -> TickEnv<'static>) -> Vec<EmotionEvent> {
        let mut all = Vec::new();
        for i in 0..=n {
            let t = start_ms + (i as i64) * 1000;
            all.extend(e.tick_1s(t, mk(t)).events);
        }
        all
    }

    // ══════════════════════════════════════════════════════════════════
    // 验收 1：Given 锁定屏幕 30min；Then 累积暂停，解锁后不跳变
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn ac1_screen_locked_30min_does_not_accumulate_pressure() {
        let (cfg, needs) = (EmotionConfig::default(), NeedsConfig::default());
        let mut e = EmotionEngine::new(&cfg, &needs);
        // 先跑 5min 正常态，P 应有累积（证明基线非零）
        run(&mut e, 0, 300, |_| env_here(day()));
        let p_before_lock = e.neglect.p;
        assert!(p_before_lock > 0.0, "正常态应累积 P，实际 {p_before_lock}");

        // 锁屏 30min：每秒一次 tick，全部 session_paused = true
        let lock_start = 300_000;
        let mut pause_changes = 0;
        let mut events_during_lock = Vec::new();
        for i in 0..=(30 * 60) {
            let t = lock_start + i * 1000;
            let out = e.tick_1s(t, TickEnv { session_paused: true, ..env_here(day()) });
            if out.pause_changed {
                pause_changes += 1;
            }
            events_during_lock.extend(out.events);
        }

        // 累积暂停：P 与阶段一字不动
        assert_eq!(e.neglect.p, p_before_lock, "锁屏期间 P 必须冻结");
        assert_eq!(e.neglect.level, 0, "锁屏期间阶段不得跳变");
        assert!(events_during_lock.is_empty(), "锁屏期间不得产出任何情绪事件：{events_during_lock:?}");
        // 暂停窗口只迁移一次（进入），幂等
        assert_eq!(pause_changes, 1, "暂停观察应恰好迁移一次");
        assert!(e.state.pause.is_paused(), "应处于暂停中");
        assert_eq!(e.state.pause.count, 1);
        assert_eq!(e.state.pause.start_ms, Some(lock_start));
    }

    #[test]
    fn ac1_unlock_does_not_jump_and_resumes_normally() {
        let (cfg, needs) = (EmotionConfig::default(), NeedsConfig::default());
        let mut e = EmotionEngine::new(&cfg, &needs);
        run(&mut e, 0, 300, |_| env_here(day()));
        let p_before = e.neglect.p;
        let mood_before = e.state.values.mood;

        // 锁屏 30min
        let lock_start = 300_000;
        for i in 0..=(30 * 60) {
            e.tick_1s(lock_start + i * 1000, TickEnv { session_paused: true, ..env_here(day()) });
        }
        // 解锁瞬间：**不得跳变**（不补偿锁屏期间的 P，也不补扣 Mood）
        let unlock_t = lock_start + 30 * 60 * 1000;
        let out = e.tick_1s(unlock_t, env_here(day()));
        assert!(out.pause_changed, "解锁应产生一次暂停迁移");
        assert_eq!(e.neglect.p, p_before, "解锁瞬间 P 不得跳变");
        let mood_jump = (e.state.values.mood - mood_before).abs();
        assert!(mood_jump < 1.0, "解锁瞬间 Mood 不得跳变，实际变化 {mood_jump}");
        assert!(!e.state.pause.is_paused());
        assert_eq!(e.state.pause.end_ms, Some(unlock_t));
        assert_eq!(e.state.pause.accumulated_ms, 30 * MIN);

        // 解锁后恢复累积：继续 5min，P 必增
        run(&mut e, unlock_t, 300, |_| env_here(day()));
        assert!(e.neglect.p > p_before, "解锁后应恢复累积：{} -> {}", p_before, e.neglect.p);
    }

    #[test]
    fn pause_window_records_start_end_timestamps_into_state() {
        // P2-2 裁定：起止时间戳入状态，S5-M2 只读
        let (cfg, needs) = (EmotionConfig::default(), NeedsConfig::default());
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env_here(day()));
        e.tick_1s(1_000, TickEnv { session_paused: true, ..env_here(day()) });
        assert_eq!(e.state.pause.start_ms, Some(1_000));
        assert_eq!(e.state.pause.end_ms, None);
        e.tick_1s(61_000, env_here(day()));
        assert_eq!(e.state.pause.start_ms, Some(1_000), "起止时间戳须保留在状态里");
        assert_eq!(e.state.pause.end_ms, Some(61_000));
        assert_eq!(e.state.pause.current_span_ms(999_999), 60_000);
    }

    #[test]
    fn fullscreen_pause_uses_same_channel_as_lock() {
        // F10：锁屏 / 远程桌面 / 全屏前台共用 session_paused 一个口径
        let (cfg, needs) = (EmotionConfig::default(), NeedsConfig::default());
        let mut e = EmotionEngine::new(&cfg, &needs);
        run(&mut e, 0, 120, |_| env_here(day()));
        let p = e.neglect.p;
        for i in 0..=600 {
            e.tick_1s(120_000 + i * 1000, TickEnv { session_paused: true, ..env_here(day()) });
        }
        assert_eq!(e.neglect.p, p);
    }

    #[test]
    fn performing_also_freezes_pressure() {
        // `02 §5.2` 第 0 步：session_paused || performing
        let (cfg, needs) = (EmotionConfig::default(), NeedsConfig::default());
        let mut e = EmotionEngine::new(&cfg, &needs);
        run(&mut e, 0, 120, |_| env_here(day()));
        let p = e.neglect.p;
        for i in 0..=600 {
            e.tick_1s(120_000 + i * 1000, TickEnv { performing: true, ..env_here(day()) });
        }
        assert_eq!(e.neglect.p, p, "演出期间 P 必须冻结");
        assert!(!e.state.pause.is_paused(), "performing 不是会话暂停，不应污染暂停窗口");
    }

    // ══════════════════════════════════════════════════════════════════
    // 验收 2：Given 修改 emotion.json 数值；Then 行为随变化
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn ac2_threshold_change_alters_level_transitions() {
        let needs = NeedsConfig::default();

        // 基线：阈值 5/15/30/60/120 → 在场快进 200min 应稳定到 L3（P=200 封顶 120 → L5？
        // 走离场 0.05 因子以便精确控制 P）
        let cfg_a = EmotionConfig::default();
        let mut a = EmotionEngine::new(&cfg_a, &needs);
        a.tick_1s(0, env(day()));
        for i in 1..=300u64 {
            a.tick_1s((i as i64) * 1000, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
        }
        // 离场 5min：P = 0.05×5 = 0.25 → L0
        assert_eq!(a.neglect.level, 0, "P={}", a.neglect.p);

        // 压低 L1 阈值到 0 → 同为 L1 应立刻进入（P≈0.25 ≥ 0）
        let mut cfg_b = EmotionConfig::default();
        cfg_b.thresholds.l1 = 0;
        let mut b = EmotionEngine::new(&cfg_b, &needs);
        b.tick_1s(0, env(day()));
        // 确认期需 60s：跑到 90s 确保跨过 up_sec
        for i in 1..=90u64 {
            b.tick_1s((i as i64) * 1000, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
        }
        assert_eq!(b.neglect.level, 1, "阈值下调后应进入 L1（P={}）", b.neglect.p);
    }

    #[test]
    fn ac2_confirm_periods_come_from_config() {
        let needs = NeedsConfig::default();
        let mut cfg = EmotionConfig::default();
        cfg.thresholds.l1 = 0; // 立即满足 L1 条件
        cfg.confirm.up_sec = 10; // 缩短确认期到 10s

        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        // 5s 时仍在确认期
        for i in 1..=5u64 {
            let out = e.tick_1s((i as i64) * 1000, env(day()));
            assert!(!out.events.iter().any(|ev| matches!(ev, EmotionEvent::ColdLevelChanged { .. })), "t={i}s 仍在确认期");
        }
        assert_eq!(e.neglect.level, 0);
        assert_eq!(e.neglect.pending_since_ms, Some(1_000));
        // 11s 时确认期已过 → 升级
        let out = e.tick_1s(11_000, env(day()));
        assert!(
            out.events.iter().any(|ev| matches!(ev, EmotionEvent::ColdLevelChanged { from: 0, to: 1, .. })),
            "11s 应完成 L0→L1：{:?}",
            out.events
        );
        assert_eq!(e.neglect.level, 1);
    }

    #[test]
    fn ac2_mood_delta_and_relief_config_are_consumed() {
        // moodDelta 由配置驱动：改 levels[1].moodDelta 后扣减量随之变化。
        // 用 cramped 阈值使 L0→L1 立即可达，再隔离 decay/drain，单看 moodDelta 效果。
        let needs = NeedsConfig::default();

        let run_case = |delta: i32| -> f32 {
            let mut cfg = cramped_cfg();
            cfg.levels[1].mood_delta = delta;
            cfg.inertia.tau_down_sec = 1; // 消除一阶低通延迟，让 delta 立即体现
            cfg.inertia.tau_up_sec = 1;
            cfg.mood.drain_coef = 0.0; // 隔离 P 抽血，只观察 moodDelta
            let mut e = EmotionEngine::new(&cfg, &needs);
            e.tick_1s(0, env(day()));
            // factor_away=1.0 → P 每分钟 +1；跨过 l1=1 后升至 L1
            for i in 1..=120u64 {
                e.tick_1s((i as i64) * 1000, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
                if e.neglect.level >= 1 {
                    break;
                }
            }
            e.state.values.mood
        };
        let m5 = run_case(-5);
        let m25 = run_case(-25);
        // `02 §5.3`：实际扣减 = moodDelta × mood_delta_scale(1.02 @temper=0.55)
        let scale = cfg_delta_scale();
        assert!((m5 - (60.0 - 5.0 * scale)).abs() < 1e-2, "moodDelta=-5 应扣 {}…，实际 {m5}", 5.0 * scale);
        assert!((m25 - (60.0 - 25.0 * scale)).abs() < 1e-2, "moodDelta=-25 应扣 {}…，实际 {m25}", 25.0 * scale);
        // 「改配置 → 行为随变化」的核心断言：两者差 = ΔmoodDelta × scale
        assert!((m5 - m25 - 20.0 * scale).abs() < 1e-2, "Δ 应为 20×scale：{m5} vs {m25}");
    }

    /// `02 §5.3` 的 `mood_delta_scale`（默认 temper=0.55 → 1.02）。
    fn cfg_delta_scale() -> f32 {
        let c = EmotionConfig::default().personality;
        c.mood_delta_base + c.mood_delta_temper * c.defaults.temper
    }

    // ══════════════════════════════════════════════════════════════════
    // 状态机骨架：L0~L5、不跳级、升级扣减 / 降级不扣
    // ══════════════════════════════════════════════════════════════════

    /// 压低全部阈值，使「离场慢速累积」也能走完 0→5 全链。
    ///
    /// 用途：验证**逐层不跳级**语义。真实配置下 P 受 `capFree=120` 约束，
    /// 从 L0 爬到 L5 需要 `120` 分钟离场累积（约 2400 个 3s tick），测试不经济；
    /// 故按「配置驱动」这一 S4-M1 核心性质，直接压阈值构造跨 5 档的场景。
    fn cramped_cfg() -> EmotionConfig {
        let mut cfg = EmotionConfig {
            thresholds: crate::config::model::ThresholdsCfg { l1: 1, l2: 2, l3: 3, l4: 4, l5: 5 },
            ..EmotionConfig::default()
        };
        cfg.presence.factor_away = 1.0; // 让离场速率=1.0/min，压缩测试时长
        cfg.confirm.up_sec = 0;
        cfg.confirm.down_sec = 0;
        cfg.dimensions.mood.decay_per_min = 0.0;
        cfg.dimensions.mood.decay_per_min_active = 0.0;
        cfg.mood.drain_coef = 0.0;
        cfg.levels[5].mood_lock_max = 0; // 本组测试不叠加 moodLock，单独由专门用例验证
        cfg
    }

    #[test]
    fn level_climb_is_never_skipping_and_deducts_once_per_step() {
        let cfg = cramped_cfg();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));

        // 离场 5min → P = 0.25，越过全部压低后的阈值（1..5）
        let mut seen: Vec<(u8, u8)> = Vec::new();
        for i in 1..=600u64 {
            for ev in e.tick_1s((i as i64) * 1000, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) }).events {
                if let EmotionEvent::ColdLevelChanged { from, to, .. } = ev {
                    seen.push((from, to));
                }
            }
            if e.neglect.level == 5 {
                break;
            }
        }
        // 逐层：0→1→2→3→4→5，绝不跳级
        assert_eq!(seen, vec![(0, 1), (1, 2), (2, 3), (3, 4), (4, 5)], "阶段迁移必须逐层：{seen:?}");
        assert_eq!(e.neglect.level, 5);
        // 每层各扣一次：−5−10−15−20−30 = −80，从 60 起 → 0（clamp）
        assert_eq!(e.state.values.mood, 0.0, "Mood 应被逐层扣减至下限");
    }

    #[test]
    fn level_descent_never_deducts_mood() {
        // R18-4 红线：降级路径不得产生负向扣减。
        //
        // S4-M4 起 L4/L5 由**强制地板**锁定（不得自然回退），故本用例改在 **L3**
        // 上验证「逐层回落不扣 Mood」；L4/L5 的地板另见
        // `l4_l5_require_coax_and_never_regress_naturally`。
        let cfg = cramped_cfg();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let mut t = 0i64;
        for i in 1..=600u64 {
            t = (i as i64) * 1000;
            e.tick_1s(t, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
            if e.neglect.level == 3 {
                break;
            }
        }
        assert_eq!(e.neglect.level, 3);

        // ① L3 → L2（S7-M5 收紧）：L3 **不开放无条件自然回退**，只能走自然消气通道，
        //    故先补齐该通道的三条可满足条件（P<15 持续 60s + 期间正向交互 ≥3 + 近 1h 无负向）。
        e.neglect.p = 0.0;
        for i in 0..cfg.confirm.natural_min_positive {
            let _ = e.on_interaction(true, t + 1 + i as i64);
        }
        let mut deltas = Vec::new();
        for _ in 0..(cfg.confirm.natural_hold_sec + 120) {
            t += 1_000;
            e.neglect.p = 0.0;
            for ev in e.tick_1s(t, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) }).events {
                if let EmotionEvent::ColdLevelChanged { mood_delta, .. } = ev {
                    deltas.push(mood_delta);
                }
            }
            if e.neglect.level == 2 {
                break;
            }
        }
        assert_eq!(e.neglect.level, 2, "L3 只经自然消气通道降到 L2");
        let mood_at_l2 = e.state.values.mood;

        // ② L2 → L0：由 P 自然下降回退（`01 §6.11.8` 状态回退 ①），全程 mood_delta 必须为 0
        for _ in 0..40 {
            t += 1_000;
            e.neglect.p = 0.0;
            for ev in e.tick_1s(t, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) }).events {
                if let EmotionEvent::ColdLevelChanged { mood_delta, .. } = ev {
                    deltas.push(mood_delta);
                }
            }
            if e.neglect.level == 0 {
                break;
            }
        }
        assert_eq!(e.neglect.level, 0, "应逐层回落到 L0");
        assert!(deltas.iter().all(|d| *d == 0.0), "降级不得扣减 Mood：{deltas:?}");
        assert_eq!(e.state.values.mood, mood_at_l2, "降级不得退还也不得再扣");
    }

    /// S4-M4（`01 §6.5.2` / `02 §5.3`）：**L4 生气 / L5 离家出走必须走 CoaxFlow**，
    /// 正常可交互时不开放自然回退。
    #[test]
    fn l4_l5_require_coax_and_never_regress_naturally() {
        let cfg = cramped_cfg();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let mut t = 0i64;
        for i in 1..=600u64 {
            t = (i as i64) * 1000;
            e.tick_1s(t, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
            if e.neglect.level == 4 {
                break;
            }
        }
        assert_eq!(e.neglect.level, 4);
        // 压 P 回 0 后长跑：L4 必须原地不动。
        e.neglect.p = 0.0;
        for _ in 0..180 {
            t += 1000;
            e.tick_1s(t, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
        }
        assert_eq!(e.neglect.level, 4, "L4 不得自然回退（必须走 CoaxFlow）");

        // 继续升到 L5（先恢复累积），再压 P：L5 同样锁定。
        for i in 0..600u64 {
            t += (i as i64) * 1000;
            e.tick_1s(t, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
            if e.neglect.level == 5 {
                break;
            }
        }
        assert_eq!(e.neglect.level, 5);
        let l5_ran_at = t;
        e.neglect.p = 0.0;
        for _ in 0..180 {
            t += 1000;
            e.tick_1s(t, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
        }
        assert_eq!(e.neglect.level, 5, "L5 任何情况不开放自然回退");

        // L5 离家演出计时（`ACT-E-05` 约 6s）→ 离家。
        assert!(!e.is_runaway_away());
        e.coax_stroke_tick(l5_ran_at + RUNAWAY_PERFORMANCE_MS, false);
        assert!(e.is_runaway_away(), "演出满 6s 应离家（窗口隐藏）");
    }

    /// S4-M4：`force_lower`（设置 / 托盘「重置情绪」）强制解除 L5。
    #[test]
    fn force_lower_clears_l5_and_resets_level() {
        let cfg = cramped_cfg();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let mut t = 0i64;
        for i in 1..=600u64 {
            t = (i as i64) * 1000;
            e.tick_1s(t, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
            if e.neglect.level == 5 {
                break;
            }
        }
        assert_eq!(e.neglect.level, 5);
        e.coax_stroke_tick(t + RUNAWAY_PERFORMANCE_MS, false);
        assert!(e.is_runaway_away());

        let out = e.force_lower(t + RUNAWAY_PERFORMANCE_MS + 1_000);
        assert_eq!(e.neglect.level, 0, "L5 应被强制解除");
        assert_eq!(e.neglect.p, 0.0);
        assert!(!e.is_runaway_away(), "重置后应恢复可见");
        assert_eq!(e.state.emotion, EmotionState::Idle);
        assert!(
            out.iter().any(|ev| matches!(ev, EmotionEvent::ColdLevelChanged { to: 0, .. })),
            "应产出阶段迁移事件：{out:?}"
        );
        assert!(
            out.iter().any(|ev| matches!(ev, EmotionEvent::PersistNow)),
            "重置应请求落盘"
        );
    }

    /// S4-M3：完成三部曲 → `L4 → L3` + `Mood ≥ 50` + `ACT-E-06` + `P relief`。
    #[test]
    fn coax_success_lowers_level_and_restores_mood() {
        let cfg = cramped_cfg();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let mut t = 0i64;
        for i in 1..=600u64 {
            t = (i as i64) * 1000;
            e.tick_1s(t, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
            if e.neglect.level == 4 {
                break;
            }
        }
        assert_eq!(e.neglect.level, 4);
        let p_before = e.neglect.p;
        e.coax_input(CoaxInput::Call, t);
        assert_eq!(e.coax_step(), CoaxStep::Call);
        let mut ck = t;
        e.coax_stroke_tick(ck, true);
        for _ in 0..120 {
            ck += 100;
            e.coax_stroke_tick(ck, true);
            if e.coax_step() == CoaxStep::Heart {
                break;
            }
        }
        assert_eq!(e.coax_step(), CoaxStep::Heart, "抚摸 5s 应满进度环");
        let out = e.coax_input(CoaxInput::Heart, ck + 100);
        assert_eq!(e.neglect.level, 3, "L4 生气 → L3 生闷气（`01 §6.5.3`）");
        assert!(e.state.values.mood >= 50.0, "Mood 兜底 ≥50：{}", e.state.values.mood);
        assert!(e.neglect.p < p_before, "应扣减 relief.coax：{p_before} → {}", e.neglect.p);
        assert!(
            out.iter().any(|ev| matches!(ev, EmotionEvent::CoaxSucceeded { .. })),
            "应产出完成事件：{out:?}"
        );
        assert!(
            out.iter().any(|ev| matches!(ev, EmotionEvent::ForceAction { action_id, .. } if action_id == "ACT-E-06")),
            "应播 ACT-E-06：{out:?}"
        );
    }

    /// S4-M3：打断三部曲 → `P + rough.interruptPenalty`（B-6）。
    #[test]
    fn interrupting_trilogy_adds_pressure_penalty() {
        let cfg = cramped_cfg();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let mut t = 0i64;
        for i in 1..=600u64 {
            t = (i as i64) * 1000;
            e.tick_1s(t, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
            if e.neglect.level == 4 {
                break;
            }
        }
        e.coax_input(CoaxInput::Call, t);
        let mut ck = t;
        e.coax_stroke_tick(ck, true);
        for _ in 0..10 {
            ck += 100;
            e.coax_stroke_tick(ck, true);
        }
        let p_before = e.neglect.p;
        let out = e.coax_input(CoaxInput::Negative, ck + 10);
        let want = cfg.rough.interrupt_penalty as f32;
        assert!(
            (e.neglect.p - (p_before + want)).abs() < 1e-3,
            "打断应加 {want} P：{p_before} → {}",
            e.neglect.p
        );
        assert!(
            out.iter().any(|ev| matches!(ev, EmotionEvent::CoaxFailed { reason: CoaxFailReason::Interrupted })),
            "{out:?}"
        );
    }

    #[test]
    fn level_emotion_and_force_action_follow_config_pools() {
        let cfg = cramped_cfg();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let mut actions = Vec::new();
        for i in 1..=600u64 {
            for ev in e.tick_1s((i as i64) * 1000, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) }).events {
                if let EmotionEvent::ForceAction { action_id, .. } = ev {
                    actions.push(action_id);
                }
            }
            if e.neglect.level == 5 {
                break;
            }
        }
        // L2/L3/L4/L5 各下发一次 idlePool[0]（L1 不下发）
        assert_eq!(actions, vec!["ACT-E-02", "ACT-E-03", "ACT-E-04", "ACT-E-05"], "强制动作应取自配置 idlePool：{actions:?}");
        assert_eq!(e.state.emotion, EmotionState::Runaway);
    }

    #[test]
    fn l5_mood_lock_and_freezing_are_applied() {
        // 单独验证 L5 的 moodLockMax（cramped_cfg 已将其清零以免干扰，此处还原）
        let mut cfg = cramped_cfg();
        cfg.levels[5].mood_lock_max = 10;
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        for i in 1..=600u64 {
            e.tick_1s((i as i64) * 1000, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
            if e.neglect.level == 5 {
                break;
            }
        }
        assert_eq!(e.neglect.level, 5);
        assert!(e.state.values.mood <= 10.0, "L5 应锁 Mood 上限 10，实际 {}", e.state.values.mood);
    }

    #[test]
    fn night_redirect_switches_emotion_to_sleepy_hint() {
        let cfg = cramped_cfg();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(night()));
        let mut redirected_seen = false;
        for i in 1..=600u64 {
            for ev in e.tick_1s((i as i64) * 1000, TickEnv { preset_idle_ms: u64::MAX, ..env(night()) }).events {
                if let EmotionEvent::ColdLevelChanged { redirected, to: 2, .. } = ev {
                    redirected_seen = redirected;
                }
            }
            if redirected_seen {
                break;
            }
        }
        assert!(redirected_seen, "深夜进 L2 应标记 redirected（level={}）", e.neglect.level);
        assert_eq!(e.state.emotion, EmotionState::SleepyHint, "深夜应改播困倦提示");
    }

    #[test]
    fn daytime_same_level_is_not_redirected() {
        let cfg = cramped_cfg();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        for i in 1..=600u64 {
            e.tick_1s((i as i64) * 1000, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
            if e.neglect.level >= 2 {
                break;
            }
        }
        assert_eq!(e.neglect.level, 2);
        assert_eq!(e.state.emotion, EmotionState::Aggrieved, "白天应保持配置 emotion");
    }

    // ══════════════════════════════════════════════════════════════════
    // 在场 / 离场因子与迟滞
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn presence_away_lowers_rate_but_not_to_zero() {
        // FR-11-12 层 ①：不归零，避免「开穿透 / 挂机 = 永不生气」作弊
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        // S7-M4：首建性格随机（粘人度 45~55）会轻微缩放乘积；本用例断言「典型场景
        // product == 1.0」，故先钉住出厂五维（粘人 50 → personality 1.0 / adapt 1.0）。
        e.set_personality(Personality::from_cfg(&cfg.personality));
        e.tick_1s(0, env(day())); // 首拍建基线（不求解因子）
        e.tick_1s(1_000, env_here(day()));
        assert!(
            (e.neglect.rate_per_min - cfg.presence.factor_here).abs() < 1e-4,
            "在场速率应 = factorHere，实际 {}",
            e.neglect.rate_per_min
        );
        // 离场（idle 远超 180s），迟滞 15s 内仍按在场
        e.tick_1s(2_000, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
        assert!((e.neglect.rate_per_min - cfg.presence.factor_here).abs() < 1e-4, "迟滞未满应仍算在场");
        // 跨过迟滞（≥15s）→ 切离场
        e.tick_1s(20_000, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
        let away_rate = e.neglect.rate_per_min;
        assert!(away_rate > 0.0, "离场速率必须 > 0（不归零）");
        assert!((away_rate - cfg.presence.factor_away).abs() < 1e-4, "离场速率应 = factorAway，实际 {away_rate}");
        assert!(away_rate < cfg.presence.factor_here, "离场速率应显著低于在场");
    }

    #[test]
    fn interaction_unavailable_degrades_presence_factor() {
        // FR-11-12 层 ①：interaction_available = false → 与离场同档
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.set_personality(Personality::from_cfg(&cfg.personality));
        e.tick_1s(0, env_here(day()));
        e.tick_1s(1_000, TickEnv { interaction_available: false, ..env_here(day()) });
        let want = cfg.presence.factor_here * cfg.presence.factor_away;
        assert!((e.neglect.rate_per_min - want).abs() < 1e-4, "速率 {} != {want}", e.neglect.rate_per_min);
    }

    #[test]
    fn presence_hysteresis_prevents_flapping() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.set_personality(Personality::from_cfg(&cfg.personality));
        e.tick_1s(0, env_here(day()));
        e.tick_1s(1_000, env_here(day()));
        // 离场候选起计：迟滞 15s 内仍按在场
        e.tick_1s(2_000, TickEnv { preset_idle_ms: u64::MAX, ..env_here(day()) });
        e.tick_1s(5_000, TickEnv { preset_idle_ms: u64::MAX, ..env_here(day()) });
        assert!(
            (e.neglect.rate_per_min - cfg.presence.factor_here).abs() < 1e-4,
            "迟滞未满应仍在场，rate={}",
            e.neglect.rate_per_min
        );
        // 跨过 15s 迟滞 → 切离场
        e.tick_1s(20_000, TickEnv { preset_idle_ms: u64::MAX, ..env_here(day()) });
        assert!(
            (e.neglect.rate_per_min - cfg.presence.factor_away).abs() < 1e-4,
            "迟滞满应切离场，rate={}",
            e.neglect.rate_per_min
        );
        // 回归：同样需跨迟滞才切回在场
        e.tick_1s(21_000, env_here(day()));
        assert!(
            (e.neglect.rate_per_min - cfg.presence.factor_away).abs() < 1e-4,
            "回归迟滞未满不应立即切回"
        );
        e.tick_1s(40_000, env_here(day()));
        assert!(
            (e.factors().presence - cfg.presence.factor_here).abs() < 1e-4,
            "回归迟滞满应切回在场（presence 槽 = {}）",
            e.factors().presence
        );
        // 回迁同时开启「开机宽限」窗口（S7-M5）：rhythm × warmupFactor，
        // 故此时**总速率**低于 factorHere 属预期，宽限窗口结束后即恢复。
        assert!(e.in_warmup(), "回迁应当开启宽限窗口");
        // 容差 1e-3：本拍 `needs` 因子已因 satiety 自然衰减（−0.05/min）微微 >1.0。
        let want = cfg.presence.factor_here * cfg.rhythm.warmup_factor;
        assert!(
            (e.neglect.rate_per_min - want).abs() < 1e-3,
            "宽限期内速率 ≈ factorHere × warmupFactor（{want}），实际 {}",
            e.neglect.rate_per_min
        );
    }

    // ══════════════════════════════════════════════════════════════════
    // 敏感度（FR-11-11）
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn sensitivity_scales_rate_within_clamp() {
        let needs = NeedsConfig::default();
        // 低敏感度 0.7
        let mut cfg_low = EmotionConfig::default();
        cfg_low.sensitivity.value = 0.7;
        let mut low = EmotionEngine::new(&cfg_low, &needs);
        low.tick_1s(0, env(day()));
        for i in 1..=5u64 {
            low.tick_1s((i as i64) * 1000, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
        }
        // 高敏感度 1.3
        let mut cfg_high = EmotionConfig::default();
        cfg_high.sensitivity.value = 1.3;
        let mut high = EmotionEngine::new(&cfg_high, &needs);
        high.tick_1s(0, env(day()));
        for i in 1..=5u64 {
            high.tick_1s((i as i64) * 1000, TickEnv { preset_idle_ms: u64::MAX, ..env(day()) });
        }
        assert!(high.neglect.p > low.neglect.p, "高敏感度 P 应更大：{} vs {}", high.neglect.p, low.neglect.p);
    }

    #[test]
    fn sensitivity_clamp_is_enforced() {
        let s = Sensitivity { value: 100.0, rate_clamp: RateClamp { min: 0.5, max: 1.6 } };
        assert!((s.effective_rate(1.0) - 1.6).abs() < 1e-6, "上限钳制失效");
        let s2 = Sensitivity { value: 0.001, rate_clamp: RateClamp { min: 0.5, max: 1.6 } };
        assert!((s2.effective_rate(1.0) - 0.5).abs() < 1e-6, "下限钳制失效");
    }

    #[test]
    fn low_raw_rate_bypasses_clamp_floor_for_away_path() {
        // S4-M1 裁定（待核准）：离场 0.05 < rateClamp.min 时不得被抬到 0.5，
        // 否则 02 §5.5 的 RV-16 冻结算例（3h→P=9 / 24h→P=72<120）全部失效。
        let s = Sensitivity::default();
        // 直接钳制会抬起（证明冲突真实存在）
        assert!((s.effective_rate(0.05) - 0.5).abs() < 1e-6, "字面钳制确会抬到 0.5");
        // 未钳制通道保持原值
        assert!((s.effective_rate_raw(0.05) - 0.05).abs() < 1e-6, "未钳制通道应保持 0.05");
        // 统一裁决点：低 raw 走未钳制，高 raw 走钳制
        assert!((rate_from(&s, 0.05) - 0.05).abs() < 1e-6);
        assert!((rate_from(&s, 2.0) - 1.6).abs() < 1e-6);
        assert!((rate_from(&s, 1.0) - 1.0).abs() < 1e-6);
        // 敏感度仍作用于离场速率（0.05 × 0.7 / 1.3）
        let slow = Sensitivity { value: 0.7, rate_clamp: RateClamp { min: 0.5, max: 1.6 } };
        let fast = Sensitivity { value: 1.3, rate_clamp: RateClamp { min: 0.5, max: 1.6 } };
        assert!((slow.effective_rate_raw(0.05) - 0.035).abs() < 1e-6);
        assert!((fast.effective_rate_raw(0.05) - 0.065).abs() < 1e-6);
    }

    #[test]
    fn offline_rate_is_not_raised_by_clamp_floor() {
        // 端到端证据：离线速率 = 0.05（而非钳制下限 0.5），否则 RV-16 算例全部失效
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        e.offline_compensate(60 * MIN, env(day()));
        assert!(
            (e.neglect.rate_per_min - cfg.presence.factor_away).abs() < 1e-4,
            "离线速率应 = 0.05，实际 {}",
            e.neglect.rate_per_min
        );
        assert!((e.neglect.p - 3.0).abs() < 0.3, "1h 离线 P 应 ≈3，实际 {}", e.neglect.p);
    }

    // ══════════════════════════════════════════════════════════════════
    // 暂停 / 外出冻结 / 首拍基线 / 回拨防御 / 确定性
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn first_tick_establishes_baseline_without_accumulation() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        // 首拍哪怕 now_ms 很大，也不得把「启动前的空闲」算成冷落
        let out = e.tick_1s(1_700_000_000_000, env_here(day()));
        assert!(out.events.is_empty());
        assert_eq!(e.neglect.p, 0.0);
        assert_eq!(e.last_tick_ms, Some(1_700_000_000_000));
    }

    #[test]
    fn same_timestamp_tick_is_noop() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        e.tick_1s(1_000, env(day()));
        let p = e.neglect.p;
        let out = e.tick_1s(1_000, env(day()));
        assert!(out.events.is_empty());
        assert_eq!(e.neglect.p, p, "同时间戳不得重复累积");
    }

    #[test]
    fn backward_clock_does_not_accumulate_or_panic() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        e.tick_1s(10_000, env(day()));
        let p = e.neglect.p;
        // 回拨 5s：dt 被 max(0) 钳成 0 → 不累积
        e.tick_1s(5_000, env(day()));
        assert_eq!(e.neglect.p, p, "回拨不得产生负累积");
    }

    #[test]
    fn activity_running_freezes_accumulation() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        e.tick_1s(1_000, env(day()));
        let p = e.neglect.p;
        assert!(p > 0.0);
        for i in 2..=600u64 {
            e.tick_1s((i as i64) * 1000, TickEnv { activity_running: true, ..env(day()) });
        }
        assert_eq!(e.neglect.p, p, "外出期间 P 必须冻结（activity.freezeNeglectWhileOut）");
    }

    #[test]
    fn caps_are_respected() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        // 在场快进 1h：P 不得越过 capFree=120（1.0/min × 120min = 120 恰好触顶）
        // 在场 1.0/min：跑满 120min（7200 tick）恰好触顶 capFree=120
        for i in 1..=7200u64 {
            e.tick_1s((i as i64) * 1000, env_here(day()));
        }
        assert!(e.neglect.p <= cfg.busyness.cap_free as f32 + 1e-3, "P 越过 cap：{}", e.neglect.p);
        assert!((e.neglect.p - cfg.busyness.cap_free as f32).abs() < 1e-3, "应到达封顶 120，实际 {}", e.neglect.p);
        assert_eq!(e.neglect.cap, cfg.busyness.cap_free as f32);
    }

    #[test]
    fn tick_is_deterministic_for_same_input_sequence() {
        // S4-M2 验收前哨：同输入序列 → 同输出（无隐藏随机 / 无墙钟）
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let seq: Vec<(i64, bool)> = (0..600).map(|i| (i as i64 * 1000, i % 97 == 0)).collect();
        let run_once = || {
            let mut e = EmotionEngine::new(&cfg, &needs);
            let mut evs = Vec::new();
            for (t, paused) in &seq {
                evs.extend(e.tick_1s(*t, TickEnv { session_paused: *paused, ..env(day()) }).events);
            }
            (e.neglect.p, e.neglect.level, e.state.values.mood, evs)
        };
        let a = run_once();
        let b = run_once();
        assert_eq!(a.0, b.0);
        assert_eq!(a.1, b.1);
        assert_eq!(a.2, b.2);
        assert_eq!(a.3, b.3);
    }

    #[test]
    fn tick_always_ignores_wall_clock_when_now_ms_frozen() {
        // C3 纪律的实证：不注入 now_ms 前进 → 内核绝不推进（没有任何内部时钟）
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(1_000, env(day()));
        for _ in 0..100 {
            e.tick_1s(1_000, env(day()));
        }
        assert_eq!(e.neglect.p, 0.0, "now_ms 不前进则 P 恒为 0（证明内核零时钟）");
    }

    // ══════════════════════════════════════════════════════════════════
    // Mood 一阶低通惯性（K-5）
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn mood_follows_low_pass_without_jump() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env_here(day()));
        // 甩一个大的 event_delta 负值：单 tick 内 Mood 不得直接跳到 target
        let before = e.state.values.mood;
        let out = e.tick_1s(1_000, TickEnv { event_delta: -50.0, ..env_here(day()) });
        let after = e.state.values.mood;
        assert!(after < before, "应下降");
        assert!(before - after < 10.0, "单 tick 不得跳变（低通），实际 {before} -> {after}");
        assert!(out.events.iter().any(|ev| matches!(ev, EmotionEvent::ValuesChanged { .. })), "应产出 ValuesChanged");
    }

    #[test]
    fn mood_tau_down_is_faster_than_tau_up() {
        // tau_down=20s / tau_up=90s → 下降比上升更快
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();

        let mut down = EmotionEngine::new(&cfg, &needs);
        down.tick_1s(0, env_here(day()));
        for i in 1..=60u64 {
            down.tick_1s((i as i64) * 1000, TickEnv { event_delta: -20.0, ..env_here(day()) });
        }
        let drop = 60.0 - down.state.values.mood;

        let mut up = EmotionEngine::new(&cfg, &needs);
        up.state.values.mood = 30.0;
        up.tick_1s(0, env_here(day()));
        for i in 1..=60u64 {
            up.tick_1s((i as i64) * 1000, TickEnv { event_delta: 20.0, ..env_here(day()) });
        }
        let rise = up.state.values.mood - 30.0;
        assert!(drop > rise, "下降应快于上升：drop={drop} rise={rise}");
    }

    #[test]
    fn events_carry_persist_now_when_non_empty() {
        let mut cfg = EmotionConfig::default();
        cfg.thresholds.l1 = 0;
        cfg.confirm.up_sec = 0;
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let out = e.tick_1s(1_000, env(day()));
        assert!(!out.events.is_empty(), "应产生阶段变化事件");
        assert_eq!(out.events.last(), Some(&EmotionEvent::PersistNow), "非空事件须尾部附 PersistNow");
    }

    // ══════════════════════════════════════════════════════════════════
    // 离线补偿（RV-16）
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn offline_30min_is_just_left_and_no_decay() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let out = e.offline_compensate(30 * MIN, env(day()));
        assert_eq!(out, OfflineOutcome::JustLeft, "≤ graceMin(30) 应 JustLeft");
        assert_eq!(e.neglect.level, 0);
    }

    #[test]
    fn offline_3h_lands_l1() {
        // `02 §5.5` 口径表：3h → P = 0.05 × 180 = 9 → L1 无聊
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let out = e.offline_compensate(180 * MIN, env(day()));
        assert_eq!(out, OfflineOutcome::ColdApplied(1), "3h → P≈9 → L1，实际 P={}", e.neglect.p);
        assert!((e.neglect.p - 9.0).abs() < 0.6, "P 应 ≈9，实际 {}", e.neglect.p);
    }

    #[test]
    fn offline_4h_is_near_l1_l2_boundary_and_never_l5() {
        // `02 §5.5`：4h → P = 0.05 × 240 = 12 → L1~L2 临界，绝不 L5（RV-16 验证点）
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let out = e.offline_compensate(240 * MIN, env(day()));
        let lv = match out {
            OfflineOutcome::ColdApplied(l) => l,
            other => panic!("4h 应 ColdApplied，实际 {other:?}"),
        };
        assert!(lv <= 2, "4h 应为 L1~L2 临界，绝不 L5：{lv}（P={}）", e.neglect.p);
        assert!(e.neglect.p < cfg.thresholds.l3 as f32, "4h 的 P 应低于 L3=30，实际 {}", e.neglect.p);
    }

    #[test]
    fn offline_5h_is_longing() {
        // `02 §5.5`：5h → P = 0.05 × 300 = 15 → L2 委屈（Longing 展示）
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let out = e.offline_compensate(300 * MIN, env(day()));
        assert!(matches!(out, OfflineOutcome::Longing(_)), ">4h 应 Longing，实际 {out:?}");
        assert_eq!(e.state.emotion, EmotionState::Longing);
        assert!(e.is_longing());
        assert_eq!(e.neglect.level, 2, "5h → L2，实际 {}", e.neglect.level);
    }

    #[test]
    fn offline_24h_is_capped_at_l4_and_never_l5() {
        // RV-16 核心回归点：原 Bug 为 4h 直接落 L5
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let out = e.offline_compensate(24 * 60 * MIN, env(day()));
        let lv = match out {
            OfflineOutcome::Longing(l) => l,
            other => panic!("24h 应 Longing，实际 {other:?}"),
        };
        assert_eq!(lv, 4, "24h 应封顶 L4（min(4) 双保险），实际 {lv}");
        assert!(e.neglect.level <= 4, "离线永不触 L5");
        assert_ne!(e.state.emotion, EmotionState::Runaway, "离线不得进入离家出走");
    }

    #[test]
    fn offline_steps_are_bounded_by_max_sim_steps() {
        // 100h 离线：步数被 maxSimSteps(1440) 截断，且仍封顶 L4
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let out = e.offline_compensate(100 * 60 * MIN, env(day()));
        assert!(matches!(out, OfflineOutcome::Longing(_)));
        assert!(e.neglect.level <= 4);
        assert!(e.neglect.p < cfg.thresholds.l5 as f32, "步数截断后 P 不得越过 L5");
    }

    /// S5-M2 收口：步数被 `maxSimSteps`（24h）截断后，**残留间隙不得**被首个 `tick_1s` 全额计入。
    ///
    /// 反例（修复前）：100h 离线 → 只模拟 24h、锚点停在 `start + 24h`；首个 tick 的
    /// Δt = 76h → ΔP 远超 cap → P 冲 120 = L5 阈值 → **离家出走**，RV-16 被绕开。
    #[test]
    fn offline_truncated_gap_is_not_charged_to_next_tick() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));

        let away = 100 * 60 * MIN;
        e.offline_compensate(away, env(day()));
        let p_after = e.neglect.p;
        let level_after = e.neglect.level;
        assert!(level_after <= 4, "补偿后已封顶 L4，实际 L{level_after}");

        // 真实当下 = 起点 0 + away；首个业务 tick 的 Δt 必须归零（锚点已被推到当下）。
        e.tick_1s(away, env(day()));
        assert_eq!(e.neglect.p, p_after, "残留间隙必须被吞掉（首个 tick Δt 归零）");
        assert_eq!(e.neglect.level, level_after, "首个 tick 不得跳级");
        assert!(e.neglect.level <= 4, "RV-16：离线 + 首个 tick 全程不越 L4");
        assert_ne!(e.state.emotion, EmotionState::Runaway, "绝不离家出走");

        // 后续正常 tick 仍能累积（证明不是把时钟锚到未来把 P 永久冻住）。
        e.tick_1s(away + 60 * 60_000, env(day()));
        assert!(e.neglect.p >= p_after, "锚点不得超前于当下（否则 P 永久冻结）");
    }

    #[test]
    fn offline_does_not_touch_pause_window() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        e.offline_compensate(300 * MIN, env(day()));
        assert!(!e.state.pause.is_paused(), "离线补偿不是会话暂停");
        assert_eq!(e.state.pause.count, 0);
    }

    #[test]
    fn offline_uses_chunked_inertia_not_single_jump() {
        // 分块推进的证据：Mood 走低通而非一次性跳到 target。
        // 对照实验：把 tau 设为极大（=> alpha≈0，几乎不动）与设为 1s（=> alpha≈1，立即到位），
        // 两者在同样离线时长下应给出显著不同的 Mood —— 证明推进确实是「分块 + 惯性」而非「一步到位」。
        let needs = NeedsConfig::default();

        let mesure = |tau: u64| -> f32 {
            let mut cfg = EmotionConfig::default();
            cfg.inertia.tau_down_sec = tau;
            cfg.inertia.tau_up_sec = tau;
            let mut e = EmotionEngine::new(&cfg, &needs);
            e.tick_1s(0, env(day()));
            e.offline_compensate(240 * MIN, env(day()));
            e.state.values.mood
        };
        let sticky = mesure(10_000); // 惯性极大 → Mood 几乎不动
        let instant = mesure(1); // 惯性极小 → Mood 追平 target
        assert!(sticky > instant, "大 tau 应让 Mood 更滞后：sticky={sticky} instant={instant}");
        assert!(sticky - instant > 1.0, "两者应可区分（证明惯性参与）：{sticky} vs {instant}");
    }

    #[test]
    fn offline_does_not_raise_mood_above_start() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        let before = e.state.values.mood;
        e.offline_compensate(240 * MIN, env(day()));
        assert!(e.state.values.mood <= before + 1e-3, "离线不得升 Mood：{before} -> {}", e.state.values.mood);
    }

    // ══════════════════════════════════════════════════════════════════
    // 交互计数 / 日切 / 配置驱动
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn positive_interactions_are_counted() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        assert_eq!(e.positive_interactions(), 0);
        e.on_interaction(true, 1_000);
        e.on_interaction(true, 2_000);
        e.on_interaction(false, 3_000);
        assert_eq!(e.positive_interactions(), 2);
    }

    #[test]
    fn daily_roll_resets_counters_on_new_day() {
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        e.tick_1s(0, env(day()));
        e.on_interaction(true, 1_000);
        assert_eq!(e.positive_interactions(), 1);
        // 首个 tick 建立 today 基线
        e.tick_1s(1_000, env(day()));
        assert_eq!(e.state.today, "2026-09-14");
        // 次日：日切重置计数
        let next_day = Local.with_ymd_and_hms(2026, 9, 15, 10, 0, 0).unwrap();
        e.tick_1s(86_400_000, env_here(next_day));
        assert_eq!(e.state.today, "2026-09-15");
        assert_eq!(e.positive_interactions(), 0, "日切应重置交互计数");
    }

    #[test]
    fn engine_reads_all_thresholds_from_config() {
        let needs = NeedsConfig::default();
        let mut cfg = EmotionConfig::default();
        cfg.thresholds.l1 = 1;
        cfg.thresholds.l2 = 2;
        cfg.thresholds.l3 = 3;
        cfg.thresholds.l4 = 4;
        cfg.thresholds.l5 = 5;
        let e = EmotionEngine::new(&cfg, &needs);
        assert_eq!(e.level_for(0.5), 0);
        assert_eq!(e.level_for(1.0), 1);
        assert_eq!(e.level_for(2.0), 2);
        assert_eq!(e.level_for(3.0), 3);
        assert_eq!(e.level_for(4.0), 4);
        assert_eq!(e.level_for(5.0), 5);
        assert_eq!(e.level_for(1000.0), 5);
        // 反向：默认配置
        let default_cfg = EmotionConfig::default();
        let d = EmotionEngine::new(&default_cfg, &needs);
        assert_eq!(d.level_for(4.99), 0);
        assert_eq!(d.level_for(5.0), 1);
        assert_eq!(d.level_for(15.0), 2);
        assert_eq!(d.level_for(30.0), 3);
        assert_eq!(d.level_for(60.0), 4);
        assert_eq!(d.level_for(120.0), 5);
    }

    #[test]
    fn boredom_display_uses_l5_as_divisor() {
        let needs = NeedsConfig::default();
        let mut cfg = EmotionConfig::default();
        cfg.thresholds.l5 = 60; // 改分母
        let e = EmotionEngine::new(&cfg, &needs);
        assert!((e.boredom_display() - 0.0).abs() < 1e-6);
        let mut e2 = EmotionEngine::new(&cfg, &needs);
        e2.neglect.p = 60.0;
        assert!((e2.boredom_display() - 100.0).abs() < 1e-4, "分母应随 l5 变化");
    }

    #[test]
    fn threshold_scale_matches_config_formula() {
        // `02 §5.3`：threshold_scale = 1.2 − 0.4 × temper，作用于「实际比较值」，
        // 不改配置默认值。配置默认 temper = 0.55 → 0.98，属**预期**而非偏差。
        let cfg = EmotionConfig::default();
        let needs = NeedsConfig::default();
        let e = EmotionEngine::new(&cfg, &needs);
        let temper = cfg.personality.defaults.temper;
        let want = cfg.personality.threshold_scale_base - cfg.personality.threshold_scale_temper * temper;
        assert!((e.threshold_scale() - want).abs() < 1e-6, "应严格等于配置公式");

        // temper = 0.5（典型场景）时恰为 1.0 → 精确对齐 5/15/30/60/120
        let mut mid = EmotionConfig::default();
        mid.personality.defaults.temper = 0.5;
        let em = EmotionEngine::new(&mid, &needs);
        assert!((em.threshold_scale() - 1.0).abs() < 1e-6, "temper=0.5 应精确为 1.0");
    }

    #[test]
    fn temper_scales_thresholds_and_mood_delta() {
        let needs = NeedsConfig::default();
        // 高脾气 → 阈值比较值放大（更容易生气）、Mood 扣减更重
        let mut hot = EmotionConfig::default();
        hot.personality.defaults.temper = 1.0;
        let mut cold = EmotionConfig::default();
        cold.personality.defaults.temper = 0.0;
        let h = EmotionEngine::new(&hot, &needs);
        let c = EmotionEngine::new(&cold, &needs);
        assert!(h.threshold_scale() < c.threshold_scale(), "高脾气阈值缩放更小（更敏感）");
        assert!(h.mood_delta_scale() > c.mood_delta_scale(), "高脾气扣减更重");
    }

    #[test]
    fn emotion_state_parses_all_config_names() {
        let names = [
            ("Idle", EmotionState::Idle),
            ("Happy", EmotionState::Happy),
            ("Curious", EmotionState::Curious),
            ("Bored", EmotionState::Bored),
            ("Aggrieved", EmotionState::Aggrieved),
            ("Sulking", EmotionState::Sulking),
            ("Angry", EmotionState::Angry),
            ("Runaway", EmotionState::Runaway),
            ("Longing", EmotionState::Longing),
            ("Sleepy", EmotionState::Sleepy),
            ("Asleep", EmotionState::Asleep),
            ("Excited", EmotionState::Excited),
            ("Outing", EmotionState::Outing),
            ("SleepyHint", EmotionState::SleepyHint),
        ];
        for (s, want) in names {
            assert_eq!(EmotionState::from_cfg_name(s), want, "{s} 解析错误");
        }
        assert_eq!(EmotionState::from_cfg_name("Nope"), EmotionState::Idle, "未知应回退 Idle");
    }

    #[test]
    fn all_six_levels_parse_from_default_config() {
        let cfg = EmotionConfig::default();
        assert_eq!(cfg.levels.len(), crate::emotion::LEVEL_COUNT);
        let needs = NeedsConfig::default();
        let mut e = EmotionEngine::new(&cfg, &needs);
        // 六档遍历：每档 emotion 名都能解析为对应状态（配置↔枚举一致性）
        for (i, lv) in cfg.levels.iter().enumerate() {
            e.neglect.level = i as u8;
            assert_eq!(EmotionState::from_cfg_name(&lv.emotion), {
                let lvcfg = e.current_level_cfg();
                EmotionState::from_cfg_name(&lvcfg.emotion)
            });
        }
    }

    #[test]
    fn mood_drain_grows_superlinearly_with_p() {
        // drain = 2.0 × (P/120)^1.5 → P 越大单 tick 抽血越多
        let needs = NeedsConfig::default();

        let measure = |p: f32| -> f32 {
            let mut cfg = EmotionConfig::default();
            cfg.dimensions.mood.decay_per_min = 0.0;
            cfg.dimensions.mood.decay_per_min_active = 0.0;
            cfg.inertia.tau_down_sec = 1; // 消除惯性延迟以直接观察 drain
            cfg.inertia.tau_up_sec = 1;
            // 固定 P：用 activity_running 冻结累积
            let mut e = EmotionEngine::new(&cfg, &needs);
            e.tick_1s(0, env(day()));
            e.neglect.p = p;
            let before = e.state.values.mood;
            e.tick_1s(1_000, TickEnv { activity_running: true, ..env(day()) });
            before - e.state.values.mood
        };
        let low = measure(30.0);
        let high = measure(120.0);
        assert!(high > low * 4.0, "抽血应超线性：low={low} high={high}");
    }

    /// **S7-M3 起口径变更**：Mood 衰减乘子由**耦合矩阵**（C-01/C-02/C-06 取 max + 5s 平滑）
    /// 给出，不再是 S4-M1 的 `emotion.json.needs.coef` 线性占位——后者是七因子 `needs`
    /// 因子（`02 §5.2`，归 S7-M4）的量。本测试锁定「亏空 → 衰减更快」的定性方向仍成立，
    /// 且放大倍数与 `02 §5.10` 的矩阵系数一致（`satiety<=0` → ×2.5）。
    #[test]
    fn mood_decay_multiplier_follows_coupling_matrix() {
        let needs = NeedsConfig::default();
        let mut cfg = EmotionConfig::default();
        cfg.dimensions.mood.decay_per_min = 0.0;
        cfg.dimensions.mood.decay_per_min_active = -1.0;
        cfg.inertia.tau_down_sec = 1;
        cfg.inertia.tau_up_sec = 1;
        cfg.mood.drain_coef = 0.0;

        let measure = |sat: f32| -> (f32, f32) {
            let mut e = EmotionEngine::new(&cfg, &needs);
            e.tick_1s(0, env(day()));
            e.tick_1s(1_000, env(day())); // 建立基线（耦合快照取自内核数值）
            // 直接改内核六维数值：`state.values` 是耦合快照 S0 的唯一来源。
            e.state.values.satiety = sat;
            e.state.values.cleanliness = sat;
            let before = e.state.values.mood;
            // 30 个 1s tick：越过 5s 平滑窗口，使 `moodDecayMul` 收敛到矩阵真值。
            let mut t = 1_000i64;
            for _ in 0..30 {
                t += 1_000;
                e.tick_1s(t, env(day()));
            }
            (before - e.state.values.mood, e.coupling().mood_decay_mul)
        };
        let (full, full_mul) = measure(100.0); // 无亏空 → 无规则命中
        let (starved, starved_mul) = measure(0.0); // satiety=0 → C-02(2.5) 与 C-06(1.3) 取 max
        assert!((full_mul - 1.0).abs() < 1e-6, "无亏空乘子应为 1.0：{full_mul}");
        assert!((starved_mul - 2.5).abs() < 1e-6, "satiety<=0 → C-02 = 2.5：{starved_mul}");
        assert!(starved > full, "亏空应放大衰减：full={full} starved={starved}");
        assert!(full > 0.0, "基础自然衰减应生效");
    }
}

