//! `dp-economy`：心币账本、商城目录、限购、购买事务与背包（T-22 / S8-M5、S8-M6）。
//!
//! 设计契约（`02 §5 K-13` / `§5.18`）：
//!   - **账本可重放**：每条流水带单调序号 `seq`、UTC 毫秒时间戳、幂等 `refId`、
//!     来源 `CoinSource`、带符号 `amount`（正入账 / 负消费）、落账后余额 `balanceAfter`、
//!     当日桶 `dayBucket`；`replay()` 自空账重放必须等于 `balance()`（**AC-24 前置**）。
//!   - **三道硬顶**：余额上限 `coinMax=99999`、单笔入账 `perTxMax=2000`、
//!     日入账硬顶 `dailyIncomeCap=350`（QI-05）；触顶截断并记 [`CreditOutcome::clipped`]。
//!   - **消费不占日顶**：`debit`（购买）不受 350 限制，只校验余额；失败冲正回到调用前。
//!
//! 本 crate **零时钟**（C3）：所有日期桶 / 周桶由调用方（dp-app 逻辑档）注入
//! `day_key`（`YYYY-MM-DD` 本地）/ `week_key`（ISO 周），纯函数可测。

pub mod catalog;
pub mod fulfillment;
pub mod inventory;
pub mod ledger;
pub mod order;
pub mod quota;

pub use catalog::{AchievementJudge, Catalog};
pub use inventory::Inventory;
pub use ledger::{CreditOutcome, CreditRecord, Ledger};
pub use order::{OrderOutcome, OrderRequest};
pub use quota::QuotaTracker;

use serde::{Deserialize, Serialize};

/// 心币入账来源（白名单；`02 §5.18`）。
///
/// 编译期闭合：入账只能经 [`Ledger::credit`]，来源限定本枚举，杜绝任意串注入。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "arg", rename_all = "camelCase")]
pub enum CoinSource {
    /// 打工结算（主渠道；携带岗位 ID）。
    WorkSettle {
        /// 岗位 / 活动定义 ID（`W-01`…）。
        activity_id: String,
    },
    /// 日常任务。
    DailyTask,
    /// 连续登录奖励（携带第几天）。
    LoginStreak {
        /// 连续登录天数（从 1 起）。
        day: u32,
    },
    /// 成就达成（FR-9-1；携带成就 ID，幂等 refId）。
    Achievement {
        /// 成就 ID。
        achievement_id: String,
    },
    /// v1→v2 迁移补偿（D-5；一次性，`refId="migration:v1tov2"`）。
    MigrationGrant,
    /// 旅游发现。
    TravelFind,
    /// 节日红包。
    Festival,
    /// 彩蛋拾取。
    Egg,
    /// 购买冲正 / 退款（事务失败回滚时的负向入账）。
    Refund,
}

impl CoinSource {
    /// 判别名（日志 / 测试用）。
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            CoinSource::WorkSettle { .. } => "workSettle",
            CoinSource::DailyTask => "dailyTask",
            CoinSource::LoginStreak { .. } => "loginStreak",
            CoinSource::Achievement { .. } => "achievement",
            CoinSource::MigrationGrant => "migrationGrant",
            CoinSource::TravelFind => "travelFind",
            CoinSource::Festival => "festival",
            CoinSource::Egg => "egg",
            CoinSource::Refund => "refund",
        }
    }
}

/// 经济错误（购买被拒等；`02 §7.4.3` 中文可读）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EconomyError {
    /// 余额不足。
    #[error("心币不足：需要 {need}，余额 {have}")]
    InsufficientCoin {
        /// 需要的金额。
        need: i64,
        /// 当前余额。
        have: i64,
    },
    /// 商品不存在于目录。
    #[error("商品不存在：{0}")]
    ItemNotFound(String),
    /// 未解锁（亲密度不足）。
    #[error("商品未解锁：{0}")]
    Locked(String),
    /// 超过限购。
    #[error("超过限购：{0}")]
    QuotaExceeded(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coin_source_kind_names() {
        assert_eq!(CoinSource::DailyTask.kind(), "dailyTask");
        assert_eq!(
            CoinSource::Achievement { achievement_id: "A1".into() }.kind(),
            "achievement"
        );
    }
}
