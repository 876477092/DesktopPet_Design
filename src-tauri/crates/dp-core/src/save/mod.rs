//! `save`：存档持久化（`01 FR-8` / `02 §5 K-7`）——v2 Schema、原子写、30s 定时、损坏恢复。
//!
//! ## 分层
//!
//! ```text
//!   save::schema   结构定义（SaveFileV2 / 子段 / 版本常量 / 段归属）     零 IO
//!   save::store    读取降级链 + 原子写 + 定时 + 隔离 + 离线时段推导    只做 std::fs
//!   save::SaveError 类型化错误（thiserror；`02 §7.4`）
//! ```
//!
//! ## 与相邻模块的边界（F-01 归口核实）
//!
//! - **配置**（`config::ConfigService`，S1-M5）：管 `resources/config/*.json` 的**默认值**；
//!   本模块管 `%APPDATA%\DesktopPet\save.json` 的**用户数据**。两者字段可能同名
//!   （如 `emotionSensitivity`），但**不是同一份真源**：配置是只读基线，存档是覆盖值。
//! - **迁移**（v1→v2）：归 **S8-M7**。本模块只提供判定（[`store::LoadStatus::MigrationPending`]）
//!   与 `v1bak` 路径口径（[`store::v1_bak_path`]），**不实现任何字段映射**。
//! - **v2 各段归属**：段 A 由本卡读写（六维 / P / 敏感度 / 展示态 / meta）；
//!   段 B 归 S5-M3/M4/M5（设置 / 命名 / 口头禅）；段 C 归 S7-M2/M4（需求冷却 / 性格/自适应/粗暴）；
//!   段 D 归 S8（经济 / 背包 / 相册 / 装饰 / 技能 / 活动 / 计数）。详见 [`schema::SaveFileV2`]。
//! - **单实例与文件锁**：归 **S5-M4**（`02 §5 K-7` R17）。本模块**无锁**——写者唯一
//!   （core-loop 业务档），多开防护由单实例锁在进程级拒绝。
//!
//! ## 时间纪律（C3）
//!
//! 本模块**零时钟**：墙钟毫秒 `now_ms` 与单调毫秒 `now_mono_ms` 全部由调用方注入。
//!
//! ## 不做的事（显式声明）
//!
//! - 不做存档加密 / 压缩（`01 FR-8` 未要求，且会破坏「用户可手改 JSON」的可观测性）。
//! - 不做 v2→v1 降级迁移（`02 §5 K-7`：新增字段无法映射；回退走 `v1bak` 手工导入）。
//! - 不新增 `pet://` 事件（C8）：损坏提示经返回值 + 日志传达到装配层。

pub mod schema;
pub mod store;

use thiserror::Error;

pub use schema::{
    AdaptSampleSave, AdaptationSave, CatchphraseSave, CountersSave, EmotionSave, NeedsSave,
    PerformanceSave, PetSave, PrivacySave, RoughSave, SaveFileV2, SaveMeta, SettingsSave,
    DECOR_SLOTS, SAVE_FILE, SAVE_TEMPLATE_FILE, SAVE_VERSION, SKILL_KEYS,
};
pub use store::{
    v1_bak_path, LoadOutcome, LoadStatus, SaveStore, MIN_MERGE_MS, SAVE_INTERVAL_MS,
};

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 存档读写错误（`#[non_exhaustive]`，`02 §7.4`：类型化错误 + 不泄露内部细节）。
///
/// 口径：**错误不外泄到用户界面原文**——装配层据 [`LoadOutcome`] 给出可读提示，
/// 本类型的 `Display` 只用于日志。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SaveError {
    /// 文件系统操作失败（创建 / 写入 / fsync / rename / 隔离）。
    #[error("存档 IO 失败（{path}）：{reason}")]
    Io {
        /// 出错路径（可读形式）。
        path: String,
        /// 底层原因（`std::io::Error` 的展示文本）。
        reason: String,
    },
    /// 缓存序列化失败（正常路径不可达；缓存是自产结构，出现即程序缺陷）。
    #[error("存档序列化失败：{0}")]
    Serialize(String),
}

impl SaveError {
    /// 由 `std::io::Error` 构造（统一路径可读化）。
    pub(crate) fn io(path: &std::path::Path, err: std::io::Error) -> Self {
        Self::Io { path: path.display().to_string(), reason: err.to_string() }
    }
}
