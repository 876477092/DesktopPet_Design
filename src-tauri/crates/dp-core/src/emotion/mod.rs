//! `emotion`：情绪数值、六维状态机与暂停累积内核（`02 §5 K-5` / §5.1~5.5；FR-5 / FR-11）。
//!
//! 模块分工（`02 §5.1`）：
//!   - [`engine`] —— 主 tick 编排：因子 → ΔP → 阶段结算 → Mood 惯性 → 日切；
//!   - [`coax`] —— 道歉三部曲 `CoaxFlow`（含 L5 离家找回 / `force_lower` / 托盘输入，
//!     **S4-M3 / S4-M4** 交付）；
//!   - [`lines`] —— 台词池抽取、冷却、占位符渲染、气泡计划与口头禅闸门
//!     （**S4-M5** 交付；口头禅**改写**与台词库重写归 S7-M8）。
//!
//! ## 本卡交付边界（S4-M3 / S4-M4）
//!
//!   1. 三部曲状态机（呼唤 → 抚摸 `strokeSec` → 比心）与进度环数据；
//!   2. 中断回退（`interruptRollbackRatio` 保留部分进度 + `rough.interruptPenalty`）；
//!   3. 完成：`Mood = max(Mood, 50)` + 档位目标 + `ACT-E-06`（`P relief` 取 `relief.coax`）；
//!   4. L5 离家演出 / 找回走回 / `force_lower` 兜底（L5 不开放自然回退）。
//!
//! ## 本卡交付边界（S4-M5）
//!
//!   1. `LinesLibrary`：`resources/config/lines.json` 加载 + 与 `character.json`
//!      交叉校验（`linePools.count=13` / 每池 ≥6 条 / 口头禅分布，L-02）；
//!   2. `LineSelector`：同池抽取冷却（`selector.cooldownSec`，冷却未过不刷新）；
//!   3. `render_placeholders`：`{name}`/`{user}`/`{coin}`/`{item}` 渲染（C2，未命中原样保留）；
//!   4. `BubblePlanner` + `PlannedBubble`：`pet://bubble` 语义中继（含求助气泡
//!      3min 间隔 + 拒绝翻倍 ≤15min 退避）；
//!   5. `CatchphraseGate` + `CatchphraseFrequency`：口头禅**闸门接口预留**（枚举档位 L-03）。
//!
//! ## 与 S7 的切割（禁止顺手改动）
//!
//!   - **七因子求解**（`presence` / `busyness` / `personality` / `rhythm` / `needs` /
//!     `rough` / `adapt`）在 **S7-M4** 落地，实现见 [`solver`] 与六个因子子模块；
//!   - **Mood 惯性 / 阶段状态机 / 自然消气 / 深夜重定向** 在 **S7-M5** 落地
//!     （[`neglect`] 承载计量器，[`engine`] 承载编排）；
//!   - **敏感度滑杆与交互死锁三层防护（托盘入口闭环）** 在 **S7-M6** 落地；
//!   - **口头禅改写 / 台词库重写 / 命名接入设置页** 归 S7-M8：S4-M5 只交付闸门接口与
//!     骨架内容；`positionBias` 位置偏置与「无口头禅变体改写」不在 S4 卡。
//!
//! ## S7-M4 交付拼接（子模块与 `02 §5.1` 文件清单的对应）
//!
//!   - [`presence`] → `neglect`/`presence` 中的在场因子（`02 §5.1` 的 `emotion/presence.rs`）；
//!   - [`busyness`] → `emotion/busyness.rs`；
//!   - [`rhythm`] → `emotion/rhythm.rs`（时段因子由 S7-M1 的 `perception::time::TimeRhythm` 提供）；
//!   - [`personality`] → `emotion/personality.rs`；
//!   - [`rough`] → `emotion/rough.rs`；
//!   - [`adapt`] → `emotion/adapt.rs`（**`03 §2 S7-M4` 交付物清单漏列该文件**，
//!     但七因子含 `adapt` 且 `02 §5.1` 明确列出 `emotion/adapt.rs` ⇒ 按 `02` 交付并登记）；
//!   - [`neglect`] → `emotion/neglect.rs`（P 累积 / 封顶 / 阈值 / 两条保持窗口计时器）；
//!   - [`solver`] → `emotion/solver.rs`（固定顺序快照求解）。

pub mod adapt;
pub mod busyness;
pub mod coax;
pub mod engine;
pub mod lines;
pub mod neglect;
pub mod personality;
pub mod presence;
pub mod rhythm;
pub mod rough;
pub mod solver;
pub mod sys_env;

pub use adapt::{AdaptEvent, AdaptationState, DailySample};
pub use busyness::{
    BUSYNESS_LEVEL_COUNT, BusynessLevel, BusynessOutput, BusynessSolver, BusynessSmoother,
};
pub use coax::{
    COAX_MIN_LEVEL, COAX_REQUIRED_MIN_LEVEL, CoaxEffect, CoaxFailReason, CoaxFlow, CoaxInput,
    CoaxStep, RUNAWAY_PERFORMANCE_MS, TRAY_COAX_STROKE_TAPS, coax_target_level, stroke_target_ms,
};
pub use engine::{
    ColdReason, EmotionEngine, EmotionEvent, EmotionState, NeglectPressure, OfflineOutcome,
    RateClamp, ReliefKind, Sensitivity, TickEnv, TickOutcome,
};
pub use neglect::{NaturalCoolMeter, UnreachableMeter};
pub use personality::Personality;
pub use presence::{PresenceLatch, PresenceOutput};
pub use rhythm::{RhythmOutput, RhythmSolver};
pub use rough::RoughTracker;
pub use solver::{
    FACTOR_ORDER, FactorInputs, FactorSet, FactorSolver, InteractionPolicy, SolverCtx, needs_factor,
};
pub use lines::{
    BUBBLE_DWELL_DEFAULT_MS, BUBBLE_DWELL_MAX_MS, BUBBLE_DWELL_MIN_MS, BubbleAction, BubbleKind,
    BubblePlanner, CatchphraseFrequency, CatchphraseGate, HELP_REJECT_WINDOW_SEC, HelpCooldown,
    LinePick, LineSelector, LinesConfig, LinesError, LinesLibrary, POOL_KEYS, PlaceholderVars,
    PlannedBubble, SelectorCfg, clamp_dwell_ms, render_placeholders,
};
pub use sys_env::{NoSysEnv, SysEnv};

/// 阶段档位数（L0~L5，`02 §5.7 levels` 六档）。
pub const LEVEL_COUNT: usize = 6;

/// 需求维度键（`needs.json.dimensions`，`02 §5.12`）。
pub mod need_keys {
    /// 饱食度键名。
    pub const SATIETY: &str = "satiety";
    /// 清洁度键名。
    pub const CLEANLINESS: &str = "cleanliness";
}
