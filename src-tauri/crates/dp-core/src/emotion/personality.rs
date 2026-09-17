//! `emotion::personality`：五维隐藏性格生成、持久化与系数换算（`02 §5.1` / §5.4；`01 §6.11.6`）。
//!
//! ## 五维与影响面（`01 §6.11.6`）
//!
//! | 维度 | 符号 | 本模块输出的系数 |
//! |---|---|---|
//! | 粘人度 | c | `personalityFactor = A + B×c`；`T_exp0 = baseMin − clingyCoef×c` |
//! | 好奇心 | q | 微动 / 彩蛋概率、旅游稀有道具（**消费点在 S8 / 展示层**） |
//! | 脾气 | t | `thresholdScale = 1.2 − 0.4t`；`moodDeltaScale = 0.8 + 0.4t` |
//! | 胆量 | b | 旅游负面事件、甩出惩罚（**消费点在 S8**） |
//! | 勤快 | d | 打工 / 学习收益与体力消耗（**消费点在 S8**） |
//!
//! ## 首次创建随机（`01 §6.11.6` / §6.11.13）
//!
//! 只有**粘人度**参与首次随机（区间 `clingyInitMin~Max` = 45~55），其余四维取出厂建议值。
//! 理由：粘人度直接决定 P 速率与「期望互动间隔」，出厂区间 45~55 使
//! `personalityFactor ∈ [0.96, 1.04]`（触发时间偏移 ≤±4%），**与 AC-16 的 ±10% 容差自洽**；
//! 若首建就放开 0~100，AC-16 将在出厂随机下失守。重掷（S10 设置页长按名字）才放开全区间。
//!
//! ## 时间纪律（C3）
//!
//! 零时钟、零第三方随机：随机数用 `dp-core` 内置 [`SplitMix64`]（与运动引擎同一实现），
//! 种子由调用方注入（生产取注入的 `now_ms`，测试取常量）。
//!
//! ## 边界
//!
//! 本模块**不**持有重掷次数记账（`rerollPerDay` 的日切计数与设置页入口归 S10）；
//! `reroll` 只交付「五维抖动」这一纯运算。

use crate::config::model::PersonalityCfg;
use crate::motion::SplitMix64;

/// 五维隐藏性格（归一化值，各维 ∈ [0, 1]）。
///
/// **不含 serde 派生**：存档形状的唯一真源是 `save::schema::PersonalitySave`（字段逐项同名），
/// 两侧经显式转换函数搬运，避免同一份数据出现两套 serde 契约。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Personality {
    /// 粘人度（影响 P 速率与期望互动间隔）。
    pub clingy: f32,
    /// 好奇心。
    pub curiosity: f32,
    /// 脾气（影响阶段阈值比较值与 Mood 扣减幅度）。
    pub temper: f32,
    /// 胆量。
    pub courage: f32,
    /// 勤快。
    pub diligence: f32,
}

impl Default for Personality {
    /// 与 `emotion.json.personality.defaults` 同值（`02 §5.7` 冻结五维默认）。
    fn default() -> Self {
        let d = crate::config::model::PersonalityDefaultsCfg::default();
        Self {
            clingy: d.clingy,
            curiosity: d.curiosity,
            temper: d.temper,
            courage: d.courage,
            diligence: d.diligence,
        }
    }
}

impl Personality {
    /// 由配置默认值构造（出厂五维建议值）。
    #[must_use]
    pub fn from_cfg(cfg: &PersonalityCfg) -> Self {
        Self {
            clingy: cfg.defaults.clingy,
            curiosity: cfg.defaults.curiosity,
            temper: cfg.defaults.temper,
            courage: cfg.defaults.courage,
            diligence: cfg.defaults.diligence,
        }
    }

    /// 五维整体钳到 `[0, 1]`（存档读入 / 重掷后调用）。
    pub fn clamp(&mut self) {
        for v in [
            &mut self.clingy,
            &mut self.curiosity,
            &mut self.temper,
            &mut self.courage,
            &mut self.diligence,
        ] {
            if !v.is_finite() {
                *v = 0.5;
            } else {
                *v = v.clamp(0.0, 1.0);
            }
        }
    }

    /// `personalityFactor = A + B × 粘人度`（`02 §5.2` 因子 F8）。
    #[must_use]
    pub fn factor(&self, cfg: &PersonalityCfg) -> f32 {
        let f = cfg.clingy_coef_a + cfg.clingy_coef_b * self.clingy;
        if f.is_finite() {
            f.max(0.0)
        } else {
            1.0
        }
    }

    /// `thresholdScale = base − temperCoef × 脾气`（`02 §5.3`；`temper=0.5` 时 = 1.0）。
    #[must_use]
    pub fn threshold_scale(&self, cfg: &PersonalityCfg) -> f32 {
        let s = cfg.threshold_scale_base - cfg.threshold_scale_temper * self.temper;
        if s > 0.0 {
            s
        } else {
            1.0
        }
    }

    /// `moodDeltaScale = base + temperCoef × 脾气`（`02 §5.3`）。
    #[must_use]
    pub fn mood_delta_scale(&self, cfg: &PersonalityCfg) -> f32 {
        cfg.mood_delta_base + cfg.mood_delta_temper * self.temper
    }

    /// `T_exp0 = baseMin − clingyCoef × 粘人度`（`02 §5.4`；**运行时派生，配置无扁平键**）。
    ///
    /// `c=0.5` → `30 − 10×0.5 = 25`（与 `AdaptationSave::default().t_exp` 初值一致）。
    #[must_use]
    pub fn t_exp0(&self, cfg: &crate::config::model::AdaptCfg) -> f32 {
        if !self.clingy.is_finite() {
            return cfg.base_min as f32;
        }
        cfg.base_min as f32 - cfg.clingy_coef as f32 * self.clingy
    }

    /// 首次创建随机（仅粘人度入区间，其余取出厂建议值）。
    #[must_use]
    pub fn roll_initial(seed: u64, cfg: &PersonalityCfg) -> Self {
        let mut base = Self::from_cfg(cfg);
        let lo = cfg.clingy_init_min / 100.0;
        let hi = cfg.clingy_init_max / 100.0;
        let mut rng = SplitMix64::new(seed);
        base.clingy = rng.next_range_f32(lo, hi).clamp(0.0, 1.0);
        base.clamp();
        base
    }

    /// 重掷（五维在 `±rerollJitter` 内抖动后钳 `[0,1]`；`01 §6.11.6`）。
    pub fn reroll(&mut self, seed: u64, cfg: &PersonalityCfg) {
        let j = cfg.reroll_jitter.max(0.0);
        let mut rng = SplitMix64::new(seed);
        self.clingy = rng.next_range_f32(self.clingy - j, self.clingy + j);
        self.curiosity = rng.next_range_f32(self.curiosity - j, self.curiosity + j);
        self.temper = rng.next_range_f32(self.temper - j, self.temper + j);
        self.courage = rng.next_range_f32(self.courage - j, self.courage + j);
        self.diligence = rng.next_range_f32(self.diligence - j, self.diligence + j);
        self.clamp();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{AdaptCfg, PersonalityDefaultsCfg};

    fn cfg() -> PersonalityCfg {
        PersonalityCfg::default()
    }

    #[test]
    fn default_five_dims_match_config() {
        let c = cfg();
        let p = Personality::from_cfg(&c);
        let d: PersonalityDefaultsCfg = PersonalityDefaultsCfg::default();
        assert_eq!(p.clingy, d.clingy);
        assert_eq!(p.curiosity, d.curiosity);
        assert_eq!(p.temper, d.temper);
        assert_eq!(p.courage, d.courage);
        assert_eq!(p.diligence, d.diligence);
    }

    #[test]
    fn factor_matches_prd_formula() {
        let c = cfg();
        let p = Personality { clingy: 0.5, ..Personality::default() };
        assert!((p.factor(&c) - 1.0).abs() < 1e-6, "0.6 + 0.8×0.5 = 1.0");
        let p1 = Personality { clingy: 1.0, ..Personality::default() };
        assert!((p1.factor(&c) - 1.4).abs() < 1e-6);
        let p0 = Personality { clingy: 0.0, ..Personality::default() };
        assert!((p0.factor(&c) - 0.6).abs() < 1e-6);
    }

    #[test]
    fn threshold_scale_and_mood_delta_scale() {
        let c = cfg();
        let p = Personality { temper: 0.5, ..Personality::default() };
        assert!((p.threshold_scale(&c) - 1.0).abs() < 1e-6);
        assert!((p.mood_delta_scale(&c) - 1.0).abs() < 1e-6);
        let pt = Personality { temper: 1.0, ..Personality::default() };
        assert!((pt.threshold_scale(&c) - 0.8).abs() < 1e-6);
        assert!((pt.mood_delta_scale(&c) - 1.2).abs() < 1e-6);
    }

    #[test]
    fn t_exp0_is_derived_not_configured() {
        let a: AdaptCfg = AdaptCfg::default();
        let p = Personality { clingy: 0.5, ..Personality::default() };
        assert!((p.t_exp0(&a) - 25.0).abs() < 1e-6);
        let p0 = Personality { clingy: 0.0, ..Personality::default() };
        assert!((p0.t_exp0(&a) - 30.0).abs() < 1e-6);
        let p1 = Personality { clingy: 1.0, ..Personality::default() };
        assert!((p1.t_exp0(&a) - 20.0).abs() < 1e-6);
    }

    #[test]
    fn initial_roll_stays_in_factory_window() {
        let c = cfg();
        for seed in 0..200u64 {
            let p = Personality::roll_initial(seed, &c);
            assert!(
                (0.45..=0.55).contains(&p.clingy),
                "seed={seed} clingy={} 越出 45~55 出厂区间",
                p.clingy
            );
            // 其余四维不参与首建随机
            assert!((p.temper - c.defaults.temper).abs() < 1e-6);
            assert!((p.curiosity - c.defaults.curiosity).abs() < 1e-6);
        }
    }

    #[test]
    fn initial_roll_keeps_ac16_within_tolerance() {
        // AC-16 容差 ±10%：出厂 personalityFactor 必须落在 [0.96, 1.04]
        let c = cfg();
        for seed in 0..200u64 {
            let f = Personality::roll_initial(seed, &c).factor(&c);
            assert!((0.96..=1.04).contains(&f), "seed={seed} factor={f}");
        }
    }

    #[test]
    fn initial_roll_is_deterministic() {
        let c = cfg();
        assert_eq!(Personality::roll_initial(42, &c), Personality::roll_initial(42, &c));
        assert_ne!(Personality::roll_initial(42, &c), Personality::roll_initial(43, &c));
    }

    #[test]
    fn reroll_bounded_and_clamped() {
        let c = cfg();
        let mut p = Personality::from_cfg(&c);
        for seed in 0..50u64 {
            p.reroll(seed, &c);
            for v in [p.clingy, p.curiosity, p.temper, p.courage, p.diligence] {
                assert!((0.0..=1.0).contains(&v), "越界 {v}");
            }
        }
    }

    #[test]
    fn clamp_rejects_non_finite() {
        let mut p = Personality { clingy: f32::NAN, curiosity: 5.0, temper: -1.0, courage: 0.4, diligence: 0.9 };
        p.clamp();
        assert_eq!(p.clingy, 0.5);
        assert_eq!(p.curiosity, 1.0);
        assert_eq!(p.temper, 0.0);
    }
}
