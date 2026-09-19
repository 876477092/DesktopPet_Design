//! `dp-app/src/coreloop.rs` —— S3-M0 运行时装配层：core-loop 三档 tick 与五引擎装配。
//!
//! 目标（03 台账 §2 S3-M0 卡片；闭合架构审查 F-01「S2 引擎零装配点 ⇒ 宠物不会动」与 G5）：
//! **把「库」变成「会动的程序」**——实例化五大引擎（`MotionEngine` / `PhysicsEngine` /
//! `PlatformGraph` / `PerceptionBus` / `SystemWallClock`）+ 注入端口 + 三档 tick 编排 +
//! `MotionEvent::Landed → ActionArbiter::submit(ACT-M-06)` 接线。
//!
//! ## 结构（纯逻辑 ⇄ Tauri 外壳分离，为了可单测）
//!   - [`CoreLoopState`]：**纯逻辑**相机器 + 引擎持有者，不依赖 Tauri / 窗口，可 `#[cfg(test)]`
//!     直接驱动；[`CoreLoopState::logic_tick`] 即设计补充 §3.2 的逐步顺序。
//!   - [`ScheduleGrid`]：三档 deadline 的**绝对锚定**网格（第 k 次 deadline = `start + k×间隔`，
//!     禁止逐 tick 累加 sleep）。
//!   - [`spawn`]：Tauri 外壳——装配 [`CoreLoopState`]、起感知线程与 core-loop 线程
//!     （口径同 `supervisor::spawn`：**故意不标 `#[must_use]`**，失败降级空句柄）。
//!     各里程碑接线明细（S3-M2 命中粗筛 / S3-M3 手势消费 / S3-M4 意图 → 相机接线 /
//!     S3-M6 触发映射与右键菜单 / S4 B15-④ 播放指令通道）统一见 [`spawn`] 与
//!     [`run_loop`] 的就地注释，此处不再逐条重复（B14-⑨ 文档去重，纯注释变更）。
//!
//! ## 引擎协同不变量（设计补充 §1.2）
//!   A 单写者：任一 logic tick **至多 tick 一个引擎**（Roam/Fall 相恰一个活跃，
//!   另一停放；Drag 相两引擎均停放，权威 pos = 光标钳制位）；
//!   B 单一来源：权威 `pos` = 被 tick 引擎的 `pos()`（Drag 相 = 钳制后光标位），
//!   窗口位置只由此单点写出；
//!   C 换相重建：两引擎均无 `set_pos`，换相 = 以离场引擎末位 `pos` 重建入场引擎。
//!
//! ## 时钟口径（C3）
//!   四引擎 `now_ms` **一律**取 `loop_start.elapsed().as_millis()`（`Instant` 单调）；
//!   `SystemWallClock`（i64）**不进节拍**，仅注入 1Hz 业务档空槽。dt 观测口径钳
//!   `MAX_TICK_DT_MS`（引擎内部已自钳，本层不重写物理子步）。
//!
//! ## 红线
//!   C1 无盘符字面量；C3 见上；C8 `pet://` 事件白名单——S3-M5 前本层零新增
//!   （render 档**不产帧**，帧仍归 `bridge::spawn_frame_player`，避免 `pet://frame`
//!   双写者）；**S3-M6 起新增两个已登记事件**：`pet://fx`（粒子迸发，触发映射）
//!   与 `pet://menu`（右键单击命中弹菜单）；**S4-M2 起新增两个已登记事件**：
//!   `pet://state`（1Hz 全量 `PetSnapshotV2`）与 `pet://emotion`（阶段迁移详情），
//!   二者均已登记 `02 §7.6`，其余仍禁；
//!   C9 零网络。
//!   `dp-core` 内**零** `use dp_platform`（本文件属 `dp-app`）。
//!
//! Windows 门控：本文件 `#![cfg(windows)]` 自门禁（引用 `crate::PetPlatform` 与
//! `dp-platform/win/*`），装配方 `lib.rs` 只需 `#[cfg(windows)] pub mod coreloop;`。

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dp_core::anim::{
    ActionArbiter, ActionCatalog, ActionRequest, ActionSource, Arbitration, Started,
};
use dp_core::config::{ConfigBundle, ConfigService};
use dp_core::config::model::{
    AchievementsConfig, ActivityGlobalCfg, CatchphraseCfg, ClickFeedbackCfg, EmotionConfig,
    GestureCfg, InteractionCfg, NeedsConfig, RoamCfg, ShopConfig,
};
// S8-M1：外出活动运行时（`02 §5.13`；`ActivityGlobalCfg` 由 dp-core 配置束提供，
// 强类型活动模型住在 dp-activity——依赖方向 dp-app → dp-activity → dp-core）。
use dp_activity::machine::DispatchCheck;
use dp_activity::model::{
    ActivityError, ActivityInstance, ActivityKind, ActivityReward, ActivitySave,
    ActivityPhase, RecallKind, SettleInputs,
};
use dp_activity::{ActivityOutcome, ActivityRuntime};
use dp_core::state::ActivityDeltas;
use dp_core::emotion::coax::CoaxStep;
use dp_core::emotion::solver::InteractionPolicy;
use dp_core::emotion::TickEnv;
use dp_core::emotion::lines::{
    BubblePlanner, LinesLibrary, PlaceholderVars, PlannedBubble, render_placeholders,
};
use dp_core::emotion::{EmotionEngine, EmotionEvent, OfflineOutcome};
use dp_core::event::{project_snapshot, wire_for_bubble, wire_for_events, wire_for_needs};
#[cfg(test)]
use dp_core::event::PetSnapshotV2;
use dp_core::interaction::{InteractionKind, THROW_LANDING_CHAIN, act_of};
use dp_core::motion::physics::{MAX_TICK_DT_MS, SUB_STEP_MS};
use dp_core::motion::{
    MotionEngine, MotionEvent, PhysicsEngine, PlatformGraph, PlatformInputs, StandSurface, Vec2,
};
use dp_core::needs::{DispatchKind, NeedsActionTrigger};
use dp_platform::win::tray::TrayIconState;
use dp_core::perception::{
    ActivitySample, PerceptionBus, PerceptionEvent, ReminderChannel, ReminderConfig,
    ReminderScheduler, SystemSample, SystemWallClock, WallClock,
};
use dp_core::save::{LoadOutcome, SaveStore};

use tauri::{AppHandle, Emitter, Manager};

use dp_audio::{AudioBus, AudioCue, AudioSettings};
use dp_platform::win::cursor::read_cursor_vdc;
use dp_platform::win::session::SessionWatcher;
use dp_platform::win::system::{battery_status, last_input_idle_ms, CpuLoadSampler};
use dp_platform::{DisplayService, PlatformWindow, WinPlatformWindow};

use crate::PetPlatform;
use crate::bridge::{
    self, CoreInput, CoreInputChannel, FX_EVENT, MENU_EVENT, ParticleCmd, ParticleKind,
    PlaybackChannel, PlaybackOrder, PlaybackReport, STATE_EVENT, resolve_save_dir,
};
use crate::hit_latest::HitLatestHandle;
use crate::hook_sink::ChannelSink;
use crate::interaction_consumer::InteractionConsumer;
use crate::ports;

/// 比心 / 抚摸 / 戳痒的爱心迸发数量（S3-M6 触发映射表现档；文档未冻结数值）。
const FX_HEART_BURST_COUNT: u32 = 12;

/// 甩出落地尘土迸发数量（S3-M6 表现档；落地由 `landed_thrown` 触发，挂落地链起点）。
const FX_DUST_BURST_COUNT: u32 = 16;

/// 提醒演出动作 ID（`01 §6.3.2` P 类动作表：`ACT-P-03` 伸手指提醒）。
///
/// 单一真源说明：动作元数据（优先级 / 打断规则 / 演出标记）仍以 `actions.json` 为准，
/// 本常量只是「提醒 → 动作」映射的键；目录缺该动作或 `disabled`（批次 C 资源未交付）
/// 时 [`ActionRequest::from_cfg`] 返回 `None`，触发面安全降级为仅日志。
const REMINDER_ACTION_ID: &str = "ACT-P-03";

/// 活动感知采样间隔（毫秒；`02 §5.6`：键击/点击/移动强度 1Hz 级汇总）。
const ACTIVITY_INTERVAL_MS: u64 = 1_000;

/// 三部曲成功后取词的台词池（S4-M5：`runaway` 池含 AC-04「哼…原谅你啦，下不为例！」）。
const RUNAWAY_POOL: &str = "runaway";

/// 进入比心窗时取词的台词池（S4-M5：比心撒娇属正向亲昵场景）。
const HAPPY_POOL: &str = "happy";

/// 回归问候动作（S5-M2：`02 §6.1` 启动时序末行「触发 ACT-T-01 挥手 + 时段问候」）。
///
/// 该动作在 `actions.json` 的触发类型为 `lifecycle.value = "launchOrLongAbsence"`
/// ——「启动或长时间离开后返回」，与本处的调用时机（离线补偿之后）逐字对应。
const STARTUP_GREETING_ACTION: &str = "ACT-T-01";

// 存档目录名 `SAVE_DIR_NAME` 与目录解析 `resolve_save_dir` 已上移到 `crate::bridge`
// （S5-M4：设置页「数据」Tab 的 `save_status` 命令同样需要它，上移避免双份实现）。

// ---------------------------------------------------------------------------
// 周期常量（三档 + 感知三档；C3 绝对锚定网格的步长）
// ---------------------------------------------------------------------------

/// logic 档间隔（20Hz = 50ms；`02 §1.4`）。
pub const LOGIC_INTERVAL_MS: u64 = 50;

/// 业务档间隔（1Hz = 1000ms；`02 §1.4`，S3-M0 只搭空槽）。
pub const BIZ_INTERVAL_MS: u64 = 1_000;

/// render 档默认间隔（≈60fps；无 `FrameTierHandle` 时的兜底，实际按 K-4 档位自适应）。
pub const RENDER_DEFAULT_INTERVAL_MS: u64 = 16;

/// 光标采样间隔（20Hz，FR-6-1）。
pub const CURSOR_INTERVAL_MS: u64 = 50;

/// 窗口枚举间隔（0.5Hz = 2s，FR-6-4）。
pub const WINDOWS_INTERVAL_MS: u64 = 2_000;

/// 系统采样间隔（0.1Hz = 10s，FR-6-3）。
pub const SYSTEM_INTERVAL_MS: u64 = 10_000;

/// 落地缓冲动作 ID（`01 §6.3.2`；源码中不存在常量，取自 `resources/config/actions.json`）。
const LANDING_ACTION_ID: &str = "ACT-M-06";

/// `seed_k` 递推乘子（SplitMix 风格摘取；设计补充 §1.2-6，避免同种子重复漫游序列）。
const SEED_GAIN: u64 = 6364136223846793005;

/// 甩出飞行时长上限（毫秒；FR-4-6：甩出后 1.5s 内必须完成落地）。
const THROW_MAX_FLIGHT_MS: u64 = 1_500;

/// 钳制公式的离散化裕量（毫秒；3 个物理积分子步 + 1 个 logic 档间隔）。
///
/// 覆盖三段误差：① 半隐式欧拉离散解相对连续闭式解的落地偏差（≤ 2 子步，20ms）；
/// ② `PhysicsEngine::thrown` 首 tick `dt=0` 只锚定不推进——物理推进自下一 logic
/// tick 开始，墙钟口径恒损失 1 个 logic 档间隔（50ms）；③ 落地 tick 按 50ms 粒度
/// 取整的尾差。三者合计取 80ms：连续飞行 ≤ `T_eff = 上限 − 本裕量` 时，落地墙钟
/// ≤ `T_eff + 80 = THROW_MAX_FLIGHT_MS`（FR-4-6 成立）。
const THROW_FLIGHT_MARGIN_MS: u64 = 3 * SUB_STEP_MS + LOGIC_INTERVAL_MS;

/// 宠物窗口物理高度（像素；与 `tauri.conf.json` 窗口 256×256、图集锚点
/// `anchor.y = 256` 对齐）。2026-09-13 现场修复：`outcome.pos` 为脚底锚点
/// （`dp-core` 站立语义 = 工作区底边，测试断言 `pos.y = work_bottom`），
/// core-loop 写窗口位置须换算为窗口顶部（`pos.y − 窗口物理高 ÷ scale`），
/// 否则窗口垂直溢出屏外（实测窗口顶 T=1392、窗口底 1648 > 屏高 1440，溢出 208px）。
const PET_WINDOW_PHYS_H: f32 = 256.0;

// ---------------------------------------------------------------------------
// 相机器与逻辑产出（纯逻辑）
// ---------------------------------------------------------------------------

/// 运行相（三态；设计补充 §1.2，S3-M4 增拖拽相）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// 漫游相：活跃引擎 = [`MotionEngine`]，`PhysicsEngine` 停放。
    Roam,
    /// 下坠相：活跃引擎 = [`PhysicsEngine`]（`surface = PlatformGraph`），`MotionEngine` 停放。
    Fall,
    /// 拖拽相（S3-M4）：**两引擎均停放**，权威 `pos` = 钳制后光标位（不变量 B 特例，
    /// 由 [`CoreLoopState::logic_tick`] 的 Drag 臂单点刷新）。
    Drag,
}

/// 一次 logic tick 的产出（纯逻辑；无 Tauri 依赖）。
#[derive(Clone, Debug, PartialEq)]
pub struct LogicOutcome {
    /// 本 tick 的权威位置（VDC；= 被 tick 引擎的 `pos()`）。
    pub pos: Vec2,
    /// 权威位置是否相对上一次发生改变（决定是否调 `set_position_vdc`）。
    pub moved: bool,
    /// 本 tick 唯一 `MotionEvent → 仲裁` 接线的结果（仅 `Landed` 时非空）。
    pub submitted: Option<Arbitration>,
    /// 本 tick 是否结算了一次**甩出落地**（S3-M6：run_loop 据此发落地尘土
    /// `pet://fx{kind:"dust"}`，挂落地链起点；纯标记，不影响结算本身）。
    pub landed_thrown: bool,
}

/// 显示器几何类型别名（等价 `dp_core::motion::MonitorGeom`）。
type MonitorGeomAlias = dp_core::motion::MonitorGeom;

/// `settings.json.interaction` → 内核侧交互策略（S7-M6 / FR-11-12 三层防护的数值口径）。
///
/// **单一搬运点**：全项目只在这里做「配置 → 内核策略」的字段搬运，避免多处各写一遍
/// 造成口径漂移。`available` 恒置 `true`——它是**每拍运行时量**，由
/// `TickEnv::interaction_available` 如实喂入，配置侧不参与（避免两处真源）。
#[must_use]
pub fn interaction_policy_of(cfg: &InteractionCfg) -> InteractionPolicy {
    InteractionPolicy {
        available: true,
        unavailable_presence_factor: cfg.unavailable_presence_factor,
        tray_fallback_enabled: cfg.tray_fallback_enabled,
        unreachable_floor_level: u8::try_from(cfg.unreachable_natural_floor_level).unwrap_or(4),
        unreachable_hold_sec: cfg.unreachable_hold_sec,
        unreachable_no_negative_sec: cfg.unreachable_no_negative_sec,
    }
}

/// `seed_k` 递推（SplitMix 风格摘取；公开以便单测直接断言递推性质）。
#[must_use]
pub fn advance_seed(seed: u64, now_ms: u64) -> u64 {
    seed.wrapping_mul(SEED_GAIN).wrapping_add(now_ms)
}

/// 触发映射（S3-M6，纯函数单测覆盖）：交互意图 → 粒子迸发命令（`pet://fx`）。
///
/// 映射口径（主理人 brief 冻结）：
///   - `DoubleClick` → 比心爱心（heart）；
///   - `Tickle` / `Stroke` → 头顶爱心（heart）；
///   - `Click` → 微反馈尘土（dust），**仅在 `clickFeedback.enabled` 时**，数量取配置；
///   - 其余意图（Hover / DragStart / Throw 起飞等）→ `None`（零迸发）；
///   - 甩出**落地**尘土不在此映射（落地非意图）：run_loop 依 `LogicOutcome.landed_thrown`
///     直发 `pet://fx{dust}`，挂落地链起点。
///
/// 数量统一经 [`bridge::particle_cmd`] 钳 `[1, 60]`（`02 §5.22` maxPerBurst）。
#[must_use]
pub fn fx_burst_for_intent(
    kind: InteractionKind,
    click_feedback: &ClickFeedbackCfg,
) -> Option<ParticleCmd> {
    match kind {
        InteractionKind::DoubleClick | InteractionKind::Tickle | InteractionKind::Stroke => {
            Some(bridge::particle_cmd(ParticleKind::Heart, FX_HEART_BURST_COUNT))
        }
        InteractionKind::Click if click_feedback.enabled => {
            Some(bridge::particle_cmd(ParticleKind::Dust, click_feedback.burst_count))
        }
        _ => None,
    }
}

/// 交互意图 → 音效 Cue（**S4-M6**；`01 §9.2` 清单的已落地触发面）。
///
/// 映射口径（唯一映射点，避免上层重复分支）：
///   - `Click` → 「嘿嘿」；`DoubleClick` → 比心「叮」；`Stroke` → 心跳；
///   - `Tickle` → 「呀啊啊」；`Throw` → 下落风声；
///   - 其余（Hover / EarTwitch / DragStart / 轨迹彩蛋 / S7-M6 占位变体）→ `None`。
///
/// 情绪态 → 音效的**完整**映射表见 `01 §6.5.5`，其尚未落地的表现模块（讨食/求洗澡/
/// 外出等）随各自里程碑补 Cue；本卡只接当前已存在的触发面。
#[must_use]
pub fn audio_cue_for_intent(kind: InteractionKind) -> Option<AudioCue> {
    match kind {
        InteractionKind::Click => Some(AudioCue::InteractHehe),
        InteractionKind::DoubleClick => Some(AudioCue::InteractHeart),
        InteractionKind::Stroke => Some(AudioCue::InteractHeartbeat),
        InteractionKind::Tickle => Some(AudioCue::InteractYaa),
        InteractionKind::Throw => Some(AudioCue::MoveWind),
        _ => None,
    }
}

/// 情绪算法事件 → 音效 Cue（**S4-M6**；`01 §6.5.5` 的 S4 已落地子集）。
///
/// 口径：
///   - 阶段**升级**（`ColdLevelChanged`）按目标档位取音：L1 叹气 / L2 抽泣 / L3「哼」/
///     L4 怒气音 / L5 脚步渐远（离家演出）；
///   - 三部曲：进比心窗 → 比心「叮」；离家演出开始 → 脚步渐远；完成 → 欢呼；失败 →「哼！」。
#[must_use]
pub fn audio_cue_for_emotion(ev: &EmotionEvent) -> Option<AudioCue> {
    match ev {
        EmotionEvent::ColdLevelChanged { to, .. } => match *to {
            1 => Some(AudioCue::EmotionSigh),
            2 => Some(AudioCue::EmotionSob),
            3 => Some(AudioCue::InteractHmph),
            4 => Some(AudioCue::EmotionAnger),
            5 => Some(AudioCue::EmotionStepsFade),
            _ => None,
        },
        EmotionEvent::CoaxProgress { step, .. } => match step {
            CoaxStep::Heart => Some(AudioCue::InteractHeart),
            CoaxStep::Runaway | CoaxStep::Away => Some(AudioCue::EmotionStepsFade),
            _ => None,
        },
        EmotionEvent::CoaxSucceeded { .. } => Some(AudioCue::EmotionCheer),
        EmotionEvent::CoaxFailed { .. } => Some(AudioCue::InteractHmph),
        _ => None,
    }
}

/// core-loop 装配用的配置束（S4-M1 起引入：把 4 份配置从 `CoreLoopState::new` 的
/// 位置参数里收进一个具名结构体，避免参数表膨胀到 clippy 的 7 参数上限）。
///
/// `emotion` / `needs` 在构造时被 `Box::leak` 固化为 `'static`（见
/// [`CoreLoopState::new`]），故本结构体只在装配期短暂存在。
///
/// `Default` 供**测试夹具**与「只关心个别字段」的装配点用 `..CoreCfg::default()` 补全
/// （`lines` 为空库、`audio` 为 `None`，即「无台词 / 无音效」的静默形态）。
#[derive(Debug, Clone, Default)]
pub struct CoreCfg {
    /// 漫游配置。
    pub roam_cfg: RoamCfg,
    /// 交互 / 物理配置。
    pub interaction_cfg: InteractionCfg,
    /// 动作目录。
    pub catalog: ActionCatalog,
    /// 情绪配置（`emotion.json`）。
    pub emotion: EmotionConfig,
    /// 需求配置（`needs.json`）。
    pub needs: NeedsConfig,
    /// 角色默认名（`character.json.defaultName`；C2：`{name}` 变量的唯一来源）。
    ///
    /// 用户改名（FR-7-1）经设置页写入存档后由上层重新注入；本卡只取配置默认名。
    pub character_default_name: String,
    /// 口头禅配置（`character.json.catchphrase`；`None` = 未装配，气泡退回 S4-M5 基线）。
    pub character_catchphrase: Option<CatchphraseCfg>,
    /// 台词库（`resources/config/lines.json`；缺失时为空库，只少气泡不崩）。
    pub lines: LinesLibrary,
    /// 音效总线（S4-M6；`None` = 纯逻辑模式 / 音频未装配：全部播放入口退化为 no-op）。
    pub audio: Option<AudioBus>,
    /// 存档（S5-M1；`None` = 纯逻辑模式 / 未装配：不载档、不落盘，行为与 S4 完全一致）。
    ///
    /// 装配期由 [`build_state`] 经 `SaveStore::load` 建立（含降级链）；`Some` 时
    /// [`CoreLoopState::new`] 会用它**恢复内核状态**（`EmotionEngine::restore`），
    /// 使重启后数值 / P / 敏感度 / 展示态连续。
    pub save: Option<SaveStore>,
    /// 活动全局配置（`activities.json.global`；S8-M1 派遣 / 结算 / 明信片口径，C7）。
    pub activities: ActivityGlobalCfg,
    /// 商城目录（`shop.json`；S8-M5/M6 经济上限 + 30 商品）。
    pub shop: ShopConfig,
    /// 成就目录（`achievements.json`；FR-9-1 幂等入账）。
    pub achievements: AchievementsConfig,
}

/// 共享站立面适配器（S7-M3 前置首件 / B12-① / P2-16）。
///
/// 作用：让 `MotionEngine` 的**决策层**与 core-loop 的**物理层**（
/// `PhysicsEngine::tick` 的落地判定）读同一份 [`PlatformGraph`]——
/// `PlatformGraph` 的节点集含每屏桌底（`DesktopBottom`，与 `DesktopFloor` 同语义）
/// 并叠加任务栏 / 窗口标题栏，因此是决策面 `DesktopFloor` 的**超集**：
/// 注入后「走上标题栏站立」在决策层生效，而 R2 降级（无标题栏）自动退化为桌底行走。
///
/// 位置：适配类型落在 `dp-app` 自有类型（E0117 规避既有口径：`PlatformGraph`
/// 与 trait 均在 `dp-core`，本 crate 不能为其追加 trait 实现）。
/// 恒等语义：`PlatformGraph` 的 `should_rebuild` / `rebuild` 仍由 core-loop 独家驱动，
/// 本适配器只读。
#[derive(Debug)]
struct SharedSurface(Arc<RwLock<PlatformGraph>>);

impl StandSurface for SharedSurface {
    fn is_valid_stand(&self, p: Vec2) -> bool {
        // 锁中毒（他线程 panic）→ 取回内层值继续（不 panic、不阻断运动）。
        let graph = self.0.read().unwrap_or_else(|e| e.into_inner());
        graph.is_valid_stand(p)
    }

    fn clamp_to_stand(&self, p: Vec2) -> Vec2 {
        let graph = self.0.read().unwrap_or_else(|e| e.into_inner());
        graph.clamp_to_stand(p)
    }
}

/// S8-M1/M4：活动演出槽位（`actionIds`；S8-M4 演出接线全量启用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivitySlot {
    /// 出发演出（ACT-N-09 / ACT-N-12）。
    Depart,
    /// 回归演出（ACT-N-10 / ACT-N-14）。
    Return,
    /// 学习桌面循环演出（ACT-N-11，S8-M4）。
    DeskLoop,
    /// 旅游明信片演出（ACT-N-13，S8-M4）。
    Postcard,
}

/// core-loop 纯逻辑状态：相机器 + 引擎持有者 + 端口暂存（**不依赖 Tauri / 窗口**）。
pub struct CoreLoopState {
    /// 权威位置（VDC；= 活跃引擎 `pos()` 的镜像）。
    pos: Vec2,
    /// 当前相。
    phase: Phase,
    /// 漫游决策引擎（Roam 相活跃）。
    motion: MotionEngine,
    /// 物理引擎（Fall 相活跃；Roam 相停放为 `None`）。
    physics: Option<PhysicsEngine>,
    /// 平台图（`StandSurface` 提供者 + 窗口标题栏站立面）。
    ///
    /// S7-M3 前置首件：改为 `Arc<RwLock<..>>` 共享——决策层经 [`SharedSurface`]
    /// 注入 `MotionEngine`，物理层直接读同一份图，两侧永不脱节。
    /// 重建仍按「整体替换 + 每 2s `rebuild`」既有口径（`§6-5`：无 `set_monitors`）。
    platform_graph: Arc<RwLock<PlatformGraph>>,
    /// 动作仲裁器（S3-M0 仅落地缓冲 submit）。
    arbiter: ActionArbiter,
    /// 动作目录（`ACT-M-06` 元数据来源）。
    catalog: ActionCatalog,
    /// 漫游种子递推值（每次换相重建 `MotionEngine` 时递推）。
    seed_k: u64,
    /// 当前显示器几何（权威 `pos` 归属 / 边界计算）。
    monitors: Vec<MonitorGeomAlias>,
    /// 待注入 `PlatformGraph::rebuild` 的带（由感知 `Windows` 事件暂存）。
    pending_inputs: PlatformInputs,
    /// 上一次 logic tick 的单调毫秒（dt 观测口径）。
    last_logic_ms: u64,
    /// 漫游配置（换相重建用）。
    roam_cfg: RoamCfg,
    /// 交互/物理配置（换相重建用）。
    interaction_cfg: InteractionCfg,
    /// 最近一次光标位置（VDC；重建 `MotionEngine` 后需重新注入）。
    cursor: Option<Vec2>,
    /// S7-M1：最近一次活动感知采样（键击 / 点击 / 移动强度 + 前台进程类别哈希；
    /// 隐私关闭时装配层**不产**该事件，此处保持默认「全 None」= 无信息）。
    ///
    /// 消费点：S7-M4 的 `busyness` 因子（本卡只交付采样与存储）。
    activity: ActivitySample,
    /// S7-M2：提醒调度器（久坐 / 喝水；`03 §3.3 B21 ②` 的消费方）。
    reminders: ReminderScheduler,
    /// S7-M9：生存动作触发调度器（分档轮询 + 间隔闸门；`02 §5.11`，AC-21/22 触发面）。
    needs_trigger: NeedsActionTrigger,
    /// `schedule.json.reminders.ackResetsTimer`（出厂默认；用户改动落在存档 B 段）。
    reminder_ack_resets: bool,
    /// Drag 相光标（VDC；`begin_drag` 设置、run_loop 经 `drag_to` 逐 tick 跟新、
    /// `release_drag` 清空；钳制统一在 logic_tick Drag 臂完成）。
    drag_cursor: Option<Vec2>,
    /// 甩出飞行标记（S3-M4）：Fall 相落地时为真则结算 ACT-T-07→08 落地链并计数，
    /// 为假则走常规 ACT-M-06。仅 Fall 相有意义：进 Drag / 回 Roam 即清零。
    thrown_flight: bool,
    /// 甩出落地累计次数（C8 诊断视图；core-loop 单线程独占写，普通 u64 即可）。
    throw_landed_count: u64,
    /// 播放指令通道（S4 前清障 B15-④：core-loop → 播放器线程；`None` = 未注册，
    /// 全部发播方法退化为 no-op，行为与 S3 完全一致）。
    playback: Option<PlaybackChannel>,
    /// 情绪内核（S4-M1）。配置以 `Box::leak` 固化为 `'static`（装配期一次，见
    /// [`CoreLoopState::new`]），故本字段无生命周期参数、`CoreLoopState` 可自由 `move`
    /// 进 core-loop 线程。
    emotion: EmotionEngine<'static>,
    /// 会话监视（S4-M1 要点 3；轮询式锁屏 / 远程桌面检测 + idle 回退，见
    /// `dp_platform::win::session` 模块文档）。
    session: SessionWatcher,
    /// 最近一次系统采样的用户空闲毫秒（10s 采样 → 1Hz 业务档注入 `TickEnv`）。
    /// `None` = 该采样不可用（`GetLastInputInfo` 失败），此时在场判定保守取「在场」。
    presence_idle_ms: Option<u64>,
    /// 待喂入下一 tick 的交互净情绪增益（S4-M2：`Mood` 加项入口）。
    ///
    /// 交互发生（logic 档）时累加，业务档喂入 [`TickEnv::event_delta`] 后清零——
    /// 保证跨档不丢事件（logic 20Hz / biz 1Hz）且单次消费（幂等）。
    /// 暂停期间**不清零**（内核早退不消费），恢复后一次计入。
    pending_mood_delta: f32,
    /// 上一次同步的「离家」可见态（S4-M4：`away` 翻转时隐藏 / 恢复宠物窗口）。
    runaway_away: bool,
    /// 台词库（S4-M5：`resources/config/lines.json`；缺失时为空库）。
    lines: LinesLibrary,
    /// 气泡计划器（S4-M5：抽取冷却 + 求助退避 + 快捷按钮）。
    bubble: BubblePlanner,
    /// 占位符变量（C2：`{name}` 取 `character.json.defaultName`，不在代码中硬编码角色名）。
    vars: PlaceholderVars,
    /// 音效总线（S4-M6；`None` = 未装配，全部请求退化为 no-op）。
    audio: Option<AudioBus>,
    /// 存档门面（S5-M1；`None` = 未装配，不载档不落盘）。
    save: Option<SaveStore>,
    /// 「有变更待落盘」标记（S5-M1：`PersistNow` 事件置位，业务档 [`Self::save_tick`] 消费）。
    ///
    /// 通过 `PersistNow` 走**强制落盘**（跳过 30s 定时与 2s 合并窗口）——该事件由内核在
    /// 「阶段迁移 / 自然消气 / 三部曲结算」等**用户可感知的严重节点**产出
    /// （`02 §6.2` 时序末行），丢一次即丢钱/丢阶段。
    save_requested: bool,
    /// 回归问候（S5-M2）：离线补偿产出，在**首个业务档**投递（届时 `app` 与播放通道均已就绪）。
    ///
    /// `build_state` 阶段 core-loop 线程尚未启动、播放通道尚未接入，故不能就地发播。
    startup_greeting: Option<StartupGreeting>,
    /// 勿扰模式当前值（S5-M4 热更新；`01 FR-10-4`：暂停气泡与主动漫游）。
    dnd: bool,
    /// 勿扰是否静默气泡（`schedule.json.doNotDisturb.pauseBubbles`；出厂默认 `true`）。
    dnd_pause_bubbles: bool,
    /// 活跃感知开关（Q-18 / S5-M4 热更新）：关闭后在场判定退化为「恒在场」（纯时间模型）。
    activity_sensing: bool,
    /// 鼠标穿透是否开启（S5-M4 设置快照镜像；S7-M6 用作 FR-11-12 层 ① 的输入之一）。
    ///
    /// `interaction_available = !click_through && !dnd && !runaway_away`——三源合并后喂入
    /// `TickEnv::interaction_available`，**不可达判定只有这一个真源**。
    click_through: bool,
    /// 上一次同步到托盘的图标态（S7-M6：只在变化时调 `set_state`，避免每拍重建菜单）。
    tray_state: Option<TrayIconState>,
    /// 上一次同步到托盘的「离家」菜单位（决定是否追加「把{name}找回来」项）。
    tray_left_home: bool,
    /// 自动走动开关（S5-M4）。
    ///
    /// ⚠️ **已知缺口（登记 §3.3 遗留，非本卡可闭环）**：本卡只把它落到设置状态、存档 B 段
    /// 与 `pet://config` 广播，**运行期消费点尚未承接**。原因是 `MotionEngine` 目前没有
    /// 「漫游使能位」接口（只有 `walk_to` / `is_walking`），在 core-loop 侧反复
    /// `walk_to(self.pos)` 会造成起步-急停抖动，比不做更糟。按 F-01「悬空归口禁止」纪律，
    /// **不**把它指派给任何尚未开卡核实的模块。故 FR-7-4 的「每项即时生效」在 `autoRoam`
    /// 一项上**未闭环**，验收时不得据本字段判定该项通过。
    auto_roam: bool,
    /// S8-M1：外出活动运行时（打工 / 学习 / 旅游状态机；`02 §5.13`）。
    ///
    /// 唯一写者 = core-loop 线程（与 `emotion` 同生命周期）；dispatch / recall 经
    /// [`Self::apply_core_input`] 进入，1Hz 推进经 [`Self::activity_tick`]。
    activity_runtime: ActivityRuntime,
    /// S8-M1：墙钟端口（C3）。
    ///
    /// 活动 dispatch 的安静时段判定 / 回归结算墙钟的唯一真源（`activity_tick` 的
    /// `wall` 参数同源）；测试经 [`Self::set_wall_for_test`] 注入 `FakeWallClock`。
    wall: Arc<dyn WallClock>,
    /// S8-M1：用户召回标记（`ActivityRecall` 置位；`settle_returning` 消费）。
    ///
    /// `Some(Early)` = 本次回归是用户提前召回（收益 ×0.5 + P+6 + rough+0.15）；
    /// `None` = 自然到期（收益全量，P−20）。异常中止不经此通道（`Aborted` 直结）。
    pending_recall_kind: Option<RecallKind>,
    /// S8-M5/M6：经济运行时（账本 / 限购 / 目录 / 背包 / 成就）。
    economy: crate::economy_rt::EconomyRuntime,
}

/// 回归问候（S5-M2：`02 §6.1` 启动时序「ACT-T-01 挥手 + 时段问候」）。
#[derive(Clone, Debug)]
struct StartupGreeting {
    /// 回归动作（恒 [`STARTUP_GREETING_ACTION`]；目录缺该动作时降级跳过）。
    action_id: &'static str,
    /// 时段问候气泡（取自冷落档位台词池；`None` = 无台词库 / 冷却未过）。
    bubble: Option<PlannedBubble>,
}

impl CoreLoopState {
    /// 构造核心状态。
    ///
    /// `pos` 为初始位置（VDC，构造时被 `MotionEngine` 钳到桌面地板）；`now_ms` 为启动单调毫秒
    /// （首档 deadline 锚点）；`seed_k` 为漫游 PRNG 种子。
    ///
    /// 构造核心状态。
    ///
    /// `pos` 为初始位置（VDC，构造时被 `MotionEngine` 钳到桌面地板）；`now_ms` 为启动单调毫秒
    /// （首档 deadline 锚点）；`seed_k` 为漫游 PRNG 种子。配置项统一经 [`CoreCfg`] 传入。
    ///
    /// `cfg.emotion` / `cfg.needs` 为 S4-M1 情绪内核的配置本体，在此**按值**接收并以
    /// `Box::leak` 固化为 `'static`，使 [`EmotionEngine`] 能借用稳定地址而无需把生命周期
    /// 参数传染给整个 `CoreLoopState`（后者要被 `move` 进 core-loop 线程）。
    ///
    /// 泄漏口径（C7 说明）：固化对象 = 两份配置（KB 级），发生时机 = core-loop 装配一次，
    /// 与进程同生命周期——进程退出即由 OS 回收，**不是**稳态泄漏。替代方案（在 `AppHandle`
    /// 内以 `Arc` 持有配置并让 `EmotionEngine` 借用）会把 `Arc` 的生命周期绑到 Tauri 状态
    /// 管理器上，反而引入跨层所有权纠缠；本卡取前者。
    #[must_use]
    pub fn new(
        pos: Vec2,
        monitors: Vec<MonitorGeomAlias>,
        cfg: CoreCfg,
        seed_k: u64,
        now_ms: u64,
    ) -> Self {
        let CoreCfg {
            roam_cfg,
            interaction_cfg,
            catalog,
            emotion,
            needs,
            character_default_name,
            character_catchphrase,
            lines,
            audio,
            save,
            activities,
            shop,
            achievements,
        } = cfg;
        let platform_graph = Arc::new(RwLock::new(PlatformGraph::new(monitors.clone(), now_ms)));
        // S7-M3 前置首件（B12-① / P2-16）：把平台图注入决策层站立面——决策层据此
        // 感知标题栏平台（「走上标题栏站立」在决策层生效）；无标题栏时图内仅桌底节点，
        // 等价既有 `DesktopFloor` 行为（R2 降级口径不变）。
        let motion = MotionEngine::new_with_surface(
            pos,
            monitors.clone(),
            roam_cfg.clone(),
            seed_k,
            now_ms,
            Box::new(SharedSurface(Arc::clone(&platform_graph))),
        );
        let clamped = motion.pos();
        // 固化堆地址为 `'static`（装配期一次；见本方法文档的泄漏口径说明）。
        let cfg_ref: &'static EmotionConfig = Box::leak(Box::new(emotion));
        let needs_ref: &'static NeedsConfig = Box::leak(Box::new(needs));
        // S5-M2：有**可用锚点**的存档 → 以存档恢复内核（数值 / P / 敏感度 / 展示态 /
        // 上次 tick 时刻），**在首个 tick 之前**完成，否则离线补偿的起点会被首拍压缩
        // （见 `EmotionEngine::restore` 文档）。
        // 无锚点（`lastTickMs == 0`：全新安装，或上次运行在首个业务档落盘前就退出）→
        // 走 `new`（默认值 + 首拍建立基线），不恢复：此时「距 1970 纪元」不是离线时长。
        let mut emotion = match save.as_ref().filter(|store| store.has_session_anchor()) {
            Some(store) => {
                let cached = store.cache();
                let engine = EmotionEngine::restore(
                    cfg_ref,
                    needs_ref,
                    cached.to_pet_state(),
                    cached.emotion.neglect,
                    cached.emotion.sensitivity,
                    cached.meta.last_tick_ms,
                );
                // S7-M4：段 C 因子侧（性格 / 首建标记 / 自适应 / 粗暴）随存档恢复。
                // **必须在首个 tick 之前**：否则首拍会按「未随过」重新随机性格。
                engine
            }
            None => EmotionEngine::new(cfg_ref, needs_ref),
        };
        if let Some(store) = save.as_ref().filter(|s| s.has_session_anchor()) {
            emotion.restore_factors(&store.cache().emotion);
        }
        // S7-M6：FR-11-12 三层防护的**数值口径**来自 `settings.json.interaction`
        // （内核不持有配置引用，避免生命周期纠缠；只搬运 Copy 的策略结构）。
        emotion.set_interaction_policy(interaction_policy_of(&interaction_cfg));
        // S4-M5：气泡计划器的冷却 / 间隔参数全部取自 `lines.json.selector`（C7）。
        let mut bubble = BubblePlanner::from_library(&lines);
        // S7-M8：装配口头禅改写（token / 权重 / 禁用池取自 `character.json.catchphrase`；
        // `None` = 未装配，退回 S4-M5 基线）。
        if let Some(cp_cfg) = &character_catchphrase {
            bubble = bubble.with_catchphrase(cp_cfg);
        }
        // S4-M5：`{name}` 变量取配置默认名（C2：代码内零角色名字面量）。
        let vars = PlaceholderVars::new().with_name(character_default_name);
        // S8-M1：活动运行时（`02 §5 K-7` D 段 `activity` 恢复进行中实例；无档 /
        // 无实例 / 段解析失败 → 空运行 Idle，不崩不伪造）。
        let mut activity_runtime = ActivityRuntime::new(activities.clone());
        if let Some(store) = save.as_ref().filter(|s| s.has_session_anchor()) {
            match serde_json::from_value::<ActivitySave>(store.cache().activity.clone()) {
                Ok(saved) => {
                    activity_runtime =
                        ActivityRuntime::restore(activities, saved.instance, saved.phase);
                }
                Err(err) => eprintln!(
                    "[dp-app] core-loop 活动存档段解析失败（降级空运行）：{err}"
                ),
            }
        }
        // S8-M5/M6：经济运行时。有存档 → 从 save.economy/save.inventory 还原；否则全新。
        let economy = match save.as_ref().filter(|st| st.has_session_anchor()) {
            Some(st) => {
                let cache = st.cache();
                crate::economy_rt::EconomyRuntime::restore(
                    shop,
                    achievements,
                    &cache.economy,
                    &cache.inventory,
                )
            }
            None => crate::economy_rt::EconomyRuntime::new(shop, achievements),
        };

        Self {
            pos: clamped,
            phase: Phase::Roam,
            motion,
            physics: None,
            platform_graph,
            arbiter: ActionArbiter::new(),
            catalog,
            seed_k,
            monitors,
            pending_inputs: PlatformInputs::default(),
            last_logic_ms: now_ms,
            roam_cfg,
            interaction_cfg,
            cursor: None,
            drag_cursor: None,
            thrown_flight: false,
            throw_landed_count: 0,
            playback: None,
            emotion,
            session: SessionWatcher::default(),
            presence_idle_ms: None,
            pending_mood_delta: 0.0,
            runaway_away: false,
            lines,
            bubble,
            vars,
            audio,
            save,
            save_requested: false,
            startup_greeting: None,
            // S5-M4：初值 = 出厂默认（与 `settings.json` / `schedule.json` 同值域）；
            // 装配期由 [`Self::apply_settings_snapshot`] 用真实有效值覆盖。
            dnd: false,
            dnd_pause_bubbles: true,
            activity_sensing: true,
            click_through: false,
            tray_state: None,
            tray_left_home: false,
            auto_roam: true,
            activity_runtime,
            wall: Arc::new(SystemWallClock),
            pending_recall_kind: None,
            economy,
            activity: ActivitySample::default(),
            reminders: ReminderScheduler::new(
                ReminderConfig::new(true, 45, true, 45, 15, 180, true),
                now_ms as i64,
            ),
            needs_trigger: NeedsActionTrigger::new(),
            reminder_ack_resets: true,
        }
    }

    /// 把设置快照落到**内核侧**状态（S5-M4：装配期一次 + 每次热更新补丁后一次）。
    ///
    /// 只处理与内核 / 引擎有关的部分；窗口、音频总线、注册表等**应用层**效果在
    /// `commands::apply_platform_effects` 同步落地（见该函数文档的时序说明）。
    pub fn apply_settings_snapshot(&mut self, snapshot: &bridge::SettingsSnapshot) {
        self.emotion.set_sensitivity_value(snapshot.sensitivity_value);
        self.emotion.set_easy_coax_mode(snapshot.easy_coax_mode);
        self.dnd = snapshot.do_not_disturb;
        self.click_through = snapshot.click_through;
        self.activity_sensing = snapshot.activity_sensing;
        // S7-M6：交互策略随设置重算（层 ① 的不可达因子 / 层 ③ 的两条阈值与兜底等级）。
        self.emotion
            .set_interaction_policy(interaction_policy_of(&self.interaction_cfg));
        self.auto_roam = snapshot.auto_roam;
        self.interaction_cfg.click_feedback.enabled = snapshot.click_feedback_enabled;
        // 节奏：写进「换相重建用」的配置副本。**口径说明**：`MotionEngine` 在构造时持有
        // 自己的 `roam_cfg` 快照，故节奏变更作用于**下一次重建**（显示变更 / 换相），
        // 本卡刻意不打断进行中的漫游（中止行走比晚一拍生效更糟）。
        self.roam_cfg.pace = snapshot.roam_pace;
        if !snapshot.name.is_empty() {
            self.vars = self.vars.clone().with_name(snapshot.name.clone());
        }
        // S7-M8：口头禅开关与频率档位 → 内核改写闸门（装配期 / 热更新均走这里；
        // AC-27：设 0 后不出现；档位字符串与存档 / 配置同真源，L-03）。
        self.bubble.set_catchphrase_enabled(snapshot.catchphrase_enabled);
        self.bubble.set_catchphrase_frequency(
            dp_core::emotion::lines::CatchphraseFrequency::from_cfg_name(
                &snapshot.catchphrase_frequency,
            ),
        );
    }

    /// 落地 `schedule.json` 的勿扰行为位（S5-M4 装配期一次；未做热更新——该文件是
    /// 出厂默认，用户改动落在存档 `settings.behavior.doNotDisturb`）。
    pub fn apply_schedule_flags(&mut self, pause_bubbles: bool) {
        self.dnd_pause_bubbles = pause_bubbles;
    }

    // -----------------------------------------------------------------------
    // S7-M6：交互可达性与托盘替代入口（FR-11-12 三层防护）
    // -----------------------------------------------------------------------

    /// 交互是否可达（FR-11-12 层 ① 的**唯一判定点**）。
    ///
    /// 三个来源任一为真即不可达：
    ///   1. `click_through`（穿透）：`WH_MOUSE_LL` + `WH_KEYBOARD_LL` 双双卸载；
    ///   2. `dnd`（勿扰）：鼠标钩子卸载（`01 FR-10-4`）；
    ///   3. 已离家出走（`runaway_away`）：宠物窗口本身隐藏，桌面无目标可点。
    ///
    /// **已知缺口（登记 §3.3）**：钩子因完整性级别 / `LowLevelHooksTimeout` 安装失败时的
    /// 「静默不可达」窗口**不在本判定覆盖范围**（缺该失败信号的上报通路），
    /// 故本判定只覆盖「配置层面可判定的三源」。
    #[must_use]
    pub fn interaction_available(&self) -> bool {
        !self.click_through && !self.dnd && !self.runaway_away
    }

    /// 托盘图标态（S7-M6）：离家 > 不可交互 > 普通（优先级从高到低）。
    #[must_use]
    pub fn tray_icon_state(&self) -> TrayIconState {
        if self.runaway_away {
            TrayIconState::LeftHome
        } else if !self.interaction_available() {
            TrayIconState::NotInteractive
        } else {
            TrayIconState::Normal
        }
    }

    /// 同步托盘图标 / tooltip / 菜单（S7-M6；**仅变化时**执行，避免每拍重建菜单）。
    ///
    /// 两个变化源：
    ///   - 图标态（普通 / 离家灰化 / 不可交互）；
    ///   - 「离家」菜单位（决定是否追加「把{name}找回来」）。
    ///
    /// 纯逻辑模式（`app = None`）与未装托盘（`tray_by_id` 取不到）均静默降级。
    pub fn sync_tray(&mut self, app: Option<&AppHandle>) {
        let Some(handle) = app else { return };
        let icon = self.tray_icon_state();
        let icon_changed = self.tray_state != Some(icon);
        let menu_changed = self.tray_left_home != self.runaway_away;
        if !icon_changed && !menu_changed {
            return;
        }
        if let Err(err) = crate::tray_menu::sync_state(handle, icon, self.runaway_away) {
            eprintln!("[dp-app] 托盘状态同步失败（降级继续）：{err}");
        }
        self.tray_state = Some(icon);
        self.tray_left_home = self.runaway_away;
    }

    /// 消费应用层投递的设置补丁（S5-M4 业务档每拍调用一次；无待消费 → 立即返回）。
    ///
    /// 职责分工：**应用层**（`commands::settings_apply`）负责快照合并与窗口 / 音频 / 注册表；
    /// **本函数**负责内核侧落地 + **存档 B 段写入**（存档唯一写者在 core-loop）+ `pet://config`
    /// 摘要广播。这样既保住单写者语义，又让「10s 内生效」由 1Hz 业务档保证（远小于 10s）。
    fn apply_pending_settings(&mut self, app: Option<&AppHandle>, now_mono_ms: u64) {
        let Some(handle) = app else { return };
        let Some(state) = handle.try_state::<bridge::SettingsState>() else {
            return;
        };
        let Some(pending) = state.take_pending() else {
            return;
        };
        let patch = pending.patch.clone();
        // 内核侧：用「应用层已合并后的快照」整体覆盖（避免逐字段重复实现合并语义）。
        let sensing_before = self.activity_sensing;
        self.apply_settings_snapshot(&state.snapshot());
        // S7-M1：隐私一键关闭 → 卸载键盘钩子（`02 §5.6` ④「关闭后退化为纯时间模型」）。
        // 按差异下发：`KeyHookService` 内部虽幂等，但差异判定可省一次锁与后端查询。
        if self.activity_sensing != sensing_before {
            if let Some(pet) = handle.try_state::<PetPlatform>() {
                pet.keyhook.set_activity_sensing(self.activity_sensing);
            }
            eprintln!(
                "[dp-app] core-loop 活动感知开关变更：activitySensing={}（键盘钩子随之装/卸）",
                self.activity_sensing
            );
        }
        // S7-M2：提醒偏好 → 调度器（幂等：配置未变则不重排 deadline）。
        self.sync_reminders(&state.snapshot(), now_mono_ms as i64);
        // 托盘文案里的角色名同步（C2：名字经 `{name}` 模板渲染，不硬编码）。
        crate::tray_menu::set_pet_name(handle, &state.snapshot().name);
        // 存档 B 段（pet / settings）。
        let persisted = self.persist_settings_patch(&patch);
        if persisted {
            self.save_requested = true;
        }
        // C8：事件名与载荷生产在 `dp-core::event`（本层只 emit）。
        let wire = dp_core::event::wire_for_config(pending.revision, &patch.groups(), persisted);
        if let Err(err) = handle.emit(wire.event, &wire.payload) {
            eprintln!("[dp-app] core-loop 广播 {} 降级：{err}", wire.event);
        }
        // 落盘探针（`now_mono_ms` 仅用于日志对齐，不参与判定）。
        eprintln!(
            "[dp-app] core-loop 设置热更新：revision={} 分组={:?} 已落盘={persisted}（t={now_mono_ms}ms）",
            pending.revision,
            patch.groups()
        );
    }

    /// 把设置补丁写进存档 B 段；返回「存档是否可写」（`false` = 未装配 / v1 待迁移保护态）。
    fn persist_settings_patch(&mut self, patch: &bridge::SettingsPatch) -> bool {
        let Some(save) = self.save.as_mut() else {
            return false;
        };
        let writable = save.is_writable();
        if !writable {
            return false;
        }
        let cache = save.cache_mut();
        if let Some(name) = &patch.name {
            cache.pet.name = name.clone();
        }
        if let Some(value) = patch.catchphrase_enabled {
            cache.pet.catchphrase.enabled = value;
        }
        if let Some(value) = &patch.catchphrase_frequency {
            if let Ok(freq) = serde_json::from_value::<dp_core::emotion::lines::CatchphraseFrequency>(
                serde_json::Value::String(value.clone()),
            ) {
                cache.pet.catchphrase.frequency = freq;
            }
        }
        let settings = &mut cache.settings;
        if let Some(value) = patch.scale_percent {
            settings.appearance.scale_percent = value;
        }
        if let Some(value) = patch.opacity_percent {
            settings.appearance.opacity_percent = value;
        }
        if let Some(value) = &patch.language {
            settings.appearance.language = value.clone();
        }
        if let Some(value) = patch.master_volume_percent {
            settings.audio.master_volume_percent = value;
        }
        if let Some(value) = patch.muted {
            settings.audio.muted = value;
        }
        if let Some(value) = patch.auto_roam {
            settings.behavior.auto_roam = value;
        }
        if let Some(value) = patch.roam_pace {
            settings.behavior.roam_pace = value;
        }
        if let Some(value) = patch.do_not_disturb {
            settings.behavior.do_not_disturb = value;
        }
        if let Some(value) = patch.easy_coax_mode {
            settings.behavior.easy_coax_mode = value;
        }
        if let Some(value) = patch.click_through {
            settings.behavior.click_through = value;
        }
        if let Some(value) = &patch.always_on_top_policy {
            settings.behavior.always_on_top_policy = value.clone();
        }
        if let Some(value) = patch.autostart {
            settings.behavior.autostart = value;
        }
        if let Some(value) = patch.sensitivity_value {
            settings.emotion_sensitivity = value;
        }
        if let Some(value) = patch.activity_sensing {
            settings.privacy.activity_sensing = value;
        }
        if let Some(reminders) = patch.reminders {
            if let Some(value) = reminders.sedentary_enabled {
                settings.reminders.sedentary_enabled = value;
            }
            if let Some(value) = reminders.sedentary_interval_min {
                settings.reminders.sedentary_interval_min = value;
            }
            if let Some(value) = reminders.water_enabled {
                settings.reminders.water_enabled = value;
            }
            if let Some(value) = reminders.water_interval_min {
                settings.reminders.water_interval_min = value;
            }
        }
        true
    }

    /// 接入播放指令通道（S4 前清障 B15-④；装配点 `spawn` 在起线程前调用一次）。
    pub fn attach_playback(&mut self, channel: PlaybackChannel) {
        self.playback = Some(channel);
    }

    /// 只读：存档门面（S5-M4 装配期由 `build_state` 用它推导设置快照；运行期不暴露可变引用）。
    #[inline]
    #[must_use]
    pub fn save_ref(&self) -> Option<&SaveStore> {
        self.save.as_ref()
    }

    // -----------------------------------------------------------------------
    // S5-M2：离线补偿接入 + 回归问候（T-14 段 · 下）
    // -----------------------------------------------------------------------

    /// 只读：存档门面（诊断 / 单测）。
    #[inline]
    #[must_use]
    pub fn save(&self) -> Option<&SaveStore> {
        self.save.as_ref()
    }

    /// 离线补偿（**S5-M2 要点 2**：`02 §5.5` / §6.1 启动时序）。
    ///
    /// 调用时机（硬约束）：`build_state` 内、core-loop 线程启动**之前**——因为
    /// [`EmotionEngine::restore`] 已把 `last_tick_ms` 恢复到上次落盘时刻，若先跑首拍
    /// 再补偿，离线时长会被压缩（见 `restore` 文档）。
    ///
    /// 语义：
    ///   - 离线时长取 [`SaveStore::away_ms`]（墙钟回拨钳 0）；
    ///   - **无存档 / 无锚点（`lastTickMs == 0`）/ 无离线（`away_ms == 0`）→ `None`**：
    ///     全新安装不做任何补偿与演出（否则「距 1970 纪元」会被当成 5 万小时离线）；
    ///   - 离线环境：不在场（`preset_idle_ms` 置饱和值）、不暂停、不演出、无交互增益——
    ///     与 `02 §5.5` 的分块推进口径一致，速度上界可复算成 `P = 0.05 × 分钟数`；
    ///   - 产出[回归问候](StartupGreeting)：`ACT-T-01` + 档位台词池文案，
    ///     在首个业务档投递（`02 §6.1`「时段问候」）。
    pub fn compensate_offline(&mut self, wall: &dyn WallClock) -> Option<OfflineOutcome> {
        let now_ms = wall.now_ms();
        let store = self.save.as_ref()?;
        if !store.has_session_anchor() {
            return None;
        }
        let away_ms = store.away_ms(now_ms);
        if away_ms <= 0 {
            return None;
        }
        // 离线环境快照（字段**全量显式**给出：不落 `..TickEnv::default()`，
        // 避免 `default()` 内部多读一次墙钟；C3）。
        let env = TickEnv {
            now_local: wall.now_local(),
            session_paused: false,
            performing: false,
            activity_running: false,
            event_delta: 0.0,
            preset_idle_ms: u64::MAX,
            // S7-M6：离线期间的可达性沿用**离线前**的运行时判定（离线不改变穿透 / 勿扰 /
            // 离家三源，故不得在此凭空改写；S7-M4 的离线因子口径只取 presence 一项）。
            interaction_available: self.interaction_available(),
            satiety: self.emotion.state.values.satiety,
            cleanliness: self.emotion.state.values.cleanliness,
            // S7-M4：离线期间无感知样本（忙碌档退化轻度，`02 §5.5` 的算例前置）。
            activity: None,
            negative_happened: false,
            _marker: core::marker::PhantomData,
        };
        let outcome = self.emotion.offline_compensate(away_ms, env);
        let level = self.emotion.neglect.level;
        eprintln!(
            "[dp-app] core-loop 离线补偿：away={away_ms}ms（{:.1}h）→ {outcome:?}（L{level}，P={:.2}）",
            away_ms as f64 / 3_600_000.0,
            self.emotion.neglect.p
        );
        // 回归问候：`ACT-T-01` 挥手 + 档位台词池文案（无台词库 / 冷却未过 → 只有动作）。
        // 取词口径与 S4-M5 的阶段迁移一致（池键来自 `emotion.json.levels[level].linePool`）。
        let bubble = self.bubble.bubble_for_level(
            &self.lines,
            self.emotion.cfg().levels.as_slice(),
            level,
            &self.vars,
            now_ms,
        );
        self.startup_greeting = Some(StartupGreeting { action_id: STARTUP_GREETING_ACTION, bubble });
        Some(outcome)
    }

    /// 投递回归问候（首个业务档；`app` 为 `None` 时保留待投，不消费）。
    fn flush_startup_greeting(&mut self, app: Option<&AppHandle>, now_ms: u64) {
        let Some(handle) = app else { return };
        let Some(greeting) = self.startup_greeting.take() else {
            return;
        };
        match self.catalog.find(greeting.action_id) {
            Some(cfg) => match ActionRequest::from_cfg(cfg, ActionSource::Emotion) {
                Some(request) => {
                    let verdict = self.arbiter.submit(request, now_ms);
                    self.settle_play(verdict.clone(), greeting.action_id);
                    eprintln!(
                        "[dp-app] core-loop 回归问候动作 {} 仲裁={verdict:?}",
                        greeting.action_id
                    );
                }
                None => eprintln!(
                    "[dp-app] core-loop 回归问候动作 {} 不可用（disabled），降级不提交",
                    greeting.action_id
                ),
            },
            None => eprintln!(
                "[dp-app] core-loop 回归问候动作 {} 不在目录，降级不提交",
                greeting.action_id
            ),
        }
        if let Some(plan) = greeting.bubble {
            let wire = wire_for_bubble(&plan);
            if let Err(err) = handle.emit(wire.event, &wire.payload) {
                eprintln!("[dp-app] core-loop 广播 {} 降级：{err}", wire.event);
            }
        }
    }

    /// 存档落盘档（**S5-M2 要点 1 / S5-M1 要点 1**；业务档 1Hz 调用）。
    ///
    /// 流程：① 把内核当前状态刷进存档「段 A」（返回是否语义变化）；
    /// ② 若本 tick 收到过 `PersistNow` → **强制落盘**（跳过定时与合并窗口）；
    /// ③ 否则按 [`SaveStore::due`] 落盘（30s 定时 或 变更后 2s 合并窗口）。
    ///
    /// `now_mono_ms` 为单调毫秒（间隔语义，C3 允许）；`now_ms` 为墙钟毫秒（时刻语义）。
    pub fn save_tick(&mut self, now_mono_ms: u64, now_ms: i64) {
        let Self { save, emotion, save_requested, .. } = self;
        let Some(store) = save.as_mut() else {
            return;
        };
        store.capture_from(emotion, now_ms);
        // S8-M1：D 段 `activity` 随每拍落盘——进行中实例 / 阶段可跨进程恢复
        // （`02 §5 K-7`；`ActivitySave` ↔ `serde_json::Value`，dp-core 侧保持 Value 冻结）。
        let activity_save = ActivitySave {
            phase: self.activity_runtime.phase(),
            instance: self.activity_runtime.current().cloned(),
        };
        store.cache_mut().activity =
            serde_json::to_value(activity_save).unwrap_or(serde_json::Value::Null);
        // S8-M5/M6：经济 D 段随每拍落盘。
        store.cache_mut().economy = self.economy.to_save_economy();
        store.cache_mut().inventory = self.economy.to_save_inventory();
        let force = core::mem::take(save_requested);
        // 无论成功失败都清位：失败靠下一拍重试，绝不让一个坏盘位把后续落盘全堵死。
        let result = if force { store.flush_force(now_mono_ms) } else { store.flush(now_mono_ms) };
        if let Err(err) = result {
            eprintln!("[dp-app] core-loop 存档落盘降级（force={force}）：{err}");
        }
    }

    /// 回归问候探针（`#[cfg(test)]`：验证「离线补偿 → 档位台词池 → 渲染后文案」链路，
    /// 不必构造 `AppHandle` 触发 emit）。
    #[cfg(test)]
    pub(crate) fn startup_greeting_for_test(&self) -> Option<(&'static str, Option<String>)> {
        self.startup_greeting
            .as_ref()
            .map(|g| (g.action_id, g.bubble.as_ref().map(|b| b.text.clone())))
    }

    /// 记一次交互的情绪净增益（S4-M2：`Mood` 加项通路，跨档暂存）。
    ///
    /// 由 logic 档（20Hz）在识别到正向交互意图后调用，累加到
    /// [`Self::pending_mood_delta`]，业务档（1Hz）下一次 tick 经
    /// [`TickEnv::event_delta`] 一次性喂入内核。
    ///
    /// **数值口径归 S4-M3**（`emotion.json.relief.*` 的 P 缓解量与冷却）：本卡只提供
    /// 「交互 → 暂存 → 单次消费」的通路与幂等语义，**不内置任何增益数值常量**，
    /// 由调用方按 S4-M3 的缓解表给值（避免本层与配置真源双写）。
    ///
    /// 暂存语义：跨档不丢事件（logic 20Hz / biz 1Hz 频率差）；消费幂等（喂入即清零）；
    /// 暂停期间不清零（内核早退不消费，恢复后一次计入）。
    pub fn record_interaction_mood(&mut self, delta: f32) {
        if delta.is_finite() {
            self.pending_mood_delta += delta;
        }
    }

    /// 当前待喂入的情绪增益（诊断 / 单测用）。
    #[inline]
    pub fn pending_mood_delta(&self) -> f32 {
        self.pending_mood_delta
    }

    // -----------------------------------------------------------------------
    // S4-M3 / S4-M4：交互缓解 + 道歉三部曲 + 离家可见性 + 入站指令
    // -----------------------------------------------------------------------

    /// 交互意图结算（S4-M3：缓解表 `relief.*` + 道歉三部曲推进）。
    ///
    /// 由 logic 档（20Hz）对每个手势意图调用一次；返回内核算法事件（`pet://coax` /
    /// `pet://emotion` 的载荷来源），由调用方经 [`Self::dispatch_emotion_events`] 落地。
    pub fn on_interaction_intent(&mut self, kind: InteractionKind, now_ms: u64) -> Vec<EmotionEvent> {
        self.emotion.on_interaction_kind(kind, now_ms as i64)
    }

    /// 道歉三部曲 20Hz 推进（S4-M3：连续抚摸累计 / 比心窗超时 / 离家演出计时）。
    ///
    /// `stroke_active` 取 [`InteractionConsumer::is_stroking`]——`Stroke` 意图只在松手
    /// 结算一次，无法表达持续时间，故需每档读手势机的抚摸态。
    pub fn coax_stroke_tick(&mut self, now_ms: u64, stroke_active: bool) -> Vec<EmotionEvent> {
        self.emotion.coax_stroke_tick(now_ms as i64, stroke_active)
    }

    /// 入站指令落地（S4-M4：设置页「重置情绪」/ 托盘「把心月狐找回来」）。
    pub fn apply_core_input(&mut self, input: CoreInput, now_ms: u64) -> Vec<EmotionEvent> {
        match input {
            CoreInput::ResetEmotion => {
                let from = self.emotion.neglect.level;
                eprintln!("[dp-app] core-loop 收到「重置情绪」：L{from} → L0（`force_lower` 兜底）");
                self.emotion.force_lower(now_ms as i64)
            }
            CoreInput::RecallRunaway => {
                eprintln!("[dp-app] core-loop 收到「把心月狐找回来」：L5 走回，仍需完成三部曲");
                self.emotion.coax_recall(now_ms as i64)
            }
            // S7-M6（FR-11-12 层 ②）：托盘替代入口。**仍走完整三部曲**（产品底线不动）。
            CoreInput::TrayCoax => {
                let stage = self.emotion.coax_step();
                eprintln!("[dp-app] core-loop 收到托盘「摸摸」输入：coax 阶段={stage:?}");
                self.emotion.coax_tray_tap(now_ms as i64)
            }
            CoreInput::TrayFeed => {
                eprintln!("[dp-app] core-loop 收到托盘「喂食」：等效一次喂食");
                let events = self.emotion.tray_feed(now_ms as i64);
                // S7-M9：喂食事务端点动作（AC-22 触发面；未启用自动跳过）。
                self.settle_feed_transaction(now_ms);
                events
            }
            CoreInput::TrayBath => {
                eprintln!("[dp-app] core-loop 收到托盘「洗澡」：等效一次洗澡");
                let events = self.emotion.tray_bath(now_ms as i64);
                // S7-M9：洗澡事务端点动作（AC-22 触发面；未启用自动跳过）。
                self.settle_bath_transaction(now_ms);
                events
            }
            // S5-M4：重置数据 / 导入存档 / 退出需要 `AppHandle`（重启进程 / 强制落盘后退出），
            // 由 [`Self::handle_admin_input`] 在 drain 处先行消化；走到这里说明调用方
            // 未走该出口（属实现错误），按「不产生情绪事件」处理并记日志。
            CoreInput::ResetAllData | CoreInput::ImportSave { .. } | CoreInput::Shutdown => {
                eprintln!("[dp-app] core-loop 收到特权指令但未走 handle_admin_input 出口，已忽略");
                Vec::new()
            }
            // S6-M2 自愈：阻塞落盘请求（supervisor 渲染看门狗触发）。
            // 立即 `flush_force`（跳过 30s 定时与 2s 合并窗口），完成后回执——
            // 保证「重建渲染前存档已落盘」，渲染异常时**不丢档**（`02 §5 K-8`）。
            CoreInput::FlushSave { ack } => {
                let result = match self.save.as_mut() {
                    Some(save) => match save.flush_force(now_ms) {
                        Ok(true) => "已强制落盘".to_string(),
                        Ok(false) => "落盘被跳过（禁写盘态）".to_string(),
                        Err(err) => format!("落盘失败（降级继续自愈）：{err}"),
                    },
                    None => "存档未装配（无档可落）".to_string(),
                };
                eprintln!("[dp-app] core-loop 收到「阻塞落盘」（S6-M2 自愈）：{result}");
                // 无论落盘结果如何都回执（supervisor 的 3s 超时只防死等，不阻断自愈）。
                let _ = ack.send(());
                Vec::new()
            }
            // S8-M1：派遣外出活动（`02 §5.13`；前置校验在 core-loop 以当时内核快照执行，
            // 成功 → Preparing + 出发演出 + 前置消耗；拒绝只记日志，快照不产活动）。
            CoreInput::ActivityDispatch { kind, def_id, duration_min } => {
                let result = self.handle_activity_dispatch(&kind, &def_id, duration_min, now_ms);
                match &result {
                    Ok(inst) => eprintln!(
                        "[dp-app] core-loop 活动派遣成功：{}（{}，{}min）",
                        inst.def_id,
                        inst.kind.label(),
                        inst.planned_ms / 60_000
                    ),
                    Err(err) => eprintln!("[dp-app] core-loop 活动派遣被拒：{err}"),
                }
                Vec::new()
            }
            // S8-M6：商城购买（事务：余额 / 限购 / 失败冲正；结果经 economy 段回传前端）。
            CoreInput::Purchase { item_id, qty } => {
                let at_ms = now_ms as i64;
                let day_key = Self::local_day_key(at_ms);
                let week_key = Self::local_week_key(at_ms);
                let affinity_level = self.emotion.state.values.affinity_level;
                match self.economy.buy(&item_id, qty, affinity_level, &day_key, &week_key, at_ms) {
                    Ok(out) => {
                        eprintln!(
                            "[dp-app] 购买成功：{}×{} 花 {}，余额 {}",
                            item_id, out.qty, out.spent, out.balance_after
                        );
                        self.request_save();
                    }
                    Err(err) => eprintln!("[dp-app] 购买被拒：{err}"),
                }
                Vec::new()
            }
            // S10-M1：桌面装饰摆放（槽位 0..=4；背包须拥有该摆件；写 save.decor[slot]）。
            CoreInput::DecorPlace { slot, item_id } => {
                let slot = slot as usize;
                let slots = dp_core::save::schema::DECOR_SLOTS;
                if slot >= slots {
                    eprintln!("[dp-app] 装饰摆放被拒：槽位 {slot} 越界（{slots} 槽）");
                    return Vec::new();
                }
                if self.economy.inventory_count(&item_id) == 0 {
                    eprintln!("[dp-app] 装饰摆放被拒：背包无摆件 {item_id}");
                    return Vec::new();
                }
                if let Some(save) = self.save.as_mut() {
                    let decor = save
                        .cache_mut()
                        .decor
                        .as_array_mut()
                        .expect("save.decor 恒为数组");
                    if decor.len() < slots {
                        decor.resize(slots, serde_json::Value::Null);
                    }
                    decor[slot] = serde_json::Value::String(item_id.clone());
                    self.request_save();
                    eprintln!("[dp-app] 装饰已摆放：槽 {slot} ← {item_id}");
                }
                Vec::new()
            }
            // S10-M1：取下某槽装饰（写 save.decor[slot] = null）。
            CoreInput::DecorRemove { slot } => {
                let slot = slot as usize;
                let slots = dp_core::save::schema::DECOR_SLOTS;
                if slot >= slots {
                    eprintln!("[dp-app] 装饰取下被拒：槽位 {slot} 越界");
                    return Vec::new();
                }
                if let Some(save) = self.save.as_mut() {
                    let decor = save
                        .cache_mut()
                        .decor
                        .as_array_mut()
                        .expect("save.decor 恒为数组");
                    if let Some(cell) = decor.get_mut(slot) {
                        *cell = serde_json::Value::Null;
                    }
                    self.request_save();
                    eprintln!("[dp-app] 装饰已取下：槽 {slot}");
                }
                Vec::new()
            }
            // S8-M1：提前召回（Running → Returning；收益按已完成比例 ×0.5 + P+6）。
            CoreInput::ActivityRecall => {
                match self.activity_runtime.recall() {
                    Ok(()) => {
                        eprintln!("[dp-app] core-loop 活动召回已受理（回归演出后结算）");
                        self.pending_recall_kind = Some(RecallKind::Early);
                        self.request_save();
                    }
                    Err(err) => eprintln!("[dp-app] core-loop 活动召回被拒：{err}"),
                }
                Vec::new()
            }
        }
    }

    /// S5-M4：需要 `AppHandle` 的入站指令（重置全部数据 / 导入存档 / 退出）。
    ///
    /// 返回 `true` = 本函数已消化该指令（调用方**不再**把它交给 `apply_core_input`）。
    ///
    /// ## 为什么必须在 core-loop 里做
    /// `SaveStore` 的唯一写者是 core-loop 业务档（`save::store` 模块文档），命令层直写会
    /// 破坏单写者语义。故三条指令都只**投递**进来，由本函数在 core-loop 线程内：
    ///   1. 重置：`SaveStore::reset_to_default`（内存档替换 + **立即落盘**）→ `app.restart()`；
    ///   2. 导入：白名单校验后的备份路径（命令层已校验一次）→ `SaveStore::import_from` → 重启；
    ///   3. 退出：`flush_force`（退出前存档确认，`01 FR-1-10`）→ `app.exit(0)`。
    ///
    /// ## 重启而非「原地重建」
    /// B/C/D 段（性格 / 需求 / 经济 / 背包 / 成就）由后续里程碑接管，本卡无法逐段实现
    /// 「运行时重置」；`app.restart()` 让**所有段**（含未来新增段）都从出厂态启动，
    /// 是唯一不会随里程碑推进而腐化的实现（`01 FR-8-4`「清档重建」）。
    pub fn handle_admin_input(
        &mut self,
        app: &AppHandle,
        input: &CoreInput,
        now_mono_ms: u64,
    ) -> bool {
        match input {
            // S7-M6：三个托盘替代入口不需要 `AppHandle`（只动内核状态）→ 交回
            // `apply_core_input` 落地，与「重置情绪 / 找回」同一出口。
            CoreInput::ResetEmotion
            | CoreInput::RecallRunaway
            | CoreInput::TrayCoax
            | CoreInput::TrayFeed
            | CoreInput::TrayBath
            | CoreInput::FlushSave { .. }
            // S8-M1：活动命令不需要 AppHandle（只动运行时 + 内核）→ 交回纯逻辑出口。
            | CoreInput::ActivityDispatch { .. }
            | CoreInput::ActivityRecall
            // S8-M6：购买不需要 AppHandle → 交回纯逻辑出口。
            // S10-M1：装饰摆放/取下只动存档缓存 → 交回纯逻辑出口。
            | CoreInput::Purchase { .. }
            | CoreInput::DecorPlace { .. }
            | CoreInput::DecorRemove { .. } => false,
            CoreInput::ResetAllData => {
                match self.save.as_mut() {
                    Some(save) => match save.reset_to_default(now_mono_ms) {
                        Ok(_) => {
                            eprintln!(
                                "[dp-app] core-loop 重置全部数据：默认档已落盘，重启进程（{}）",
                                save.save_path().display()
                            );
                            app.restart();
                        }
                        Err(err) => {
                            eprintln!("[dp-app] core-loop 重置全部数据失败（降级不重启）：{err}");
                        }
                    },
                    None => eprintln!("[dp-app] core-loop 重置全部数据：存档未装配，忽略"),
                }
                true
            }
            CoreInput::ImportSave { file } => {
                let Some(dir) = resolve_save_dir(app) else {
                    eprintln!("[dp-app] core-loop 导入存档：存档目录不可用，忽略");
                    return true;
                };
                // 命令层已校验一次；此处**再校验一次**（防内部调用绕过命令层）。
                let path = match bridge::resolve_backup_path(&dir, file) {
                    Ok(path) => path,
                    Err(err) => {
                        eprintln!("[dp-app] core-loop 导入存档被拒绝：{err}");
                        return true;
                    }
                };
                match self.save.as_mut() {
                    Some(save) => match save.import_from(&path, now_mono_ms) {
                        Ok(()) => {
                            eprintln!("[dp-app] core-loop 已导入存档 {}，重启进程", path.display());
                            app.restart();
                        }
                        Err(err) => eprintln!("[dp-app] core-loop 导入存档失败（降级不重启）：{err}"),
                    },
                    None => eprintln!("[dp-app] core-loop 导入存档：存档未装配，忽略"),
                }
                true
            }
            CoreInput::Shutdown => {
                // 退出前存档确认（`01 FR-1-10`）：强制落盘（跳过 30s 定时与 2s 合并窗口）。
                if let Some(save) = self.save.as_mut() {
                    match save.flush_force(now_mono_ms) {
                        Ok(true) => eprintln!("[dp-app] core-loop 退出前已强制落盘（段 A）"),
                        Ok(false) => eprintln!("[dp-app] core-loop 退出前落盘被跳过（禁写盘态）"),
                        Err(err) => eprintln!("[dp-app] core-loop 退出前落盘失败（仍退出）：{err}"),
                    }
                }
                app.exit(0);
                true
            }
        }
    }

    /// 离家可见性同步（S4-M4）：`away` 翻转时隐藏 / 恢复宠物窗口。
    ///
    /// 复用 [`crate::tray_menu::set_pet_visible`]（与托盘 / 菜单 `hide` 同一写点），
    /// 保证 `PET_VISIBLE` 镜像一致。
    pub fn sync_runaway_visibility(&mut self, app: &AppHandle) {
        let away = self.emotion.is_runaway_away();
        if away == self.runaway_away {
            return;
        }
        self.runaway_away = away;
        if let Err(err) = crate::tray_menu::set_pet_visible(app, !away) {
            eprintln!("[dp-app] core-loop 离家可见性同步降级（away={away}）：{err}");
        }
    }

    /// 只读：是否已离家（S4-M4 诊断 / 单测）。
    #[inline]
    #[must_use]
    pub fn is_runaway_away(&self) -> bool {
        self.emotion.is_runaway_away()
    }

    /// 下发「起播」指令（覆盖态：播放器打断轮播优先播放该动作；无通道 → no-op）。
    fn emit_play(&self, action_id: &str) {
        if let Some(channel) = &self.playback {
            channel.push_order(PlaybackOrder::Play { action_id: action_id.to_string() });
        }
    }

    /// 下发「停播」指令（播放器清覆盖回落轮播；无通道 → no-op）。
    fn emit_stop(&self) {
        if let Some(channel) = &self.playback {
            channel.push_order(PlaybackOrder::Stop);
        }
    }

    /// 仲裁结果 → 发播指令（仅起播型结果 `Play` / `Interrupt` 需要覆盖态起播；
    /// 入队/丢弃/压制不下发——出队起播由 `Finished` 回报经 `drain_finished` 承载）。
    fn settle_play(&self, verdict: Arbitration, action_id: &str) {
        if matches!(verdict, Arbitration::Play | Arbitration::Interrupt { .. }) {
            self.emit_play(action_id);
        }
    }

    /// current 是否为循环动作（目录缺该动作 → 非循环口径，保守不停播）。
    fn current_is_looping(&self) -> bool {
        self.arbiter
            .current()
            .and_then(|a| self.catalog.find(&a.request.id))
            .is_some_and(|cfg| cfg.looping)
    }

    /// 停播在播的循环动作并按 `on_action_finished` 语义推进仲裁链（S4 前清障 B15-④）。
    ///
    /// current 为循环动作 → 发 `Stop` 停播（画面回落轮播）+ `on_action_finished`
    /// （弹出 current、队列最优者起播 fade 0）→ 起播者经 `emit_play` 重挂覆盖态；
    /// current 缺席 / 非循环 → no-op（非循环动作的播完由播放器 `Finished` 回报驱动）。
    fn stop_looping_current(&mut self, now_ms: u64) {
        if !self.current_is_looping() {
            return;
        }
        self.emit_stop();
        if let Some(started) = self.arbiter.on_action_finished(now_ms) {
            self.emit_play(&started.request.id);
        }
    }

    /// 排空播放回报（S4 前清障 B15-④：每 logic tick 调用；`Finished` 匹配 current
    /// → `on_action_finished` 链尾推进 → 出队者 `emit_play` 起播；不匹配 → 丢弃）。
    pub fn drain_finished(&mut self, now_ms: u64) {
        let Some(channel) = &self.playback else {
            return;
        };
        for report in channel.drain_reports() {
            let PlaybackReport::Finished { action_id } = report;
            let matches = self.arbiter.current().is_some_and(|a| a.request.id == action_id);
            if !matches {
                eprintln!("[dp-app] core-loop 播放回报与 current 不符，丢弃：{action_id}");
                continue;
            }
            if let Some(started) = self.arbiter.on_action_finished(now_ms) {
                eprintln!(
                    "[dp-app] core-loop 链尾推进：{} 播毕 → 出队起播 {}（fade={}ms）",
                    action_id, started.request.id, started.fade_ms
                );
                self.emit_play(&started.request.id);
            }
        }
    }

    /// 当前相（调试 / 单测视图）。
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// 手势参数快照（S3-M3 消费端构造用；随 `interaction_cfg` 在启动期固化，
    /// 运行期不热更——配置热更归后续里程碑）。
    #[must_use]
    pub fn gesture_cfg(&self) -> GestureCfg {
        self.interaction_cfg.gesture.clone()
    }

    /// 单击微反馈配置快照（S3-M6 触发映射用；同启动期固化口径）。
    #[must_use]
    pub fn click_feedback_cfg(&self) -> ClickFeedbackCfg {
        self.interaction_cfg.click_feedback.clone()
    }

    /// 当前权威位置（VDC）。
    #[must_use]
    pub const fn pos(&self) -> Vec2 {
        self.pos
    }

    /// 最近一次感知光标（VDC；S3-M4 run_loop 接线 DragStart / Drag 跟手时取最新点用）。
    #[must_use]
    pub const fn cursor(&self) -> Option<Vec2> {
        self.cursor
    }

    /// 是否处于甩出飞行（S3-M4 单测 / 诊断视图：Fall 相落地结算的分流依据）。
    #[must_use]
    pub const fn thrown_flight(&self) -> bool {
        self.thrown_flight
    }

    /// 甩出落地累计次数（C8 诊断视图；每判定一次「甩出落地」加一）。
    #[must_use]
    pub const fn throw_landed_count(&self) -> u64 {
        self.throw_landed_count
    }

    /// 应用一批感知事件（设计补充 §3.2-①；`Cursor`→引擎、`Windows`→暂存带）。
    ///
    /// `System` 样本中的 `presence_idle_ms` 被缓存供 1Hz 业务档注入 `TickEnv`（S4-M1）；
    /// 其余系统字段（电量 / CPU）与 `Fullscreen` 仍不消费（全屏轮询已由 supervisor 处理，
    /// 本层不重复）。
    pub fn apply_perception(&mut self, events: Vec<PerceptionEvent>) {
        for ev in events {
            match ev {
                PerceptionEvent::Cursor { pos } => {
                    self.cursor = Some(pos);
                    self.motion.set_cursor(Some(pos));
                }
                PerceptionEvent::Windows { titlebars, taskbars } => {
                    self.pending_inputs = ports::platform_inputs(titlebars, taskbars);
                }
                PerceptionEvent::System(sample) => {
                    self.presence_idle_ms = sample.presence_idle_ms;
                }
                PerceptionEvent::Fullscreen { .. } => {}
                // S7-M1：活动感知采样（键击 / 点击 / 移动强度 + 前台进程类别哈希）。
                // 隐私：载荷只含计数类量与哈希；感知关闭时装配层不产本事件。
                PerceptionEvent::Activity(sample) => {
                    self.activity = sample;
                }
            }
        }
    }

    /// 最近一次活动感知采样（S7-M1；供 S7-M4 的 `busyness` 因子消费）。
    #[must_use]
    pub const fn activity(&self) -> ActivitySample {
        self.activity
    }

    /// 摄入活动感知采样（供测试 / 未来第二数据源直注；生产路径经
    /// [`Self::apply_perception`] 的 `Activity` 臂）。
    pub fn set_activity(&mut self, sample: ActivitySample) {
        self.activity = sample;
    }

    /// 应用显示器拓扑快照（设计补充 §3.2-②；变更时 `on_monitors_changed` + `PlatformGraph` 整体重建）。
    pub fn apply_monitors(&mut self, monitors: Vec<MonitorGeomAlias>, now_ms: u64) {
        if !monitors_changed(&self.monitors, &monitors) {
            return;
        }
        self.monitors = monitors;
        self.motion.on_monitors_changed(self.monitors.clone(), now_ms);
        // PlatformGraph 无 set_monitors（§6-5）：整体重建。
        // S7-M3 前置首件：`Arc` 句柄被决策层站立面共享，故**原地替换内容**而非换句柄。
        *self.platform_graph.write().unwrap_or_else(|e| e.into_inner()) =
            PlatformGraph::new(self.monitors.clone(), now_ms);
        let bounds = ports::bounds_of(&self.monitors, self.active_pos());
        if let Some(phys) = self.physics.as_mut() {
            phys.set_bounds(bounds);
        }
    }

    /// 推进一次 logic tick（设计补充 §3.2 逐步顺序：①平台图重建 → ②站立面消失检查/换相
    /// →③tick 活跃引擎 →④事件转发；返回权威 `pos` 与待发动作）。
    pub fn logic_tick(&mut self, now_ms: u64) -> LogicOutcome {
        let prev = self.pos;
        // 驱动侧 dt 观测口径（护栏：引擎内部已各自钳 MAX_TICK_DT_MS，此处不重写物理子步）。
        let _dt_ms = now_ms.saturating_sub(self.last_logic_ms).min(MAX_TICK_DT_MS);
        self.last_logic_ms = now_ms;

        // ① PlatformGraph 2s 重建（deadline 链绝对锚定由引擎内部维护）。
        {
            let mut graph = self.platform_graph.write().unwrap_or_else(|e| e.into_inner());
            if graph.should_rebuild(now_ms) {
                let inputs = self.pending_inputs.clone();
                graph.rebuild(now_ms, inputs);
            }
        }

        // ② 站立面消失检查（仅 Roam 相；AC-08）：命中即换相重建 PhysicsEngine。
        let on_surface = {
            let graph = self.platform_graph.read().unwrap_or_else(|e| e.into_inner());
            graph.is_valid_stand(self.pos)
        };
        if self.phase == Phase::Roam && !on_surface {
            let bounds = ports::bounds_of(&self.monitors, self.pos);
            self.physics =
                Some(PhysicsEngine::new(self.pos, self.interaction_cfg.clone(), bounds));
            self.phase = Phase::Fall;
        }

        // ③ tick 活跃引擎（不变量 A：只 tick 一个，另一个停放）。
        let events: Vec<MotionEvent> = match self.phase {
            Phase::Roam => {
                self.motion.set_cursor(self.cursor);
                let evs = self.motion.tick(now_ms);
                self.pos = self.motion.pos();
                evs
            }
            Phase::Fall => {
                // 共享站立面：物理落地判定读同一份 `PlatformGraph`（只读锁，
                // 作用域内不与其他字段的可变借用相交）。
                let graph = self.platform_graph.read().unwrap_or_else(|e| e.into_inner());
                let evs = match self.physics.as_mut() {
                    Some(phys) => phys.tick(now_ms, &*graph),
                    None => Vec::new(),
                };
                self.pos = self.physics.as_ref().map_or(self.pos, |p| p.pos());
                evs
            }
            Phase::Drag => {
                // 两引擎均停放（不变量 A 特例）：权威 pos = 钳制后光标位（不变量 B
                // 特例）。无光标样本（异常）保持原位；钳制细节见 `clamp_drag_target`。
                self.pos = self.clamp_drag_target(self.drag_cursor.unwrap_or(self.pos));
                Vec::new()
            }
        };

        // ④ 事件转发：仅 Landed 走仲裁（唯一 MotionEvent→仲裁 接线，卡片要点 5）。
        let mut submitted = None;
        let mut landed_thrown = false;
        for ev in &events {
            match ev {
                MotionEvent::Landed { .. } => {
                    // 换相重建 MotionEngine（不变量 C：以物理末位为入场 pos，seed_k 递推）。
                    self.reenter_roam(now_ms);
                    let thrown = self.thrown_flight;
                    self.thrown_flight = false;
                    landed_thrown = thrown;
                    if thrown {
                        // 甩出落地（FR-4-6）：ACT-T-07→08 落地链 + C8 计数
                        // （落地尘土 `pet://fx{dust}` 由 run_loop 依 landed_thrown 发出）。
                        self.throw_landed_count = self.throw_landed_count.wrapping_add(1);
                        eprintln!("[dp-app] core-loop 甩出落地：count={}", self.throw_landed_count);
                        // 链提交**先于**停播推进（B15-④ 裁定）：07/08 依序入队依赖
                        // toss 仍在 current 位；若先停播，07 起播后 08 会被 R-A
                        // Suppressed而断链。停播推进后队列最优者（07）fade 0 起播。
                        submitted = self.submit_throw_landing_chain(now_ms);
                        // S4 B15-④：落地即结束在播循环 toss（所有 Landed 口径）。
                        self.stop_looping_current(now_ms);
                    } else {
                        // S4 B15-④：落地即结束在播循环动作（所有 Landed 口径，计划
                        // 注记：原仅甩出路径，现扩至全部 Landed；非循环 current 不受影响）。
                        self.stop_looping_current(now_ms);
                        submitted = self.submit_landing(now_ms);
                    }
                    break;
                }
                // 其余事件（TargetDecided/Arrived/IdleFallback/Migrated）本阶段仅消费不换相
                // （业务动作下发登记为遗留 §6-8）。
                MotionEvent::TargetDecided { .. }
                | MotionEvent::Arrived { .. }
                | MotionEvent::IdleFallback
                | MotionEvent::Migrated { .. } => {}
            }
        }

        LogicOutcome { pos: self.pos, moved: self.pos != prev, submitted, landed_thrown }
    }

    /// 仲裁器活性轮询（设计补充 §3.2-⑧；S4 B15-④ 起播经播放指令通道下发覆盖态，
    /// 本轮询仅保留 fade 观测日志）。
    pub fn poll_arbiter(&mut self, now_ms: u64) -> Option<Started> {
        self.arbiter.poll(now_ms)
    }

    /// 业务 1Hz 档（S4-M1 接线：会话暂停检测 → [`EmotionEngine::tick_1s`] → 事件落日志）。
    ///
    /// `wall` 为墙钟端口（C3）。业务时间与本地时间**都经该端口取**，本层不直接使用
    /// `chrono` —— `dp-app` 不引入 `chrono` 依赖（C9：不新增第三方依赖）。
    ///
    /// ## 数据流
    ///
    ///   1. **会话监视**（S4-M1 要点 3）：`SessionWatcher::observe` 以轮询式锁屏 / 远程桌面
    ///      检测（`dp-platform` 侧 `WTSQuerySessionInformationW`）叠加 idle 回退，输出
    ///      `session_paused`（带 `hysteresisMs` 迟滞，避免临界抖动）。
    ///   2. **环境组装**：把会话暂停 + 缓存的 `presence_idle_ms` + 需求快照塞进 [`TickEnv`]。
    ///   3. **内核推进**：`tick_1s` 内部完成因子 → ΔP → 阶段结算 → Mood 惯性 → 日切。
    ///   4. **事件落地**：本卡只做 C8 诊断日志（`pet://` 事件注册与气泡投递归 S4-M2）。
    ///
    /// 暂停期间 `tick_1s` 会冻结 P 累积且不跳变（验收标准 (a) 的实现位置在内核，
    /// 本方法只负责把 `session_paused` 如实喂入）。
    pub fn business_tick(&mut self, wall: &dyn WallClock, app: Option<&AppHandle>) {
        let wall_now_ms = wall.now_ms();
        // ① 会话暂停（迟滞口径在 `SessionWatcher` 内）。注意 `observe` 的返回值是
        //    **迁移标志**而非当前暂停态 —— 后者经 `is_paused()` 读。
        let session_changed = self
            .session
            .observe(wall_now_ms.max(0) as u64, self.presence_idle_ms);
        let paused = self.session.is_paused();
        self.business_tick_inner(wall, wall_now_ms, paused, session_changed, app);
    }

    /// 业务档内核推进（**跳过会话监视**，直接给定 `session_paused`）。
    ///
    /// 拆出此函数的原因：① 单测需要绕开平台会话查询（本机无头环境下
    /// `query_session_state()` 恒返回 `Disconnected`，会使所有业务档用例恒处于暂停态）；
    /// ② 后续里程碑（S4-M2 起）若有其它暂停来源（如演出中 `performing`），
    /// 也可经此单点注入而不重复实现链路。
    ///
    /// `app` 为传输层句柄（`None` = 纯逻辑模式，常见于单测）：**仅**用于 `emit`
    /// 事件；所有事件名与载荷生产均在 `dp-core`（C8 单一真源），本层不做任何
    /// 事件名拼接。
    fn business_tick_inner(
        &mut self,
        wall: &dyn WallClock,
        wall_now_ms: i64,
        paused: bool,
        session_changed: bool,
        app: Option<&AppHandle>,
    ) {
        // ① S5-M2：回归问候投递（离线补偿在装配期产出，首个业务档才具备发播条件：
        //    此时 core-loop 线程已起、播放通道已接入、`app` 句柄可用）。
        self.flush_startup_greeting(app, wall_now_ms.max(0) as u64);

        // ①' S5-M4：设置热更新消费（应用层已落地窗口 / 音频 / 注册表；此处落内核与存档）。
        //     放在最前：本拍的 TickEnv 与事件派生都应当使用**最新**设置。
        self.apply_pending_settings(app, wall_now_ms.max(0) as u64);

        // ② 环境组装（本地时间同经端口取；需求快照取自内核当前状态）。
        let env = TickEnv {
            now_local: wall.now_local(),
            session_paused: paused,
            performing: false,
            // S8-M1：外出活动进行中（Preparing / Running / Returning）→ 内核演出占用位
            // （`TickEnv.activity_running` 语义：宠物不在桌面，情绪 / 需求处理按在途活动降档）。
            activity_running: self.activity_runtime.is_running(),
            event_delta: self.pending_mood_delta,
            // 采样不可用（`None`）→ 保守取 0 = 判定「在场」，避免把检测失败
            // 误算成用户离开而虚增冷落压力。
            // S5-M4 / Q-18：活跃感知关闭 → 恒取 0（恒在场），退化为纯时间模型。
            preset_idle_ms: if self.activity_sensing {
                self.presence_idle_ms.unwrap_or(0)
            } else {
                0
            },
            // S7-M6 / FR-11-12：**不可达判定唯一真源** = 穿透 ∨ 勿扰 ∨ 已离家。
            // （`dnd` 会卸载鼠标钩子，`click_through` 会卸载鼠标 + 键盘钩子，`runaway_away`
            //   时窗口本身隐藏 —— 三者都让桌面交互完全不可达。）
            interaction_available: self.interaction_available(),
            satiety: self.emotion.state.values.satiety,
            cleanliness: self.emotion.state.values.cleanliness,
            // S7-M4：活动感知采样（隐私关闭时**不喂**，忙碌档退化为轻度；Q-18 ④）。
            activity: if self.activity_sensing { Some(self.activity) } else { None },
            // 负向事件的**主通路**是 `on_interaction_kind(Tickle/Throw)`（逐次精确定位）；
            // 本字段保留给「非交互来源的负向事件」（如跨档结算），当前恒 false。
            negative_happened: false,
            ..TickEnv::default()
        };
        // 交互净增益按 tick 消费（幂等：喂入后清零，避免重复计入后续 tick）。
        // **暂停态不清零**：暂停期间内核早退不消费，保留待恢复后一次计入。
        if !paused {
            self.pending_mood_delta = 0.0;
        }

        // ③ 内核推进。
        let outcome = self.emotion.tick_1s(wall_now_ms, env);

        // ④ 事件落地（S4-M2）：算法事件 → 线上事件（映射在 `dp-core::event`，
        //    本层只做 emit + 降级日志，C8）。`ForceAction` 走动作仲裁而非事件面。
        if session_changed || outcome.pause_changed {
            eprintln!(
                "[dp-app] core-loop 会话暂停态迁移：paused={paused} 累计={}ms（{} 次窗口）",
                self.emotion.state.pause.accumulated_ms, self.emotion.state.pause.count
            );
        }
        self.dispatch_emotion_events(&outcome.events, wall_now_ms, app);

        // ④' S7-M2：需求跨档 → `pet://needs`（`02 §7.6` 频率列 = 属性跨档时）。
        self.emit_needs_event(app);

        // ④'' S7-M2：提醒到点（`03 §3.3 B21 ②`：久坐 / 喝水 → `ACT-P-03` 触发面）。
        self.reminder_tick(wall_now_ms, self.last_logic_ms);

        // ④''' S7-M3：耦合矩阵速度输出 → 行走速度（C-12 精力 ×0.8 / C-11 档位饥饿 ×0.85）。
        self.apply_speed_mul();

        // ④'''' S7-M9：生存动作分档轮询（`02 §5.11`；AC-21/22 触发面，未启用自动跳过）。
        self.needs_action_tick(wall_now_ms, app);

        // ④''''' S8-M1/M2：活动推进（时钟观察 / 异常判定 / 明信片调度 / 到期回归 /
        //      演出闭环）。放在需求动作之后：活动在途时需求演出让位给活动演出。
        self.activity_tick(wall, app);

        // ④'''''' S7-M6：托盘图标 / 菜单位同步（仅变化时调，避免每拍重建菜单）。
        self.sync_tray(app);

        // ⑤ 1Hz 全量快照（`pet://state`）：无论有无算法事件都发，供属性面板 /
        //    原因卡实时刷新（`02 §7.6` 频率列 = 1Hz）。
        self.emit_state_snapshot(app);
    }

    // -----------------------------------------------------------------------
    // S8-M1/M2：外出活动运行时（`02 §5.13` ~ §5.16）
    // -----------------------------------------------------------------------

    /// 派遣前置检查（当时内核快照；`dispatch_precheck` 逐条执行：唯一性 → AC-36
    /// L4/L5 → RV-03 Energy → RV-01 Satiety → 学习 Cleanliness → 打工日限 →
    /// 安静时段；`today_job_count` 随 S8-M5 经济计费落账后计数，本卡恒 0）。
    fn dispatch_check(&self) -> DispatchCheck {
        let now_ms = self.wall.now_ms();
        let offset_sec = self.wall.local_offset_sec();
        let quiet = &self.activity_runtime.cfg().quiet_hours;
        DispatchCheck {
            active: self.activity_runtime.is_running(),
            neglect_level: u32::from(self.emotion.neglect.level),
            energy: self.emotion.state.values.energy,
            satiety: self.emotion.state.values.satiety,
            cleanliness: self.emotion.state.values.cleanliness,
            today_job_count: 0,
            local_hour: dp_activity::local_hour_from(now_ms, offset_sec),
            quiet_from_hour: hour_of_hhmm(&quiet.from),
            quiet_to_hour: hour_of_hhmm(&quiet.to),
            daily_job_limit: self.activity_runtime.cfg().daily_job_limit,
        }
    }

    /// S8-M1：派遣出口（`apply_core_input(ActivityDispatch)` 消费）。
    ///
    /// 成功 → ① 实例进入 Preparing；② 出发演出提交（S8-M4 启用后起播；未启用 →
    /// 降级，`step_activity_performance` 下一拍自动 `confirm_departed`）；③ 前置消耗
    /// （`cost.energy/cleanliness` 出发瞬间扣，`02 §5.15`）。
    fn handle_activity_dispatch(
        &mut self,
        kind: &str,
        def_id: &str,
        duration_min: u32,
        now_ms: u64,
    ) -> Result<ActivityInstance, ActivityError> {
        let kind = match kind {
            "work" => ActivityKind::Work,
            "study" => ActivityKind::Study,
            "travel" => ActivityKind::Travel,
            _ => return Err(ActivityError::UnknownDef(kind.to_string())),
        };
        let check = self.dispatch_check();
        let inst = self
            .activity_runtime
            .dispatch(kind, def_id, duration_min, now_ms as i64, None, &check)?;
        // 出发演出（未启用 → 降级记录，活动照常推进）。
        self.submit_activity_play(ActivitySlot::Depart);
        // 前置消耗（出发瞬间立即扣；旅游无前置数值消耗）。
        if let Some(cost) = self.activity_runtime.current_cost() {
            let deltas = ActivityDeltas {
                energy: -cost.energy,
                cleanliness: -cost.cleanliness,
                ..ActivityDeltas::default()
            };
            let _ = self.emotion.apply_activity_deltas(&deltas, now_ms as i64);
        }
        self.request_save();
        Ok(inst)
    }

    /// S8-M1/M2：活动每拍推进（1Hz 业务档；时间全部经 `wall` 端口，C3）。
    ///
    /// 顺序：① `ActivityRuntime::tick`（时钟观察 → 异常判定 → 明信片调度 → 到期回归 /
    /// 深夜延后 D-1）；② 演出闭环（Preparing 出发演出完成 → Running；Returning 回归
    /// 演出完成 → 结算）；③ 明信片到期落存档计数（挂件 UI 归 S8-M3）。
    fn activity_tick(&mut self, wall: &dyn WallClock, _app: Option<&AppHandle>) {
        if !self.activity_runtime.is_running() {
            return;
        }
        let now_ms = wall.now_ms();
        let offset_sec = wall.local_offset_sec();
        let mut out = Vec::new();
        self.activity_runtime.tick(now_ms, offset_sec, &mut out);
        for outcome in out {
            match outcome {
                ActivityOutcome::PostcardDue { instance, at_ms } => {
                    // S8-M3/M4：明信片落存档计数 + 挂件 UI（快照驱动）+ 寄明信片演出
                    //（`actionIds.postcard` → ACT-N-13；不可打断短演出，播毕链尾推进）。
                    eprintln!(
                        "[dp-app] 旅游明信片到期：{}（第 {} 张 @{at_ms}）",
                        instance.def_id, instance.postcards_sent
                    );
                    self.submit_activity_play(ActivitySlot::Postcard);
                    self.request_save();
                }
                ActivityOutcome::TimeUp(inst) => {
                    // 到期 → 回归演出（S8-M4 启用后播；未启用降级 → 下一拍直接结算）。
                    // 学习桌面循环演出（ACT-N-11 looping）到期时先停播收尾，再走回归。
                    eprintln!(
                        "[dp-app] 活动 {}（{}）到期，进入回归演出",
                        inst.def_id,
                        inst.kind.label()
                    );
                    if inst.kind == ActivityKind::Study {
                        self.stop_study_loop(now_ms as u64);
                    }
                    self.submit_activity_play(ActivitySlot::Return);
                    self.request_save();
                }
                ActivityOutcome::Aborted(reward) => {
                    // 时钟异常保底结算（`02 §5.14`：按已完成比例，不再额外惩罚）。
                    eprintln!(
                        "[dp-app] 活动异常中止（时钟回拨），保底结算：coin={} mood={:+.1}",
                        reward.coin, reward.mood_delta
                    );
                    self.apply_reward(&reward, now_ms);
                    self.request_save();
                }
                _ => {}
            }
        }
        // S8-M4：学习桌面循环演出（Running 期间循环播 `desk_loop` → ACT-N-11；
        // `has_action` 幂等守卫——已播/已排队不重复提交，防队列堆积）。
        if self.activity_runtime.phase() == ActivityPhase::Running
            && self
                .activity_runtime
                .current()
                .is_some_and(|i| i.kind == ActivityKind::Study)
        {
            self.submit_study_loop();
        }
        // 演出闭环（不依赖播放完成回调的精确时序：1Hz 轮询 + drain_finished 推进队列）。
        self.step_activity_performance(now_ms);
    }

    /// 学习桌面循环演出提交（`desk_loop` → ACT-N-11；幂等守卫见 `has_action`）。
    fn submit_study_loop(&mut self) {
        let Some(action_id) = self.activity_play_id(ActivitySlot::DeskLoop) else {
            return;
        };
        if action_id.is_empty() || self.arbiter.has_action(&action_id) {
            return;
        }
        self.submit_play_by_id(&action_id, ActionSource::Activity);
    }

    /// 学习循环演出停播（到期收尾：仅当 current 恰为 `desk_loop` 循环动作时停播
    /// 并推进仲裁链；被用户交互打断后 current 已非学习动作 → no-op，不误停）。
    fn stop_study_loop(&mut self, now_ms: u64) {
        let Some(action_id) = self.activity_play_id(ActivitySlot::DeskLoop) else {
            return;
        };
        if action_id.is_empty()
            || !self
                .arbiter
                .current()
                .is_some_and(|a| a.request.id == action_id)
        {
            return;
        }
        self.stop_looping_current(now_ms);
    }

    /// 演出闭环：Preparing（出发演出播毕 / 未启用）→ `confirm_departed`；
    /// Returning（回归演出播毕 / 未启用）→ 结算。
    ///
    /// 判定口径：仲裁器当前在播动作 == 槽位动作 ID 才视为「演出中」；动作未启用 /
    /// 不在目录 / 播毕出队 → 下一拍立即推进，**不卡死**（S8-M4 资源缺失可降级运行）。
    fn step_activity_performance(&mut self, now_ms: i64) {
        match self.activity_runtime.phase() {
            ActivityPhase::Preparing => {
                if self.activity_play_active(ActivitySlot::Depart) {
                    return;
                }
                if let Err(err) = self.activity_runtime.confirm_departed() {
                    eprintln!("[dp-app] 活动出发确认失败（降级继续）：{err}");
                }
            }
            ActivityPhase::Returning => {
                if self.activity_play_active(ActivitySlot::Return) {
                    return;
                }
                self.settle_returning(now_ms);
            }
            _ => {}
        }
    }

    /// 提交活动槽位演出（出发 / 回归；`actionIds.depart / return`）。
    ///
    /// 目录缺失 / `disabled`（批次 C 未交付）→ 降级记录，不 panic；演出闭环由
    /// [`Self::step_activity_performance`] 兜底推进。
    fn submit_activity_play(&mut self, slot: ActivitySlot) {
        let Some(action_id) = self.activity_play_id(slot) else {
            return;
        };
        self.submit_play_by_id(&action_id, ActionSource::Activity);
    }

    /// 通用演出提交（目录缺失 / `disabled`（批次 C 未交付）→ 降级记录，不 panic；
    /// 演出闭环由 [`Self::step_activity_performance`] 兜底推进；S8-M4 起活动演出
    /// 源为 [`ActionSource::Activity`]（仲裁器分源口径，`02 §4.3` 冻结词汇））。
    fn submit_play_by_id(&mut self, action_id: &str, source: ActionSource) {
        if action_id.is_empty() {
            return;
        }
        let Some(cfg) = self.catalog.find(action_id) else {
            eprintln!("[dp-app] 活动演出动作 {action_id} 不在目录（降级不提交）");
            return;
        };
        let Some(request) = ActionRequest::from_cfg(cfg, source) else {
            eprintln!("[dp-app] 活动演出动作 {action_id} 未启用（批次 C 未交付）→ 降级");
            return;
        };
        let now = self.wall.now_ms().max(0) as u64;
        let verdict = self.arbiter.submit(request, now);
        self.settle_play(verdict, action_id);
    }

    /// 槽位动作 ID（当前实例 `actionIds`；无实例 → `None`）。
    fn activity_play_id(&self, slot: ActivitySlot) -> Option<String> {
        let ids = self.activity_runtime.action_ids()?;
        Some(match slot {
            ActivitySlot::Depart => ids.depart.clone(),
            ActivitySlot::Return => ids.r#return.clone(),
            ActivitySlot::DeskLoop => ids.desk_loop.clone(),
            ActivitySlot::Postcard => ids.postcard.clone(),
        })
    }

    /// 槽位演出是否在播（仲裁器 current == 槽位动作 ID）。
    fn activity_play_active(&self, slot: ActivitySlot) -> bool {
        let Some(id) = self.activity_play_id(slot) else {
            return false;
        };
        if id.is_empty() {
            return false;
        }
        self.arbiter.current().is_some_and(|a| a.request.id == id)
    }

    /// 回归结算（Returning → Settled → Idle）：组装 [`SettleInputs`] 后
    /// `confirm_reported` → 应用到内核（`ActivityDeltas`）→ 清场。
    ///
    /// `recall_kind` 由用户召回来源决定（`ActivityRecall` 置位；自然到期 = Normal）；
    /// 提前召回惩罚（Mood−4 / P+6 / rough+0.15）在 [`crate::settle`] 计算、
    /// 此处经 `neglect_delta` / `rough_delta` 落地到 `EmotionEngine`。
    fn settle_returning(&mut self, _now_ms: i64) {
        let now = self.wall.now_ms();
        let offset_sec = self.wall.local_offset_sec();
        let local_hour = dp_activity::local_hour_from(now, offset_sec);
        let recall_kind = self.pending_recall_kind.take().unwrap_or(RecallKind::Normal);
        let neglect_add = self.activity_runtime.cfg().recall_penalty.neglect_add;
        let rough_step = self.activity_runtime.cfg().recall_penalty.rough_step;
        let inputs = SettleInputs {
            mood: self.emotion.state.values.mood,
            cleanliness: self.emotion.state.values.cleanliness,
            satiety: self.emotion.state.values.satiety,
            diligence: (self.emotion.personality().diligence * 100.0).clamp(0.0, 100.0),
            // S8-M5/M6：饱食度 <20 → 打工收益 ×0.7（饿肚子干活打折）；<5 的拒派在 dispatch 处。
            economy_scale: if self.emotion.state.values.satiety < 20.0 { 0.7 } else { 1.0 },
            time_segment: dp_activity::time_segment_of(local_hour),
            present: true, // 预留字段：settle 当前不消费；在场判定细化归 S8-M5
            local_hour,
            seed: self
                .activity_runtime
                .current()
                .map(|i| i.seed)
                .unwrap_or(0),
            now_ms: now,
            recall_neglect_add: neglect_add,
            recall_rough_step: rough_step,
            regress_neglect_delta: -20.0, // `02 §5.15` 回归 | P−20
        };
        match self.activity_runtime.confirm_reported(&inputs, recall_kind) {
            Ok(reward) => {
                eprintln!(
                    "[dp-app] 活动结算（{recall_kind:?}）：coin={} skill={} mood={:+.1} 亲和={:+.1}",
                    reward.coin, reward.skill_points, reward.mood_delta, reward.affinity_exp
                );
                self.apply_reward(&reward, now);
                self.activity_runtime.clear();
                // S8-M4：结算后 Energy<40 → 疲惫喘气演出（ACT-N-16 looping，
                // `01 §6.13`；低能量打工/学习收尾表现）。
                if self.emotion.state.values.energy < 40.0 {
                    self.submit_play_by_id("ACT-N-16", ActionSource::Activity);
                }
                self.request_save();
            }
            Err(err) => eprintln!("[dp-app] 活动结算失败（保持 Returning 待重试）：{err}"),
        }
    }

    /// 活动结算结果 → 内核数值（`ActivityDeltas`；`02 §5.15` 数值面；经济归 S8-M5）。
    ///
    /// `reward.mood_delta` 已含召回 Mood 惩罚（`recallPenalty.mood`）；`neglect_delta` /
    /// `rough_delta` 只在召回时非零（P+6 / rough+0.15）。
    fn apply_reward(&mut self, reward: &ActivityReward, now_ms: i64) {
        let deltas = ActivityDeltas {
            mood: reward.mood_delta,
            energy: reward.energy_delta,
            cleanliness: reward.cleanliness_delta,
            affinity_exp: reward.affinity_exp,
            neglect_p_delta: reward.neglect_delta,
            rough_step: reward.rough_delta,
        };
        let _ = self.emotion.apply_activity_deltas(&deltas, now_ms);
        // S8-M5：打工结算心币入账（经三道硬顶；幂等 refId = 结算时刻）。
        if reward.coin > 0 {
            let day_key = Self::local_day_key(now_ms);
            let activity_id = format!("{:?}", reward.kind);
            let ref_id = format!("work:{:?}:{now_ms}", reward.kind);
            self.economy.credit_work(
                &activity_id,
                reward.coin,
                &ref_id,
                now_ms,
                &day_key,
            );
            self.request_save();
        }
    }

    /// 由墙钟毫秒推导本地日桶（`YYYY-MM-DD`；C3 口径与 dp-economy 一致）。
    fn local_day_key(at_ms: i64) -> String {
        use chrono::{TimeZone, Utc};
        let dt = Utc.timestamp_millis_opt(at_ms).single().unwrap_or_default();
        dt.format("%Y-%m-%d").to_string()
    }

    /// 由墙钟毫秒推导 ISO 周桶（`YYYY-Www`；限购周桶用）。
    fn local_week_key(at_ms: i64) -> String {
        use chrono::{Datelike, TimeZone, Utc};
        let dt = Utc.timestamp_millis_opt(at_ms).single().unwrap_or_default();
        format!("{}-W{:02}", dt.iso_week().year(), dt.iso_week().week())
    }

    /// 活动相关状态变更 → 存档脏位（下一拍 `save_tick` 落盘）。
    fn request_save(&mut self) {
        self.save_requested = true;
    }

    /// S8-M3：活动快照 JSON（`pet://state.activity`；前端 ActivityCard 消费）。
    fn activity_snapshot_json(&self) -> Option<serde_json::Value> {
        let rt = &self.activity_runtime;
        let phase = rt.phase();
        if !phase.is_active() {
            return Some(serde_json::json!({ "phase": phase.as_str(), "running": false }));
        }
        let inst = rt.current()?;
        let now = self.wall.now_ms();
        Some(serde_json::json!({
            "phase": phase.as_str(),
            "running": true,
            "instance": {
                "kind": inst.kind.label(),
                "defId": inst.def_id,
                "progressRatio": inst.progress_ratio(now),
                "remainingMs": inst.remaining_ms(now),
                "startMs": inst.start_ms,
                "endMs": inst.end_ms,
                "postcardsSent": inst.postcards_sent,
                "deferredSettle": inst.deferred_settle,
            }
        }))
    }

    /// 测试注入墙钟（C3：活动 dispatch / 结算时间唯一真源）。
    #[cfg(test)]
    pub fn set_wall_for_test(&mut self, wall: Arc<dyn WallClock>) {
        self.wall = wall;
    }

    /// S7-M2：需求跨档 → `pet://needs`（唯一生产点 `dp_core::event::wire_for_needs`，C8）。
    ///
    /// 未跨档 / 尚未推进过 / 纯逻辑模式（`app = None`）→ 不产事件。
    fn emit_needs_event(&self, app: Option<&AppHandle>) {
        let Some(handle) = app else { return };
        let Some(outcome) = self.emotion.last_needs() else { return };
        if !outcome.band_changed {
            return;
        }
        let wire = wire_for_needs(&outcome);
        if let Err(err) = handle.emit(wire.event, &wire.payload) {
            eprintln!("[dp-app] core-loop 广播 {} 降级：{err}", wire.event);
        }
    }

    /// S7-M2：提醒偏好（存档 B 段 ⊕ `schedule.json` 默认）→ 调度器。
    ///
    /// **幂等**：配置与当前一致 → 不重排（否则 1Hz 业务档会每秒重置 deadline）。
    pub fn sync_reminders(&mut self, snapshot: &bridge::SettingsSnapshot, now_ms: i64) {
        let r = &snapshot.reminders;
        let cfg = ReminderConfig::new(
            r.sedentary_enabled,
            r.sedentary_interval_min,
            r.water_enabled,
            r.water_interval_min,
            r.interval_min_min,
            r.interval_max_min,
            self.reminder_ack_resets,
        );
        if *self.reminders.config() == cfg {
            return;
        }
        self.reminders.set_config(cfg, now_ms);
    }

    /// S7-M2：提醒到点处置（`03 §3.3 B21 ②`）。
    ///
    /// 现状口径（重要）：`ACT-P-03` 属**资源批次 C**（`actions.json` `disabled=true`），
    /// [`ActionRequest::from_cfg`] 对其返回 `None` → 本函数只记录「触发面已到点」，
    /// 不提交动作。资源交付后本条链路**零改动**即可实播。
    /// 提醒气泡文案（`lines.json` 无提醒池）归 S7-M8 台词库重写。
    fn reminder_tick(&mut self, wall_now_ms: i64, mono_ms: u64) {
        // FR-10-4：勿扰静默（调度器自身也判，双保险）。
        self.reminders.set_do_not_disturb(self.dnd);
        let Some(ev) = self.reminders.tick(wall_now_ms) else { return };
        let channel = match ev.channel {
            ReminderChannel::Sedentary => "sedentary",
            ReminderChannel::Water => "water",
        };
        eprintln!("[dp-app] core-loop 提醒到点：{channel}（t={wall_now_ms}ms）");
        let Some(cfg) = self.catalog.find(REMINDER_ACTION_ID) else {
            eprintln!("[dp-app] 提醒动作 {REMINDER_ACTION_ID} 不在动作目录（降级仅记录）");
            return;
        };
        let Some(request) = ActionRequest::from_cfg(cfg, ActionSource::Ambient) else {
            eprintln!(
                "[dp-app] 提醒动作 {REMINDER_ACTION_ID} 未启用（批次 C 资源未交付）→ 触发面就绪，待资源"
            );
            return;
        };
        let verdict = self.arbiter.submit(request, mono_ms);
        self.settle_play(verdict, REMINDER_ACTION_ID);
    }

    /// S7-M9：生存动作分档轮询（`02 §5.11`；AC-21/22 触发面）。
    ///
    /// 口径同提醒链路：`actions.json` 的 `disabled`（资源批次 B 未交付）→
    /// [`ActionRequest::from_cfg`] 返回 `None`，只记「触发面到点」；资源交付后
    /// **零改动实播**。求助类动作（讨食 / 求洗澡）同时产求助气泡（`01 §6.16.3`，
    /// 3min 间隔由 [`BubblePlanner`] 内建；勿扰静默口径同 S5-M4）。
    fn needs_action_tick(&mut self, now_ms: i64, app: Option<&AppHandle>) {
        let effects = self
            .emotion
            .needs()
            .effects(self.emotion.needs_cfg(), &self.emotion.state.values);
        let intents = self.needs_trigger.poll(&effects, now_ms);
        for intent in intents {
            self.submit_needs_id(&intent.action_id, now_ms.max(0) as u64);
            if let Some(pool) = intent.bubble_pool {
                self.emit_help_bubble(&pool, now_ms, app);
            }
        }
    }

    /// S7-M9：求助气泡（讨食 / 求洗澡；冷却未过 → 不弹；勿扰静默口径同 S5-M4）。
    fn emit_help_bubble(&mut self, pool: &str, now_ms: i64, app: Option<&AppHandle>) {
        if self.dnd && self.dnd_pause_bubbles {
            return;
        }
        let Some(plan) = self.bubble.bubble_for_pool(&self.lines, pool, &self.vars, now_ms, true)
        else {
            return;
        };
        let wire = wire_for_bubble(&plan);
        let Some(handle) = app else { return };
        if let Err(err) = handle.emit(wire.event, &wire.payload) {
            eprintln!("[dp-app] core-loop 广播 {} 降级：{err}", wire.event);
        }
    }

    /// S7-M9：按 ID 提交生存动作（`disabled` / 目录缺失 → 降级记录，不 panic）。
    fn submit_needs_id(&mut self, action_id: &str, mono_ms: u64) {
        let Some(cfg) = self.catalog.find(action_id) else {
            eprintln!("[dp-app] 生存动作 {action_id} 不在动作目录（降级仅记录）");
            return;
        };
        let Some(request) = ActionRequest::from_cfg(cfg, ActionSource::Ambient) else {
            eprintln!(
                "[dp-app] 生存动作 {action_id} 未启用（批次 B 资源未交付）→ 触发面就绪，待资源"
            );
            return;
        };
        let verdict = self.arbiter.submit(request, mono_ms);
        self.settle_play(verdict, action_id);
    }

    /// S7-M9：喂食事务端点动作（AC-22：喂食开始 → ACT-N-02；完成后饱食 ≥ 满档 → ACT-N-03）。
    ///
    /// 调用点：`apply_core_input(TrayFeed)` 消费**之后**（饱食度已更新，读真实终值）。
    fn settle_feed_transaction(&mut self, now_ms: u64) {
        self.submit_needs_id(NeedsActionTrigger::feed_started(), now_ms);
        let satiety = self.emotion.state.values.satiety;
        if let Some(action) = NeedsActionTrigger::feed_completed(satiety, self.emotion.needs_cfg())
        {
            self.submit_needs_id(action, now_ms);
        }
    }

    /// S7-M9：洗澡事务端点动作（AC-22：洗澡开始 → ACT-N-07、结束 → ACT-N-08）。
    ///
    /// 调用点：`apply_core_input(TrayBath)` 消费**之后**。演出时长编排（8~12s）归 S8；
    /// 本卡提交端点动作，仲裁器按优先级排队（N-08 在 N-07 演出结束后起播）。
    fn settle_bath_transaction(&mut self, now_ms: u64) {
        self.submit_needs_id(NeedsActionTrigger::bath_started(self.emotion.needs_cfg()), now_ms);
        self.submit_needs_id(
            NeedsActionTrigger::bath_completed(self.emotion.needs_cfg()),
            now_ms,
        );
    }

    /// S7-M3：耦合矩阵速度输出 → 行走速度。
    ///
    /// 两个来源相乘（口径见 `needs::bands` 与 `needs::coupling` 文档）：
    ///   - 耦合矩阵 `speed`（C-12：`energy < 20` → ×0.8）；
    ///   - 需求分档 `speedMul`（C-11 档位口径：`satiety < 20` → ×0.85）。
    fn apply_speed_mul(&mut self) {
        let coupling = self.emotion.speed_mul();
        let needs = self
            .emotion
            .needs()
            .effects(self.emotion.needs_cfg(), &self.emotion.state.values)
            .speed_mul;
        self.motion.set_speed_mul(coupling * needs);
    }

    /// 派遣门禁（S7-M3 / AC-30 / AC-31）：**矩阵级 ∪ 分档级**并集判定（`true` = 允许）。
    ///
    /// 消费点说明：派遣动作本身归 S8-M1（活动状态机），本方法交付**判定口径**供其调用；
    /// 矩阵与分档两处配置语义一致，取并集即「任一拒绝即拒绝」。
    #[must_use]
    pub fn dispatch_verdict(&self, kind: DispatchKind) -> bool {
        if !self.emotion.dispatch_verdict(kind) {
            return false;
        }
        !self
            .emotion
            .needs()
            .effects(self.emotion.needs_cfg(), &self.emotion.state.values)
            .denies_dispatch(kind)
    }

    /// 情绪算法事件落地（S4-M2 要点 1/3）。
    ///
    /// 两类处置：
    ///   - `ForceAction` → 动作仲裁（`02 §6.2`：`submit 情绪动作 优先级≥7`），
    ///     经 `ActionRequest::from_cfg(cfg, ActionSource::Emotion)` 构造；目录缺
    ///     该动作 / `disabled` / 优先级域外 → 降级不提交（不 panic）；
    ///   - 其余 → 经 [`wire_for_events`] 映射为 `pet://emotion`（阶段迁移）；
    ///     `ValuesChanged` 不产线上事件（快照走 ⑤）；
    ///     `PersistNow` 不产线上事件，转 [`Self::save_requested`]（存盘触发，S5-M1/M2）。
    fn dispatch_emotion_events(
        &mut self,
        events: &[EmotionEvent],
        wall_now_ms: i64,
        app: Option<&AppHandle>,
    ) {
        // S5-M1/M2：`PersistNow`（内核在阶段迁移 / 自然消气 / 三部曲结算等严重节点产出）
        // → 置「请强制落盘」标记，由本 tick 之后的 [`Self::save_tick`] 消费。
        // 该事件**不产线上广播**（`dp-core::event` 映射为 `None`），只是存盘触发信号。
        if events.iter().any(|ev| matches!(ev, EmotionEvent::PersistNow)) {
            self.save_requested = true;
        }

        // 强制动作（优先级 `02 §6.2` 要求 ≥7，由 `emotion.json.levels[].priorityFloor`
        // 保证；此处不再二次钳制，避免与配置真源双写）。
        for ev in events {
            let EmotionEvent::ForceAction { action_id, priority } = ev else {
                continue;
            };
            let Some(cfg) = self.catalog.find(action_id) else {
                eprintln!("[dp-app] core-loop 情绪动作 {action_id} 不在目录，降级不提交");
                continue;
            };
            let Some(request) = ActionRequest::from_cfg(cfg, ActionSource::Emotion) else {
                eprintln!("[dp-app] core-loop 情绪动作 {action_id} 不可用（disabled），降级不提交");
                continue;
            };
            let verdict = self.arbiter.submit(request, wall_now_ms.max(0) as u64);
            self.settle_play(verdict.clone(), action_id);
            eprintln!(
                "[dp-app] core-loop 情绪强制动作 {action_id}（priority={priority}）仲裁={verdict:?}"
            );
        }

        // 线上事件（`pet://emotion` / `pet://coax`）。
        for wire in wire_for_events(events, &self.emotion) {
            let Some(handle) = app else { continue };
            if let Err(err) = handle.emit(wire.event, &wire.payload) {
                eprintln!("[dp-app] core-loop 广播 {} 降级：{err}", wire.event);
            }
        }

        // S4-M5：气泡（`pet://bubble`，把算法事件翻译为「抽到的台词 + 语义字段」）。
        //
        // S5-M4 / FR-10-4：**勿扰模式静默气泡**——勿扰的前提是「暂停气泡与主动漫游，
        // 仅保留待机动画」。此处只掐**气泡产出**（不产事件即不弹），音频侧的静默由
        // `dp-audio::resolve_play` 的勿扰门控负责（S4-M6），两边口径一致、互不重复。
        if !(self.dnd && self.dnd_pause_bubbles) {
            for ev in events {
                let Some(plan) = self.derive_bubble(ev, wall_now_ms) else {
                    continue;
                };
                let wire = wire_for_bubble(&plan);
                let Some(handle) = app else { continue };
                if let Err(err) = handle.emit(wire.event, &wire.payload) {
                    eprintln!("[dp-app] core-loop 广播 {} 降级：{err}", wire.event);
                }
            }
        }

        // S4-M6：音效（`01 §6.5.5` 的已落地子集；门控 / 队列在 `dp-audio` 侧）。
        for ev in events {
            if let Some(cue) = audio_cue_for_emotion(ev) {
                self.request_audio(cue);
            }
        }
    }

    /// S4-M5：把一条算法事件翻译为气泡计划（冷却未过 → `None`）。
    ///
    /// 触发面（本卡口径）：
    ///   - 阶段迁移 → 池键取 `emotion.json.levels[to].linePool`（配置驱动，零硬编码）；
    ///   - 三部曲完成 → `runaway` 池（含 AC-04「哼…原谅你啦，下不为例！」文案）；
    ///   - 进入比心窗 → `happy` 池，`preempt = true`（用户交互台词即时覆盖系统台词）。
    ///
    /// 求助 / 提醒 / 明信片类气泡的**触发源**（需求阈值 / 提醒调度 / 活动状态）归
    /// S7-M2 / S10 / S8——本卡交付其冷却策略（[`BubblePlanner`]）与载荷通路。
    fn derive_bubble(&mut self, ev: &EmotionEvent, now_ms: i64) -> Option<PlannedBubble> {
        let pool = match ev {
            EmotionEvent::ColdLevelChanged { to, .. } => return self
                .bubble
                .bubble_for_level(&self.lines, self.emotion.cfg().levels.as_slice(), *to, &self.vars, now_ms),
            EmotionEvent::CoaxSucceeded { .. } => RUNAWAY_POOL,
            EmotionEvent::CoaxProgress { step: CoaxStep::Heart, .. } => HAPPY_POOL,
            // S7-M5 / AC-19：摸鱼专属提示——池键由内核按档位给出（配置驱动）。
            EmotionEvent::SlackLinger { pool, .. } => pool.as_str(),
            _ => return None,
        };
        let preempt = pool == HAPPY_POOL;
        self.bubble
            .bubble_for_pool(&self.lines, pool, &self.vars, now_ms, preempt)
    }

    /// S4-M6：请求播一条音效（未装配音频 / 被门控 / 队列满 → 静默降级，不 panic）。
    ///
    /// 诊断口径：被静默与溢出丢弃**只计数不刷日志**（音效是表现层，日志噪声远大于价值），
    /// 需要排查时读 [`AudioBus::suppressed_count`] / [`AudioBus::dropped_count`]。
    pub fn request_audio(&self, cue: AudioCue) -> dp_audio::RequestOutcome {
        let Some(bus) = self.audio.as_ref() else {
            return dp_audio::RequestOutcome::Closed;
        };
        bus.request(cue)
    }

    /// S4-M5：当前气泡计划器（只读；单测 / 诊断用）。
    #[inline]
    #[must_use]
    pub fn bubble_planner(&self) -> &BubblePlanner {
        &self.bubble
    }

    /// S4-M5：渲染一段台词（`{name}` 取配置默认名；单测 / 诊断用）。
    #[must_use]
    pub fn render_line(&self, text: &str) -> String {
        render_placeholders(text, &self.vars)
    }

    /// 广播 1Hz 全量快照（`pet://state`，载荷 [`PetSnapshotV2`]）。
    ///
    /// 载荷经 [`project_snapshot`] 投影（唯一生产者）；前端先行契约见
    /// `src/shared/ipc.ts` 的 `PetSnapshotV2`。无 `app`（纯逻辑模式）时不广播。
    fn emit_state_snapshot(&mut self, app: Option<&AppHandle>) {
        let Some(handle) = app else { return };
        // 性格文本 / 重掷次数归 S7 性格面板；本卡投影空串 / 0（字段存在，形状正确）。
        let mut snapshot = project_snapshot(&self.emotion, "", 0);
        // S8-M3：活动快照（进行中实例 → `activity` 段；前端 ActivityCard 消费；
        // `PetSnapshotV2.activity` 为 `Option<Value>`，不破坏既有契约）。
        snapshot.activity = self.activity_snapshot_json();
        // S10-M1：经济余额 + 背包并入快照（商城 / 背包 Tab 的唯一数据源；
        // 此前 economy/inventory 仅落盘、未回传前端）。
        snapshot.economy.coin = self.economy.balance();
        snapshot.inventory = self.economy.inventory_wire();
        // S10-M1：相册 + 桌面装饰 5 槽回传前端（只读；写经 pet_decor_place/remove）。
        if let Some(save) = &self.save {
            let cache = save.cache();
            snapshot.album = cache.album.clone();
            snapshot.decor = cache
                .decor
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![serde_json::Value::Null; dp_core::save::schema::DECOR_SLOTS]);
        }
        if let Err(err) = handle.emit(STATE_EVENT, &snapshot) {
            eprintln!("[dp-app] core-loop 广播 {STATE_EVENT} 降级：{err}");
        }
    }

    /// 当前快照（`#[cfg(test)]` 探针：验证契约而非广播路径）。
    #[cfg(test)]
    pub(crate) fn snapshot_for_test(&self) -> PetSnapshotV2 {
        project_snapshot(&self.emotion, "", 0)
    }

    /// 气泡派生探针（`#[cfg(test)]`：验证「算法事件 → 台词池 → 渲染后的文案」链路，
    /// 而不必构造 Tauri `AppHandle`）。
    #[cfg(test)]
    pub(crate) fn derive_bubble_for_test(
        &mut self,
        ev: &EmotionEvent,
        now_ms: i64,
    ) -> Option<PlannedBubble> {
        self.derive_bubble(ev, now_ms)
    }

    /// 业务档推进（**测试专用**：强制 `session_paused = false`，绕开平台会话查询）。
    ///
    /// 存在的理由见 [`Self::business_tick_inner`] 的文档：本机无头环境下平台查询恒为
    /// `Disconnected`，若不绕开则无法在单测中触达「在场 / 离场因子」这条路径。
    #[cfg(test)]
    fn business_tick_present(&mut self, wall: &dyn WallClock) {
        let now = wall.now_ms();
        self.business_tick_inner(wall, now, false, false, None);
    }

    /// 当前权威 `pos`（不变量 B）。
    fn active_pos(&self) -> Vec2 {
        match self.phase {
            Phase::Roam => self.motion.pos(),
            Phase::Fall => self.physics.as_ref().map_or(self.pos, |p| p.pos()),
            Phase::Drag => self.pos,
        }
    }

    /// `Landed{impact}` → `ACT-M-06` → `ActionRequest::from_cfg(Motion)` → `arbiter.submit`。
    ///
    /// 目录缺该动作 / `disabled` / 优先级域外 → `None`（降级，不 panic）。
    /// 起播型仲裁结果（`Play` / `Interrupt`）经 `settle_play` 下发覆盖态起播（B15-④）。
    fn submit_landing(&mut self, now_ms: u64) -> Option<Arbitration> {
        let cfg = self.catalog.find(LANDING_ACTION_ID)?;
        let request = ActionRequest::from_cfg(cfg, ActionSource::Motion)?;
        let verdict = self.arbiter.submit(request, now_ms);
        self.settle_play(verdict.clone(), LANDING_ACTION_ID);
        Some(verdict)
    }

    /// 进入拖拽相（S3-M4：`DragStart` 意图接线点，run_loop 每 logic tick 调用）。
    ///
    /// 已在 Drag 相：幂等——只刷新钳制后光标并同步权威 `pos`，返回 `None`
    /// （不重复提交动作）；其余相：两引擎停放（Fall 相来的 `PhysicsEngine` 直接
    /// 丢弃，不变量 A）、`phase = Drag`、`thrown_flight` 清零（飞行中抓回 → 落地链
    /// 作废），并按幂等口径补提交 ACT-T-06「抛物线翻滚」挂光标（id 唯一真源 =
    /// [`act_of`]`(`[`InteractionKind::DragStart`]`)`）：current 已是 ACT-T-06 时跳过
    /// （空中抓回时 toss 仍在播，不重复入队污染队列），其余情况照常提交。
    ///
    /// 幂等口径与 [`Self::release_drag`] 一致，均经 `ensure_toss_submitted`；
    /// 目录缺失则降级为不提交。
    ///
    /// 返回 toss 补提交的仲裁结果（仅当在播非 ACT-T-06 且目录可用时非空）。
    pub fn begin_drag(&mut self, cursor: Vec2, now_ms: u64) -> Option<Arbitration> {
        if self.phase == Phase::Drag {
            let clamped = self.clamp_drag_target(cursor);
            self.drag_cursor = Some(clamped);
            self.pos = clamped;
            return None;
        }
        self.physics = None;
        self.phase = Phase::Drag;
        self.thrown_flight = false;
        let clamped = self.clamp_drag_target(cursor);
        self.drag_cursor = Some(clamped);
        self.pos = clamped;
        self.ensure_toss_submitted(now_ms)
    }

    /// Drag 相跟手（S3-M4：只暂存光标，钳制与权威 `pos` 刷新统一在 logic_tick
    /// Drag 臂完成；非 Drag 相调用为无操作）。
    pub fn drag_to(&mut self, cursor: Vec2) {
        if self.phase == Phase::Drag {
            self.drag_cursor = Some(cursor);
        }
    }

    /// 结束拖拽并结算（S3-M4：`Throw` 意图接线点）。
    ///
    /// `Some(vel)`（甩出）：初速度经 [`Self::clamp_throw_velocity`] 钳制（保证
    /// `THROW_MAX_FLIGHT_MS` 内落地）后以 [`PhysicsEngine::thrown`] 构造抛物线下坠态，
    /// `phase = Fall`、`thrown_flight = true`；仲裁器在播的不是 ACT-T-06 时补提交
    /// （toss 缺席的退化场景，如 DragStart 丢失）。
    ///
    /// `None`（纯松手）：仅 Drag 相有意义——落点为合法站立面 → 直接
    /// `reenter_roam`（无落地演出）；悬空 → `PhysicsEngine::new` 自然下坠（落地走
    /// ACT-M-06）；非 Drag 相调用为无操作。
    ///
    /// Throw 直达（非 Drag 相，如同批互斥序 Throw 先于 DragStart 的竞态）：以当前
    /// 权威位为起飞点走甩出分支。返回本次补提交 toss 的仲裁结果（无补提交时 `None`）。
    pub fn release_drag(&mut self, throw_vel: Option<Vec2>, now_ms: u64) -> Option<Arbitration> {
        let cursor = if self.phase == Phase::Drag {
            self.drag_cursor.take().unwrap_or(self.pos)
        } else {
            self.pos
        };
        match throw_vel {
            Some(raw_vel) => {
                let vel = self.clamp_throw_velocity(raw_vel);
                let bounds = ports::vd_bounds_of(&self.monitors);
                self.physics = Some(PhysicsEngine::thrown(
                    cursor,
                    vel,
                    self.interaction_cfg.clone(),
                    bounds,
                ));
                self.pos = cursor;
                self.phase = Phase::Fall;
                self.thrown_flight = true;
                // S4 B15-④：起飞即停播在播的循环 toss（画面转甩出物理飞行）。
                // 仲裁器 current **保留**（toss 仍是「在播」语义，链首 07 差 0
                // 优先级仍依序排队）。
                if self.current_is_looping() {
                    self.emit_stop();
                }
                // 退化补提交（toss 缺席，如 DragStart 丢失）；toss 已 current 时
                // **不发 Play**——会抵消刚发的 Stop（B15-④ 裁定，重挂仅归 begin_drag）。
                let toss_id = act_of(InteractionKind::DragStart);
                if !self.arbiter.current().is_some_and(|a| a.request.id == toss_id) {
                    return self.submit_toss(now_ms);
                }
                None
            }
            None => {
                if self.phase != Phase::Drag {
                    return None;
                }
                let landable = {
                    let graph = self.platform_graph.read().unwrap_or_else(|e| e.into_inner());
                    graph.is_valid_stand(cursor)
                };
                if landable {
                    self.pos = cursor;
                    self.reenter_roam(now_ms);
                    self.thrown_flight = false;
                    return None;
                }
                // 悬空松手：自然下坠（无初速），落地走常规 ACT-M-06。
                let bounds = ports::bounds_of(&self.monitors, cursor);
                self.physics =
                    Some(PhysicsEngine::new(cursor, self.interaction_cfg.clone(), bounds));
                self.pos = cursor;
                self.phase = Phase::Fall;
                self.thrown_flight = false;
                None
            }
        }
    }

    /// 换相回 Roam（不变量 C：以离场位为入场 pos 重建 `MotionEngine`，seed_k 递推）。
    ///
    /// 从 `logic_tick` 的 Landed 分支与 `release_drag` 的合法落点分支复用；**不**动
    /// `thrown_flight`（结算方读后自行清零，避免隐藏状态转移）。
    fn reenter_roam(&mut self, now_ms: u64) {
        self.seed_k = advance_seed(self.seed_k, now_ms);
        // S7-M3 前置首件：重建时同样注入共享平台图（决策层口径与首次装配一致）。
        self.motion = MotionEngine::new_with_surface(
            self.pos,
            self.monitors.clone(),
            self.roam_cfg.clone(),
            self.seed_k,
            now_ms,
            Box::new(SharedSurface(Arc::clone(&self.platform_graph))),
        );
        self.motion.set_cursor(self.cursor);
        self.pos = self.motion.pos();
        self.phase = Phase::Roam;
        self.physics = None;
    }

    /// 拖拽挂光标动作提交（ACT-T-06，id 唯一真源 = `act_of(InteractionKind::DragStart)`）。
    ///
    /// 目录缺失 / `disabled` / 优先级域外 → `None`（降级，不 panic）。
    /// 起播型仲裁结果经 `settle_play` 下发覆盖态起播（B15-④）。
    fn submit_toss(&mut self, now_ms: u64) -> Option<Arbitration> {
        let toss_id = act_of(InteractionKind::DragStart);
        let cfg = self.catalog.find(toss_id)?;
        let request = ActionRequest::from_cfg(cfg, ActionSource::Interaction)?;
        let verdict = self.arbiter.submit(request, now_ms);
        self.settle_play(verdict.clone(), toss_id);
        Some(verdict)
    }

    /// 甩出起飞时确保 ACT-T-06 在播（退化补提交；已在播则重挂覆盖态并返回 `None`）。
    ///
    /// S4 B15-④：toss 已 current（空中抓回）→ 重发 `Play` 重挂播放器覆盖态——
    /// 播放器侧可能已因上一次 Stop 回落轮播，拖拽演出须持续跟手，故此处**重挂**。
    /// （甩出路径 `release_drag` 不走本方法的重挂分支，见其内联裁定。）
    fn ensure_toss_submitted(&mut self, now_ms: u64) -> Option<Arbitration> {
        let toss_id = act_of(InteractionKind::DragStart);
        if self.arbiter.current().is_some_and(|a| a.request.id == toss_id) {
            self.emit_play(toss_id);
            return None;
        }
        self.submit_toss(now_ms)
    }

    /// 甩出落地链提交（FR-4-6；[`THROW_LANDING_CHAIN`] 顺序 = ACT-T-07 → ACT-T-08）。
    ///
    /// 链首 07 走 `force_submit`（仅免 500ms 打断冷却；面对可打断的 toss 07 差 0
    /// 优先级 → 排队 `Queued`）；链尾 08 常规 `submit` 依序入队。起播型仲裁结果
    /// （current 空的退化路径）经 `settle_play` 下发覆盖态（B15-④）；入队型结果
    /// 的出队起播由停播推进 / `Finished` 回报驱动。
    ///
    /// 返回链首的仲裁结果（`LogicOutcome::submitted` 槽位语义 = 单条，取链首）。
    fn submit_throw_landing_chain(&mut self, now_ms: u64) -> Option<Arbitration> {
        let head = self
            .catalog
            .find(THROW_LANDING_CHAIN[0])
            .and_then(|cfg| ActionRequest::from_cfg(cfg, ActionSource::Interaction));
        let head_result = head.map(|request| {
            let verdict = self.arbiter.force_submit(request, now_ms);
            self.settle_play(verdict.clone(), THROW_LANDING_CHAIN[0]);
            verdict
        });
        if let Some(tail) = self
            .catalog
            .find(THROW_LANDING_CHAIN[1])
            .and_then(|cfg| ActionRequest::from_cfg(cfg, ActionSource::Interaction))
        {
            let tail_result = self.arbiter.submit(tail, now_ms);
            self.settle_play(tail_result.clone(), THROW_LANDING_CHAIN[1]);
            eprintln!(
                "[dp-app] core-loop 甩出落地链尾 {}：仲裁={tail_result:?}",
                THROW_LANDING_CHAIN[1]
            );
        }
        head_result
    }

    /// 甩出初速度钳制（FR-4-6：保证 `PhysicsEngine::thrown` 在 `THROW_MAX_FLIGHT_MS`
    /// 内落地；dp-app 持有 monitors 几何，是唯一能算「最大可落高度」的层）。
    ///
    /// 闭式解推导（VDC y 向下，向上初速 v = −vel.y；g 取 `interaction_cfg` 单一真源
    /// RV-18，非法回退默认——**本文件禁重力数值字面量**）：上升 t_up = v/g、上升高度
    /// h_up = v²/(2g)；最坏落差 h = 虚拟桌面垂直跨度（`ports::vd_vertical_span`）。
    /// 落地约束 t_up + √(2(h_up + h)/g) ≤ T_eff，两边平方整理得
    /// `v ≤ g·T_eff/2 − h/T_eff`，其中 `T_eff = THROW_MAX_FLIGHT_MS − THROW_FLIGHT_MARGIN_MS`。
    ///
    /// 只钳向上分量：向下分量越大落地越快（physics 落地检测为「y ≥ 站立面顶」的
    /// 钳制式判定 + x 已被 `vd_bounds_of` 收进工作区水平界 → 任意向下速度都有面可落）；
    /// 水平分量由左右墙反弹兜住，不影响飞行时长。
    fn clamp_throw_velocity(&self, vel: Vec2) -> Vec2 {
        let g_raw = self.interaction_cfg.gravity_px_per_sec2;
        let g = if g_raw.is_finite() && g_raw > 0.0 {
            g_raw
        } else {
            InteractionCfg::default().gravity_px_per_sec2
        };
        let (top, bottom) = ports::vd_vertical_span(&self.monitors);
        let h = if top.is_finite() && bottom.is_finite() && bottom > top {
            bottom - top
        } else {
            0.0
        };
        let t_eff = (THROW_MAX_FLIGHT_MS.saturating_sub(THROW_FLIGHT_MARGIN_MS)) as f32 / 1000.0;
        // 向上允许的最大初速（px/s）；几何退化时为 0（不放行任何向上分量，保守）。
        let v_up_max = (g * t_eff / 2.0 - h / t_eff).max(0.0);
        let vy = if vel.y < -v_up_max { -v_up_max } else { vel.y };
        Vec2::new(vel.x, vy)
    }

    /// Drag 相光标钳制（x → 全工作区并集水平界，y → 虚拟桌面垂直跨度；FR-4-5）。
    ///
    /// 非有限输入（NaN / ±Inf，钩子层异常透传的防御）保持原位并落 C8 诊断日志；
    /// 无显示器时钳制域退化为 (−∞, +∞)（`ports::vd_*` 已防御）→ 恒等。
    fn clamp_drag_target(&self, cursor: Vec2) -> Vec2 {
        if !cursor.x.is_finite() || !cursor.y.is_finite() {
            eprintln!(
                "[dp-app] core-loop 拖拽光标非有限坐标，保持原位：({},{})",
                cursor.x, cursor.y
            );
            return self.pos;
        }
        let bounds = ports::vd_bounds_of(&self.monitors);
        let (top, bottom) = ports::vd_vertical_span(&self.monitors);
        Vec2::new(cursor.x.clamp(bounds.left, bounds.right), cursor.y.clamp(top, bottom))
    }
}

/// 右键单击命中 → `pet://menu` 广播（S3-M6；core-loop logic 档内调用，失败降级）。
///
/// 坐标换算（RV-17 口径）：命中点屏幕物理 − 窗口物理左上，除以命中点所在屏
/// DPI 因子 = 窗口内 CSS 坐标（[`bridge::menu_cmd`] 纯函数承载，单测覆盖）。
/// 窗口矩形不可用时降级为窗口中心锚点 (128, 128)（前端 `clampMenuPlacement`
/// 会钳回容器内，退化不越界弹出）。
fn emit_menu_events(
    app: &AppHandle,
    pet: &PetPlatform,
    hits: Vec<crate::interaction_consumer::RightClickHit>,
) {
    for hit in hits {
        let cmd = match pet.window.window_physical_rect() {
            Ok(rect) => {
                let display = pet.platform.display();
                // physical_to_vdc 产出平台 Vec2（monitor_at 直收，无需 ports 换算）。
                let vdc = display.physical_to_vdc(hit.x, hit.y);
                let scale = display.monitor_at(vdc).scale;
                bridge::menu_cmd(rect.left, rect.top, scale, hit.x, hit.y)
            }
            Err(err) => {
                eprintln!(
                    "[dp-app] core-loop 菜单命中坐标换算降级（窗口矩形不可用：{err}）：使用中心锚点"
                );
                bridge::MenuCmd {
                    screen_x: f64::from(hit.x),
                    screen_y: f64::from(hit.y),
                    local_x: 128.0,
                    local_y: 128.0,
                    ..bridge::MenuCmd::default()
                }
            }
        };
        if let Err(err) = app.emit(MENU_EVENT, &cmd) {
            eprintln!("[dp-app] core-loop 广播 {MENU_EVENT} 降级：{err}");
        }
    }
}

/// 显示器几何是否变化（集合语义：id 集合 + 每屏几何 / 主屏标记）。
#[must_use]
fn monitors_changed(prev: &[MonitorGeomAlias], now: &[MonitorGeomAlias]) -> bool {    if prev.len() != now.len() {
        return true;
    }
    prev.iter().any(|p| match now.iter().find(|n| n.id == p.id) {
        None => true,
        Some(n) => {
            p.origin_vdc != n.origin_vdc
                || p.size_vdc != n.size_vdc
                || p.work_origin_vdc != n.work_origin_vdc
                || p.work_size_vdc != n.work_size_vdc
                || p.primary != n.primary
        }
    })
}

// ---------------------------------------------------------------------------
// 三档 deadline 绝对锚定网格（纯逻辑，可单测）
// ---------------------------------------------------------------------------

/// 本轮到期的档位集合。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TiersFired {
    /// render 档是否到期。
    pub render: bool,
    /// logic 档是否到期。
    pub logic: bool,
    /// 业务档是否到期。
    pub biz: bool,
}

/// 三档绝对锚定调度网格：每档 deadline 恒为 `start + k×间隔`，**禁止逐 tick 累加**。
///
/// `advance(now)` 一次推进所有已到期档（同一轮多档同时到期则全做）；若上层单次迟到超过
/// 一个周期，`advance` 以固定间隔逐档追赶（deadline 链不因迟到而漂移，AC-11 / 提案要点 1）。
#[derive(Clone, Copy, Debug)]
pub struct ScheduleGrid {
    render_interval_ms: u64,
    logic_interval_ms: u64,
    biz_interval_ms: u64,
    next_render: u64,
    next_logic: u64,
    next_biz: u64,
}

impl ScheduleGrid {
    /// 以锚定起点 `start_ms` 与三档间隔构造（各档首 deadline = `start + 间隔`）。
    #[must_use]
    pub fn new(
        start_ms: u64,
        render_interval_ms: u64,
        logic_interval_ms: u64,
        biz_interval_ms: u64,
    ) -> Self {
        let render = render_interval_ms.max(1);
        let logic = logic_interval_ms.max(1);
        let biz = biz_interval_ms.max(1);
        Self {
            render_interval_ms: render,
            logic_interval_ms: logic,
            biz_interval_ms: biz,
            next_render: start_ms.saturating_add(render),
            next_logic: start_ms.saturating_add(logic),
            next_biz: start_ms.saturating_add(biz),
        }
    }

    /// render 档下一 deadline（调试 / 单测视图）。
    #[must_use]
    pub const fn next_render(&self) -> u64 {
        self.next_render
    }

    /// logic 档下一 deadline（调试 / 单测视图）。
    #[must_use]
    pub const fn next_logic(&self) -> u64 {
        self.next_logic
    }

    /// 业务档下一 deadline（调试 / 单测视图）。
    #[must_use]
    pub const fn next_biz(&self) -> u64 {
        self.next_biz
    }

    /// 当前 render 档间隔（毫秒）。
    #[must_use]
    pub const fn render_interval(&self) -> u64 {
        self.render_interval_ms
    }

    /// 三档中最早的 deadline（供 `sleep_until`）。
    #[must_use]
    pub fn min_deadline(&self) -> u64 {
        self.next_render.min(self.next_logic).min(self.next_biz)
    }

    /// 推进所有已到期档（绝对锚定：每次 `+= 固定间隔`，与实际触发时刻无关）。
    pub fn advance(&mut self, now_ms: u64) -> TiersFired {
        let mut fired = TiersFired::default();
        while now_ms >= self.next_render {
            fired.render = true;
            let next = self.next_render.saturating_add(self.render_interval_ms);
            if next == self.next_render {
                break;
            }
            self.next_render = next;
        }
        while now_ms >= self.next_logic {
            fired.logic = true;
            let next = self.next_logic.saturating_add(self.logic_interval_ms);
            if next == self.next_logic {
                break;
            }
            self.next_logic = next;
        }
        while now_ms >= self.next_biz {
            fired.biz = true;
            let next = self.next_biz.saturating_add(self.biz_interval_ms);
            if next == self.next_biz {
                break;
            }
            self.next_biz = next;
        }
        fired
    }

    /// render 档间隔热切换（K-4 档位自适应）：变更即重锚 render 网格（相位重置，同 bridge 口径）。
    pub fn set_render_interval(&mut self, interval_ms: u64, now_ms: u64) {
        let interval = interval_ms.max(1);
        if interval == self.render_interval_ms {
            return;
        }
        self.render_interval_ms = interval;
        self.next_render = now_ms.saturating_add(interval);
    }
}

// ---------------------------------------------------------------------------
// Tauri 外壳：装配 + 三档 master loop + 感知采样线程
// ---------------------------------------------------------------------------

/// 启动 core-loop 执行体（`dp-core-loop` 线程）+ 感知采样线程（`dp-perception`）。
///
/// 口径同 `supervisor::spawn`：**故意不标 `#[must_use]`**，线程启动失败降级空句柄、
/// 不 panic 主线程。`bus` / `hit` / `sink` 为跨线程共享端口：`PerceptionBus` 单生产者
/// 语义、`HitLatestHandle` 无锁三态读（S3-M2，写侧 = 本层 render 档 + bridge 播放器）、
/// `ChannelSink` 钩子事件队列（S3-M3 本层为唯一消费端）。
pub fn spawn(
    app: AppHandle,
    bus: Arc<PerceptionBus<PerceptionEvent>>,
    hit: HitLatestHandle,
    sink: Arc<ChannelSink>,
) -> JoinHandle<()> {
    let Some(mut state) = build_state(&app) else {
        eprintln!("[dp-app] core-loop 未装配（平台状态不可用），降级空循环");
        return empty_handle();
    };
    // S4 前清障 B15-④：播放指令通道（装配点 lib.rs 须先 manage；未注册 → 不接，
    // 发播方法全部 no-op，行为与 S3 完全一致）。
    if let Some(channel) = app.try_state::<PlaybackChannel>().map(|c| (*c).clone()) {
        state.attach_playback(channel);
        eprintln!("[dp-app] core-loop 播放指令通道已接入（出队起播/停播/链推进）");
    }
    // 感知采样线程（单线程三档分档；`offer` 单一生产者语义，不得多线程 offer）。
    let _perception = spawn_perception(app.clone(), Arc::clone(&bus), sink.clone());

    // S3-M3：手势消费端（队列 drain → 手势机 → 意图日志+计数；仅本线程驱动）。
    let consumer = InteractionConsumer::new(sink, state.gesture_cfg());

    let builder = std::thread::Builder::new().name("dp-core-loop".to_string());
    match builder.spawn(move || run_loop(app, state, bus, hit, consumer)) {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("[dp-app] core-loop 线程启动失败，降级为空循环：{err}");
            empty_handle()
        }
    }
}

/// 线程启动失败的降级句柄（立即结束的空线程；同 `supervisor::spawn` 口径）。
fn empty_handle() -> JoinHandle<()> {
    std::thread::spawn(|| {})
}

/// 从 `app` 状态 + 配置装核心状态（读窗口/显示器取初始 `pos`）。
fn build_state(app: &AppHandle) -> Option<CoreLoopState> {
    let pet = app.try_state::<PetPlatform>()?;
    let display = pet.platform.display();
    let bundle = load_config(app);
    let lines = load_lines(app);
    let monitors = ports::to_monitor_geoms(display.monitors());
    let pos = initial_pos(&pet.window, &display, &monitors);
    // 播种：非节拍（C3 三段口径——PRNG 种子非业务时间，从墙钟端口取毫秒摘取）。
    let seed = SystemWallClock.now_ms() as u64;
    // S4-M6：音效总线（`lib.rs` 已在 setup 期 `manage`）。取用时先同步一次设置快照
    // （`settings.json.audio` + `behavior`），避免启动瞬间用默认音量播第一条音效。
    let audio = app.try_state::<AudioBus>().map(|bus| {
        bus.set_settings(audio_settings_from(&bundle));
        bus.inner().clone()
    });
    // S5-M1：存档装载（含损坏 / 未来版本 / 待迁移三级降级链）。失败 / 不可解析目录 →
    // `None`（不载档不落盘），行为退回 S4 基线（`02 §7.4.2`：降级不崩）。
    let save = resolve_save_dir(app).map(|dir| {
        let (store, outcome) = SaveStore::load(&dir, SystemWallClock.now_ms());
        log_save_outcome(&outcome);
        // S5-M4：把加载落点登记进「数据」Tab 的查询句柄（`save_status` 命令的真源）。
        // 这是 S5-M1 裁定 ④「用户可见提示」的可观测面——设置页据此显示健康状态与提示条。
        if let Some(handle) = app.try_state::<bridge::SaveStatusHandle>() {
            handle.set(bridge::SaveStatusInfo {
                state: save_state_key(&outcome.status),
                healthy: outcome.is_healthy(),
                needs_notice: outcome.needs_notice(),
                writable: store.is_writable(),
                last_seen_ms: store.cache().meta.last_seen_ms,
            });
        }
        store
    });
    let cfg = CoreCfg {
        roam_cfg: bundle.settings.roam.clone(),
        interaction_cfg: bundle.settings.interaction.clone(),
        catalog: ActionCatalog::from_config(&bundle.actions),
        emotion: bundle.emotion.clone(),
        needs: bundle.needs.clone(),
        character_default_name: bundle.character.default_name.clone(),
        character_catchphrase: Some(bundle.character.catchphrase.clone()),
        lines,
        audio,
        save,
        // S8-M1：活动全局配置（`activities.json.global`；派遣 / 结算 / 明信片口径）。
        activities: bundle.activities.activity.clone(),
        shop: bundle.shop.clone(),
        achievements: bundle.achievements.clone(),
    };
    let mut state = CoreLoopState::new(pos, monitors, cfg, seed, 0);
    // S5-M4：设置状态（应用层唯一设置真源）由配置束 ⊕ 存档 B 段推导，装配后
    // `manage` 进 Tauri 状态管理器（设置窗口的 `settings_get` / 热更新都读它）。
    let snapshot = bridge::build_settings_snapshot(&bundle, state.save_ref());
    state.apply_settings_snapshot(&snapshot);
    // schedule.json 的勿扰行为位（`01 FR-10-4`：勿扰静默气泡，与 S4-M6 音频门控同口径）。
    state.apply_schedule_flags(bundle.schedule.do_not_disturb.pause_bubbles);
    app.manage(bridge::SettingsState::new(snapshot));
    // S5-M2：离线补偿（**必须在首个业务 tick 之前**——`restore` 已把起点置为存档里的
    // `lastTickMs`，若先跑首拍则离线时长被压缩成 1 秒；见 `compensate_offline` 文档）。
    // 无存档 / 无离线 → `None`，不做任何演出（全新安装路径）。
    let _ = state.compensate_offline(&SystemWallClock);
    Some(state)
}

/// 存档加载落点 → 词表名（`save_status` 与设置页「数据」Tab 共用；与 `LoadStatus` 一一对应）。
fn save_state_key(status: &dp_core::save::LoadStatus) -> String {
    match status {
        dp_core::save::LoadStatus::Fresh => "fresh",
        dp_core::save::LoadStatus::Loaded => "loaded",
        dp_core::save::LoadStatus::RecoveredFromBak { .. } => "recoveredFromBak",
        dp_core::save::LoadStatus::IsolatedCorrupt { .. } => "isolatedCorrupt",
        dp_core::save::LoadStatus::IsolatedFuture { .. } => "isolatedFuture",
        dp_core::save::LoadStatus::MigrationPending { .. } => "migrationPending",
    }
    .to_string()
}

/// 记录存档装载结果（S5-M1：降级链的可观测面）。
///
/// ⚠️ **已知覆盖缺口（登记待下游）**：`02 §5 K-7` 要求损坏档「重建默认档**并提示**」，
/// 本卡只做到「隔离 + 重建 + 日志」。用户可见提示需要一处 UI 承载，而 `pet://` 事件面
/// （C8）**没有**「存档状态」事件、托盘也无通知 API，故归 **S5-M3**（设置页「数据」Tab
/// 展示存档健康状态 + 导入 `save.corrupt.*`）与 **S6-M2**（自愈守护巡检）。此处日志
/// 保留隔离路径，用户可据此手工回捞原档。
fn log_save_outcome(outcome: &LoadOutcome) {
    for warning in &outcome.warnings {
        eprintln!("[dp-app] core-loop 存档告警：{warning}");
    }
    let line = outcome.describe();
    if outcome.needs_notice() {
        eprintln!("[dp-app] core-loop 存档需要提示（可见提示归 S5-M3/S6-M2）：{line}");
    } else {
        eprintln!("[dp-app] core-loop 存档正常：{line}");
    }
}

/// S8-M1：`HH:MM` 配置串 → 本地小时（`quietHours.from/to`；解析失败取 0，配置坏不崩）。
fn hour_of_hhmm(hhmm: &str) -> u8 {
    hhmm.split(':').next().and_then(|h| h.parse::<u8>().ok()).unwrap_or(0)
}

/// 由配置束投影音频设置快照（S4-M6：`settings.json.audio` + `behavior` 的静音相关位）。
fn audio_settings_from(bundle: &ConfigBundle) -> AudioSettings {
    AudioSettings {
        master_volume_percent: bundle.settings.audio.master_volume_percent,
        muted: bundle.settings.audio.muted,
        do_not_disturb: bundle.settings.behavior.do_not_disturb,
        click_through: bundle.settings.behavior.click_through,
    }
}

/// 初始 `pos`：窗口物理矩形中心 → VDC（RV-17）；失败退回主屏工作区中心。
fn initial_pos(
    window: &WinPlatformWindow,
    display: &DisplayService,
    monitors: &[MonitorGeomAlias],
) -> Vec2 {
    if let Ok(rect) = window.window_physical_rect() {
        let c = rect.center();
        let vdc =
            ports::to_core_vec2(display.physical_to_vdc(c.x.round() as i32, c.y.round() as i32));
        if vdc.x.is_finite() && vdc.y.is_finite() {
            return vdc;
        }
    }
    monitors
        .iter()
        .find(|m| m.primary)
        .or_else(|| monitors.first())
        .map_or(Vec2::ZERO, |m| m.work_center())
}

/// 加载配置束（六份 JSON 经 [`ConfigService::load_all`]）；失败降级内置默认
/// （C1 无盘符字面量；R19：加载失败不崩）。
///
/// 返回整份 [`ConfigBundle`]（S4-M5 起需要 `character.json` 的 `defaultName`、
/// S4-M6 起需要 `settings.json` 的 `audio`/`behavior`），由 [`build_state`] 按需取用。
fn load_config(app: &AppHandle) -> ConfigBundle {
    if let Some(dir) = resolve_config_dir(app) {
        match ConfigService::load_all(&dir) {
            Ok((bundle, warnings)) => {
                if !warnings.is_empty() {
                    eprintln!("[dp-app] core-loop 配置加载告警 {} 条：{:?}", warnings.len(), warnings);
                }
                return bundle;
            }
            Err(err) => eprintln!("[dp-app] core-loop 配置加载降级：{err}"),
        }
    } else {
        eprintln!("[dp-app] core-loop 未发现配置目录，使用内置默认");
    }
    ConfigBundle::default()
}

/// 加载台词库（`resources/config/lines.json`，**S4-M5**）。
///
/// 降级口径（R19 同源）：文件缺失 / JSON 损坏 → 空库 + 告警 —— 只少气泡，不崩、不阻断启动。
/// 与 `character.json` 的交叉校验（`linePools.count=13` / 每池 ≥6 条 / 口头禅分布，L-02）
/// 在此**只告警不阻断**（内容问题不应让桌面宠物起不来），全绿校验由 `lines.rs` 单测保证。
fn load_lines(app: &AppHandle) -> LinesLibrary {
    let Some(dir) = resolve_config_dir(app) else {
        eprintln!("[dp-app] core-loop 未发现配置目录，台词库取空库");
        return LinesLibrary::empty();
    };
    match LinesLibrary::load_dir(&dir) {
        Ok(lib) => {
            // 交叉校验需要 character.json 的 linePools / catchphrase；此处再读一次（KB 级、
            // 仅装配期一次），避免把 `load_one` 的内部形态暴露给装配层。
            match std::fs::read_to_string(dir.join("character.json"))
                .map_err(|err| err.to_string())
                .and_then(|text| {
                    serde_json::from_str::<dp_core::config::model::CharacterConfig>(&text)
                        .map_err(|err| err.to_string())
                }) {
                Ok(character) => {
                    if let Err(err) = lib.validate(&character.line_pools, &character.catchphrase) {
                        eprintln!("[dp-app] core-loop 台词库交叉校验告警：{err}");
                    }
                }
                Err(err) => {
                    eprintln!("[dp-app] core-loop 台词库跳过交叉校验（character.json 不可用：{err}）");
                }
            }
            lib
        }
        Err(err) => {
            eprintln!("[dp-app] core-loop 台词库加载降级为空库：{err}");
            LinesLibrary::empty()
        }
    }
}

/// 解析配置目录（`<resource_dir>/resources/config`；dev 兜底工程根相对路径，C1 无盘符字面量）。
fn resolve_config_dir(app: &AppHandle) -> Option<PathBuf> {
    let root = app.path().resource_dir().ok()?;
    let candidates = [
        root.join("resources").join("config"),
        root.join("config"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/config"),
    ];
    candidates.into_iter().find(|c| c.join("actions.json").is_file())
}

/// core-loop 循环体：三档 deadline 合并 `sleep_until(min)`，到期档依次全做（单线程 Actor 单写者）。
fn run_loop(
    app: AppHandle,
    mut state: CoreLoopState,
    bus: Arc<PerceptionBus<PerceptionEvent>>,
    hit: HitLatestHandle,
    consumer: InteractionConsumer,
) {
    // 墙钟端口（仅业务 1Hz 档使用；不进节拍，C3）。
    let wall: Arc<dyn WallClock> = Arc::new(SystemWallClock);
    let start = Instant::now();
    let mut grid =
        ScheduleGrid::new(0, RENDER_DEFAULT_INTERVAL_MS, LOGIC_INTERVAL_MS, BIZ_INTERVAL_MS);
    let mut seen_render = false;
    let mut seen_logic = false;
    let mut seen_biz = false;
    // B17-②（2026-09-13）：启动首拍锚定位是否已写入窗口（loop 外局部，写成功才置位）。
    let mut anchored_written = false;

    eprintln!(
        "[dp-app] core-loop 启动：render={RENDER_DEFAULT_INTERVAL_MS}ms logic={LOGIC_INTERVAL_MS}ms biz={BIZ_INTERVAL_MS}ms"
    );

    loop {
        sleep_until(start, grid.min_deadline());
        let now = start.elapsed().as_millis() as u64;
        let fired = grid.advance(now);

        if fired.logic {
            if !seen_logic {
                seen_logic = true;
                eprintln!("[dp-app] core-loop logic 档起跳：interval={LOGIC_INTERVAL_MS}ms");
            }
            // S3-M3/M4：手势消费（drain 钩子队列 → 手势机 → 意图收集）。
            // 纯队列消费，不依赖 pet 状态（独立于下方窗口写者；20Hz 节拍内微秒级）。
            let intents = consumer.drain_collect(now).1;
            if let Some(pet) = app.try_state::<PetPlatform>() {
                let display = pet.platform.display();
                state.apply_monitors(ports::to_monitor_geoms(display.monitors()), now);

                // drain 感知积压（20Hz 光标 / 0.5Hz 窗口 / 0.1Hz 系统）。
                let mut evs = Vec::new();
                bus.drain(&mut evs);
                state.apply_perception(evs);

                // S3-M4 Drag 相跟手：begin_drag 定锚后逐 tick 以最新感知光标跟随
                // （钳制统一在 logic_tick Drag 臂完成，此处只注入原始采样）。
                if state.phase() == Phase::Drag {
                    if let Some(cur) = state.cursor() {
                        state.drag_to(cur);
                    }
                }

                // S3-M4 意图接线（感知光标已就位、钳制几何已刷新；DragStart / Throw
                // 之外的意图在 drain_collect 内仍只日志+计数，不投仲裁器）。
                for intent in &intents {
                    match intent.kind {
                        InteractionKind::DragStart => {
                            let cursor = state.cursor().unwrap_or(state.pos());
                            let sub = state.begin_drag(cursor, now);
                            eprintln!("[dp-app] core-loop DragStart → 拖拽相；toss 仲裁={sub:?}");
                        }
                        InteractionKind::Throw => {
                            // 初速度口径桥（C6/RV-17）：手势轨迹在物理像素域，甩出按
                            // 释放点所在屏 DPI 因子除算回 VDC（monitor_at 永不失败）。
                            let scale =
                                display.monitor_at(ports::to_platform_vec2(state.pos())).scale;
                            let vel = ports::scale_velocity(intent.vel_px_per_sec, scale);
                            let sub = state.release_drag(Some(vel), now);
                            eprintln!("[dp-app] core-loop Throw → 甩出起飞；toss 补提交仲裁={sub:?}");
                        }
                        _ => {}
                    }
                }

                // S3-M6 触发映射：意图 → 粒子迸发（`pet://fx`，C8 已登记；广播失败降级日志）。
                let click_feedback = state.click_feedback_cfg();
                for intent in &intents {
                    if let Some(cmd) = fx_burst_for_intent(intent.kind, &click_feedback) {
                        if let Err(err) = app.emit(FX_EVENT, &cmd) {
                            eprintln!("[dp-app] core-loop 广播 {FX_EVENT} 降级：{err}");
                        }
                    }
                }
                // S4-M6 触发映射：交互意图 → 音效（`01 §9.2`；门控 / 队列 / 溢出丢弃在 `dp-audio`）。
                for intent in &intents {
                    if let Some(cue) = audio_cue_for_intent(intent.kind) {
                        state.request_audio(cue);
                    }
                }
                // S3-M6 右键单击命中 → `pet://menu`（Up 触发；屏幕坐标 + 窗口内 CSS 坐标）。
                emit_menu_events(&app, &pet, consumer.take_menu_clicks());

                // S4-M3 / S4-M4：情绪交互结算（缓解表 + 道歉三部曲）+ 入站指令 + 离家可见性。
                // 事件名与载荷全在 `dp-core`（C8 单一真源），本层只 emit / 投仲裁。
                let mut emotion_events: Vec<EmotionEvent> = Vec::new();
                for intent in &intents {
                    emotion_events.extend(state.on_interaction_intent(intent.kind, now));
                }
                emotion_events.extend(state.coax_stroke_tick(now, consumer.is_stroking()));
                if let Some(channel) = app.try_state::<CoreInputChannel>() {
                    for input in channel.drain() {
                        // S5-M4：重置数据 / 导入存档 / 退出需要 `AppHandle`（重启进程 /
                        // 强制落盘后退出），走独立出口；其余仍走纯逻辑 `apply_core_input`。
                        if state.handle_admin_input(&app, &input, now) {
                            continue;
                        }
                        emotion_events.extend(state.apply_core_input(input, now));
                    }
                }
                if !emotion_events.is_empty() {
                    state.dispatch_emotion_events(&emotion_events, now as i64, Some(&app));
                }
                state.sync_runaway_visibility(&app);

                // S4 前清障 B15-④：播放回报排空（Finished 匹配 current → 链尾推进
                // → 出队起播 `Play` 下发覆盖态；每 logic tick 至多排空一次）。
                state.drain_finished(now);

                let outcome = state.logic_tick(now);
                // B17-②（2026-09-13）启动首拍：引擎构造即已钳制到工作区底边，但
                // `moved=false` 不写窗口，窗口停在 tauri.conf 初始 (120,120)，首次漫游
                // 才跳到位。首拍无视 `moved` 强制写一次锚定位，消除启动瞬移观感；
                // 写失败不置位，下个 logic tick 自然重试。
                if outcome.moved || !anchored_written {
                    // 2026-09-13 现场修复（锚点→窗口顶部换算）：`outcome.pos` 为脚底
                    // 锚点（`dp-core` 站立语义 = 工作区底边），窗口顶部 = 锚点 − 窗口
                    // 物理高 ÷ 所在屏 scale（VDC 域）。修复前以脚底直写窗口顶，窗口
                    // 垂直溢出屏外（B17-①，实测溢出 208px）。
                    let scale = display.monitor_at(ports::to_platform_vec2(outcome.pos)).scale;
                    let mut top = outcome.pos;
                    top.y -= PET_WINDOW_PHYS_H / scale;
                    match pet.window.set_position_vdc(ports::to_platform_vec2(top)) {
                        Ok(()) => anchored_written = true,
                        Err(err) => eprintln!("[dp-app] core-loop 写窗口位置降级：{err}"),
                    }
                }
                if outcome.landed_thrown {
                    // 甩出落地 → 尘土迸发（挂落地链起点；表现档数量，钳上限同源）。
                    let cmd = bridge::particle_cmd(ParticleKind::Dust, FX_DUST_BURST_COUNT);
                    if let Err(err) = app.emit(FX_EVENT, &cmd) {
                        eprintln!("[dp-app] core-loop 广播 {FX_EVENT} 降级：{err}");
                    }
                    // S4-M6：落地「噗 / 咚」（`01 §6.5.5` 移动类）。
                    state.request_audio(AudioCue::MoveLand);
                }
                if let Some(started) = state.poll_arbiter(now) {
                    eprintln!(
                        "[dp-app] core-loop 仲裁起播：{} fade={}ms（动作真播属后续 §6-8）",
                        started.request.id, started.fade_ms
                    );
                }
            }
        }

        if fired.render {
            if !seen_render {
                seen_render = true;
                eprintln!(
                    "[dp-app] core-loop render 档起跳：interval={}ms（不产帧，帧归 bridge）",
                    grid.render_interval()
                );
            }
            // K-4 档位自适应：按 bridge 帧播放器档位调整 render 网格（无档位槽 → 兜底）。
            let interval = app
                .try_state::<crate::bridge::FrameTierHandle>()
                .map_or(RENDER_DEFAULT_INTERVAL_MS, |t| {
                    1_000 / u64::from(t.get().fps()).max(1)
                });
            grid.set_render_interval(interval, now);
            // 刷新 bbox（物理像素；S3-M2 起经 HIT_LATEST 句柄委托写入——唯一矩形
            // 真源仍是 ports::PetBBoxHandle，钩子三态读点零感知）。
            if let Some(pet) = app.try_state::<PetPlatform>() {
                if let Ok(rect) = pet.window.window_physical_rect() {
                    hit.store_bbox(rect.left, rect.top, rect.right, rect.bottom);
                }
            }
        }

        if fired.biz {
            if !seen_biz {
                seen_biz = true;
                eprintln!(
                    "[dp-app] core-loop 业务档起跳：interval={BIZ_INTERVAL_MS}ms（S4-M1 情绪内核已接线；S4-M2 事件总线已接通）"
                );
            }
            // S4-M2：传入 `app` 作为传输层句柄（`pet://state` 1Hz / `pet://emotion`
            // 变更时）；事件名与载荷生产在 `dp-core::event`（C8 单一真源）。
            state.business_tick(wall.as_ref(), Some(&app));
            // S5-M1/M2：存档落盘档（30s 定时 / 变更 2s 合并窗口 / `PersistNow` 强制）。
            // `now` 为单调毫秒（间隔语义，C3）；`wall.now_ms()` 为墙钟（时刻语义）。
            state.save_tick(now, wall.now_ms());
        }
    }
}

/// 启动感知采样线程（同线程内三档 deadline 分档，各档 `bus.offer(...)`）。
fn spawn_perception(
    app: AppHandle,
    bus: Arc<PerceptionBus<PerceptionEvent>>,
    sink: Arc<ChannelSink>,
) -> JoinHandle<()> {
    let builder = std::thread::Builder::new().name("dp-perception".to_string());
    match builder.spawn(move || run_perception(app, bus, sink)) {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("[dp-app] 感知线程启动失败，降级为空采样：{err}");
            empty_handle()
        }
    }
}

/// 感知循环体：单线程三档 deadline 分档（**不得多线程 offer**，保 `PerceptionBus` 单生产者语义）。
fn run_perception(
    app: AppHandle,
    bus: Arc<PerceptionBus<PerceptionEvent>>,
    sink: Arc<ChannelSink>,
) {
    let start = Instant::now();
    let mut next_cursor = 0u64;
    let mut next_windows = 0u64;
    let mut next_system = 0u64;
    // S7-M1：活动感知 1Hz 档（键击 / 点击 / 移动强度 + 前台进程类别哈希 + 前台全屏）。
    let mut next_activity = 0u64;
    let mut cpu = CpuLoadSampler::new();
    let mut activity = dp_platform::InputIntensitySampler::new();

    loop {
        let target = next_cursor.min(next_windows).min(next_system).min(next_activity);
        sleep_until(start, target);

        let Some(pet) = app.try_state::<PetPlatform>() else {
            // 平台尚未装配（启动竞态）：短暂让出后重试，绝不 panic。
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let display = pet.platform.display();
        let now = start.elapsed().as_millis() as u64;

        if now >= next_cursor {
            next_cursor = advance_deadline(next_cursor, now, CURSOR_INTERVAL_MS);
            if let Some(pos) = read_cursor_vdc(&display) {
                bus.offer(PerceptionEvent::Cursor { pos: ports::to_core_vec2(pos) });
            }
        }
        if now >= next_windows {
            next_windows = advance_deadline(next_windows, now, WINDOWS_INTERVAL_MS);
            let (titlebars, taskbars) = ports::enumerate_window_bands(&display);
            bus.offer(PerceptionEvent::Windows { titlebars, taskbars });
        }
        if now >= next_system {
            next_system = advance_deadline(next_system, now, SYSTEM_INTERVAL_MS);
            let sample = SystemSample {
                battery: battery_status().map(ports::to_core_battery),
                cpu_load: cpu.sample(),
                presence_idle_ms: last_input_idle_ms(),
            };
            bus.offer(PerceptionEvent::System(sample));
        }
        if now >= next_activity {
            next_activity = advance_deadline(next_activity, now, ACTIVITY_INTERVAL_MS);
            // S7-M1 隐私红线 ④：感知关闭 → **根本不采样**（连前台进程查询都不做），
            // 退化为纯时间模型；此时 core-loop 侧的 `preset_idle_ms` 亦恒取 0（恒在场）。
            let Some(pet) = app.try_state::<PetPlatform>() else {
                continue;
            };
            if !pet.keyhook.is_activity_sensing() {
                continue;
            }
            let totals = dp_platform::InputTotals {
                keys: pet.keyhook.counters().total(),
                clicks: sink.clicks(),
                move_px: sink.move_px() as f64,
            };
            if let Some(intensity) = activity.sample(now as i64, totals) {
                let sample = ActivitySample {
                    key_kps: Some(intensity.key_kps),
                    clicks_per_min: Some(intensity.clicks_per_min),
                    move_px_per_min: Some(intensity.move_px_per_min),
                    // 隐私：平台层只交出 FNV-1a64 类别哈希，明文进程名不出函数。
                    foreground_hash: dp_platform::foreground_process_hash(),
                    // 全屏：全屏隐藏态 → 确定 `true`；否则「未知」（不复用 0.5Hz 窗控结果，
                    // 避免在本档引入第二次全屏查询）。
                    fullscreen: pet.window.is_hidden_for_fullscreen().then_some(true),
                };
                bus.offer(PerceptionEvent::Activity(sample));
            }
        }
    }
}

/// 推进单个绝对锚定 deadline 至首个 `> now` 的时刻（`+= 间隔`，过冲不累积）。
fn advance_deadline(mut next: u64, now: u64, interval_ms: u64) -> u64 {
    let interval = interval_ms.max(1);
    while now >= next {
        let bumped = next.saturating_add(interval);
        if bumped == next {
            break;
        }
        next = bumped;
    }
    next
}

/// 睡到 `start + target_ms` 的绝对锚定 deadline（越过则不补偿、不忙等；同 supervisor 策略）。
fn sleep_until(start: Instant, target_ms: u64) {
    let target = start + Duration::from_millis(target_ms);
    if let Some(remaining) = target.checked_duration_since(Instant::now()) {
        std::thread::sleep(remaining);
    }
}

// ---------------------------------------------------------------------------
// 纯逻辑单测（不依赖窗口）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dp_core::config::ActionCfg;
    use dp_core::motion::{MonitorGeom, SplitMix64};
    use dp_core::perception::FakeWallClock;
    // S5-M2：展示态枚举仅在测试内断言「离线不得进入离家出走」（生产代码不参考它）。
    use dp_core::emotion::EmotionState;
    // S5-M1：降级链落点判定（断言「全新安装走 Fresh」等装配期语义）。
    use dp_core::save::LoadStatus;

    /// 活动测试墙钟起点（UTC 毫秒；本地 = UTC 除非注入 offset）。
    const T0: i64 = 1_700_000_000_000;

    /// 主屏：VDC (0,0) 1920×1080，工作区底边 1040。
    fn mon_a() -> MonitorGeom {
        MonitorGeom {
            id: 1,
            origin_vdc: Vec2::new(0.0, 0.0),
            size_vdc: Vec2::new(1920.0, 1080.0),
            work_origin_vdc: Vec2::new(0.0, 0.0),
            work_size_vdc: Vec2::new(1920.0, 1040.0),
            primary: true,
        }
    }

    /// 更低的屏（工作区底边 2000，用于构造「原站立面消失」的下坠场景）。
    fn mon_low() -> MonitorGeom {
        MonitorGeom {
            id: 1,
            origin_vdc: Vec2::new(0.0, 0.0),
            size_vdc: Vec2::new(1920.0, 2400.0),
            work_origin_vdc: Vec2::new(0.0, 0.0),
            work_size_vdc: Vec2::new(1920.0, 2000.0),
            primary: true,
        }
    }

    /// ACT-T-01 启用目录（S5-M2 回归问候；priority 6 / `lifecycle.launchOrLongAbsence`，
    /// 元数据对齐 `resources/config/actions.json`）。
    fn greeting_catalog() -> ActionCatalog {
        ActionCatalog::from_actions(vec![ActionCfg {
            id: STARTUP_GREETING_ACTION.to_string(),
            name: "挥手打招呼".to_string(),
            category: "interact".to_string(),
            priority: 6,
            interruptible: false,
            looping: false,
            fps: 12,
            fade_ms: 200,
            disabled: false,
            ..ActionCfg::default()
        }])
    }

    /// ACT-M-06 启用目录（落地缓冲；priority 6，`resources/config/actions.json` 口径）。
    fn landing_catalog() -> ActionCatalog {
        ActionCatalog::from_actions(vec![ActionCfg {
            id: "ACT-M-06".to_string(),
            name: "落地缓冲".to_string(),
            category: "move".to_string(),
            priority: 6,
            interruptible: false,
            looping: false,
            fps: 15,
            fade_ms: 200,
            disabled: false,
            ..ActionCfg::default()
        }])
    }

    // -----------------------------------------------------------------------
    // S8-M1/M2：活动运行时集成（真实 `activities.json`；FakeWallClock 驱动，C3）
    // -----------------------------------------------------------------------

    /// S8-M1：活动全局配置夹具（`resources/config/activities.json`；验收口径与运行
    /// 一致；资源缺失降级内置默认，不 panic）。
    fn test_activities() -> ActivityGlobalCfg {
        match ConfigService::load_all(&resources_config_dir()) {
            Ok((bundle, _)) => bundle.activities.activity,
            Err(_) => ActivityGlobalCfg::default(),
        }
    }

    /// S8-M1：带真实活动配置的 `CoreCfg`（活动集成测试专用）。
    fn core_cfg_with_activities(catalog: ActionCatalog) -> CoreCfg {
        let mut cfg = core_cfg(catalog);
        cfg.activities = test_activities();
        cfg
    }

    /// 装配活动测试状态（`FakeWallClock` 注入，C3；返回共享 Arc 供推进）。
    fn activity_state(catalog: ActionCatalog, t0: i64) -> (CoreLoopState, Arc<FakeWallClock>) {
        let mut state = CoreLoopState::new(
            Vec2::ZERO,
            vec![mon_a()],
            core_cfg_with_activities(catalog),
            7,
            0,
        );
        let wall = Arc::new(FakeWallClock::new(t0));
        state.set_wall_for_test(Arc::clone(&wall) as Arc<dyn WallClock>);
        (state, wall)
    }

    /// 空目录（演出动作未启用）：验证 S8-M4 之前的**降级闭环**——出发 / 回归演出
    /// 不提交，`step_activity_performance` 下一拍自动推进，活动照常结算。
    fn empty_catalog() -> ActionCatalog {
        ActionCatalog::from_actions(Vec::new())
    }

    /// 派遣 → 出发（降级）→ Running → 到期 → 回归（降级）→ 结算 → Idle 的完整闭环。
    #[test]
    fn activity_dispatch_to_settle_roundtrip() {
        let (mut state, wall) = activity_state(empty_catalog(), T0);
        // ① 派遣（logic 档入口；W-01 30min）。
        let _ = state.apply_core_input(
            CoreInput::ActivityDispatch {
                kind: "work".into(),
                def_id: "W-01".into(),
                duration_min: 30,
            },
            T0 as u64,
        );
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Preparing);
        // 前置消耗已扣（W-01：energy −12 / cleanliness −12，`02 §5.15` 出发瞬间）。
        assert!(
            (state.emotion.state.values.energy - 88.0).abs() < 1e-6,
            "energy={}",
            state.emotion.state.values.energy
        );
        assert!((state.emotion.state.values.cleanliness - 73.0).abs() < 1e-6);
        // ② 业务拍：出发演出未启用 → confirm_departed → Running。
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Running);
        // ③ 推进到 end 之后 → TimeUp；回归演出未启用 → **同拍直达结算** → Idle
        //   （启用回归动作时停在 Returning 等待播毕，见 `activity_return_play_holds`）。
        wall.set_now_ms(T0 + 30 * 60_000 + 1_000);
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Idle);
        // 结算数值已应用到内核：Mood 经「31min 时间跳变的情绪衰减（dt 已按引擎口径
        // 钳制）+ 打工 jitter ∈ [-2, +5]（seed 确定性）」→ 落在衰减后的合理带内。
        assert!(
            state.emotion.state.values.mood >= 30.0 && state.emotion.state.values.mood <= 45.0,
            "mood={}",
            state.emotion.state.values.mood
        );
    }

    /// 回归演出启用时：到期 → Returning 停留（演出 in-flight），播毕后推进结算。
    #[test]
    fn activity_return_play_holds_then_settles() {
        // 目录含 ACT-N-10（启用；`actions.json` 口径字段）。
        let catalog = ActionCatalog::from_actions(vec![ActionCfg {
            id: "ACT-N-10".to_string(),
            name: "下班回家".to_string(),
            category: "activity".to_string(),
            priority: 8,
            interruptible: true,
            looping: false,
            fps: 12,
            fade_ms: 200,
            disabled: false,
            ..ActionCfg::default()
        }]);
        let (mut state, wall) = activity_state(catalog, T0);
        let _ = state.apply_core_input(
            CoreInput::ActivityDispatch {
                kind: "work".into(),
                def_id: "W-01".into(),
                duration_min: 30,
            },
            T0 as u64,
        );
        // 出发演出同样启用（ACT-N-09 不在目录 → 降级，Preparing 下一拍推进）。
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Running);
        // 到期：回归演出在播 → Returning 停留（不提前结算）。
        wall.set_now_ms(T0 + 30 * 60_000 + 1_000);
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Returning);
        assert!(
            state
                .arbiter
                .current()
                .is_some_and(|a| a.request.id == "ACT-N-10"),
            "回归动作应已提交在播"
        );
        // 下一拍：动作仍在播（无 Finished 回报）→ 仍 Returning。
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Returning);
        // 停播推进（等价播完）：`on_action_finished` 出队 → 下一拍 settle。
        // 测试经私有入口不可达，直接验证「非在播即推进」——用不含动作的状态等价路径
        // 已在 `activity_dispatch_to_settle_roundtrip` 覆盖；此处补 `stop_looping` 无
        // 循环动作 → no-op，保持 Returning 语义正确。
        assert!(!state.current_is_looping(), "ACT-N-10 非循环，停播走 Finished 回报路径");
    }

    /// 提前召回：Running → Returning；结算按已完成比例 ×0.5，P+6 生效。
    #[test]
    fn activity_recall_applies_early_penalty() {
        let (mut state, wall) = activity_state(empty_catalog(), T0);
        let _ = state.apply_core_input(
            CoreInput::ActivityDispatch {
                kind: "work".into(),
                def_id: "W-01".into(),
                duration_min: 30,
            },
            T0 as u64,
        );
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Running);
        // 中途（15min）召回。
        wall.set_now_ms(T0 + 15 * 60_000);
        let _ = state.apply_core_input(CoreInput::ActivityRecall, (T0 + 15 * 60_000) as u64);
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Returning);
        let p_before = state.emotion.neglect.p;
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Idle);
        assert!(
            state.emotion.neglect.p > p_before,
            "召回 P+6 应生效：{p_before} → {}",
            state.emotion.neglect.p
        );
    }

    /// 安静时段（23:00-05:00 本地）拒派。
    #[test]
    fn activity_dispatch_refused_in_quiet_hours() {
        let (mut state, wall) = activity_state(empty_catalog(), T0);
        // T0 = 22:13 UTC；offset +3600s → 本地 23:13（安静时段）。
        wall.set_offset_sec(3600);
        let _ = state.apply_core_input(
            CoreInput::ActivityDispatch {
                kind: "work".into(),
                def_id: "W-01".into(),
                duration_min: 30,
            },
            T0 as u64,
        );
        assert_eq!(
            state.activity_runtime.phase(),
            ActivityPhase::Idle,
            "安静时段应拒派"
        );
    }

    /// AC-36：冷落阶段 L4/L5 拒派（「她在生气，先哄好她吧」）。
    #[test]
    fn activity_dispatch_refused_when_cold_level_high() {
        let (mut state, _wall) = activity_state(empty_catalog(), T0);
        state.emotion.neglect.level = 4;
        let _ = state.apply_core_input(
            CoreInput::ActivityDispatch {
                kind: "work".into(),
                def_id: "W-01".into(),
                duration_min: 30,
            },
            T0 as u64,
        );
        assert_eq!(
            state.activity_runtime.phase(),
            ActivityPhase::Idle,
            "L4 应拒派"
        );
    }

    /// 旅游：明信片按 `postcardIntervalMin` 到期累计；快照带活动段。
    #[test]
    fn activity_travel_accumulates_postcards_and_snapshot() {
        let (mut state, wall) = activity_state(empty_catalog(), T0);
        let _ = state.apply_core_input(
            CoreInput::ActivityDispatch {
                kind: "travel".into(),
                def_id: "TR-01".into(),
                duration_min: 0, // 旅游忽略该参数（固定 120min）。
            },
            T0 as u64,
        );
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Running);
        // TR-01：interval 40min → 40 / 80 / 120 三张；41min 时第 1 张到期。
        wall.set_now_ms(T0 + 41 * 60_000);
        state.business_tick_present(wall.as_ref());
        assert_eq!(
            state.activity_runtime.current().unwrap().postcards_sent,
            1,
            "第一张明信片应已到期"
        );
        // 快照（`app=None` 时广播早退；直接探针活动段）。
        let snap = state.activity_snapshot_json().expect("活动快照");
        assert_eq!(snap["running"], true);
        assert_eq!(snap["instance"]["postcardsSent"], 1);
        assert_eq!(snap["instance"]["defId"], "TR-01");
        // 未运行 → 快照 `running=false`。
        let _ = state.apply_core_input(CoreInput::ActivityRecall, (T0 + 41 * 60_000) as u64);
        state.business_tick_present(wall.as_ref());
        let idle = state.activity_snapshot_json().expect("空闲快照");
        assert_eq!(idle["running"], false);
    }

    // -----------------------------------------------------------------------
    // S8-M4：批次 C 活动演出接入（出发演出实播 / 学习桌面循环 / 疲惫喘气）
    // -----------------------------------------------------------------------

    /// S8-M4：单动作启用目录（活动演出测试专用；字段对齐 `actions.json` 口径）。
    fn one_action_catalog(id: &str, name: &str, priority: u32, looping: bool, performance: bool) -> ActionCatalog {
        ActionCatalog::from_actions(vec![ActionCfg {
            id: id.to_string(),
            name: name.to_string(),
            category: "activity".to_string(),
            priority,
            interruptible: !performance,
            looping,
            fps: 12,
            fade_ms: 200,
            disabled: false,
            ..ActionCfg::default()
        }])
    }

    /// 派遣打工 → 出发演出实播（ACT-N-09 in-flight）；播毕回报 → 推进 Running。
    #[test]
    fn activity_dispatch_plays_departure_action_and_advances() {
        let catalog = one_action_catalog("ACT-N-09", "打工准备", 7, false, true);
        let (mut state, wall) = activity_state(catalog, T0);
        let _ = state.apply_core_input(
            CoreInput::ActivityDispatch {
                kind: "work".into(),
                def_id: "W-01".into(),
                duration_min: 30,
            },
            T0 as u64,
        );
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Preparing);
        assert!(
            state.arbiter.current().is_some_and(|a| a.request.id == "ACT-N-09"),
            "出发演出应已提交在播（批次 C 启用）"
        );
        // 演出在播 → 下一拍仍停留 Preparing（不提前推进）。
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Preparing);
        // 模拟播放完成回报（播放器经 drain_finished 的语义同构：on_action_finished）。
        let _ = state.arbiter.on_action_finished(T0 as u64 + 1_000);
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Running);
    }

    /// 结算后 Energy<40 → 疲惫喘气演出（ACT-N-16；`01 §6.13`）。
    #[test]
    fn activity_settle_low_energy_plays_tired_pant() {
        let catalog = one_action_catalog("ACT-N-16", "疲惫喘气", 4, true, false);
        let (mut state, wall) = activity_state(catalog, T0);
        let _ = state.apply_core_input(
            CoreInput::ActivityDispatch {
                kind: "work".into(),
                def_id: "W-01".into(),
                duration_min: 30,
            },
            T0 as u64,
        );
        // 出发演出不在目录 → 降级，下一拍 Running。
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Running);
        // 低能量场景（打工后疲惫）。
        state.emotion.state.values.energy = 30.0;
        wall.set_now_ms(T0 + 30 * 60_000 + 1_000);
        state.business_tick_present(wall.as_ref());
        // 回归演出不在目录 → 同拍结算；Energy<40 → ACT-N-16 已提交。
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Idle);
        assert!(
            state.arbiter.current().is_some_and(|a| a.request.id == "ACT-N-16"),
            "结算后低能量应播疲惫喘气"
        );
    }

    /// 学习桌面循环：Running 期间循环播 ACT-N-11（幂等不堆积）；到期停播收尾。
    #[test]
    fn activity_study_plays_desk_loop_and_stops_on_timeup() {
        let catalog = one_action_catalog("ACT-N-11", "学习", 6, true, false);
        let (mut state, wall) = activity_state(catalog, T0);
        let _ = state.apply_core_input(
            CoreInput::ActivityDispatch {
                kind: "study".into(),
                def_id: "CRS-01".into(),
                duration_min: 30,
            },
            T0 as u64,
        );
        // 出发演出不在目录 → 降级，下一拍 Running。
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Running);
        // Running & study → 下一拍提交 ACT-N-11（学习循环在播）。
        state.business_tick_present(wall.as_ref());
        assert!(
            state.arbiter.current().is_some_and(|a| a.request.id == "ACT-N-11"),
            "学习循环应已提交在播"
        );
        // 幂等守卫：下一拍不重复提交（current 仍为 ACT-N-11，不堆积队列）。
        state.business_tick_present(wall.as_ref());
        assert!(
            state.arbiter.current().is_some_and(|a| a.request.id == "ACT-N-11"),
            "学习循环持续在播"
        );
        // 到期：学习循环停播收尾 → 无回归动作 → 同拍结算 Idle。
        wall.set_now_ms(T0 + 30 * 60_000 + 1_000);
        state.business_tick_present(wall.as_ref());
        assert_eq!(state.activity_runtime.phase(), ActivityPhase::Idle);
        assert!(
            !state.arbiter.current().is_some_and(|a| a.request.id == "ACT-N-11"),
            "学习循环到期应停播"
        );
    }

    /// S3-M4 拖拽/甩出目录（ACT-M-06 + ACT-T-06/07/08；元数据逐字段对齐
    /// `resources/config/actions.json`，含 looping / loopRange / minInterruptPriority）。
    fn toss_catalog() -> ActionCatalog {
        ActionCatalog::from_actions(vec![
            ActionCfg {
                id: "ACT-M-06".to_string(),
                name: "落地缓冲".to_string(),
                category: "move".to_string(),
                priority: 6,
                interruptible: false,
                looping: false,
                fps: 15,
                fade_ms: 200,
                disabled: false,
                ..ActionCfg::default()
            },
            ActionCfg {
                id: "ACT-T-06".to_string(),
                name: "抛物线翻滚".to_string(),
                category: "interact".to_string(),
                priority: 7,
                interruptible: true,
                min_interrupt_priority: 9,
                looping: true,
                loop_range: Some([0, 7]),
                fps: 18,
                fade_ms: 200,
                disabled: false,
                ..ActionCfg::default()
            },
            ActionCfg {
                id: "ACT-T-07".to_string(),
                name: "摔倒打滚".to_string(),
                category: "interact".to_string(),
                priority: 7,
                interruptible: false,
                min_interrupt_priority: 0,
                looping: false,
                fps: 12,
                fade_ms: 200,
                disabled: false,
                ..ActionCfg::default()
            },
            ActionCfg {
                id: "ACT-T-08".to_string(),
                name: "抗议跺脚".to_string(),
                category: "interact".to_string(),
                priority: 6,
                interruptible: false,
                min_interrupt_priority: 0,
                looping: false,
                fps: 10,
                fade_ms: 200,
                disabled: false,
                ..ActionCfg::default()
            },
        ])
    }

    fn roam_cfg() -> RoamCfg {
        RoamCfg {
            pace_options: dp_core::config::model::RoamCfg::default().pace_options,
            pace: 1.0,
            decision_interval_sec: [5, 30],
            cursor_avoid_radius_px: 150,
            walk_speed_px_per_sec: 60.0,
        }
    }

    /// 资源目录（`resources/config`；`CARGO_MANIFEST_DIR` 上溯三级，C1 无盘符字面量）。
    fn resources_config_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/config")
    }

    /// C2 口径：角色默认名以 Unicode 码点构造，测试内不写角色名字面量。
    fn name_cp() -> String {
        ['\u{5FC3}', '\u{6708}', '\u{72F0}'].iter().collect()
    }

    /// S4-M5 装配夹具：台词库取随包 `lines.json`（真实内容，非空库）。
    fn test_lines() -> LinesLibrary {
        LinesLibrary::load_dir(&resources_config_dir()).expect("随包 lines.json 应可加载")
    }

    /// S4-M1 装配夹具：除动作目录外的配置一律取内置默认（数值口径 = `emotion.json` v2
    /// 冻结值 / `settings.json` 默认）；S4-M5/M6 起附台词库与「无音效」占位。
    fn core_cfg(catalog: ActionCatalog) -> CoreCfg {
        CoreCfg {
            roam_cfg: roam_cfg(),
            interaction_cfg: InteractionCfg::default(),
            catalog,
            emotion: EmotionConfig::default(),
            needs: NeedsConfig::default(),
            character_default_name: name_cp(),
            character_catchphrase: None,
            lines: test_lines(),
            audio: None,
            // S5-M1 起 `CoreCfg` 追加存档门面；测试取 `None` = 不载档不落盘（纯逻辑模式）。
            save: None,
            // S8-M1 起 `CoreCfg` 追加活动全局配置；测试取默认（C7：口径来自 activities.json）。
            activities: ActivityGlobalCfg::default(),
            shop: ShopConfig::default(),
            achievements: AchievementsConfig::default(),
        }
    }

    // -- ScheduleGrid：三档绝对锚定无漂移（第 k 次 deadline = start + k×间隔） ------

    #[test]
    fn schedule_grid_deadlines_absolutely_anchored_no_drift() {
        let mut g = ScheduleGrid::new(0, 16, 50, 1_000);
        assert_eq!(g.next_logic(), 50, "首个 logic deadline = start + 1×50");
        // 每轮迟到 30ms 触发（overshoot < 间隔，每轮恰推进一档）。
        for k in 1..=10u64 {
            let fired = g.advance(k * 50 + 30);
            assert!(fired.logic, "第 {k} 轮 logic 应到期");
        }
        // 绝对锚定：过冲不累积；若按「实际触发时刻 + 间隔」累加将漂移到 830。
        assert_eq!(g.next_logic(), 550, "第 11 次 deadline = start + 11×50");

        // 业务档 1s：now=1030 时恰推进一档 → 下一 deadline = 0 + 2×1000。
        let mut g2 = ScheduleGrid::new(0, 16, 50, 1_000);
        let fired = g2.advance(1_030);
        assert!(fired.logic && fired.biz, "1030ms 时 logic 与业务档同时到期");
        assert_eq!(g2.next_biz(), 2_000, "业务档绝对锚定 = start + 2×1000");
    }

    #[test]
    fn schedule_grid_overshoot_catches_up_and_stays_anchored() {
        let mut g = ScheduleGrid::new(0, 16, 50, 1_000);
        // 迟到整 3 个周期（now=150）：一次 advance 以固定间隔逐档追赶至首个 `> now` 的 deadline。
        let fired = g.advance(150);
        assert!(fired.logic);
        assert_eq!(g.next_logic(), 200, "追赶后 deadline 仍为 start + k×50（过冲不漂移）");
        // 已追平：同一 now 再次 advance 不重复触发（deadline 均已超前）。
        let fired = g.advance(150);
        assert!(!fired.logic, "已追平，同一 now 不重复触发");
    }

    #[test]
    fn schedule_grid_render_interval_rescale_reanchors() {
        let mut g = ScheduleGrid::new(0, 16, 50, 1_000);
        g.set_render_interval(500, 1_000);
        assert_eq!(g.next_render(), 1_500, "重锚：now + 新间隔");
        assert_eq!(g.render_interval(), 500);
        // 同值不重锚。
        g.set_render_interval(500, 9_999);
        assert_eq!(g.next_render(), 1_500, "间隔不变不重锚");
    }

    // -- 相机器：Roam→Fall（非法站立面）→ Fall→Roam（Landed + ACT-M-06 submit）------

    #[test]
    fn phase_machine_roams_falls_then_lands_and_submits_landing_action() {
        // 空显示器：无任何站立面 → 首 logic tick 即 Roam→Fall（注入式冒烟，§6-7 可达性注）。
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 500.0),
            Vec::new(),
            core_cfg(landing_catalog()),
            7,
            0,
            );
        assert_eq!(state.phase(), Phase::Roam);

        let out = state.logic_tick(50);
        assert_eq!(state.phase(), Phase::Fall, "非法站立面 → 换相 Fall");
        assert!(out.submitted.is_none(), "起坠不提交动作");

        // 持续下坠若干 tick（无站立面，永不落地）。
        for k in 1..=20u64 {
            let out = state.logic_tick(50 + k * 50);
            assert!(out.submitted.is_none(), "下坠中不提交动作");
        }
        assert!(state.pos().y > 500.0, "下坠位置显著下移：{}", state.pos().y);

        // 站立面重现（更低的地板 y=2000）→ 继续下坠至落地 → Landed → 换相 Roam + submit。
        state.apply_monitors(vec![mon_low()], 1_100);
        let mut landed = None;
        for k in 0..300u64 {
            let out = state.logic_tick(1_150 + k * 50);
            if out.submitted.is_some() {
                landed = out.submitted;
                break;
            }
        }
        assert_eq!(state.phase(), Phase::Roam, "Landed 后换相回 Roam");
        assert_eq!(
            landed,
            Some(Arbitration::Play),
            "Landed → 空仲裁器首次 submit(ACT-M-06) 命中 Play"
        );
    }

    #[test]
    fn stable_floor_does_not_fall_and_pos_holds() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(landing_catalog()),
            7,
            0,
            );
        // 短时间内不触发漫游决策（间隔 5~30s）；位置保持，不换相。
        let out = state.logic_tick(50);
        assert_eq!(state.phase(), Phase::Roam);
        assert!(!out.moved, "决策未到期，pos 不变");
        assert!(out.submitted.is_none());
        assert!((state.pos().y - 1040.0).abs() < 1e-3);
    }

    // -- seed_k 递推：两次重建产生不同种子 / 不同漫游随机序列 ----------------------

    #[test]
    fn seed_recursion_yields_distinct_seeds_and_sequences() {
        let s0 = 42u64;
        let s1 = advance_seed(s0, 1_000);
        let s2 = advance_seed(s1, 2_000);
        assert_ne!(s1, s0);
        assert_ne!(s2, s1, "递推两次应得不同种子（避免同种子重复漫游）");
        let mut a = SplitMix64::new(s1);
        let mut b = SplitMix64::new(s2);
        assert_ne!(a.next_u64(), b.next_u64(), "不同种子 → 不同漫游随机序列");
    }

    // -- 显示器拓扑变化检测 ------------------------------------------------------

    #[test]
    fn monitors_changed_detects_add_remove_and_geometry() {
        assert!(!monitors_changed(&[mon_a()], &[mon_a()]), "同几何无变化");
        assert!(monitors_changed(&[mon_a()], &[]), "删屏");
        assert!(monitors_changed(&[mon_a()], &[mon_a(), mon_low()]), "增屏");
        assert!(monitors_changed(&[mon_a()], &[mon_low()]), "同 id 几何变化");
    }

    // -- S3-M4：拖拽相（begin_drag / drag_to / release_drag）与甩出物理 ------------

    #[test]
    fn begin_drag_enters_drag_tracks_cursor_and_submits_toss() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        // 首次进入：Drag 相 + 权威 pos 即钳制后光标 + toss Play。
        assert_eq!(
            state.begin_drag(Vec2::new(1000.0, 500.0), 50),
            Some(Arbitration::Play),
            "空仲裁器首次提交 toss → Play"
        );
        assert_eq!(state.phase(), Phase::Drag);
        assert_eq!(state.pos(), Vec2::new(1000.0, 500.0));
        // 幂等：已在 Drag 相只跟手，不重复提交。
        assert_eq!(state.begin_drag(Vec2::new(1100.0, 600.0), 100), None, "幂等返回 None");
        assert_eq!(state.pos(), Vec2::new(1100.0, 600.0));
        // drag_to 暂存 + logic_tick Drag 臂刷新权威 pos（跟手闭环）。
        state.drag_to(Vec2::new(1200.0, 300.0));
        let out = state.logic_tick(150);
        assert_eq!(state.pos(), Vec2::new(1200.0, 300.0), "Drag 相 pos = 钳制后光标位");
        assert_eq!(out.pos, state.pos());
        assert!(out.moved);
        assert!(out.submitted.is_none(), "Drag 相无 MotionEvent → 无仲裁接线");
    }

    #[test]
    fn drag_target_clamps_to_virtual_desktop_bounds() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        assert_eq!(state.begin_drag(Vec2::new(960.0, 500.0), 50), Some(Arbitration::Play));
        // 右 / 下越界 → 钳到工作区并集右下角（mon_a：1920, 1040）。
        state.drag_to(Vec2::new(99_999.0, 99_999.0));
        state.logic_tick(100);
        assert_eq!(state.pos(), Vec2::new(1920.0, 1040.0), "右下越界钳到并集右下角");
        // 左 / 上越界 → 钳到并集左上角。
        state.drag_to(Vec2::new(-9.0, -9.0));
        state.logic_tick(150);
        assert_eq!(state.pos(), Vec2::new(0.0, 0.0), "左上越界钳到并集左上角");
    }

    #[test]
    fn release_none_on_valid_stand_reenters_roam_without_action() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        assert_eq!(state.begin_drag(Vec2::new(960.0, 1040.0), 50), Some(Arbitration::Play));
        // 落点即合法站立面（地板）→ 直接回 Roam，无落地演出。
        assert_eq!(state.release_drag(None, 100), None, "纯松手无仲裁产出");
        assert_eq!(state.phase(), Phase::Roam);
        assert!(!state.thrown_flight());
        assert!(state.arbiter.queue_snapshot().is_empty(), "无落地演出 → 队列空");
    }

    #[test]
    fn release_none_offscreen_falls_naturally_and_lands_with_motion_action() {
        // 仅落地缓冲目录（无 toss）：隔离「自然下坠 ≠ 甩出」的结算路径。
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(landing_catalog()),
            7,
            0,
            );
        assert_eq!(state.begin_drag(Vec2::new(960.0, 500.0), 50), None, "目录缺 toss → 降级 None");
        assert_eq!(state.phase(), Phase::Drag);
        assert_eq!(state.release_drag(None, 100), None);
        assert_eq!(state.phase(), Phase::Fall, "悬空松手 → 自然下坠");
        assert!(!state.thrown_flight(), "自然下坠非甩出飞行");
        let mut submitted = None;
        for k in 1..=40u64 {
            let out = state.logic_tick(100 + k * 50);
            if out.submitted.is_some() {
                submitted = out.submitted;
                break;
            }
        }
        assert_eq!(state.phase(), Phase::Roam);
        assert_eq!(submitted, Some(Arbitration::Play), "自然落地 → ACT-M-06 Play");
        assert_eq!(state.throw_landed_count(), 0, "非甩出落地不计数");
    }

    #[test]
    fn throw_from_screen_top_lands_within_1500ms_with_landing_chain() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        // 最坏起点 = 屏顶：向上初速远超预算 → 被闭式解钳制，仍必须 1.5s 内落地。
        assert_eq!(state.begin_drag(Vec2::new(960.0, 0.0), 1_000), Some(Arbitration::Play));
        assert_eq!(
            state.release_drag(Some(Vec2::new(300.0, -100_000.0)), 1_000),
            None,
            "toss 已在播 → 无补提交"
        );
        assert_eq!(state.phase(), Phase::Fall);
        assert!(state.thrown_flight());
        let mut landed_at = None;
        let mut submitted = None;
        for k in 1..=60u64 {
            let out = state.logic_tick(1_000 + k * 50);
            if out.submitted.is_some() {
                landed_at = Some(1_000 + k * 50);
                submitted = out.submitted;
                break;
            }
        }
        let land = landed_at.expect("1.5s 预算内应落地");
        assert!(land - 1_000 <= 1_500, "FR-4-6：实测飞行+tick 粒度 {}ms", land - 1_000);
        assert_eq!(state.phase(), Phase::Roam);
        assert!(!state.thrown_flight());
        assert_eq!(state.throw_landed_count(), 1, "甩出落地计数 +1");
        // 链首 07：force_submit 面对 toss（p7 可打断）差 0 → 排队 pos=1（1 基）。
        // S4 B15-④：链提交后停播推进 toss 出局、07 fade 0 起播成 current →
        // 队列快照仅剩链尾 08（07 播毕由 Finished 回报起播 08）。
        assert_eq!(submitted, Some(Arbitration::Queued { pos: 1 }));
        let ids: Vec<&str> =
            state.arbiter.queue_snapshot().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["ACT-T-08"], "链尾 08 依序在队（07 已起播为 current）");
    }

    #[test]
    fn thrown_flight_follows_parabolic_direction() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        assert_eq!(state.begin_drag(Vec2::new(300.0, 500.0), 50), Some(Arbitration::Play));
        // 右上抛（vx=500、vy=−400：未达闭式解上限，不被钳）。
        assert_eq!(state.release_drag(Some(Vec2::new(500.0, -400.0)), 100), None);
        let mut min_y = f32::MAX;
        let mut last_x = 300.0f32;
        let mut landed = false;
        for k in 1..=60u64 {
            let out = state.logic_tick(100 + k * 50);
            min_y = min_y.min(out.pos.y);
            last_x = out.pos.x;
            if out.submitted.is_some() {
                landed = true;
                break;
            }
        }
        assert!(landed, "应落地");
        assert!(min_y < 490.0, "向上初速应先升（实测 min_y={min_y}）");
        assert!(last_x > 300.0, "向右初速应净右移（实测 x={last_x}）");
        assert_eq!(state.throw_landed_count(), 1);
    }

    #[test]
    fn throw_intent_without_drag_takes_off_directly() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        // 同批互斥序 Throw 先于 DragStart 的竞态：Drag 相缺席 → 以当前权威位起飞 + 补提交 toss。
        assert_eq!(
            state.release_drag(Some(Vec2::new(0.0, -800.0)), 1_000),
            Some(Arbitration::Play),
            "current 空 → 补提交 toss 命中 Play"
        );
        assert_eq!(state.phase(), Phase::Fall);
        assert!(state.thrown_flight());
        let mut submitted = None;
        for k in 1..=60u64 {
            let out = state.logic_tick(1_000 + k * 50);
            if out.submitted.is_some() {
                submitted = out.submitted;
                break;
            }
        }
        assert_eq!(state.throw_landed_count(), 1);
        // S4 B15-④：停播推进后队列仅剩链尾 08（07 已出队起播为 current）。
        let ids: Vec<&str> =
            state.arbiter.queue_snapshot().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["ACT-T-08"], "补提交 toss 在播 → 落地链提交后 08 在队");
        assert_eq!(submitted, Some(Arbitration::Queued { pos: 1 }));
    }

    #[test]
    fn clamp_throw_velocity_caps_upward_vy_to_closed_form_budget() {
        let state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        // 闭式解期望值：重力取 cfg 单一真源（RV-18，禁字面量）；h 取夹具几何跨度。
        let g = InteractionCfg::default().gravity_px_per_sec2;
        let m = mon_a();
        let h = m.work_bottom_vdc() - m.work_origin_vdc.y;
        let t_eff = ((THROW_MAX_FLIGHT_MS - THROW_FLIGHT_MARGIN_MS) as f32) / 1000.0;
        let expected = (g * t_eff / 2.0 - h / t_eff).max(0.0);
        // 超预算向上速度 → 钳到闭式解。
        let clamped = state.clamp_throw_velocity(Vec2::new(0.0, -100_000.0));
        assert!(
            (clamped.y + expected).abs() < 1e-2,
            "向上 vy 应钳到闭式解 {expected}（实测 {}）",
            clamped.y
        );
        // 单子步位移对照：钳后 |vy|×10ms ≤ 预算 ×10ms（防穿透的实质约束）。
        let sub = SUB_STEP_MS as f32 / 1000.0;
        assert!(clamped.y.abs() * sub <= expected * sub + 1e-3);
        // 向下分量不受限（下落更快，落地时限恒成立）。
        let down = state.clamp_throw_velocity(Vec2::new(0.0, 50_000.0));
        assert_eq!(down.y, 50_000.0, "向下分量不钳");
    }

    // -- QA 探针（S3-M4 独立验证）：AC-09 端到端时序 + 边界探查 --------------------
    // 以下探针为 QA 验证轮新增：只探公共行为边界，不改源码，探查理由逐条注明。

    /// AC-09 端到端完整时序：Roam → DragStart → begin_drag → drag_to 跟手 →
    /// Throw → release_drag（上抛钳制内、vx>0）→ Fall 逐 tick → Landed。
    ///
    /// 探查理由：既有用例各自覆盖单段（begin_drag 幂等 / 屏顶甩出 / 抛物线方向），
    /// 但「拖拽移动后甩出」的完整验收链无单测串联；且既有用例未断言
    /// ①逐 tick x 单调性 ⑤落点 y 精确等于工作区底边（屏内）。本探针按
    /// AC-09 逐条断言：①抛物线方向 ②落地 ≤1500ms ③落地链 [07,08] ④计数 1
    /// ⑤落点钳回底边。
    #[test]
    fn qa_probe_ac09_end_to_end_drag_then_throw_full_sequence() {
        let mut state = CoreLoopState::new(
            Vec2::new(300.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        state.logic_tick(50);
        assert_eq!(state.phase(), Phase::Roam, "起点应处于漫游相");

        // ① DragStart 意图 → begin_drag：挂光标 toss 起播、进入 Drag 相。
        assert_eq!(
            state.begin_drag(Vec2::new(300.0, 900.0), 100),
            Some(Arbitration::Play),
            "DragStart → toss Play"
        );

        // ② drag_to 跟手：三拍移动，权威 pos 逐拍刷新。
        let path = [(400.0, 800.0), (500.0, 750.0), (600.0, 700.0)];
        for (k, (x, y)) in path.iter().enumerate() {
            state.drag_to(Vec2::new(*x, *y));
            let out = state.logic_tick(150 + k as u64 * 50);
            assert!(out.moved, "Drag 相第 {k} 拍应跟手位移");
            assert!(out.submitted.is_none(), "Drag 相不产仲裁接线");
        }
        let release_pos = state.pos();
        assert_eq!(release_pos, Vec2::new(600.0, 700.0), "权威 pos = 最后拖拽点");

        // ③ Throw 意图 → release_drag(Some)：vx>0、小幅上抛（闭式解钳制内）。
        let release_ms = 300u64;
        assert_eq!(
            state.release_drag(Some(Vec2::new(400.0, -300.0)), release_ms),
            None,
            "toss 已在播 → 起飞无补提交"
        );
        assert_eq!(state.phase(), Phase::Fall);
        assert!(state.thrown_flight(), "甩出飞行标记置位");

        // ④ Fall 逐 tick：x 单调向 vx 方向（不降）+ 上抛段先升。
        let mut prev_x = release_pos.x;
        let mut min_y = release_pos.y;
        let mut landed_at = None;
        let mut submitted = None;
        for k in 1..=40u64 {
            let now = release_ms + k * 50;
            let out = state.logic_tick(now);
            if state.phase() == Phase::Fall {
                assert!(
                    out.pos.x >= prev_x,
                    "飞行 x 应单调不降（vx>0 无撞墙）：{} vs {prev_x}",
                    out.pos.x
                );
                prev_x = out.pos.x;
                min_y = min_y.min(out.pos.y);
            }
            if out.submitted.is_some() {
                landed_at = Some(now);
                submitted = out.submitted;
                break;
            }
        }

        // ②落地时限（FR-4-6：甩出后 1.5s 内完成落地）。
        let land = landed_at.expect("1.5s 预算内应落地");
        assert!(
            land - release_ms <= 1_500,
            "FR-4-6：实测飞行+tick 粒度 {}ms",
            land - release_ms
        );
        // ①抛物线符合初速度方向：上抛段先升 + x 净右移。
        assert!(min_y < release_pos.y - 5.0, "上抛段应先升：min_y={min_y}");
        assert!(state.pos().x > release_pos.x, "x 净位移应向 vx 方向");
        // ③落地结算：submitted 非空 + 链尾 08 在队（S4 B15-④：07 已由停播推进
        // 出队起播为 current，队列快照仅剩 08；07 播毕由 Finished 回报起播 08）。
        assert_eq!(
            submitted,
            Some(Arbitration::Queued { pos: 1 }),
            "链首 07 force_submit（差 0 优先级）→ 排队 pos=1"
        );
        let ids: Vec<&str> =
            state.arbiter.queue_snapshot().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["ACT-T-08"], "链尾 08 在队（07 已起播为 current）");
        // ④C8 计数。
        assert_eq!(state.throw_landed_count(), 1, "甩出落地计数恰 +1");
        // ⑤落点钳在屏内：y 精确等于工作区底边。
        assert!(
            (state.pos().y - 1040.0).abs() < 1e-3,
            "落点 y 应等于工作区底边 1040：{}",
            state.pos().y
        );
        assert_eq!(state.phase(), Phase::Roam);
        assert!(!state.thrown_flight(), "落地后甩出标记清零");
    }

    /// 钳制边界探查：release_drag 初速度恰为闭式解边界 −v_up_max（最坏起点=屏顶）。
    ///
    /// 探查理由：`clamp_throw_velocity` 以严格小于（`vel.y < -v_up_max`）判定钳制，
    /// 边界值应原样放行；且边界速度的连续解恰在 T_eff=1420ms 落地——验证离散化
    /// 裕量（80ms）覆盖下最坏几何 + 最坏速度组合仍满足 FR-4-6 的 1500ms 硬上限。
    #[test]
    fn qa_probe_throw_at_exact_clamp_boundary_lands_within_1500ms() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        // 最坏起点 = 屏顶 y=0（可落高度 h 取工作区垂直跨度）。
        assert_eq!(state.begin_drag(Vec2::new(960.0, 0.0), 1_000), Some(Arbitration::Play));
        // 闭式解边界值（重力取配置真源 RV-18，无字面量）。
        let g = InteractionCfg::default().gravity_px_per_sec2;
        let h = mon_a().work_bottom_vdc() - mon_a().work_origin_vdc.y;
        let t_eff = (THROW_MAX_FLIGHT_MS - THROW_FLIGHT_MARGIN_MS) as f32 / 1000.0;
        let v_up_max = (g * t_eff / 2.0 - h / t_eff).max(0.0);
        let vel = Vec2::new(0.0, -v_up_max);
        // 边界值恰不触发钳制（严格小于判定）。
        let clamped = state.clamp_throw_velocity(vel);
        assert!(
            (clamped.y + v_up_max).abs() < 1e-2,
            "恰在边界的 vy 应原样放行：{} vs -{v_up_max}",
            clamped.y
        );
        assert_eq!(state.release_drag(Some(vel), 1_000), None, "toss 在播无补提交");
        assert_eq!(state.phase(), Phase::Fall);
        let mut landed_at = None;
        for k in 1..=40u64 {
            let now = 1_000 + k * 50;
            let out = state.logic_tick(now);
            if out.submitted.is_some() {
                landed_at = Some(now);
                break;
            }
        }
        let land = landed_at.expect("边界速度应在预算内落地");
        assert!(land - 1_000 <= 1_500, "边界 vy 落地实测 {}ms ≤ 1500", land - 1_000);
        assert_eq!(state.throw_landed_count(), 1, "边界速度甩出落地仍计数");
        assert!(
            (state.pos().y - 1040.0).abs() < 1e-3,
            "落点仍钳回工作区底边：{}",
            state.pos().y
        );
    }

    /// 非对称双屏探查：两台显示器拼接的 VD 包围矩形（屏1 1920×1040 工作区 +
    /// 屏2 1280×960 工作区），光标拖到屏 2 / 越界钳制 / 跨屏甩出落点。
    ///
    /// 探查理由：既有 `drag_target_clamps_to_virtual_desktop_bounds` 只用单屏
    /// 对称夹具；非对称拼接下 `vd_bounds_of`（并集水平界）与 `vd_vertical_span`
    /// （全局垂直跨度）是否跟手正确、跨屏甩出是否落在**所在屏**（屏 2）的工作区
    /// 底边 960（而非较高屏 1 的 1040），是 FR-4-5/4-6 的关键遗漏面。
    #[test]
    fn qa_probe_dual_monitor_asymmetric_vd_drag_clamp_and_cross_screen_throw() {
        // 屏 2：VDC x ∈ [1920, 3200)，工作区底边 960（比屏 1 的 1040 高）。
        let mon_c = MonitorGeom {
            id: 2,
            origin_vdc: Vec2::new(1920.0, 0.0),
            size_vdc: Vec2::new(1280.0, 1024.0),
            work_origin_vdc: Vec2::new(1920.0, 0.0),
            work_size_vdc: Vec2::new(1280.0, 960.0),
            primary: false,
        };
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a(), mon_c],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        assert_eq!(state.begin_drag(Vec2::new(960.0, 500.0), 50), Some(Arbitration::Play));
        // 拖到屏 2 内（VD 包围矩形内）→ 跟手原样保留（跨屏拖拽不误钳）。
        state.drag_to(Vec2::new(2_500.0, 400.0));
        state.logic_tick(100);
        assert_eq!(state.pos(), Vec2::new(2_500.0, 400.0), "屏 2 内拖拽点应原样保留");
        // 越界 → 钳到 VD 包围矩形右下（x=屏 2 右缘 3200；y=全局垂直跨度底 1040）。
        state.drag_to(Vec2::new(99_999.0, 99_999.0));
        state.logic_tick(150);
        assert_eq!(state.pos(), Vec2::new(3_200.0, 1_040.0), "越界钳到双屏外包矩形右下角");
        // 左越界 → 钳到并集左缘 0。
        state.drag_to(Vec2::new(-100.0, 100.0));
        state.logic_tick(200);
        assert_eq!(state.pos().x, 0.0, "左越界钳到并集左缘");

        // 跨屏甩出：从屏 2 起抛（vx>0）→ 落点仍在屏 2 工作区（y=屏 2 底边 960）。
        state.drag_to(Vec2::new(2_500.0, 500.0));
        state.logic_tick(250);
        let release_ms = 300u64;
        assert_eq!(state.release_drag(Some(Vec2::new(200.0, -100.0)), release_ms), None);
        assert_eq!(state.phase(), Phase::Fall);
        let mut landed_at = None;
        for k in 1..=40u64 {
            let now = release_ms + k * 50;
            let out = state.logic_tick(now);
            if out.submitted.is_some() {
                landed_at = Some(now);
                break;
            }
        }
        let land = landed_at.expect("跨屏甩出应在预算内落地");
        assert!(land - release_ms <= 1_500, "跨屏甩出落地实测 {}ms", land - release_ms);
        assert!(
            state.pos().x >= 1_920.0 && state.pos().x <= 3_200.0,
            "落点 x 应留在屏 2 水平域（反弹界=并集）：{}",
            state.pos().x
        );
        assert!(
            (state.pos().y - 960.0).abs() < 1e-3,
            "落点 y 应为屏 2 工作区底边 960（所在屏口径）：{}",
            state.pos().y
        );
        assert_eq!(state.throw_landed_count(), 1);
    }

    /// 飞行中抓回探查：甩出后未落地即 begin_drag（空中抓回）→ 拖回地面轻放。
    ///
    /// 探查理由：`thrown_flight` 清零（落地链作废）只在 begin_drag 实现路径有
    /// 注释、无既有测试覆盖；若未清零，轻放会误产 ACT-T-07→08 抗议链与 C8
    /// 计数，直接违背 AC-09 与 FR-4-6「轻放无惩罚」。
    #[test]
    fn qa_probe_catch_mid_flight_voids_throw_landing_chain() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        assert_eq!(state.begin_drag(Vec2::new(960.0, 300.0), 1_000), Some(Arbitration::Play));
        assert_eq!(state.release_drag(Some(Vec2::new(0.0, -500.0)), 1_000), None);
        assert!(state.thrown_flight(), "起飞后处于甩出飞行");
        // 飞行中一拍（未落地）。
        let out = state.logic_tick(1_050);
        assert!(out.submitted.is_none(), "飞行中不结算");
        assert_eq!(state.phase(), Phase::Fall);
        // 空中抓回 → 拖拽相、甩出标记清零。
        state.begin_drag(Vec2::new(960.0, 300.0), 1_100);
        assert_eq!(state.phase(), Phase::Drag);
        assert!(!state.thrown_flight(), "飞行中抓回应清零甩出标记（落地链作废）");
        // 拖回地面轻放 → 合法站立面直接回 Roam。
        state.drag_to(Vec2::new(960.0, 1_040.0));
        state.logic_tick(1_150);
        assert_eq!(state.release_drag(None, 1_200), None, "轻放无仲裁产出");
        assert_eq!(state.phase(), Phase::Roam);
        // 落地链作废：无 07/08、计数不增。
        assert_eq!(state.throw_landed_count(), 0, "抓回后轻放不计甩出落地");
        let ids: Vec<&str> =
            state.arbiter.queue_snapshot().iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"ACT-T-07") && !ids.contains(&"ACT-T-08"),
            "落地链应作废，队列不得含 07/08：{ids:?}"
        );
    }

    /// 源码缺陷复现探针：飞行中抓回（Fall → begin_drag）时 toss（ACT-T-06）已在播，
    /// `begin_drag` 无条件 `submit_toss`（未对齐 `release_drag` 的
    /// `ensure_toss_submitted` 口径）→ 同优先级重复 toss 排队。
    ///
    /// 期望行为：toss 已在播时不重复提交（返回 None、队列不新增）——与
    /// `release_drag` 起飞补提交的幂等语义一致。
    #[test]
    fn qa_probe_begin_drag_mid_flight_does_not_duplicate_toss() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        assert_eq!(state.begin_drag(Vec2::new(960.0, 300.0), 1_000), Some(Arbitration::Play));
        assert_eq!(state.release_drag(Some(Vec2::new(0.0, -500.0)), 1_000), None);
        let out = state.logic_tick(1_050);
        assert!(out.submitted.is_none());
        // 空中抓回：toss（p7）正在播 → 不应重复提交。
        assert_eq!(
            state.begin_drag(Vec2::new(960.0, 300.0), 1_100),
            None,
            "toss 已在播时 begin_drag 不应重复提交（ensure 口径）"
        );
    }

    // -- S3-M6：触发映射（fx_burst_for_intent）+ landed_thrown 落地标记 ------------

    #[test]
    fn fx_burst_maps_double_click_tickle_stroke_to_hearts() {
        let cfg = ClickFeedbackCfg::default();
        for kind in [InteractionKind::DoubleClick, InteractionKind::Tickle, InteractionKind::Stroke]
        {
            let cmd = fx_burst_for_intent(kind, &cfg).expect("三类意图应产爱心迸发");
            assert_eq!(cmd.kind, ParticleKind::Heart, "{kind:?} → heart");
            assert_eq!(cmd.count, FX_HEART_BURST_COUNT);
            assert_eq!(cmd.version, bridge::PARTICLE_CMD_VERSION);
        }
    }

    #[test]
    fn fx_burst_click_gated_by_click_feedback_config_and_clamped() {
        // 默认开启：Click → 微反馈尘土，数量取配置。
        let on = ClickFeedbackCfg { enabled: true, burst_count: 6 };
        let cmd = fx_burst_for_intent(InteractionKind::Click, &on).expect("开关开启应产尘土");
        assert_eq!(cmd.kind, ParticleKind::Dust);
        assert_eq!(cmd.count, 6);

        // 数量钳制：0→1、超限→60（与 PARTICLE_BURST_CAP 同源）。
        let big = ClickFeedbackCfg { enabled: true, burst_count: 500 };
        assert_eq!(
            fx_burst_for_intent(InteractionKind::Click, &big).expect("有产出").count,
            bridge::PARTICLE_BURST_CAP
        );
        let zero = ClickFeedbackCfg { enabled: true, burst_count: 0 };
        assert_eq!(fx_burst_for_intent(InteractionKind::Click, &zero).expect("有产出").count, 1);

        // 关闭：Click 零迸发。
        let off = ClickFeedbackCfg { enabled: false, burst_count: 6 };
        assert!(fx_burst_for_intent(InteractionKind::Click, &off).is_none());
    }

    #[test]
    fn fx_burst_ignores_non_visual_intents() {
        let cfg = ClickFeedbackCfg::default();
        for kind in [InteractionKind::Hover, InteractionKind::DragStart, InteractionKind::Throw] {
            assert!(
                fx_burst_for_intent(kind, &cfg).is_none(),
                "{kind:?} 不产粒子迸发"
            );
        }
    }

    #[test]
    fn logic_tick_marks_landed_thrown_only_on_throw_landing() {
        // 甩出落地：landed_thrown = true（run_loop 据此发落地尘土）。
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
            );
        assert_eq!(state.begin_drag(Vec2::new(960.0, 500.0), 50), Some(Arbitration::Play));
        assert_eq!(state.release_drag(Some(Vec2::new(0.0, -500.0)), 100), None);
        let mut marked = false;
        for k in 1..=60u64 {
            let out = state.logic_tick(100 + k * 50);
            if out.submitted.is_some() {
                assert!(out.landed_thrown, "甩出落地 tick 应置 landed_thrown");
                assert_eq!(state.throw_landed_count(), 1);
                marked = true;
                break;
            }
            assert!(!out.landed_thrown, "飞行中不置标记");
        }
        assert!(marked, "应在预算内落地");

        // 对照：自然下坠落地（非甩出）→ landed_thrown = false。
        let mut natural = CoreLoopState::new(
            Vec2::new(960.0, 500.0),
            Vec::new(),
            core_cfg(landing_catalog()),
            7,
            0,
            );
        let _ = natural.logic_tick(50); // 无站立面 → Fall
        for k in 1..=20u64 {
            let _ = natural.logic_tick(50 + k * 50);
        }
        natural.apply_monitors(vec![mon_low()], 1_100);
        for k in 0..300u64 {
            let out = natural.logic_tick(1_150 + k * 50);
            if out.submitted.is_some() {
                assert!(!out.landed_thrown, "自然落地不置标记");
                break;
            }
        }
    }

    // -- S4 前清障 B15-④：播放指令通道（出队起播 / 停播 / 链推进） ------------------

    /// 装配带播放通道的状态（默认 toss_catalog + mon_a 起点可换）。
    fn state_with_playback(
        pos: Vec2,
        monitors: Vec<MonitorGeom>,
        catalog: ActionCatalog,
    ) -> (CoreLoopState, PlaybackChannel) {
        let mut state = CoreLoopState::new(pos, monitors, core_cfg(catalog), 7, 0);
        let channel = PlaybackChannel::default();
        state.attach_playback(channel.clone());
        (state, channel)
    }

    /// 排空通道指令（保序）。
    fn drain_orders(channel: &PlaybackChannel) -> Vec<PlaybackOrder> {
        let mut orders = Vec::new();
        while let Some(order) = channel.try_pop_order() {
            orders.push(order);
        }
        orders
    }

    #[test]
    fn playback_play_order_emitted_on_action_start_and_reattach() {
        let (mut state, channel) =
            state_with_playback(Vec2::new(960.0, 1040.0), vec![mon_a()], toss_catalog());
        // 起播：DragStart → toss Play 下发覆盖态。
        assert_eq!(state.begin_drag(Vec2::new(960.0, 500.0), 50), Some(Arbitration::Play));
        assert_eq!(
            drain_orders(&channel),
            vec![PlaybackOrder::Play { action_id: "ACT-T-06".to_string() }],
            "起播型仲裁结果 → Play 指令"
        );

        // 起飞：停播循环 toss（下一用例详测）；空中抓回 → 重发 Play 重挂覆盖态。
        assert_eq!(state.release_drag(Some(Vec2::new(0.0, -300.0)), 100), None);
        assert_eq!(
            drain_orders(&channel),
            vec![PlaybackOrder::Stop],
            "起飞停播且不发 Play 抵消（B15-④ 裁定）"
        );
        assert_eq!(
            state.begin_drag(Vec2::new(1000.0, 600.0), 150),
            None,
            "toss 已在播不重复提交"
        );
        assert_eq!(
            drain_orders(&channel),
            vec![PlaybackOrder::Play { action_id: "ACT-T-06".to_string() }],
            "空中抓回 → 重发 Play 重挂覆盖态"
        );
    }

    #[test]
    fn playback_finished_report_advances_landing_chain() {
        let (mut state, channel) =
            state_with_playback(Vec2::new(960.0, 1040.0), vec![mon_a()], toss_catalog());
        assert_eq!(state.begin_drag(Vec2::new(960.0, 500.0), 50), Some(Arbitration::Play));
        assert_eq!(drain_orders(&channel).len(), 1, "toss 起播恰一条指令");

        // 起飞：Stop（仲裁 current 保留）。
        assert_eq!(state.release_drag(Some(Vec2::new(0.0, -500.0)), 100), None);
        assert_eq!(drain_orders(&channel), vec![PlaybackOrder::Stop]);

        // 落地：链提交（07 Queued{pos:1}、08 依序入队）→ 停播推进（toss 出局、07 起播）。
        let mut land_ms = None;
        for k in 1..=60u64 {
            let now = 100 + k * 50;
            let out = state.logic_tick(now);
            if out.submitted.is_some() {
                land_ms = Some(now);
                break;
            }
        }
        let land = land_ms.expect("1.5s 预算内应落地");
        assert_eq!(
            drain_orders(&channel),
            vec![
                PlaybackOrder::Stop,
                PlaybackOrder::Play { action_id: "ACT-T-07".to_string() },
            ],
            "落地停播 toss + 链首 07 出队起播（fade 0）"
        );
        let ids: Vec<&str> =
            state.arbiter.queue_snapshot().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["ACT-T-08"], "队列仅剩链尾 08（07 已成 current）");

        // 链推进：播放器回报 07 播毕 → 08 出队起播。
        channel.push_report(PlaybackReport::Finished { action_id: "ACT-T-07".to_string() });
        state.drain_finished(land);
        assert_eq!(
            drain_orders(&channel),
            vec![PlaybackOrder::Play { action_id: "ACT-T-08".to_string() }],
            "Finished 回报 → 链尾 08 出队起播"
        );
        assert!(state.arbiter.queue_snapshot().is_empty(), "链推进后队列清空");

        // 不匹配回报丢弃：伪 Finished 不产生任何指令。
        channel.push_report(PlaybackReport::Finished { action_id: "ACT-T-06".to_string() });
        state.drain_finished(land + 50);
        assert!(drain_orders(&channel).is_empty(), "不匹配 current 的回报丢弃");
    }

    #[test]
    fn playback_natural_landing_stops_looping_current_and_starts_landing_action() {
        // 非甩出 Landed 也停播循环 current（计划注记：停播口径扩至所有 Landed）。
        let (mut state, channel) =
            state_with_playback(Vec2::new(960.0, 1040.0), vec![mon_a()], toss_catalog());
        assert_eq!(state.begin_drag(Vec2::new(960.0, 1040.0), 50), Some(Arbitration::Play));
        assert_eq!(drain_orders(&channel).len(), 1, "toss 起播恰一条指令");
        // 拖回地面轻放（合法站立面 → 回 Roam；toss 仍 current、循环在播）。
        assert_eq!(state.release_drag(None, 100), None);
        assert_eq!(state.phase(), Phase::Roam);

        // 站立面消失 → 自然下坠 → 落地（非甩出）。
        state.apply_monitors(Vec::new(), 150);
        assert!(state.logic_tick(200).submitted.is_none(), "起坠不提交");
        state.apply_monitors(vec![mon_low()], 250);
        let mut landed = None;
        for k in 1..=400u64 {
            let now = 250 + k * 50;
            let out = state.logic_tick(now);
            if out.submitted.is_some() {
                landed = Some(now);
                break;
            }
        }
        assert!(landed.is_some(), "应在预算内落地");
        assert_eq!(state.throw_landed_count(), 0, "自然落地不计数");
        assert_eq!(
            drain_orders(&channel),
            vec![
                PlaybackOrder::Stop,
                PlaybackOrder::Play { action_id: "ACT-M-06".to_string() },
            ],
            "所有 Landed 口径：停播循环 toss + 落地缓冲覆盖起播"
        );
    }

    // -- S4 前清障 B15-④ QA 探针：落地链「先提交后停播推进」时序独立复核 --------

    /// QA 独立探针（第 1 轮）：链首 07 仲裁 verdict 必须为 `Queued{pos:1}`（依赖
    /// toss 仍在 current 位——「先提交后停播」裁定的核心语义），且 07/08 均不被
    /// R-A `Suppressed`；随后 Finished 回报驱动 08 出队，链完整排空、无残留指令。
    #[test]
    fn qa_probe_landing_chain_head_queued_and_both_links_start_without_suppression() {
        let (mut state, channel) =
            state_with_playback(Vec2::new(960.0, 1040.0), vec![mon_a()], toss_catalog());
        // 拖拽起飞 → toss 起播（current = ACT-T-06，循环）。
        assert_eq!(state.begin_drag(Vec2::new(960.0, 500.0), 50), Some(Arbitration::Play));
        assert_eq!(drain_orders(&channel).len(), 1);
        // 甩出起飞：Stop（current 保留）。
        assert_eq!(state.release_drag(Some(Vec2::new(0.0, -400.0)), 100), None);
        assert_eq!(drain_orders(&channel), vec![PlaybackOrder::Stop]);

        // 落地 tick：链头 verdict = Queued{pos:1}（非 Suppressed/Dropped/Play）。
        let mut land_ms = None;
        for k in 1..=60u64 {
            let now = 100 + k * 50;
            let out = state.logic_tick(now);
            if let Some(verdict) = out.submitted {
                assert_eq!(
                    verdict,
                    Arbitration::Queued { pos: 1 },
                    "链首 07 在 toss 仍 current 时入队（先提交后停播的关键语义）"
                );
                assert!(out.landed_thrown, "甩出落地标记");
                land_ms = Some(now);
                break;
            }
        }
        let land = land_ms.expect("1.5s 预算内应落地");
        // 同 tick 内停播推进：toss Stop + 07 出队起播（fade 0），顺序不可反。
        assert_eq!(
            drain_orders(&channel),
            vec![
                PlaybackOrder::Stop,
                PlaybackOrder::Play { action_id: "ACT-T-07".to_string() },
            ],
            "落地 tick：先 Stop 停 toss，后 Play 07（提交序已先行）"
        );
        // 链尾 08 仍在队列（未丢失、未 Suppressed）。
        assert_eq!(
            state
                .arbiter
                .queue_snapshot()
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ACT-T-08"]
        );
        // 播放器回报 07 播毕 → 08 出队起播 → 队列清空。
        channel.push_report(PlaybackReport::Finished { action_id: "ACT-T-07".to_string() });
        state.drain_finished(land);
        assert_eq!(
            drain_orders(&channel),
            vec![PlaybackOrder::Play { action_id: "ACT-T-08".to_string() }],
            "07 Finished → 08 出队起播（链尾推进）"
        );
        channel.push_report(PlaybackReport::Finished { action_id: "ACT-T-08".to_string() });
        state.drain_finished(land + 50);
        assert!(drain_orders(&channel).is_empty(), "08 播毕队列已空 → 零指令");
        assert!(state.arbiter.queue_snapshot().is_empty());
        assert!(state.arbiter.current().is_none(), "链排空后 current 清空");
    }

    // -- S4-M1：业务档接线（情绪内核 + 会话暂停） ----------------------------------

    #[test]
    fn business_tick_drives_emotion_engine_and_accumulates_pressure_while_present() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(landing_catalog()),
            7,
            0,
        );
        // 业务档墙钟（C3：端口注入；`FakeWallClock` 是 dp-core 提供的测试替身）。
        let wall = FakeWallClock::new(1_800_000_000_000);
        // 绕开平台会话查询（本机无头环境恒 `Disconnected` → 恒暂停，速率恒 0）。
        state.emotion.state.pause.observe(false, wall.now_ms());

        // 首拍建基线（不累积）。
        state.business_tick_present(&wall);
        assert!(
            state.emotion.neglect.p.abs() < f32::EPSILON,
            "首拍不产生累积（避免把启动前空闲算成冷落）"
        );

        // 在场（idle = 0）推进 10 分钟：P 应按在场速率增长。
        for _ in 0..600 {
            wall.advance_ms(1_000);
            state.business_tick_present(&wall);
        }
        let here_p = state.emotion.neglect.p;
        assert!(here_p > 0.0, "在场 10min 后 P 应增长（实测 {here_p}）");
    }

    #[test]
    fn business_tick_with_idle_beyond_threshold_uses_away_factor() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(landing_catalog()),
            7,
            0,
        );
        let wall = FakeWallClock::new(1_800_000_000_000);
        state.business_tick(&wall, None);

        // 注入「远超离场阈值」的空闲（1h > awayThresholdSec），并跨过迟滞窗口。
        //
        // 注意：本机（无头 / 远程会话）的 `query_session_state()` 可能返回
        // `Disconnected`，此时 `SessionWatcher` 会判定暂停、`tick_1s` 提前返回，
        // 速率恒 0 —— 这是**正确**行为而非缺陷。为让本用例只检验「离场因子」这一
        // 条路径，这里绕过会话监视、直接把内置内核置为非暂停态。
        state.emotion.state.pause.observe(false, wall.now_ms());
        state.presence_idle_ms = Some(3_600_000);
        for _ in 0..600 {
            wall.advance_ms(1_000);
            state.business_tick_present(&wall);
        }
        let away_rate = state.emotion.neglect.rate_per_min;
        // 离场因子 0.05 → 速率应落在 0.5 以下（若被 rateClamp.min 抬到 0.5 即回归失败）。
        assert!(
            away_rate > 0.0 && away_rate < 0.5,
            "离场速率应显著低于钳制下限（实测 {away_rate}）"
        );
        // RV-16 口径：离场 10min ≈ P 0.5 量级，绝不可接近在场速率。
        assert!(
            state.emotion.neglect.p < 10.0,
            "离场 10min 后 P 应极小（实测 {}）",
            state.emotion.neglect.p
        );
    }

    #[test]
    fn business_tick_pause_window_is_recorded_into_emotion_state() {
        // 会话暂停（锁屏）期间 P 冻结且暂停时间戳入状态（P2-2 裁定：
        // 起止时间戳入状态，S5-M2 只读）。
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(landing_catalog()),
            7,
            0,
        );
        let wall = FakeWallClock::new(1_800_000_000_000);
        state.business_tick(&wall, None);
        assert!(!state.emotion.state.pause.is_paused());

        // 直接驱动内核的暂停窗口（`SessionWatcher` 的平台查询在单测中不可控，
        // 故此处以状态层为断点验证「暂停 → 时间戳入状态 → 累计」的口径）。
        let start = wall.now_ms();
        state.emotion.state.pause.observe(true, start);
        assert!(state.emotion.state.pause.is_paused());
        assert_eq!(state.emotion.state.pause.start_ms, Some(start));

        wall.advance_ms(30 * 60 * 1_000);
        assert_eq!(state.emotion.state.pause.current_span_ms(wall.now_ms()), 30 * 60 * 1_000);

        // 解锁：窗口闭合、累计入账、不处于暂停。
        state.emotion.state.pause.observe(false, wall.now_ms());
        assert!(!state.emotion.state.pause.is_paused());
        assert_eq!(state.emotion.state.pause.accumulated_ms, 30 * 60 * 1_000);
        assert_eq!(state.emotion.state.pause.count, 1);
    }

    // ==================== S4-M2：事件总线与 1Hz tick 链路 ====================

    /// 交互增益跨档暂存 → 单次消费（**幂等**：喂入即清零，不重复计入）。
    #[test]
    fn pending_mood_delta_is_consumed_once_per_business_tick() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
        );
        let wall = FakeWallClock::new(1_800_000_000_000);

        // 首拍建立基线（不消费累积）。
        state.business_tick_present(&wall);
        state.record_interaction_mood(3.0);
        state.record_interaction_mood(2.5);
        assert_eq!(state.pending_mood_delta(), 5.5, "跨档暂存应累加");

        // 业务档消费后清零（幂等：连续两次 tick 不会重复计入）。
        wall.advance_ms(1_000);
        state.business_tick_present(&wall);
        assert_eq!(state.pending_mood_delta(), 0.0, "喂入内核后必须清零");

        // 清零后的下一 tick 不再产生额外增益 → Mood 只受惯性驱动（单调不回升）。
        let mood_after = state.emotion.state.values.mood;
        wall.advance_ms(1_000);
        state.business_tick_present(&wall);
        assert!(
            state.emotion.state.values.mood <= mood_after,
            "无新交互时 Mood 不得因残留增益回升"
        );
    }

    /// 非有限增益防御（NaN / inf 不得污染内核数值）。
    #[test]
    fn record_interaction_mood_ignores_non_finite() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
        );
        state.record_interaction_mood(f32::NAN);
        state.record_interaction_mood(f32::INFINITY);
        state.record_interaction_mood(2.0);
        assert_eq!(state.pending_mood_delta(), 2.0, "仅有限值计入");
    }

    /// 暂停期间暂存**不清零**（内核早退不消费，恢复后一次计入）。
    #[test]
    fn pending_delta_survives_paused_tick() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
        );
        let wall = FakeWallClock::new(1_800_000_000_000);
        state.business_tick_present(&wall);
        state.record_interaction_mood(4.0);

        // 暂停态 tick：内核早退，暂存必须保留。
        wall.advance_ms(1_000);
        state.business_tick_inner(&wall, wall.now_ms(), true, true, None);
        assert_eq!(state.pending_mood_delta(), 4.0, "暂停期间不得丢弃待喂增益");

        // 恢复后一次计入并清零。
        wall.advance_ms(1_000);
        state.business_tick_present(&wall);
        assert_eq!(state.pending_mood_delta(), 0.0);
    }

    // -- S7-M9：生存动作触发面接入（AC-21/22） ----------------------------------

    /// 真实 needs.json 分档解析（S7-M9 应用层测试夹具；数值口径 = 随包配置）。
    fn real_needs_cfg() -> dp_core::config::model::NeedsConfig {
        let text = std::fs::read_to_string(resources_config_dir().join("needs.json"))
            .expect("needs.json 可读");
        serde_json::from_str(&text).expect("needs.json 可解析")
    }

    /// 真实目录 + 真实分档（资源批次 B 未交付，N 系列 `disabled=true`）：
    /// 分档轮询与喂食 / 洗澡事务端点**不 panic、不提交**——触发面就绪，待资源
    /// （`02 §10.1` R3：交付后零改动实播）。
    #[test]
    fn needs_actions_armed_but_disabled_skip_submission() {
        let cfg = CoreCfg {
            catalog: ActionCatalog::load(&resources_config_dir()).expect("真实目录可加载"),
            needs: real_needs_cfg(),
            ..core_cfg(toss_catalog())
        };
        let mut state = CoreLoopState::new(Vec2::new(960.0, 1040.0), vec![mon_a()], cfg, 7, 0);

        // ① 分档轮询：Satiety=30（peckish）→ ACT-N-01 触发面到点；disabled → 不提交。
        state.emotion.state.values.satiety = 30.0;
        state.emotion.state.values.cleanliness = 80.0;
        state.needs_action_tick(1_000, None);
        assert!(state.arbiter.current().is_none(), "disabled 不得起播");
        assert_eq!(state.arbiter.queue_len(), 0, "disabled 不得入队");

        // ② 喂食事务端点：N-02 / N-03 disabled → 不提交。
        let _ = state.apply_core_input(CoreInput::TrayFeed, 1_000);
        assert!(state.arbiter.current().is_none(), "喂食端点不得起播");

        // ③ 洗澡事务端点：N-07 / N-08 disabled → 不提交。
        let _ = state.apply_core_input(CoreInput::TrayBath, 1_000);
        assert!(state.arbiter.current().is_none(), "洗澡端点不得起播");
    }

    /// 解除 disabled（模拟资源批次 B 交付）→ 同一轮询 / 事务路径**确有提交**
    /// （AC-21：讨食 N-01 起播；喂食完成且饱食 ≥ 满档 → N-02/N-03 提交）。
    #[test]
    fn needs_actions_submit_when_enabled() {
        let catalog = ActionCatalog::load(&resources_config_dir()).expect("真实目录可加载");
        let enabled = ActionCatalog::from_actions(
            catalog
                .all()
                .iter()
                .map(|c| {
                    let mut c = c.clone();
                    if c.id.starts_with("ACT-N-0") {
                        c.disabled = false;
                    }
                    c
                })
                .collect(),
        );
        let cfg = CoreCfg {
            catalog: enabled,
            needs: real_needs_cfg(),
            ..core_cfg(toss_catalog())
        };
        let mut state = CoreLoopState::new(Vec2::new(960.0, 1040.0), vec![mon_a()], cfg, 7, 0);

        // ① 讨食轮询 → N-01 起播（fresh 仲裁器 → Play）。
        state.emotion.state.values.satiety = 30.0;
        state.emotion.state.values.cleanliness = 80.0;
        state.needs_action_tick(1_000, None);
        let current = state.arbiter.current().expect("解除 disabled 后应起播");
        assert_eq!(current.request.id, "ACT-N-01", "讨食应起播");

        // ② 喂食事务端点（饱食 ≥ 满档）→ N-02 开始 + N-03 满足，至少一个提交。
        state.emotion.state.values.satiety = 80.0;
        let _ = state.apply_core_input(CoreInput::TrayFeed, 2_000);
        assert!(
            state.arbiter.queue_len() > 0 || state.arbiter.current().is_some(),
            "N-02/N-03 应至少提交一个"
        );
    }

    /// 快照契约（`02 §4.3`）：1Hz 投影字段齐备且数值与内核同源。
    #[test]
    fn snapshot_contract_matches_core_state() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
        );
        let wall = FakeWallClock::new(1_800_000_000_000);
        state.business_tick_present(&wall);

        let snap = state.snapshot_for_test();
        assert_eq!(snap.v, 2, "契约版本恒为 2");
        assert_eq!(snap.values.mood, state.emotion.state.values.mood);
        assert_eq!(snap.values.boredom, state.emotion.boredom_display());
        assert_eq!(snap.neglect.level, state.emotion.neglect.level);
        assert_eq!(snap.neglect.p, state.emotion.neglect.p);
        assert!(snap.activity.is_none(), "活动快照归 S8，本卡恒 null");
        assert!(snap.inventory.is_empty());
        assert!(snap.skills.is_empty());
        // 线上序列化：camelCase 逐项到位（前端契约）。
        let json = serde_json::to_value(&snap).expect("快照可序列化");
        assert!(json["values"].get("affinityLevel").is_some());
        assert!(json["neglect"].get("ratePerMin").is_some());
        assert!(json["neglect"]["factors"].get("product").is_some());
    }

    /// `ForceAction` → `arbiter.submit`（`02 §6.2`：情绪动作优先级 ≥7）。
    #[test]
    fn force_action_submits_to_arbiter_with_emotion_source() {
        // 目录含一个「离家出走」档待机动作（`emotion.json.levels[4].idlePool` 首选）。
        let catalog = ActionCatalog::from_actions(vec![ActionCfg {
            id: "ACT-T-07".to_string(),
            name: "甩出落地".to_string(),
            category: "interact".to_string(),
            priority: 8,
            interruptible: true,
            min_interrupt_priority: 9,
            looping: false,
            fps: 15,
            fade_ms: 200,
            disabled: false,
            ..ActionCfg::default()
        }]);
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(catalog),
            7,
            0,
        );
        let events = vec![EmotionEvent::ForceAction {
            action_id: "ACT-T-07".to_string(),
            priority: 8,
        }];
        state.dispatch_emotion_events(&events, 1_000, None);

        let current = state.arbiter.current().expect("强制动作应进入仲裁器");
        assert_eq!(current.request.id, "ACT-T-07");
        assert_eq!(current.request.source, ActionSource::Emotion);
    }

    /// `ForceAction` 指向目录外动作 → 降级不提交（不 panic，`02 §7.4.2`）。
    #[test]
    fn force_action_missing_in_catalog_degrades_without_panic() {
        let mut state = CoreLoopState::new(
            Vec2::new(960.0, 1040.0),
            vec![mon_a()],
            core_cfg(toss_catalog()),
            7,
            0,
        );
        let events = vec![EmotionEvent::ForceAction {
            action_id: "ACT-ZZZZ".to_string(),
            priority: 8,
        }];
        state.dispatch_emotion_events(&events, 1_000, None);
        assert!(state.arbiter.current().is_none(), "目录缺失时不得凭空占位");
    }

    /// 确定性（验收标准 (b)）：**给定输入序列 → 固定输出**。
    ///
    /// 两组同构状态喂入完全相同的 tick 序列，末态与全程 Mood 轨迹必须逐点相等
    /// （内核零随机、零时钟，P/Mood 均为纯函数递推）。
    #[test]
    fn emotion_tick_is_deterministic_for_identical_input_sequence() {
        let drive = || {
            let mut state = CoreLoopState::new(
                Vec2::new(960.0, 1040.0),
                vec![mon_a()],
                core_cfg(toss_catalog()),
                7,
                0,
            );
            let wall = FakeWallClock::new(1_800_000_000_000);
            let mut trace = Vec::new();
            for k in 0..180u32 {
                wall.advance_ms(1_000);
                state.business_tick_present(&wall);
                trace.push((
                    state.emotion.state.values.mood,
                    state.emotion.neglect.p,
                    state.emotion.neglect.level,
                ));
                // 每 60 拍注入一次正向交互，制造非平凡轨迹。
                if k % 60 == 59 {
                    state.record_interaction_mood(2.0);
                }
            }
            trace
        };
        let a = drive();
        let b = drive();
        assert_eq!(a.len(), 180);
        assert_eq!(a, b, "相同输入序列必须产出逐点相同的输出轨迹");
    }

    /// 事件按序发出（验收标准 (a)）：阶段迁移事件顺序 = 从 → 到 逐级递进。
    #[test]
    fn cold_level_events_are_emitted_in_ascending_order() {
        // 用极小阈值构造「一 tick 内连跳多级」的极端场景（阈值全为 1）。
        let mut emotion_cfg = EmotionConfig::default();
        emotion_cfg.thresholds.l1 = 1;
        emotion_cfg.thresholds.l2 = 2;
        emotion_cfg.thresholds.l3 = 3;
        emotion_cfg.thresholds.l4 = 4;
        emotion_cfg.thresholds.l5 = 5;
        emotion_cfg.confirm.up_sec = 0; // 无确认期：立即生效
        emotion_cfg.personality.threshold_scale_base = 1.0;
        emotion_cfg.personality.threshold_scale_temper = 0.0;
        emotion_cfg.levels.truncate(6);

        let cfg: &'static EmotionConfig = Box::leak(Box::new(emotion_cfg));
        let needs: &'static NeedsConfig = Box::leak(Box::new(NeedsConfig::default()));
        let mut e = EmotionEngine::new(cfg, needs);

        // 本地时间经 `FakeWallClock` 端口取（C3：`dp-app` 不引入 `chrono` 依赖）。
        let clock = FakeWallClock::new(1_800_000_000_000);
        let mk_env = || TickEnv {
            now_local: clock.now_local(),
            session_paused: false,
            // 离场因子加速累积（本用例只关心顺序，不关心速率口径）。
            preset_idle_ms: u64::MAX,
            satiety: 100.0,
            cleanliness: 100.0,
            ..TickEnv::default()
        };

        // 首拍基线。
        e.tick_1s(1_000, mk_env());
        // 后续每拍累积（速率 = 离场因子 × 敏感度；每拍 ≥1 分钟量级的 ΔP）。
        let mut seen_levels = Vec::new();
        let mut last_from = 0u8;
        for k in 1..=20i64 {
            let now = 1_000 + k * 60_000;
            let out = e.tick_1s(now, mk_env());
            for ev in &out.events {
                if let EmotionEvent::ColdLevelChanged { from, to, .. } = ev {
                    assert_eq!(*from, last_from, "事件必须逐级衔接（不跳级、不乱序）");
                    assert!(*to > *from, "升级路径 to 必须大于 from");
                    last_from = *to;
                    seen_levels.push(*to);
                }
            }
        }
        assert!(!seen_levels.is_empty(), "该场景应至少触发一次阶段迁移");
        // 严格递增（无重复、无回退）。
        for w in seen_levels.windows(2) {
            assert!(w[0] < w[1], "阶段序列必须严格递增：{seen_levels:?}");
        }
    }

    // -----------------------------------------------------------------------
    // S4-M5：台词 / 气泡生产链路
    // -----------------------------------------------------------------------

    fn state_with_lines() -> CoreLoopState {
        CoreLoopState::new(Vec2::ZERO, vec![mon_a()], core_cfg(landing_catalog()), 1, 0)
    }

    fn level_event(to: u8) -> EmotionEvent {
        EmotionEvent::ColdLevelChanged {
            from: to.saturating_sub(1),
            to,
            mood_delta: -5.0,
            redirected: false,
            reason: dp_core::emotion::ColdReason::Accumulate,
        }
    }

    /// 阶段迁移 → 池键取 `emotion.json.levels[to].linePool`（配置驱动），且 `{name}` 已被替换。
    #[test]
    fn level_change_bubble_uses_level_pool_and_renders_name() {
        let mut st = state_with_lines();
        let bubble = st
            .derive_bubble_for_test(&level_event(2), 0)
            .expect("L2 迁移应产出气泡");
        assert_eq!(bubble.cooldown_key, "aggrieved", "L2 对应 aggrieved 池");
        // 文案必须是「该池某条经 `{name}` 渲染后」的结果（池内含 `{name}` 的行渲染后
        // 与原文不同，故不能拿原始池直接比对）。
        let rendered: Vec<String> = test_lines()
            .pool("aggrieved")
            .expect("aggrieved 池存在")
            .iter()
            .map(|line| st.render_line(line))
            .collect();
        assert!(
            rendered.contains(&bubble.text),
            "文案必须来自 aggrieved 池（渲染后），实际 {:?}",
            bubble.text
        );
        assert!(!bubble.text.contains("{name}"), "生产端必须完成 {{name}} 渲染（C2）");
        assert_eq!(st.render_line("{name}"), name_cp(), "渲染变量取 character.json 默认名");
    }

    /// 同池冷却：窗口内第二次不再出话（`01 §6.5.4` 同状态 ≥20s）。
    #[test]
    fn bubble_respects_pool_cooldown() {
        let mut st = state_with_lines();
        assert!(st.derive_bubble_for_test(&level_event(2), 0).is_some());
        assert!(
            st.derive_bubble_for_test(&level_event(2), 1_000).is_none(),
            "1s 内同池第二次必须被冷却拦下"
        );
        assert!(
            st.derive_bubble_for_test(&level_event(2), 20_000).is_some(),
            "恰达 20s 冷却窗口应放行"
        );
    }

    /// 三部曲完成 → `runaway` 池（含 AC-04「哼…原谅你啦，下不为例！」文案），非 preempt。
    #[test]
    fn coax_success_bubble_uses_runaway_pool() {
        let mut st = state_with_lines();
        let bubble = st
            .derive_bubble_for_test(&EmotionEvent::CoaxSucceeded { mood: 60.0 }, 0)
            .expect("三部曲完成应产出气泡");
        assert_eq!(bubble.cooldown_key, "runaway");
        let pool = test_lines().pool("runaway").expect("runaway 池存在").to_vec();
        let rendered: Vec<String> = pool.iter().map(|line| st.render_line(line)).collect();
        assert!(rendered.contains(&bubble.text), "文案必须来自 runaway 池（渲染后）");
        assert!(!bubble.preempt);

        // AC-04 文案确在池内（以码点构造期望子串，避免测试硬编码中文长的脆弱性）。
        let ac04_tail: String = ['\u{4E0B}', '\u{4E0D}', '\u{4E3A}', '\u{4F8B}'].iter().collect();
        assert!(
            pool.iter().any(|line| line.contains(&ac04_tail)),
            "runaway 池必须含 AC-04 文案（下不为例）"
        );
    }

    /// 进入比心窗 → `happy` 池且 `preempt = true`（用户交互台词即时覆盖系统台词）。
    #[test]
    fn coax_heart_bubble_is_preempt_from_happy_pool() {
        let mut st = state_with_lines();
        let bubble = st
            .derive_bubble_for_test(
                &EmotionEvent::CoaxProgress { ratio: 0.5, step: CoaxStep::Heart },
                0,
            )
            .expect("比心窗应产出气泡");
        assert_eq!(bubble.cooldown_key, "happy");
        assert!(bubble.preempt, "交互台词须标记 preempt");
        let rendered: Vec<String> = test_lines()
            .pool("happy")
            .expect("happy 池存在")
            .iter()
            .map(|line| st.render_line(line))
            .collect();
        assert!(rendered.contains(&bubble.text), "文案必须来自 happy 池（渲染后）");
    }

    /// 未装配音频 / 未登记意图 → 静默降级，绝不 panic。
    #[test]
    fn audio_requests_degrade_gracefully_without_bus() {
        let st = state_with_lines();
        assert_eq!(st.request_audio(AudioCue::EmotionHum), dp_audio::RequestOutcome::Closed);
        assert!(audio_cue_for_intent(InteractionKind::Hover).is_none());
        assert!(audio_cue_for_intent(InteractionKind::DragStart).is_none());
    }

    /// 装配音频总线后，请求进入 `dp-audio` 的有界队列（S4-M6 通路）。
    #[test]
    fn audio_requests_enqueue_when_bus_attached() {
        let (bus, rx) = AudioBus::channel(
            AudioSettings::default(),
            PathBuf::from("assets").join("audio"),
            dp_audio::AUDIO_QUEUE_CAP,
        );
        let mut cfg = core_cfg(landing_catalog());
        cfg.audio = Some(bus);
        let st = CoreLoopState::new(Vec2::ZERO, vec![mon_a()], cfg, 1, 0);
        assert_eq!(st.request_audio(AudioCue::EmotionHum), dp_audio::RequestOutcome::Enqueued);
        assert_eq!(rx.try_recv().ok(), Some(AudioCue::EmotionHum));
    }

    /// 意图 / 事件 → 音效 Cue 的映射表（`01 §9.2` 已落地触发面）。
    #[test]
    fn audio_cue_mapping_covers_landed_triggers() {
        assert_eq!(audio_cue_for_intent(InteractionKind::Click), Some(AudioCue::InteractHehe));
        assert_eq!(
            audio_cue_for_intent(InteractionKind::DoubleClick),
            Some(AudioCue::InteractHeart)
        );
        assert_eq!(audio_cue_for_intent(InteractionKind::Stroke), Some(AudioCue::InteractHeartbeat));
        assert_eq!(audio_cue_for_intent(InteractionKind::Tickle), Some(AudioCue::InteractYaa));
        assert_eq!(audio_cue_for_intent(InteractionKind::Throw), Some(AudioCue::MoveWind));

        assert_eq!(audio_cue_for_emotion(&level_event(1)), Some(AudioCue::EmotionSigh));
        assert_eq!(audio_cue_for_emotion(&level_event(4)), Some(AudioCue::EmotionAnger));
        assert_eq!(
            audio_cue_for_emotion(&EmotionEvent::CoaxSucceeded { mood: 60.0 }),
            Some(AudioCue::EmotionCheer)
        );
        assert_eq!(
            audio_cue_for_emotion(&EmotionEvent::CoaxProgress { ratio: 0.2, step: CoaxStep::Heart }),
            Some(AudioCue::InteractHeart)
        );
        // 中性事件不产音效。
        assert!(audio_cue_for_emotion(&EmotionEvent::PersistNow).is_none());
        assert!(audio_cue_for_emotion(&level_event(0)).is_none());
    }

    // ══════════════════════════════════════════════════════════════════
    // S5-M1 / S5-M2：存档落盘 + 离线补偿接入（T-14 段）
    // ══════════════════════════════════════════════════════════════════

    /// S5 测试用临时存档目录（目录名用测试名区分；C3：不依赖时钟）。
    fn save_temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("dp-app-s5m2-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败（测试前置）");
        dir
    }

    /// 造一个「上次 tick 在 `away_ms` 之前」的存档并**重新载入**（模拟真实重启路径）。
    fn save_store_away(name: &str, now_ms: i64, away_ms: i64) -> SaveStore {
        let dir = save_temp_dir(name);
        let (mut store, outcome) = SaveStore::load(&dir, now_ms);
        assert!(outcome.is_healthy(), "新建临时存档不应降级：{:?}", outcome.warnings);
        store.cache_mut().meta.last_tick_ms = now_ms - away_ms;
        store.flush_force(0).expect("写入测试存档");
        let (store, outcome) = SaveStore::load(&dir, now_ms);
        assert!(outcome.is_healthy(), "重载测试存档不应降级：{:?}", outcome.warnings);
        store
    }

    /// 装一个「带存档」的纯逻辑状态（`save: Some` → 内核经 `restore` 恢复）。
    fn state_with_save(catalog: ActionCatalog, save: SaveStore) -> CoreLoopState {
        CoreLoopState::new(
            Vec2::new(960.0, 500.0),
            Vec::new(),
            CoreCfg { save: Some(save), ..core_cfg(catalog) },
            7,
            0,
        )
    }

    /// 存档目录名必须与 `tauri.conf.json.productName` 一致（`01 FR-8-1`：`%APPDATA%\DesktopPet`）。
    #[test]
    fn save_dir_name_matches_tauri_product_name() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tauri.conf.json");
        let text = std::fs::read_to_string(&path).expect("应能读取 tauri.conf.json");
        let value: serde_json::Value =
            serde_json::from_str(&text).expect("tauri.conf.json 应为合法 JSON");
        assert_eq!(
            value["productName"].as_str(),
            Some(crate::bridge::SAVE_DIR_NAME),
            "存档目录名与 productName 漂移会导致存档路径与 `02 §6.1` 不符"
        );
    }

    /// AC-37 边界（3h/4h/5h/24h）+ AC-18（3h 不生气）+ RV-16 双保险（永不 L5）。
    ///
    /// 与 `dp-core` 的引擎级用例同口径，但走**完整装配路径**：
    /// `SaveStore::load` → `CoreLoopState::new`（`EmotionEngine::restore`）→ `compensate_offline`。
    #[test]
    fn offline_compensation_matches_ac37_boundaries_and_never_l5() {
        const MIN: i64 = 60_000;
        const HOUR: i64 = 3_600_000;
        let now = 1_800_000_000_000i64;
        let wall = FakeWallClock::new(now);

        // ① 30min → JustLeft（不衰减，L0）
        let mut s = state_with_save(greeting_catalog(), save_store_away("ac37-30min", now, 30 * MIN));
        assert_eq!(s.compensate_offline(&wall), Some(OfflineOutcome::JustLeft));
        assert_eq!(s.emotion.neglect.level, 0, "≤graceMin 不衰减");

        // ② 3h → L1 无聊（AC-18：不生气）
        let mut s = state_with_save(greeting_catalog(), save_store_away("ac37-3h", now, 3 * HOUR));
        assert_eq!(s.compensate_offline(&wall), Some(OfflineOutcome::ColdApplied(1)));
        assert_eq!(s.emotion.neglect.level, 1, "3h → P≈9 → L1（实际 P={}）", s.emotion.neglect.p);
        assert_ne!(s.emotion.neglect.level, 5, "关机 3h 绝不离家出走");

        // ③ 4h → L1~L2 临界（RV-16 原 Bug 验证点：绝不 L5）
        let mut s = state_with_save(greeting_catalog(), save_store_away("ac37-4h", now, 4 * HOUR));
        match s.compensate_offline(&wall) {
            Some(OfflineOutcome::ColdApplied(lv)) => {
                assert!(lv <= 2, "4h 应为 L1~L2 临界：L{lv}");
            }
            other => panic!("4h 应 ColdApplied，实际 {other:?}"),
        }
        assert!(s.emotion.neglect.p < 30.0, "4h 的 P 应低于 L3 阈值：{}", s.emotion.neglect.p);

        // ④ 5h → Longing（>4h，L2 委屈）
        let mut s = state_with_save(greeting_catalog(), save_store_away("ac37-5h", now, 5 * HOUR));
        assert_eq!(s.compensate_offline(&wall), Some(OfflineOutcome::Longing(2)));
        assert!(s.emotion.is_longing(), ">4h 应进入想念展示");

        // ⑤ 24h → P=72 < 120，封顶 L4（`min(4)` 双保险）
        let mut s = state_with_save(greeting_catalog(), save_store_away("ac37-24h", now, 24 * HOUR));
        assert_eq!(s.compensate_offline(&wall), Some(OfflineOutcome::Longing(4)));
        assert!(s.emotion.neglect.p < 120.0, "24h 上界 P=72 < 120：{}", s.emotion.neglect.p);

        // ⑥ 100h → 步数截断，仍 ≤ L4
        let mut s = state_with_save(greeting_catalog(), save_store_away("ac37-100h", now, 100 * HOUR));
        assert!(matches!(s.compensate_offline(&wall), Some(OfflineOutcome::Longing(_))));
        assert!(s.emotion.neglect.level <= 4, "任何离线时长一律封顶 L4");
        assert_ne!(s.emotion.state.emotion, EmotionState::Runaway, "离线不得进入离家出走");
    }

    /// 无离线（`awayMs == 0`，含全新安装）→ 不补偿、不产问候，行为与 S4 基线一致。
    #[test]
    fn zero_away_ms_is_a_noop_and_produces_no_greeting() {
        let now = 1_800_000_000_000i64;
        let wall = FakeWallClock::new(now);
        let mut s = state_with_save(greeting_catalog(), save_store_away("ac37-zero", now, 0));
        assert_eq!(s.compensate_offline(&wall), None);
        assert!(s.startup_greeting_for_test().is_none());
        assert_eq!(s.emotion.neglect.level, 0);
    }

    /// 全新安装 / 无 tick 锚点 → 既不恢复也不补偿。
    ///
    /// 防的是一条真实路径：存档曾被写出但**从未 tick**（`lastTickMs == 0`）。若不加锚点判定，
    /// `away_ms(now)` = 「距 1970 纪元」≈ 5 万小时 → 首次启动即补偿到封顶 L4，宠物
    /// 「一装上就很生气」。
    #[test]
    fn fresh_save_without_anchor_does_not_restore_or_compensate() {
        let now = 1_800_000_000_000i64;
        let wall = FakeWallClock::new(now);
        let dir = save_temp_dir("fresh-no-anchor");
        let (mut store, outcome) = SaveStore::load(&dir, now);
        assert_eq!(outcome.status, LoadStatus::Fresh);
        // 模拟「写出过但从未 tick」的历史档：数值非默认，锚点仍为 0。
        store.cache_mut().values.mood = 7.0;
        store.flush_force(0).expect("写入测试档");

        let mut s = state_with_save(greeting_catalog(), store);
        assert!(!s.save().unwrap().has_session_anchor());
        assert_eq!(s.compensate_offline(&wall), None, "无锚点 → 不补偿");
        assert!(s.startup_greeting_for_test().is_none());
        assert_eq!(s.emotion.neglect.level, 0, "全新档不得直接落到封顶档");
        assert_eq!(
            s.emotion.state.values.mood,
            EmotionConfig::default().dimensions.mood.default,
            "无锚点 → 不做 restore（走 new 的配置默认值）"
        );
    }

    /// 档位越界 / 无存档 → 不 panic（防御口径）。
    #[test]
    fn compensate_offline_without_save_is_none() {
        let wall = FakeWallClock::new(1_800_000_000_000);
        let mut s = CoreLoopState::new(
            Vec2::new(960.0, 500.0),
            Vec::new(),
            core_cfg(greeting_catalog()),
            7,
            0,
        );
        assert!(s.save().is_none());
        assert_eq!(s.compensate_offline(&wall), None, "无存档 → 不补偿");
        assert!(s.startup_greeting_for_test().is_none());
    }

    /// 回归问候 = `ACT-T-01` 挥手 + 档位台词池文案（`02 §6.1`）；占位符已渲染（C2）。
    #[test]
    fn startup_greeting_carries_act_t01_and_level_pool_line() {
        let now = 1_800_000_000_000i64;
        let wall = FakeWallClock::new(now);
        let mut s = state_with_save(greeting_catalog(), save_store_away("greeting-3h", now, 3 * 3_600_000));
        assert!(s.compensate_offline(&wall).is_some());
        let (action, bubble) = s.startup_greeting_for_test().expect("离线后应有回归问候");
        assert_eq!(action, STARTUP_GREETING_ACTION);
        assert_eq!(action, "ACT-T-01", "`02 §5.5` 口径表：JustLeft/回归统一播 ACT-T-01 挥手");
        let text = bubble.expect("随包 lines.json 应能取到 L1 档位台词");
        assert!(!text.is_empty());
        assert!(!text.contains('{'), "占位符应已渲染（C2）：{text}");
    }

    /// 落盘链路：内核推进 → `capture_from` 置脏 → 过合并窗口落盘 → 重载内容一致。
    #[test]
    fn save_tick_persists_kernel_state_and_reloads() {
        let now = 1_800_000_000_000i64;
        let mut s = state_with_save(greeting_catalog(), save_store_away("save-tick", now, 0));
        let path = s.save().unwrap().save_path().to_path_buf();

        // 推进 60s（1Hz 业务档一次；`business_tick_present` 绕开无头环境的会话查询）。
        let wall = FakeWallClock::new(now + 60_000);
        s.business_tick_present(&wall);
        let expected_mood = s.emotion.state.values.mood;
        let expected_p = s.emotion.neglect.p;

        // `now_mono=10_000`：距上次落盘（0）已过 2s 合并窗口且有变更 → 落盘。
        s.save_tick(10_000, wall.now_ms());
        assert!(!s.save().unwrap().is_dirty(), "落盘后脏位应清零");

        let dir = path.parent().expect("存档路径必有父目录").to_path_buf();
        let (reloaded, outcome) = SaveStore::load(&dir, wall.now_ms());
        assert!(outcome.is_healthy(), "{:?}", outcome.warnings);
        assert_eq!(reloaded.cache().values.mood, expected_mood);
        assert_eq!(reloaded.cache().emotion.neglect.p, expected_p);
        assert_eq!(reloaded.last_seen_ms(), wall.now_ms(), "lastSeenMs = 本次落盘墙钟");
        assert_eq!(reloaded.away_ms(wall.now_ms()), 0, "刚落盘 → 无离线");
    }

    /// `PersistNow` → **强制落盘**（跳过 30s 定时与 2s 合并窗口）；对照组证明「非强制不写」。
    ///
    /// 判别口径：先各落一次盘建立锚点，再**删掉主档**；随后在「距上次落盘仅 1ms」的时刻
    /// 再跑一拍 —— 对照组不应重建主档，实验组（注入 `PersistNow`）必须重建。
    #[test]
    fn persist_now_forces_flush_within_merge_window() {
        let now = 1_800_000_000_000i64;
        let save_path = |state: &CoreLoopState| {
            state.save().expect("测试态应装配存档").save_path().to_path_buf()
        };

        // 对照组：无 `PersistNow` → 未到点 → 不重建。
        let mut control =
            state_with_save(greeting_catalog(), save_store_away("persist-control", now, 0));
        let control_path = save_path(&control);
        control.save_tick(0, now);
        assert!(control_path.exists(), "首拍必落（建立 30s 锚点）");
        std::fs::remove_file(&control_path).expect("删主档失败（测试前置）");
        control.save_tick(1, now);
        assert!(!control_path.exists(), "距上次落盘仅 1ms 且无 PersistNow → 不得写入");

        // 实验组：注入 `PersistNow` → 同一时刻强制落盘。
        let mut forced =
            state_with_save(greeting_catalog(), save_store_away("persist-forced", now, 0));
        let forced_path = save_path(&forced);
        forced.save_tick(0, now);
        std::fs::remove_file(&forced_path).expect("删主档失败（测试前置）");
        forced.dispatch_emotion_events(&[EmotionEvent::PersistNow], now, None);
        forced.save_tick(1, now);
        assert!(forced_path.exists(), "PersistNow 必须跳过定时与合并窗口立即落盘");
        let (disk, outcome) = SaveStore::load(forced_path.parent().unwrap(), now);
        assert!(outcome.is_healthy());
        assert_eq!(disk.last_seen_ms(), now);
        assert!(!forced.save().unwrap().is_dirty());
    }

    /// `save_tick` 在未装配存档时是 no-op（纯逻辑模式零副作用）。
    #[test]
    fn save_tick_without_store_is_noop() {
        let mut s = CoreLoopState::new(
            Vec2::new(960.0, 500.0),
            Vec::new(),
            core_cfg(greeting_catalog()),
            7,
            0,
        );
        s.save_tick(999_999, 1_800_000_000_000);
        assert!(s.save().is_none());
    }
}
