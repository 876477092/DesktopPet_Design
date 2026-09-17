//! `save::schema`：存档 v2 结构（`02 §5 K-7`「存档 Schema v2 新增字段全表」）。
//!
//! ## 边界（S5-M1 卡面「单次会话边界：只做 store/schema/恢复；迁移在 S8-M7」）
//!
//! 本模块只负责**结构定义**与**字段归属划分**，不含落盘 / 恢复 / 定时逻辑
//! （归 [`crate::save::store`]），也**不含 v1→v2 迁移**（归 S8-M7）。
//!
//! ## 字段归属四段（避免「谁写这个字段」扯皮）
//!
//! ```text
//!   A. 内核承接段（S5-M1 起真实读写）   v / values / emotion.{neglect,sensitivity,state} / meta
//!   B. 配置承接段（S5-M3/M4/M5 已接管）  pet.name / pet.catchphrase / settings.*
//!   C. 占位段（S7-M2 / S7-M4 接管）      emotion.{personality,personalityRolled,adapt,rough} / needs
//!   D. 占位段（S8-M5/M6/M7 接管）        economy / inventory / album / decor / photoWidget
//!                                        / skills / activity / activityCounter / counters
//! ```
//!
//! **B 段落地（T-15）**：`settings.*` 已由 S5-M1 的「三行占位」补齐为用户可改设置的全量
//! 有效值（`appearance` / `audio` / `behavior` / `reminders` + 既有 `emotionSensitivity` /
//! `privacy` / `performance`）。划分口径：`settings.json` = 出厂默认 + 取值域（安装目录只读），
//! 存档 B 段 = 用户改动后的有效值；`pet.name` 空串 = 未改名（取 `character.json.defaultName`）。
//!
//! C / D 两段在本卡**只冻结「字段名 + 默认值 + 往返不丢」三件事**，类型不发明领域语义：
//! 用 `serde_json::Value` 承载（与 `crate::event::PetSnapshotV2.activity` 的既有先例同口径）。
//! 理由：若在此凭空为经济 / 活动 / 背包造 Rust 类型，S8 落地时必然返工，且会把「空壳类型」
//! 伪装成已完成契约。默认值逐项取自 `02 §5 K-7` 全表，已由 `default_save.json` 固化为模板。
//!
//! ## 时间纪律（C3）
//!
//! 本模块**零时钟**：`meta.last_seen_ms` / `meta.last_tick_ms` 均由调用方注入。
//!
//! ## 不做的事（显式声明，防越界）
//!
//! - **暂停窗口不落盘**：`state::PauseWindow` 是会话内记账（跨进程重启无意义，
//!   离线补偿走 `EmotionEngine::offline_compensate`），见 `state.rs` 模块文档。
//! - **不落 P 的 `pending_since_ms` 语义扩展**：`NeglectPressure` 整体序列化，
//!   确认期状态随存档往返（避免重启后确认期计时凭空归零）。
//! - **不写盘位字面量**（C1）：路径一律由调用方注入。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::emotion::engine::{EmotionState, NeglectPressure, RateClamp, Sensitivity};
use crate::emotion::lines::CatchphraseFrequency;
use crate::state::{PetState, PetValues};

// ---------------------------------------------------------------------------
// 版本与文件名常量
// ---------------------------------------------------------------------------

/// 存档结构版本（`02 §5 K-7`：v2；**本卡不得改此值**，卡面「禁止顺手改动」）。
pub const SAVE_VERSION: u32 = 2;

/// 存档文件名（`01 FR-8-1`：`%APPDATA%/DesktopPet/save.json`）。
pub const SAVE_FILE: &str = "save.json";

/// 全新存档模板文件名（`02 §3`：`resources/config/default_save.json`）。
///
/// **版本号与 [`SAVE_VERSION`] 必须一致**：模板是「全新用户的首档」，不是历史档；
/// 由 `default_save_template_matches_builtin` 单测机械校验。
pub const SAVE_TEMPLATE_FILE: &str = "default_save.json";

/// v1 存档备份文件名后缀（`02 §5 K-7`「cp save.json → save.json.v1bak（仅首次，永不覆盖）」）。
///
/// 本卡**只定义常量**，写入动作归 S8-M7（迁移）。
pub const V1_BAK_SUFFIX: &str = "v1bak";

/// 原子写临时文件后缀（`02 §5 K-7`；`save.json.tmp`）。
pub const TMP_SUFFIX: &str = "json.tmp";

/// 上一次成功落盘的完整备份后缀（`02 §5 K-7`；`save.json.bak`）。
pub const BAK_SUFFIX: &str = "json.bak";

// ---------------------------------------------------------------------------
// 存档根
// ---------------------------------------------------------------------------

/// 存档 v2 根结构（`02 §5 K-7` 全表；serde `camelCase` 与 TS 侧 / 设置页「数据」Tab 同源）。
///
/// 容器级 `#[serde(default)]`：老档缺字段一律由 [`Self::default`] 补齐（前向兼容），
/// 未知字段被忽略（与 `ConfigService::load_one` 同容错口径）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SaveFileV2 {
    /// 结构版本（**读取方必须先看此字段再决定是否解析**，`02 §5 K-7` 加载链）。
    pub v: u32,
    /// 六维数值（`01 §6.5.1`；本卡承接段 A）。
    pub values: PetValues,
    /// 情绪内核（P / 敏感度 / 展示态 / 性格 / 自适应 / 粗暴度）。
    pub emotion: EmotionSave,
    /// 宠物身份（名字 + 口头禅设置）。
    pub pet: PetSave,
    /// 生存需求冷却时间戳（`01 §6.12`；占位段 C，运行期写入归 S7-M2）。
    pub needs: NeedsSave,
    /// 用户设置覆盖（`01 §6.7`；占位段 B，写入归 S5-M3/M4/M5）。
    pub settings: SettingsSave,
    /// 计数与日切容器（占位段 D）。
    pub counters: CountersSave,
    /// 存档元信息（最后见面 / 最后 tick / 日切键；本卡承接段 A）。
    pub meta: SaveMeta,
    /// 经济（占位段 D；`02 §5 K-7`：真值归 S8-M5）。
    pub economy: serde_json::Value,
    /// 背包（占位段 D；归 S8-M6）。
    pub inventory: serde_json::Value,
    /// 相册（占位段 D；归 S10-M1）。
    pub album: serde_json::Value,
    /// 桌面装饰 5 槽（占位段 D；归 S8-M6/S10-M1）。
    pub decor: serde_json::Value,
    /// 桌面明信片挂件（占位段 D；归 S8-M3）。
    pub photo_widget: serde_json::Value,
    /// 技能（占位段 D；归 S8/S9）。
    pub skills: serde_json::Value,
    /// 外出活动实例（占位段 D；归 S8-M1，恒 `null` 或活动对象）。
    pub activity: serde_json::Value,
    /// 活动计数（占位段 D；归 S8-M1）。
    pub activity_counter: serde_json::Value,
}

impl Default for SaveFileV2 {
    /// 全新存档默认值：逐项对齐 `02 §5 K-7` 全表的「默认」列。
    fn default() -> Self {
        Self {
            v: SAVE_VERSION,
            values: PetValues::default(),
            emotion: EmotionSave::default(),
            pet: PetSave::default(),
            needs: NeedsSave::default(),
            settings: SettingsSave::default(),
            counters: CountersSave::default(),
            meta: SaveMeta::default(),
            economy: default_economy(),
            inventory: serde_json::Value::Array(Vec::new()),
            album: serde_json::Value::Array(Vec::new()),
            decor: serde_json::Value::Array(vec![serde_json::Value::Null; DECOR_SLOTS]),
            photo_widget: serde_json::Value::Null,
            skills: default_skills(),
            activity: serde_json::Value::Null,
            activity_counter: default_activity_counter(),
        }
    }
}

/// 桌面装饰槽位数（`01 FR-13`：5 槽；`02 §5 K-7` 默认 `[null×5]`）。
pub const DECOR_SLOTS: usize = 5;

/// 技能键清单（`01 FR-13`：礼仪 / 烹饪 / 才艺 / 知识 / 体魄；`02 §5 K-7` 默认各 `{level:1,points:0}`）。
pub const SKILL_KEYS: [&str; 5] = ["etiquette", "cooking", "talent", "knowledge", "physique"];

impl SaveFileV2 {
    /// 结构版本是否为本卡可解析的 v2。
    #[inline]
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.v == SAVE_VERSION
    }

    /// 从任意 JSON 值探测其结构版本（`None` = 非对象 / 无 `v` 字段 = 未知档）。
    ///
    /// 加载链（`02 §5 K-7`）**必须先看版本再解析**：未知档若直接按 v2 反序列化会得到
    /// 一份「字段全默认」的假档，静默丢掉用户数据。
    #[must_use]
    pub fn peek_version(value: &serde_json::Value) -> Option<u32> {
        value.get("v").and_then(serde_json::Value::as_u64).map(|v| v as u32)
    }

    /// 用当前内核状态刷新存档（`02 §5 K-7`）。
    ///
    /// 覆写范围：
    ///   - **段 A**：`values` / `emotion.{neglect,sensitivity,state}` / `meta`
    ///     —— ② 由 `to_pet_state` + `EmotionEngine::restore` 消费；
    ///   - **段 C 因子侧**（**S7-M4 增量**）：`emotion.{personality,personalityRolled,adapt,rough}`
    ///     —— ① 与 `EmotionEngine::restore_factors` 严格成对；S7-M4 前这些字段由
    ///     S5-M1 取配置默认占位，自 S7-M4 起**由内核写**（「谁拥有谁写」不变量不变，
    ///     只是归属方从 S5-M1 转交 S7-M4）。
    ///
    /// 段 B / D 段**原样保留**——它们由各自的归口模块写入（配置 / S8）。
    ///
    /// `now_ms` 为墙钟毫秒（由调用方经 `WallClock` 注入，C3）。
    pub fn capture_from(&mut self, engine: &crate::emotion::EmotionEngine<'_>, now_ms: i64) {
        self.v = SAVE_VERSION;
        self.values = engine.state.values;
        self.emotion.neglect = engine.neglect;
        self.emotion.sensitivity = engine.sensitivity;
        self.emotion.state = engine.state.emotion;
        // S7-M4：C 段因子侧（性格五维 / 首建标记 / 自适应基线 / 粗暴计量）。
        let p = engine.personality();
        self.emotion.personality = PersonalitySave {
            clingy: p.clingy,
            curiosity: p.curiosity,
            temper: p.temper,
            courage: p.courage,
            diligence: p.diligence,
        };
        self.emotion.personality_rolled = engine.personality_rolled();
        self.emotion.adapt = engine.adapt().to_save();
        self.emotion.rough = engine.rough().to_save();
        // 段 A 的元信息：`last_tick_ms` 同时镜像在 `state` 与引擎内部，取快照侧即可
        // （`tick_1s` / `offline_compensate` 都同步写了 `state.last_tick_ms`）。
        self.meta.last_tick_ms = engine.state.last_tick_ms;
        self.meta.today = engine.state.today.clone();
        self.meta.last_seen_ms = now_ms;
    }

    /// 由存档投影内核状态容器（**S5-M2 补偿接入**；`02 §5 K-7` 段 A）。
    ///
    /// 口径：
    /// - `values` / `emotion` / `lastTickMs` / `today` 逐项来自存档；
    /// - `pause` **取默认空窗口**——暂停窗口是会话内记账，不做持久化
    ///   （见 `crate::state::PauseWindow` 模块文档），跨进程重启的暂停窗口无意义。
    ///
    /// 返回的 `PetState` 配 [`crate::emotion::EmotionEngine::restore`] 使用。
    #[must_use]
    pub fn to_pet_state(&self) -> PetState {
        PetState {
            values: self.values,
            emotion: self.emotion.state,
            pause: crate::state::PauseWindow::default(),
            last_tick_ms: self.meta.last_tick_ms,
            today: self.meta.today.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// 段 A/B/C：情绪、身份、需求、设置、计数器、元信息
// ---------------------------------------------------------------------------

/// 情绪内核存档段（`02 §5 K-7`：`emotion.*`）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EmotionSave {
    /// 冷落压力与阶段（段 A；含确认期 `pendingLevel` / `pendingSinceMs`）。
    pub neglect: NeglectPressure,
    /// 敏感度（段 A；FR-11-11）。
    pub sensitivity: Sensitivity,
    /// 性格五维当前值（段 C；运行期写入归 S7-M4，本卡取配置默认）。
    pub personality: PersonalitySave,
    /// 性格是否已重掷过（段 C；归 S7-M4）。
    pub personality_rolled: bool,
    /// 自适应基线（段 C；归 S7-M4）。
    pub adapt: AdaptationSave,
    /// 粗暴对待计量（段 C；归 S7-M4）。
    pub rough: RoughSave,
    /// 展示态（段 A）：`Longing` / `Sleepy` 等演出态需要跨会话保持。
    ///
    /// **编码口径**：沿用内核 [`EmotionState`] 自身的 serde 编码（`PascalCase`，即
    /// `02 §4.3` 的 14 值冻结词表名 `Idle` / `Sulking` / `Longing` …）。
    /// **刻意不复用** `event::EmotionStateWire`（camelCase）：那是 `pet://state` 的
    /// **线上**字段，与本字段是两回事；复用会引入「Wire ⇄ 内核」双向映射表，
    /// 违背单一真源。此处的差异是**有意保留**的，不是缺陷。
    pub state: EmotionState,
}

impl Default for EmotionSave {
    /// 全新存档的情绪段默认：逐项对齐 `02 §5 K-7` 全表。
    ///
    /// `neglect.cap` 取 `emotion.json.busyness.capFree`（120）而非 `NeglectPressure::default()`
    /// 的 `0.0` —— K-7 表把「当前封顶」的初始值定义为空闲档上限（C7：数值取自配置，
    /// 不在代码里写 120 字面量）。运行期首拍会按真实忙碌档重算，此处只管「未 tick 前」的值。
    fn default() -> Self {
        let cfg = crate::config::model::EmotionConfig::default();
        Self {
            neglect: NeglectPressure {
                cap: cfg.busyness.cap_free as f32,
                ..NeglectPressure::default()
            },
            sensitivity: Sensitivity {
                value: cfg.sensitivity.value,
                // 配置侧 `RateClampCfg` → 内核侧 `RateClamp`：字段显式搬运（F-05 同款收口，
                // 任一侧改字段即编译报错，杜绝同名直转的静默错位）。
                rate_clamp: RateClamp {
                    min: cfg.sensitivity.rate_clamp.min,
                    max: cfg.sensitivity.rate_clamp.max,
                },
            },
            personality: PersonalitySave::default(),
            personality_rolled: false,
            adapt: AdaptationSave::default(),
            rough: RoughSave::default(),
            state: EmotionState::Idle,
        }
    }
}

/// 性格五维存档（`02 §5 K-7` 默认 `{50,70,55,50,60}`；字段名与
/// `config::model::PersonalityDefaultsCfg` 逐项一致，避免两套命名）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersonalitySave {
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

impl Default for PersonalitySave {
    /// 与 `PersonalityDefaultsCfg::default()` 同值（`02 §5.7` 冻结五维默认）。
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

/// 自适应基线存档（`02 §5 K-7` 默认 `{samples:[],tExp:25,coolDays:0,zeroDays:0}`）。
///
/// `tExp` 的初值 `30 − 10 × 粘人度` 是**运行时派生**（`02 §5.4` 存储注记：
/// 配置中不存在扁平 `adapt.exp0`，出现即 Schema 报错），故此处硬编码 25 只作为
/// 「未派生前的存档槽初值」，运行时由 S7-M4 覆写。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AdaptationSave {
    /// 近 7 日交互样本（`02 §5.4`：最多 7 条 ≈ 420B）。
    pub samples: Vec<AdaptSampleSave>,
    /// 期望交互间隔（分钟）。
    pub t_exp: f32,
    /// 关系降温累计天数。
    pub cool_days: u32,
    /// 零交互降温累计天数。
    pub zero_days: u32,
}

impl Default for AdaptationSave {
    fn default() -> Self {
        Self { samples: Vec::new(), t_exp: 25.0, cool_days: 0, zero_days: 0 }
    }
}

/// 单日交互样本（`02 §5.4` `DailySample`）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AdaptSampleSave {
    /// 日期键（`YYYY-MM-DD`）。
    pub date: String,
    /// 当日交互间隔累计（分钟）。
    pub interval_sum_min: f32,
    /// 当日间隔采样次数。
    pub interval_samples: u32,
    /// 当日交互次数（关系降温判定用）。
    pub interact_count: u32,
}

impl Default for AdaptSampleSave {
    fn default() -> Self {
        Self { date: String::new(), interval_sum_min: 0.0, interval_samples: 0, interact_count: 0 }
    }
}

/// 粗暴对待计量存档（`02 §5 K-7` 默认 `{value:1.0,lastNegativeMs:0}`）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RoughSave {
    /// 粗暴度 1.0~`rough.max`。
    pub value: f32,
    /// 最近一次负向事件墙钟毫秒。
    pub last_negative_ms: i64,
}

impl Default for RoughSave {
    fn default() -> Self {
        Self { value: 1.0, last_negative_ms: 0 }
    }
}

/// 宠物身份存档（`01 FR-8-1`「宠物名」+ `02 §5 K-7` `pet.catchphrase`）。
///
/// 字段默认值均为类型默认（空名 + `CatchphraseSave::default()`），故直接 derive。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PetSave {
    /// 用户自定义名（FR-7-1；空串 = 未改名，取 `character.json.defaultName`）。
    ///
    /// C2：代码内零角色名字面量，默认空串即「未设置」语义。
    pub name: String,
    /// 口头禅设置（L-03：枚举档位，**作废 `"1:3"` 字符串比例**）。
    pub catchphrase: CatchphraseSave,
}

/// 口头禅设置存档（`02 §5 K-7` 默认 `{enabled:true,"standard"}`）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CatchphraseSave {
    /// 开关。
    pub enabled: bool,
    /// 频率档位（枚举；`off` / `low` / `standard` / `high`）。
    pub frequency: CatchphraseFrequency,
}

impl Default for CatchphraseSave {
    fn default() -> Self {
        Self { enabled: true, frequency: CatchphraseFrequency::Standard }
    }
}

/// 需求冷却存档（`02 §5 K-7` `needs.*`；三项默认 `0` = 类型默认，直接 derive）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NeedsSave {
    /// 免费洗澡冷却截止墙钟毫秒。
    pub bath_free_cd_until_ms: i64,
    /// 香味 Buff 截止墙钟毫秒。
    pub scent_buff_until_ms: i64,
    /// 去污泡泡减速截止墙钟毫秒。
    pub clean_slow_until_ms: i64,
}

/// 设置覆盖存档（`02 §5 K-7` `settings.*` 三行 + **T-15/S5-M3/M4/M5 补齐段 B 全量**）。
///
/// 段归属：本段由 **S5-M3（设置面板 UI）/ S5-M4（热更新） / S5-M5（偏好项）** 读写；
/// `settings.json` 提供**出厂默认与取值域**，本段承载**用户改动后的有效值**——
/// 「配置只读 / 用户数据落在存档」的既有划分（`02 §3`：安装目录资源只读）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsSave {
    /// 情绪敏感度（FR-11-11；与 `settings.json.emotion.sensitivity` 同源）。
    pub emotion_sensitivity: f32,
    /// 隐私。
    pub privacy: PrivacySave,
    /// 性能。
    pub performance: PerformanceSave,
    /// 外观（T-15 / FR-7-2 / FR-7-6 / FR-7-7）。
    pub appearance: AppearanceSave,
    /// 声音（T-15 / FR-7-3）。
    pub audio: AudioSave,
    /// 行为（T-15 / FR-7-4 / FR-1-2 / FR-1-9）。
    pub behavior: BehaviorSave,
    /// 提醒偏好（T-15 / FR-10-2；出厂默认值来自 `schedule.json`）。
    pub reminders: RemindersSave,
}

impl Default for SettingsSave {
    fn default() -> Self {
        Self {
            emotion_sensitivity: 1.0,
            privacy: PrivacySave::default(),
            performance: PerformanceSave::default(),
            appearance: AppearanceSave::default(),
            audio: AudioSave::default(),
            behavior: BehaviorSave::default(),
            reminders: RemindersSave::default(),
        }
    }
}

/// 外观设置存档（`01 FR-7-2 / FR-7-6 / FR-7-7`；默认值与 `settings.json` 同源）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppearanceSave {
    /// 缩放百分比（50~200）。
    pub scale_percent: u32,
    /// 主体不透明度百分比（60~100）。
    pub opacity_percent: u32,
    /// 界面语言（`zh-CN` / `en-US`；非法值由消费侧回退默认语言）。
    pub language: String,
}

impl Default for AppearanceSave {
    fn default() -> Self {
        Self { scale_percent: 100, opacity_percent: 100, language: "zh-CN".to_string() }
    }
}

/// 声音设置存档（`01 FR-7-3`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioSave {
    /// 主音量百分比（0~100）。
    pub master_volume_percent: u32,
    /// 是否静音。
    pub muted: bool,
}

impl Default for AudioSave {
    fn default() -> Self {
        Self { master_volume_percent: 80, muted: false }
    }
}

/// 行为开关存档（`01 FR-7-4 / FR-1-2 / FR-1-9`）。
///
/// `always_on_top_policy` 取值 `Always` / `BelowFullscreen` / `Never`（`02 K-1` 三态；
/// 与 `settings.json` 同字面量，**不新增枚举**，避免 Rust ⇄ 配置双向映射表）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BehaviorSave {
    /// 自动走动。
    pub auto_roam: bool,
    /// 漫游节奏（`01 §8.3`「节奏」；档位候选值来自 `settings.json.roam.paceOptions`）。
    pub roam_pace: f32,
    /// 勿扰模式（`01 FR-10-4`）。
    pub do_not_disturb: bool,
    /// 轻松模式（`01 §6.5.2` Q-E：抚摸门槛 5s → `easyModeStrokeSec`）。
    pub easy_coax_mode: bool,
    /// 鼠标穿透（`01 FR-1-6`）。
    pub click_through: bool,
    /// 置顶策略（三态字面量）。
    pub always_on_top_policy: String,
    /// 开机自启（`01 FR-1-9`；注册表 Run 项的真实状态由 S5-M4 写入后回填）。
    pub autostart: bool,
}

impl Default for BehaviorSave {
    fn default() -> Self {
        Self {
            auto_roam: true,
            roam_pace: 1.0,
            do_not_disturb: false,
            easy_coax_mode: false,
            click_through: false,
            always_on_top_policy: "Always".to_string(),
            autostart: false,
        }
    }
}

/// 提醒偏好存档（`01 FR-10-2`；出厂默认值来自 `schedule.json`，此处存用户改动值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RemindersSave {
    /// 久坐提醒开关。
    pub sedentary_enabled: bool,
    /// 久坐提醒间隔（分钟）。
    pub sedentary_interval_min: u32,
    /// 喝水提醒开关。
    pub water_enabled: bool,
    /// 喝水提醒间隔（分钟）。
    pub water_interval_min: u32,
}

impl Default for RemindersSave {
    fn default() -> Self {
        Self {
            sedentary_enabled: true,
            sedentary_interval_min: 45,
            water_enabled: true,
            water_interval_min: 45,
        }
    }
}

/// 隐私设置（Q-18：`activitySensing` 默认开）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PrivacySave {
    /// 活动感知开关（关闭后退化为纯时间模型）。
    pub activity_sensing: bool,
}

impl Default for PrivacySave {
    fn default() -> Self {
        Self { activity_sensing: true }
    }
}

/// 性能设置（FR-15：渲染后端 `auto` / `skeleton` / `frame`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PerformanceSave {
    /// 渲染后端档位。
    pub renderer: String,
}

impl Default for PerformanceSave {
    fn default() -> Self {
        Self { renderer: "auto".to_string() }
    }
}

/// 计数与日切容器（段 D；`02 §5 K-7` `counters.dailyTasks`）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CountersSave {
    /// 每日任务进度（`[{id,progress}]`；归 S8 每日任务）。
    pub daily_tasks: serde_json::Value,
}

impl Default for CountersSave {
    fn default() -> Self {
        Self { daily_tasks: serde_json::Value::Array(Vec::new()) }
    }
}

/// 存档元信息（段 A；三项默认均为类型默认，直接 derive）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SaveMeta {
    /// 最近一次落盘时的墙钟毫秒（FR-8-1「最后见面时间」）。
    pub last_seen_ms: i64,
    /// 最近一次业务 tick 的墙钟毫秒（**离线补偿的唯一起点**，`02 §5.5`）。
    pub last_tick_ms: i64,
    /// 日切键（`YYYY-MM-DD`；空串 = 未日切）。
    pub today: String,
}

// ---------------------------------------------------------------------------
// 段 D 默认值（`02 §5 K-7` 全表逐项）
// ---------------------------------------------------------------------------

/// 经济默认值（`02 §5 K-7`：新档 `coin=0`、空账本、空配额、空连续登录）。
///
/// ⚠️ `coin` 的 `min(20×亲密度等级,200)` 是 **v1 迁移补偿**（D-5），归 S8-M7；
/// 本卡只给全新档的 `0`。
fn default_economy() -> serde_json::Value {
    serde_json::json!({
        "coin": 0,
        "ledger": [],
        "earnedToday": 0,
        "dayKey": "",
        "quotas": {},
        "loginStreak": { "days": 0, "dayKey": "" }
    })
}

/// 技能默认值（`02 §5 K-7`：五项各 `{level:1,points:0}`）。
fn default_skills() -> serde_json::Value {
    let mut map = BTreeMap::new();
    for key in SKILL_KEYS {
        map.insert(key.to_string(), serde_json::json!({ "level": 1, "points": 0 }));
    }
    serde_json::Value::Object(map.into_iter().collect())
}

/// 活动计数默认值（`02 §5 K-7`：`{todayJobCount:0,dayKey:""}`）。
fn default_activity_counter() -> serde_json::Value {
    serde_json::json!({ "todayJobCount": 0, "dayKey": "" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{EmotionConfig, NeedsConfig};

    /// 工程根（dp-core 位于 crates/dp-core，上溯三级；C1 无盘符字面量）。
    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../")
    }

    fn template_path() -> std::path::PathBuf {
        repo_root().join("resources").join("config").join(SAVE_TEMPLATE_FILE)
    }

    #[test]
    fn default_is_v2_and_has_frozen_defaults() {
        let s = SaveFileV2::default();
        assert_eq!(s.v, SAVE_VERSION, "版本号必须是 v2");
        assert!(s.is_current());
        // 六维默认（`02 §5 K-7` / `emotion.json`）
        assert_eq!(s.values.mood, 60.0);
        assert_eq!(s.values.energy, 100.0);
        assert_eq!(s.values.affinity_level, 1);
        assert_eq!(s.values.satiety, 70.0);
        assert_eq!(s.values.cleanliness, 85.0);
        // neglect 默认 `{p:0,cap:120,level:0,pendingLevel:0,pendingSinceMs:null,ratePerMin:0}`
        assert_eq!(s.emotion.neglect.level, 0);
        assert_eq!(s.emotion.neglect.p, 0.0);
        assert_eq!(s.emotion.neglect.cap, 120.0, "K-7 表：cap 初值 = busyness.capFree");
        assert_eq!(s.emotion.neglect.pending_level, 0);
        assert_eq!(s.emotion.neglect.rate_per_min, 0.0);
        assert!(s.emotion.neglect.pending_since_ms.is_none());
        // sensitivity 默认 `{value:1.0, rateClamp:{min:0.5,max:1.6}}`
        assert_eq!(s.emotion.sensitivity.value, 1.0);
        assert_eq!(s.emotion.sensitivity.rate_clamp.min, 0.5);
        assert_eq!(s.emotion.sensitivity.rate_clamp.max, 1.6);
        // personality `{50,70,55,50,60}`
        assert_eq!(s.emotion.personality.clingy, 0.50);
        assert_eq!(s.emotion.personality.curiosity, 0.70);
        assert_eq!(s.emotion.personality.temper, 0.55);
        assert_eq!(s.emotion.personality.courage, 0.50);
        assert_eq!(s.emotion.personality.diligence, 0.60);
        assert!(!s.emotion.personality_rolled);
        // adapt `{samples:[],tExp:25,coolDays:0,zeroDays:0}`
        assert!(s.emotion.adapt.samples.is_empty());
        assert_eq!(s.emotion.adapt.t_exp, 25.0);
        assert_eq!(s.emotion.adapt.cool_days, 0);
        assert_eq!(s.emotion.adapt.zero_days, 0);
        // rough `{1.0, 0}`
        assert_eq!(s.emotion.rough.value, 1.0);
        assert_eq!(s.emotion.rough.last_negative_ms, 0);
        // catchphrase `{true,"standard"}`（枚举，非 "1:3"）
        assert!(s.pet.catchphrase.enabled);
        assert_eq!(s.pet.catchphrase.frequency, CatchphraseFrequency::Standard);
        // needs 三项 0
        assert_eq!(s.needs.bath_free_cd_until_ms, 0);
        assert_eq!(s.needs.scent_buff_until_ms, 0);
        assert_eq!(s.needs.clean_slow_until_ms, 0);
        // settings
        assert_eq!(s.settings.emotion_sensitivity, 1.0);
        assert!(s.settings.privacy.activity_sensing);
        assert_eq!(s.settings.performance.renderer, "auto");
        // economy 新档 coin=0
        assert_eq!(s.economy["coin"], 0);
        assert_eq!(s.economy["earnedToday"], 0);
        assert_eq!(s.economy["loginStreak"]["days"], 0);
        // decor 恒 5 槽 null
        assert_eq!(s.decor.as_array().map(Vec::len), Some(DECOR_SLOTS));
        assert!(s.decor.as_array().unwrap().iter().all(serde_json::Value::is_null));
        // skills 五键各 level 1
        for key in SKILL_KEYS {
            assert_eq!(s.skills[key]["level"], 1, "{key}");
            assert_eq!(s.skills[key]["points"], 0, "{key}");
        }
        assert!(s.activity.is_null());
        assert_eq!(s.activity_counter["todayJobCount"], 0);
        assert!(s.inventory.as_array().is_some_and(Vec::is_empty));
        assert!(s.album.as_array().is_some_and(Vec::is_empty));
        assert!(s.photo_widget.is_null());
        assert!(s.counters.daily_tasks.as_array().is_some_and(Vec::is_empty));
    }

    /// 模板文件与内置默认**必须逐位一致**（防「模板改了忘改代码」的双真源漂移）。
    #[test]
    fn default_save_template_matches_builtin() {
        let path = template_path();
        assert!(path.is_file(), "缺少存档模板：{}", path.display());
        let text = std::fs::read_to_string(&path).expect("读取模板失败");
        let value: serde_json::Value = serde_json::from_str(&text).expect("模板应为合法 JSON");
        assert_eq!(SaveFileV2::peek_version(&value), Some(SAVE_VERSION), "模板版本必须是 v2");
        let parsed: SaveFileV2 = serde_json::from_value(value).expect("模板应可反序列化");
        assert_eq!(parsed, SaveFileV2::default(), "模板与内置默认不一致（双真源漂移）");
    }

    #[test]
    fn serde_is_camel_case_and_round_trips() {
        let s = SaveFileV2::default();
        let json = serde_json::to_value(&s).expect("可序列化");
        // `02 §5 K-7` 表路径逐项存在（camelCase）
        for key in [
            "v", "values", "emotion", "pet", "needs", "settings", "counters", "meta", "economy",
            "inventory", "album", "decor", "photoWidget", "skills", "activity", "activityCounter",
        ] {
            assert!(json.get(key).is_some(), "缺少顶层字段 {key}");
        }
        assert!(json["emotion"].get("personalityRolled").is_some());
        assert!(json["emotion"].get("sensitivity").is_some());
        assert_eq!(json["values"]["affinityLevel"], 1);
        assert_eq!(json["pet"]["catchphrase"]["frequency"], "standard");
        assert_eq!(
            json["emotion"]["state"], "Idle",
            "沿用内核 EmotionState 的 PascalCase 词表名（非线上 camelCase）"
        );
        assert_eq!(json["settings"]["privacy"]["activitySensing"], true);
        assert_eq!(json["settings"]["performance"]["renderer"], "auto");
        assert_eq!(json["meta"]["lastTickMs"], 0);
        // 无 snake_case 泄漏
        assert!(json["emotion"].get("personality_rolled").is_none());
        assert!(json["settings"].get("emotion_sensitivity").is_none());
        // 往返无损
        let back: SaveFileV2 = serde_json::from_value(json).expect("可反序列化");
        assert_eq!(back, s);
    }

    /// T-15（段 B 落地）：用户可改设置全量落存档，且与 `settings.json` 出厂默认同值。
    #[test]
    fn settings_segment_covers_sections_with_config_defaults() {
        let s = SettingsSave::default();
        let cfg = crate::config::model::SettingsConfig::default();
        // 外观 / 声音 / 行为：默认值与 settings.json 同源（三处逐项比对，防双真源）。
        assert_eq!(s.appearance.scale_percent, cfg.appearance.scale_percent);
        assert_eq!(s.appearance.opacity_percent, cfg.appearance.opacity_percent);
        assert_eq!(s.appearance.language, cfg.appearance.language);
        assert_eq!(s.audio.master_volume_percent, cfg.audio.master_volume_percent);
        assert_eq!(s.audio.muted, cfg.audio.muted);
        assert_eq!(s.behavior.auto_roam, cfg.behavior.auto_roam);
        assert_eq!(s.behavior.do_not_disturb, cfg.behavior.do_not_disturb);
        assert_eq!(s.behavior.click_through, cfg.behavior.click_through);
        assert_eq!(s.behavior.always_on_top_policy, cfg.behavior.always_on_top_policy);
        assert_eq!(s.emotion_sensitivity, cfg.emotion.sensitivity_value);
        assert_eq!(s.privacy.activity_sensing, cfg.privacy.activity_sensing);
        // 自启默认关（`01 FR-1-9`：用户显式开启才写注册表）。
        assert!(!s.behavior.autostart);

        // 序列化后四段齐全且 camelCase（设置页「数据」Tab / 设置窗口读档同源）。
        let json = serde_json::to_value(&s).expect("可序列化");
        for key in ["appearance", "audio", "behavior", "reminders", "privacy", "performance"] {
            assert!(json.get(key).is_some(), "settings 段缺少 {key}");
        }
        assert_eq!(json["behavior"]["alwaysOnTopPolicy"], "Always");
        assert_eq!(json["reminders"]["sedentaryIntervalMin"], 45);
    }

    /// 提醒偏好默认值必须与 `schedule.json` 出厂默认同值（两处默认值漂移即测试失败）。
    #[test]
    fn reminder_save_defaults_match_schedule_config() {
        let save = RemindersSave::default();
        let schedule = crate::config::model::ScheduleConfig::default();
        assert_eq!(save.sedentary_enabled, schedule.reminders.sedentary_enabled);
        assert_eq!(save.sedentary_interval_min, schedule.reminders.sedentary_interval_min);
        assert_eq!(save.water_enabled, schedule.reminders.water_enabled);
        assert_eq!(save.water_interval_min, schedule.reminders.water_interval_min);
        // 出厂默认间隔必须落在取值域内（否则设置页滑杆首帧即越界）。
        assert!(
            schedule.reminders.sedentary_interval_min >= schedule.reminders.interval_min_min
                && schedule.reminders.sedentary_interval_min <= schedule.reminders.interval_max_min
        );
    }

    /// 老档前向兼容：B 段新字段缺失时由 `default` 补齐（不阻断载档，R19）。
    #[test]
    fn settings_segment_tolerates_missing_new_fields() {
        // 模拟 S5-M1 期的老存档：settings 只有三行。
        let legacy = serde_json::json!({
            "emotionSensitivity": 1.3,
            "privacy": { "activitySensing": false },
            "performance": { "renderer": "frame" }
        });
        let s: SettingsSave = serde_json::from_value(legacy).expect("老档应可解析");
        assert_eq!(s.emotion_sensitivity, 1.3, "已有值必须保留");
        assert!(!s.privacy.activity_sensing);
        assert_eq!(s.appearance.scale_percent, 100, "新字段由默认补齐");
        assert_eq!(s.behavior.always_on_top_policy, "Always");
        assert_eq!(s.reminders.water_interval_min, 45);
    }

    /// L-03：存档里的口头禅档位必须是**枚举字符串**（作废 `"1:3"`），
    /// 且与配置侧 [`CatchphraseFrequency::as_cfg_name`] 逐字一致（单一真源）。
    #[test]
    fn catchphrase_frequency_serde_matches_config_names() {
        for freq in [
            CatchphraseFrequency::Off,
            CatchphraseFrequency::Low,
            CatchphraseFrequency::Standard,
            CatchphraseFrequency::High,
        ] {
            let json = serde_json::to_value(freq).expect("档位可序列化");
            assert_eq!(
                json,
                serde_json::json!(freq.as_cfg_name()),
                "serde 名与 as_cfg_name 必须同源"
            );
            let back: CatchphraseFrequency = serde_json::from_value(json).expect("档位可反序列化");
            assert_eq!(back, freq);
        }
        // 存档默认 `{enabled:true,"standard"}`；作废写法必须不被接受为 Standard。
        let save = CatchphraseSave::default();
        assert!(save.enabled);
        assert_eq!(save.frequency, CatchphraseFrequency::Standard);
        assert_eq!(serde_json::to_value(save).unwrap()["frequency"], "standard");
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_fields_defaulted() {
        let mut json = serde_json::to_value(SaveFileV2::default()).expect("可序列化");
        json.as_object_mut().unwrap().remove("values");
        json["futureField"] = serde_json::json!(123);
        let back: SaveFileV2 = serde_json::from_value(json).expect("缺字段应补默认、未知字段应忽略");
        assert_eq!(back.values, PetValues::default());
    }

    #[test]
    fn peek_version_reads_v_or_none() {
        assert_eq!(SaveFileV2::peek_version(&serde_json::json!({ "v": 2 })), Some(2));
        assert_eq!(SaveFileV2::peek_version(&serde_json::json!({ "v": 1 })), Some(1));
        assert_eq!(SaveFileV2::peek_version(&serde_json::json!({ "v": 99 })), Some(99));
        assert_eq!(SaveFileV2::peek_version(&serde_json::json!({})), None);
        assert_eq!(SaveFileV2::peek_version(&serde_json::json!([1, 2])), None);
        assert_eq!(SaveFileV2::peek_version(&serde_json::json!("x")), None);
    }

    /// 承接写入必须**不越界**改动非归属段（归属不变量）。
    ///
    /// S7-M4 归属变更：段 C 的**因子侧**（`personality` / `personalityRolled` / `adapt` /
    /// `rough`）自 S7-M4 起由内核写（S5-M1 时只是配置默认占位），故本用例的「越界」
    /// 边界相应收缩为：B 段全部 + C 段的 `needs` + D 段。
    #[test]
    fn capture_from_covers_section_a_and_factor_state() {
        let cfg: &'static EmotionConfig = Box::leak(Box::new(EmotionConfig::default()));
        let needs: &'static NeedsConfig = Box::leak(Box::new(NeedsConfig::default()));
        let mut engine = crate::emotion::EmotionEngine::new(cfg, needs);
        engine.neglect.p = 42.0;
        engine.state.values.mood = 33.0;
        engine.state.today = "2026-09-15".to_string();
        engine.state.last_tick_ms = 1_700_000_000_000;

        // 预置 B/C/D 段的「非默认」值，验证 capture 不覆盖它们。
        let mut save = SaveFileV2::default();
        save.pet.name = "X".to_string();
        save.pet.catchphrase.enabled = false;
        save.emotion.personality.temper = 0.9;
        save.emotion.rough.value = 1.5;
        save.needs.scent_buff_until_ms = 777;
        save.settings.performance.renderer = "frame".to_string();
        save.economy = serde_json::json!({ "coin": 1234 });
        save.decor = serde_json::json!([{ "id": "d1" }, null, null, null, null]);
        save.counters.daily_tasks = serde_json::json!([{ "id": "t1", "progress": 2 }]);

        save.capture_from(&engine, 1_700_000_001_000);

        // 段 A 已刷新
        assert_eq!(save.values.mood, 33.0);
        assert_eq!(save.emotion.neglect.p, 42.0);
        assert_eq!(save.meta.last_tick_ms, 1_700_000_000_000);
        assert_eq!(save.meta.today, "2026-09-15");
        assert_eq!(save.meta.last_seen_ms, 1_700_000_001_000);
        assert_eq!(save.v, SAVE_VERSION);
        // 段 C 因子侧：由内核写（S7-M4 起），会覆盖预置值
        let want_temper = crate::config::model::PersonalityDefaultsCfg::default().temper;
        assert_eq!(save.emotion.personality.temper, want_temper, "性格五维归内核写");
        assert!(!save.emotion.personality_rolled, "未随过：标记保持 false");
        assert_eq!(save.emotion.rough.value, 1.0, "粗暴计量归内核写（默认中性）");
        assert_eq!(save.emotion.adapt.samples.len(), 0, "自适应样本归内核写");
        // 段 B 全部 + C 段 needs + D 段：原样
        assert_eq!(save.pet.name, "X");
        assert!(!save.pet.catchphrase.enabled);
        assert_eq!(save.needs.scent_buff_until_ms, 777);
        assert_eq!(save.settings.performance.renderer, "frame");
        assert_eq!(save.economy["coin"], 1234);
        assert_eq!(save.decor[0]["id"], "d1");
        assert_eq!(save.counters.daily_tasks[0]["progress"], 2);
    }

    #[test]
    fn to_pet_state_mirrors_section_a_and_clears_pause() {
        let mut save = SaveFileV2::default();
        save.values.mood = 41.0;
        save.emotion.neglect.level = 3;
        save.emotion.neglect.p = 31.5;
        save.emotion.state = EmotionState::Longing;
        save.meta.last_tick_ms = 1_234_567;
        save.meta.today = "2026-09-15".to_string();
        let state = save.to_pet_state();
        assert_eq!(state.values.mood, 41.0);
        assert_eq!(state.emotion, EmotionState::Longing);
        assert_eq!(state.last_tick_ms, 1_234_567);
        assert_eq!(state.today, "2026-09-15");
        assert!(!state.pause.is_paused(), "暂停窗口不落盘（取默认空窗口）");
        assert_eq!(state.pause.accumulated_ms, 0);
        // P / 敏感度不重复进 PetState（内核字段，由调用方另行从存档取）。
        assert_eq!(save.emotion.neglect.p, 31.5);
        assert_eq!(save.emotion.sensitivity.value, 1.0);
    }
}
