//! 耦合矩阵与四阶段单遍求解（S7-M3，T-18 段 · 中 / `02 §5.10` / `01 §6.12.4`）。
//!
//! ## 求解结构（`02 §5.10` 四阶段，单遍）
//!
//! | 阶段 | 本模块的落点 |
//! |---|---|
//! | **S0 快照** | [`CouplingSnapshot`]：tick 起始的 `(mood, energy, satiety, cleanliness, affinityLevel)`；所有 `when` **只**读快照 |
//! | **S1 求耦合** | [`CouplingSolver::evaluate`]：按 [`CombineStrategy`] 合并各 `rules` → [`CouplingOutput`] |
//! | **S2 属性变化** | Satiety/Cleanliness 速率推进在 [`super::NeedsSystem::tick`]；Energy 恢复 × `energy_recover_mul` 由情绪内核消费 |
//! | **S3 Mood / Affinity** | Mood 衰减 × `mood_decay_mul`、交互增益 × `stroke_gain_mul`、Affinity × `affinity_gain_mul` 由 [`crate::emotion::engine`] 消费（**Mood 不回写 S1**） |
//!
//! ## 环检测（两道保险之二）
//!
//! ① 构建期拓扑检查：`ConfigService::load_all` 复用 [`crate::config::check_coupling_cycles`]
//! （`02 §5.10`：新增成环规则在配置加载阶段即失败，CI 断言）；② 本模块
//! [`CouplingSolver::build`] **再跑一次**同一检查——使求解器在「不经配置中心构造」
//! 时（单测 / 未来热加载）也**自守**，不依赖上层是否记得校验。求解本身是
//! **单遍 + 快照**语义（`02 §5.10` 循环依赖结论）：不存在同 tick 迭代，
//! 也不会因配置成环而无限循环——成环在**构建期**即被拒绝，运行期无环可绕。
//!
//! ## 合并策略与两处显式口径
//!
//! - `combine.moodDecay = "max"`，其余 `"mul"`（`02 §5.10`）；**`travelGain`（C-16）
//!   在 `combine` 表中无对应项** → 本模块按 **`mul`** 合并（与其余乘子同口径；
//!   已在 03 台账 S7-M3 完工登记中记为显式裁定，未发明新配置键）。
//! - `op` 仅支持 `"mul"` 与 `"deny"`；未知 `op` **跳过并在构建期计数**（[`CouplingSolver::skipped_rules`]），
//!   不 panic —— 与配置中心「文件级问题降级不崩」同口径。
//!
//! ## 时间纪律（C3）
//!
//! `moodDecayMul` 的 **5s 移动平均**（`02 §5.10` `smoothSec`）以**注入的 `now_ms`**
//! 为唯一时间源：内部为定长环形缓冲（无堆分配），窗口外的样本自然淘汰。
//! 本模块零时钟。

use crate::config::model::{CombineCfg, CouplingCfg, RuleValue};
use crate::state::PetValues;

use super::DispatchKind;

/// 平滑窗口的最大样本数（防止高频调用导致缓冲被撑满；超出即覆盖最旧）。
pub const SMOOTH_SAMPLE_CAP: usize = 32;

/// 求解器构建错误（`#[non_exhaustive]`，`02 §7.4`）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CouplingError {
    /// 规则 `target → when` 引用成环（`02 §5.10` 构建期拓扑检查）。
    #[error("needs.coupling 规则引用成环：{chain}")]
    Cycle {
        /// 成环链路（如 `satiety -> moodDecay -> mood -> satiety`）。
        chain: String,
    },
}

/// 快照维度（`02 §5.10` S0 阶段的 `when` 可引用维度）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NeedDim {
    /// 心情。
    Mood,
    /// 精力。
    Energy,
    /// 饱食度。
    Satiety,
    /// 清洁度。
    Cleanliness,
    /// 亲密度等级。
    Affinity,
}

impl NeedDim {
    /// 维度名（与配置 `when` 表达式左值逐字一致，C7）。
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            NeedDim::Mood => "mood",
            NeedDim::Energy => "energy",
            NeedDim::Satiety => "satiety",
            NeedDim::Cleanliness => "cleanliness",
            NeedDim::Affinity => "affinity",
        }
    }

    /// 由维度名反查。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "mood" => Some(NeedDim::Mood),
            "energy" => Some(NeedDim::Energy),
            "satiety" => Some(NeedDim::Satiety),
            "cleanliness" => Some(NeedDim::Cleanliness),
            "affinity" => Some(NeedDim::Affinity),
            _ => None,
        }
    }
}

/// 比较运算符（`02 §5.10` `when` 表达式支持集）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CmpOp {
    /// `<`。
    Lt,
    /// `<=`。
    Le,
    /// `>`。
    Gt,
    /// `>=`。
    Ge,
    /// `==`。
    Eq,
    /// `!=`。
    Ne,
}

impl CmpOp {
    /// 求值。
    #[must_use]
    pub fn eval(self, lhs: f32, rhs: f32) -> bool {
        match self {
            CmpOp::Lt => lhs < rhs,
            CmpOp::Le => lhs <= rhs,
            CmpOp::Gt => lhs > rhs,
            CmpOp::Ge => lhs >= rhs,
            CmpOp::Eq => (lhs - rhs).abs() < f32::EPSILON,
            CmpOp::Ne => (lhs - rhs).abs() >= f32::EPSILON,
        }
    }
}

/// 单条 `when` 条件的解析结果（`{dim} {op} {value}`）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CouplingCond {
    /// 左值维度。
    pub dim: NeedDim,
    /// 比较运算符。
    pub op: CmpOp,
    /// 右值阈值。
    pub threshold: f32,
}

impl CouplingCond {
    /// 解析 `"satiety<20"` / `"satiety<=0"` / `"mood>80"` 等表达式。
    ///
    /// **双字符运算符优先**（否则 `satiety<=0` 会被误解析为 `<` + `=0`）。
    /// 未知维度 / 未知运算符 / 右值非法 → `None`（调用方按「跳过该规则」处理）。
    #[must_use]
    pub fn parse(expr: &str) -> Option<Self> {
        let (dim_str, op, rhs) = split_cond(expr)?;
        let dim = NeedDim::from_id(dim_str)?;
        let threshold: f32 = rhs.trim().parse().ok()?;
        if !threshold.is_finite() {
            return None;
        }
        Some(Self { dim, op, threshold })
    }

    /// 在快照上求值。
    #[must_use]
    pub fn eval(&self, snapshot: &CouplingSnapshot) -> bool {
        self.op.eval(snapshot.value(self.dim), self.threshold)
    }
}

/// 拆分条件表达式为 `(左值, 运算符, 右值)`（双字符运算符优先）。
fn split_cond(expr: &str) -> Option<(&str, CmpOp, &str)> {
    const TWO_CHAR: [(&str, CmpOp); 4] =
        [("<=", CmpOp::Le), (">=", CmpOp::Ge), ("==", CmpOp::Eq), ("!=", CmpOp::Ne)];
    for (token, op) in TWO_CHAR {
        if let Some(pos) = expr.find(token) {
            return Some((&expr[..pos], op, &expr[pos + token.len()..]));
        }
    }
    const ONE_CHAR: [(&str, CmpOp); 2] = [("<", CmpOp::Lt), (">", CmpOp::Gt)];
    for (token, op) in ONE_CHAR {
        if let Some(pos) = expr.find(token) {
            return Some((&expr[..pos], op, &expr[pos + token.len()..]));
        }
    }
    None
}

/// S0 快照（`02 §5.10`：所有 `when` 只基于 tick 起始快照）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CouplingSnapshot {
    /// 心情。
    pub mood: f32,
    /// 精力。
    pub energy: f32,
    /// 饱食度。
    pub satiety: f32,
    /// 清洁度。
    pub cleanliness: f32,
    /// 亲密度等级。
    pub affinity_level: u32,
}

impl CouplingSnapshot {
    /// 便捷构造。
    #[must_use]
    pub const fn new(
        mood: f32,
        energy: f32,
        satiety: f32,
        cleanliness: f32,
        affinity_level: u32,
    ) -> Self {
        Self { mood, energy, satiety, cleanliness, affinity_level }
    }

    /// 取指定维度值（`affinity` 返回等级；未知维度不会出现——枚举封闭）。
    #[must_use]
    pub fn value(&self, dim: NeedDim) -> f32 {
        match dim {
            NeedDim::Mood => self.mood,
            NeedDim::Energy => self.energy,
            NeedDim::Satiety => self.satiety,
            NeedDim::Cleanliness => self.cleanliness,
            NeedDim::Affinity => self.affinity_level as f32,
        }
    }
}

impl From<&PetValues> for CouplingSnapshot {
    /// 由六维数值取快照（S0 阶段唯一入口）。
    fn from(v: &PetValues) -> Self {
        Self {
            mood: v.mood,
            energy: v.energy,
            satiety: v.satiety,
            cleanliness: v.cleanliness,
            affinity_level: v.affinity_level,
        }
    }
}

/// 派遣拒绝依据（指向配置规则；零分配）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DispatchDeny<'a> {
    /// 触发的规则 ID（如 `C-11`）。
    pub rule_id: &'a str,
    /// 拒绝原因文案（`needs.json` 的 `reason`，C2：文案一律外置）。
    pub reason: Option<&'a str>,
}

/// 派遣门禁判定（`01 §6.12.4` / AC-30 / AC-31）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchVerdict<'a> {
    /// 允许派遣。
    Allow,
    /// 拒绝派遣（含依据）。
    Deny(DispatchDeny<'a>),
}

impl<'a> DispatchVerdict<'a> {
    /// 是否允许。
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, DispatchVerdict::Allow)
    }

    /// 拒绝原因（允许时为 `None`）。
    #[must_use]
    pub const fn reason(&self) -> Option<&'a str> {
        match self {
            DispatchVerdict::Allow => None,
            DispatchVerdict::Deny(d) => d.reason,
        }
    }
}

/// 耦合输出（S1 阶段结果；`Copy` + 零分配）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CouplingOutput<'a> {
    /// 心情衰减乘子（`combine.moodDecay = max`）。
    pub mood_decay_mul: f32,
    /// 精力恢复乘子。
    pub energy_recover_mul: f32,
    /// 抚摸增益乘子。
    pub stroke_gain_mul: f32,
    /// 亲密度增益乘子。
    pub affinity_gain_mul: f32,
    /// 打工收益乘子。
    pub job_reward_mul: f32,
    /// 学习效率乘子。
    pub study_eff_mul: f32,
    /// 移动 / 动作速度乘子。
    pub speed_mul: f32,
    /// 旅游收益乘子（`combine` 无该键 → 按 `mul`，见模块文档）。
    pub travel_gain_mul: f32,
    /// 派遣拒绝依据（按 [`DispatchKind::ALL`] 顺序：work / study / travel）。
    pub denies: [Option<DispatchDeny<'a>>; 3],
}

impl Default for CouplingOutput<'_> {
    fn default() -> Self {
        Self {
            mood_decay_mul: 1.0,
            energy_recover_mul: 1.0,
            stroke_gain_mul: 1.0,
            affinity_gain_mul: 1.0,
            job_reward_mul: 1.0,
            study_eff_mul: 1.0,
            speed_mul: 1.0,
            travel_gain_mul: 1.0,
            denies: [None; 3],
        }
    }
}

impl<'a> CouplingOutput<'a> {
    /// 该派遣类别是否被拒绝（`None` = 允许）。
    #[must_use]
    pub fn deny(&self, kind: DispatchKind) -> Option<DispatchDeny<'a>> {
        self.denies[dcg_index(kind)]
    }

    /// 是否拒绝全部派遣（`01 §6.12.2`：`satiety = 0` 时全部 `dispatch = Refuse`）。
    #[must_use]
    pub fn denies_all_dispatch(&self) -> bool {
        self.denies.iter().all(Option::is_some)
    }
}

/// 派遣类别 → `denies` 下标。
const fn dcg_index(kind: DispatchKind) -> usize {
    match kind {
        DispatchKind::Work => 0,
        DispatchKind::Study => 1,
        DispatchKind::Travel => 2,
    }
}

/// 合并策略（`combine.*` 的取值解析结果）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CombineStrategy {
    /// 取最大。
    Max,
    /// 连乘。
    Mul,
}

impl CombineStrategy {
    fn parse(text: &str) -> Self {
        if text.eq_ignore_ascii_case("max") {
            CombineStrategy::Max
        } else {
            // 未知策略按 mul（配置写坏时的最保守语义，不 panic）。
            CombineStrategy::Mul
        }
    }

    fn merge(self, acc: f32, v: f32) -> f32 {
        match self {
            CombineStrategy::Max => acc.max(v),
            CombineStrategy::Mul => acc * v,
        }
    }
}

/// 解析后的目标输出量。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    MoodDecay,
    EnergyRecover,
    StrokeGain,
    AffinityGain,
    JobReward,
    StudyEff,
    Speed,
    TravelGain,
    Dispatch,
}

impl Target {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "moodDecay" => Some(Target::MoodDecay),
            "energyRecover" => Some(Target::EnergyRecover),
            "strokeGain" => Some(Target::StrokeGain),
            "affinityGain" => Some(Target::AffinityGain),
            "jobReward" => Some(Target::JobReward),
            "studyEff" => Some(Target::StudyEff),
            "speed" => Some(Target::Speed),
            "travelGain" => Some(Target::TravelGain),
            "dispatch" => Some(Target::Dispatch),
            _ => None,
        }
    }
}

/// 解析后的数值规则（S1 阶段）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct MulRule<'a> {
    rule_id: &'a str,
    cond: CouplingCond,
    target: Target,
    value: f32,
}

/// 解析后的拒绝规则（S1 阶段 → 派遣门禁）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct DenyRule<'a> {
    rule_id: &'a str,
    cond: CouplingCond,
    kinds: [bool; 3],
    reason: Option<&'a str>,
}

/// `moodDecayMul` 的定长滑动平均（`02 §5.10` `smoothSec`，绝对时间锚定于注入 `now_ms`）。
#[derive(Clone, Debug)]
struct MoodSmooth {
    window_ms: i64,
    samples: [(i64, f32); SMOOTH_SAMPLE_CAP],
    len: usize,
    head: usize,
}

impl MoodSmooth {
    fn new(window_ms: i64) -> Self {
        Self { window_ms: window_ms.max(0), samples: [(0, 1.0); SMOOTH_SAMPLE_CAP], len: 0, head: 0 }
    }

    fn push(&mut self, now_ms: i64, value: f32) {
        self.samples[self.head] = (now_ms, value);
        self.head = (self.head + 1) % SMOOTH_SAMPLE_CAP;
        if self.len < SMOOTH_SAMPLE_CAP {
            self.len += 1;
        }
    }

    /// 窗口内样本均值；窗口内无样本（或窗口为 0）→ 返回最新值。
    fn mean(&self, now_ms: i64, latest: f32) -> f32 {
        if self.window_ms <= 0 || self.len == 0 {
            return latest;
        }
        let mut sum = 0.0f32;
        let mut count = 0u32;
        for i in 0..self.len {
            let idx = (self.head + SMOOTH_SAMPLE_CAP - 1 - i) % SMOOTH_SAMPLE_CAP;
            let (t, v) = self.samples[idx];
            if now_ms.saturating_sub(t) <= self.window_ms {
                sum += v;
                count += 1;
            }
        }
        if count == 0 {
            latest
        } else {
            sum / count as f32
        }
    }
}

/// 耦合矩阵求解器（`02 §5.10`；构建期解析 + 环检测，运行期单遍求值）。
#[derive(Clone, Debug)]
pub struct CouplingSolver<'a> {
    cfg: &'a CouplingCfg,
    mul_rules: Vec<MulRule<'a>>,
    deny_rules: Vec<DenyRule<'a>>,
    strategies: Strategies,
    smooth: MoodSmooth,
    skipped_rules: usize,
}

/// 各目标输出量的合并策略（解析自 `combine`）。
#[derive(Clone, Copy, Debug)]
struct Strategies {
    mood_decay: CombineStrategy,
    energy_recover: CombineStrategy,
    stroke_gain: CombineStrategy,
    affinity_gain: CombineStrategy,
    job_reward: CombineStrategy,
    study_eff: CombineStrategy,
    speed: CombineStrategy,
}

impl Strategies {
    fn from_cfg(c: &CombineCfg) -> Self {
        Self {
            mood_decay: CombineStrategy::parse(&c.mood_decay),
            energy_recover: CombineStrategy::parse(&c.energy_recover),
            stroke_gain: CombineStrategy::parse(&c.stroke_gain),
            affinity_gain: CombineStrategy::parse(&c.affinity_gain),
            job_reward: CombineStrategy::parse(&c.job_reward),
            study_eff: CombineStrategy::parse(&c.study_eff),
            speed: CombineStrategy::parse(&c.speed),
        }
    }

    fn for_target(&self, t: Target) -> CombineStrategy {
        match t {
            Target::MoodDecay => self.mood_decay,
            Target::EnergyRecover => self.energy_recover,
            Target::StrokeGain => self.stroke_gain,
            Target::AffinityGain => self.affinity_gain,
            Target::JobReward => self.job_reward,
            Target::StudyEff => self.study_eff,
            Target::Speed => self.speed,
            // `combine` 表无 travelGain / dispatch 项：前者按 mul（见模块文档），
            // 后者不走数值合并（deny 直接成表）。
            Target::TravelGain | Target::Dispatch => CombineStrategy::Mul,
        }
    }
}

impl<'a> CouplingSolver<'a> {
    /// 由 `needs.json.coupling` 构建（含**构建期环检测**与规则解析）。
    ///
    /// 未知 `target` / `op` / 无法解析的 `when` → 跳过该规则并计入
    /// [`Self::skipped_rules`]（不 panic、不猜语义）；成环 → `Err`。
    pub fn build(cfg: &'a CouplingCfg) -> Result<Self, CouplingError> {
        // ① 环检测：复用配置中心的单一实现（`02 §5.10`），本处再跑一次以「自守」。
        crate::config::check_coupling_cycles(cfg).map_err(|err| match err {
            crate::config::ConfigError::CouplingCycle { chain } => CouplingError::Cycle { chain },
            other => CouplingError::Cycle { chain: other.to_string() },
        })?;

        // ② 规则解析。
        let mut mul_rules: Vec<MulRule<'a>> = Vec::new();
        let mut deny_rules: Vec<DenyRule<'a>> = Vec::new();
        let mut skipped = 0usize;
        for rule in &cfg.rules {
            let Some(cond) = CouplingCond::parse(&rule.when) else {
                skipped += 1;
                continue;
            };
            let Some(target) = Target::parse(&rule.target) else {
                skipped += 1;
                continue;
            };
            match (rule.op.as_str(), target) {
                ("mul", Target::Dispatch) | ("deny", Target::Dispatch) => {
                    // `deny` 是 dispatch 的合法 op；`mul dispatch` 无语义 → 跳过。
                    if rule.op != "deny" {
                        skipped += 1;
                        continue;
                    }
                    let RuleValue::Text(list) = &rule.value else {
                        skipped += 1;
                        continue;
                    };
                    let mut kinds = [false; 3];
                    let mut any = false;
                    for token in list.split(',') {
                        if let Some(kind) = DispatchKind::from_id(token.trim()) {
                            kinds[dcg_index(kind)] = true;
                            any = true;
                        }
                    }
                    if !any {
                        skipped += 1;
                        continue;
                    }
                    deny_rules.push(DenyRule {
                        rule_id: rule.id.as_str(),
                        cond,
                        kinds,
                        reason: rule.reason.as_deref(),
                    });
                }
                ("mul", _) => {
                    let RuleValue::Number(v) = &rule.value else {
                        skipped += 1;
                        continue;
                    };
                    mul_rules.push(MulRule {
                        rule_id: rule.id.as_str(),
                        cond,
                        target,
                        value: *v,
                    });
                }
                _ => {
                    // 未知 op（含 `deny` 作用于非 dispatch 目标）。
                    skipped += 1;
                }
            }
        }

        let smooth = MoodSmooth::new(i64::try_from(cfg.smooth_sec).unwrap_or(5) * 1_000);
        Ok(Self {
            cfg,
            mul_rules,
            deny_rules,
            strategies: Strategies::from_cfg(&cfg.combine),
            smooth,
            skipped_rules: skipped,
        })
    }

    /// 已解析的数值规则条数（配置自检 / 测试观测口）。
    #[must_use]
    pub fn mul_rule_count(&self) -> usize {
        self.mul_rules.len()
    }

    /// 已解析的拒绝规则条数。
    #[must_use]
    pub fn deny_rule_count(&self) -> usize {
        self.deny_rules.len()
    }

    /// 被跳过的规则条数（未知 op / 无法解析的条件）。
    #[must_use]
    pub const fn skipped_rules(&self) -> usize {
        self.skipped_rules
    }

    /// 平滑窗口（毫秒；取自 `smoothSec`）。
    #[must_use]
    pub const fn smooth_window_ms(&self) -> i64 {
        self.smooth.window_ms
    }

    /// 底稿配置引用。
    #[must_use]
    pub const fn cfg(&self) -> &'a CouplingCfg {
        self.cfg
    }

    /// **S1 阶段**：遍历规则、按 `combine` 合并出 [`CouplingOutput`]（单遍，不迭代）。
    ///
    /// `now_ms` 仅用于 `moodDecayMul` 的 5s 滑动平均（C3：注入时间，不读时钟）。
    pub fn evaluate(&mut self, snapshot: &CouplingSnapshot, now_ms: i64) -> CouplingOutput<'a> {
        let mut mood_decay = 1.0f32;
        let mut energy_recover = 1.0f32;
        let mut stroke_gain = 1.0f32;
        let mut affinity_gain = 1.0f32;
        let mut job_reward = 1.0f32;
        let mut study_eff = 1.0f32;
        let mut speed = 1.0f32;
        let mut travel_gain = 1.0f32;
        let mut denies: [Option<DispatchDeny<'a>>; 3] = [None; 3];

        // 单遍（`02 §5.10`：不做同 tick 迭代，天然断开反馈回路）。
        for rule in &self.mul_rules {
            if !rule.cond.eval(snapshot) {
                continue;
            }
            let strategy = self.strategies.for_target(rule.target);
            let slot = match rule.target {
                Target::MoodDecay => &mut mood_decay,
                Target::EnergyRecover => &mut energy_recover,
                Target::StrokeGain => &mut stroke_gain,
                Target::AffinityGain => &mut affinity_gain,
                Target::JobReward => &mut job_reward,
                Target::StudyEff => &mut study_eff,
                Target::Speed => &mut speed,
                Target::TravelGain => &mut travel_gain,
                Target::Dispatch => continue,
            };
            *slot = strategy.merge(*slot, rule.value);
        }

        for rule in &self.deny_rules {
            if !rule.cond.eval(snapshot) {
                continue;
            }
            let deny = DispatchDeny { rule_id: rule.rule_id, reason: rule.reason };
            for (idx, hit) in rule.kinds.iter().enumerate() {
                if *hit && denies[idx].is_none() {
                    // 首条命中的拒绝规则胜出（配置顺序 = 优先级，`needs.json` 自上而下）。
                    denies[idx] = Some(deny);
                }
            }
        }

        // `moodDecayMul` 5s 移动平均（`02 §5.10`；Mood 不回写 S1）。
        self.smooth.push(now_ms, mood_decay);
        let mood_decay_mul = self.smooth.mean(now_ms, mood_decay);

        CouplingOutput {
            mood_decay_mul,
            energy_recover_mul: energy_recover,
            stroke_gain_mul: stroke_gain,
            affinity_gain_mul: affinity_gain,
            job_reward_mul: job_reward,
            study_eff_mul: study_eff,
            speed_mul: speed,
            travel_gain_mul: travel_gain,
            denies,
        }
    }

    /// 派遣门禁（`01 §6.12.4` / AC-30 / AC-31）：只回答「本例矩是否拒绝该类别」。
    ///
    /// 口径：**矩阵级**拒绝（C-05 / C-10 / C-11）由本方法给出；
    /// **分档级**拒绝（`needs.json.bands.*.deny`）由
    /// [`super::bands::BandEffects::denies_dispatch`] 给出——调用方取**并集**
    /// （两处配置语义一致，见 `01 §6.12.4` 与 §6.12.2/3 的对照）。
    #[must_use]
    pub fn dispatch_verdict<'s>(
        &'s self,
        kind: DispatchKind,
        output: &CouplingOutput<'a>,
    ) -> DispatchVerdict<'a> {
        match output.deny(kind) {
            Some(deny) => DispatchVerdict::Deny(deny),
            None => DispatchVerdict::Allow,
        }
    }

    /// 仅按条件即时判定派遣（不依赖已求出的输出；便于「菜单灰化」类前置查询）。
    #[must_use]
    pub fn check_dispatch(
        &self,
        kind: DispatchKind,
        snapshot: &CouplingSnapshot,
    ) -> DispatchVerdict<'a> {
        let idx = dcg_index(kind);
        for rule in &self.deny_rules {
            if rule.kinds[idx] && rule.cond.eval(snapshot) {
                return DispatchVerdict::Deny(DispatchDeny {
                    rule_id: rule.rule_id,
                    reason: rule.reason,
                });
            }
        }
        DispatchVerdict::Allow
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::NeedsConfig;

    /// 默认矩阵（C-01~C-16）。
    fn solver() -> CouplingSolver<'static> {
        let cfg: &'static NeedsConfig = Box::leak(Box::new(NeedsConfig::default()));
        CouplingSolver::build(&cfg.coupling).expect("默认矩阵必须无环")
    }

    /// 全满状态的快照（无任何规则命中）。
    fn healthy() -> CouplingSnapshot {
        CouplingSnapshot::new(80.0, 100.0, 90.0, 90.0, 3)
    }

    #[test]
    fn default_matrix_parses_all_sixteen_rules() {
        let s = solver();
        assert_eq!(s.skipped_rules(), 0, "默认 C-01~C-16 应全部可解析");
        assert_eq!(s.mul_rule_count(), 13, "C-01~C-04 / C-06~C-09 / C-12~C-16 共 13 条数值规则");
        assert_eq!(s.deny_rule_count(), 3, "C-05 / C-10 / C-11 共 3 条拒绝规则");
        assert_eq!(s.smooth_window_ms(), 5_000, "smoothSec = 5");
    }

    #[test]
    fn condition_parser_handles_two_char_operators_first() {
        assert_eq!(
            CouplingCond::parse("satiety<=0"),
            Some(CouplingCond { dim: NeedDim::Satiety, op: CmpOp::Le, threshold: 0.0 }),
            "<= 不得被拆成 < + =0"
        );
        assert_eq!(
            CouplingCond::parse("satiety<20"),
            Some(CouplingCond { dim: NeedDim::Satiety, op: CmpOp::Lt, threshold: 20.0 })
        );
        assert_eq!(
            CouplingCond::parse("mood>80"),
            Some(CouplingCond { dim: NeedDim::Mood, op: CmpOp::Gt, threshold: 80.0 })
        );
        assert_eq!(
            CouplingCond::parse("energy<20"),
            Some(CouplingCond { dim: NeedDim::Energy, op: CmpOp::Lt, threshold: 20.0 })
        );
        // 非法表达式一律 None（不 panic、不猜）。
        assert_eq!(CouplingCond::parse("hunger<20"), None, "未知维度");
        assert_eq!(CouplingCond::parse("satiety"), None, "无运算符");
        assert_eq!(CouplingCond::parse("satiety<abc"), None, "右值非法");
        assert_eq!(CouplingCond::parse("satiety<NaN"), None, "NaN 非法");
    }

    #[test]
    fn healthy_snapshot_yields_neutral_output() {
        let mut s = solver();
        let out = s.evaluate(&healthy(), 0);
        assert_eq!(out, CouplingOutput::default(), "无规则命中 → 全 1.0、无拒绝");
        assert!(!out.denies_all_dispatch(), "常态不应拒绝任何派遣");
        assert!(s.dispatch_verdict(DispatchKind::Work, &out).is_allowed());
    }

    #[test]
    fn mood_decay_uses_max_combination() {
        let mut s = solver();
        // satiety=10 命中 C-01(1.8)；cleanliness=10 命中 C-06(1.3) → max = 1.8。
        let snap = CouplingSnapshot::new(80.0, 100.0, 10.0, 10.0, 3);
        let out = s.evaluate(&snap, 0);
        assert!((out.mood_decay_mul - 1.8).abs() < 1e-6, "max 合并：{}", out.mood_decay_mul);

        // satiety = 0 再命中 C-02(2.5) → 仍取 max 2.5。
        let mut s2 = solver();
        let out2 = s2.evaluate(&CouplingSnapshot::new(80.0, 100.0, 0.0, 0.0, 3), 0);
        assert!((out2.mood_decay_mul - 2.5).abs() < 1e-6, "{}", out2.mood_decay_mul);
    }

    #[test]
    fn mul_combination_accumulates_across_rules() {
        let mut s = solver();
        // jobReward：C-04(0.7, satiety<20) × C-09(0.6, cleanliness<15) × C-14(0.8, mood<30)
        // = 0.336（三条件同时命中）。
        let snap = CouplingSnapshot::new(20.0, 100.0, 10.0, 10.0, 3);
        let out = s.evaluate(&snap, 0);
        assert!((out.job_reward_mul - 0.336).abs() < 1e-6, "{}", out.job_reward_mul);
        // strokeGain / affinityGain：仅 C-07 / C-08（0.5）。
        assert!((out.stroke_gain_mul - 0.5).abs() < 1e-6);
        assert!((out.affinity_gain_mul - 0.5).abs() < 1e-6);
        // speed：仅 C-12（0.8）——energy=100 不命中。
        assert!((out.speed_mul - 1.0).abs() < 1e-6);
    }

    /// **AC-30**：`Satiety=15` 时打工**可执行**且收益 ×0.7（解除死亡负循环）。
    #[test]
    fn ac30_satiety_15_still_allows_work_with_reduced_reward() {
        let mut s = solver();
        let snap = CouplingSnapshot::new(60.0, 80.0, 15.0, 80.0, 3);
        let out = s.evaluate(&snap, 0);
        assert!(
            s.dispatch_verdict(DispatchKind::Work, &out).is_allowed(),
            "satiety∈[5,20) 必须仍可打工（AC-30）"
        );
        assert!((out.job_reward_mul - 0.7).abs() < 1e-6, "收益 ×0.7：{}", out.job_reward_mul);
        // 负循环解除证明：收益仍为正。
        assert!(out.job_reward_mul > 0.0);
    }

    /// **AC-31**：`Energy < 20` 拒绝一切派遣；`Satiety < 5` 拒绝打工 / 学习 / 旅游。
    #[test]
    fn ac31_energy_below_20_denies_all_dispatch() {
        let mut s = solver();
        // Energy = 15（<20），其余正常：C-11 拒绝 work/study/travel。
        let snap = CouplingSnapshot::new(80.0, 15.0, 80.0, 80.0, 3);
        let out = s.evaluate(&snap, 0);
        for kind in DispatchKind::ALL {
            let v = s.dispatch_verdict(kind, &out);
            assert!(!v.is_allowed(), "Energy<20 必须拒绝 {:?}（AC-31）", kind);
        }
        assert!(out.denies_all_dispatch());
        assert_eq!(out.deny(DispatchKind::Travel).map(|d| d.rule_id), Some("C-11"));
        assert!(out.deny(DispatchKind::Work).and_then(|d| d.reason).is_some(), "必须带拒绝原因文案");
        // 边界：Energy = 20 → 不拒绝（阈值严格小于）。
        let mut s2 = solver();
        let out2 = s2.evaluate(&CouplingSnapshot::new(80.0, 20.0, 80.0, 80.0, 3), 0);
        assert!(s2.dispatch_verdict(DispatchKind::Work, &out2).is_allowed());
    }

    #[test]
    fn ac31_satiety_below_5_denies_work_only_via_matrix() {
        let mut s = solver();
        let snap = CouplingSnapshot::new(80.0, 80.0, 3.0, 80.0, 3);
        let out = s.evaluate(&snap, 0);
        assert_eq!(out.deny(DispatchKind::Work).map(|d| d.rule_id), Some("C-05"));
        assert!(out.deny(DispatchKind::Study).is_none(), "C-05 只拒绝打工（学习/旅游由 C-11 或分档负责）");
        assert!(out.deny(DispatchKind::Travel).is_none());
        // 边界：satiety = 5 → 不拒绝。
        let mut s2 = solver();
        let out2 = s2.evaluate(&CouplingSnapshot::new(80.0, 80.0, 5.0, 80.0, 3), 0);
        assert!(out2.deny(DispatchKind::Work).is_none());
    }

    #[test]
    fn cleanliness_below_15_denies_study() {
        let mut s = solver();
        let out = s.evaluate(&CouplingSnapshot::new(80.0, 80.0, 80.0, 10.0, 3), 0);
        assert_eq!(out.deny(DispatchKind::Study).map(|d| d.rule_id), Some("C-10"));
        assert!(out.deny(DispatchKind::Work).is_none());
    }

    #[test]
    fn mood_above_80_boosts_rewards() {
        let mut s = solver();
        // mood=90 → C-15(1.15) × C-16 travelGain(1.2)。
        let out = s.evaluate(&CouplingSnapshot::new(90.0, 80.0, 80.0, 80.0, 3), 0);
        assert!((out.job_reward_mul - 1.15).abs() < 1e-6, "{}", out.job_reward_mul);
        assert!((out.travel_gain_mul - 1.2).abs() < 1e-6, "travelGain 按 mul：{}", out.travel_gain_mul);
    }

    #[test]
    fn energy_recover_slowdown_applies_below_satiety_40() {
        let mut s = solver();
        let out = s.evaluate(&CouplingSnapshot::new(80.0, 80.0, 30.0, 80.0, 3), 0);
        assert!((out.energy_recover_mul - 0.5).abs() < 1e-6, "{}", out.energy_recover_mul);
        let out_ok = s.evaluate(&CouplingSnapshot::new(80.0, 80.0, 40.0, 80.0, 3), 0);
        assert!((out_ok.energy_recover_mul - 1.0).abs() < 1e-6, "边界严格小于");
    }

    #[test]
    fn snapshot_is_the_only_input_no_same_tick_feedback() {
        // 同一快照重复求值 → 结果稳定（除 5s 平滑外无状态漂移）。
        let mut s = solver();
        let snap = CouplingSnapshot::new(80.0, 80.0, 10.0, 10.0, 3);
        let a = s.evaluate(&snap, 0);
        let b = s.evaluate(&snap, 0);
        assert_eq!(a, b, "同快照、同时刻 → 同输出（确定性）");
        // 快照不因求解而改变（Mood / Affinity 不回写 S1）。
        assert_eq!(snap.mood, 80.0);
        assert_eq!(snap.affinity_level, 3);
    }

    #[test]
    fn mood_decay_smoothing_averages_within_window() {
        let mut s = solver();
        let hungry = CouplingSnapshot::new(80.0, 80.0, 10.0, 80.0, 3);
        let full = CouplingSnapshot::new(80.0, 80.0, 90.0, 80.0, 3);
        // 第 1 拍：1.8（窗口内仅此样本）。
        let a = s.evaluate(&hungry, 0);
        assert!((a.mood_decay_mul - 1.8).abs() < 1e-6);
        // 第 2 拍（1s 后，仍走饥饿）：窗口内两样本均值仍 1.8。
        let b = s.evaluate(&hungry, 1_000);
        assert!((b.mood_decay_mul - 1.8).abs() < 1e-6, "同值样本平滑后不变：{}", b.mood_decay_mul);
        // 第 3 拍（2s 后，恢复满饱）：窗口内 {1.8, 1.8, 1.0} → 均值 1.5333…
        let cand = s.evaluate(&full, 2_000);
        assert!(cand.mood_decay_mul < 1.8 && cand.mood_decay_mul > 1.0, "平滑应介于两端：{}", cand.mood_decay_mul);
        // 窗口滑过（6s 后）→ 仅剩最新样本 1.0。
        let settled = s.evaluate(&full, 7_000);
        assert!((settled.mood_decay_mul - 1.0).abs() < 1e-6, "窗口外样本淘汰：{}", settled.mood_decay_mul);
    }

    #[test]
    fn zero_smooth_window_returns_instant_value() {
        let mut cfg = NeedsConfig::default();
        cfg.coupling.smooth_sec = 0;
        let cfg: &'static NeedsConfig = Box::leak(Box::new(cfg));
        let mut s = CouplingSolver::build(&cfg.coupling).expect("无环");
        let a = s.evaluate(&CouplingSnapshot::new(80.0, 80.0, 10.0, 80.0, 3), 0);
        assert!((a.mood_decay_mul - 1.8).abs() < 1e-6, "窗口 0 → 即时值");
    }

    /// **环检测**：`A→B→A` 一圈在构建期即被拒绝（不进入运行期，故不可能无限循环）。
    #[test]
    fn cycle_detection_rejects_cyclic_matrix_at_build_time() {
        let mut cfg = NeedsConfig::default();
        cfg.coupling.rules.push(crate::config::model::CouplingRuleCfg {
            id: "X-01".to_string(),
            when: "mood<30".to_string(),
            target: "satiety".to_string(),
            op: "mul".to_string(),
            value: RuleValue::Number(0.9),
            reason: None,
        });
        let err = CouplingSolver::build(&cfg.coupling).expect_err("成环必须构建期失败");
        match err {
            CouplingError::Cycle { chain } => {
                assert!(chain.contains("satiety"), "环链应含 satiety：{chain}");
            }
        }
    }

    #[test]
    fn single_pass_terminates_on_wide_matrix() {
        // 单遍语义：即使规则数量放大，evaluate 也只遍历一次（不迭代 → 无环可绕）。
        let mut cfg = NeedsConfig::default();
        let base = cfg.coupling.rules.clone();
        for (i, r) in base.iter().enumerate() {
            let mut extra = r.clone();
            extra.id = format!("DUP-{i}");
            cfg.coupling.rules.push(extra);
        }
        let cfg: &'static NeedsConfig = Box::leak(Box::new(cfg));
        let mut s = CouplingSolver::build(&cfg.coupling).expect("复制规则不成环");
        let out = s.evaluate(&CouplingSnapshot::new(80.0, 80.0, 10.0, 10.0, 3), 0);
        // 规则重复 → 乘子重复施加（mul 幂等性不作为契约；此处只断言「能终止且数值合理」）。
        assert!(out.mood_decay_mul.is_finite());
        assert!(out.job_reward_mul > 0.0);
    }

    #[test]
    fn unknown_op_and_target_are_skipped_not_panicked() {
        let mut cfg = NeedsConfig::default();
        cfg.coupling.rules.push(crate::config::model::CouplingRuleCfg {
            id: "Y-01".to_string(),
            when: "satiety<20".to_string(),
            target: "moodDecay".to_string(),
            op: "add".to_string(), // 未知 op
            value: RuleValue::Number(5.0),
            reason: None,
        });
        cfg.coupling.rules.push(crate::config::model::CouplingRuleCfg {
            id: "Y-02".to_string(),
            when: "satiety<20".to_string(),
            target: "unknownTarget".to_string(),
            op: "mul".to_string(),
            value: RuleValue::Number(5.0),
            reason: None,
        });
        cfg.coupling.rules.push(crate::config::model::CouplingRuleCfg {
            id: "Y-03".to_string(),
            when: "not-a-cond".to_string(),
            target: "moodDecay".to_string(),
            op: "mul".to_string(),
            value: RuleValue::Number(5.0),
            reason: None,
        });
        let cfg: &'static NeedsConfig = Box::leak(Box::new(cfg));
        let s = CouplingSolver::build(&cfg.coupling).expect("未知项不得致构建失败");
        assert_eq!(s.skipped_rules(), 3, "三条非法规则全部跳过");
        assert_eq!(s.mul_rule_count(), 13);
    }

    #[test]
    fn deny_rules_use_first_match_in_config_order() {
        let mut cfg = NeedsConfig::default();
        // 追加一条更宽的拒绝（satiety<50 拒绝 work），应排在 C-05 之后 → 不抢占。
        cfg.coupling.rules.push(crate::config::model::CouplingRuleCfg {
            id: "Z-01".to_string(),
            when: "satiety<50".to_string(),
            target: "dispatch".to_string(),
            op: "deny".to_string(),
            value: RuleValue::Text("work".to_string()),
            reason: Some("测试用".to_string()),
        });
        let cfg: &'static NeedsConfig = Box::leak(Box::new(cfg));
        let mut s = CouplingSolver::build(&cfg.coupling).expect("无环");
        // satiety=3：C-05（satiety<5）先命中 → 依据为 C-05。
        let out = s.evaluate(&CouplingSnapshot::new(80.0, 80.0, 3.0, 80.0, 3), 0);
        assert_eq!(out.deny(DispatchKind::Work).map(|d| d.rule_id), Some("C-05"));
        // satiety=30：仅 Z-01 命中 → 依据为 Z-01。
        let out2 = s.evaluate(&CouplingSnapshot::new(80.0, 80.0, 30.0, 80.0, 3), 0);
        assert_eq!(out2.deny(DispatchKind::Work).map(|d| d.rule_id), Some("Z-01"));
    }

    #[test]
    fn check_dispatch_matches_evaluate_result() {
        let mut s = solver();
        let snap = CouplingSnapshot::new(80.0, 15.0, 80.0, 80.0, 3);
        let out = s.evaluate(&snap, 0);
        for kind in DispatchKind::ALL {
            assert_eq!(
                s.check_dispatch(kind, &snap).is_allowed(),
                s.dispatch_verdict(kind, &out).is_allowed(),
                "即时判定与求解结果必须一致（{:?}）",
                kind
            );
        }
    }
}
