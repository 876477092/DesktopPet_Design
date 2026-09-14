//! 交互内核（S3-M2 / S3-M3）：像素命中判定与手势状态机（`02 §5 K-2` / K-6）。
//!
//! 承载内容（`02 §3` interaction/ 段）：
//!   - [`hit`]：`HitSource` 三态命中判定源 trait（`Hit` / `Hover` / `Miss`）与
//!     bbox 回退实现 [`hit::BboxHitSource`]（裁定1：trait 落 dp-core，掩码实现
//!     `MaskHitSource` 为 dp-app 自有类型组合 dp-assets，孤儿规则合规）；
//!   - [`gesture`]：手势状态机 [`gesture::GestureMachine`]（K-6 状态图简化实现，
//!     `now_ms` 全注入，C3 零时钟）与轨迹三分类纯函数 [`gesture::classify_trail`]
//!     （FR-4-9，32 点环形缓冲）；
//!   - [`router`]：意图枚举 [`router::InteractionKind`]（含 S7-M6 占位变体）、
//!     ACT 码纯映射 [`router::act_of`]（只映射不提交仲裁）与分类计数下标。
//!
//! 数据流（设计 §4）：钩子事件 → dp-app 消费端映射为 [`gesture::InputEvent`] →
//! 状态机 → [`gesture::GestureOutput`] → [`router::InteractionKind`] + ACT 码 →
//! 日志 + 分类计数（本阶段终态；仲裁/情绪结算归 S4，C8 零新增 pet:// 事件）。

pub mod gesture;
pub mod hit;
pub mod router;

pub use gesture::{GestureMachine, GestureOutput, InputEvent, TrailShape, classify_trail};
pub use hit::{BboxHitSource, HitRect, HitResult, HitSource};
pub use router::{
    InteractionKind, InteractionRouter, KIND_COUNT, THROW_LANDING_CHAIN, act_of, as_index,
};
