//! `emotion::engine`：情绪 tick 内核（`02 §5 K-5`；S4-M1 骨架）。
//!
//! ## 时间纪律（C3）
//!
//! 本模块**零时钟**：所有时间点由调用方注入 `now_ms: i64`（墙钟毫秒，口径与
//! `dp-app` 的 `WallClock::now_ms()` 一致）。`now_local` 仅用于时段判定
//! （深夜重定向 / 日切），也由调用方经 [`TickEnv`] 注入。
//!
//! ## 本卡（S4-M1）口径
//!
//! 台账 P2-1 裁定：「结构占位 + `emotion.json` v2 冻结阈值简化判定，S7-M5 只换判定内核」。
//! 因此 [`EmotionEngine::tick_1s`] 保留 `02 §5.2` 的**六步结构**（暂停 → 速率 → ΔP →
//! 自然消气计量 → 阶段结算 → Mood 惯性），但**第 1 步的七因子求解是简化版**：
//! 速率 = 在场因子 × 敏感度钳制（见 [`FactorProduct::from_env`]），真实七因子归 S7-M4。
//! 第 3~6 步已按 `02` 冻结口径完整实装，S7-M5 只需替换第 1~2 步的速率来源，其余零改动。
//!
//! ## 禁止顺手改动
//!
//! 不引入七因子求解器（S7-M4）；不接台词 / 气泡（S4-M5）。`EmotionEvent` 是
//! **内核算法的返回值**，不是 Tauri 事件。
//!
//! ## S4-M3 / S4-M4 增量
//!
//!   - 交互缓解表 `relief.*` + `cooldownSec` + `strokeMaxPerWindow`（FR-11-7）；
//!   - 道歉三部曲接线（[`EmotionEngine::coax_input`] / [`EmotionEngine::coax_stroke_tick`]）
//!     + 进度环事件（`CoaxProgress/Succeeded/Failed`）；
//!   - **L4/L5 强制地板**：不开放自然回退，只能走 CoaxFlow（`01 §6.5.2`）；
//!   - [`EmotionEngine::force_lower`] 兜底（`02 §5.23` R18）。

use chrono::{Datelike, Local, Timelike};

use crate::config::model::{EmotionLevelCfg, MoodDimCfg};
use crate::emotion::coax::{
    CoaxEffect, CoaxFailReason, CoaxFlow, CoaxInput, CoaxStep, COAX_REQUIRED_MIN_LEVEL,
    COAX_SUCCESS_ACTION, COAX_SUCCESS_PRIORITY,
};
use crate::interaction::router::InteractionKind;
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

/// 每 tick 由调用方组装的不可变环境快照（`02 §4.2` 精简版 · S4-M1 骨架）。
///
/// 与 `02 §4.2` 完整版相比，本卡只保留骨架必需字段（暂停 / 在场 / 交互净增益 /
/// 目的地时段 / 外出冻结）；七因子原始输入（击键强度、前台哈希、全屏等）归 S7-M4，
/// 此处以 `preset_idle_ms` 统一承载「在场判定」这一 S4 必需信息。
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
    /// 用户空闲毫秒（在场判定：≥ `presence.awayThresholdSec` 判离场 /  hysteresis 迟滞）。
    pub preset_idle_ms: u64,
    /// 交互可达（穿透 / 钩子卸载 / 勿扰时为 false，FR-11-12 三层防护 ①）。
    pub interaction_available: bool,
    /// 需求快照：饱食度。
    pub satiety: f32,
    /// 需求快照：清洁度。
    pub cleanliness: f32,
    /// 生命周期占位（保持 `TickEnv<'a>` 与 `02` 签名同构，S7-M4 填入 `&InputIntensity`）。
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
            _marker: core::marker::PhantomData,
        }
    }
}

/// S4-M1 简化因子乘积来源（**替换点**：S7-M4 换成真实七因子求解器）。
///
/// S4-M1 口径 = `presence`（在场 / 离场）× 交互可达性（FR-11-12 层 ①）；
/// `busyness` / `personality` / `rhythm` / `needs` / `rough` / `adapt` 恒 `1.0`
/// （即「因子未引入」的显式表达，避免隐式默认值散落）。
#[derive(Clone, Copy, Debug)]
struct FactorProduct {
    presence: f32,
    interaction: f32,
}

impl FactorProduct {
    /// 计算原始乘积（未加敏感度）。
    #[inline]
    fn raw(&self) -> f32 {
        self.presence * self.interaction
    }

    /// 从环境快照求解（S4-M1 简化版）。
    fn from_env(env: &TickEnv<'_>, engine: &mut EmotionEngine, now_ms: i64) -> (Self, bool) {
        let presence = engine.presence_factor(env, now_ms);
        // FR-11-12 层 ①：交互不可达时同档「不在场」（不归零，避免"开穿透=永不生气"作弊）。
        let interaction = if env.interaction_available {
            1.0
        } else {
            engine.cfg.presence.factor_away.max(0.0)
        };
        (Self { presence, interaction }, presence > engine.cfg.presence.factor_away)
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

/// 在场判定迟滞状态（`presence.hysteresisSec`）。
#[derive(Clone, Copy, Debug, Default)]
struct PresenceLatch {
    /// 当前判定为在场。
    here: bool,
    /// 判定翻转的候选起始时刻。
    since_ms: Option<i64>,
}

impl PresenceLatch {
    fn new() -> Self {
        Self { here: true, since_ms: None }
    }
}

/// 情绪内核（`02 §4.1` `EmotionEngine`，S4-M1 骨架范围）。
///
/// 持有：配置（借用）、数值状态、P 与阶段、因子、自适应基线、简化因子来源。
/// **不持有**：台词 / 气泡 / 道歉三部曲 / 事件总线（分别归 S4-M5 / S4-M3 / S4-M2）。
pub struct EmotionEngine<'c> {
    cfg: &'c crate::config::model::EmotionConfig,
    needs_cfg: &'c crate::config::model::NeedsConfig,
    /// 六维数值 + 暂停窗口 + 情绪展示态。
    pub state: PetState,
    /// 冷落压力与阶段。
    pub neglect: NeglectPressure,
    /// 敏感度。
    pub sensitivity: Sensitivity,
    /// 当前因子乘积（简化版；供快照 / 原因卡）。
    factors: FactorProduct,
    /// 在场迟滞。
    presence_latch: PresenceLatch,
    /// 日常基线（S7-M4 自适应基线的占位：恒 1.0 = 不缩放）。
    adapt_factor: f32,
    /// 性格「脾气」维（影响阈值比较值与 Mood 扣减幅度；完整五维归 S7-M4）。
    temper: f32,
    /// 上次 tick 的墙钟毫秒（`None` = 尚未 tick）。
    last_tick_ms: Option<i64>,
    /// 上次 tick 注入的本地时间（**不读时钟**，仅缓存上游经 `WallClock` 取到的值；
    /// 供 `settle_level` 的深夜重定向判定使用，C3）。
    last_now_local: chrono::DateTime<Local>,
    /// 离线补偿期间是否处于「想念」展示（供快照）。
    longing: bool,
    /// 今日净正向交互数（自然消气条件②的计量，S4-M1 只维护不消费）。
    positive_interactions: u32,
    /// 道歉三部曲状态机（S4-M3 / S4-M4）。
    coax: CoaxFlow,
    /// 交互缓解冷却与抚摸窗口（S4-M3）。
    relief: ReliefTracker,
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
            factors: FactorProduct { presence: cfg.presence.factor_here, interaction: 1.0 },
            presence_latch: PresenceLatch::new(),
            adapt_factor: 1.0,
            temper: cfg.personality.defaults.temper,
            last_tick_ms: None,
            last_now_local: chrono::Local::now(),
            longing: false,
            positive_interactions: 0,
            coax: CoaxFlow::new(),
            relief: ReliefTracker::default(),
        }
    }

    /// 由存档 / 恢复状态构造（S5 存档接入前供测试与离线补偿复用）。
    pub fn with_state(
        cfg: &'c crate::config::model::EmotionConfig,
        needs_cfg: &'c crate::config::model::NeedsConfig,
        state: PetState,
        neglect: NeglectPressure,
        last_tick_ms: i64,
    ) -> Self {
        let mut e = Self::new(cfg, needs_cfg);
        e.state = state;
        e.neglect = neglect;
        e.last_tick_ms = Some(last_tick_ms);
        e
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

    /// 只读：展示态（`02 §4.3`；`pet://state` 投影用）。
    #[inline]
    pub fn emotion_state(&self) -> EmotionState {
        self.state.emotion
    }

    /// 七因子投影（`02 §4.3` `neglect.factors`，S4-M2 起供 `pet://state`）。
    ///
    /// **S4-M1 口径说明**：内核当前只引入 `presence`（含 FR-11-12 层① 的「交互可达性」
    /// 折扣——`02 §5.23` 把不可达折进 `presenceFactor=0.05`，**不是**独立因子）
    /// 与 `adapt`（S4-M1 恒 1.0）；其余五项（`busyness` / `personality` / `rhythm` /
    /// `needs` / `rough`）恒 `1.0` —— 这是「因子未引入」的**显式表达**
    /// （`02 §4.2` 完整七因子归 S7-M4）。
    ///
    /// `presence` 槽 = 内核 `presence × interaction` 的**有效覆盖因子**（即把不可达
    /// 折扣合入在场因子），`product` 与之一致——保证前端由
    /// `product == presence × busyness × … × adapt` 反算时自洽。
    #[must_use]
    pub fn factors_snapshot(&self) -> crate::event::FactorsSnapshot {
        let product = self.factors.raw() * self.adapt_factor;
        crate::event::FactorsSnapshot {
            presence: self.factors.presence * self.factors.interaction,
            busyness: 1.0,
            personality: 1.0,
            rhythm: 1.0,
            needs: 1.0,
            rough: 1.0,
            adapt: self.adapt_factor,
            product,
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

    /// 在场判定（含迟滞）。
    ///
    /// `preset_idle_ms >= awayThresholdSec×1000` 判离场，需持续 `hysteresisSec` 才翻转，
    /// 避免临界抖动导致 P 速率噪声。
    fn presence_factor(&mut self, env: &TickEnv<'_>, now_ms: i64) -> f32 {
        self.presence_latch.here = presence_decide(
            &mut self.presence_latch,
            env.preset_idle_ms,
            self.cfg.presence.away_threshold_sec,
            self.cfg.presence.hysteresis_sec,
            now_ms,
        );
        if self.presence_latch.here {
            self.cfg.presence.factor_here
        } else {
            self.cfg.presence.factor_away
        }
    }

    /// 阶段阈值比较值（`02 §5.3`：`P_eff = P / threshold_scale`）。
    ///
    /// `threshold_scale = 1.2 − 0.4 × temper`，`temper=0.5` 时精确等于 1.0，
    /// 保证典型场景对齐 5/15/30/60/120。
    #[inline]
    fn threshold_scale(&self) -> f32 {
        let s = self.cfg.personality.threshold_scale_base
            - self.cfg.personality.threshold_scale_temper * self.temper;
        if s > 0.0 {
            s
        } else {
            1.0
        }
    }

    /// Mood 扣减幅度缩放（`02 §5.3`：`mood_delta_scale = 0.8 + 0.4 × temper`）。
    #[inline]
    fn mood_delta_scale(&self) -> f32 {
        self.cfg.personality.mood_delta_base + self.cfg.personality.mood_delta_temper * self.temper
    }

    /// 需求衰减放大（S4-M1 简化：「亏空」按 satiety/cleanliness 的缺口线性放大量）。
    ///
    /// 完整口径（`needs.coef × max(0, (deficitRef − min(satiety,cleanliness))/deficitRef)`）
    /// 归 S4-M2+S7；本卡只保证「漏了/脏了心情掉得快」的定性方向，且系数全部取自配置。
    fn needs_mood_decay_mul(&self, env: &TickEnv<'_>) -> f32 {
        let ref_v = self.cfg.needs.deficit_ref;
        if ref_v <= 0.0 {
            return 1.0;
        }
        1.0 + self.cfg.needs.coef * ((ref_v - env.satiety.min(env.cleanliness)) / ref_v).max(0.0)
    }

    /// 阶段判定：`P_eff` → 目标档位（`02 §5.3` 冻结阈值表）。
    fn level_for(&self, p_eff: f32) -> u8 {
        let t = &self.cfg.thresholds;
        if p_eff >= t.l5 as f32 {
            5
        } else if p_eff >= t.l4 as f32 {
            4
        } else if p_eff >= t.l3 as f32 {
            3
        } else if p_eff >= t.l2 as f32 {
            2
        } else if p_eff >= t.l1 as f32 {
            1
        } else {
            0
        }
    }

    /// 1Hz 业务 tick（`02 §5.2` 六步结构的 S4-M1 实装）。
    ///
    /// 返回值：[`TickOutcome`]（算法事件 + 暂停迁移标志）。
    pub fn tick_1s(&mut self, now_ms: i64, env: TickEnv<'_>) -> TickOutcome {
        // 暂停观察（P2-2：起止时间戳入状态；幂等）
        let pause_changed = self.state.pause.observe(env.session_paused, now_ms);

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
            return TickOutcome { events: vec![], pause_changed };
        }

        // ── 1) 因子求解（S4-M1 简化版；S7-M4 只替换这里）────────────────────
        let (factors, _here) = FactorProduct::from_env(&env, self, now_ms);
        self.factors = factors;
        let raw = factors.raw() * self.adapt_factor;
        self.neglect.rate_per_min = rate_from(&self.sensitivity, raw);

        // ── 2) ΔP 累积 + 忙碌封顶（外出期间冻结，`emotion.activity`）────────
        if !env.activity_running {
            let cap = self.cfg.busyness.cap_free as f32;
            self.neglect.cap = cap;
            self.neglect.p = (self.neglect.p + dt_min * self.neglect.rate_per_min).clamp(0.0, cap);
        }

        // ── 3~4) 阶段结算（60s 升级 / 30s 回退 / 不跳级）────────────────────
        let mut events = self.settle_level(now_ms);

        // ── 4.5) 三部曲阶段同步（S4-M3/S4-M4：L5 离家演出触发 / 档位回落清空）──
        // 必须在 `settle_level` 之后：本 tick 刚进入 L5 时要立刻开始离家演出计时。
        let level_now = self.neglect.level;
        let coax_effects = self.coax.sync_level(level_now, now_ms);
        events.extend(self.apply_coax_effects(coax_effects));

        // ── 5) Mood 一阶低通惯性 ────────────────────────────────────────────
        events.extend(self.step_mood(dt_min, env.event_delta, &env));

        // ── 6) 日切（自然消气计量与正向计数的重置）──────────────────────────
        events.extend(self.daily_roll(&env));

        // 展示态投影：L0 且非想念 → 依 phase 保持 Idle（S4-M2 接 Happy/Curious 等）
        if self.neglect.level == 0 && self.state.emotion == EmotionState::Idle {
            self.positive_interactions = self.positive_interactions.saturating_add(0);
        }

        let events = dedup_events(events);
        if !events.is_empty() {
            let mut e = events;
            e.push(EmotionEvent::PersistNow);
            return TickOutcome { events: e, pause_changed };
        }
        TickOutcome { events, pause_changed }
    }

    /// 阶段结算（`02 §5.3` 冻结口径）：确认期 + 不跳级 + 逐层扣 Mood。
    fn settle_level(&mut self, now_ms: i64) -> Vec<EmotionEvent> {
        let p_eff = self.neglect.p / self.threshold_scale();
        let mut target = self.level_for(p_eff);
        // S4-M4（`01 §6.5.2` / `02 §5.3`）：**L4 生气 / L5 离家出走必须走 CoaxFlow**，
        // 正常可交互时不开放自然回退（「任何降级路径不得产生负向扣减」R18-4 不受影响：
        // 此处只是**阻止**降级，不做任何扣减）。`confirm.naturalFloorLevel` 的完整口径
        // （含 L3 自然消气通道）归 S7-M5；本卡先锁 L4/L5 强制地板。
        if target < self.neglect.level && self.neglect.level >= COAX_REQUIRED_MIN_LEVEL {
            target = self.neglect.level;
        }
        if target == self.neglect.level {
            self.neglect.pending_since_ms = None;
            self.neglect.pending_level = self.neglect.level;
            return vec![];
        }

        let need_ms = if target > self.neglect.level {
            self.cfg.confirm.up_sec
        } else {
            self.cfg.confirm.down_sec
        } as i64
            * 1000;

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
            let redirected = up
                && def.level >= self.cfg.rhythm.night_redirect_from_level
                && is_night(&self.cfg.rhythm, &self.state_reference_now_local(now_ms));
            if def.mood_lock_max > 0 {
                self.state.values.mood = self.state.values.mood.min(def.mood_lock_max as f32);
            }
            self.state.emotion = if redirected {
                EmotionState::SleepyHint
            } else {
                EmotionState::from_cfg_name(&def.emotion)
            };
            out.push(EmotionEvent::ColdLevelChanged {
                from: self.neglect.level,
                to: next,
                mood_delta: delta,
                redirected,
                reason: ColdReason::Accumulate,
            });
            if up && def.level >= 2 && !redirected {
                if let Some(first) = def.idle_pool.first() {
                    out.push(EmotionEvent::ForceAction {
                        action_id: first.clone(),
                        priority: def.priority_floor.min(255) as u8,
                    });
                }
            }
            self.neglect.level = next;
        }
        self.neglect.pending_level = self.neglect.level;
        out
    }

    /// 深夜判定所需的 `now_local`：取最近一次 tick 缓存的注入值。
    ///
    /// C3 说明：本方法**不读时钟**；`now_local` 由调用方经 `WallClock::now_local()`
    /// 取到后随 [`TickEnv`] 注入，此处只做缓存读取。
    #[inline]
    fn state_reference_now_local(&self, _now_ms: i64) -> chrono::DateTime<Local> {
        self.last_now_local
    }

    /// Mood 一阶低通惯性（`02 §5 K-5` `step_mood`）。
    fn step_mood(&mut self, dt_min: f32, event_delta: f32, env: &TickEnv<'_>) -> Vec<EmotionEvent> {
        let m: &MoodDimCfg = &self.cfg.dimensions.mood;
        let ref_p = self.cfg.mood.drain_ref_p;
        let p_ratio = if ref_p > 0.0 { (self.neglect.p / ref_p).max(0.0) } else { 0.0 };
        let drain = self.cfg.mood.drain_coef * p_ratio.powf(self.cfg.mood.drain_exp);
        // S4-M1 骨架：`is_active()` 判定依赖活跃度感知（S7-M4），本卡统一取
        // `decayPerMinActive`；S7-M4 接入后按 `is_active()` 在两档间切换即可。
        let decay = m.decay_per_min_active.abs();
        let needs_mul = self.needs_mood_decay_mul(env);
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

    /// 日切（自然消气计量与正向交互计数重置；数值日切归 S4-M2）。
    fn daily_roll(&mut self, env: &TickEnv<'_>) -> Vec<EmotionEvent> {
        let today = date_key(&env.now_local);
        if self.state.today.is_empty() {
            self.state.today = today;
            return vec![];
        }
        if self.state.today != today {
            self.state.today = today;
            self.positive_interactions = 0;
            self.longing = false;
            return vec![];
        }
        vec![]
    }

    /// 交互回调（S4-M1 只做最小记账：正向交互计数 + 事件净增益入口）。
    ///
    /// 完整缓解表（`relief.hover/click/…`）与冷却（`cooldownSec`）归 S4-M2 / S4-M3；
    /// 本卡只维护自然消气条件②所需的「净正向交互计数」。
    pub fn on_interaction(&mut self, positive: bool, now_ms: i64) -> TickOutcome {
        self.last_tick_ms.get_or_insert(now_ms);
        if positive {
            self.positive_interactions = self.positive_interactions.saturating_add(1);
        }
        TickOutcome::default()
    }

    // -----------------------------------------------------------------------
    // S4-M3 / S4-M4：交互缓解表 + 道歉三部曲 + force_lower
    // -----------------------------------------------------------------------

    /// 设置「轻松模式」（`01 §6.5.2`；降低抚摸门槛至 `easyModeStrokeSec`）。
    /// 设置项接线归 S5；此处只透传。
    pub fn set_easy_coax_mode(&mut self, on: bool) {
        self.coax.set_easy_mode(on);
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

    /// 交互意图统一入口（S4-M3）：缓解表 → 三部曲推进 → 正向计数。
    ///
    /// `InteractionKind` 到三条通路的映射（唯一映射点，避免上层重复分支）：
    ///   - 缓解：Hover/Click/DoubleClick/Stroke/Feed/Bath、轨迹彩蛋 Circle/Line/Zigzag → Play；
    ///   - 三部曲：Click → 呼唤；DoubleClick → 比心；Tickle/Throw → **打断**（负向）；
    ///     `TrayCoax` → 呼唤（`02 §5.23` 第 2 层托盘替代入口）；
    ///   - 正向计数：有缓解语义的交互计入（自然消气条件②，消费归 S7-M5）。
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
        if let Some(rk) = relief_kind {
            let relief = self.relief_for(rk, now_ms);
            self.apply_relief(relief);
            self.positive_interactions = self.positive_interactions.saturating_add(1);
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
        self.last_tick_ms = Some(t);
        self.state.last_tick_ms = t;

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
        // 因子：不在场；`interaction_available` 保持与离线前一致（离线期间无交互概念，
        // 但 FR-11-12 层①的「不可达」状态在用户离开前后应连续，避免回归瞬间速率跳变）。
        self.factors = FactorProduct {
            presence: self.cfg.presence.factor_away,
            interaction: if env.interaction_available {
                1.0
            } else {
                self.cfg.presence.factor_away.max(0.0)
            },
        };
        self.neglect.rate_per_min = rate_from(&self.sensitivity, self.factors.raw() * self.adapt_factor);
        // 离线期间的忙碌档归 `busyness_level`（S7-M4 引入）；S4-M1 只使用 `capFree`，
        // 与 `tick_1s` 保持同一口径，保证 P 上界可复算（RV-16 的 `P = 0.05 × 分钟数`）。
        let cap = self.cfg.busyness.cap_free as f32;
        self.neglect.cap = cap;
        self.neglect.p = (self.neglect.p + dt_min * self.neglect.rate_per_min).clamp(0.0, cap);
        let _ = self.settle_level(now_ms);
        let _ = self.step_mood(dt_min, 0.0, &env);
        // 离线期间不推进日切（S4-M2 用真实 now_local 处理跨日）
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

/// 在场判定（迟滞实现，纯函数 + 就地更新）。
fn presence_decide(
    latch: &mut PresenceLatch,
    idle_ms: u64,
    away_threshold_sec: u64,
    hysteresis_sec: u64,
    now_ms: i64,
) -> bool {
    let away_ms = away_threshold_sec.saturating_mul(1000);
    let hyst_ms = hysteresis_sec.saturating_mul(1000) as i64;
    let idle = if idle_ms == u64::MAX { away_ms + 1 } else { idle_ms };
    let want_here = idle < away_ms;
    if want_here == latch.here {
        latch.since_ms = None;
        return latch.here;
    }
    let since = *latch.since_ms.get_or_insert(now_ms);
    if now_ms - since >= hyst_ms {
        latch.here = want_here;
        latch.since_ms = None;
    }
    latch.here
}

/// 深夜判定（`rhythm.segments` 中 id == "night" 的时段；配置驱动）。
fn is_night(rhythm: &crate::config::model::RhythmCfg, now_local: &chrono::DateTime<Local>) -> bool {
    let hm = now_local.hour() * 60 + now_local.minute();
    for seg in &rhythm.segments {
        if seg.id != "night" {
            continue;
        }
        if let (Some(from), Some(to)) = (parse_hm(&seg.from), parse_hm(&seg.to)) {
            return if from <= to { hm >= from && hm < to } else { hm >= from || hm < to };
        }
    }
    false
}

/// `"HH:mm"` → 当日分钟数。
fn parse_hm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h < 24 && m < 60 {
        Some(h * 60 + m)
    } else {
        None
    }
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
        let mood_at_l3 = e.state.values.mood;

        // 直接压 P 回 0 → 逐层回落，全程 mood_delta 必须为 0
        e.neglect.p = 0.0;
        let mut deltas = Vec::new();
        for _ in 0..40 {
            t += 1000;
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
        assert_eq!(e.state.values.mood, mood_at_l3, "降级不得退还也不得再扣");
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
            (e.neglect.rate_per_min - cfg.presence.factor_here).abs() < 1e-4,
            "回归迟滞满应切回在场"
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

    #[test]
    fn needs_decay_multiplier_follows_config_coef() {
        let needs = NeedsConfig::default();
        let mut cfg = EmotionConfig::default();
        cfg.dimensions.mood.decay_per_min = 0.0;
        cfg.dimensions.mood.decay_per_min_active = -1.0;
        cfg.inertia.tau_down_sec = 1;
        cfg.inertia.tau_up_sec = 1;
        cfg.mood.drain_coef = 0.0;

        let measure = |sat: f32| -> f32 {
            let mut e = EmotionEngine::new(&cfg, &needs);
            e.tick_1s(0, env(day()));
            e.tick_1s(1_000, env(day())); // 建立非零 rate 基线
            let before = e.state.values.mood;
            let mut env2 = env(day());
            env2.satiety = sat;
            env2.cleanliness = sat;
            // 用一个较大 Δt 让衰减可见（30 个 1s tick 累计）
            let mut t = 1_000i64;
            for _ in 0..30 {
                t += 1_000;
                e.tick_1s(t, env2);
            }
            before - e.state.values.mood
        };
        let full = measure(100.0); // 无亏空
        let starved = measure(0.0); // 满亏空
        assert!(starved > full, "亏空应放大衰减：full={full} starved={starved}");
        assert!(full > 0.0, "基础自然衰减应生效");
    }
}

