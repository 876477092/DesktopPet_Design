//! `emotion`：情绪数值、六维状态机与暂停累积内核（`02 §5 K-5` / §5.1~5.5；FR-5 / FR-11）。
//!
//! 模块分工（`02 §5.1`）：
//!   - [`engine`] —— 主 tick 编排：因子 → ΔP → 阶段结算 → Mood 惯性 → 日切；
//!   - [`coax`] —— 道歉三部曲 `CoaxFlow`（含 L5 离家找回 / `force_lower` / 托盘输入，
//!     **S4-M3 / S4-M4** 交付）；
//!   - `speech.rs` / `lines.rs` —— 口头禅闸门 / 台词池（**S4-M5 / S7-M8**，尚未交付）。
//!
//! ## 本卡交付边界（S4-M3 / S4-M4）
//!
//!   1. 三部曲状态机（呼唤 → 抚摸 `strokeSec` → 比心）与进度环数据；
//!   2. 中断回退（`interruptRollbackRatio` 保留部分进度 + `rough.interruptPenalty`）；
//!   3. 完成：`Mood = max(Mood, 50)` + 档位目标 + `ACT-E-06`（`P relief` 取 `relief.coax`）；
//!   4. L5 离家演出 / 找回走回 / `force_lower` 兜底（L5 不开放自然回退）。
//!
//! ## 与 S7 的切割（禁止顺手改动）
//!
//!   - **七因子求解归 S7-M4**：本卡不引入 `FactorSolver`。
//!   - **自然消气通道（L3→L2）与阶段阈值内核归 S7-M5**：本卡只在 `settle_level` 中
//!     锁死 **L4/L5 强制地板**（必须走 CoaxFlow），L3 的完整自然消气口径仍归 S7-M5。
//!   - **敏感度滑杆 / 交互死锁三层防护（托盘入口闭环）归 S7-M6**：本卡只让 `CoaxFlow`
//!     **接受**托盘输入并交付 `force_lower`，托盘菜单接线与 `interaction_available` 归 S7-M6。
//!   - **台词 / 气泡归 S4-M5**：本卡不产任何台词文本。

pub mod coax;
pub mod engine;
pub mod sys_env;

pub use coax::{
    COAX_MIN_LEVEL, COAX_REQUIRED_MIN_LEVEL, CoaxEffect, CoaxFailReason, CoaxFlow, CoaxInput,
    CoaxStep, RUNAWAY_PERFORMANCE_MS, TRAY_COAX_STROKE_TAPS, coax_target_level, stroke_target_ms,
};
pub use engine::{
    ColdReason, EmotionEngine, EmotionEvent, EmotionState, NeglectPressure, OfflineOutcome,
    RateClamp, ReliefKind, Sensitivity, TickEnv, TickOutcome,
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
