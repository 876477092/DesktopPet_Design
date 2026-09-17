//! 生存需求系统（S7-M2，T-18 段 · 上 / `01 §6.12` FR-12 / `02 §5 K-11` / §5.9 / §5.12）。
//!
//! 职责（03 台账 S7-M2 卡片）：
//!   - **六维自然变化**（`02 §5.9`）：Satiety `-0.05/分钟`（B-01 定版）× 用餐 2.5 ×
//!     活动 2.0 × 睡眠 0.4；Cleanliness `-0.08/分钟` × 活动 2.0 × 去污减速 0.5；
//!   - **一次性事件扣减**：`needs.json.eventDeltas`（打滚 / 甩出落地 / 吃饭 / 学习 / 旅游）；
//!   - **分档**（[`bands`]）：饱食 / 清洁五档与表现聚合（动作候选、乘子、污渍、拒绝派遣）；
//!   - **跨档事件源**：分档变化时置位，供上层发 `pet://needs`（`02 §7.6`，属性跨档时）。
//!
//! ## 单一真源口径（重要）
//!
//! 六维数值的真源是 [`crate::state::PetValues`]（存档段 A `values`），本模块**不另存
//! 副本**——[`NeedsSystem::tick`] / [`NeedsSystem::apply_event`] 一律以 `&mut PetValues`
//! 就地推进。本模块只额外持有**冷却时间戳**（存档段 C `needs.*`，与 `NeedsSave` 同构）
//! 与**上一次分档缓存**（用于跨档判定）。此设计避免「需要两处同时更新」的双真源缺陷。
//!
//! ## 红线（`01 §6.12.2` Q-14 / AC-32）
//!
//! **不设计死亡机制**：本模块无任何「死亡 / 消失 / 永久损失」分支——数值只做
//! `clamp(min, max)`；`satiety = 0` 的最坏结果仅是「拒绝派遣 + 委屈表现」。
//! 由 `tick` 的 72h 连续演练单测锁定（`satiety=0` 且 `cleanliness=0` 恒不越界、不 panic）。
//!
//! 时间纪律（C3）：本模块**零时钟**——`dt_min` 与 `now_ms` 均由调用方注入；
//! `clean_slow_until_ms` 的比较是「注入时刻 vs 存档时间戳」，不读真实时间。
//!
//! 边界：**不含**耦合矩阵求值（归 [`coupling`]，S7-M3）；**不含**洗澡流程状态机
//! （`bath` 段演出与动作接入归 S7-M9，本模块只做冷却时间戳记账）；**不含**任何动作
//! 提交（N 系列接入归 S7-M9）。

pub mod bands;
pub mod coupling;

pub use bands::{BandEffects, CleanBand, SatietyBand};
pub use coupling::{
    CouplingError, CouplingOutput, CouplingSolver, DispatchDeny, DispatchVerdict,
};

use crate::config::model::NeedsConfig;
use crate::save::schema::NeedsSave;
use crate::state::PetValues;

/// 派遣类别（`needs.json` 的 `deny` 列表字面量，`01 §6.12.4`）。
///
/// 与活动状态机（S8）共用同一词表：`"work"` / `"study"` / `"travel"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DispatchKind {
    /// 打工。
    Work,
    /// 学习。
    Study,
    /// 旅游。
    Travel,
}

impl DispatchKind {
    /// 全部类别。
    pub const ALL: [DispatchKind; 3] =
        [DispatchKind::Work, DispatchKind::Study, DispatchKind::Travel];

    /// 类别 ID（与配置 `deny` 字面量逐字一致，C7）。
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            DispatchKind::Work => "work",
            DispatchKind::Study => "study",
            DispatchKind::Travel => "travel",
        }
    }

    /// 由 ID 反查（未知 → `None`）。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        DispatchKind::ALL.into_iter().find(|k| k.id() == id)
    }
}

/// 一次性扣减原因（键与 `needs.json.eventDeltas` 逐字一致，C7）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NeedCause {
    /// 打滚 / 摔倒（`ACT-T-07` 落地打滚后）。
    Roll,
    /// 被甩出落地。
    ThrownLand,
    /// 吃饭（吧唧嘴弄脏）。
    Eat,
    /// 学习。
    Study,
    /// 旅游。
    Trip,
}

impl NeedCause {
    /// 全部原因。
    pub const ALL: [NeedCause; 5] = [
        NeedCause::Roll,
        NeedCause::ThrownLand,
        NeedCause::Eat,
        NeedCause::Study,
        NeedCause::Trip,
    ];

    /// 配置键（`needs.json.eventDeltas` 的键名）。
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            NeedCause::Roll => "roll",
            NeedCause::ThrownLand => "thrownLand",
            NeedCause::Eat => "eat",
            NeedCause::Study => "study",
            NeedCause::Trip => "trip",
        }
    }
}

/// 需求推进环境（`02 §5.9` `NeedsEnv`）。
///
/// 全部字段由调用方在**本拍**求值后注入（时间、时段、活动、睡眠状态均非需求模块职责）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NeedsEnv {
    /// 本拍墙钟毫秒（与存档段 C 的截止时间戳同口径）。
    pub now_ms: i64,
    /// 是否处于用餐窗口（`02 §5.6` 用餐窗口 07-09 / 11-13 / 17-19，
    /// 由 `perception::time::TimeRhythm::is_meal_window` 求值后注入）。
    pub meal_window: bool,
    /// 是否处于外出活动进行中（`01 §6.13`；倍率 ×2.0）。
    pub activity_running: bool,
    /// 是否睡眠中（倍率 ×0.4；S7-M2 口径 = 展示态 `Sleepy`，
    /// 睡眠动作状态机归 S8-M1，届时改为显式信号）。
    pub asleep: bool,
}

impl NeedsEnv {
    /// 便捷构造。
    #[must_use]
    pub const fn new(
        now_ms: i64,
        meal_window: bool,
        activity_running: bool,
        asleep: bool,
    ) -> Self {
        Self { now_ms, meal_window, activity_running, asleep }
    }
}

/// 需求系统（冷却记账 + 分档缓存；数值真源见模块文档）。
///
/// `Default` 语义 = 全新系统：冷却时间戳全 `0`（无冷却 / 无 Buff）、分档基线为空
/// （首拍只建立基线、不报跨档）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NeedsSystem {
    /// 免费洗澡冷却截止墙钟毫秒（`0` = 无冷却）。
    bath_free_cd_until_ms: i64,
    /// 香味 Buff 截止墙钟毫秒（`0` = 无 Buff）。
    scent_buff_until_ms: i64,
    /// 去污泡泡减速截止墙钟毫秒（`0` = 无减速）。
    clean_slow_until_ms: i64,
    /// 上一次饱食档（`None` = 尚未评估，首拍不报跨档）。
    last_satiety: Option<SatietyBand>,
    /// 上一次清洁档（同上）。
    last_clean: Option<CleanBand>,
}

/// 一次推进（tick / 事件）的产出。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NeedsOutcome {
    /// 推进后的饱食度。
    pub satiety: f32,
    /// 推进后的清洁度。
    pub cleanliness: f32,
    /// 推进后的饱食档。
    pub satiety_band: SatietyBand,
    /// 推进后的清洁档。
    pub clean_band: CleanBand,
    /// 本次是否发生**跨档**（任一分档与上一拍不同）→ 上层发 `pet://needs`。
    pub band_changed: bool,
}

impl NeedsSystem {
    /// 全新需求系统（无冷却、无分档基线：**首拍只建立基线，不报跨档**）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 由存档段 C 的冷却时间戳恢复（分档基线仍为空——首拍重建基线）。
    #[must_use]
    pub fn restore_from(save: &NeedsSave) -> Self {
        Self {
            bath_free_cd_until_ms: save.bath_free_cd_until_ms,
            scent_buff_until_ms: save.scent_buff_until_ms,
            clean_slow_until_ms: save.clean_slow_until_ms,
            last_satiety: None,
            last_clean: None,
        }
    }

    /// 导出为存档段 C（`02 §5 K-7` `needs.*`）。
    #[must_use]
    pub const fn to_save(&self) -> NeedsSave {
        NeedsSave {
            bath_free_cd_until_ms: self.bath_free_cd_until_ms,
            scent_buff_until_ms: self.scent_buff_until_ms,
            clean_slow_until_ms: self.clean_slow_until_ms,
        }
    }

    /// 免费洗澡冷却截止（`0` = 无冷却）。
    #[must_use]
    pub const fn bath_free_cd_until_ms(&self) -> i64 {
        self.bath_free_cd_until_ms
    }

    /// 香味 Buff 截止（`0` = 无）。
    #[must_use]
    pub const fn scent_buff_until_ms(&self) -> i64 {
        self.scent_buff_until_ms
    }

    /// 去污泡泡减速截止（`0` = 无）。
    #[must_use]
    pub const fn clean_slow_until_ms(&self) -> i64 {
        self.clean_slow_until_ms
    }

    /// 记入免费洗澡冷却（`needs.json.bath.freeCdMin`；`now_ms` 由调用方注入）。
    pub fn mark_bath(&mut self, cfg: &NeedsConfig, now_ms: i64) {
        let cd_ms = i64::from(cfg.bath.free_cd_min).saturating_mul(60_000);
        self.bath_free_cd_until_ms = now_ms.saturating_add(cd_ms);
    }

    /// 免费洗澡当前是否可用（未到冷却截止）。
    #[must_use]
    pub const fn bath_available(&self, now_ms: i64) -> bool {
        now_ms >= self.bath_free_cd_until_ms
    }

    /// 记入香味 Buff（`01 §6.12.3`：洗护用品 12min）。
    pub fn mark_scent_buff(&mut self, minutes: u32, now_ms: i64) {
        self.scent_buff_until_ms = now_ms.saturating_add(i64::from(minutes) * 60_000);
    }

    /// 香味 Buff 当前是否生效（Mood 衰减 ×0.8，消费方 = 情绪引擎）。
    #[must_use]
    pub const fn scent_buff_active(&self, now_ms: i64) -> bool {
        now_ms < self.scent_buff_until_ms
    }

    /// 记入去污泡泡减速（0.5 倍清洁衰减）。
    pub fn mark_clean_slow(&mut self, duration_ms: i64, now_ms: i64) {
        self.clean_slow_until_ms = now_ms.saturating_add(duration_ms.max(0));
    }

    /// 自然变化推进（`02 §5.9` 逐式对齐；就地推进 `values`）。
    ///
    /// 公式：
    ///   - `satiety += decayPerMin × 用餐 × 活动 × 睡眠 × dt_min`；
    ///   - `cleanliness += decayPerMin × 用餐 × 活动 ×（去污减速 0.5 若生效）× dt_min`；
    ///   - 两维随后各自 `clamp(min, max)`（`01 §6.12` 六维定义区间）。
    ///
    /// `dt_min ≤ 0`（暂停窗口 / 时钟回拨）→ 不推进（但**仍重建分档基线**，避免恢复后
    /// 因基线陈旧而误报跨档）。
    pub fn tick(
        &mut self,
        values: &mut PetValues,
        cfg: &NeedsConfig,
        dt_min: f32,
        env: &NeedsEnv,
    ) -> NeedsOutcome {
        let dn = &cfg.dimensions;
        if dt_min.is_finite() && dt_min > 0.0 {
            let sat_rate = dn.satiety.decay_per_min
                * meal_mul(dn.satiety.meal_multiplier, env.meal_window)
                * activity_mul(dn.satiety.activity_multiplier, env.activity_running)
                * sleep_mul(dn.satiety.sleep_multiplier, env.asleep);
            values.satiety += sat_rate * dt_min;

            let clean_slow = if self.clean_slow_until_ms > env.now_ms { 0.5 } else { 1.0 };
            let cln_rate = dn.cleanliness.decay_per_min
                * meal_mul(dn.cleanliness.meal_multiplier, env.meal_window)
                * activity_mul(dn.cleanliness.activity_multiplier, env.activity_running)
                * sleep_mul(dn.cleanliness.sleep_multiplier, env.asleep)
                * clean_slow;
            values.cleanliness += cln_rate * dt_min;

            values.satiety = values.satiety.clamp(dn.satiety.min, dn.satiety.max);
            values.cleanliness = values.cleanliness.clamp(dn.cleanliness.min, dn.cleanliness.max);
        }
        self.observe_bands(values, cfg)
    }

    /// 一次性事件扣减（`02 §5.9` / `needs.json.eventDeltas`；统一入口）。
    ///
    /// 未知原因键（配置缺该项）→ 不扣减、只重建分档基线（不 panic）。
    pub fn apply_event(
        &mut self,
        values: &mut PetValues,
        cfg: &NeedsConfig,
        cause: NeedCause,
    ) -> NeedsOutcome {
        if let Some(delta) = cfg.event_deltas.get(cause.key()) {
            let dn = &cfg.dimensions;
            values.cleanliness =
                (values.cleanliness + delta.cleanliness).clamp(dn.cleanliness.min, dn.cleanliness.max);
        }
        self.observe_bands(values, cfg)
    }

    /// 直接置入清洁度（喂食 / 洗澡等道具效果出口；仍走 clamp）。
    pub fn add_cleanliness(&mut self, values: &mut PetValues, cfg: &NeedsConfig, delta: f32) -> NeedsOutcome {
        let dn = &cfg.dimensions;
        values.cleanliness = (values.cleanliness + delta).clamp(dn.cleanliness.min, dn.cleanliness.max);
        self.observe_bands(values, cfg)
    }

    /// 直接置入饱食度（喂食道具效果出口；仍走 clamp）。
    pub fn add_satiety(&mut self, values: &mut PetValues, cfg: &NeedsConfig, delta: f32) -> NeedsOutcome {
        let dn = &cfg.dimensions;
        values.satiety = (values.satiety + delta).clamp(dn.satiety.min, dn.satiety.max);
        self.observe_bands(values, cfg)
    }

    /// 重建分档视图（零分配）并做跨档判定。
    #[must_use]
    pub fn effects<'a>(&self, cfg: &'a NeedsConfig, values: &PetValues) -> BandEffects<'a> {
        BandEffects::from_cfg(cfg, values.satiety, values.cleanliness)
    }

    /// 移动速度乘子（`01 §6.12.2`：饥饿 <20 → ×0.85）。
    #[must_use]
    pub fn speed_mul(&self, cfg: &NeedsConfig, values: &PetValues) -> f32 {
        self.effects(cfg, values).speed_mul
    }

    /// 逐拍重建分档基线并给出跨档标志。
    fn observe_bands(&mut self, values: &PetValues, cfg: &NeedsConfig) -> NeedsOutcome {
        let s_band = bands::satiety_band(values.satiety, cfg);
        let c_band = bands::clean_band(values.cleanliness, cfg);
        let changed = match (self.last_satiety, self.last_clean) {
            // 首拍：只建立基线（`01 §6.12` 无「启动即跨档」语义）。
            (None, _) | (_, None) => false,
            (Some(prev_s), Some(prev_c)) => prev_s != s_band || prev_c != c_band,
        };
        self.last_satiety = Some(s_band);
        self.last_clean = Some(c_band);
        NeedsOutcome {
            satiety: values.satiety,
            cleanliness: values.cleanliness,
            satiety_band: s_band,
            clean_band: c_band,
            band_changed: changed,
        }
    }
}

/// 用餐倍率（`true` → 配置倍率，`false` → 1.0）。
#[inline]
fn meal_mul(m: f32, on: bool) -> f32 {
    if on {
        m
    } else {
        1.0
    }
}

/// 活动倍率。
#[inline]
fn activity_mul(m: f32, on: bool) -> f32 {
    if on {
        m
    } else {
        1.0
    }
}

/// 睡眠倍率。
#[inline]
fn sleep_mul(m: f32, on: bool) -> f32 {
    if on {
        m
    } else {
        1.0
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 分钟 → 毫秒。
    const MIN_MS: i64 = 60_000;

    fn cfg() -> NeedsConfig {
        NeedsConfig::default()
    }

    fn values() -> PetValues {
        PetValues::default()
    }

    #[test]
    fn satiety_decays_at_b01_rate_over_24h() {
        // AC（03 卡片）：全天不喂 → 衰减 ≈58~72 点（不扣睡眠 72，扣 8h 睡眠 ≈58）。
        let c = cfg();
        let mut v = values();
        // 起点取上限：全天不喂会跌破 0，若从默认 70 起步则被下限 clamp 掩盖真实速率。
        v.satiety = 100.0;
        let start = v.satiety;
        let mut sys = NeedsSystem::new();
        // 1440 分钟，逐分钟推进（避免大 dt 掩盖公式误差）。
        for i in 0..1_440 {
            let env = NeedsEnv::new(i * MIN_MS, false, false, false);
            let _ = sys.tick(&mut v, &c, 1.0, &env);
        }
        let drop = start - v.satiety;
        assert!(
            (drop - 72.0).abs() < 1e-2,
            "全天 1440min × 0.05 = 72 点，实际 {drop}"
        );
        // AC 区间 58~72（上界放宽 0.05 容纳 f32 逐分钟累加舍入）。
        assert!((58.0..=72.05).contains(&drop), "落在 AC 区间 58~72：{drop}");
    }

    #[test]
    fn sleep_multiplier_reduces_daily_drain_to_about_58() {
        // 16h 清醒 + 8h 睡眠：0.05×(960 + 480×0.4) = 57.6 ≈ 58（`02 §5.9` 日消耗核算）。
        let c = cfg();
        let mut v = values();
        v.satiety = 100.0;
        let start = v.satiety;
        let mut sys = NeedsSystem::new();
        for i in 0..960 {
            let env = NeedsEnv::new(i * MIN_MS, false, false, false);
            let _ = sys.tick(&mut v, &c, 1.0, &env);
        }
        for i in 960..1_440 {
            let env = NeedsEnv::new(i * MIN_MS, false, false, true);
            let _ = sys.tick(&mut v, &c, 1.0, &env);
        }
        let drop = start - v.satiety;
        assert!((drop - 57.6).abs() < 1e-2, "扣睡眠后 ≈57.6，实际 {drop}");
    }

    #[test]
    fn meal_and_activity_multipliers_apply_per_dimension() {
        let c = cfg();
        let mut v = values();
        let mut sys = NeedsSystem::new();
        // 单分钟，用餐 + 活动：satiety 速率 = -0.05 × 2.5 × 2.0 = -0.25；
        // cleanliness = -0.08 × 1.0 × 2.0 = -0.16。
        let env = NeedsEnv::new(0, true, true, false);
        let _ = sys.tick(&mut v, &c, 1.0, &env);
        assert!((v.satiety - (70.0 - 0.25)).abs() < 1e-5, "satiety={}", v.satiety);
        assert!((v.cleanliness - (85.0 - 0.16)).abs() < 1e-5, "cleanliness={}", v.cleanliness);
    }

    #[test]
    fn cleanliness_decays_at_008_per_min() {
        let c = cfg();
        let mut v = values();
        let mut sys = NeedsSystem::new();
        let env = NeedsEnv::new(0, false, false, false);
        for _ in 0..60 {
            let _ = sys.tick(&mut v, &c, 1.0, &env);
        }
        // 逐分钟累加 60 次会有 f32 舍入（~1e-4 量级），容差取 1e-3。
        assert!((85.0 - v.cleanliness - 4.8).abs() < 1e-3, "60min × 0.08 = 4.8：{}", v.cleanliness);
    }

    #[test]
    fn clean_slow_halves_decay_until_deadline() {
        let c = cfg();
        let mut v = values();
        let mut sys = NeedsSystem::new();
        sys.mark_clean_slow(10 * MIN_MS, 0);
        // 冷却期内：速率减半（-0.04/min）。
        let _ = sys.tick(&mut v, &c, 10.0, &NeedsEnv::new(5 * MIN_MS, false, false, false));
        assert!((85.0 - v.cleanliness - 0.4).abs() < 1e-5, "10min × 0.04 = 0.4");
        // 冷却期后：恢复全速。
        let _ = sys.tick(&mut v, &c, 10.0, &NeedsEnv::new(20 * MIN_MS, false, false, false));
        assert!((85.0 - v.cleanliness - (0.4 + 0.8)).abs() < 1e-5, "再 10min × 0.08 = 0.8");
    }

    #[test]
    fn event_deltas_match_config_table() {
        let c = cfg();
        let mut sys = NeedsSystem::new();
        let expect = [
            (NeedCause::Roll, -8.0),
            (NeedCause::ThrownLand, -5.0),
            (NeedCause::Eat, -3.0),
            (NeedCause::Study, -2.0),
            (NeedCause::Trip, -15.0),
        ];
        for (cause, delta) in expect {
            let mut v = values();
            let _ = sys.apply_event(&mut v, &c, cause);
            assert!(
                (v.cleanliness - (85.0 + delta)).abs() < 1e-5,
                "{:?} 应扣 {delta}：{}",
                cause,
                v.cleanliness
            );
        }
        // 原因键必须与配置表逐字一致。
        for cause in NeedCause::ALL {
            assert!(c.event_deltas.contains_key(cause.key()), "缺配置键 {}", cause.key());
        }
    }

    #[test]
    fn band_change_is_reported_once_per_crossing() {
        let c = cfg();
        let mut v = values();
        let mut sys = NeedsSystem::new();
        let env = NeedsEnv::new(0, false, false, false);
        // 首拍只建基线（5 分钟：70 → 69.75，仍在 full 档）。
        v.satiety = 75.0;
        let first = sys.tick(&mut v, &c, 5.0, &env);
        assert!(!first.band_changed, "首拍不报跨档");
        assert_eq!(first.satiety_band, SatietyBand::Full);
        // 继续推进至 < 70（normal 档）→ 报一次跨档。
        let crossed = sys.tick(&mut v, &c, 120.0, &env);
        assert!(crossed.band_changed, "跌破 70 应跨档");
        assert_eq!(crossed.satiety_band, SatietyBand::Normal);
        assert!(crossed.satiety < 70.0, "已跌破 full 档下界：{}", crossed.satiety);
        // 同档内继续推进 → 不重复报。
        let again = sys.tick(&mut v, &c, 10.0, &env);
        assert!(!again.band_changed, "同档内不重复报跨档");
    }

    #[test]
    fn dt_zero_or_negative_does_not_move_values() {
        let c = cfg();
        let mut v = values();
        let mut sys = NeedsSystem::new();
        let before = (v.satiety, v.cleanliness);
        let _ = sys.tick(&mut v, &c, 0.0, &NeedsEnv::default());
        let _ = sys.tick(&mut v, &c, -5.0, &NeedsEnv::default());
        let _ = sys.tick(&mut v, &c, f32::NAN, &NeedsEnv::default());
        assert_eq!((v.satiety, v.cleanliness), before, "非法 dt 不得推进");
    }

    /// **AC-32 产品红线**：`satiety=0` 且 `cleanliness=0` 连续 72 小时——
    /// 不死亡 / 不消失 / 不崩溃 / 数值不永久损失。
    #[test]
    fn ac32_zero_needs_for_72h_never_dies_or_breaks_red_line() {
        let c = cfg();
        let mut v = values();
        let mut sys = NeedsSystem::new();
        v.satiety = 0.0;
        v.cleanliness = 0.0;
        let mood_before = v.mood;
        let energy_before = v.energy;
        // 72h = 4320 分钟，逐分钟推进（含用餐 / 活动 / 睡眠三态轮换，覆盖全部倍率分支）。
        for i in 0..4_320i64 {
            let env = NeedsEnv::new(i * MIN_MS, i % 3 == 0, i % 5 == 0, i % 7 == 0);
            let out = sys.tick(&mut v, &c, 1.0, &env);
            assert!(out.satiety.is_finite() && out.cleanliness.is_finite(), "数值必须有限");
            assert_eq!(v.satiety, 0.0, "归零后不得为负（第 {i} 分钟）");
            assert_eq!(v.cleanliness, 0.0, "归零后不得为负（第 {i} 分钟）");
        }
        // 需求推进**不触碰** Mood / Energy（不设计死亡即不产生「归零惩罚」）。
        assert_eq!(v.mood, mood_before, "需求归零不得扣心情（红线）");
        assert_eq!(v.energy, energy_before, "需求归零不得扣精力（红线）");
        // 归零态下仍能给出可用档位（不 panic、不返回空）。
        let e = sys.effects(&c, &v);
        assert_eq!(e.satiety_band, SatietyBand::Starving);
        assert_eq!(e.clean_band, CleanBand::Filthy);
        assert!(e.denies_dispatch(DispatchKind::Work), "委屈态拒绝打工（但不死亡）");
        assert!(e.denies_dispatch(DispatchKind::Study));
        assert!(e.denies_dispatch(DispatchKind::Travel));
    }

    #[test]
    fn values_adders_clamp_and_never_overflow() {
        let c = cfg();
        let mut v = values();
        let mut sys = NeedsSystem::new();
        let out = sys.add_satiety(&mut v, &c, 1_000.0);
        assert_eq!(out.satiety, 100.0, "上限 clamp");
        let out = sys.add_cleanliness(&mut v, &c, -1_000.0);
        assert_eq!(out.cleanliness, 0.0, "下限 clamp");
    }

    #[test]
    fn cooldowns_round_trip_through_save_section() {
        let c = cfg();
        let mut sys = NeedsSystem::new();
        sys.mark_bath(&c, 1_000_000);
        sys.mark_scent_buff(12, 1_000_000);
        sys.mark_clean_slow(60_000, 1_000_000);

        let saved = sys.to_save();
        assert_eq!(saved.bath_free_cd_until_ms, 1_000_000 + 30 * MIN_MS, "freeCdMin=30");
        assert_eq!(saved.scent_buff_until_ms, 1_000_000 + 12 * MIN_MS);
        assert_eq!(saved.clean_slow_until_ms, 1_000_000 + MIN_MS);

        let restored = NeedsSystem::restore_from(&saved);
        assert_eq!(restored.to_save(), saved, "冷却段往返不丢");
        assert!(!restored.bath_available(1_000_000 + 30 * MIN_MS - 1));
        assert!(restored.bath_available(1_000_000 + 30 * MIN_MS));
        assert!(restored.scent_buff_active(1_000_000 + 11 * MIN_MS));
        assert!(!restored.scent_buff_active(1_000_000 + 12 * MIN_MS));
        // 未设冷却的系统：默认可用、无 Buff。
        let fresh = NeedsSystem::new();
        assert!(fresh.bath_available(0));
        assert!(!fresh.scent_buff_active(0));
    }

    #[test]
    fn dispatch_kind_ids_match_deny_literals() {
        let c = cfg();
        // 配置 deny 列表的字面量必须与 `DispatchKind::id()` 同词表（C7）。
        let literals: Vec<&str> = c
            .bands
            .satiety
            .iter()
            .chain(c.bands.cleanliness.iter())
            .flat_map(|b| b.deny.iter().map(|s| s.as_str()))
            .collect();
        assert!(!literals.is_empty(), "默认配置必须存在拒绝项");
        for lit in literals {
            assert!(DispatchKind::from_id(lit).is_some(), "未知派遣类别字面量：{lit}");
        }
        assert_eq!(DispatchKind::from_id("nap"), None);
    }

    #[test]
    fn needs_system_is_pod_ish_and_restore_clears_band_baseline() {
        let saved = NeedsSave {
            bath_free_cd_until_ms: 5,
            scent_buff_until_ms: 6,
            clean_slow_until_ms: 7,
        };
        let sys = NeedsSystem::restore_from(&saved);
        // 恢复后分档基线为空：下一次 observe 不报跨档（避免「重启即跨档」假事件）。
        assert_eq!(sys.last_satiety, None);
        assert_eq!(sys.last_clean, None);
    }
}
