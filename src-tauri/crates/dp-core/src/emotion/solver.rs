//! `emotion::solver`：七因子固定顺序快照求解（`02 §5.1` / §5.2；`01 §6.11.2`）。
//!
//! ## 七因子与**冻结顺序**
//!
//! | 序 | 因子 | 取值 | 本模块来源 |
//! |---|---|---|---|
//! | 1 | `presence` | 1.0 在场 / 0.05 离场（或不可达档） | [`presence`](crate::emotion::presence) |
//! | 2 | `busyness` | 0.3 / 0.5 / 1.0 / 1.2（+ `P_Cap` 12/28/120） | [`busyness`](crate::emotion::busyness) |
//! | 3 | `personality` | `0.6 + 0.8×粘人度` | [`personality`](crate::emotion::personality) |
//! | 4 | `rhythm` | 0.35 深夜 / 1.0 常规 / 1.15 用餐（×0.5 宽限） | [`rhythm`](crate::emotion::rhythm) |
//! | 5 | `needs` | `1 + coef × deficit` ∈ [1.0, 1.6] | 入参 `satiety` / `cleanliness` |
//! | 6 | `rough` | 1.0 ~ 1.6（2h 线性衰减） | [`rough`](crate::emotion::rough) |
//! | 7 | `adapt` | 0.7 ~ 1.3 | [`adapt`](crate::emotion::adapt) |
//!
//! **顺序冻结的理由**（`03 §2 S7-M4` 要点 3）：全部因子只读**本拍 tick 起始快照**
//! （`Inputs` 传值 / 传引用），求解过程**不修改任何被读取的状态** ⇒ 顺序只影响
//! 计算次序，不影响结果，但**固定下来才能给出确定性单测**（`02 §9.2`）。
//!
//! ## 敏感度不在七因子内
//!
//! FR-11-11 的 `sensitivityFactor` 是**独立乘子**（`effectiveRate = 乘积 × 敏感度`
//! 后再钳制），不进 [`FactorSet::product`]，故七因子表里没有它。
//! `03 §2 S7-M4` 要点 1 的行内枚举写作「…/ `adapt` / `sensitivity`」并漏列 `personality`，
//! 与 `02 §5.2`「七因子取值表」不一致 ⇒ 以 `02 §5.2` 表为准（`sensitivity` 非因子、
//! `personality` 是因子），登记见台账口径登记。
//!
//! ## 关系降温乘子
//!
//! `coolMultiplier`（×1.2）**不进** [`FactorSet::product`]：`02 §9.2` 不变量 ⑥ 要求
//! `adaptFactor ∈ [0.7, 1.3]`，且 `FactorsSnapshot.product` 必须与七个槽自洽
//! （S4-M1 既有契约，供前端反算）。故降温乘子单列 [`FactorSet::cool_mul`]，
//! 由 `engine` 在**取速率时**乘入。

use chrono::Local;

use crate::config::model::{
    AdaptCfg, BusynessCfg, EmotionNeedsCfg, PersonalityCfg, PresenceCfg, RhythmCfg, RoughCfg,
};
use crate::emotion::adapt::AdaptationState;
use crate::emotion::busyness::{BusynessLevel, BusynessSolver};
use crate::emotion::personality::Personality;
use crate::emotion::presence::{self, PresenceLatch};
use crate::emotion::rhythm::{RhythmOutput, RhythmSolver};
use crate::emotion::rough::RoughTracker;
use crate::perception::time::{TimeRhythm, TimeSegment};
use crate::perception::ActivitySample;

/// 七因子固定顺序（`02 §5.2` 取值表顺序；确定性单测与 `FactorsSnapshot` 的槽序一致）。
pub const FACTOR_ORDER: [&str; 7] =
    ["presence", "busyness", "personality", "rhythm", "needs", "rough", "adapt"];

/// 交互可达性策略（`02 §5.23` 三层防护的**配置投影**）。
///
/// 由 `dp-app` 从 `settings.json.interaction` 注入（`EmotionEngine::set_interaction_policy`），
/// 避免内核借用应用层配置对象造成的生命周期纠缠（C7：数值仍来自配置，不在内核写字面量）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InteractionPolicy {
    /// 交互是否可达（穿透 / 钩子卸载 / 勿扰时为 `false`）。
    pub available: bool,
    /// 不可达时的在场因子（`interaction.unavailablePresenceFactor`，默认 0.05）。
    pub unavailable_presence_factor: f32,
    /// 层 ② 托盘替代入口开关（`interaction.trayFallbackEnabled`）。
    pub tray_fallback_enabled: bool,
    /// 层 ③ 兜底允许的最低等级（`unreachableNaturalFloorLevel`，4 = 仅 L4→L3；5 = 关闭）。
    pub unreachable_floor_level: u8,
    /// 层 ③ 兜底要求的 `P` 低于阈值持续秒数（`unreachableHoldSec`，默认 180）。
    pub unreachable_hold_sec: u64,
    /// 层 ③ 兜底要求的无负向窗口秒数（`unreachableNoNegativeSec`，默认 7200）。
    pub unreachable_no_negative_sec: u64,
}

impl Default for InteractionPolicy {
    /// 与 `settings.json.interaction` 出厂默认同值。
    fn default() -> Self {
        Self {
            available: true,
            unavailable_presence_factor: 0.05,
            tray_fallback_enabled: true,
            unreachable_floor_level: 4,
            unreachable_hold_sec: 180,
            unreachable_no_negative_sec: 7200,
        }
    }
}

impl InteractionPolicy {
    /// 层 ③ 是否启用（等级下限 ≤4 才算开放；`5` = 完全关闭兜底）。
    #[must_use]
    pub const fn unreachable_fallback_enabled(&self) -> bool {
        self.unreachable_floor_level <= 4
    }
}

/// 七因子求解入参（本拍 tick **起始快照**；全部只读）。
pub struct FactorInputs<'a> {
    /// 用户空闲毫秒（在场判定）。
    pub idle_ms: u64,
    /// 活动感知采样（`None` = 不可用 / 感知关闭 ⇒ 忙碌档退化轻度）。
    pub activity: Option<&'a ActivitySample>,
    /// 饱食度（`needs` 因子）。
    pub satiety: f32,
    /// 清洁度（`needs` 因子）。
    pub cleanliness: f32,
    /// 五维性格（`personality` 因子 + `adapt` 的 `T_exp0`）。
    pub personality: &'a Personality,
    /// 粗暴对待计量（`rough` 因子）。
    pub rough: &'a RoughTracker,
    /// 自适应基线（`adapt` 因子 + 关系降温乘子）。
    pub adapt: &'a AdaptationState,
}

/// 七因子求解上下文（全部来自配置，纯只读）。
pub struct SolverCtx<'a> {
    /// 在场配置。
    pub presence_cfg: &'a PresenceCfg,
    /// 忙碌配置。
    pub busyness_cfg: &'a BusynessCfg,
    /// 需求→P 配置。
    pub needs_cfg: &'a EmotionNeedsCfg,
    /// 性格配置。
    pub personality_cfg: &'a PersonalityCfg,
    /// 节律配置。
    pub rhythm_cfg: &'a RhythmCfg,
    /// 节律时段表（S7-M1；6 段 + 用餐窗口）。
    pub rhythm_table: &'a TimeRhythm,
    /// 粗暴配置。
    pub rough_cfg: &'a RoughCfg,
    /// 自适应配置。
    pub adapt_cfg: &'a AdaptCfg,
    /// 交互可达性策略（FR-11-12）。
    pub interaction: InteractionPolicy,
}

/// 七因子求解结果（含中间诊断量；**不含敏感度**——敏感度在 `engine` 侧乘入）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FactorSet {
    /// 在场因子。
    pub presence: f32,
    /// 忙碌因子（平滑后）。
    pub busyness: f32,
    /// 性格因子。
    pub personality: f32,
    /// 节律因子。
    pub rhythm: f32,
    /// 需求因子。
    pub needs: f32,
    /// 粗暴因子。
    pub rough: f32,
    /// 自适应因子。
    pub adapt: f32,
    /// 七因子乘积（不含敏感度、不含降温乘子）。
    pub product: f32,
    /// 忙碌档位（决定 `P_Cap`）。
    pub busyness_level: BusynessLevel,
    /// 本拍 `P_Cap`（12 / 28 / 120）。
    pub busyness_cap: f32,
    /// 迟滞判定后的在场布尔。
    pub presence_here: bool,
    /// 交互是否可达（`false` = FR-11-12 层 ① 生效）。
    pub interaction_available: bool,
    /// 本拍是否发生「离场 → 在场」回迁（预热窗口锚点）。
    pub became_here: bool,
    /// 当前时段。
    pub segment: TimeSegment,
    /// 是否用餐窗口。
    pub meal: bool,
    /// 是否深夜段。
    pub night: bool,
    /// 是否处于开机宽限窗口。
    pub warmup: bool,
    /// 关系降温乘子（非降温期 1.0；`engine` 在取速率时乘入）。
    pub cool_mul: f32,
}

impl Default for FactorSet {
    fn default() -> Self {
        Self {
            presence: 1.0,
            busyness: 1.0,
            personality: 1.0,
            rhythm: 1.0,
            needs: 1.0,
            rough: 1.0,
            adapt: 1.0,
            product: 1.0,
            busyness_level: BusynessLevel::Light,
            busyness_cap: 120.0,
            presence_here: true,
            interaction_available: true,
            became_here: false,
            segment: TimeSegment::Morning,
            meal: false,
            night: false,
            warmup: false,
            cool_mul: 1.0,
        }
    }
}

impl FactorSet {
    /// 按 [`FACTOR_ORDER`] 重算乘积（单一算式，避免多处手写乘积漂移）。
    pub fn recompute_product(&mut self) {
        let p = self.presence
            * self.busyness
            * self.personality
            * self.rhythm
            * self.needs
            * self.rough
            * self.adapt;
        self.product = if p.is_finite() { p.max(0.0) } else { 0.0 };
    }

    /// 按 [`FACTOR_ORDER`] 取某一因子的值（快照投影 / 诊断 / 参数化测试）。
    #[must_use]
    pub fn get(&self, name: &str) -> Option<f32> {
        match name {
            "presence" => Some(self.presence),
            "busyness" => Some(self.busyness),
            "personality" => Some(self.personality),
            "rhythm" => Some(self.rhythm),
            "needs" => Some(self.needs),
            "rough" => Some(self.rough),
            "adapt" => Some(self.adapt),
            _ => None,
        }
    }
}

/// `needs` 因子：`1 + coef × max(0, (deficitRef − min(sources)) / deficitRef)`（`01` F6）。
///
/// `deficitSources` 为**配置驱动的维度名单**（默认 `["satiety","cleanliness"]`），
/// 未知维度**跳过**（配置容错，不崩）。名单为空 / 参考值非法 → 恒 1.0。
#[must_use]
pub fn needs_factor(satiety: f32, cleanliness: f32, cfg: &EmotionNeedsCfg) -> f32 {
    let ref_v = cfg.deficit_ref;
    if !ref_v.is_finite() || ref_v <= 0.0 {
        return 1.0;
    }
    let mut worst: Option<f32> = None;
    for name in &cfg.deficit_sources {
        let v = match name.as_str() {
            crate::emotion::need_keys::SATIETY => satiety,
            crate::emotion::need_keys::CLEANLINESS => cleanliness,
            _ => continue,
        };
        if !v.is_finite() {
            continue;
        }
        worst = Some(worst.map_or(v, |w: f32| w.min(v)));
    }
    let Some(min_v) = worst else {
        return 1.0;
    };
    let deficit = ((ref_v - min_v) / ref_v).max(0.0);
    let f = 1.0 + cfg.coef * deficit;
    if f.is_finite() { f.max(0.0) } else { 1.0 }
}

/// 七因子求解器（持有三个有状态的子求解器：在场迟滞 / 忙碌平滑 / 节律预热）。
#[derive(Clone, Debug)]
pub struct FactorSolver {
    presence_latch: PresenceLatch,
    busyness: BusynessSolver,
    rhythm: RhythmSolver,
}

impl FactorSolver {
    /// 由配置构造。
    #[must_use]
    pub fn new(busyness_cfg: &BusynessCfg) -> Self {
        Self {
            presence_latch: PresenceLatch::new(),
            busyness: BusynessSolver::new(busyness_cfg),
            rhythm: RhythmSolver::new(),
        }
    }

    /// 只读：在场迟滞状态（测试 / 诊断）。
    #[must_use]
    pub const fn presence_latch(&self) -> PresenceLatch {
        self.presence_latch
    }

    /// 配置热更新（忙碌平滑窗口容量随之调整）。
    pub fn sync_cfg(&mut self, cfg: &BusynessCfg) {
        self.busyness.sync_cfg(cfg);
    }

    /// 清空预热窗口（会话暂停 / 离场时由 `engine` 调用）。
    pub fn clear_warmup(&mut self) {
        self.rhythm.clear_warmup();
    }

    /// 按 [`FACTOR_ORDER`]**固定顺序**求解七因子并快照。
    ///
    /// 各步只读本拍起始快照；唯一副作用是「在场回迁 → 锚定预热窗口」（属步骤 1 的语义）。
    pub fn solve(
        &mut self,
        now_ms: i64,
        now_local: &chrono::DateTime<Local>,
        inputs: &FactorInputs<'_>,
        ctx: &SolverCtx<'_>,
    ) -> FactorSet {
        let mut out = FactorSet::default();

        // ① presence（含 FR-11-12 层 ① 不可达档）
        let p = presence::resolve(
            &mut self.presence_latch,
            inputs.idle_ms,
            ctx.presence_cfg,
            now_ms,
            ctx.interaction.available,
            ctx.interaction.unavailable_presence_factor,
        );
        out.presence = p.factor;
        out.presence_here = p.here;
        out.interaction_available = p.interaction_available;
        out.became_here = p.became_here;
        // 预热窗口锚点：刚回到电脑前 / 刚从不可交互恢复 → 开启宽限；离场 → 关闭。
        if p.became_here {
            self.rhythm.note_reentry(now_ms, ctx.rhythm_cfg);
        }
        if !p.here {
            self.rhythm.clear_warmup();
        }

        // ② busyness（隐私关闭时调用方传 `None` ⇒ 恒轻度档）
        let b = self.busyness.solve(inputs.activity, ctx.busyness_cfg, now_ms);
        out.busyness = b.factor;
        out.busyness_level = b.level;
        out.busyness_cap = b.cap;

        // ③ personality
        out.personality = inputs.personality.factor(ctx.personality_cfg);

        // ④ rhythm（含用餐覆盖与开机宽限）
        let r: RhythmOutput =
            self.rhythm.solve(now_local, ctx.rhythm_table, ctx.rhythm_cfg, now_ms);
        out.rhythm = r.factor;
        out.segment = r.segment;
        out.meal = r.meal;
        out.night = r.night;
        out.warmup = r.warmup;

        // ⑤ needs
        out.needs = needs_factor(inputs.satiety, inputs.cleanliness, ctx.needs_cfg);

        // ⑥ rough
        out.rough = inputs.rough.factor(now_ms, ctx.rough_cfg);

        // ⑦ adapt（依赖 personality 的 T_exp0）
        let t_exp0 = inputs.personality.t_exp0(ctx.adapt_cfg);
        out.adapt = inputs.adapt.factor(t_exp0, ctx.adapt_cfg);
        out.cool_mul = inputs.adapt.cool_multiplier(ctx.adapt_cfg);

        out.recompute_product();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32, minute: u32) -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 17, hour, minute, 0).single().expect("本地时刻")
    }

    struct Ctx {
        presence: PresenceCfg,
        busyness: BusynessCfg,
        needs: EmotionNeedsCfg,
        personality: PersonalityCfg,
        rhythm: RhythmCfg,
        table: TimeRhythm,
        rough: RoughCfg,
        adapt: AdaptCfg,
    }

    impl Ctx {
        fn new() -> Self {
            let rhythm = RhythmCfg::default();
            Self {
                presence: PresenceCfg::default(),
                busyness: BusynessCfg::default(),
                needs: EmotionNeedsCfg::default(),
                personality: PersonalityCfg::default(),
                table: TimeRhythm::from_cfg(&rhythm),
                rhythm,
                rough: RoughCfg::default(),
                adapt: AdaptCfg::default(),
            }
        }

        fn solver_ctx(&self, interaction: InteractionPolicy) -> SolverCtx<'_> {
            SolverCtx {
                presence_cfg: &self.presence,
                busyness_cfg: &self.busyness,
                needs_cfg: &self.needs,
                personality_cfg: &self.personality,
                rhythm_cfg: &self.rhythm,
                rhythm_table: &self.table,
                rough_cfg: &self.rough,
                adapt_cfg: &self.adapt,
                interaction,
            }
        }
    }

    fn inputs<'a>(
        personality: &'a Personality,
        rough: &'a RoughTracker,
        adapt: &'a AdaptationState,
    ) -> FactorInputs<'a> {
        FactorInputs {
            idle_ms: 0,
            activity: None,
            satiety: 70.0,
            cleanliness: 85.0,
            personality,
            rough,
            adapt,
        }
    }

    /// 典型场景（`01 §6.11.10`）：在场 / 轻度 / 粘人 50 / 不饿不脏 / 无粗暴 / adapt 基线 25
    /// → 七因子乘积**恒 1.0**，与旧裸 `idleTime` 完全等价（AC-16 的标定前提）。
    #[test]
    fn typical_scenario_product_is_one() {
        let c = Ctx::new();
        let mut s = FactorSolver::new(&c.busyness);
        let p = Personality::default();
        let r = RoughTracker::default();
        let a = AdaptationState::default();
        let out = s.solve(0, &at(14, 0), &inputs(&p, &r, &a), &c.solver_ctx(InteractionPolicy::default()));
        assert!((out.presence - 1.0).abs() < 1e-6, "presence={}", out.presence);
        assert!((out.busyness - 1.0).abs() < 1e-6);
        assert!((out.personality - 1.0).abs() < 1e-6, "0.6+0.8×0.5");
        assert!((out.rhythm - 1.0).abs() < 1e-6);
        assert!((out.needs - 1.0).abs() < 1e-6);
        assert!((out.rough - 1.0).abs() < 1e-6);
        assert!((out.adapt - 1.0).abs() < 1e-6, "T_exp0=25 / T_exp=25");
        assert!((out.product - 1.0).abs() < 1e-6, "product={}", out.product);
        assert!((out.cool_mul - 1.0).abs() < 1e-6);
    }

    #[test]
    fn factor_order_covers_all_seven_slots() {
        let c = Ctx::new();
        let mut s = FactorSolver::new(&c.busyness);
        let p = Personality::default();
        let r = RoughTracker::default();
        let a = AdaptationState::default();
        let out = s.solve(0, &at(14, 0), &inputs(&p, &r, &a), &c.solver_ctx(InteractionPolicy::default()));
        assert_eq!(FACTOR_ORDER.len(), 7);
        for name in FACTOR_ORDER {
            assert!(out.get(name).is_some(), "缺槽 {name}");
        }
        assert!(out.get("sensitivity").is_none(), "敏感度不是七因子之一");
    }

    #[test]
    fn needs_factor_follows_prd_formula() {
        let c = EmotionNeedsCfg::default();
        assert!((needs_factor(70.0, 70.0, &c) - 1.0).abs() < 1e-6);
        // AC 算例 4：Satiety=20 → 1 + 0.6×0.7143 = 1.4286
        let got = needs_factor(20.0, 90.0, &c);
        assert!((got - 1.428_57).abs() < 1e-4, "got {got}");
        assert!((needs_factor(0.0, 0.0, &c) - 1.6).abs() < 1e-6);
        assert!((needs_factor(200.0, 200.0, &c) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn needs_factor_uses_worst_source_and_skips_unknown() {
        let only_unknown =
            EmotionNeedsCfg { deficit_sources: vec!["nope".to_string()], ..EmotionNeedsCfg::default() };
        assert!((needs_factor(0.0, 0.0, &only_unknown) - 1.0).abs() < 1e-6, "未知维度跳过 → 恒 1.0");
        let only_satiety =
            EmotionNeedsCfg { deficit_sources: vec!["satiety".to_string()], ..EmotionNeedsCfg::default() };
        assert!((needs_factor(20.0, 0.0, &only_satiety) - 1.428_57).abs() < 1e-4, "只看 satiety");
    }

    #[test]
    fn needs_factor_guards_bad_ref() {
        let c = EmotionNeedsCfg { deficit_ref: 0.0, ..EmotionNeedsCfg::default() };
        assert!((needs_factor(0.0, 0.0, &c) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn product_is_recomputed_from_slots() {
        let mut f = FactorSet { presence: 0.05, busyness: 0.3, ..FactorSet::default() };
        f.recompute_product();
        let want = 0.05 * 0.3;
        assert!((f.product - want).abs() < 1e-6);
    }

    #[test]
    fn unreachable_folds_into_presence_factor() {
        let c = Ctx::new();
        let mut s = FactorSolver::new(&c.busyness);
        let p = Personality::default();
        let r = RoughTracker::default();
        let a = AdaptationState::default();
        let policy = InteractionPolicy { available: false, ..InteractionPolicy::default() };
        let out = s.solve(0, &at(14, 0), &inputs(&p, &r, &a), &c.solver_ctx(policy));
        assert!(!out.interaction_available);
        assert!((out.presence - 0.05).abs() < 1e-6, "层 ① presenceFactor=0.05 不归零");
        assert!((out.product - 0.05).abs() < 1e-6);
    }

    #[test]
    fn presence_reentry_opens_warmup_window() {
        let c = Ctx::new();
        let mut s = FactorSolver::new(&c.busyness);
        let p = Personality::default();
        let r = RoughTracker::default();
        let a = AdaptationState::default();
        // 先离场（触发迟滞）
        let mut away = inputs(&p, &r, &a);
        away.idle_ms = 600_000;
        let _ = s.solve(0, &at(14, 0), &away, &c.solver_ctx(InteractionPolicy::default()));
        let _ = s.solve(20_000, &at(14, 0), &away, &c.solver_ctx(InteractionPolicy::default()));
        assert!(!s.presence_latch().is_here());
        // 回到在场：满迟滞后触发回迁 → 预热窗口生效（rhythm ×0.5）
        let back = inputs(&p, &r, &a);
        let _ = s.solve(21_000, &at(14, 0), &back, &c.solver_ctx(InteractionPolicy::default()));
        let out = s.solve(40_000, &at(14, 0), &back, &c.solver_ctx(InteractionPolicy::default()));
        assert!(out.became_here);
        assert!(out.warmup, "回迁应开启宽限");
        assert!((out.rhythm - 0.5).abs() < 1e-6);
    }

    #[test]
    fn night_segment_and_meal_flags() {
        let c = Ctx::new();
        let mut s = FactorSolver::new(&c.busyness);
        let p = Personality::default();
        let r = RoughTracker::default();
        let a = AdaptationState::default();
        let out = s.solve(0, &at(2, 0), &inputs(&p, &r, &a), &c.solver_ctx(InteractionPolicy::default()));
        assert!(out.night);
        assert!(!out.meal);
        let mut s2 = FactorSolver::new(&c.busyness);
        let out = s2.solve(0, &at(12, 0), &inputs(&p, &r, &a), &c.solver_ctx(InteractionPolicy::default()));
        assert!(out.meal);
        assert!(!out.night);
    }

    #[test]
    fn cooling_multiplier_is_separate_from_product() {
        let c = Ctx::new();
        let mut s = FactorSolver::new(&c.busyness);
        let p = Personality::default();
        let r = RoughTracker::default();
        let mut a = AdaptationState::default();
        for d in 1..=4 {
            let _ = a.roll_day(&format!("2026-09-{d:02}"), 25.0, &c.adapt);
        }
        assert!(a.is_cooling(&c.adapt));
        let out = s.solve(0, &at(14, 0), &inputs(&p, &r, &a), &c.solver_ctx(InteractionPolicy::default()));
        assert!((out.cool_mul - c.adapt.cool_multiplier).abs() < 1e-6);
        // 降温乘子**不进** product：product 必须与七个槽的乘积严格自洽。
        let seven = out.presence * out.busyness * out.personality * out.rhythm
            * out.needs * out.rough * out.adapt;
        assert!((out.product - seven).abs() < 1e-6, "product={} seven={seven}", out.product);
        assert!(
            (c.adapt.factor_min..=c.adapt.factor_max).contains(&out.adapt),
            "adapt 因子须落在 [{}, {}]，实际 {}",
            c.adapt.factor_min,
            c.adapt.factor_max,
            out.adapt
        );
    }

    #[test]
    fn solve_is_deterministic_for_identical_inputs() {
        let c = Ctx::new();
        let p = Personality::default();
        let r = RoughTracker::default();
        let a = AdaptationState::default();
        let mut s1 = FactorSolver::new(&c.busyness);
        let mut s2 = FactorSolver::new(&c.busyness);
        for i in 0..30 {
            let t = i * 1000;
            let o1 = s1.solve(t, &at(14, 0), &inputs(&p, &r, &a), &c.solver_ctx(InteractionPolicy::default()));
            let o2 = s2.solve(t, &at(14, 0), &inputs(&p, &r, &a), &c.solver_ctx(InteractionPolicy::default()));
            assert_eq!(o1, o2);
        }
    }
}
