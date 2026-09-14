//! `emotion::coax`：道歉三部曲（`CoaxFlow`）与 L5 离家找回（**S4-M3 / S4-M4**；
//! `02 §5 K-10` §5.1/§5.23、`01 §6.5.2` / §6.11.8、`03 §2 T-12`）。
//!
//! ## 职责
//!
//! 纯逻辑状态机：**L3~L5 阶段的唯一「降档」通道**（L4/L5 不开放自然回退）。
//! 本模块**零时钟**（C3，`now_ms` 全由调用方注入）、**零 Tauri**、**零配置读取**
//! （`CoaxCfg` 由调用方按引用传入，C7）。
//!
//! ```text
//!   呼唤(Call) ──抚摸 ≥ strokeSec──▶ 抚摸(Stroke) ──进度满──▶ 比心(Heart)
//!        ▲                              │                        │
//!        └──── 中断回退 ×rollbackRatio ──┘        双击比心 ≤heartWindowSec → 成功
//! ```
//!
//! ## 口径（与 PRD / 架构逐条对应）
//!
//! 1. **三部曲**：点击呼唤 → 连续抚摸 `strokeSec=5`（每 2s `Mood +3`）→ 进度环满后
//!    `heartWindowSec=10` 内双击比心 → 成功（`01 §6.5.2`）。
//! 2. **中断回退**：进度环回退 `interruptRollbackRatio=0.5`（**保留部分进度，不重置**）；
//!    打断（负向事件 / 中途快速连点）额外 `P + rough.interruptPenalty`（`01 §6.5.2` B-6，
//!    惩罚量由内核按 `rough.interruptPenalty` 叠加，本模块只标 `Failed(Interrupted)`）。
//! 3. **成功**：`Mood = max(Mood, recoverMoodFloor=50)`；档位目标由 [`coax_target_level`]
//!    给出（生气 L4→L3、离家出走 L5→L3、生闷气 L3→L2，`01 §6.5.3` 状态机）；
//!    `ACT-E-06` 与 `P relief` 由内核/上层承接。
//! 4. **L5 离家**：`ACT-E-05` 演出（`RUNAWAY_PERFORMANCE_MS`）后「离家」（`away=true`，
//!    窗口隐藏）；托盘「把心月狐找回来」→ [`CoaxInput::Recall`] 走回（`away=false`），
//!    仍需完成三部曲（`01 §6.5.2`）。
//! 5. **`force_lower`**：托盘 / 设置「重置情绪」兜底，强制解除 L5（`02 §5.23` R18）。
//! 6. **托盘替代入口**（`02 §5.23` 第 2 层）：[`CoaxInput::TrayStroke`] 每次折算
//!    一格抚摸（累计 `TRAY_COAX_STROKE_TAPS` 次 = 抚摸阶段），走**完整**三部曲。
//!
//! ## 禁止顺手改动
//!
//! 不产台词 / 气泡（归 S4-M5）；不做七因子 `roughFactor` 累积（归 S7-M4）；
//! 不读系统时钟（C3）；不落盘（归 S5）。

use crate::config::model::CoaxCfg;

// ---------------------------------------------------------------------------
// 常量（口径来源：`01 §6.3.2` / `01 §6.5.2` / `02 §5.23` / `03 §2 T-12`）
// ---------------------------------------------------------------------------

/// `ACT-E-05` 离家出走演出时长（毫秒；`01 §6.3.2`「单次(约6s)」、`03 §2 S4-M4`「约 6s」）。
pub const RUNAWAY_PERFORMANCE_MS: i64 = 6_000;

/// 托盘「摸摸」次数 = 抚摸阶段格数（`02 §5.23`：累计 5 次 = 抚摸阶段）。
pub const TRAY_COAX_STROKE_TAPS: i64 = 5;

/// 单次抚摸推进的最大计步（毫秒）：防长时间停顿（锁屏 / 线程挂起）被一次性计入。
pub const STROKE_MAX_STEP_MS: i64 = 200;

/// 每 2s `Mood +3` 的节拍周期（`01 §6.5.2` / `emotion.json.coax.moodGainPerTwoSec`）。
const MOOD_GAIN_PERIOD_MS: i64 = 2_000;

/// 进度环广播粒度（1%）：低于该粒度的变化不发事件，压住 20Hz 档的广播量。
const PROGRESS_EMIT_STEP: f32 = 0.01;

/// 进入 CoaxFlow 域的最低档位（L3 生闷气；`01 §6.5.2`）。
pub const COAX_MIN_LEVEL: u8 = 3;

/// 必须走 CoaxFlow、不开放自然回退的最低档位（L4 生气；`01 §6.5.2` / `02 §5.3`）。
pub const COAX_REQUIRED_MIN_LEVEL: u8 = 4;

/// 三部曲成功下发的动作（`actions.json` ACT-E-06「哄好破涕为笑」，优先级 9）。
pub const COAX_SUCCESS_ACTION: &str = "ACT-E-06";
/// 三部曲成功动作的优先级下限（`01 §6.3.1` / `actions.json` priority）。
pub const COAX_SUCCESS_PRIORITY: u8 = 9;

// ---------------------------------------------------------------------------
// 枚举
// ---------------------------------------------------------------------------

/// 三部曲子状态（`02 §4.3` `CoaxStep` 扩展；`Runaway`/`Away` 为 L5 离家段）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CoaxStep {
    /// 未进行（进度环隐藏）。
    #[default]
    Idle,
    /// 已呼唤，等待抚摸。
    Call,
    /// 抚摸中（进度环推进）。
    Stroke,
    /// 进度环已满，等待双击比心。
    Heart,
    /// L5 离家演出中（`ACT-E-05`）。
    Runaway,
    /// L5 已离家（窗口隐藏）。
    Away,
}

impl CoaxStep {
    /// 进度环是否可见（`Idle` / `Runaway` / `Away` 隐藏）。
    #[must_use]
    pub fn ring_visible(self) -> bool {
        matches!(self, Self::Call | Self::Stroke | Self::Heart)
    }
}

/// 三部曲失败原因（`02 §4.3` `CoaxFailReason`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CoaxFailReason {
    /// 被打断（负向事件 / 中途快速连点；额外 `P + interruptPenalty`）。
    Interrupted,
    /// 比心窗超时（进度环满后未在 `heartWindowSec` 内完成）。
    Timeout,
    /// 中途放弃（松手 / 停止抚摸；仅回退，不加罚）。
    Abandoned,
}

/// 三部曲输入（`03 §2 S4-M3` 三部曲 + `02 §5.23` 托盘替代入口）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CoaxInput {
    /// 点击呼唤（桌面单击 / 托盘「摸摸」首击）。
    Call,
    /// 比心回应（桌面双击 / 托盘「比心」项）。
    Heart,
    /// 负向事件（甩出 / 戳痒 → 打断三部曲）。
    Negative,
    /// L5 找回（托盘「把心月狐找回来」）。
    Recall,
    /// 托盘「摸摸」累计一格（`02 §5.23` 第 2 层）。
    TrayStroke,
}

/// 状态机对外产出的效果（由内核翻译为算法事件并落地状态变更）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CoaxEffect {
    /// 进度环 / 子状态变化（`pet://coax` 载荷来源）。
    Progress {
        /// 进度环比例 0..=1。
        ratio: f32,
        /// 子状态。
        step: CoaxStep,
    },
    /// `Mood` 增益（抚摸每 2s）。
    MoodGain(f32),
    /// 三部曲完成（内核落地：`P relief` + 档位降至 [`coax_target_level`] + `Mood` 兜底 + `ACT-E-06`）。
    Succeeded,
    /// 三部曲失败（打断裂附加 `P` 惩罚由内核按 `rough.interruptPenalty` 落地）。
    Failed(CoaxFailReason),
}

// ---------------------------------------------------------------------------
// 档位目标（`01 §6.5.3` 状态机）
// ---------------------------------------------------------------------------

/// 三部曲成功后的目标档位（`01 §6.5.3`：生闷气→委屈、生气→生闷气、离家出走→生闷气）。
///
/// 注：`02 §5.3` 的「CoaxFlow 可降 2 级」指**最大**跨度（L5→L3 恰为 2 级），
/// 与状态机逐条迁移一致；L4→L3（1 级）、L3→L2（1 级）不冲突。
#[must_use]
pub const fn coax_target_level(level: u8) -> u8 {
    match level {
        0..=2 => level,
        3 => 2,
        4 => 3,
        _ => 3,
    }
}

/// 抚摸阶段目标时长（毫秒；轻松模式取 `easyModeStrokeSec`，设置接线归 S5）。
#[must_use]
pub fn stroke_target_ms(cfg: &CoaxCfg, easy_mode: bool) -> i64 {
    let secs = if easy_mode {
        cfg.easy_mode_stroke_sec
    } else {
        cfg.stroke_sec
    };
    (secs as i64).max(1) * 1_000
}

// ---------------------------------------------------------------------------
// CoaxFlow
// ---------------------------------------------------------------------------

/// 道歉三部曲状态机（`02 §5.1` `emotion/coax.rs`；`EmotionEngine *-- CoaxFlow`）。
#[derive(Clone, Debug, Default)]
pub struct CoaxFlow {
    /// 已进入 L3~L5 域（否则全部输入忽略）。
    engaged: bool,
    /// 子状态。
    step: CoaxStep,
    /// 当前档位（由 [`Self::sync_level`] 维护；供 `force_lower` / 目标档位推导）。
    level: u8,
    /// 进度环比例 0..=1。
    progress: f32,
    /// 已累计抚摸毫秒。
    stroke_ms: i64,
    /// 上一次抚摸推进时刻（`None` = 当前未在抚摸）。
    last_stroke_ms: Option<i64>,
    /// 每 2s 增益节拍锚点。
    gain_anchor_ms: Option<i64>,
    /// 比心窗截止时刻（进度环满后 `heartWindowSec`）。
    heart_deadline_ms: Option<i64>,
    /// 进度环保持到期时刻（失败后 `ringHoldSec` 内仍显示回退后的环）。
    hide_at_ms: Option<i64>,
    /// 离家演出起始时刻。
    ran_ms: Option<i64>,
    /// 是否已找回（防 `sync_level` 在 L5 上重复触发离家演出）。
    recalled: bool,
    /// 已离家（窗口应隐藏）。
    away: bool,
    /// 轻松模式（设置项；接线归 S5）。
    easy_mode: bool,
    /// 最近一次广播的进度百分比（-1 = 强制重发）。
    emitted_pct: i32,
    /// 最近一次广播的子状态。
    emitted_step: Option<CoaxStep>,
}

impl CoaxFlow {
    /// 构造（空状态）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            emitted_pct: -1,
            ..Self::default()
        }
    }

    /// 子状态。
    #[inline]
    #[must_use]
    pub fn step(&self) -> CoaxStep {
        self.step
    }

    /// 进度环比例 0..=1。
    #[inline]
    #[must_use]
    pub fn progress(&self) -> f32 {
        self.progress
    }

    /// 进度环是否可见。
    #[inline]
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.step.ring_visible()
    }

    /// 是否已离家（窗口应隐藏）。
    #[inline]
    #[must_use]
    pub fn is_away(&self) -> bool {
        self.away
    }

    /// 是否处于 L3~L5 域。
    #[inline]
    #[must_use]
    pub fn is_engaged(&self) -> bool {
        self.engaged
    }

    /// 设置轻松模式（`01 §6.5.2` 设置页「轻松模式」；接线归 S5）。
    pub fn set_easy_mode(&mut self, on: bool) {
        self.easy_mode = on;
    }

    /// 清空全部状态，返回需要广播的效果（隐藏进度环 / 恢复可见）。
    pub fn clear(&mut self) -> Vec<CoaxEffect> {
        let was_visible = self.is_active() || self.away || self.step == CoaxStep::Runaway;
        let level = self.level;
        *self = Self { level, emitted_pct: -1, ..Self::default() };
        if was_visible {
            vec![CoaxEffect::Progress { ratio: 0.0, step: CoaxStep::Idle }]
        } else {
            Vec::new()
        }
    }

    /// `force_lower` 兜底（`02 §5.23` R18：托盘 / 设置「重置情绪」强制解除 L5）。
    pub fn force_lower(&mut self) -> Vec<CoaxEffect> {
        self.clear()
    }

    /// 阶段同步：由内核在每次 tick 结算后调用（`level` 为当前生效档位）。
    ///
    /// - `level < 3` → 清空（回到平静/无聊/委屈不保留哄好进度）；
    /// - `level == 5` 且未离家 / 未找回 → 进入离家演出（`ACT-E-05` 由内核下发）。
    pub fn sync_level(&mut self, level: u8, now_ms: i64) -> Vec<CoaxEffect> {
        self.level = level;
        if level < COAX_MIN_LEVEL {
            return self.clear();
        }
        self.engaged = true;
        if level < 5 {
            self.recalled = false;
            self.ran_ms = None;
            if matches!(self.step, CoaxStep::Runaway | CoaxStep::Away) {
                // 理论不可达（L5 不开放自然回退），防御性解除。
                self.away = false;
                self.step = CoaxStep::Idle;
                return vec![CoaxEffect::Progress { ratio: 0.0, step: CoaxStep::Idle }];
            }
        }
        if level >= 5 && !self.away && !self.recalled && self.ran_ms.is_none() {
            self.step = CoaxStep::Runaway;
            self.ran_ms = Some(now_ms);
            return self.emit(0.0, CoaxStep::Runaway);
        }
        Vec::new()
    }

    /// 20Hz 推进（`03 §2 S4-M3/S4-M4`：连续抚摸累计 + 比心窗超时 + 离家演出计时）。
    ///
    /// `stroke_active` 为「光标当前是否处于抚摸态」（钩子域透传；见 `dp-app`
    /// `InteractionConsumer::is_stroking`）。非抚摸态且已开始抚摸 → 判定为**中途放弃**。
    pub fn tick(&mut self, now_ms: i64, stroke_active: bool, cfg: &CoaxCfg) -> Vec<CoaxEffect> {
        let mut out = Vec::new();

        // 1) 离家演出推进（`ACT-E-05` 播完 → 离家）。
        if self.step == CoaxStep::Runaway {
            if let Some(t0) = self.ran_ms {
                if now_ms - t0 >= RUNAWAY_PERFORMANCE_MS {
                    self.away = true;
                    self.step = CoaxStep::Away;
                    out.extend(self.emit(0.0, CoaxStep::Away));
                }
            }
        }

        // 2) 进度环保持到期 → 隐藏。
        if let Some(until) = self.hide_at_ms {
            if now_ms >= until {
                self.hide_at_ms = None;
                self.step = CoaxStep::Idle;
                self.progress = 0.0;
                self.stroke_ms = 0;
                out.extend(self.emit(0.0, CoaxStep::Idle));
            }
        }

        if self.away || !self.engaged {
            return out;
        }

        // 3) 抚摸进入（Idle/Call + 抚摸态 → Stroke；免去「必须先点击」的硬门槛）。
        //
        // ⚠️ **不重置 `stroke_ms` / `progress`**：中断回退后回到 `Call` 态，用户再次
        // 抚摸必须**从保留的进度继续累加**（`01 §6.5.2`「回退 50%，不重置整个序列」）。
        // `Idle` 态的不变量是 `stroke_ms == 0`（`clear` / 成功 / 保持到期均归零）。
        if stroke_active && matches!(self.step, CoaxStep::Idle | CoaxStep::Call) {
            self.step = CoaxStep::Stroke;
            self.hide_at_ms = None;
            self.last_stroke_ms = Some(now_ms);
            self.gain_anchor_ms = Some(now_ms);
            out.extend(self.emit(self.progress, CoaxStep::Stroke));
        }

        // 4) 抚摸累计（每秒推进；每 2s Mood +3）。
        if self.step == CoaxStep::Stroke {
            let total = stroke_target_ms(cfg, self.easy_mode);
            if stroke_active {
                let dt = self
                    .last_stroke_ms
                    .map_or(0, |t| (now_ms - t).clamp(0, STROKE_MAX_STEP_MS));
                self.last_stroke_ms = Some(now_ms);
                if dt > 0 {
                    self.stroke_ms += dt;
                    let anchor = self.gain_anchor_ms.get_or_insert(now_ms);
                    if now_ms - *anchor >= MOOD_GAIN_PERIOD_MS {
                        *anchor = now_ms;
                        out.push(CoaxEffect::MoodGain(cfg.mood_gain_per_two_sec));
                    }
                }
                self.progress = if total > 0 {
                    (self.stroke_ms as f32 / total as f32).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                if self.progress >= 1.0 {
                    self.step = CoaxStep::Heart;
                    self.heart_deadline_ms =
                        Some(now_ms + (cfg.heart_window_sec as i64).max(0) * 1_000);
                    out.extend(self.emit(1.0, CoaxStep::Heart));
                } else {
                    out.extend(self.emit(self.progress, CoaxStep::Stroke));
                }
            } else if self.last_stroke_ms.is_some() {
                // 停止抚摸 → 中途放弃：回退但保留部分进度（不重置）。
                out.extend(self.rollback(now_ms, cfg, CoaxFailReason::Abandoned));
            }
        }

        // 5) 比心窗超时。
        if self.step == CoaxStep::Heart {
            if let Some(deadline) = self.heart_deadline_ms {
                if now_ms > deadline {
                    out.extend(self.rollback(now_ms, cfg, CoaxFailReason::Timeout));
                }
            }
        }

        out
    }

    /// 离散输入（`03 §2 S4-M3` 三部曲；托盘输入见 `02 §5.23`）。
    pub fn on_input(&mut self, input: CoaxInput, now_ms: i64, cfg: &CoaxCfg) -> Vec<CoaxEffect> {
        match input {
            CoaxInput::Recall => self.recall(),
            CoaxInput::Call => self.on_call(now_ms, cfg),
            CoaxInput::Heart => self.on_heart(now_ms, cfg),
            CoaxInput::Negative => {
                if self.is_active() {
                    self.rollback(now_ms, cfg, CoaxFailReason::Interrupted)
                } else {
                    Vec::new()
                }
            }
            CoaxInput::TrayStroke => self.on_tray_stroke(now_ms, cfg),
        }
    }

    // -- 内部：各输入处理 --------------------------------------------------

    /// 呼唤：`Idle` → `Call`；`Stroke` 中再次点击 = 快速连点 → 打断。
    fn on_call(&mut self, now_ms: i64, cfg: &CoaxCfg) -> Vec<CoaxEffect> {
        if self.away || !self.engaged {
            return Vec::new();
        }
        match self.step {
            CoaxStep::Idle => {
                self.step = CoaxStep::Call;
                self.progress = 0.0;
                self.stroke_ms = 0;
                self.hide_at_ms = None;
                self.emit(0.0, CoaxStep::Call)
            }
            // 中途快速连点 = 打断（`01 §6.5.2`）：回退且附加 P 惩罚。
            CoaxStep::Stroke => self.rollback(now_ms, cfg, CoaxFailReason::Interrupted),
            // `Call` 幂等 / `Heart` 忽略 / `Runaway`、`Away` 需先找回。
            _ => Vec::new(),
        }
    }

    /// 比心：仅在 `Heart` 且未超窗时成功，否则按超时回退。
    fn on_heart(&mut self, now_ms: i64, cfg: &CoaxCfg) -> Vec<CoaxEffect> {
        if self.step != CoaxStep::Heart {
            return Vec::new();
        }
        match self.heart_deadline_ms {
            Some(deadline) if now_ms <= deadline => {
                self.step = CoaxStep::Idle;
                self.away = false;
                self.recalled = false;
                self.ran_ms = None;
                self.progress = 0.0;
                self.stroke_ms = 0;
                self.last_stroke_ms = None;
                self.gain_anchor_ms = None;
                self.heart_deadline_ms = None;
                self.hide_at_ms = None;
                let mut out = self.emit(0.0, CoaxStep::Idle);
                out.push(CoaxEffect::Succeeded);
                out
            }
            _ => self.rollback(now_ms, cfg, CoaxFailReason::Timeout),
        }
    }

    /// 托盘「摸摸」：每次折算一格抚摸（`02 §5.23`：累计 5 次 = 抚摸阶段）。
    fn on_tray_stroke(&mut self, now_ms: i64, cfg: &CoaxCfg) -> Vec<CoaxEffect> {
        if self.away || !self.engaged {
            return Vec::new();
        }
        if matches!(self.step, CoaxStep::Runaway) {
            return Vec::new();
        }
        let total = stroke_target_ms(cfg, self.easy_mode);
        let per_tap = (total / TRAY_COAX_STROKE_TAPS).max(1);
        let mut out = Vec::new();
        if matches!(self.step, CoaxStep::Idle | CoaxStep::Call) {
            if self.step == CoaxStep::Idle {
                // `Idle` → 全新一轮：清零进度（`Call` → 保留中断回退的进度继续累加）。
                self.progress = 0.0;
                self.stroke_ms = 0;
            }
            self.step = CoaxStep::Stroke;
            self.hide_at_ms = None;
            out.extend(self.emit(self.progress, CoaxStep::Stroke));
        }
        self.stroke_ms += per_tap;
        // 每格抚摸等价 `per_tap/2s` 份的 Mood 增益（与桌面抚摸同源速率）。
        out.push(CoaxEffect::MoodGain(
            cfg.mood_gain_per_two_sec * (per_tap as f32 / MOOD_GAIN_PERIOD_MS as f32),
        ));
        self.progress = if total > 0 {
            (self.stroke_ms as f32 / total as f32).clamp(0.0, 1.0)
        } else {
            1.0
        };
        if self.progress >= 1.0 {
            self.step = CoaxStep::Heart;
            self.heart_deadline_ms = Some(now_ms + (cfg.heart_window_sec as i64).max(0) * 1_000);
            out.extend(self.emit(1.0, CoaxStep::Heart));
        } else {
            out.extend(self.emit(self.progress, CoaxStep::Stroke));
        }
        out
    }

    /// L5 找回：解除离家、回到 `Idle`（仍需完成完整三部曲）。
    fn recall(&mut self) -> Vec<CoaxEffect> {
        if !matches!(self.step, CoaxStep::Runaway | CoaxStep::Away) {
            return Vec::new();
        }
        self.away = false;
        self.recalled = true;
        self.ran_ms = None;
        self.step = CoaxStep::Idle;
        self.progress = 0.0;
        self.stroke_ms = 0;
        self.last_stroke_ms = None;
        self.gain_anchor_ms = None;
        self.heart_deadline_ms = None;
        self.hide_at_ms = None;
        self.emit(0.0, CoaxStep::Idle)
    }

    // -- 内部：回退与广播 --------------------------------------------------

    /// 回退（保留部分进度，不重置）+ 失败效果。
    ///
    /// `interruptRollbackRatio` 只回退**抚摸累计**，不重置整个序列（`01 §6.5.2`）；
    /// 回退后进度环按 `ringHoldSec` 保持显示，便于用户看到「保住了多少」。
    fn rollback(&mut self, now_ms: i64, cfg: &CoaxCfg, reason: CoaxFailReason) -> Vec<CoaxEffect> {
        let ratio = cfg.interrupt_rollback_ratio.clamp(0.0, 1.0);
        self.stroke_ms = (self.stroke_ms as f32 * ratio).round() as i64;
        let total = stroke_target_ms(cfg, self.easy_mode);
        self.progress = if total > 0 {
            (self.stroke_ms as f32 / total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.step = CoaxStep::Call; // 保留进度，回到「已呼唤」态（不重置序列）。
        self.last_stroke_ms = None;
        self.gain_anchor_ms = None;
        self.heart_deadline_ms = None;
        self.hide_at_ms = Some(now_ms + (cfg.ring_hold_sec as i64).max(0) * 1_000);
        let mut out = vec![CoaxEffect::Failed(reason)];
        out.extend(self.emit(self.progress, self.step));
        out
    }

    /// 按 [`PROGRESS_EMIT_STEP`] 粒度 / 子状态变化去重后广播（压 20Hz 档广播量）。
    fn emit(&mut self, ratio: f32, step: CoaxStep) -> Vec<CoaxEffect> {
        let pct = (ratio / PROGRESS_EMIT_STEP).round() as i32;
        if pct == self.emitted_pct && self.emitted_step == Some(step) {
            return Vec::new();
        }
        self.emitted_pct = pct;
        self.emitted_step = Some(step);
        vec![CoaxEffect::Progress { ratio, step }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_000;

    fn cfg() -> CoaxCfg {
        CoaxCfg::default()
    }

    /// 把状态机推进到「进度环满、等待比心」。
    fn fill_ring(f: &mut CoaxFlow, cfg: &CoaxCfg, start_ms: i64) -> i64 {
        let mut t = start_ms;
        // 首拍进入抚摸态。
        f.tick(t, true, cfg);
        // 每 100ms 推进一次，直到环满（5s → 50 次）。
        for _ in 0..200 {
            t += 100;
            f.tick(t, true, cfg);
            if f.step() == CoaxStep::Heart {
                break;
            }
        }
        t
    }

    // ══════════════════════════════════════════════════════════════════
    // 目标档位（`01 §6.5.3` 状态机）
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn coax_target_level_follows_state_machine() {
        assert_eq!(coax_target_level(0), 0);
        assert_eq!(coax_target_level(1), 1);
        assert_eq!(coax_target_level(2), 2);
        assert_eq!(coax_target_level(3), 2, "生闷气 → 委屈");
        assert_eq!(coax_target_level(4), 3, "生气 → 生闷气");
        assert_eq!(coax_target_level(5), 3, "离家出走 → 生闷气（跨度 2 级）");
    }

    #[test]
    fn stroke_target_follows_easy_mode() {
        let c = cfg();
        assert_eq!(stroke_target_ms(&c, false), 5_000);
        assert_eq!(stroke_target_ms(&c, true), 2_000);
    }

    // ══════════════════════════════════════════════════════════════════
    // AC-04：完成三部曲 → 成功
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn ac04_full_trilogy_succeeds() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(4, 0);
        f.on_input(CoaxInput::Call, 0, &c);
        assert_eq!(f.step(), CoaxStep::Call);
        let t = fill_ring(&mut f, &c, 100);
        assert_eq!(f.step(), CoaxStep::Heart, "抚摸 5s 后应进入比心窗");
        assert!((f.progress() - 1.0).abs() < 1e-6);
        let out = f.on_input(CoaxInput::Heart, t + 500, &c);
        assert!(out.contains(&CoaxEffect::Succeeded), "窗内比心应成功：{out:?}");
        assert_eq!(f.step(), CoaxStep::Idle);
        assert!(!f.is_active(), "成功后进度环应隐藏");
    }

    #[test]
    fn mood_gain_happens_every_two_seconds_of_stroking() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(3, 0);
        let mut t = 0;
        f.tick(t, true, &c);
        let mut gains = 0;
        // 5s 抚摸（进度环在 5s 处满 → 转 Heart，此后不再计抚摸增益）。
        for _ in 0..60 {
            t += 100;
            for e in f.tick(t, true, &c) {
                if matches!(e, CoaxEffect::MoodGain(_)) {
                    gains += 1;
                }
            }
            if f.step() == CoaxStep::Heart {
                break;
            }
        }
        assert_eq!(gains, 2, "5s 抚摸（第 2s / 第 4s 各一次）→ 恰 2 次 Mood 增益");
    }

    #[test]
    fn heart_window_timeout_rolls_back_and_fails() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(4, 0);
        let t = fill_ring(&mut f, &c, 0);
        assert_eq!(f.step(), CoaxStep::Heart);
        // 超过 10s 窗 → 超时回退。
        let out = f.tick(t + 11 * T, false, &c);
        assert!(
            out.contains(&CoaxEffect::Failed(CoaxFailReason::Timeout)),
            "超窗应失败：{out:?}"
        );
        assert_eq!(f.step(), CoaxStep::Call, "失败后回到呼唤态且保留部分进度");
        assert!(f.progress() > 0.0 && f.progress() < 1.0, "进度应部分保留：{}", f.progress());
    }

    // ══════════════════════════════════════════════════════════════════
    // AC-05：中途放弃抚摸 → 进度回退 50%，不重置
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn ac05_abandon_stroke_rolls_back_half_without_reset() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(4, 0);
        // 抚摸 2s（50% 进度）。
        let mut t = 0;
        f.tick(t, true, &c);
        for _ in 0..20 {
            t += 100;
            f.tick(t, true, &c);
        }
        let before = f.progress();
        assert!((before - 0.4).abs() < 0.05, "2s/5s ≈ 0.4，实际 {before}");
        // 松手 → 放弃。
        let out = f.tick(t + 100, false, &c);
        assert!(
            out.contains(&CoaxEffect::Failed(CoaxFailReason::Abandoned)),
            "松手应判定放弃：{out:?}"
        );
        let after = f.progress();
        assert!(after > 0.0, "进度必须部分保留（不重置）：{after}");
        assert!((after - before * 0.5).abs() < 0.06, "回退 50%：{before} → {after}");
        assert_eq!(f.step(), CoaxStep::Call, "回到呼唤态");
    }

    /// 中断回退之后继续抚摸，应保留此前进度（累加而非从头）。
    #[test]
    fn rollback_progress_is_retained_and_accumulates() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(4, 0);
        let mut t = 0;
        f.tick(t, true, &c);
        for _ in 0..20 {
            t += 100;
            f.tick(t, true, &c);
        }
        f.tick(t + 100, false, &c); // 放弃 → 回退
        let retained = f.progress();
        // 再次抚摸 1s，应从 retained 继续累加。
        let mut t2 = t + 200;
        f.tick(t2, true, &c);
        for _ in 0..10 {
            t2 += 100;
            f.tick(t2, true, &c);
        }
        assert!(f.progress() > retained, "保留进度应继续累加：{retained} → {}", f.progress());
    }

    // ══════════════════════════════════════════════════════════════════
    // 打断（负向事件 / 快速连点）
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn negative_event_interrupts_trilogy() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(4, 0);
        let mut t = 0;
        f.tick(t, true, &c);
        for _ in 0..10 {
            t += 100;
            f.tick(t, true, &c);
        }
        let out = f.on_input(CoaxInput::Negative, t + 10, &c);
        assert!(
            out.contains(&CoaxEffect::Failed(CoaxFailReason::Interrupted)),
            "负向事件应打断：{out:?}"
        );
    }

    #[test]
    fn rapid_click_during_stroke_interrupts() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(4, 0);
        let mut t = 0;
        f.tick(t, true, &c);
        for _ in 0..5 {
            t += 100;
            f.tick(t, true, &c);
        }
        let out = f.on_input(CoaxInput::Call, t + 10, &c);
        assert!(
            out.contains(&CoaxEffect::Failed(CoaxFailReason::Interrupted)),
            "抚摸中快速连点应打断：{out:?}"
        );
    }

    #[test]
    fn call_outside_coax_domain_is_ignored() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        // 未 sync 到 L3+ → 未 engaged。
        assert!(f.on_input(CoaxInput::Call, 0, &c).is_empty());
        assert_eq!(f.step(), CoaxStep::Idle);
    }

    // ══════════════════════════════════════════════════════════════════
    // AC-06/AC-07：L5 离家与找回
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn l5_starts_runaway_then_goes_away_after_performance() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        let out = f.sync_level(5, 0);
        assert!(
            out.contains(&CoaxEffect::Progress { ratio: 0.0, step: CoaxStep::Runaway }),
            "进入 L5 应开始离家演出：{out:?}"
        );
        assert!(!f.is_away());
        let out = f.tick(RUNAWAY_PERFORMANCE_MS - 1, false, &c);
        assert!(!f.is_away(), "演出未满不得离家");
        assert!(out.is_empty());
        let out = f.tick(RUNAWAY_PERFORMANCE_MS, false, &c);
        assert!(f.is_away(), "演出满 6s 应离家");
        assert!(
            out.contains(&CoaxEffect::Progress { ratio: 0.0, step: CoaxStep::Away }),
            "应广播离家态：{out:?}"
        );
    }

    #[test]
    fn ac07_recall_brings_back_and_trilogy_still_required() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(5, 0);
        f.tick(RUNAWAY_PERFORMANCE_MS, false, &c);
        assert!(f.is_away());
        // 找回 → 走回（可见），仍需三部曲。
        let out = f.on_input(CoaxInput::Recall, RUNAWAY_PERFORMANCE_MS + T, &c);
        assert!(!f.is_away(), "找回后应恢复可见");
        assert!(
            out.contains(&CoaxEffect::Progress { ratio: 0.0, step: CoaxStep::Idle }),
            "找回应广播：{out:?}"
        );
        // 找回后 sync_level(5) 不得再次离家。
        assert!(f.sync_level(5, RUNAWAY_PERFORMANCE_MS + 2 * T).is_empty(), "不得重复触发离家");
        assert!(!f.is_away());
        // 完成三部曲 → 成功。
        f.on_input(CoaxInput::Call, 0, &c);
        let t = fill_ring(&mut f, &c, 10 * T);
        let out = f.on_input(CoaxInput::Heart, t + 100, &c);
        assert!(out.contains(&CoaxEffect::Succeeded));
    }

    #[test]
    fn recall_without_runaway_is_noop() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(4, 0);
        assert!(f.on_input(CoaxInput::Recall, 0, &c).is_empty());
    }

    // ══════════════════════════════════════════════════════════════════
    // force_lower（S4-M4）+ 阶段同步清理
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn force_lower_clears_runaway_and_ring() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(5, 0);
        f.tick(RUNAWAY_PERFORMANCE_MS, false, &c);
        assert!(f.is_away());
        let out = f.force_lower();
        assert!(!f.is_away(), "force_lower 必须解除离家");
        assert_eq!(f.step(), CoaxStep::Idle);
        assert!(
            out.iter().any(|e| matches!(e, CoaxEffect::Progress { step: CoaxStep::Idle, .. })),
            "应广播恢复：{out:?}"
        );
    }

    #[test]
    fn dropping_below_coax_domain_clears_state() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(4, 0);
        f.on_input(CoaxInput::Call, 0, &c);
        assert_eq!(f.step(), CoaxStep::Call);
        let out = f.sync_level(2, T);
        assert!(!f.is_active(), "回到 L2 应清空进度环");
        assert!(!f.is_engaged());
        assert!(
            out.iter().any(|e| matches!(e, CoaxEffect::Progress { step: CoaxStep::Idle, .. })),
            "应广播隐藏：{out:?}"
        );
    }

    // ══════════════════════════════════════════════════════════════════
    // 托盘替代入口（`02 §5.23` 第 2 层）
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn tray_five_taps_fill_stroke_stage() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(5, 0);
        f.on_input(CoaxInput::Recall, T, &c);
        for i in 0..TRAY_COAX_STROKE_TAPS {
            f.on_input(CoaxInput::TrayStroke, T * (i + 2), &c);
        }
        assert_eq!(f.step(), CoaxStep::Heart, "累计 5 次托盘抚摸应满环");
        let out = f.on_input(CoaxInput::Heart, T * 8, &c);
        assert!(out.contains(&CoaxEffect::Succeeded), "托盘比心应成功：{out:?}");
    }

    #[test]
    fn tray_stroke_ignored_before_recall() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(5, 0);
        f.tick(RUNAWAY_PERFORMANCE_MS, false, &c);
        assert!(f.is_away());
        assert!(f.on_input(CoaxInput::TrayStroke, 0, &c).is_empty(), "离家期间托盘不可达");
    }

    // ══════════════════════════════════════════════════════════════════
    // 广播节流（1% 粒度，同状态同比例不重发）
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn progress_broadcast_is_throttled_to_one_percent() {
        let c = cfg();
        let mut f = CoaxFlow::new();
        f.sync_level(4, 0);
        f.tick(0, true, &c);
        // 同一时刻重复推进（dt=0）不得重复广播。
        assert!(f.tick(0, true, &c).is_empty(), "dt=0 不应产生新广播");
    }

    #[test]
    fn clear_without_visible_state_emits_nothing() {
        let mut f = CoaxFlow::new();
        assert!(f.clear().is_empty());
    }
}
