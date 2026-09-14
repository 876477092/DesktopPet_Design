//! 动作目录与动作播放器（S2-M2，T-04 段 · 下）。
//!
//! 职责（卡片三条）：
//!   1. [`catalog::ActionCatalog`]：加载 `actions.json`（53 条全量元数据，ID 一律
//!      `ACT-*`，复用 `dp-core::config::ActionsConfig` 模型，**不重复定义字段结构**）；
//!      未交付动作（`disabled=true`，24 条）仅做标记暴露，跳过决策归 S2-M3 仲裁器；
//!   2. [`player::ActionPlayer`]：批次 A 29 动作轮播播放——动画帧号 =
//!      `elapsed × 动作 fps`（actions.json 每动作 `fps` 驱动，如 ACT-M-02 行走 12fps），
//!      循环段按 `loopRange` 回绕、非循环动作播完停留；
//!   3. **fps 档位**（K-4 场景表）：2/4/6/15/30/60 为播放器 **tick 节拍档位**
//!      （睡眠 2 / 省电 4 / 空闲 6 / 高负载 15 / 中档 30 / 普通活动 60），可切换，
//!      CPU 随档位变化；动作自身帧时长由动作 `fps` 独立驱动，二者正交；
//!   4. **镜像规则**（`02 §5 K-4`）：素材仅交付朝左（direction 仅 `l`），
//!      播放器按动作元数据 `mirror` + 当前朝向产出 `mirror` 标志；
//!      渲染端 `ctx.scale(-1,1)` / uFlipX 与命中源 `frameW - x` 映射（T-08）共用
//!      [`player::mirror_x`] 的同一离散语义（`frameW - 1 - x`）。
//!
//! 边界：
//!   - 不做仲裁器（S2-M3）——目录只提供「已启用」视图，不决定播放哪个动作；
//!   - 不做 150~250ms 缓动升级（S9-M3）——本阶段交叉淡入由前端 FrameRenderer
//!     固定 150ms 线性实现，**不读取 animation.json 的 fadeMs / easing**（卡片口径）；
//!   - `RenderFrameCmd` v1 载荷仍定义在 `dp-app/src/bridge.rs`（C8 冻结），
//!     本模块只产出与载荷同构的 [`player::PlayerFrame`]，由 dp-app 拼装。
//!
//! 约束：C3 单调钟语义（播放器只消费 `Duration`，锚定由调用方 tick 循环持有
//! `Instant`）；C1 无盘符字面量；C9 零网络。

pub mod arbiter;
pub mod catalog;
pub mod player;

pub use arbiter::{
    ActionArbiter, ActionId, ActionRequest, ActionSource, ActiveAction, Arbitration, HelpBackoff,
    INTERRUPT_COOLDOWN_MS, QUEUE_CAP, Started,
};
pub use catalog::ActionCatalog;
pub use player::{ActionPlayer, Facing, FpsTier, PlayItem, PlayerFrame, mirror_x};

use thiserror::Error;

/// 动画域错误（`#[non_exhaustive]`，`02 §7.4`）。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AnimError {
    /// fps 档位非法（只允许 [`FpsTier`] 登记的 2/4/6/15/30/60）。
    #[error("非法 fps 档位：{value}（允许档位：2/4/6/15/30/60）")]
    InvalidFpsTier {
        /// 被拒绝的档位数值。
        value: u32,
    },
    /// 动作元数据缺图集帧数（目录派生播放项时动作在图集中不存在）。
    #[error("动作 {action_id} 在图集中无元数据，无法派生播放项")]
    MissingAtlasMeta {
        /// 缺失的动作 ID。
        action_id: String,
    },
}
