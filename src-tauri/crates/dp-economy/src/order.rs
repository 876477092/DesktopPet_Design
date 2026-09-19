//! 购买事务请求与结果（K-13：校验→预留→扣款→入库，失败冲正）。

/// 一笔购买请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderRequest {
    /// 商品 ID。
    pub item_id: String,
    /// 数量。
    pub qty: u32,
    /// 当前亲密度等级（解锁校验）。
    pub affinity_level: u32,
    /// 当日桶（`YYYY-MM-DD`）。
    pub day_key: String,
    /// 当周桶（`YYYY-Www`）。
    pub week_key: String,
}

/// 购买结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderOutcome {
    /// 实际成交数量。
    pub qty: u32,
    /// 实扣心币。
    pub spent: i64,
    /// 成交后余额。
    pub balance_after: i64,
}
