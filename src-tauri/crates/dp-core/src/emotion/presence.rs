//! `emotion::presence`：在场因子求解（`02 §5.1` / §5.2 因子 F3 + FR-11-12 层 ①）。
//!
//! ## 职责
//!
//!   1. 由「无键鼠输入时长」判定在场 / 离场（`presence.awayThresholdSec` +
//!      `hysteresisSec` 迟滞，避免临界抖动污染 P 速率）；
//!   2. 输出 `presenceFactor`：在场 [`PresenceCfg::factor_here`]（默认 1.0）/ 离场
//!      [`PresenceCfg::factor_away`]（默认 0.05）；
//!   3. **FR-11-12 层 ①**（`02 §5.23`）：`interaction_available == false`（穿透 /
//!      钩子卸载 / 勿扰）时**复用离场档**——`presenceFactor = unavailablePresenceFactor`
//!      （默认 0.05），**不归零**。语义统一在 B-1 同一分支，保留「她还是有点想你」，
//!      避免「开穿透 = 永不生气」的隐藏作弊开关。
//!
//! ## 时间纪律（C3）
//!
//! 本模块**零时钟**：`now_ms` 由调用方注入，迟滞窗口一律**绝对锚定**（`since_ms` 记录
//! 候选翻转起点，比较 `now_ms − since_ms`），不做逐 tick 累加。

use crate::config::model::PresenceCfg;

/// 在场判定迟滞状态（`presence.hysteresisSec`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenceLatch {
    /// 当前判定为在场。
    here: bool,
    /// 判定翻转的候选起始时刻（绝对锚定；`None` = 无待定翻转）。
    since_ms: Option<i64>,
}

impl Default for PresenceLatch {
    /// 初值恒「在场」：进程刚起来时不应把启动前的空闲算成离场。
    fn default() -> Self {
        Self { here: true, since_ms: None }
    }
}

impl PresenceLatch {
    /// 新建（恒在场起始）。
    #[must_use]
    pub const fn new() -> Self {
        Self { here: true, since_ms: None }
    }

    /// 当前是否判定在场。
    #[must_use]
    pub const fn is_here(self) -> bool {
        self.here
    }

    /// 强制回到「在场」并清空迟滞计时（恢复不可达 → 可交互时用）。
    pub fn force_here(&mut self) {
        self.here = true;
        self.since_ms = None;
    }
}

/// 在场因子求解结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresenceOutput {
    /// `presenceFactor`（0.05 / 1.0，或不可达档）。
    pub factor: f32,
    /// 迟滞判定后的在场布尔（不可达时仍如实反映迟滞状态）。
    pub here: bool,
    /// 本拍是否发生 `离场 → 在场` 翻转（预热窗口的锚点来源）。
    pub became_here: bool,
    /// 交互是否可达（`false` = 走了 FR-11-12 层 ① 分支）。
    pub interaction_available: bool,
}

/// 求解在场因子（`02 §5.2` 因子 F3）。
///
/// `idle_ms == u64::MAX` 视为「不可用 / 无限空闲」（离线补偿路径），仍走离场判定。
#[must_use]
pub fn resolve(
    latch: &mut PresenceLatch,
    idle_ms: u64,
    cfg: &PresenceCfg,
    now_ms: i64,
    interaction_available: bool,
    unavailable_presence_factor: f32,
) -> PresenceOutput {
    let was_here = latch.here;
    let here = decide(latch, idle_ms, cfg, now_ms);
    let factor = if interaction_available {
        if here {
            cfg.factor_here
        } else {
            cfg.factor_away
        }
    } else {
        // FR-11-12 层 ①：与「不在场」同档，不归零（`02 §5.23`）。
        unavailable_presence_factor
    };
    PresenceOutput {
        factor: factor.max(0.0),
        here,
        became_here: !was_here && here,
        interaction_available,
    }
}

/// 迟滞判定内核（就地更新 `latch`，返回判定后的在场布尔）。
///
/// 口径（保持 S4-M1 已交付行为**逐字不变**）：期望态与当前态一致即清空候选计时；
/// 不一致则以首次不一致时刻为锚，持续满 `hysteresisSec` 才翻转。
fn decide(latch: &mut PresenceLatch, idle_ms: u64, cfg: &PresenceCfg, now_ms: i64) -> bool {
    let away_ms = cfg.away_threshold_sec.saturating_mul(1000);
    let hyst_ms = cfg.hysteresis_sec.saturating_mul(1000) as i64;
    let idle = if idle_ms == u64::MAX { away_ms.saturating_add(1) } else { idle_ms };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PresenceCfg {
        PresenceCfg::default()
    }

    #[test]
    fn here_when_idle_below_threshold() {
        let mut latch = PresenceLatch::new();
        let out = resolve(&mut latch, 0, &cfg(), 0, true, 0.05);
        assert!(out.here);
        assert!((out.factor - 1.0).abs() < 1e-6);
        assert!(!out.became_here);
    }

    #[test]
    fn away_after_hysteresis_only() {
        let mut latch = PresenceLatch::new();
        let c = cfg();
        // 触发离场候选：idle 超阈，但迟滞未满 → 仍在场
        let out = resolve(&mut latch, 200_000, &c, 1_000, true, 0.05);
        assert!(out.here, "迟滞 15s 未满不得翻转");
        // 15s 后仍离场 → 翻转
        let out = resolve(&mut latch, 200_000, &c, 16_000, true, 0.05);
        assert!(!out.here);
        assert!((out.factor - c.factor_away).abs() < 1e-6);
    }

    #[test]
    fn hysteresis_candidate_cleared_when_back() {
        let mut latch = PresenceLatch::new();
        let c = cfg();
        let _ = resolve(&mut latch, 200_000, &c, 1_000, true, 0.05);
        // 中途回到在场 → 候选计时清零，随后再离场须重新计满 15s
        let _ = resolve(&mut latch, 0, &c, 5_000, true, 0.05);
        let out = resolve(&mut latch, 200_000, &c, 10_000, true, 0.05);
        assert!(out.here, "候选计时应已清零");
    }

    #[test]
    fn became_here_reported_on_transition() {
        let mut latch = PresenceLatch::new();
        let c = cfg();
        let _ = resolve(&mut latch, 200_000, &c, 0, true, 0.05);
        let out = resolve(&mut latch, 200_000, &c, 20_000, true, 0.05);
        assert!(!out.here);
        // 回到在场：先满迟滞，再翻转
        let _ = resolve(&mut latch, 0, &c, 21_000, true, 0.05);
        let out = resolve(&mut latch, 0, &c, 40_000, true, 0.05);
        assert!(out.here);
        assert!(out.became_here);
    }

    #[test]
    fn unavailable_uses_dedicated_factor_without_zeroing() {
        let mut latch = PresenceLatch::new();
        let out = resolve(&mut latch, 0, &cfg(), 0, false, 0.05);
        assert!(!out.interaction_available);
        assert!((out.factor - 0.05).abs() < 1e-6, "不归零");
        assert!(out.here, "迟滞状态本身仍如实反映在场");
    }

    #[test]
    fn unavailable_factor_never_negative() {
        let mut latch = PresenceLatch::new();
        let out = resolve(&mut latch, 0, &cfg(), 0, false, -1.0);
        assert_eq!(out.factor, 0.0);
    }

    #[test]
    fn idle_ms_max_is_away() {
        let mut latch = PresenceLatch::new();
        let c = cfg();
        let _ = resolve(&mut latch, u64::MAX, &c, 0, true, 0.05);
        let out = resolve(&mut latch, u64::MAX, &c, 20_000, true, 0.05);
        assert!(!out.here);
    }
}
