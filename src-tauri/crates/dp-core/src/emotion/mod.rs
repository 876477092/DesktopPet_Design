//! `emotion`：情绪数值、六维状态机与暂停累积内核（`02 §5 K-5` / §5.1~5.5；FR-5 / FR-11）。
//!
//! 模块分工（`02 §5.1`）：
//!   - [`engine`] —— 主 tick 编排：因子 → ΔP → 阶段结算 → Mood 惯性 → 日切；
//!   - `speech.rs` / `coax.rs` / `lines.rs` —— 台词闸门 / 道歉三部曲 / 台词池（**S4-M3 / S4-M5**，本卡不做）。
//!
//! ## 本卡（S4-M1）交付边界
//!
//! 台账行 1534 补登 P2-1 裁定：**「L0~L5 骨架口径 = 结构占位 + `emotion.json` v2 冻结阈值
//! 简化判定，S7-M5 只换判定内核」**。据此本模块交付：
//!   1. 六维数值外置读取（`emotion.json` / `needs.json`，C7）+ Boredom 派生展示量；
//!   2. L0~L5 阶段判定与确认期（升级 60s / 回退 30s、**绝不跳级**、逐层扣 Mood）；
//!   3. 会话暂停（锁屏 / 远程桌面 / 全屏前台）期间**冻结 P 累积且不跳变**；
//!      `WallClock` 端口计时（C3，dp-core 零时钟，时间全由调用方注入 `now_ms`）。
//!
//! ## 与 S7-M4 / S4-M2 的切割（禁止顺手改动）
//!
//!   - **七因子求解归 S7-M4**：本卡不引入 `FactorSolver`。因子乘积由调用方经
//!     [`sys_env::SysEnv::rate_per_min`] 注入（S4-M1 简化口径 = 在场/离场 × 敏感度钳制），
//!     保证 `switch` 到 S7-M4 的真实七因子时 `engine` 主体零改动。
//!   - **事件总线与 tick 链路归 S4-M2**（**2026-09-14 解禁**）：`engine` 只产出
//!     [`engine::EmotionEvent`] 列表；**映射为 `pet://` 事件**（C8 白名单）与
//!     [`engine::EmotionEngine::snapshot`] 投影实装于 [`crate::event`]，`dp-app` 1Hz
//!     业务档只做 emit（不含事件名逻辑）。**本模块仍不依赖 Tauri**。
//!   - **台词 / 气泡 / 道歉三部曲归 S4-M3 / S4-M5**。

pub mod engine;
pub mod sys_env;

pub use engine::{
    ColdReason, EmotionEngine, EmotionEvent, EmotionState, NeglectPressure, OfflineOutcome,
    RateClamp, Sensitivity, TickEnv, TickOutcome,
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
