//! 生存需求六维系统的**分档**层（S7-M2，T-18 段 · 上 / `01 §6.12.2` / §6.12.3 / `02 §5.11`）。
//!
//! 职责：
//!   - 档位枚举 [`SatietyBand`] / [`CleanBand`]（五档，ID 与 `needs.json.bands.*[].id`
//!     **逐字一致**，C7 单一真源）；
//!   - 档位解析 [`satiety_band`] / [`clean_band`]：按 `min` 降序取首个命中档；
//!   - 分档表现聚合 [`BandEffects`]：把「饱食 + 清洁」两档的全部系统影响收敛为
//!     一个**零分配**视图（含 `action` 候选与 `deny` 判定），供动作仲裁 / 运动 /
//!     活动派遣按需消费。
//!
//! 边界（03 台账 S7-M2「禁止顺手改动」）：
//!   - 本模块**不含**数值推进（归 [`super`] 的 `tick` / `apply_event`）；
//!   - **不含**耦合矩阵求值（归 [`super::coupling`]，S7-M3）；
//!   - **不含**动作提交（N 系列动作接入归 S7-M9），只给出「该触发哪个动作 ID」。
//!
//! 时间纪律（C3）：本模块**零时钟**（纯值 → 值映射）。

use crate::config::model::{NeedBandCfg, NeedsConfig};

// ---------------------------------------------------------------------------
// 档位枚举
// ---------------------------------------------------------------------------

/// 饱食度档位（`needs.json.bands.satiety` 五档，`01 §6.12.2`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SatietyBand {
    /// 满足（> 70）。
    Full,
    /// 正常（40~70）。
    Normal,
    /// 有点饿（20~40）：触发 `ACT-N-01` 讨食。
    Peckish,
    /// 饥饿（5~20）：讨食高频 + 走路变慢（打工收益折扣见耦合矩阵）。
    Hungry,
    /// 饿到委屈（< 5）：拒绝派遣 + 委屈情绪表现；**不死亡**。
    Starving,
}

/// 清洁度档位（`needs.json.bands.cleanliness` 五档，`01 §6.12.3`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CleanBand {
    /// 清爽（> 70）。
    Fresh,
    /// 正常（50~70）。
    Normal,
    /// 有污渍（30~50）：`ACT-N-04` 脏污待机。
    Stained,
    /// 明显脏（< 30）：`ACT-N-05` 挠痒。
    Dirty,
    /// 很脏（< 15）：`ACT-N-06` 求洗澡 + 拒绝被抚摸增益。
    Filthy,
}

impl SatietyBand {
    /// 全部档位（高 → 低，与配置顺序一致）。
    pub const ALL: [SatietyBand; 5] = [
        SatietyBand::Full,
        SatietyBand::Normal,
        SatietyBand::Peckish,
        SatietyBand::Hungry,
        SatietyBand::Starving,
    ];

    /// 档位 ID（与 `needs.json.bands.satiety[].id` 逐字一致）。
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            SatietyBand::Full => "full",
            SatietyBand::Normal => "normal",
            SatietyBand::Peckish => "peckish",
            SatietyBand::Hungry => "hungry",
            SatietyBand::Starving => "starving",
        }
    }

    /// 由 ID 反查（未知 ID → `None`，不 panic）。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        SatietyBand::ALL.into_iter().find(|b| b.id() == id)
    }
}

impl CleanBand {
    /// 全部档位（高 → 低，与配置顺序一致）。
    pub const ALL: [CleanBand; 5] = [
        CleanBand::Fresh,
        CleanBand::Normal,
        CleanBand::Stained,
        CleanBand::Dirty,
        CleanBand::Filthy,
    ];

    /// 档位 ID（与 `needs.json.bands.cleanliness[].id` 逐字一致）。
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            CleanBand::Fresh => "fresh",
            CleanBand::Normal => "normal",
            CleanBand::Stained => "stained",
            CleanBand::Dirty => "dirty",
            CleanBand::Filthy => "filthy",
        }
    }

    /// 由 ID 反查（未知 ID → `None`，不 panic）。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        CleanBand::ALL.into_iter().find(|b| b.id() == id)
    }
}

// ---------------------------------------------------------------------------
// 档位解析
// ---------------------------------------------------------------------------

/// 按 `min` 降序取首个 `value >= min` 的档（`01 §6.12.2/3` 的阈值语义）。
///
/// 配置顺序即优先级（`needs.json` 从高到低书写）；实现上**显式求最大值**而非依赖
/// 数组顺序，避免「配置被重排即行为改变」的隐式耦合。
#[must_use]
pub fn resolve_band(bands: &[NeedBandCfg], value: f32) -> Option<&NeedBandCfg> {
    bands
        .iter()
        .filter(|b| value >= b.min)
        .max_by(|a, b| a.min.partial_cmp(&b.min).unwrap_or(std::cmp::Ordering::Equal))
}

/// 饱食度档位解析（配置缺失 / 非法 → 按阈值映射的**内置默认档**，不 panic）。
#[must_use]
pub fn satiety_band(value: f32, cfg: &NeedsConfig) -> SatietyBand {
    resolve_band(&cfg.bands.satiety, value)
        .and_then(|b| SatietyBand::from_id(&b.id))
        .unwrap_or_else(|| default_satiety_band(value))
}

/// 清洁度档位解析（配置缺失 / 非法 → 内置默认档，不 panic）。
#[must_use]
pub fn clean_band(value: f32, cfg: &NeedsConfig) -> CleanBand {
    resolve_band(&cfg.bands.cleanliness, value)
        .and_then(|b| CleanBand::from_id(&b.id))
        .unwrap_or_else(|| default_clean_band(value))
}

/// 内置默认饱食档（`01 §6.12.2` 阈值：70 / 40 / 20 / 5）。
#[must_use]
pub fn default_satiety_band(value: f32) -> SatietyBand {
    match value {
        v if v >= 70.0 => SatietyBand::Full,
        v if v >= 40.0 => SatietyBand::Normal,
        v if v >= 20.0 => SatietyBand::Peckish,
        v if v >= 5.0 => SatietyBand::Hungry,
        _ => SatietyBand::Starving,
    }
}

/// 内置默认清洁档（`01 §6.12.3` 阈值：70 / 50 / 30 / 15）。
#[must_use]
pub fn default_clean_band(value: f32) -> CleanBand {
    match value {
        v if v >= 70.0 => CleanBand::Fresh,
        v if v >= 50.0 => CleanBand::Normal,
        v if v >= 30.0 => CleanBand::Stained,
        v if v >= 15.0 => CleanBand::Dirty,
        _ => CleanBand::Filthy,
    }
}

// ---------------------------------------------------------------------------
// 分档表现聚合
// ---------------------------------------------------------------------------

/// 分档表现聚合视图（零分配；数值全部取自 `needs.json`，C7）。
///
/// 两维乘子合成口径（`02 §5.10` `coupling.combine` 同源语义）：
///   - `mood_decay_mul` 取**两者最大**（`combine.moodDecay = "max"`，避免重度饥饿
///     与重度脏污的加速被对方稀释）；
///   - 其余（`job_reward_mul` / `speed_mul` / `stroke_gain_mul` / `affinity_gain_mul`）
///     取**乘积**（`combine.* = "mul"`），未定义项等价乘子 1.0。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandEffects<'a> {
    /// 当前饱食档位。
    pub satiety_band: SatietyBand,
    /// 当前饱食档配置（提供 `action` / `intervalSec` / `deny` / `emotion`；
    /// `None` = 配置缺该档，此时全部修正视为无）。
    pub satiety_cfg: Option<&'a NeedBandCfg>,
    /// 当前清洁档位。
    pub clean_band: CleanBand,
    /// 当前清洁档配置（同 `satiety_cfg` 语义）。
    pub clean_cfg: Option<&'a NeedBandCfg>,
    /// 心情衰减乘子（两维取 max）。
    pub mood_decay_mul: f32,
    /// 打工收益乘子（两维取积）。
    pub job_reward_mul: f32,
    /// 移动速度乘子（两维取积）。
    pub speed_mul: f32,
    /// 抚摸增益乘子（两维取积）。
    pub stroke_gain_mul: f32,
    /// 亲密度增益乘子（两维取积）。
    pub affinity_gain_mul: f32,
}

impl<'a> BandEffects<'a> {
    /// 由配置 + 当前数值构造（数值不在区间内亦不 panic：档位解析含默认兜底；
    /// 配置缺档 → 修正项全部退化为「无修正」）。
    #[must_use]
    pub fn from_cfg(cfg: &'a NeedsConfig, satiety: f32, cleanliness: f32) -> Self {
        let s_band = satiety_band(satiety, cfg);
        let c_band = clean_band(cleanliness, cfg);
        let s_cfg = band_cfg(&cfg.bands.satiety, s_band.id());
        let c_cfg = band_cfg(&cfg.bands.cleanliness, c_band.id());
        Self {
            satiety_band: s_band,
            satiety_cfg: s_cfg,
            clean_band: c_band,
            clean_cfg: c_cfg,
            mood_decay_mul: mul_max(field(s_cfg, |b| b.mood_decay_mul), field(c_cfg, |b| b.mood_decay_mul)),
            job_reward_mul: mul_mul(field(s_cfg, |b| b.job_reward_mul), field(c_cfg, |b| b.job_reward_mul)),
            speed_mul: mul_mul(field(s_cfg, |b| b.speed_mul), field(c_cfg, |b| b.speed_mul)),
            stroke_gain_mul: mul_mul(field(s_cfg, |b| b.stroke_gain_mul), field(c_cfg, |b| b.stroke_gain_mul)),
            affinity_gain_mul: mul_mul(field(s_cfg, |b| b.affinity_gain_mul), field(c_cfg, |b| b.affinity_gain_mul)),
        }
    }

    /// 当前应触发的动作候选（`01 §6.12.2/3`）：`[饱食档动作, 清洁档动作]`。
    ///
    /// 交给仲裁器按 `actions.json` 优先级定夺（N 系列动作接入归 S7-M9）；
    /// 本模块**不代仲裁**、不做优先级排序。
    #[must_use]
    pub fn action_candidates(&self) -> [Option<&'a str>; 2] {
        [
            self.satiety_cfg.and_then(|c| c.action.as_deref()),
            self.clean_cfg.and_then(|c| c.action.as_deref()),
        ]
    }

    /// 当前档位要求的动作触发间隔（秒；饱食档优先，缺省取清洁档）。
    #[must_use]
    pub fn interval_sec(&self) -> Option<[u64; 2]> {
        self.satiety_cfg
            .and_then(|c| c.interval_sec)
            .or_else(|| self.clean_cfg.and_then(|c| c.interval_sec))
    }

    /// 当前档位要求的污渍贴片数量区间（清洁档，`None` = 不显示污渍）。
    #[must_use]
    pub fn patches(&self) -> Option<[u32; 2]> {
        self.clean_cfg.and_then(|c| c.patches)
    }

    /// 当前档位强制情绪态（`01 §6.12.2`：`starving` → `Aggrieved`）。
    #[must_use]
    pub fn forced_emotion(&self) -> Option<&'a str> {
        self.satiety_cfg
            .and_then(|c| c.emotion.as_deref())
            .or_else(|| self.clean_cfg.and_then(|c| c.emotion.as_deref()))
    }

    /// 当前档位要求的附加视觉特效（`filthy` → `flies`）。
    #[must_use]
    pub fn effect(&self) -> Option<&'a str> {
        self.clean_cfg.and_then(|c| c.effect.as_deref())
    }

    /// 该派遣类别是否被当前分档拒绝（两维 `deny` 列表取并集）。
    #[must_use]
    pub fn denies_dispatch(&self, kind: super::DispatchKind) -> bool {
        let id = kind.id();
        let in_cfg = |c: Option<&'a NeedBandCfg>| {
            c.is_some_and(|c| c.deny.iter().any(|d| d == id))
        };
        in_cfg(self.satiety_cfg) || in_cfg(self.clean_cfg)
    }

    /// 污点贴片判定：当前清洁档是否要求显示「苍蝇 / 问号」类密集特效态。
    #[must_use]
    pub fn is_visibly_filthy(&self) -> bool {
        self.clean_band == CleanBand::Filthy
    }
}

/// 取配置中指定 ID 的档位（缺失 → `None`；调用方按「无修正」处理）。
fn band_cfg<'a>(bands: &'a [NeedBandCfg], id: &str) -> Option<&'a NeedBandCfg> {
    bands.iter().find(|b| b.id == id)
}

/// 从（可能缺失的）档配置里取某个 `Option<f32>` 乘子。
fn field<F>(cfg: Option<&NeedBandCfg>, pick: F) -> Option<f32>
where
    F: FnOnce(&NeedBandCfg) -> Option<f32>,
{
    cfg.and_then(pick)
}

/// 两乘子取最大（`None` 视为 1.0）。
#[inline]
fn mul_max(a: Option<f32>, b: Option<f32>) -> f32 {
    a.unwrap_or(1.0).max(b.unwrap_or(1.0))
}

/// 两乘子取积（`None` 视为 1.0）。
#[inline]
fn mul_mul(a: Option<f32>, b: Option<f32>) -> f32 {
    a.unwrap_or(1.0) * b.unwrap_or(1.0)
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::needs::DispatchKind;

    fn cfg() -> NeedsConfig {
        NeedsConfig::default()
    }

    #[test]
    fn band_ids_match_config_ids() {
        let c = cfg();
        let s_ids: Vec<&str> = c.bands.satiety.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(s_ids, ["full", "normal", "peckish", "hungry", "starving"]);
        let c_ids: Vec<&str> = c.bands.cleanliness.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(c_ids, ["fresh", "normal", "stained", "dirty", "filthy"]);
        for b in SatietyBand::ALL {
            assert!(s_ids.contains(&b.id()), "枚举 ID 必须在配置中：{}", b.id());
            assert_eq!(SatietyBand::from_id(b.id()), Some(b));
        }
        for b in CleanBand::ALL {
            assert!(c_ids.contains(&b.id()), "枚举 ID 必须在配置中：{}", b.id());
            assert_eq!(CleanBand::from_id(b.id()), Some(b));
        }
        assert_eq!(SatietyBand::from_id("nope"), None);
        assert_eq!(CleanBand::from_id("nope"), None);
    }

    #[test]
    fn satiety_band_boundaries_follow_spec() {
        let c = cfg();
        // `01 §6.12.2`：>70 满足 / 40~70 正常 / 20~40 有点饿 / 5~20 饥饿 / <5 委屈。
        assert_eq!(satiety_band(100.0, &c), SatietyBand::Full);
        assert_eq!(satiety_band(70.0, &c), SatietyBand::Full);
        assert_eq!(satiety_band(69.9, &c), SatietyBand::Normal);
        assert_eq!(satiety_band(40.0, &c), SatietyBand::Normal);
        assert_eq!(satiety_band(39.9, &c), SatietyBand::Peckish);
        assert_eq!(satiety_band(20.0, &c), SatietyBand::Peckish);
        assert_eq!(satiety_band(19.9, &c), SatietyBand::Hungry);
        assert_eq!(satiety_band(5.0, &c), SatietyBand::Hungry);
        assert_eq!(satiety_band(4.9, &c), SatietyBand::Starving);
        assert_eq!(satiety_band(0.0, &c), SatietyBand::Starving);
    }

    #[test]
    fn clean_band_boundaries_follow_spec() {
        let c = cfg();
        // `01 §6.12.3`：>70 清爽 / 50~70 正常 / 30~50 有污渍 / <30 明显脏 / <15 很脏。
        assert_eq!(clean_band(100.0, &c), CleanBand::Fresh);
        assert_eq!(clean_band(70.0, &c), CleanBand::Fresh);
        assert_eq!(clean_band(69.9, &c), CleanBand::Normal);
        assert_eq!(clean_band(50.0, &c), CleanBand::Normal);
        assert_eq!(clean_band(49.9, &c), CleanBand::Stained);
        assert_eq!(clean_band(30.0, &c), CleanBand::Stained);
        assert_eq!(clean_band(29.9, &c), CleanBand::Dirty);
        assert_eq!(clean_band(15.0, &c), CleanBand::Dirty);
        assert_eq!(clean_band(14.9, &c), CleanBand::Filthy);
        assert_eq!(clean_band(0.0, &c), CleanBand::Filthy);
    }

    #[test]
    fn band_resolution_survives_broken_config() {
        let mut c = cfg();
        c.bands.satiety.clear();
        c.bands.cleanliness.clear();
        // 配置被清空 → 内置默认阈值兜底（不 panic、不返回空）。
        assert_eq!(satiety_band(10.0, &c), SatietyBand::Hungry);
        assert_eq!(clean_band(10.0, &c), CleanBand::Filthy);
        let e = BandEffects::from_cfg(&c, 10.0, 10.0);
        assert_eq!(e.satiety_band, SatietyBand::Hungry);
        assert_eq!(e.clean_band, CleanBand::Filthy);
        assert_eq!(e.mood_decay_mul, 1.0, "无配置档 → 无乘子修正");
    }

    #[test]
    fn band_effects_combine_max_for_mood_and_mul_for_others() {
        let c = cfg();
        // 饥饿（satiety=10 → moodDecay 1.8 / jobReward 0.7 / speed 0.85）
        // × 很脏（cleanliness=10 → moodDecay 1.3 / jobReward 0.6 / stroke 0.5 / affinity 0.5）
        let e = BandEffects::from_cfg(&c, 10.0, 10.0);
        assert_eq!(e.satiety_band, SatietyBand::Hungry);
        assert_eq!(e.clean_band, CleanBand::Filthy);
        assert!((e.mood_decay_mul - 1.8).abs() < 1e-6, "取 max：{}", e.mood_decay_mul);
        assert!((e.job_reward_mul - 0.42).abs() < 1e-6, "取积 0.7×0.6：{}", e.job_reward_mul);
        assert!((e.speed_mul - 0.85).abs() < 1e-6);
        assert!((e.stroke_gain_mul - 0.5).abs() < 1e-6);
        assert!((e.affinity_gain_mul - 0.5).abs() < 1e-6);
    }

    #[test]
    fn band_effects_full_and_fresh_have_no_modifiers() {
        let c = cfg();
        let e = BandEffects::from_cfg(&c, 100.0, 100.0);
        assert_eq!(e.satiety_band, SatietyBand::Full);
        assert_eq!(e.clean_band, CleanBand::Fresh);
        assert_eq!(e.mood_decay_mul, 1.0);
        assert_eq!(e.job_reward_mul, 1.0);
        assert_eq!(e.speed_mul, 1.0);
        assert!(e.action_candidates() == [None, None]);
        assert!(e.patches().is_none());
        assert!(e.forced_emotion().is_none());
    }

    #[test]
    fn band_effects_expose_action_candidates_and_effects() {
        let c = cfg();
        // 有点饿（20~40）→ ACT-N-01。
        let e = BandEffects::from_cfg(&c, 30.0, 80.0);
        assert_eq!(e.action_candidates(), [Some("ACT-N-01"), None]);
        assert_eq!(e.interval_sec(), Some([180, 480]));
        // 有污渍（30~50）→ ACT-N-04 + 污渍 1~2 处。
        let e = BandEffects::from_cfg(&c, 80.0, 40.0);
        assert_eq!(e.action_candidates(), [None, Some("ACT-N-04")]);
        assert_eq!(e.patches(), Some([1, 2]));
        // 很脏（<15）→ ACT-N-06 + 苍蝇特效 + Aggrieved 由饱食档给出。
        let e = BandEffects::from_cfg(&c, 3.0, 10.0);
        assert_eq!(e.action_candidates(), [Some("ACT-N-01"), Some("ACT-N-06")]);
        assert_eq!(e.effect(), Some("flies"));
        assert!(e.is_visibly_filthy());
        assert_eq!(e.forced_emotion(), Some("Aggrieved"));
        assert_eq!(e.patches(), Some([4, 6]));
    }

    #[test]
    fn band_effects_deny_union_matches_spec() {
        let c = cfg();
        // `01 §6.12.4`：Satiety<5 → 拒绝 全部三种派遣；Cleanliness<15 → 拒绝 study。
        let e = BandEffects::from_cfg(&c, 3.0, 80.0);
        for k in [DispatchKind::Work, DispatchKind::Study, DispatchKind::Travel] {
            assert!(e.denies_dispatch(k), "挨饿必须拒绝 {:?}", k);
        }
        let e = BandEffects::from_cfg(&c, 80.0, 10.0);
        assert!(e.denies_dispatch(DispatchKind::Study));
        assert!(!e.denies_dispatch(DispatchKind::Work));
        assert!(!e.denies_dispatch(DispatchKind::Travel));
        // 正常态不拒绝任何派遣。
        let e = BandEffects::from_cfg(&c, 80.0, 80.0);
        for k in [DispatchKind::Work, DispatchKind::Study, DispatchKind::Travel] {
            assert!(!e.denies_dispatch(k));
        }
    }
}
