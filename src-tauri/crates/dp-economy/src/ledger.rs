//! 心币账本：可重放、三道硬顶（`02 §5 K-13` / `§5.18`）。

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{CoinSource, EconomyError};

/// 单条流水（正入账 / 负消费；`amount` 带符号）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditRecord {
    /// 单调递增序号（从 1 起；重放即按 seq 累加）。
    pub seq: u64,
    /// UTC 毫秒时间戳。
    pub at_ms: i64,
    /// 幂等键（同 refId 重复入账视为同一笔，不重复记账）。
    pub ref_id: String,
    /// 来源。
    pub source: CoinSource,
    /// 实际入账 / 消费金额（正 = 入账，负 = 消费）。
    pub amount: i64,
    /// 落账后余额。
    pub balance_after: i64,
    /// 当日桶（`YYYY-MM-DD` 本地；消费桶可空）。
    pub day_bucket: String,
}

/// 入账结果（记录实际入账与是否触顶截断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreditOutcome {
    /// 实际入账金额（触顶截断后可能 < 请求额；0 = 被日顶/余额顶完全挡下）。
    pub credited: i64,
    /// 是否触顶截断（单笔 > perTx / 日顶 / 余额顶任一）。
    pub clipped: bool,
}

/// 经济上限参数（取自 `ShopEconomyCfg`，运行期只读快照）。
#[derive(Debug, Clone, Copy)]
#[derive(Default)]
pub struct EconomyCaps {
    /// 余额上限。
    pub coin_max: i64,
    /// 单笔入账上限。
    pub per_tx_max: i64,
    /// 日入账硬顶。
    pub daily_income_cap: i64,
}

impl EconomyCaps {
    /// 构造。
    #[must_use]
    pub fn new(coin_max: i64, per_tx_max: i64, daily_income_cap: i64) -> Self {
        Self { coin_max, per_tx_max, daily_income_cap }
    }
}

/// 账本（含余额、当日已入账、当日桶；序列化为 save.economy）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ledger {
    /// 流水。
    records: Vec<CreditRecord>,
    /// 当前余额（= 重放结果；冗余缓存，供 O(1) 查询）。
    coin: i64,
    /// 当日已入账（仅正入账合计；消费不计）。
    earned_today: i64,
    /// 当前当日桶（`YYYY-MM-DD`）。
    day_key: String,
    /// 上限。
    #[serde(skip)]
    caps: EconomyCaps,
    /// 已用过的 refId（幂等去重）。
    #[serde(skip)]
    seen_refs: BTreeSet<String>,
}

impl Ledger {
    /// 空账本（新档：余额 0）。
    #[must_use]
    pub fn new(caps: EconomyCaps) -> Self {
        Self {
            records: Vec::new(),
            coin: 0,
            earned_today: 0,
            day_key: String::new(),
            caps,
            seen_refs: BTreeSet::new(),
        }
    }

    /// 从持久化形态还原（save.economy）。
    ///
    /// `records` 反序列化后做一次 `replay` 重算余额，保证外部串改 / 手改不污染内存真值。
    #[must_use]
    pub fn from_parts(
        records: Vec<CreditRecord>,
        earned_today: i64,
        day_key: String,
        caps: EconomyCaps,
    ) -> Self {
        let mut seen = BTreeSet::new();
        for r in &records {
            seen.insert(r.ref_id.clone());
        }
        let mut ledger = Self {
            records,
            coin: 0,
            earned_today: 0,
            day_key,
            caps,
            seen_refs: seen,
        };
        ledger.coin = ledger.replay();
        // 当日桶切日时 earned_today 由调用方在 tick 时清零；此处仅重放。
        if ledger.day_key.is_empty() {
            ledger.earned_today = 0;
        } else {
            ledger.earned_today = earned_today;
        }
        ledger
    }

    /// 当前余额。
    #[must_use]
    pub fn balance(&self) -> i64 {
        self.coin
    }

    /// 当日已入账。
    #[must_use]
    pub fn earned_today(&self) -> i64 {
        self.earned_today
    }

    /// 当日桶。
    #[must_use]
    pub fn day_key(&self) -> &str {
        &self.day_key
    }

    /// 全部流水（只读）。
    #[must_use]
    pub fn records(&self) -> &[CreditRecord] {
        &self.records
    }

    /// 切日（跨 00:00）：`day_key` 变化时清零 `earned_today` 并写入新桶。
    ///
    /// 幂等：同日重复调用无副作用。
    pub fn roll_day(&mut self, new_day_key: &str) {
        if self.day_key != new_day_key {
            self.day_key = new_day_key.to_string();
            self.earned_today = 0;
        }
    }

    /// 入账（正向）。
    ///
    /// 三道截断依次作用：
    ///   1. `amount > perTxMax` → 截到 `perTxMax`；
    ///   2. `earned_today + x > dailyIncomeCap` → 只补到顶；
    ///   3. `coin + x > coinMax` → 截到 `coinMax`。
    ///
    /// 同 `ref_id` 第二次调用返回零入账（幂等），不重复记账。
    pub fn credit(
        &mut self,
        source: CoinSource,
        mut amount: i64,
        ref_id: &str,
        at_ms: i64,
        day_key: &str,
    ) -> CreditOutcome {
        let mut clipped = false;
        if self.day_key != day_key {
            self.roll_day(day_key);
        }
        // 幂等：同一 refId 已记过，直接返回（视为已入账，不重复）。
        if !self.seen_refs.insert(ref_id.to_string()) {
            let already =
                self.records.iter().find(|r| r.ref_id == ref_id).map(|r| r.amount).unwrap_or(0);
            return CreditOutcome { credited: already, clipped: false };
        }
        if amount <= 0 {
            return CreditOutcome { credited: 0, clipped: false };
        }
        // 1. 单笔顶。
        if amount > self.caps.per_tx_max {
            amount = self.caps.per_tx_max;
            clipped = true;
        }
        // 2. 日入账顶。
        let room = self.caps.daily_income_cap - self.earned_today;
        if amount > room {
            amount = room.max(0);
            clipped = true;
        }
        // 3. 余额顶。
        let coin_room = self.caps.coin_max - self.coin;
        if amount > coin_room {
            amount = coin_room.max(0);
            clipped = true;
        }
        if amount == 0 {
            // 完全被顶掉：记一笔 0 流水保留幂等 refId，但不动余额。
            self.push(0, source, ref_id, at_ms, day_key);
            return CreditOutcome { credited: 0, clipped: true };
        }
        self.earned_today += amount;
        self.coin += amount;
        self.push(amount, source, ref_id, at_ms, day_key);
        CreditOutcome { credited: amount, clipped }
    }

    /// 消费（购买；负向）。
    ///
    /// 不占日顶；余额不足返回 [`EconomyError::InsufficientCoin`]。
    /// 成功后写一条负向流水（`amount = -price`）。
    ///
    /// # Errors
    /// 余额不足时返回错误，账本不变（由调用方做事务冲正）。
    pub fn debit(
        &mut self,
        amount: i64,
        ref_id: &str,
        at_ms: i64,
        day_key: &str,
    ) -> Result<(), EconomyError> {
        if amount <= 0 {
            return Ok(());
        }
        if self.coin < amount {
            return Err(EconomyError::InsufficientCoin { need: amount, have: self.coin });
        }
        self.coin -= amount;
        if self.day_key != day_key {
            self.day_key = day_key.to_string();
        }
        // 消费用伪来源（购买），记负向流水。
        let source = CoinSource::Refund;
        let neg = -(amount);
        self.push(neg, source, ref_id, at_ms, day_key);
        Ok(())
    }

    /// 重放：自空账按 seq 累加所有 amount，应等于当前余额。
    #[must_use]
    pub fn replay(&self) -> i64 {
        self.records.iter().map(|r| r.amount).sum()
    }

    /// 自检：`replay() == balance()` 且 seq 严格单调、余额非负。
    ///
    /// # Errors
    /// 重放不一致 / seq 重复 / 余额为负时返回可读错误。
    pub fn sanity_check(&self) -> Result<(), String> {
        if self.replay() != self.coin {
            return Err(format!(
                "重放不一致：replay={} balance={}",
                self.replay(),
                self.coin
            ));
        }
        let mut prev = 0u64;
        let mut expect = 0i64;
        for r in &self.records {
            if r.seq != prev + 1 {
                return Err(format!("seq 不连续：期望 {} 实得 {}", prev + 1, r.seq));
            }
            prev = r.seq;
            expect += r.amount;
            if expect != r.balance_after {
                return Err(format!(
                    "seq={} balanceAfter 不一致：期望 {} 记录 {}",
                    r.seq, expect, r.balance_after
                ));
            }
        }
        if self.coin < 0 {
            return Err(format!("余额为负：{}", self.coin));
        }
        Ok(())
    }

    /// 序列化形态（写 save.economy）。
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "coin": self.coin,
            "ledger": self.records,
            "earnedToday": self.earned_today,
            "dayKey": self.day_key,
        })
    }

    // ------------------------------------------------------------------

    fn push(&mut self, amount: i64, source: CoinSource, ref_id: &str, at_ms: i64, day_key: &str) {
        let seq = (self.records.len() as u64) + 1;
        self.records.push(CreditRecord {
            seq,
            at_ms,
            ref_id: ref_id.to_string(),
            source,
            amount,
            balance_after: self.coin,
            day_bucket: day_key.to_string(),
        });
    }
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new(EconomyCaps::new(99_999, 2_000, 350))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> EconomyCaps {
        EconomyCaps::new(99_999, 2_000, 350)
    }

    #[test]
    fn credit_then_replay_equals_balance() {
        let mut l = Ledger::new(caps());
        l.credit(CoinSource::DailyTask, 10, "t1", 1, "2026-09-19");
        l.credit(CoinSource::WorkSettle { activity_id: "W-01".into() }, 36, "t2", 2, "2026-09-19");
        l.debit(15, "b1", 3, "2026-09-19").unwrap();
        assert_eq!(l.balance(), 31);
        assert_eq!(l.replay(), l.balance());
        l.sanity_check().unwrap();
    }

    #[test]
    fn ten_thousand_random_ops_replay_consistent() {
        // 确定性 LCG（不引 rand 依赖），模拟 1 万笔随机入账/消费。
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let next = |state: &mut u64| -> i64 {
            *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((*state >> 33) % 50 + 1) as i64
        };
        let mut l = Ledger::new(caps());
        for i in 0..10_000u64 {
            let amount = next(&mut state);
            let do_credit = (state % 10) < 8;
            if do_credit {
                l.credit(
                    CoinSource::WorkSettle { activity_id: "W-01".into() },
                    amount,
                    &format!("r{i}"),
                    i as i64,
                    "2026-09-19",
                );
            } else if l.balance() >= amount {
                l.debit(amount, &format!("d{i}"), i as i64, "2026-09-19").unwrap();
            }
        }
        assert_eq!(l.replay(), l.balance());
        l.sanity_check().unwrap();
    }

    #[test]
    fn balance_cap_truncates_and_flags() {
        // 高日顶 + 高单笔顶（只隔离余额顶 99999）。
        let mut l = Ledger::new(EconomyCaps::new(99_999, 100_000_000, 100_000_000));
        l.credit(CoinSource::MigrationGrant, 99_990, "seed", 1, "2026-09-19");
        let out = l.credit(CoinSource::DailyTask, 100, "over", 2, "2026-09-19");
        assert!(out.clipped, "触顶应标记 clipped");
        assert_eq!(l.balance(), 99_999, "应截到余额上限");
        l.sanity_check().unwrap();
    }

    #[test]
    fn daily_income_cap_applies() {
        let mut l = Ledger::new(EconomyCaps::new(99_999, 2_000, 100));
        l.credit(CoinSource::DailyTask, 60, "a", 1, "2026-09-19");
        let out = l.credit(CoinSource::DailyTask, 60, "b", 2, "2026-09-19");
        assert!(out.clipped);
        assert_eq!(out.credited, 40, "日顶 100，已入 60，再入只补 40");
        assert_eq!(l.earned_today(), 100);
    }

    #[test]
    fn per_tx_cap_applies() {
        // 高日顶（隔离单笔顶）：单笔顶 2_000。
        let mut l = Ledger::new(EconomyCaps::new(99_999, 2_000, 10_000_000));
        let out = l.credit(CoinSource::MigrationGrant, 5_000, "big", 1, "2026-09-19");
        assert!(out.clipped);
        assert_eq!(out.credited, 2_000);
    }

    #[test]
    fn idempotent_ref_id() {
        let mut l = Ledger::new(caps());
        l.credit(CoinSource::Achievement { achievement_id: "A".into() }, 50, "ach:A", 1, "2026-09-19");
        let again =
            l.credit(CoinSource::Achievement { achievement_id: "A".into() }, 50, "ach:A", 2, "2026-09-19");
        assert_eq!(again.credited, 50, "幂等返回首次入账额");
        assert_eq!(l.balance(), 50, "重复入账不应再加");
        assert_eq!(l.records().len(), 1);
    }

    #[test]
    fn debit_insufficient_coin_errors() {
        let mut l = Ledger::new(caps());
        l.credit(CoinSource::DailyTask, 10, "seed", 1, "2026-09-19");
        let err = l.debit(999, "b1", 2, "2026-09-19").unwrap_err();
        assert!(matches!(err, EconomyError::InsufficientCoin { .. }));
        assert_eq!(l.balance(), 10, "失败不应动账");
    }

    #[test]
    fn roll_day_resets_earned() {
        let mut l = Ledger::new(caps());
        l.credit(CoinSource::DailyTask, 300, "a", 1, "2026-09-19");
        assert_eq!(l.earned_today(), 300);
        l.roll_day("2026-09-20");
        assert_eq!(l.earned_today(), 0);
        // 新一天再入账不受昨天累计影响。
        l.credit(CoinSource::DailyTask, 100, "b", 2, "2026-09-20");
        assert_eq!(l.earned_today(), 100);
    }
}
