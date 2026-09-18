//! 六份外置配置（`resources/config/*.json`）的 serde 数据模型。
//!
//! 出处（逐字对齐，见各结构体文档注释）：
//!   - `emotion.json`   → `02 §5.7`（v2 全结构）
//!   - `character.json` → `02 §5.8`
//!   - `needs.json`     → `02 §5.12` + coupling → `02 §5.10`
//!   - `animation.json` → `02 §5.22`（physics.parts 见 §5.20，本阶段空数组）
//!   - `settings.json`  → `01 §6.7 FR-7` + `02 K-3` + `02 §5.23` + `02` Q-18（合成）
//!   - `actions.json`   → `02 §5.11` 元数据样例 + `01 §6.3.2` 53 动作总表（53 条全量）
//!
//! 约定：
//!   - 所有结构体 `#[serde(rename_all = "camelCase", default)]`（C7：camelCase + 单位后缀；
//!     缺字段由 `Default` 实现补齐——即「内置默认」，R19 兜底的最终兜底值）；
//!   - `schemars` 仅在 `schema` feature（dev-only）下派生，默认构建零开销；
//!   - 除 `character.json` 的 `defaultName` 外，任何地方不得出现角色名硬编码（C2）。

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// settings.json —— 设置项默认值与范围
// ---------------------------------------------------------------------------

/// `settings.json` 根：设置项默认值与范围。
///
/// 合成出处：`01 §6.7 FR-7`（缩放/音量/开关/置顶/透明度/语言/敏感度）、
/// `02 K-3`（`gravityPxPerSec2` 单一真源 RV-18、roam 漫游参数）、
/// `02 §5.23`（交互死锁防护三层配置）、`02` Q-18（`privacy.activitySensing`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SettingsConfig {
    /// 配置版本。
    pub version: u32,
    /// 外观：缩放 / 透明度 / 语言。
    pub appearance: AppearanceCfg,
    /// 声音：主音量与静音。
    pub audio: AudioCfg,
    /// 行为开关：自动走动 / 勿扰 / 穿透 / 置顶策略。
    pub behavior: BehaviorCfg,
    /// 漫游参数（`02 K-3`：决策间隔 5~30s、避让光标 150px）。
    pub roam: RoamCfg,
    /// 交互：重力单一真源（RV-18）与死锁防护配置（`02 §5.23`）。
    pub interaction: InteractionCfg,
    /// 情绪敏感度设置（`01 FR-7-9` 三档）。
    pub emotion: EmotionSettingsCfg,
    /// 隐私开关（Q-18）。
    pub privacy: PrivacyCfg,
}

impl Default for SettingsConfig {
    fn default() -> Self {
        Self {
            version: 1,
            appearance: AppearanceCfg::default(),
            audio: AudioCfg::default(),
            behavior: BehaviorCfg::default(),
            roam: RoamCfg::default(),
            interaction: InteractionCfg::default(),
            emotion: EmotionSettingsCfg::default(),
            privacy: PrivacyCfg::default(),
        }
    }
}

/// 外观设置（`01 FR-7-2 / FR-7-6 / FR-7-7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AppearanceCfg {
    /// 当前缩放百分比（50~200，步进 10）。
    pub scale_percent: u32,
    /// 缩放下限百分比。
    pub scale_min_percent: u32,
    /// 缩放上限百分比。
    pub scale_max_percent: u32,
    /// 缩放步进百分比。
    pub scale_step_percent: u32,
    /// 主体不透明度百分比（60~100）。
    pub opacity_percent: u32,
    /// 透明度下限百分比。
    pub opacity_min_percent: u32,
    /// 透明度上限百分比。
    pub opacity_max_percent: u32,
    /// 界面语言（默认简中，预留 en-US 文案包）。
    pub language: String,
}

impl Default for AppearanceCfg {
    fn default() -> Self {
        Self {
            scale_percent: 100,
            scale_min_percent: 50,
            scale_max_percent: 200,
            scale_step_percent: 10,
            opacity_percent: 100,
            opacity_min_percent: 60,
            opacity_max_percent: 100,
            language: "zh-CN".to_string(),
        }
    }
}

/// 声音设置（`01 FR-7-3`：主音量 0~100 + 托盘一键静音）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AudioCfg {
    /// 主音量百分比（0~100；默认值为实现自定，`01` 仅冻结范围）。
    pub master_volume_percent: u32,
    /// 音量下限百分比。
    pub volume_min_percent: u32,
    /// 音量上限百分比。
    pub volume_max_percent: u32,
    /// 是否静音。
    pub muted: bool,
}

impl Default for AudioCfg {
    fn default() -> Self {
        Self {
            master_volume_percent: 80,
            volume_min_percent: 0,
            volume_max_percent: 100,
            muted: false,
        }
    }
}

/// 行为开关（`01 FR-7-4`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BehaviorCfg {
    /// 自动走动。
    pub auto_roam: bool,
    /// 勿扰模式。
    pub do_not_disturb: bool,
    /// 穿透模式。
    pub click_through: bool,
    /// 置顶策略：`Always` | `BelowFullscreen` | `Never`（`02 K-1`）。
    pub always_on_top_policy: String,
}

impl Default for BehaviorCfg {
    fn default() -> Self {
        Self {
            auto_roam: true,
            do_not_disturb: false,
            click_through: false,
            always_on_top_policy: "Always".to_string(),
        }
    }
}

/// 漫游参数（`02 K-3`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RoamCfg {
    /// 漫游步调节奏缩放系数（`02 K-3` `roamPace`）。
    pub pace: f32,
    /// 节奏可选档位（`01 §8.3` 设置面板「节奏 (●正常)」；与
    /// `emotion.sensitivityOptions` 同范式——**范围外置，UI 不硬编码数值**）。
    pub pace_options: Vec<f32>,
    /// 漫游决策间隔区间（秒，`02 K-3`：每 5~30s）。
    pub decision_interval_sec: [u64; 2],
    /// 避让光标半径（物理像素，`02 K-3`：150px 热区）。
    pub cursor_avoid_radius_px: u32,
    /// 行走速度（VDC 逻辑像素/秒；S2-M4 新增，`01 §6.2` FR-2-7「运动参数可调」
    /// 对照项。缺字段由容器级 `default` 兜底为内置默认，向后兼容）。
    pub walk_speed_px_per_sec: f32,
}

impl Default for RoamCfg {
    fn default() -> Self {
        Self {
            pace: 1.0,
            pace_options: vec![0.7, 1.0, 1.3],
            decision_interval_sec: [5, 30],
            cursor_avoid_radius_px: 150,
            walk_speed_px_per_sec: 60.0,
        }
    }
}

/// 交互配置（`02 K-3` RV-18 + `02 §5.23` 三层防护 + `02 §5 K-6` 手势参数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InteractionCfg {
    /// 重力加速度（`02 K-3` RV-18：`gravityPxPerSec2` 单一真源，默认 2400）。
    pub gravity_px_per_sec2: f32,
    /// 交互不可用时的在场因子（`02 §5.23`：与"不在场"同档，不归零）。
    pub unavailable_presence_factor: f32,
    /// 托盘替代入口开关（`02 §5.23` 首选解法）。
    pub tray_fallback_enabled: bool,
    /// 不可达自然回退等级下限（L4；L5 任何情况不开放）。
    pub unreachable_natural_floor_level: u32,
    /// 不可达自然回退保持时长（秒）。
    pub unreachable_hold_sec: u64,
    /// 不可达自然回退要求的无负向窗口时长（秒）。
    pub unreachable_no_negative_sec: u64,
    /// 手势参数（S3-M3，`02 §5 K-6` 表；嵌套子对象，serde default 向后兼容——
    /// 旧 settings.json 缺 `gesture` 段时整体取内置默认，R19）。
    pub gesture: GestureCfg,
    /// 单击微反馈（S3-M6 触发映射：Click → 微反馈尘土；`interaction.clickFeedback`
    /// 嵌套子对象，serde default 向后兼容——旧 settings.json 缺段时取内置默认）。
    pub click_feedback: ClickFeedbackCfg,
}

impl Default for InteractionCfg {
    fn default() -> Self {
        Self {
            gravity_px_per_sec2: 2400.0,
            unavailable_presence_factor: 0.05,
            tray_fallback_enabled: true,
            unreachable_natural_floor_level: 4,
            unreachable_hold_sec: 180,
            unreachable_no_negative_sec: 7200,
            gesture: GestureCfg::default(),
            click_feedback: ClickFeedbackCfg::default(),
        }
    }
}

/// 单击微反馈配置（S3-M6 触发映射的配置开关；`interaction.clickFeedback` 段）。
///
/// 裁定（主理人 brief）：Click → 微反馈尘土，走**配置开关**而非写死行为；
/// serde camelCase + `default` 前向兼容（旧配置缺段不致解析失败，R19）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ClickFeedbackCfg {
    /// 是否启用单击微反馈（尘土迸发）。
    pub enabled: bool,
    /// 单次微反馈迸发数量（消费端再钳 `maxPerBurst=60` 上限）。
    pub burst_count: u32,
}

impl Default for ClickFeedbackCfg {
    fn default() -> Self {
        Self { enabled: true, burst_count: 6 }
    }
}

/// 手势参数（S3-M3，`02 §5 K-6` 参数表；14 键全部 camelCase + 单位后缀，C7）。
///
/// 数值外置 `settings.json` 的 `interaction.gesture` 子对象（裁定8：嵌套 + serde
/// default 向后兼容）；缺字段由本 Default 兜底（K-6 表内置默认）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GestureCfg {
    /// 悬停进入时长（ms；K-6：光标停留满 600ms → Hover）。
    pub hover_enter_ms: u64,
    /// 悬停长驻时长（ms；K-6：>2s → ACT-S-01）。
    pub hover_long_ms: u64,
    /// 双击判定窗（ms；K-6：300ms 内第二次按下 → 双击）。
    pub double_click_window_ms: u64,
    /// 长按阈值（ms；K-6：按住满 500ms 且速度达标 → 抚摸域）。
    pub long_press_ms: u64,
    /// 抚摸速度上限（px/s；K-6：速度 <800 判抚摸，否则分流拖拽）。
    pub stroke_speed_max_px_per_sec: u32,
    /// 拖拽分流位移（px；K-6：位移 >8px 分流拖拽）。
    pub drag_threshold_px: u32,
    /// 甩出速度阈值（px/s；K-6：松手速度 >1200 → 甩出）。
    pub throw_speed_min_px_per_sec: u32,
    /// 连点滚动窗（ms；K-6：2s 内累计点击）。
    pub tickle_window_ms: u64,
    /// 连点触发次数（次；K-6：窗内 ≥5 击 → 戳痒）。
    pub tickle_clicks_min: u32,
    /// 轨迹缓冲点数（点；FR-4-9：最近 32 采样点）。
    pub trail_max_points: u32,
    /// 画圈净转角阈值（度；FR-4-9：>270° 判画圈）。
    pub circle_turn_min_deg: u32,
    /// 直线残差阈值（px；FR-4-9：拟合残差 <12px 判直线）。
    pub line_residual_max_px: u32,
    /// Z 字反转次数阈值（次；FR-4-9：≥2 次方向反转）。
    pub zigzag_reversal_min: u32,
    /// Z 字反转夹角阈值（度；FR-4-9：夹角 >60°）。
    pub zigzag_turn_min_deg: u32,
}

impl Default for GestureCfg {
    fn default() -> Self {
        Self {
            hover_enter_ms: 600,
            hover_long_ms: 2000,
            double_click_window_ms: 300,
            long_press_ms: 500,
            stroke_speed_max_px_per_sec: 800,
            drag_threshold_px: 8,
            throw_speed_min_px_per_sec: 1200,
            tickle_window_ms: 2000,
            tickle_clicks_min: 5,
            trail_max_points: 32,
            circle_turn_min_deg: 270,
            line_residual_max_px: 12,
            zigzag_reversal_min: 2,
            zigzag_turn_min_deg: 60,
        }
    }
}

/// 情绪敏感度设置（`01 FR-7-9`：慢热 0.7 / 刚刚好 1.0 / 超黏人 1.3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EmotionSettingsCfg {
    /// 当前敏感度值。
    pub sensitivity_value: f32,
    /// 三档可选值。
    pub sensitivity_options: Vec<f32>,
}

impl Default for EmotionSettingsCfg {
    fn default() -> Self {
        Self {
            sensitivity_value: 1.0,
            sensitivity_options: vec![0.7, 1.0, 1.3],
        }
    }
}

/// 隐私开关（`02` Q-18：关闭后 presence/busyness 恒 1.0、退化为纯时间模型）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PrivacyCfg {
    /// 活跃感知开关。
    pub activity_sensing: bool,
}

impl Default for PrivacyCfg {
    fn default() -> Self {
        Self { activity_sensing: true }
    }
}

// ---------------------------------------------------------------------------
// schedule.json —— 时段与提醒默认间隔（`01 FR-10` / `02 §3`；S5-M5 交付并冻结）
// ---------------------------------------------------------------------------

/// `schedule.json` 根：提醒默认间隔与勿扰默认行为（`03 §4.2` 已登记该契约行）。
///
/// 出处：`01 FR-10-2`（久坐/喝水提醒，默认 45min，间隔可配；勿扰静默；点击「知道了」
/// 重新计时）与 `01 FR-10-4`（全局勿扰：暂停气泡与主动漫游，仅保留待机动画）。
///
/// 边界（S5-M5）：本文件只提供**默认值与时序参数**；提醒的**调度与触发**（计时器、
/// 气泡、`ACT-P-03` 演出）归 **S7-M2**——其卡片「前置依赖」已显式列出本模块
/// （提醒偏好），故此处不是悬空归口。用户改过的间隔写入存档（`settings.reminders`），
/// 本文件退回为「出厂默认 + 取值域」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ScheduleConfig {
    /// 配置版本。
    pub version: u32,
    /// 提醒默认间隔与开关。
    pub reminders: RemindersCfg,
    /// 勿扰默认行为。
    pub do_not_disturb: DoNotDisturbCfg,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            version: 1,
            reminders: RemindersCfg::default(),
            do_not_disturb: DoNotDisturbCfg::default(),
        }
    }
}

/// 提醒默认间隔（`01 FR-10-2`）。
///
/// `interval_min_min` / `interval_max_min` 是滑杆取值域（与 `settings.json` 的
/// `scaleMinPercent/scaleMaxPercent` 同范式：**范围随配置外置**，UI 不硬编码数值）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RemindersCfg {
    /// 久坐提醒开关默认值。
    pub sedentary_enabled: bool,
    /// 久坐提醒默认间隔（分钟；`01 FR-10-2` 默认 45）。
    pub sedentary_interval_min: u32,
    /// 喝水提醒开关默认值。
    pub water_enabled: bool,
    /// 喝水提醒默认间隔（分钟）。
    pub water_interval_min: u32,
    /// 间隔可配下界（分钟）。
    pub interval_min_min: u32,
    /// 间隔可配上界（分钟）。
    pub interval_max_min: u32,
    /// 点击「知道了」是否重新计时（`01 FR-10-2`）。
    pub ack_resets_timer: bool,
}

impl Default for RemindersCfg {
    fn default() -> Self {
        Self {
            sedentary_enabled: true,
            sedentary_interval_min: 45,
            water_enabled: true,
            water_interval_min: 45,
            interval_min_min: 15,
            interval_max_min: 180,
            ack_resets_timer: true,
        }
    }
}

/// 勿扰默认行为（`01 FR-10-4`）。
///
/// `mute_audio` 默认 `true` 与 **S4-M6 门控口径**一致：`resolve_play` 的优先级为
/// 「静音 > 音量 0 > 勿扰 > 穿透」，即勿扰态**不播**主动音效（K-6）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DoNotDisturbCfg {
    /// 出厂默认是否开启勿扰（`false`：首次安装不打扰用户）。
    pub default_on: bool,
    /// 勿扰是否暂停气泡。
    pub pause_bubbles: bool,
    /// 勿扰是否暂停主动漫游。
    pub pause_roam: bool,
    /// 勿扰是否保留待机动画（`01 FR-10-4`：仅保留待机动画）。
    pub keep_idle_anim: bool,
    /// 勿扰是否静音音效（与 S4-M6 门控同口径）。
    pub mute_audio: bool,
}

impl Default for DoNotDisturbCfg {
    fn default() -> Self {
        Self {
            default_on: false,
            pause_bubbles: true,
            pause_roam: true,
            keep_idle_anim: true,
            mute_audio: true,
        }
    }
}

// ---------------------------------------------------------------------------
// character.json —— 命名 / 口头禅 / 台词池 / 渲染（`02 §5.8`）
// ---------------------------------------------------------------------------

/// `character.json` 根：角色命名与口头禅（`02 §5.8`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CharacterConfig {
    /// 配置版本。
    pub version: u32,
    /// 默认角色名（仅允许出现在本配置文件；Rust 侧以 Unicode 码点构造兜底，C2）。
    pub default_name: String,
    /// 口头禅配置（L-03 枚举档位）。
    pub catchphrase: CatchphraseCfg,
    /// 台词池约束（L-02：count 与 keys 数一致）。
    pub line_pools: LinePoolsCfg,
    /// 渲染后端与资源路径。
    pub renderer: RendererCfg,
    /// 换装插槽列表。
    pub slots: Vec<String>,
    /// 表情数量。
    pub expressions: ExpressionsCfg,
    /// 性格描述文案。
    pub personality_text: PersonalityTextCfg,
}

impl Default for CharacterConfig {
    fn default() -> Self {
        Self {
            version: 1,
            // C2：默认名以 Unicode 码点构造（「心月狐」），禁止裸字面量出现在代码中。
            default_name: ['\u{5FC3}', '\u{6708}', '\u{72D0}'].iter().collect(),
            catchphrase: CatchphraseCfg::default(),
            line_pools: LinePoolsCfg::default(),
            renderer: RendererCfg::default(),
            slots: vec![
                "costume".to_string(),
                "hairpin".to_string(),
                "cape".to_string(),
                "collar".to_string(),
                "held".to_string(),
            ],
            expressions: ExpressionsCfg::default(),
            personality_text: PersonalityTextCfg::default(),
        }
    }
}

/// 口头禅配置（L-03：频率为枚举档位，作废 "1:3" 字符串比例）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CatchphraseCfg {
    /// 口头禅 token（以码点构造兜底，C2 同理）。
    pub token: String,
    /// 默认开关。
    pub default_enabled: bool,
    /// 各档位出现频率（句分之 X）。
    pub frequency: CatchphraseFrequencyCfg,
    /// 默认档位。
    pub default_frequency: String,
    /// 同句最小间隔（秒）。
    pub min_interval_sec: u64,
    /// 单句出现上限。
    pub max_per_sentence: u32,
    /// 出现位置偏置。
    pub position_bias: PositionBiasCfg,
    /// 禁用口头禅的台词池。
    pub forbid_pools: Vec<String>,
    /// 必带口头禅的台词池。
    pub require_pools: Vec<String>,
}

impl Default for CatchphraseCfg {
    fn default() -> Self {
        Self {
            token: "\u{5FC3}\u{5FC3}".to_string(),
            default_enabled: true,
            frequency: CatchphraseFrequencyCfg::default(),
            default_frequency: "standard".to_string(),
            min_interval_sec: 25,
            max_per_sentence: 1,
            position_bias: PositionBiasCfg::default(),
            forbid_pools: ["runaway", "angry", "remind", "system", "cool"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            require_pools: ["greet", "coax", "apologize", "begFood", "begBath", "depart", "return"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

/// 口头禅频率枚举档位（L-03：off / low=5 / standard=3 / high=2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CatchphraseFrequencyCfg {
    /// 关闭档（0）。
    pub off: u32,
    /// 低频档。
    pub low: u32,
    /// 标准档。
    pub standard: u32,
    /// 高频档。
    pub high: u32,
}

impl Default for CatchphraseFrequencyCfg {
    fn default() -> Self {
        Self { off: 0, low: 5, standard: 3, high: 2 }
    }
}

/// 口头禅出现位置偏置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PositionBiasCfg {
    /// 句首权重。
    pub head: f32,
    /// 句尾权重。
    pub tail: f32,
    /// 句中权重。
    pub middle: f32,
}

impl Default for PositionBiasCfg {
    fn default() -> Self {
        Self { head: 0.7, tail: 0.3, middle: 0.0 }
    }
}

/// 台词池约束（L-02：count=13 且与 keys 数一致；每池 ≥6 条）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LinePoolsCfg {
    /// 台词池数量。
    pub count: u32,
    /// 每池最少条数。
    pub min_per_pool: u32,
    /// 每池最少含口头禅条数。
    pub min_with_catchphrase: u32,
    /// 每池最少无口头禅变体条数。
    pub min_plain_variant: u32,
    /// 台词池键列表（13 个，含 idle）。
    pub keys: Vec<String>,
}

impl Default for LinePoolsCfg {
    fn default() -> Self {
        Self {
            count: 13,
            min_per_pool: 6,
            min_with_catchphrase: 2,
            min_plain_variant: 2,
            keys: [
                "idle",
                "happy",
                "curious",
                "bored",
                "aggrieved",
                "sulking",
                "angry",
                "sleepy",
                "excited",
                "runaway",
                "begFood",
                "begBath",
                "outing",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        }
    }
}

/// 渲染后端与资源路径（`02 §5.8`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RendererCfg {
    /// 首选渲染路径。
    pub prefer: String,
    /// 渲染回退链。
    pub fallback_chain: Vec<String>,
    /// 骨骼引擎名。
    pub engine: String,
    /// 骨骼引擎版本。
    pub spine_version: String,
    /// 骨骼数据路径。
    pub skeleton_path: String,
    /// 骨骼图集路径。
    pub atlas_path: String,
    /// 图集最大边长（像素）。
    pub atlas_max_px: u32,
    /// 图集内存预算（MB）。
    pub atlas_mem_budget_mb: u32,
    /// 帧回退图集目录。
    pub frame_fallback_path: String,
    /// 资源探测超时（毫秒）。
    pub probe_timeout_ms: u64,
}

impl Default for RendererCfg {
    fn default() -> Self {
        Self {
            prefer: "skeleton".to_string(),
            fallback_chain: ["skeleton", "frame"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            engine: "spine".to_string(),
            spine_version: "4.2".to_string(),
            skeleton_path: "assets/sprites/xinyuehu/default/xinyuehu.json".to_string(),
            atlas_path: "assets/sprites/xinyuehu/default/xinyuehu.atlas".to_string(),
            atlas_max_px: 2048,
            atlas_mem_budget_mb: 16,
            frame_fallback_path: "assets/sprites/xinyuehu/default/atlas".to_string(),
            probe_timeout_ms: 3000,
        }
    }
}

/// 表情数量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExpressionsCfg {
    /// 表情差分数量。
    pub count: u32,
}

impl Default for ExpressionsCfg {
    fn default() -> Self {
        Self { count: 20 }
    }
}

/// 性格描述文案（`02 §5.8` personalityText）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PersonalityTextCfg {
    /// 黏人描述。
    pub clingy: Vec<String>,
    /// 好奇心描述。
    pub curiosity: Vec<String>,
    /// 脾气描述。
    pub temper: Vec<String>,
    /// 胆量描述。
    pub courage: Vec<String>,
    /// 勤快描述。
    pub diligence: Vec<String>,
}

impl Default for PersonalityTextCfg {
    fn default() -> Self {
        let list = |items: &[&str]| -> Vec<String> {
            items.iter().map(|s| (*s).to_string()).collect()
        };
        Self {
            clingy: list(&["有点黏人", "很黏人", "独立"]),
            curiosity: list(&["好奇心强", "爱探索"]),
            temper: list(&["脾气不小", "温和"]),
            courage: list(&["胆子大", "有点胆小"]),
            diligence: list(&["勤快", "爱偷懒"]),
        }
    }
}

// ---------------------------------------------------------------------------
// needs.json —— 六维 + 分档 + 洗澡 + 耦合（`02 §5.12` / §5.10）
// ---------------------------------------------------------------------------

/// `needs.json` 根：生存需求六维、分档、事件扣减、洗澡与耦合矩阵。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NeedsConfig {
    /// 配置版本。
    pub version: u32,
    /// 六维速率定义（satiety / cleanliness）。
    pub dimensions: NeedsDimensionsCfg,
    /// 分档表现。
    pub bands: NeedsBandsCfg,
    /// 一次性事件扣减（`02 §5.9` / §5.12）。
    pub event_deltas: std::collections::BTreeMap<String, NeedEventDeltaCfg>,
    /// 洗澡流程配置。
    pub bath: BathCfg,
    /// 耦合矩阵（`02 §5.10`，含 C-01~C-16 与构建期环检查）。
    pub coupling: CouplingCfg,
}

impl Default for NeedsConfig {
    fn default() -> Self {
        let mut event_deltas = std::collections::BTreeMap::new();
        event_deltas.insert("roll".to_string(), NeedEventDeltaCfg { cleanliness: -8.0 });
        event_deltas.insert("thrownLand".to_string(), NeedEventDeltaCfg { cleanliness: -5.0 });
        event_deltas.insert("eat".to_string(), NeedEventDeltaCfg { cleanliness: -3.0 });
        event_deltas.insert("study".to_string(), NeedEventDeltaCfg { cleanliness: -2.0 });
        event_deltas.insert("trip".to_string(), NeedEventDeltaCfg { cleanliness: -15.0 });
        Self {
            version: 1,
            dimensions: NeedsDimensionsCfg::default(),
            bands: NeedsBandsCfg::default(),
            event_deltas,
            bath: BathCfg::default(),
            coupling: CouplingCfg::default(),
        }
    }
}

/// 六维速率定义（`02 §5.12`；`decayPerMin` 对齐 RV-01：satiety −0.05 / cleanliness −0.08）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NeedsDimensionsCfg {
    /// 饱食度维度。
    pub satiety: NeedDimensionCfg,
    /// 清洁度维度。
    pub cleanliness: NeedDimensionCfg,
}

impl Default for NeedsDimensionsCfg {
    fn default() -> Self {
        Self {
            satiety: NeedDimensionCfg {
                min: 0.0,
                max: 100.0,
                default: 70.0,
                decay_per_min: -0.05,
                meal_multiplier: 2.5,
                activity_multiplier: 2.0,
                sleep_multiplier: 0.4,
            },
            cleanliness: NeedDimensionCfg {
                min: 0.0,
                max: 100.0,
                default: 85.0,
                decay_per_min: -0.08,
                meal_multiplier: 1.0,
                activity_multiplier: 2.0,
                sleep_multiplier: 1.0,
            },
        }
    }
}

/// 单维度速率定义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NeedDimensionCfg {
    /// 下限。
    pub min: f32,
    /// 上限。
    pub max: f32,
    /// 默认值。
    pub default: f32,
    /// 每分钟自然变化速率（负 = 衰减）。
    pub decay_per_min: f32,
    /// 用餐窗口倍率。
    pub meal_multiplier: f32,
    /// 活动进行倍率。
    pub activity_multiplier: f32,
    /// 睡眠倍率。
    pub sleep_multiplier: f32,
}

impl Default for NeedDimensionCfg {
    // 内置默认取 satiety 口径（`02 §5.12`）；cleanliness 由 NeedsDimensionsCfg
    // 的 Default 显式构造。
    fn default() -> Self {
        Self {
            min: 0.0,
            max: 100.0,
            default: 70.0,
            decay_per_min: -0.05,
            meal_multiplier: 2.5,
            activity_multiplier: 2.0,
            sleep_multiplier: 0.4,
        }
    }
}

/// 分档表现集合。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NeedsBandsCfg {
    /// 饱食度分档。
    pub satiety: Vec<NeedBandCfg>,
    /// 清洁度分档。
    pub cleanliness: Vec<NeedBandCfg>,
}

impl Default for NeedsBandsCfg {
    fn default() -> Self {
        let satiety = vec![
            NeedBandCfg { id: "full".into(), min: 70.0, ..Default::default() },
            NeedBandCfg { id: "normal".into(), min: 40.0, ..Default::default() },
            NeedBandCfg {
                id: "peckish".into(),
                min: 20.0,
                action: Some("ACT-N-01".into()),
                mood_decay_mul: Some(1.2),
                interval_sec: Some([180, 480]),
                ..Default::default()
            },
            NeedBandCfg {
                id: "hungry".into(),
                min: 5.0,
                action: Some("ACT-N-01".into()),
                mood_decay_mul: Some(1.8),
                job_reward_mul: Some(0.7),
                speed_mul: Some(0.85),
                ..Default::default()
            },
            NeedBandCfg {
                id: "starving".into(),
                min: 0.0,
                action: Some("ACT-N-01".into()),
                mood_decay_mul: Some(2.5),
                deny: ["work", "study", "travel"].iter().map(|s| (*s).to_string()).collect(),
                emotion: Some("Aggrieved".into()),
                ..Default::default()
            },
        ];
        let cleanliness = vec![
            NeedBandCfg { id: "fresh".into(), min: 70.0, ..Default::default() },
            NeedBandCfg { id: "normal".into(), min: 50.0, ..Default::default() },
            NeedBandCfg {
                id: "stained".into(),
                min: 30.0,
                action: Some("ACT-N-04".into()),
                patches: Some([1, 2]),
                ..Default::default()
            },
            NeedBandCfg {
                id: "dirty".into(),
                min: 15.0,
                action: Some("ACT-N-05".into()),
                mood_decay_mul: Some(1.3),
                patches: Some([2, 4]),
                interval_sec: Some([120, 300]),
                ..Default::default()
            },
            NeedBandCfg {
                id: "filthy".into(),
                min: 0.0,
                action: Some("ACT-N-06".into()),
                stroke_gain_mul: Some(0.5),
                affinity_gain_mul: Some(0.5),
                job_reward_mul: Some(0.6),
                deny: ["study"].iter().map(|s| (*s).to_string()).collect(),
                patches: Some([4, 6]),
                effect: Some("flies".into()),
                ..Default::default()
            },
        ];
        Self { satiety, cleanliness }
    }
}

/// 单条分档（`02 §5.12` bands）。`None` 数值字段语义 = 不修正（等价乘子 1.0）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NeedBandCfg {
    /// 档位 ID。
    pub id: String,
    /// 进入该档的最小值。
    pub min: f32,
    /// 触发动作 ID（无 = 不触发）。
    pub action: Option<String>,
    /// 心情衰减乘子。
    pub mood_decay_mul: Option<f32>,
    /// 触发间隔区间（秒）。
    pub interval_sec: Option<[u64; 2]>,
    /// 打工收益乘子。
    pub job_reward_mul: Option<f32>,
    /// 移动速度乘子。
    pub speed_mul: Option<f32>,
    /// 拒绝派遣列表。
    pub deny: Vec<String>,
    /// 强制情绪态。
    pub emotion: Option<String>,
    /// 污渍贴片数量区间。
    pub patches: Option<[u32; 2]>,
    /// 抚摸增益乘子。
    pub stroke_gain_mul: Option<f32>,
    /// 亲密度增益乘子。
    pub affinity_gain_mul: Option<f32>,
    /// 附加视觉特效。
    pub effect: Option<String>,
}

/// 单条事件扣减（`02 §5.12` eventDeltas，当前仅 cleanliness）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NeedEventDeltaCfg {
    /// 清洁度变化量。
    pub cleanliness: f32,
}

impl Default for NeedEventDeltaCfg {
    fn default() -> Self {
        Self { cleanliness: 0.0 }
    }
}

/// 洗澡流程配置（`02 §5.12` bath）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BathCfg {
    /// 洗澡时长区间（毫秒，8~12s）。
    pub duration_ms: [u64; 2],
    /// 洗澡入场动作。
    pub action_id: String,
    /// 洗澡结束动作。
    pub end_action_id: String,
    /// 结束时清洁度恢复到值。
    pub cleanliness: f32,
    /// 结束心情变化。
    pub mood: f32,
    /// 结束精力变化。
    pub energy: f32,
    /// 免费冷却（分钟）。
    pub free_cd_min: u32,
    /// 洗澡演出优先级。
    pub priority: u32,
}

impl Default for BathCfg {
    fn default() -> Self {
        Self {
            duration_ms: [8000, 12000],
            action_id: "ACT-N-07".to_string(),
            end_action_id: "ACT-N-08".to_string(),
            cleanliness: 100.0,
            mood: 6.0,
            energy: -3.0,
            free_cd_min: 30,
            priority: 8,
        }
    }
}

/// 耦合矩阵（`02 §5.10`：combine + smoothSec + C-01~C-16 规则）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CouplingCfg {
    /// 耦合配置版本。
    pub version: u32,
    /// 各输出量的合并策略（max / mul）。
    pub combine: CombineCfg,
    /// moodDecayMul 移动平均平滑窗口（秒，`02 §5.10`：5s）。
    pub smooth_sec: u64,
    /// 耦合规则列表。
    pub rules: Vec<CouplingRuleCfg>,
}

impl Default for CouplingCfg {
    fn default() -> Self {
        Self {
            version: 1,
            combine: CombineCfg::default(),
            smooth_sec: 5,
            rules: default_coupling_rules(),
        }
    }
}

/// 各输出量合并策略（`02 §5.10`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CombineCfg {
    /// 心情衰减合并策略。
    pub mood_decay: String,
    /// 精力恢复合并策略。
    pub energy_recover: String,
    /// 抚摸增益合并策略。
    pub stroke_gain: String,
    /// 亲密度增益合并策略。
    pub affinity_gain: String,
    /// 打工收益合并策略。
    pub job_reward: String,
    /// 学习效率合并策略。
    pub study_eff: String,
    /// 速度合并策略。
    pub speed: String,
}

impl Default for CombineCfg {
    fn default() -> Self {
        let mul = |v: &str| v.to_string();
        Self {
            mood_decay: "max".to_string(),
            energy_recover: mul("mul"),
            stroke_gain: mul("mul"),
            affinity_gain: mul("mul"),
            job_reward: mul("mul"),
            study_eff: mul("mul"),
            speed: mul("mul"),
        }
    }
}

/// 规则取值：数值乘子或派遣类别字符串（untagged 双型）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum RuleValue {
    /// 数值（乘子）。
    Number(f32),
    /// 文本（派遣类别，如 "work,study,travel"）。
    Text(String),
}

/// 单条耦合规则（`02 §5.10`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CouplingRuleCfg {
    /// 规则 ID（C-xx）。
    pub id: String,
    /// 条件表达式（如 `satiety<20`，基于 tick 起始快照）。
    pub when: String,
    /// 目标输出量（moodDecay / energyRecover / ... / dispatch）。
    pub target: String,
    /// 运算（mul / deny 等）。
    pub op: String,
    /// 取值。
    pub value: RuleValue,
    /// 拒绝原因文案（deny 时）。
    pub reason: Option<String>,
}

impl Default for CouplingRuleCfg {
    fn default() -> Self {
        Self {
            id: String::new(),
            when: String::new(),
            target: String::new(),
            op: String::new(),
            value: RuleValue::Number(0.0),
            reason: None,
        }
    }
}

/// C-01~C-16 全量默认规则（`02 §5.10` 逐条对齐）。
fn default_coupling_rules() -> Vec<CouplingRuleCfg> {
    let num = |v: f32| RuleValue::Number(v);
    let text = |v: &str| RuleValue::Text(v.to_string());
    let rule = |id: &str, when: &str, target: &str, op: &str, value: RuleValue| CouplingRuleCfg {
        id: id.to_string(),
        when: when.to_string(),
        target: target.to_string(),
        op: op.to_string(),
        value,
        reason: None,
    };
    vec![
        rule("C-01", "satiety<20", "moodDecay", "mul", num(1.8)),
        rule("C-02", "satiety<=0", "moodDecay", "mul", num(2.5)),
        rule("C-03", "satiety<40", "energyRecover", "mul", num(0.5)),
        // C-04：饿但能打工（解除负循环）
        rule("C-04", "satiety<20", "jobReward", "mul", num(0.7)),
        CouplingRuleCfg {
            id: "C-05".to_string(),
            when: "satiety<5".to_string(),
            target: "dispatch".to_string(),
            op: "deny".to_string(),
            value: text("work"),
            reason: Some("\u{5FC3}\u{5FC3}没力气了".to_string()),
        },
        rule("C-06", "cleanliness<30", "moodDecay", "mul", num(1.3)),
        rule("C-07", "cleanliness<15", "strokeGain", "mul", num(0.5)),
        rule("C-08", "cleanliness<15", "affinityGain", "mul", num(0.5)),
        rule("C-09", "cleanliness<15", "jobReward", "mul", num(0.6)),
        CouplingRuleCfg {
            id: "C-10".to_string(),
            when: "cleanliness<15".to_string(),
            target: "dispatch".to_string(),
            op: "deny".to_string(),
            value: text("study"),
            reason: Some("洗干净再去上课".to_string()),
        },
        CouplingRuleCfg {
            id: "C-11".to_string(),
            when: "energy<20".to_string(),
            target: "dispatch".to_string(),
            op: "deny".to_string(),
            value: text("work,study,travel"),
            reason: Some("\u{5FC3}\u{5FC3}太累了".to_string()),
        },
        rule("C-12", "energy<20", "speed", "mul", num(0.8)),
        rule("C-13", "mood<30", "studyEff", "mul", num(0.6)),
        rule("C-14", "mood<30", "jobReward", "mul", num(0.8)),
        rule("C-15", "mood>80", "jobReward", "mul", num(1.15)),
        rule("C-16", "mood>80", "travelGain", "mul", num(1.2)),
    ]
}

// ---------------------------------------------------------------------------
// animation.json —— 缓动 / 物理 / 微动 / 呼吸 / 眨眼（`02 §5.22` / §5.20）
// ---------------------------------------------------------------------------

/// `animation.json` 根：缓动、物理、微动、呼吸、眨眼、视线、挤压、粒子、性能降级。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AnimationConfig {
    /// 配置版本。
    pub version: u32,
    /// 缓动曲线（位移/表情/入场/默认）。
    pub easing: EasingCfg,
    /// 交叉淡入时长范围（毫秒，`01 FR-3`：150~250ms）。
    pub fade_ms: FadeMsCfg,
    /// 物理次级动画（`02 §5.20`；本阶段 parts 空数组）。
    pub physics: PhysicsCfg,
    /// 微动层。
    pub micro: MicroCfg,
    /// 呼吸。
    pub breath: BreathCfg,
    /// 眨眼。
    pub blink: BlinkCfg,
    /// 视线。
    pub gaze: GazeCfg,
    /// 挤压拉伸。
    pub squash: SquashCfg,
    /// 粒子。
    pub particles: ParticlesCfg,
    /// 性能降级策略（**S6-M1**；K-8 / R12，见 [`DegradeCfg`]）。
    pub degrade: DegradeCfg,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            version: 1,
            easing: EasingCfg::default(),
            fade_ms: FadeMsCfg::default(),
            physics: PhysicsCfg::default(),
            micro: MicroCfg::default(),
            breath: BreathCfg::default(),
            blink: BlinkCfg::default(),
            gaze: GazeCfg::default(),
            squash: SquashCfg::default(),
            particles: ParticlesCfg::default(),
            degrade: DegradeCfg::default(),
        }
    }
}

/// 性能降级策略（**S6-M1**，`02 §5 K-8` / §10.2 R12 / §5 K-4；`animation.json` `degrade` 段）。
///
/// 数值口径（与 K-8 / R12 逐项对应）：
///   - `memory.warnMb` **200**：>200MB 卸载换装插槽纹理与粒子图集、`physics.level=primaryOnly`；
///   - `memory.hardMb` **225**：>225MB 切 FrameRenderer + 图集 LRU 压到 `lruMb`（32）；
///   - `memory.recoverMb` **170**：低于该值回退正常档（滞回防抖）；
///   - `cpu.highPercent` / `holdTicks`：CPU 高负载（tick = 5s 采样窗）——**本期仅度量**，
///     降帧触发由 K-4 场景表（全屏/电池/隐身）承担，CPU 不作为独立降帧源（`03 S6-M1` 卡片）；
///   - `fps.fullscreen` **4**（K-4 省电档）、`fps.battery` **15**（K-8 电池放电上限）、
///     `fps.hidden` **4**（隐身 / 非前台降帧）。
///
/// `slotUnload` / `physicsPrimaryOnly` 为动作开关（默认开）；执行层归 S9/S7。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DegradeCfg {
    /// 内存分级阈值（K-8 / R12）。
    pub memory: DegradeMemoryCfg,
    /// CPU 高负载度量阈值（仅度量，见模块注释）。
    pub cpu: DegradeCpuCfg,
    /// 降帧档位（fps，须为 K-4 登记档位）。
    pub fps: DegradeFpsCfg,
    /// >200MB 时卸载换装插槽纹理与粒子图集（执行归 S9）。
    pub slot_unload: bool,
    /// >200MB 时 `physics.level=primaryOnly`（执行归 S7/S9）。
    pub physics_primary_only: bool,
}

impl Default for DegradeCfg {
    fn default() -> Self {
        Self {
            memory: DegradeMemoryCfg::default(),
            cpu: DegradeCpuCfg::default(),
            fps: DegradeFpsCfg::default(),
            slot_unload: true,
            physics_primary_only: true,
        }
    }
}

/// 内存分级（K-8 / R12：告警 200、硬阈值 225、恢复 170、LRU 32MB）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DegradeMemoryCfg {
    /// 告警阈值（MB）：> 200 卸插槽纹理 + 物理主档。
    pub warn_mb: u32,
    /// 硬阈值（MB）：> 225 切 FrameRenderer + LRU 压到 `lru_mb`。
    pub hard_mb: u32,
    /// 恢复阈值（MB）：< 170 回退正常档。
    pub recover_mb: u32,
    /// 硬阈值档图集 LRU 上限（MB）。
    pub lru_mb: u32,
}

impl Default for DegradeMemoryCfg {
    fn default() -> Self {
        Self { warn_mb: 200, hard_mb: 225, recover_mb: 170, lru_mb: 32 }
    }
}

/// CPU 度量阈值（tick = 5s 采样窗；**本期仅度量**，`pet://perf.cpu` + 巡检断言）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DegradeCpuCfg {
    /// 高负载判定线（%）：AC-01 动画 ≤8% 之上留余量，作日志/巡检参考。
    pub high_percent: u32,
    /// 高负载保持 tick 数（预留；本期不触发降帧）。
    pub hold_ticks: u32,
    /// 恢复判定线（%）。
    pub recover_percent: u32,
    /// 恢复保持 tick 数（预留）。
    pub recover_ticks: u32,
}

impl Default for DegradeCpuCfg {
    fn default() -> Self {
        Self { high_percent: 60, hold_ticks: 2, recover_percent: 40, recover_ticks: 4 }
    }
}

/// 降帧档位（fps；须为 [`crate::anim::FpsTier`] 登记档位：4 / 15）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DegradeFpsCfg {
    /// 前台全屏 → 省电档 4fps（K-4）。
    pub fullscreen: u32,
    /// 电池放电 → fps 上限 15（K-8）。
    pub battery: u32,
    /// 隐身 / 非前台 → 省电档 4fps。
    pub hidden: u32,
    /// 高 CPU 负载 → 15fps（预留；本期仅度量，见 `DegradeCpuCfg`）。
    pub high_load: u32,
}

impl Default for DegradeFpsCfg {
    fn default() -> Self {
        Self { fullscreen: 4, battery: 15, hidden: 4, high_load: 15 }
    }
}

/// 缓动曲线名（`02 §5.22`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EasingCfg {
    /// 位移缓动。
    pub move_easing: String,
    /// 表情缓动。
    pub emotion: String,
    /// 演出入场缓动。
    pub entrance: String,
    /// 默认缓动。
    pub default_easing: String,
}

impl Default for EasingCfg {
    fn default() -> Self {
        Self {
            move_easing: "easeOutBack1.2".to_string(),
            emotion: "easeInOutCubic".to_string(),
            entrance: "easeOutBack1.7".to_string(),
            default_easing: "easeInOutCubic".to_string(),
        }
    }
}

/// 淡入时长范围（毫秒）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FadeMsCfg {
    /// 最短淡入（毫秒）。
    pub min: u64,
    /// 最长淡入（毫秒）。
    pub max: u64,
    /// 默认淡入（毫秒）。
    pub default: u64,
}

impl Default for FadeMsCfg {
    fn default() -> Self {
        Self { min: 150, max: 250, default: 200 }
    }
}

/// 物理配置（`02 §5.20`：8 部件半隐式欧拉；本阶段 parts 为空数组，enabled 保持 true）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PhysicsCfg {
    /// 物理总开关。
    pub enabled: bool,
    /// 固定子步长（毫秒，`02 §5.20`：8ms）。
    pub sub_step_ms: u64,
    /// 物理等级（full / primaryOnly）。
    pub level: String,
    /// 物理部件列表（`02 §5.20` 表；本阶段空数组）。
    pub parts: Vec<PhysicsPartCfg>,
}

impl Default for PhysicsCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            sub_step_ms: 8,
            level: "full".to_string(),
            parts: Vec::new(),
        }
    }
}

/// 单个物理部件（`02 §5.20` 表：bone / type / stiffness/damping/mass）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PhysicsPartCfg {
    /// 骨骼名。
    pub bone: String,
    /// 部件类型（rotate / chain / pendulum / cloth）。
    pub kind: String,
    /// 刚度。
    pub stiffness: f32,
    /// 阻尼。
    pub damping: f32,
    /// 质量。
    pub mass: f32,
    /// 继承来源部件。
    pub inherit_from: Option<String>,
    /// 心情摆动倍率（尾巴）。
    pub mood_swing_mul: Option<f32>,
    /// 生气变硬（尾巴）。
    pub angry_stiff: Option<bool>,
}

/// 微动层（`02 §5.22`：8 种微动 + 权重表 + 情绪低落替换）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MicroCfg {
    /// 抽取间隔区间（秒，5~12s）。
    pub interval_sec: [u64; 2],
    /// 60s 内不重复。
    pub no_repeat_sec: u64,
    /// 微动条目（8 项 + 权重表 [25,20,15,15,10,8,5,2]）。
    pub items: Vec<MicroItemCfg>,
    /// 情绪低落时替换项。
    pub low_mood_replace: Vec<String>,
}

impl Default for MicroCfg {
    fn default() -> Self {
        let item = |id: &str, weight: u32, clip: &str, particle: Option<&str>| MicroItemCfg {
            id: id.to_string(),
            weight,
            clip: clip.to_string(),
            particle: particle.map(|p| p.to_string()),
        };
        Self {
            interval_sec: [5, 12],
            no_repeat_sec: 60,
            items: vec![
                item("ear_twitch", 25, "anim_micro_ear", None),
                item("tail_swing", 20, "anim_micro_tail", None),
                item("weight_shift", 15, "anim_micro_shift", None),
                item("blink_smile", 15, "anim_micro_blink", None),
                item("hum", 10, "anim_micro_hum", Some("note")),
                item("look_around", 8, "anim_micro_look", None),
                item("small_jump", 5, "anim_micro_jump", None),
                item("tidy_hair", 2, "anim_micro_hair", None),
            ],
            low_mood_replace: ["sigh", "hug_knees"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

/// 单条微动。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MicroItemCfg {
    /// 微动 ID。
    pub id: String,
    /// 抽取权重。
    pub weight: u32,
    /// 骨骼剪辑名。
    pub clip: String,
    /// 附带粒子（可选）。
    pub particle: Option<String>,
}

/// 呼吸（`02 §5.22`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BreathCfg {
    /// 呼吸周期区间（秒）。
    pub period_sec: [f32; 2],
    /// 胸部缩放幅度。
    pub chest_scale: f32,
    /// 头部纵向偏移（物理像素）。
    pub head_y_px: u32,
    /// 睡眠呼吸周期（秒）。
    pub sleep_period_sec: f32,
    /// 睡眠呼吸倍率。
    pub sleep_mul: f32,
}

impl Default for BreathCfg {
    fn default() -> Self {
        Self {
            period_sec: [3.2, 4.0],
            chest_scale: 0.015,
            head_y_px: 1,
            sleep_period_sec: 5.0,
            sleep_mul: 1.6,
        }
    }
}

/// 眨眼（`02 §5.22`：禁止等间隔，randWeighted(2000..6000, peak 3500)）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BlinkCfg {
    /// 眨眼间隔区间（毫秒）。
    pub interval_ms: [u64; 2],
    /// 加权峰值间隔（毫秒）。
    pub peak_ms: u64,
    /// 单次时长区间（毫秒）。
    pub duration_ms: [u64; 2],
    /// 说话时倍率。
    pub speaking_factor: f32,
    /// 害羞时倍率。
    pub shy_factor: f32,
}

impl Default for BlinkCfg {
    fn default() -> Self {
        Self {
            interval_ms: [2000, 6000],
            peak_ms: 3500,
            duration_ms: [120, 180],
            speaking_factor: 0.3,
            shy_factor: 1.5,
        }
    }
}

/// 视线（`02 §5.22`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GazeCfg {
    /// 视线切换间隔区间（秒）。
    pub interval_sec: [u64; 2],
    /// 视线缓动时长（毫秒）。
    pub ease_ms: u64,
}

impl Default for GazeCfg {
    fn default() -> Self {
        Self { interval_sec: [3, 8], ease_ms: 200 }
    }
}

/// 挤压拉伸（`02 §5.22`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SquashCfg {
    /// 蓄力缩放区间。
    pub anticipation: [f32; 2],
    /// 拉伸缩放区间。
    pub stretch: [f32; 2],
    /// 落地缩放区间。
    pub landing: [f32; 2],
    /// 恢复时长（毫秒）。
    pub recover_ms: u64,
    /// 过冲幅度区间。
    pub overshoot: [f32; 2],
}

impl Default for SquashCfg {
    fn default() -> Self {
        Self {
            anticipation: [0.85, 1.15],
            stretch: [1.15, 0.9],
            landing: [0.8, 1.2],
            recover_ms: 200,
            overshoot: [0.08, 0.15],
        }
    }
}

/// 粒子（`02 §5.22`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ParticlesCfg {
    /// 单次爆发上限。
    pub max_per_burst: u32,
    /// 粒子寿命（秒）。
    pub life_sec: f32,
}

impl Default for ParticlesCfg {
    fn default() -> Self {
        Self { max_per_burst: 60, life_sec: 1.2 }
    }
}

// ---------------------------------------------------------------------------
// emotion.json —— P 模型 v2（`02 §5.7`）
// ---------------------------------------------------------------------------

/// `emotion.json` 根：P 模型 v2 全结构（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EmotionConfig {
    /// 配置版本（v2）。
    pub version: u32,
    /// 四维定义（mood / energy / affinity / boredom）。
    pub dimensions: EmotionDimensionsCfg,
    /// 敏感度（FR-11-11）。
    pub sensitivity: SensitivityCfg,
    /// 在场判定。
    pub presence: PresenceCfg,
    /// 忙碌判定。
    pub busyness: BusynessCfg,
    /// 作息节律。
    pub rhythm: RhythmCfg,
    /// 需求对 P 的贡献。
    pub needs: EmotionNeedsCfg,
    /// 粗暴对待。
    pub rough: RoughCfg,
    /// 性格五维。
    pub personality: PersonalityCfg,
    /// 自适应基线。
    pub adapt: AdaptCfg,
    /// P 阶段阈值（5/15/30/60/120）。
    pub thresholds: ThresholdsCfg,
    /// 阶段确认参数。
    pub confirm: ConfirmCfg,
    /// 缓解值。
    pub relief: ReliefCfg,
    /// P 抽血 drain。
    pub mood: MoodDrainCfg,
    /// 惯性时间常数。
    pub inertia: InertiaCfg,
    /// 六档阶段定义。
    pub levels: Vec<EmotionLevelCfg>,
    /// 离线补偿。
    pub offline: OfflineCfg,
    /// 外出期间情绪口径。
    pub activity: EmotionActivityCfg,
    /// 哄好流程。
    pub coax: CoaxCfg,
}

impl Default for EmotionConfig {
    fn default() -> Self {
        Self {
            version: 2,
            dimensions: EmotionDimensionsCfg::default(),
            sensitivity: SensitivityCfg::default(),
            presence: PresenceCfg::default(),
            busyness: BusynessCfg::default(),
            rhythm: RhythmCfg::default(),
            needs: EmotionNeedsCfg::default(),
            rough: RoughCfg::default(),
            personality: PersonalityCfg::default(),
            adapt: AdaptCfg::default(),
            thresholds: ThresholdsCfg::default(),
            confirm: ConfirmCfg::default(),
            relief: ReliefCfg::default(),
            mood: MoodDrainCfg::default(),
            inertia: InertiaCfg::default(),
            levels: default_emotion_levels(),
            offline: OfflineCfg::default(),
            activity: EmotionActivityCfg::default(),
            coax: CoaxCfg::default(),
        }
    }
}

/// 四维定义（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EmotionDimensionsCfg {
    /// 心情维度。
    pub mood: MoodDimCfg,
    /// 精力维度。
    pub energy: EnergyDimCfg,
    /// 亲密度维度。
    pub affinity: AffinityDimCfg,
    /// 无聊派生展示维度（RV-02：单一真源 = P）。
    pub boredom: BoredomDimCfg,
}

/// 心情维度（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MoodDimCfg {
    /// 下限。
    pub min: f32,
    /// 上限。
    pub max: f32,
    /// 默认值。
    pub default: f32,
    /// 每分钟自然衰减。
    pub decay_per_min: f32,
    /// 活跃期每分钟衰减。
    pub decay_per_min_active: f32,
}

impl Default for MoodDimCfg {
    fn default() -> Self {
        Self {
            min: 0.0,
            max: 100.0,
            default: 60.0,
            decay_per_min: -1.0,
            decay_per_min_active: -0.5,
        }
    }
}

/// 精力维度（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EnergyDimCfg {
    /// 下限。
    pub min: f32,
    /// 上限。
    pub max: f32,
    /// 默认值。
    pub default: f32,
    /// 每分钟自然衰减。
    pub decay_per_min: f32,
    /// 睡眠每分钟恢复。
    pub sleep_recover_per_min: f32,
    /// 困倦阈值。
    pub sleepy_threshold: f32,
}

impl Default for EnergyDimCfg {
    fn default() -> Self {
        Self {
            min: 0.0,
            max: 100.0,
            default: 100.0,
            decay_per_min: -0.5,
            sleep_recover_per_min: 2.0,
            sleepy_threshold: 20.0,
        }
    }
}

/// 亲密度维度（`02 §5.7`：Lv1~10，经验公式 100*level）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AffinityDimCfg {
    /// 起始等级。
    pub min_level: u32,
    /// 最高等级。
    pub max_level: u32,
    /// 每级所需经验公式（展示用）。
    pub exp_formula: String,
    /// 初始经验。
    pub default_exp: f32,
}

impl Default for AffinityDimCfg {
    fn default() -> Self {
        Self {
            min_level: 1,
            max_level: 10,
            exp_formula: "100*level".to_string(),
            default_exp: 0.0,
        }
    }
}

/// 无聊维度（RV-02 / B-02 冻结：纯展示量，单一真源 = neglect.p）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BoredomDimCfg {
    /// 仅展示。
    pub display_only: bool,
    /// 单一真源字段。
    pub single_source_of_truth: String,
    /// 展示映射公式。
    pub formula: String,
}

impl Default for BoredomDimCfg {
    fn default() -> Self {
        Self {
            display_only: true,
            single_source_of_truth: "neglect.p".to_string(),
            formula: "min(100, p*100/120)".to_string(),
        }
    }
}

/// 敏感度（FR-11-11：value × rateClamp）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SensitivityCfg {
    /// 当前敏感度值。
    pub value: f32,
    /// P 速率钳制区间。
    pub rate_clamp: RateClampCfg,
}

impl Default for SensitivityCfg {
    fn default() -> Self {
        Self {
            value: 1.0,
            rate_clamp: RateClampCfg { min: 0.5, max: 1.6 },
        }
    }
}

/// P 速率钳制区间（FR-11-11：min 0.5 / max 1.6）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RateClampCfg {
    /// 钳制下限。
    pub min: f32,
    /// 钳制上限。
    pub max: f32,
}

impl Default for RateClampCfg {
    fn default() -> Self {
        Self { min: 0.5, max: 1.6 }
    }
}

/// 在场判定（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PresenceCfg {
    /// 不在场阈值（秒）。
    pub away_threshold_sec: u64,
    /// 不在场因子。
    pub factor_away: f32,
    /// 在场因子。
    pub factor_here: f32,
    /// 迟滞窗口（秒）。
    pub hysteresis_sec: u64,
}

impl Default for PresenceCfg {
    fn default() -> Self {
        Self {
            away_threshold_sec: 180,
            factor_away: 0.05,
            factor_here: 1.0,
            hysteresis_sec: 15,
        }
    }
}

/// 忙碌判定（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BusynessCfg {
    /// 深度忙碌因子。
    pub factor_deep: f32,
    /// 忙碌因子。
    pub factor_busy: f32,
    /// 轻闲因子。
    pub factor_light: f32,
    /// 摸鱼因子。
    pub factor_slack: f32,
    /// 深度忙碌 P 上限。
    pub cap_deep: u32,
    /// 忙碌 P 上限。
    pub cap_busy: u32,
    /// 空闲 P 上限。
    pub cap_free: u32,
    /// 深度忙碌击键强度阈值。
    pub kps_deep_threshold: f32,
    /// 忙碌击键强度阈值。
    pub kps_busy_threshold: f32,
    /// 深度忙碌点击频率阈值。
    pub clicks_per_min_deep: u32,
    /// 深度忙碌移动量阈值。
    pub move_px_per_min_deep: u32,
    /// 深度忙碌应用哈希白名单（`02 §5.7` 为示意占位，默认空集，Q-18：只存哈希）。
    pub deep_apps: Vec<String>,
    /// 摸鱼应用哈希白名单。
    pub slack_apps: Vec<String>,
    /// 强度平滑窗口（秒）。
    pub smoothing_sec: u64,
    /// 摸鱼持续多久后播专属台词（秒；`01 AC-19` 前台视频 40min = 2400）。
    pub slack_linger_sec: u64,
}

impl Default for BusynessCfg {
    fn default() -> Self {
        Self {
            factor_deep: 0.3,
            factor_busy: 0.5,
            factor_light: 1.0,
            factor_slack: 1.2,
            cap_deep: 12,
            cap_busy: 28,
            cap_free: 120,
            kps_deep_threshold: 2.5,
            kps_busy_threshold: 1.0,
            clicks_per_min_deep: 30,
            move_px_per_min_deep: 3000,
            deep_apps: Vec::new(),
            slack_apps: Vec::new(),
            smoothing_sec: 20,
            slack_linger_sec: 2400,
        }
    }
}

/// 单个作息时段（`02 §5.7`：6 段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RhythmSegmentCfg {
    /// 时段 ID。
    pub id: String,
    /// 起始时刻（HH:mm）。
    pub from: String,
    /// 结束时刻（HH:mm）。
    pub to: String,
    /// 节律因子。
    pub factor: f32,
}

impl Default for RhythmSegmentCfg {
    fn default() -> Self {
        Self {
            id: String::new(),
            from: String::new(),
            to: String::new(),
            factor: 1.0,
        }
    }
}

/// 作息节律（`02 §5.7`：6 段 + 用餐窗口 + 预热）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RhythmCfg {
    /// 时段表（6 段）。
    pub segments: Vec<RhythmSegmentCfg>,
    /// 用餐窗口（3 组）。
    pub meal_windows: Vec<[String; 2]>,
    /// 用餐节律因子。
    pub meal_factor: f32,
    /// 深夜重定向起始等级。
    pub night_redirect_from_level: u32,
    /// 深夜重定向动作 ID（`01 B-4`：改播打哈欠催睡；`02 §5.7` 无对应键，S7-M5 登记）。
    pub night_redirect_action_id: String,
    /// 深夜重定向动作优先级。
    pub night_redirect_priority: u32,
    /// 预热时长（分钟）。
    pub warmup_minutes: u32,
    /// 预热因子。
    pub warmup_factor: f32,
}

impl Default for RhythmCfg {
    fn default() -> Self {
        let seg = |id: &str, from: &str, to: &str, factor: f32| RhythmSegmentCfg {
            id: id.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            factor,
        };
        Self {
            segments: vec![
                seg("morning", "05:00", "09:00", 1.0),
                seg("forenoon", "09:00", "11:00", 1.0),
                seg("noon", "11:00", "13:00", 1.15),
                seg("afternoon", "13:00", "17:00", 1.0),
                seg("dusk", "17:00", "23:00", 1.15),
                seg("night", "23:00", "05:00", 0.35),
            ],
            meal_windows: vec![
                ["07:00".to_string(), "09:00".to_string()],
                ["11:00".to_string(), "13:00".to_string()],
                ["17:00".to_string(), "19:00".to_string()],
            ],
            meal_factor: 1.15,
            night_redirect_from_level: 2,
            night_redirect_action_id: "ACT-I-02".to_string(),
            night_redirect_priority: 8,
            warmup_minutes: 10,
            warmup_factor: 0.5,
        }
    }
}

/// 需求对 P 的贡献（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EmotionNeedsCfg {
    /// 需求系数。
    pub coef: f32,
    /// 亏空参考值。
    pub deficit_ref: f32,
    /// 亏空来源维度。
    pub deficit_sources: Vec<String>,
}

impl Default for EmotionNeedsCfg {
    fn default() -> Self {
        Self {
            coef: 0.6,
            deficit_ref: 70.0,
            deficit_sources: ["satiety", "cleanliness"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

/// 粗暴对待（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RoughCfg {
    /// 单次累积步长。
    pub step: f32,
    /// 上限。
    pub max: f32,
    /// 完全衰减时长（分钟）。
    pub decay_min: u64,
    /// 打断演出惩罚。
    pub interrupt_penalty: u32,
    /// 召回惩罚。
    pub recall_penalty: u32,
}

impl Default for RoughCfg {
    fn default() -> Self {
        Self {
            step: 0.15,
            max: 1.6,
            decay_min: 120,
            interrupt_penalty: 10,
            recall_penalty: 6,
        }
    }
}

/// 性格五维（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PersonalityCfg {
    /// 黏人系数 A。
    pub clingy_coef_a: f32,
    /// 黏人系数 B。
    pub clingy_coef_b: f32,
    /// 阈值缩放基数。
    pub threshold_scale_base: f32,
    /// 阈值缩放脾气项。
    pub threshold_scale_temper: f32,
    /// 心情变化基数。
    pub mood_delta_base: f32,
    /// 心情变化脾气项。
    pub mood_delta_temper: f32,
    /// 五维默认值。
    pub defaults: PersonalityDefaultsCfg,
    /// 首次创建时粘人度随机区间下限（`01 §6.11.13`：45）。
    pub clingy_init_min: f32,
    /// 首次创建时粘人度随机区间上限（`01 §6.11.13`：55）。
    pub clingy_init_max: f32,
    /// 每日重掷次数。
    pub reroll_per_day: u32,
    /// 重掷抖动。
    pub reroll_jitter: f32,
}

impl Default for PersonalityCfg {
    fn default() -> Self {
        Self {
            clingy_coef_a: 0.6,
            clingy_coef_b: 0.8,
            threshold_scale_base: 1.2,
            threshold_scale_temper: 0.4,
            mood_delta_base: 0.8,
            mood_delta_temper: 0.4,
            defaults: PersonalityDefaultsCfg::default(),
            clingy_init_min: 45.0,
            clingy_init_max: 55.0,
            reroll_per_day: 3,
            reroll_jitter: 0.15,
        }
    }
}

/// 性格五维默认值（`02 §5.7`：clingy 0.50 / curiosity 0.70 / temper 0.55 / courage 0.50 / diligence 0.60）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PersonalityDefaultsCfg {
    /// 黏人。
    pub clingy: f32,
    /// 好奇心。
    pub curiosity: f32,
    /// 脾气。
    pub temper: f32,
    /// 胆量。
    pub courage: f32,
    /// 勤快。
    pub diligence: f32,
}

impl Default for PersonalityDefaultsCfg {
    fn default() -> Self {
        Self {
            clingy: 0.50,
            curiosity: 0.70,
            temper: 0.55,
            courage: 0.50,
            diligence: 0.60,
        }
    }
}

/// 自适应基线（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AdaptCfg {
    /// 最小交互时长（分钟）。
    pub t_min: u32,
    /// 最大交互时长（分钟）。
    pub t_max: u32,
    /// 基线最短（分钟）。
    pub base_min: u32,
    /// 黏人系数。
    pub clingy_coef: u32,
    /// 平均占比。
    pub avg_ratio: f32,
    /// 每日最大变化。
    pub max_daily_change: f32,
    /// 因子下限。
    pub factor_min: f32,
    /// 因子上限。
    pub factor_max: f32,
    /// 离群判定小时数。
    pub outlier_hours: u32,
    /// 关系降温天数。
    pub cool_days: u32,
    /// 零交互降温天数。
    pub zero_days: u32,
    /// 降温判定最少交互次数。
    pub cool_days_min_interact: u32,
    /// 降温乘子。
    pub cool_multiplier: f32,
}

impl Default for AdaptCfg {
    fn default() -> Self {
        Self {
            t_min: 8,
            t_max: 90,
            base_min: 30,
            clingy_coef: 10,
            avg_ratio: 0.6,
            max_daily_change: 0.1,
            factor_min: 0.7,
            factor_max: 1.3,
            outlier_hours: 8,
            cool_days: 3,
            zero_days: 7,
            cool_days_min_interact: 3,
            cool_multiplier: 1.2,
        }
    }
}

/// P 阶段阈值（`02 §5.7`：5/15/30/60/120）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThresholdsCfg {
    /// L1 阈值（分钟）。
    pub l1: u32,
    /// L2 阈值。
    pub l2: u32,
    /// L3 阈值。
    pub l3: u32,
    /// L4 阈值。
    pub l4: u32,
    /// L5 阈值。
    pub l5: u32,
}

impl Default for ThresholdsCfg {
    fn default() -> Self {
        Self { l1: 5, l2: 15, l3: 30, l4: 60, l5: 120 }
    }
}

/// 阶段确认参数（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfirmCfg {
    /// 升档确认（秒）。
    pub up_sec: u64,
    /// 降档确认（秒）。
    pub down_sec: u64,
    /// 自然回退档位下限。
    pub natural_floor_level: u32,
    /// 自然回退保持（秒）。
    pub natural_hold_sec: u64,
    /// 自然回退最少正向交互。
    pub natural_min_positive: u32,
    /// 自然回退 P 阈值。
    pub natural_p_threshold: u32,
    /// 无负向窗口（秒）。
    pub no_negative_window_sec: u64,
    /// 降温阻断天数。
    pub cool_days_block: u32,
    /// 自然回退心情下限。
    pub natural_mood_floor: u32,
    /// 自然消气动作 ID（`01 §6.11.8.1`：`ACT-T-04`；`02 §5.7` 无对应键，S7-M5 登记）。
    pub natural_cool_action_id: String,
    /// 自然消气动作优先级（`01 §6.11.8.1`：8）。
    pub natural_cool_priority: u32,
}

impl Default for ConfirmCfg {
    fn default() -> Self {
        Self {
            up_sec: 60,
            down_sec: 30,
            natural_floor_level: 3,
            natural_hold_sec: 60,
            natural_min_positive: 3,
            natural_p_threshold: 15,
            no_negative_window_sec: 3600,
            cool_days_block: 3,
            natural_mood_floor: 45,
            natural_cool_action_id: "ACT-T-04".to_string(),
            natural_cool_priority: 8,
        }
    }
}

/// 缓解值（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReliefCfg {
    /// 悬停缓解。
    pub hover: u32,
    /// 单击缓解。
    pub click: u32,
    /// 双击缓解。
    pub double_click: u32,
    /// 抚摸缓解。
    pub stroke: u32,
    /// 喂食缓解。
    pub feed: u32,
    /// 洗澡缓解。
    pub bath: u32,
    /// 玩耍缓解。
    pub play: u32,
    /// 哄好缓解。
    pub coax: u32,
    /// 各交互冷却（秒）。
    pub cooldown_sec: ReliefCooldownCfg,
    /// 抚摸窗口内上限次数。
    pub stroke_max_per_window: u32,
    /// 抚摸窗口时长（秒）。
    pub stroke_window_sec: u64,
}

impl Default for ReliefCfg {
    fn default() -> Self {
        Self {
            hover: 2,
            click: 3,
            double_click: 8,
            stroke: 15,
            feed: 20,
            bath: 25,
            play: 25,
            coax: 60,
            cooldown_sec: ReliefCooldownCfg::default(),
            stroke_max_per_window: 3,
            stroke_window_sec: 600,
        }
    }
}

/// 缓解冷却（`02 §5.7`：hover 60 / click 3 / doubleClick 30 / stroke 0）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReliefCooldownCfg {
    /// 悬停冷却（秒）。
    pub hover: u64,
    /// 单击冷却（秒）。
    pub click: u64,
    /// 双击冷却（秒）。
    pub double_click: u64,
    /// 抚摸冷却（秒）。
    pub stroke: u64,
}

impl Default for ReliefCooldownCfg {
    fn default() -> Self {
        Self { hover: 60, click: 3, double_click: 30, stroke: 0 }
    }
}

/// P 抽血 drain（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MoodDrainCfg {
    /// 抽血系数。
    pub drain_coef: f32,
    /// 抽血指数。
    pub drain_exp: f32,
    /// 抽血参考 P。
    pub drain_ref_p: f32,
}

impl Default for MoodDrainCfg {
    fn default() -> Self {
        Self { drain_coef: 2.0, drain_exp: 1.5, drain_ref_p: 120.0 }
    }
}

/// 惯性时间常数（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InertiaCfg {
    /// 上升时间常数（秒）。
    pub tau_up_sec: u64,
    /// 下降时间常数（秒）。
    pub tau_down_sec: u64,
}

impl Default for InertiaCfg {
    fn default() -> Self {
        Self { tau_up_sec: 90, tau_down_sec: 20 }
    }
}

/// 六档阶段定义（`02 §5.7` levels：平静/无聊/委屈/生闷气/生气/离家出走）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EmotionLevelCfg {
    /// 档位序号。
    pub level: u32,
    /// 档位名。
    pub name: String,
    /// 进档心情变化。
    pub mood_delta: i32,
    /// 情绪态标识。
    pub emotion: String,
    /// 待机动作池。
    pub idle_pool: Vec<String>,
    /// 台词池。
    pub line_pool: String,
    /// 优先级下限。
    pub priority_floor: u32,
    /// 漫游频率乘子（缺省 1.0）。
    pub roam_rate_mul: f32,
    /// 拒绝正向交互。
    pub refuse_positive: bool,
    /// 心情锁定最大值（离家出走档）。
    pub mood_lock_max: u32,
    /// 当日冻结亲密度。
    pub freeze_affinity_today: bool,
}

impl Default for EmotionLevelCfg {
    fn default() -> Self {
        Self {
            level: 0,
            name: String::new(),
            mood_delta: 0,
            emotion: String::new(),
            idle_pool: Vec::new(),
            line_pool: String::new(),
            priority_floor: 0,
            roam_rate_mul: 1.0,
            refuse_positive: false,
            mood_lock_max: 0,
            freeze_affinity_today: false,
        }
    }
}

/// levels 六档默认（`02 §5.7` 逐档对齐）。
fn default_emotion_levels() -> Vec<EmotionLevelCfg> {
    let level = |lv: u32,
                 name: &str,
                 mood_delta: i32,
                 emotion: &str,
                 idle_pool: &[&str],
                 line_pool: &str,
                 priority_floor: u32| EmotionLevelCfg {
        level: lv,
        name: name.to_string(),
        mood_delta,
        emotion: emotion.to_string(),
        idle_pool: idle_pool.iter().map(|s| (*s).to_string()).collect(),
        line_pool: line_pool.to_string(),
        priority_floor,
        roam_rate_mul: 1.0,
        refuse_positive: false,
        mood_lock_max: 0,
        freeze_affinity_today: false,
    };
    let levels = vec![
        level(0, "平静", 0, "Idle", &["ACT-M-01", "ACT-I-01", "ACT-I-04", "ACT-S-01"], "idle", 0),
        EmotionLevelCfg { roam_rate_mul: 0.6, ..level(1, "无聊", -5, "Bored", &["ACT-I-03", "ACT-I-01", "ACT-I-02"], "bored", 3) },
        level(2, "委屈", -10, "Aggrieved", &["ACT-E-02", "ACT-I-03"], "aggrieved", 7),
        EmotionLevelCfg { refuse_positive: true, ..level(3, "生闷气", -15, "Sulking", &["ACT-E-03"], "sulking", 8) },
        EmotionLevelCfg { refuse_positive: true, ..level(4, "生气", -20, "Angry", &["ACT-E-04"], "angry", 9) },
        EmotionLevelCfg {
            mood_lock_max: 10,
            freeze_affinity_today: true,
            ..level(5, "离家出走", -30, "Runaway", &["ACT-E-05"], "runaway", 10)
        },
    ];
    levels
}

/// 离线补偿（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OfflineCfg {
    /// 宽限期（分钟）。
    pub grace_min: u64,
    /// 想念分支阈值（分钟）。
    pub longing_min: u64,
    /// 想念分支亲密度经验倍率。
    pub longing_affinity_exp_multiplier: u32,
    /// 想念分支保持上限（小时）。
    pub longing_hold_hour: u64,
    /// 单次补偿最大模拟步数。
    pub max_sim_steps: u64,
    /// 模拟步长（秒）。
    pub step_sec: u64,
}

impl Default for OfflineCfg {
    fn default() -> Self {
        Self {
            grace_min: 30,
            longing_min: 240,
            longing_affinity_exp_multiplier: 2,
            longing_hold_hour: 24,
            max_sim_steps: 1440,
            step_sec: 60,
        }
    }
}

/// 外出期间情绪口径（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EmotionActivityCfg {
    /// 外出期间冻结 P 累积。
    pub freeze_neglect_while_out: bool,
    /// 回归缓解。
    pub return_relief: u32,
}

impl Default for EmotionActivityCfg {
    fn default() -> Self {
        Self { freeze_neglect_while_out: true, return_relief: 20 }
    }
}

/// 哄好流程（`02 §5.7`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CoaxCfg {
    /// 每步抚摸时长（秒）。
    pub stroke_sec: u64,
    /// 每两秒心情增益。
    pub mood_gain_per_two_sec: f32,
    /// 爱心窗口（秒）。
    pub heart_window_sec: u64,
    /// 光环比保持（秒）。
    pub ring_hold_sec: u64,
    /// 打断回滚比例。
    pub interrupt_rollback_ratio: f32,
    /// 恢复心情下限。
    pub recover_mood_floor: u32,
    /// 简单模式抚摸时长（秒）。
    pub easy_mode_stroke_sec: u64,
}

impl Default for CoaxCfg {
    fn default() -> Self {
        Self {
            stroke_sec: 5,
            mood_gain_per_two_sec: 3.0,
            heart_window_sec: 10,
            ring_hold_sec: 60,
            interrupt_rollback_ratio: 0.5,
            recover_mood_floor: 50,
            easy_mode_stroke_sec: 2,
        }
    }
}

// ---------------------------------------------------------------------------
// actions.json —— 53 动作元数据（`02 §5.11` 样例 + `01 §6.3.2` 总表）
// ---------------------------------------------------------------------------

/// `actions.json` 根：`{"version":1,"actions":[...]}`，53 条全量（批次 A 29 + B 8 + C 16）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ActionsConfig {
    /// 配置版本。
    pub version: u32,
    /// 动作元数据列表（ID 一律 `ACT-` 前缀，RV-12）。
    pub actions: Vec<ActionCfg>,
}

impl Default for ActionsConfig {
    fn default() -> Self {
        Self { version: 1, actions: Vec::new() }
    }
}

/// 动作元数据（字段结构以 `02 §5.11` 样例为准 + performance / disabled 两列）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ActionCfg {
    /// 动作 ID（`ACT-<类>-<两位序号>`，C5 / RV-12）。
    pub id: String,
    /// 动作名（中文展示名，取自 `01 §6.3.2` 总表）。
    pub name: String,
    /// 类别（move / idle / interact / emotion / need / activity / special / perception）。
    pub category: String,
    /// 优先级（1~10，10 最高；`01 §6.3.1`）。
    pub priority: u32,
    /// 是否可打断。
    pub interruptible: bool,
    /// 可打断本动作的最小优先级（0 = 不适用；无条件可打断时 = priority + 1，
    /// 对齐 `02 §5.11` 样例 ACT-N-01 的 5→6）。
    pub min_interrupt_priority: u32,
    /// 是否循环动作。
    pub looping: bool,
    /// 循环帧区间（行主序帧索引，闭区间）。
    pub loop_range: Option<[u32; 2]>,
    /// 帧率。
    pub fps: u32,
    /// 交叉淡入时长（毫秒，默认 200）。
    pub fade_ms: u64,
    /// 触发条件。
    pub trigger: TriggerCfg,
    /// 求助请求参数（仅求助类 need 动作；R-B 退避依据）。
    pub help_request: Option<HelpRequestCfg>,
    /// 压制本求助类动作的情绪优先级下限（0 = 不适用；`01 §6.3.1`：≥7 压制 N-01/05/06）。
    pub suppressed_by_emotion_priority: u32,
    /// 是否提供镜像翻转版本（`01 §6.3.2` 注：全部动作需镜像）。
    pub mirror: bool,
    /// 音效资源 ID（蛇形命名；无音效 = None）。
    pub sound: Option<String>,
    /// 播放期间是否阻塞漫游移动。
    pub block_motion: bool,
    /// 精力消耗。
    pub energy_cost: f32,
    /// 演出类（R-A：不可打断，arbiter 运行期 minInterruptPriority 置 255）。
    pub performance: bool,
    /// 是否禁用（资源批次 B/C 未交付 → true，加载侧跳过）。
    pub disabled: bool,
}

/// 动作触发条件（`02 §5.11` 样例：`{"type":"need","value":"satiety<40"}`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TriggerCfg {
    /// 触发类型（idle / roam / emotion / interaction / need / activity / sense / random / lifecycle）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 条件表达式或标识。
    pub value: String,
}

/// 求助请求参数（`02 §5.11` 样例：baseIntervalSec 180 / backoffMul 2.0 / max 900 / noResponse 60）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HelpRequestCfg {
    /// 基础求助间隔（秒）。
    pub base_interval_sec: u64,
    /// 退避乘子（R-C：60s 无响应 ×2）。
    pub backoff_mul: f32,
    /// 最大求助间隔（秒）。
    pub max_interval_sec: u64,
    /// 视为无响应时长（秒）。
    pub no_response_sec: u64,
    /// 是否带气泡。
    pub with_bubble: bool,
}

// ---------------------------------------------------------------------------
// activities.json —— 外出活动配置（S8-M1，T-21 段 · 1/4；`02 §5.16`）
// ---------------------------------------------------------------------------

/// `activities.json` 根：外出活动全局参数 + 岗位 / 课程 / 旅游目录。
///
/// 出处：`02 §5.16`（关键片段）+ `01 §6.13.3/6.13.4/6.13.5`（岗位 / 课程 / 目的地全表）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ActivitiesConfig {
    /// 配置版本。
    pub version: u32,
    /// 活动全局参数（含三类目录）。
    pub activity: ActivityGlobalCfg,
}

impl Default for ActivitiesConfig {
    fn default() -> Self {
        Self { version: 1, activity: ActivityGlobalCfg::default() }
    }
}

/// 活动全局参数（`02 §5.16` `activity` 段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ActivityGlobalCfg {
    /// 同时进行活动数上限（唯一性：恒 1）。
    pub max_concurrent: u32,
    /// 计时源（恒 `"wallClock"`）。
    pub tick_source: String,
    /// 单次活动最长时长（分钟；超过该时长的档位不提供）。
    pub max_duration_min: u32,
    /// 安静时段（23:00-05:00 不派遣）。
    pub quiet_hours: QuietHoursCfg,
    /// 深夜回归结算策略（D-1）。
    pub quiet_settlement: QuietSettlementCfg,
    /// 提前召回惩罚（`01 §6.13.1`）。
    pub recall_penalty: RecallPenaltyCfg,
    /// 打工日限（次）。
    pub daily_job_limit: u32,
    /// 心币日入账上限（QI-05 定版 350；入账侧硬顶，消费随 S8-M5）。
    pub coin_daily_cap: i64,
    /// 三档经济倍率（S8-M5 起消费）。
    pub economy_scale: EconomyScaleCfg,
    /// 打工岗位目录（`01 §6.13.3`）。
    pub jobs: Vec<JobCfg>,
    /// 课程目录（`01 §6.13.4`）。
    pub courses: Vec<CourseCfg>,
    /// 旅游目的地目录（`01 §6.13.5`）。
    pub trips: Vec<TripCfg>,
    /// 明信片挂件全局配置（D-2：画在宠物窗口内，只驻留 1 张）。
    pub postcard: PostcardCfg,
}

impl Default for ActivityGlobalCfg {
    fn default() -> Self {
        Self {
            max_concurrent: 1,
            tick_source: "wallClock".to_string(),
            max_duration_min: 240,
            quiet_hours: QuietHoursCfg::default(),
            quiet_settlement: QuietSettlementCfg::default(),
            recall_penalty: RecallPenaltyCfg::default(),
            daily_job_limit: 3,
            coin_daily_cap: 350,
            economy_scale: EconomyScaleCfg::default(),
            jobs: Vec::new(),
            courses: Vec::new(),
            trips: Vec::new(),
            postcard: PostcardCfg::default(),
        }
    }
}

/// 安静时段（`02 §5.16`：23:00-05:00）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct QuietHoursCfg {
    /// 起始本地时刻（HH:MM）。
    pub from: String,
    /// 结束本地时刻（HH:MM）。
    pub to: String,
    /// 延后结算策略（恒 `"auto"`）。
    pub defer_settlement: String,
}

impl Default for QuietHoursCfg {
    fn default() -> Self {
        Self { from: "23:00".to_string(), to: "05:00".to_string(), defer_settlement: "auto".to_string() }
    }
}

/// 深夜结算策略（D-1，`02 §5.16` `quietSettlement`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct QuietSettlementCfg {
    /// 回归演出静音。
    pub mute: bool,
    /// 结算弹窗夜间样式。
    pub style: String,
    /// 深夜回归零惩罚。
    pub no_penalty: bool,
    /// 延后补播小时（次日 07:00）。
    pub defer_until_hour: u8,
    /// 补播版本（`simple` = 简版回归演出）。
    pub replay: String,
    /// 该时段不推明信片。
    pub suppress_postcard: bool,
}

impl Default for QuietSettlementCfg {
    fn default() -> Self {
        Self {
            mute: true,
            style: "night".to_string(),
            no_penalty: true,
            defer_until_hour: 7,
            replay: "simple".to_string(),
            suppress_postcard: true,
        }
    }
}

/// 提前召回惩罚（`01 §6.13.1`：收益 ×0.5；Mood−4；P+6；rough+0.15）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RecallPenaltyCfg {
    /// 收益比例（0.5）。
    pub reward_ratio: f32,
    /// Mood 惩罚（-4）。
    pub mood: f32,
    /// 冷落压力惩罚（+6）。
    pub neglect_add: f32,
    /// 粗暴因子增量（+0.15）。
    pub rough_step: f32,
}

impl Default for RecallPenaltyCfg {
    fn default() -> Self {
        Self { reward_ratio: 0.5, mood: -4.0, neglect_add: 6.0, rough_step: 0.15 }
    }
}

/// 三档经济倍率（`02 §5.16`：casual 1.5 / standard 1.0 / diligent 0.7）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EconomyScaleCfg {
    /// 休闲档。
    pub casual: f32,
    /// 标准档。
    pub standard: f32,
    /// 勤奋档。
    pub diligent: f32,
}

impl Default for EconomyScaleCfg {
    fn default() -> Self {
        Self { casual: 1.5, standard: 1.0, diligent: 0.7 }
    }
}

/// 打工岗位（`01 §6.13.3` + `02 §5.16` `jobs[]`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct JobCfg {
    /// 岗位 ID（`W-01`…）。
    pub id: String,
    /// 岗位名（中文展示名）。
    pub name: String,
    /// 图标资源（`assets/activity/job_*.png`）。
    pub icon: String,
    /// 时薪（心币 / 分钟）。
    pub wage_per_minute: f32,
    /// 时长档位（分钟；如 [15, 30, 60]）。
    pub duration_options: Vec<u32>,
    /// 解锁条件（亲密度等级 / 技能；经济侧 S8-M5 消费）。
    pub unlock: UnlockCfg,
    /// 出发消耗（精力 / 清洁度）。
    pub cost: CostCfg,
    /// 收益修正乘子（mood≥80 ×1.15 等）。
    pub modifiers: Vec<ModifierCfg>,
    /// 打工随机事件表（`01 §6.13.3` WACT-E-*）。
    pub events: Vec<ActivityEventCfg>,
    /// 演出动作引用（`ACT-N-09` 出发 / `ACT-N-10` 回归）。
    pub action_ids: ActionRefCfg,
    /// 音效引用。
    pub sound: SoundRefCfg,
}

/// 课程（`01 §6.13.4` + `02 §5.16` `courses[]`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CourseCfg {
    /// 课程 ID（`CRS-01`…）。
    pub id: String,
    /// 课程名。
    pub name: String,
    /// 学费（心币；S8-M5 起扣款）。
    pub tuition: i64,
    /// 时长档位（分钟）。
    pub duration_options: Vec<u32>,
    /// 技能类型（etiquette / cooking / talent / knowledge / fitness）。
    pub skill_type: String,
    /// 出发消耗。
    pub cost: CostCfg,
    /// 技能点公式（`02 §5.16`：`floor(durationMin/30 * (0.85+0.3*diligence) * (mood>=70 ? 1.1 : 0.9))`）。
    pub point_formula: String,
    /// 最低技能点（1）。
    pub min_points: u32,
    /// 升级所需技能点（`10*level`）。
    pub level_up_cost: String,
    /// 必需道具（如 `book_etiquette`；S8-M5 背包检查）。
    pub require_item: Option<String>,
    /// 演出动作引用（出发 `ACT-N-09` / 桌面循环 `ACT-N-11`）。
    pub action_ids: ActionRefCfg,
}

/// 旅游目的地（`01 §6.13.5` + `02 §5.16` `trips[]`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TripCfg {
    /// 目的地 ID（`TR-01`…）。
    pub id: String,
    /// 目的地名。
    pub name: String,
    /// 时长（分钟；固定档）。
    pub duration_min: u32,
    /// 费用（心币 + 旅行券；S8-M5 起扣款）。
    pub cost: TripCostCfg,
    /// 解锁条件。
    pub unlock: UnlockCfg,
    /// 明信片间隔（分钟）。
    pub postcard_interval_min: u32,
    /// 产出（照片 / 纪念品 / Mood / 亲密度 / 清洁度）。
    pub rewards: TripRewardCfg,
    /// 天气 / 随机事件表（`01 §6.13.5` TACT-E-*）。
    pub weather_table: Vec<ActivityEventCfg>,
    /// 演出动作引用（出发 `ACT-N-12` / 明信片 `ACT-N-13` / 回归 `ACT-N-14`）。
    pub action_ids: ActionRefCfg,
}

/// 解锁条件（`02 §5.16`：亲密度等级 + 技能等级）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UnlockCfg {
    /// 所需亲密度等级（0 = 无要求）。
    pub affinity_level: u32,
    /// 所需技能等级（key = 技能类型；S8-M5 技能系统消费）。
    pub skills: std::collections::BTreeMap<String, u32>,
}

/// 出发消耗（`02 §5.16` `cost`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CostCfg {
    /// 精力消耗。
    pub energy: f32,
    /// 清洁度消耗。
    pub cleanliness: f32,
}

/// 收益修正乘子（`02 §5.16` `modifiers[]`：`when` 条件表达式 + 乘子）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModifierCfg {
    /// 条件表达式（`mood>=80` / `cleanliness<15` / `timeSegment==morning`）。
    pub when: String,
    /// 收益乘子。
    pub reward_multiplier: f32,
}

/// 随机事件（`02 §5.16` `events[]` / `weatherTable[]`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ActivityEventCfg {
    /// 事件 ID（`WACT-E-01` / `TACT-E-01`…）。
    pub id: String,
    /// 抽取权重。
    pub weight: u32,
    /// 触发条件表达式（可为空 = 无条件）。
    pub condition: String,
    /// 效果（收益乘子 / 数值变化）。
    pub effects: EventEffectsCfg,
    /// 台词键（`job.we01`…）。
    pub line_key: Option<String>,
}

/// 事件效果（`02 §5.16` `effects`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EventEffectsCfg {
    /// 收益乘子。
    pub reward_multiplier: Option<f32>,
    /// 心情变化。
    pub mood: Option<f32>,
    /// 清洁度变化。
    pub cleanliness: Option<f32>,
    /// 精力变化。
    pub energy: Option<f32>,
    /// 时长加成（分钟；`WACT-E-04` 加班 +10）。
    pub duration_add_min: Option<u32>,
    /// 技能点加成。
    pub skill_points: Option<u32>,
    /// 产出道具（`WACT-E-03` 粉丝送礼）。
    pub item_id: Option<String>,
}

/// 演出动作引用（`02 §5.16` `actionIds`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ActionRefCfg {
    /// 出发演出动作。
    pub depart: String,
    /// 回归演出动作。
    #[serde(default)]
    pub r#return: String,
    /// 学习桌面循环动作。
    pub desk_loop: String,
    /// 旅游明信片动作。
    pub postcard: String,
}

/// 音效引用（`02 §5.16` `sound`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SoundRefCfg {
    /// 出发音效。
    pub depart: Option<String>,
    /// 回归音效。
    pub r#return: Option<String>,
}

/// 旅游费用（`02 §5.16` `cost`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TripCostCfg {
    /// 心币费用。
    pub coin: i64,
    /// 旅行券道具 ID。
    pub ticket_item: String,
}

/// 旅游产出（`02 §5.16` `rewards`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TripRewardCfg {
    /// 照片道具 ID。
    pub photo: String,
    /// 纪念品道具 ID。
    pub souvenir: String,
    /// Mood 收益。
    pub mood: f32,
    /// 亲密度经验。
    pub affinity_exp: f32,
    /// 清洁度变化（负 = 消耗）。
    pub cleanliness: f32,
}

/// 明信片挂件（`02 §5.16` `postcard`；D-2：画在宠物窗口内，只驻留 1 张）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PostcardCfg {
    /// 挂件尺寸（逻辑像素宽 × 高）。
    pub size_px: [u32; 2],
    /// 默认位置（`bottom-right` / `top-right` / `top-left` / `bottom-left`）。
    pub position: String,
    /// 可拖动。
    pub draggable: bool,
    /// 可关闭。
    pub closable: bool,
}

impl Default for PostcardCfg {
    fn default() -> Self {
        Self { size_px: [160, 220], position: "bottom-right".to_string(), draggable: true, closable: true }
    }
}

// ---------------------------------------------------------------------------
// 单元测试（S2-M4 QA 补充：RoamCfg 向后兼容验收项）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// RoamCfg 向后兼容：旧版本配置 JSON（无 `walkSpeedPxPerSec` 字段）在
    /// 容器级 `#[serde(default)]` 兜底下反序列化不失败，新字段取内置默认 60.0。
    #[test]
    fn roam_cfg_old_json_without_walk_speed_deserializes() {
        let old_json = r#"{
            "pace": 1.5,
            "decisionIntervalSec": [5, 30],
            "cursorAvoidRadiusPx": 150
        }"#;
        let cfg: RoamCfg =
            serde_json::from_str(old_json).expect("旧 JSON（缺 walkSpeedPxPerSec）应可反序列化");
        assert_eq!(cfg.pace, 1.5);
        assert_eq!(cfg.decision_interval_sec, [5, 30]);
        assert_eq!(cfg.cursor_avoid_radius_px, 150);
        assert_eq!(cfg.walk_speed_px_per_sec, 60.0, "缺字段应兜底为内置默认 60.0");
    }

    /// RoamCfg 完整 JSON：camelCase 命名 + 新字段显式注入生效。
    #[test]
    fn roam_cfg_full_json_roundtrip_with_walk_speed() {
        let json = r#"{
            "pace": 2.0,
            "decisionIntervalSec": [3, 10],
            "cursorAvoidRadiusPx": 200,
            "walkSpeedPxPerSec": 120.0
        }"#;
        let cfg: RoamCfg =
            serde_json::from_str(json).expect("含 walkSpeedPxPerSec 的 JSON 应可反序列化");
        assert_eq!(cfg.walk_speed_px_per_sec, 120.0, "新字段显式值应生效");
        // 序列化回 JSON 再反序列化，字段不丢。
        let roundtrip: RoamCfg =
            serde_json::from_str(&serde_json::to_string(&cfg).expect("序列化应成功"))
                .expect("回环反序列化应成功");
        assert_eq!(roundtrip, cfg);
    }

    // ---- QA 探针（清单⑤：GestureCfg 缺省兼容 —— 空 JSON 全默认 / 部分键合并 / camelCase 回环）----

    #[test]
    fn qa_probe_gesture_cfg_empty_json_falls_back_to_k6_defaults() {
        let cfg: GestureCfg = serde_json::from_str("{}").expect("空对象应全字段走 serde default");
        assert_eq!(cfg, GestureCfg::default(), "空 JSON 与 Default 全等");
        // K-6 参数表 14 项逐项断言（缺键兜底必须是 K-6 表值，而非数值零）。
        assert_eq!(cfg.hover_enter_ms, 600);
        assert_eq!(cfg.hover_long_ms, 2000);
        assert_eq!(cfg.double_click_window_ms, 300);
        assert_eq!(cfg.long_press_ms, 500);
        assert_eq!(cfg.stroke_speed_max_px_per_sec, 800);
        assert_eq!(cfg.drag_threshold_px, 8);
        assert_eq!(cfg.throw_speed_min_px_per_sec, 1200);
        assert_eq!(cfg.tickle_window_ms, 2000);
        assert_eq!(cfg.tickle_clicks_min, 5);
        assert_eq!(cfg.trail_max_points, 32);
        assert_eq!(cfg.circle_turn_min_deg, 270);
        assert_eq!(cfg.line_residual_max_px, 12);
        assert_eq!(cfg.zigzag_reversal_min, 2);
        assert_eq!(cfg.zigzag_turn_min_deg, 60);
    }

    #[test]
    fn qa_probe_gesture_cfg_partial_json_merges_over_defaults() {
        let cfg: GestureCfg = serde_json::from_str(
            r#"{ "doubleClickWindowMs": 500, "dragThresholdPx": 50 }"#,
        )
        .expect("部分键 JSON 应与其余键的 default 合并");
        assert_eq!(cfg.double_click_window_ms, 500, "显式键生效");
        assert_eq!(cfg.drag_threshold_px, 50, "显式键生效");
        assert_eq!(cfg.hover_enter_ms, 600, "缺省键回落 K-6 默认");
        assert_eq!(cfg.throw_speed_min_px_per_sec, 1200, "缺省键回落 K-6 默认");
    }

    #[test]
    fn qa_probe_gesture_cfg_camel_case_roundtrip_is_lossless() {
        let json = serde_json::to_string(&GestureCfg::default()).expect("序列化应成功");
        let value: serde_json::Value = serde_json::from_str(&json).expect("回读 JSON 应成功");
        let obj = value.as_object().expect("顶层应为 JSON 对象");
        assert_eq!(obj.len(), 14, "14 键齐全（C7 camelCase）");
        for key in [
            "hoverEnterMs",
            "hoverLongMs",
            "doubleClickWindowMs",
            "longPressMs",
            "strokeSpeedMaxPxPerSec",
            "dragThresholdPx",
            "throwSpeedMinPxPerSec",
            "tickleWindowMs",
            "tickleClicksMin",
            "trailMaxPoints",
            "circleTurnMinDeg",
            "lineResidualMaxPx",
            "zigzagReversalMin",
            "zigzagTurnMinDeg",
        ] {
            assert!(obj.contains_key(key), "camelCase 键名应存在：{key}");
        }
        let roundtrip: GestureCfg = serde_json::from_str(&json).expect("回环反序列化应成功");
        assert_eq!(roundtrip, GestureCfg::default(), "camelCase 回环无损");
    }
}
