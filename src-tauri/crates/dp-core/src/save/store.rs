//! `save::store`：原子写、30s 定时、损坏恢复（`02 §5 K-7`）
//! + 离线补偿时段推导（**S5-M2 离线段**）。
//!
//! ## 落盘策略（`01 FR-8-2` / `02 §5 K-7`）
//!
//! ```text
//!   写临时文件 save.json.tmp → fsync → 复制旧档到 save.json.bak → rename 原子替换
//!   触发：① 30s 定时（SAVE_INTERVAL_MS）；② 有变更且过 2s 合并窗口（MIN_MERGE_MS）；
//!         ③ 关键事件强制（flush_force，跳过定时与窗口）。
//! ```
//!
//! **为什么「定时也写」**：`meta.lastTickMs` 是离线补偿的唯一起点（`02 §5.5`），
//! 若只在数值变化时落盘，一次「挂机 20 分钟后崩溃」会让补偿起点退回到 20 分钟前，
//! 离线压力被重复计入。30s 定时刷新保证起点误差 ≤30s（可忽略）。
//!
//! **为什么 rename 是原子替换**：NTFS 上 `MoveFileEx(REPLACE_EXISTING)` 语义由
//! `std::fs::rename` 承载；进程在任意时刻被杀，`save.json` 要么是旧完整档、要么是
//! 新完整档，**不存在半截文件**（AC-14 的机理）。`save.json.tmp` 即使残留也永不被读取
//! （[`SaveStore::load`] 只认 `save.json` / `save.json.bak`），故不构成污染。
//!
//! ## 读取降级链（`02 §5 K-7`；**迁移不在本卡**）
//!
//! ```text
//!   save.json 存在且 v==2 ────────────────────────→ Loaded
//!   save.json 存在且 v==1 ──→ MigrationPending（**不改文件、禁写盘**；迁移归 S8-M7）
//!   save.json 存在且 v>2  ──→ 隔离 save.future.<ts>.json → 默认档
//!   save.json 解析失败  ────→ 隔离 save.corrupt.<ts>.json → 试 save.json.bak
//!                              ├─ bak 是 v2 → RecoveredFromBak（立即写回主档）
//!                              └─ 否则     → 默认档
//!   save.json 不存在 ───────→ 试 save.json.bak（同上）→ 否则 Fresh 默认档
//! ```
//!
//! **v==1 为何不隔离**：v1 是**合法用户数据**，隔离/覆盖都是数据破坏。本卡只把
//! 状态如实报出（[`LoadStatus::MigrationPending`]）并**关闭写盘**（`writable=false`），
//! 防止 30s 定时把 v1 档覆盖成 v2 默认档；真迁移归 S8-M7。
//!
//! ## 时间纪律（C3）
//!
//! 本模块**零墙钟读取**：`now_ms`（墙钟，用于时刻/命名/补偿）与 `now_mono_ms`
//! （单调，用于 30s 间隔）均由调用方注入。间隔判定是「超时」语义，故允许单调钟。

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use super::schema::{
    SaveFileV2, BAK_SUFFIX, SAVE_FILE, SAVE_VERSION, TMP_SUFFIX, V1_BAK_SUFFIX,
};
use super::SaveError;

// ---------------------------------------------------------------------------
// 定时与窗口常量
// ---------------------------------------------------------------------------

/// 定时落盘间隔（`01 FR-8-2` / `02 §5 K-7`：30s）。
pub const SAVE_INTERVAL_MS: u64 = 30_000;

/// 变更合并窗口（`02 §5 K-7`：`ValueChanged` 类做 2s 合并窗口）。
///
/// 语义：数值变化后**最多等 2s** 即落盘（不会为了等 30s 定时而丢一次变更）。
pub const MIN_MERGE_MS: u64 = 2_000;

// ---------------------------------------------------------------------------
// 读取结果
// ---------------------------------------------------------------------------

/// 存档读取结果状态（`02 §5 K-7` 降级链的**可观测投影**；损坏提示的载荷来源）。
#[derive(Clone, Debug, PartialEq)]
pub enum LoadStatus {
    /// 目录内无存档（全新安装）→ 默认档。
    Fresh,
    /// v2 存档直接加载成功。
    Loaded,
    /// 主档损坏 / 缺失，由 `save.json.bak` 恢复并立即写回主档。
    RecoveredFromBak {
        /// 被隔离的损坏主档路径（若主档本就不存在则为 `None`）。
        isolated: Option<PathBuf>,
    },
    /// 主档损坏且无法恢复 → 隔离损坏档 + 默认档（需提示用户，AC-14）。
    IsolatedCorrupt {
        /// 隔离后的损坏档路径（用户可在设置页「数据」Tab 导入重试）。
        isolated: PathBuf,
    },
    /// 主档版本高于本程序支持的版本 → **先隔离再以默认档重建**（`02 §6.1`）。
    ///
    /// 「不覆盖」的准确含义：**不原地覆写**原档——数据先整体搬到隔离副本
    /// （`save.future.<ts>.json`），主档路径才用默认档重建，故用户数据零丢失。
    IsolatedFuture {
        /// 隔离后的未来版本档路径。
        isolated: PathBuf,
        /// 档中读到的版本号。
        found: u32,
    },
    /// 主档是 v1：**不迁移、不改文件、禁写盘**（迁移归 S8-M7）。
    MigrationPending {
        /// 档中读到的版本号。
        found: u32,
    },
}

/// 存档读取结果（状态 + 告警文本；`dp-app` 据此记日志 / 决定是否提示用户）。
#[derive(Clone, Debug, PartialEq)]
pub struct LoadOutcome {
    /// 降级链落点。
    pub status: LoadStatus,
    /// 过程告警（逐条可读；供装配层日志，空 = 无异常）。
    pub warnings: Vec<String>,
}

impl LoadOutcome {
    /// 无异常（v2 直载 / 全新档）。
    #[inline]
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(self.status, LoadStatus::Fresh | LoadStatus::Loaded)
    }

    /// 是否需要向用户提示（损坏 / 未来版本 / 待迁移）。
    #[inline]
    #[must_use]
    pub fn needs_notice(&self) -> bool {
        !self.is_healthy()
    }

    /// 单行摘要（装配层日志用；含隔离路径，便于用户按路径取回原档）。
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.status {
            LoadStatus::Fresh => "无存档，使用默认档（全新安装）".to_string(),
            LoadStatus::Loaded => "存档 v2 加载成功".to_string(),
            LoadStatus::RecoveredFromBak { isolated } => match isolated {
                Some(p) => format!("主档损坏，已由 save.json.bak 恢复（损坏档已隔离至 {}）", p.display()),
                None => "主档缺失，已由 save.json.bak 恢复".to_string(),
            },
            LoadStatus::IsolatedCorrupt { isolated } => format!(
                "存档损坏且无可用备份，已重建默认档（损坏档已隔离至 {}）",
                isolated.display()
            ),
            LoadStatus::IsolatedFuture { isolated, found } => format!(
                "存档版本 v{found} 高于本程序支持的 v{SAVE_VERSION}，已隔离至 {}（未覆盖）",
                isolated.display()
            ),
            LoadStatus::MigrationPending { found } => format!(
                "存档版本 v{found} 需迁移至 v{SAVE_VERSION}（迁移归 S8-M7；本次不写盘、未改文件）"
            ),
        }
    }
}


// ---------------------------------------------------------------------------
// 探测结果（内部）
// ---------------------------------------------------------------------------

/// 单文件探测结果（内部；`load` 的判定输入）。
enum Probe {
    /// 文件不存在。
    Missing,
    /// v2 且可反序列化。
    V2(Box<SaveFileV2>),
    /// v1（合法数据，待迁移）。
    NeedsMigration { found: u32 },
    /// 版本高于本程序。
    Future { found: u32 },
    /// 读取 / 解析 / 反序列化失败，或缺少 `v` 字段。
    Corrupt { reason: String },
}

/// 探测单个存档文件（**只看版本，不合档**；返回 `Corrupt` 时文件保持原样）。
fn probe(path: &Path) -> Probe {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Probe::Missing,
        Err(err) => return Probe::Corrupt { reason: format!("读取失败：{err}") },
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => return Probe::Corrupt { reason: format!("JSON 解析失败：{err}") },
    };
    match SaveFileV2::peek_version(&value) {
        Some(v) if v == SAVE_VERSION => match serde_json::from_value::<SaveFileV2>(value) {
            Ok(save) => Probe::V2(Box::new(save)),
            Err(err) => Probe::Corrupt { reason: format!("v{SAVE_VERSION} 反序列化失败：{err}") },
        },
        Some(v) if v == 1 => Probe::NeedsMigration { found: v },
        Some(v) if v > SAVE_VERSION => Probe::Future { found: v },
        Some(v) => Probe::Corrupt { reason: format!("未知结构版本 v{v}") },
        None => Probe::Corrupt { reason: "缺少版本字段 v（非本程序生成的档）".to_string() },
    }
}

// ---------------------------------------------------------------------------
// SaveStore
// ---------------------------------------------------------------------------

/// 存档读写门面（唯一写者 = `dp-app` 的 core-loop 业务档）。
///
/// 生命周期：装配期 [`SaveStore::load`] 建立（读取 + 降级），运行期由 core-loop 单线程
/// 调用 [`SaveStore::capture_from`] / [`SaveStore::flush`] / [`SaveStore::flush_force`]。
/// **无内部锁**：单写者语义由调用方保证（与 core-loop「唯一状态写者」口径一致）。
#[derive(Clone, Debug)]
pub struct SaveStore {
    save_path: PathBuf,
    cache: SaveFileV2,
    /// 上次成功落盘的单调毫秒（`None` = 本次运行尚未落盘）。
    last_flush_mono_ms: Option<u64>,
    /// 是否存在未被定时覆盖的变更（2s 合并窗口的触发条件）。
    dirty: bool,
    /// 是否允许写盘（`false`：v1 待迁移，防定时覆盖用户数据）。
    writable: bool,
}

impl SaveStore {
    /// 读取（或降级重建）存档目录内的 `save.json`。
    ///
    /// 返回 `(store, outcome)`；`outcome` 携带降级链落点与告警，**装配层必须记录**
    /// （损坏 / 未来版本 / 待迁移需向用户提示，AC-14）。
    ///
    /// `now_ms` 为墙钟毫秒（隔离文件命名 + `meta.lastSeenMs`；C3 由调用方注入），
    /// 测试可传固定值以获得确定性文件名。
    #[must_use]
    pub fn load(dir: &Path, now_ms: i64) -> (Self, LoadOutcome) {
        let save_path = dir.join(SAVE_FILE);
        let bak_path = save_path.with_extension(BAK_SUFFIX);
        let mut warnings: Vec<String> = Vec::new();

        let (mut store, status) = match probe(&save_path) {
            Probe::V2(cache) => (
                Self::with_cache(save_path, *cache),
                LoadStatus::Loaded,
            ),
            Probe::NeedsMigration { found } => {
                // S8-M7：执行 v1→v2 迁移（v1bak 备份 + 立即写回 v2）。
                match Self::migrate_v1_to_v2(&save_path, now_ms, &mut warnings) {
                    Ok(cache) => {
                        let store = Self::with_cache(save_path, cache);
                        warnings.push(format!(
                            "存档 v{found} 已迁移至 v{SAVE_VERSION}（v1 原档已备份为 v1bak）"
                        ));
                        (store, LoadStatus::Loaded)
                    }
                    Err(err) => {
                        // 迁移失败：降级为禁写盘保护（不破坏 v1 原档）。
                        let mut store = Self::with_cache(save_path, SaveFileV2::default());
                        store.writable = false;
                        warnings.push(format!("v1 迁移失败（{err}）；本次禁写盘以保护原档"));
                        (store, LoadStatus::MigrationPending { found })
                    }
                }
            }
            Probe::Future { found } => {
                let isolated = isolate(&save_path, "future", now_ms, &mut warnings);
                warnings.push(format!("存档版本 v{found} 高于 v{SAVE_VERSION}，已隔离（未覆盖）"));
                match isolated {
                    Some(path) => (
                        Self::with_cache(save_path, SaveFileV2::default()),
                        LoadStatus::IsolatedFuture { isolated: path, found },
                    ),
                    None => (
                        Self::with_cache(save_path, SaveFileV2::default()),
                        LoadStatus::Fresh,
                    ),
                }
            }
            Probe::Corrupt { reason } => {
                warnings.push(format!("主档不可用（{reason}）"));
                warn!("存档主档不可用：{reason}");
                Self::recover(save_path, bak_path, now_ms, true, &mut warnings)
            }
            Probe::Missing => {
                // 主档不存在：全新安装，或用户手工删档。仍试 bak（用户可能只留下了备份）。
                if bak_path.is_file() {
                    Self::recover(save_path, bak_path, now_ms, false, &mut warnings)
                } else {
                    (Self::with_cache(save_path, SaveFileV2::default()), LoadStatus::Fresh)
                }
            }
        };

        // 降级分支需**立即**写回主档（`02 §6.1`：隔离 corrupt/future 后用默认档重建；
        // 与 K-7 迁移衔接的「立即 flush_blocking 写回 v2」同款语义）——
        // 不带着「主档缺失/损坏」的状态继续跑 30s，否则一次崩溃就白恢复。
        // `MigrationPending` 是唯一例外：原档是**合法**数据，禁写盘保护（见模块文档）。
        if matches!(
            status,
            LoadStatus::RecoveredFromBak { .. }
                | LoadStatus::IsolatedCorrupt { .. }
                | LoadStatus::IsolatedFuture { .. }
        ) {
            if let Err(err) = store.flush_force(0) {
                warnings.push(format!("默认档写回失败（{err}）；本次运行仅内存生效"));
            }
        }

        if store.writable {
            let summary = LoadOutcome { status: status.clone(), warnings: Vec::new() }.describe();
            info!("存档装载：{summary}（路径 {}）", store.save_path.display());
        }
        (store, LoadOutcome { status, warnings })
    }

    /// 主档不可用时的恢复：隔离损坏主档 → 试 bak → 重建。
    fn recover(
        save_path: PathBuf,
        bak_path: PathBuf,
        now_ms: i64,
        isolate_main: bool,
        warnings: &mut Vec<String>,
    ) -> (Self, LoadStatus) {
        let isolated = if isolate_main { isolate(&save_path, "corrupt", now_ms, warnings) } else { None };
        match probe(&bak_path) {
            Probe::V2(cache) => {
                warnings.push("已由 save.json.bak 恢复".to_string());
                info!("存档由 save.json.bak 恢复");
                (
                    Self::with_cache(save_path, *cache),
                    LoadStatus::RecoveredFromBak { isolated },
                )
            }
            Probe::Missing => {
                warnings.push("无可用备份（save.json.bak 不存在），重建默认档".to_string());
                let status = match isolated {
                    Some(path) => LoadStatus::IsolatedCorrupt { isolated: path },
                    None => LoadStatus::Fresh,
                };
                (Self::with_cache(save_path, SaveFileV2::default()), status)
            }
            Probe::Corrupt { reason } => {
                warnings.push(format!("备份亦不可用（{reason}），重建默认档"));
                let status = match isolated {
                    Some(path) => LoadStatus::IsolatedCorrupt { isolated: path },
                    None => LoadStatus::Fresh,
                };
                (Self::with_cache(save_path, SaveFileV2::default()), status)
            }
            Probe::NeedsMigration { found } => {
                warnings.push(format!(
                    "仅存 v{found} 备份，需迁移至 v{SAVE_VERSION}（归 S8-M7）；本次用默认档且禁写盘"
                ));
                let mut store = Self::with_cache(save_path, SaveFileV2::default());
                store.writable = false;
                (store, LoadStatus::MigrationPending { found })
            }
            Probe::Future { found } => {
                let bak_isolated = isolate(&bak_path, "future", now_ms, warnings);
                warnings.push(format!("备份版本 v{found} 高于 v{SAVE_VERSION}，已隔离"));
                let status = match isolated.or(bak_isolated) {
                    Some(path) => LoadStatus::IsolatedFuture { isolated: path, found },
                    None => LoadStatus::Fresh,
                };
                (Self::with_cache(save_path, SaveFileV2::default()), status)
            }
        }
    }

    fn with_cache(save_path: PathBuf, cache: SaveFileV2) -> Self {
        Self { save_path, cache, last_flush_mono_ms: None, dirty: false, writable: true }
    }

    /// S8-M7：v1→v2 迁移。
    ///
    /// 步骤（`02 §5 K-7`）：
    ///   1. 读 v1 JSON；
    ///   2. **首次**复制 `save.json → save.json.v1bak`（已存在则不覆盖，永不覆盖）；
    ///   3. [`crate::save::migrate::migrate`] 做字段映射 → v2；
    ///   4. 立即原子写回 v2（迁移后即可写盘，`writable=true`）。
    fn migrate_v1_to_v2(
        save_path: &Path,
        now_ms: i64,
        warnings: &mut Vec<String>,
    ) -> Result<SaveFileV2, String> {
        let text = fs::read_to_string(save_path).map_err(|e| format!("读取 v1 档失败：{e}"))?;
        let v1: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("v1 JSON 解析失败：{e}"))?;

        // 备份 v1bak（仅首次，永不覆盖）。
        let bak = v1_bak_path(save_path);
        if !bak.exists() {
            if let Err(e) = fs::copy(save_path, &bak) {
                warnings.push(format!("v1bak 备份失败（不阻断迁移）：{e}"));
            }
        }

        let cache = crate::save::migrate::migrate(&v1);

        // 立即原子写回 v2（与降级分支同语义：不带着旧档状态跑 30s）。
        let mut store = Self::with_cache(save_path.to_path_buf(), cache);
        store.writable = true;
        store.flush_force(now_ms as u64).map_err(|e| format!("v2 写回失败：{e}"))?;
        Ok(store.cache)
    }

    /// 只读：存档文件路径。
    #[inline]
    #[must_use]
    pub fn save_path(&self) -> &Path {
        &self.save_path
    }

    /// 只读：当前内存缓存（脏数据也在此，落盘前不丢失）。
    #[inline]
    #[must_use]
    pub fn cache(&self) -> &SaveFileV2 {
        &self.cache
    }

    /// 可写：缓存（供各归口模块写入自己那一段：B/C/D）。
    #[inline]
    pub fn cache_mut(&mut self) -> &mut SaveFileV2 {
        &mut self.cache
    }

    /// 是否允许写盘（`false`：v1 待迁移，保护原档不被覆盖）。
    #[inline]
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// 是否存在待落盘变更。
    #[inline]
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 是否到点落盘（`01 FR-8-2` / `02 §5 K-7`）。
    ///
    /// 判定：① 本次运行尚未落盘 → 到点（首拍建立锚点）；
    /// ② 距上次落盘 ≥ [`SAVE_INTERVAL_MS`] → 到点（定时）；
    /// ③ 有变更且距上次 ≥ [`MIN_MERGE_MS`] → 到点（变更即写，2s 合并窗口）。
    #[must_use]
    pub fn due(&self, now_mono_ms: u64) -> bool {
        match self.last_flush_mono_ms {
            None => true,
            Some(last) => {
                let elapsed = now_mono_ms.saturating_sub(last);
                elapsed >= SAVE_INTERVAL_MS || (self.dirty && elapsed >= MIN_MERGE_MS)
            }
        }
    }

    /// 按策略落盘（未到点 / 禁写盘 → `Ok(false)`，不报错）。
    pub fn flush(&mut self, now_mono_ms: u64) -> Result<bool, SaveError> {
        if !self.writable || !self.due(now_mono_ms) {
            return Ok(false);
        }
        self.write_now(now_mono_ms)?;
        Ok(true)
    }

    /// 强制落盘（关键事件 / 退出：跳过定时与合并窗口）。
    pub fn flush_force(&mut self, now_mono_ms: u64) -> Result<bool, SaveError> {
        if !self.writable {
            return Ok(false);
        }
        self.write_now(now_mono_ms)?;
        Ok(true)
    }

    /// 重置为内置默认档并**立即**落盘（`01 FR-8-4`「重置全部数据 → 清档重建」）。
    ///
    /// 语义与边界（S5-M4）：
    /// - 内存缓存整体替换为 [`SaveFileV2::default()`]（与 `default_save.json` 模板逐位一致，
    ///   由 `save::schema` 单测锁定），**并**立即同步落盘——先落盘再返回，避免调用方
    ///   在重启前丢掉重置结果；
    /// - 标注为可写（`writable = true`）：用户显式重置即视为「接受以本档为准」，
    ///   即便原档是 v1 待迁移档（重置是用户意志，不是后台定时覆盖）；
    /// - **只替换存档**，不负责重建内存中的内核状态——调用方（core-loop）需自行决定
    ///   是否重启进程；本卡（S5-M4）走 `app.restart()`，使 B/C/D 段全部回到出厂态；
    /// - 返回是否真的写盘（禁写盘 / IO 失败时 `Ok(false)`，不报错，调用方记日志）。
    pub fn reset_to_default(&mut self, now_mono_ms: u64) -> Result<bool, SaveError> {
        self.cache = SaveFileV2::default();
        self.writable = true;
        self.dirty = true;
        self.write_now(now_mono_ms)?;
        Ok(true)
    }

    /// 用磁盘上的某个备份文件**覆盖**主档（`02 §5 K-7`：设置页「数据」Tab 导入存档）。
    ///
    /// 边界（S5-M4）：
    /// - `source` 必须是调用方从**存档目录内**解析出的备份路径（导入命令只接受目录白名单
    ///   中的文件名，不接受任意路径——防路径穿越）；
    /// - 本方法只做「复制到主档 + 校验可解析」；**不重建内存状态**，调用方随后需重启
    ///   （本卡走 `app.restart()`），保证导入的档成为唯一真源；
    /// - 复制前先复制一份 `save.json.bak`（尽力而为），保证「导入错了还能退回来」。
    pub fn import_from(&mut self, source: &Path, now_mono_ms: u64) -> Result<(), SaveError> {
        let text = std::fs::read_to_string(source).map_err(|err| SaveError::io(source, err))?;
        // 先校验可解析：导入一个解析不了的档只会把用户推进「损坏 → 重建默认档」分支。
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|err| SaveError::Serialize(format!("备份档不是合法 JSON：{err}")))?;
        let backup: SaveFileV2 = serde_json::from_value(value).map_err(|err| {
            SaveError::Serialize(format!("备份档结构不匹配（v{SAVE_VERSION}）:{err}"))
        })?;
        if backup.v != SAVE_VERSION {
            return Err(SaveError::Serialize(format!(
                "备份档版本为 v{}，仅支持 v{SAVE_VERSION}",
                backup.v
            )));
        }
        // 导入前把当前主档留一份（尽力而为，不阻断）。
        if self.save_path.is_file() {
            let _ = std::fs::copy(&self.save_path, self.save_path.with_extension(BAK_SUFFIX));
        }
        self.cache = backup;
        self.writable = true;
        self.dirty = true;
        self.write_now(now_mono_ms)?;
        Ok(())
    }

    /// 用内核当前状态刷新存档「段 A」，返回是否发生**语义变化**。
    ///
    /// 返回值即 [`Self::is_dirty`] 的置位依据：`meta.lastSeenMs` 这类记账字段的
    /// 逐秒变化**不算**变更（否则会退化成 2s 一次的无限落盘）。
    pub fn capture_from(
        &mut self,
        engine: &crate::emotion::EmotionEngine<'_>,
        now_ms: i64,
    ) -> bool {
        let changed = self.cache.values != engine.state.values
            || self.cache.emotion.neglect != engine.neglect
            || self.cache.emotion.sensitivity != engine.sensitivity
            || self.cache.emotion.state != engine.state.emotion
            || self.cache.meta.today != engine.state.today;
        self.cache.capture_from(engine, now_ms);
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// 离线时长（毫秒）：`now_ms − meta.lastTickMs`，**钳到非负**（`02 §5.5`；S5-M2 离线段）。
    ///
    /// 墙钟回拨（DST / 手工改表 / 同步校正）→ 返回 `0`，退化为「未离线」，
    /// 绝不产生负时长让 P 反向扣减。
    ///
    /// ⚠️ **调用前必须先查 [`Self::has_session_anchor`]**：`lastTickMs == 0` 时本函数
    /// 会算出「距 1970 纪元」的巨量时长（技术上正确，语义上无意义）。
    #[inline]
    #[must_use]
    pub fn away_ms(&self, now_ms: i64) -> i64 {
        (now_ms - self.cache.meta.last_tick_ms).max(0)
    }

    /// 是否存在可用的「上次 tick」锚点（`meta.lastTickMs > 0`）。
    ///
    /// `lastTickMs == 0` 表示该档**从未 tick 过**（全新安装；或上一次运行在首个业务档
    /// 落盘之前就退出了）。此时**不得**把「距 1970 纪元」当离线时长——否则首次启动会
    /// 直接补偿到封顶档（L4），宠物「一装上就很生气」。
    ///
    /// 消费者：`dp-app` 的 `CoreLoopState::new`（是否 `EmotionEngine::restore`）与
    /// `compensate_offline`（是否补偿）。
    #[inline]
    #[must_use]
    pub fn has_session_anchor(&self) -> bool {
        self.cache.meta.last_tick_ms > 0
    }

    /// 只读：最近一次落盘记录的「最后见面」墙钟毫秒（`01 FR-8-1`）。
    #[inline]
    #[must_use]
    pub fn last_seen_ms(&self) -> i64 {
        self.cache.meta.last_seen_ms
    }

    fn write_now(&mut self, now_mono_ms: u64) -> Result<(), SaveError> {
        let bytes = serde_json::to_vec_pretty(&self.cache)
            .map_err(|err| SaveError::Serialize(err.to_string()))?;
        write_atomic(&self.save_path, &bytes)?;
        self.last_flush_mono_ms = Some(now_mono_ms);
        self.dirty = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 原子写与隔离
// ---------------------------------------------------------------------------

/// 原子写（`02 §5 K-7` 伪码的实装）：
/// 临时文件 → `fsync` → 复制旧档为 `.bak` → `rename` 原子替换。
///
/// 任一步报错即返回 `Err`，**不触碰**原档（`rename` 之前的失败都不会破坏 `save.json`）。
fn write_atomic(save_path: &Path, json: &[u8]) -> Result<(), SaveError> {
    if let Some(parent) = save_path.parent() {
        fs::create_dir_all(parent).map_err(|err| SaveError::io(parent, err))?;
    }
    let tmp = save_path.with_extension(TMP_SUFFIX);
    {
        let mut file = File::create(&tmp).map_err(|err| SaveError::io(&tmp, err))?;
        file.write_all(json).map_err(|err| SaveError::io(&tmp, err))?;
        // fsync：保证 rename 之后读到的内容已落盘（否则断电可能留下「新名字 + 空内容」）。
        file.sync_all().map_err(|err| SaveError::io(&tmp, err))?;
    }
    // 备份复制是**尽力而为**（`02 §5 K-7` 伪码即 `let _ =`）：备份失败不应阻断原子写，
    // 否则一次磁盘配额问题会让存档彻底停止更新。
    if save_path.exists() {
        let bak = save_path.with_extension(BAK_SUFFIX);
        if let Err(err) = fs::copy(save_path, &bak) {
            warn!("存档备份复制失败（不阻断原子写）：{err}");
        }
    }
    fs::rename(&tmp, save_path).map_err(|err| SaveError::io(save_path, err))?;
    Ok(())
}

/// 把不可用档移到隔离路径（`save.<tag>.<ts>.json`），成功返回隔离后路径。
///
/// 隔离失败（如被其它进程占用）不阻断恢复流程：记告警并继续用默认档
/// （[`Self::recover`] 的 `isolate_main` 语义）。
fn isolate(path: &Path, tag: &str, now_ms: i64, warnings: &mut Vec<String>) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    let target = unique_isolate_path(path, tag, now_ms);
    match fs::rename(path, &target) {
        Ok(()) => {
            warn!("存档已隔离：{} → {}", path.display(), target.display());
            Some(target)
        }
        Err(err) => {
            let msg = format!("隔离 {} 失败（{err}），保留原文件", path.display());
            warn!("{msg}");
            warnings.push(msg);
            None
        }
    }
}

/// 计算不与既有文件冲突的隔离路径（同一毫秒内多次隔离时追加序号）。
fn unique_isolate_path(path: &Path, tag: &str, now_ms: i64) -> PathBuf {
    let base = path.with_extension(format!("{tag}.{now_ms}.json"));
    if !base.exists() {
        return base;
    }
    for n in 1..=999u32 {
        let candidate = path.with_extension(format!("{tag}.{now_ms}.{n}.json"));
        if !candidate.exists() {
            return candidate;
        }
    }
    base
}

/// v1 备份路径（`save.json.v1bak`；`02 §5 K-7` F「仅首次，永不覆盖」）。
///
/// **写入动作归 S8-M7**（迁移），本模块只提供口径函数，避免路径拼接在两处漂移。
#[must_use]
pub fn v1_bak_path(save_path: &Path) -> PathBuf {
    save_path.with_extension(format!("json.{V1_BAK_SUFFIX}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{EmotionConfig, NeedsConfig};
    use crate::emotion::engine::EmotionEngine;
    use crate::emotion::lines::CatchphraseFrequency;

    const T0: i64 = 1_700_000_000_000;
    /// 单调毫秒基准（`flush_*` / `reset_*` / `import_*` 的 `now_mono_ms`；与 `T0` 不同口径）。
    const M0: u64 = 1_000;

    /// 每例独立临时目录（名字用测试名，C3：不依赖时钟）。
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("dp-core-s5m1-tests").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("创建临时目录失败（测试前置）");
        dir
    }

    fn fixtures_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
    }

    fn engine() -> EmotionEngine<'static> {
        let cfg: &'static EmotionConfig = Box::leak(Box::new(EmotionConfig::default()));
        let needs: &'static NeedsConfig = Box::leak(Box::new(NeedsConfig::default()));
        EmotionEngine::new(cfg, needs)
    }

    fn read_json(path: &Path) -> serde_json::Value {
        let text = fs::read_to_string(path).expect("读取失败（测试前置）");
        serde_json::from_str(&text).expect("应为合法 JSON")
    }

    // -- 全新安装与原子写 -----------------------------------------------------

    #[test]
    fn fresh_dir_yields_default_and_first_flush_creates_v2_file() {
        let dir = temp_dir("fresh");
        let (mut store, outcome) = SaveStore::load(&dir, T0);
        assert_eq!(outcome.status, LoadStatus::Fresh);
        assert!(outcome.is_healthy());
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        assert_eq!(store.cache().v, SAVE_VERSION);
        assert!(!dir.join(SAVE_FILE).exists(), "load 不落盘（首拍才写）");

        assert!(store.due(0), "本次运行尚未落盘 → 到点");
        assert!(store.flush(0).expect("落盘应成功"));
        let path = dir.join(SAVE_FILE);
        assert!(path.is_file());
        assert_eq!(SaveFileV2::peek_version(&read_json(&path)), Some(SAVE_VERSION));
        assert!(!dir.join("save.json.tmp").exists(), "原子写不留临时文件");
        assert!(!dir.join("save.json.bak").exists(), "首写无旧档可备份");
        assert!(!store.is_dirty());
    }

    #[test]
    fn atomic_write_keeps_backup_of_previous_content() {
        let dir = temp_dir("backup");
        let (mut store, _) = SaveStore::load(&dir, T0);
        store.flush_force(0).expect("首写");
        let first = read_json(&dir.join(SAVE_FILE));

        store.cache_mut().values.mood = 7.0;
        store.flush_force(1).expect("二写");

        let bak = dir.join("save.json.bak");
        assert!(bak.is_file(), "第二次落盘应留下旧档备份");
        assert_eq!(read_json(&bak)["values"]["mood"], first["values"]["mood"]);
        assert_eq!(read_json(&dir.join(SAVE_FILE))["values"]["mood"], 7.0);
    }

    /// AC-14 机理：进程在「写完 tmp、尚未 rename」时被杀 → 主档仍是旧的完整档。
    #[test]
    fn crash_before_rename_leaves_previous_save_intact() {
        let dir = temp_dir("crash-tmp");
        let (mut store, _) = SaveStore::load(&dir, T0);
        store.cache_mut().values.energy = 42.0;
        store.flush_force(0).expect("写一份完好档");

        // 模拟崩溃现场：临时文件半截、未 rename。
        fs::write(dir.join("save.json.tmp"), "{ \"v\": 2, \"values\": { \"mo")
            .expect("写半截 tmp 失败（测试前置）");

        let (reloaded, outcome) = SaveStore::load(&dir, T0 + 1);
        assert_eq!(outcome.status, LoadStatus::Loaded, "主档必须仍然可读");
        assert_eq!(reloaded.cache().values.energy, 42.0, "主档内容未被半截 tmp 污染");
    }

    /// AC-14 机理（另一侧）：崩溃发生在「已 rename」之后 → 主档是新完整档。
    #[test]
    fn repeated_flush_never_yields_torn_file() {
        let dir = temp_dir("torn");
        let (mut store, _) = SaveStore::load(&dir, T0);
        for i in 0..50i32 {
            store.cache_mut().values.mood = i as f32;
            store.flush_force(i as u64).expect("落盘应成功");
            // 每一次落盘之后，磁盘上的主档都必须是一份完整可解析的 v2 档。
            let value = read_json(&dir.join(SAVE_FILE));
            assert_eq!(SaveFileV2::peek_version(&value), Some(SAVE_VERSION), "第 {i} 次落盘后主档不完整");
        }
        assert!(!dir.join("save.json.tmp").exists());
    }

    // -- 损坏恢复 --------------------------------------------------------------

    #[test]
    fn corrupt_save_is_isolated_and_recovered_from_bak() {
        let dir = temp_dir("corrupt-with-bak");
        let (mut store, _) = SaveStore::load(&dir, T0);
        store.cache_mut().values.mood = 33.0;
        store.flush_force(0).expect("首写");
        store.cache_mut().values.mood = 66.0;
        store.flush_force(1).expect("二写（此时 bak = mood 33 的那份）");

        fs::write(dir.join(SAVE_FILE), "{ 这不是 JSON ").expect("写坏主档失败（测试前置）");
        let (recovered, outcome) = SaveStore::load(&dir, T0 + 5);
        assert!(
            matches!(outcome.status, LoadStatus::RecoveredFromBak { isolated: Some(_) }),
            "应报「由备份恢复且损坏档已隔离」：{:?}",
            outcome.status
        );
        // bak 是「二写」时复制的那份，即 mood=33 的首写内容。
        assert_eq!(recovered.cache().values.mood, 33.0);
        assert!(recovered.is_writable());
        // 主档已被恢复内容重建。
        assert_eq!(read_json(&dir.join(SAVE_FILE))["values"]["mood"], 33.0);
        // 隔离文件带着损坏原文，用户可回捞。
        let isolated = match &outcome.status {
            LoadStatus::RecoveredFromBak { isolated: Some(p) } => p.clone(),
            other => panic!("期望隔离路径，实际 {other:?}"),
        };
        assert!(isolated.is_file());
        assert!(fs::read_to_string(&isolated).unwrap().contains("这不是 JSON"));
        // 隔离文件命名口径：save.corrupt.<ts>.json
        assert!(
            isolated.file_name().unwrap().to_string_lossy().starts_with("save.corrupt."),
            "隔离命名：{}",
            isolated.display()
        );
        assert!(outcome.needs_notice());
    }

    #[test]
    fn corrupt_save_without_bak_rebuilds_default_and_isolates() {
        let dir = temp_dir("corrupt-no-bak");
        fs::write(dir.join(SAVE_FILE), "not json at all").expect("写坏主档失败（测试前置）");
        let (store, outcome) = SaveStore::load(&dir, T0);
        assert!(
            matches!(outcome.status, LoadStatus::IsolatedCorrupt { .. }),
            "{:?}",
            outcome.status
        );
        assert_eq!(store.cache(), &SaveFileV2::default(), "应重建默认档");
        assert!(store.is_writable(), "默认档可继续写入");
        assert_eq!(
            SaveFileV2::peek_version(&read_json(&dir.join(SAVE_FILE))),
            Some(SAVE_VERSION),
            "默认档已写回主档"
        );
        assert!(outcome.warnings.iter().any(|w| w.contains("无可用备份")), "{:?}", outcome.warnings);
    }

    #[test]
    fn save_without_version_field_is_treated_as_corrupt() {
        let dir = temp_dir("no-version");
        fs::write(dir.join(SAVE_FILE), serde_json::json!({ "values": {} }).to_string())
            .expect("写档失败（测试前置）");
        let (_, outcome) = SaveStore::load(&dir, T0);
        assert!(matches!(outcome.status, LoadStatus::IsolatedCorrupt { .. }), "{:?}", outcome.status);
        assert!(outcome.warnings.iter().any(|w| w.contains("缺少版本字段")), "{:?}", outcome.warnings);
    }

    #[test]
    fn future_version_is_isolated_and_original_content_recoverable() {
        let dir = temp_dir("future");
        let payload = serde_json::json!({ "v": 9, "values": { "mood": 1 }, "unknown": true });
        fs::write(dir.join(SAVE_FILE), serde_json::to_string(&payload).unwrap())
            .expect("写未来档失败（测试前置）");

        let (store, outcome) = SaveStore::load(&dir, T0);
        match &outcome.status {
            LoadStatus::IsolatedFuture { isolated, found } => {
                assert_eq!(*found, 9);
                assert_eq!(read_json(isolated), payload, "原档内容必须逐位保留");
            }
            other => panic!("期望 IsolatedFuture，实际 {other:?}"),
        }
        assert_eq!(store.cache(), &SaveFileV2::default());
        // `02 §6.1`：隔离未来版本档后用默认档重建主档（隔离副本保留原档，未丢数据）。
        // 比较口径 = **解析成结构再比**（`save` 里的 f32 经最短表示文本往返是精确的，
        // 而直接比 `Value` 会把 f32→f64 的二进制尾数差异误判成不等）。
        let rebuilt: SaveFileV2 =
            serde_json::from_value(read_json(&dir.join(SAVE_FILE))).expect("重建档应可解析");
        assert_eq!(rebuilt, SaveFileV2::default());
    }

    /// S8-M7：v1 档启动即迁移——新字段取默认、v1bak 生成、立即写回 v2、可继续写盘。
    #[test]
    fn v1_save_is_migrated_with_v1bak_and_writable() {
        let dir = temp_dir("v1-migrate");
        let v1 = serde_json::json!({ "v": 1, "boredom": 40.0, "coldLevel": 2, "coin": 500 });
        let text = serde_json::to_string_pretty(&v1).unwrap();
        fs::write(dir.join(SAVE_FILE), &text).expect("写 v1 档失败（测试前置）");

        let (store, outcome) = SaveStore::load(&dir, T0);
        assert_eq!(outcome.status, LoadStatus::Loaded, "迁移完成后应视为已装载");
        assert!(store.is_writable(), "迁移后必须可写盘");

        // v1bak 已生成且内容与 v1 逐字一致。
        let bak = dir.join("save.json.v1bak");
        assert!(bak.exists(), "迁移必须生成 v1bak（K-7 F）");
        assert_eq!(fs::read_to_string(&bak).unwrap(), text, "v1bak 应逐字保留 v1");

        // 主档已升级为 v2，且 P 由 boredom 反推（40×1.2=48）、coldLevel→level。
        let main = read_json(&dir.join(SAVE_FILE));
        assert_eq!(main["v"], 2);
        assert_eq!(main["emotion"]["neglect"]["p"], 48.0);
        assert_eq!(main["emotion"]["neglect"]["level"], 2);
        // 经济：旧币 500 + Lv1 补偿 20 = 520，迁移流水幂等 refId。
        assert_eq!(main["economy"]["coin"], 520);
        assert_eq!(main["economy"]["ledger"][0]["refId"], "migration:v1tov2");
    }

    /// S8-M7：v1bak 仅首次写入，永不覆盖（二次迁移 / 重跑不冲掉旧备份）。
    #[test]
    fn v1bak_is_never_overwritten() {
        let dir = temp_dir("v1-bak-once");
        fs::write(dir.join(SAVE_FILE), r#"{ "v": 1, "boredom": 10.0, "coldLevel": 0, "coin": 100 }"#)
            .expect("写 v1");
        let (store1, _) = SaveStore::load(&dir, T0);
        assert!(store1.is_writable());
        let first_bak = fs::read_to_string(dir.join("save.json.v1bak")).unwrap();

        // 改主档内容（模拟用户继续玩），再把主档换回 v1 不应冲掉首次 v1bak。
        fs::write(dir.join(SAVE_FILE), r#"{ "v": 1, "boredom": 99.0, "coldLevel": 3, "coin": 999 }"#)
            .expect("重写 v1");
        let (_store2, _) = SaveStore::load(&dir, T0);
        let bak_now = fs::read_to_string(dir.join("save.json.v1bak")).unwrap();
        assert_eq!(first_bak, bak_now, "v1bak 永不覆盖");
        assert!(bak_now.contains("\"coin\": 100"));
    }

    #[test]
    fn missing_save_but_present_bak_recovers() {
        let dir = temp_dir("missing-main-with-bak");
        let (mut store, _) = SaveStore::load(&dir, T0);
        store.cache_mut().values.mood = 12.0;
        store.flush_force(0).expect("首写");
        store.cache_mut().values.mood = 24.0;
        store.flush_force(1).expect("二写（产生 bak）");
        fs::remove_file(dir.join(SAVE_FILE)).expect("删主档失败（测试前置）");

        let (store, outcome) = SaveStore::load(&dir, T0 + 5);
        assert!(
            matches!(outcome.status, LoadStatus::RecoveredFromBak { isolated: None }),
            "{:?}",
            outcome.status
        );
        assert_eq!(store.cache().values.mood, 12.0, "从 bak 恢复（二写时备份的首写内容）");
    }

    // -- 落盘节奏 --------------------------------------------------------------

    #[test]
    fn due_honours_interval_and_merge_window() {
        let dir = temp_dir("due");
        let (mut store, _) = SaveStore::load(&dir, T0);
        assert!(store.due(1_000), "首拍到点（建立落盘锚点）");
        store.flush(1_000).expect("首写");
        assert!(!store.due(1_000), "刚写完不到点");
        assert!(!store.due(30_999), "30s 定时未到且无变更 → 不到点");
        assert!(store.due(31_000), "满 30s 定时 → 到点");

        // 有变更 → 2s 合并窗口后到点（无需等满 30s）
        let mut changed = engine();
        changed.state.values.mood = 10.0;
        assert!(store.capture_from(&changed, 1_500), "数值变化应置脏");
        assert!(!store.due(1_500), "变更后瞬间仍在合并窗口内 → 不到点");
        assert!(!store.due(2_999), "窗口未满 2s → 不到点");
        assert!(store.due(3_000), "满 2s 合并窗口 → 到点");
    }

    /// `meta.lastSeenMs` 逐秒变化**不得**造成无限落盘（否则退化为 2s 一次）。
    #[test]
    fn capture_without_semantic_change_does_not_dirty() {
        let dir = temp_dir("capture-idempotent");
        let (mut store, _) = SaveStore::load(&dir, T0);
        let mut engine = engine();
        // 默认档 = 「刚装好、未 tick」的内核状态 → 首次 capture 应当**无变化**。
        // 这条断言同时锁定「模板与内核初值一致」这一不变量（模板漂移会在此暴露）。
        assert!(!store.capture_from(&engine, T0), "默认档与全新内核一致，不应判为变更");
        assert!(!store.is_dirty());
        store.flush(0).expect("落盘建立锚点");
        assert!(!store.is_dirty());

        for k in 1..=10i64 {
            assert!(!store.capture_from(&engine, T0 + k * 1_000), "语义未变（仅 lastSeenMs 走字）");
        }
        assert!(!store.is_dirty(), "lastSeenMs 走字不得置脏（否则 2s 一次无限落盘）");

        // 数值真变了才置脏
        engine.state.values.mood -= 1.0;
        assert!(store.capture_from(&engine, T0 + 11_000));
        assert!(store.is_dirty());
    }

    // -- 段 B/C/D 往返（归属不变量） -------------------------------------------

    #[test]
    fn roundtrip_preserves_other_sections() {
        let dir = temp_dir("roundtrip");
        let (mut store, _) = SaveStore::load(&dir, T0);
        {
            let cache = store.cache_mut();
            cache.pet.name = "测试名".to_string();
            cache.pet.catchphrase.enabled = false;
            cache.pet.catchphrase.frequency = CatchphraseFrequency::High;
            cache.needs.bath_free_cd_until_ms = 111;
            cache.settings.performance.renderer = "frame".to_string();
            cache.settings.privacy.activity_sensing = false;
            cache.economy = serde_json::json!({ "coin": 88, "ledger": [{ "seq": 1 }] });
            cache.skills = serde_json::json!({ "cooking": { "level": 3, "points": 5 } });
            cache.album = serde_json::json!([{ "id": "a1" }]);
            cache.activity_counter = serde_json::json!({ "todayJobCount": 2, "dayKey": "2026-09-15" });
        }
        store.flush_force(0).expect("落盘");

        let (reloaded, outcome) = SaveStore::load(&dir, T0 + 1);
        assert_eq!(outcome.status, LoadStatus::Loaded);
        let c = reloaded.cache();
        assert_eq!(c.pet.name, "测试名");
        assert!(!c.pet.catchphrase.enabled);
        assert_eq!(c.pet.catchphrase.frequency, CatchphraseFrequency::High);
        assert_eq!(c.needs.bath_free_cd_until_ms, 111);
        assert_eq!(c.settings.performance.renderer, "frame");
        assert!(!c.settings.privacy.activity_sensing);
        assert_eq!(c.economy["coin"], 88);
        assert_eq!(c.economy["ledger"][0]["seq"], 1);
        assert_eq!(c.skills["cooking"]["level"], 3);
        assert_eq!(c.album[0]["id"], "a1");
        assert_eq!(c.activity_counter["todayJobCount"], 2);
    }

    // -- 离线段（S5-M2） -------------------------------------------------------

    #[test]
    fn away_ms_uses_last_tick_and_clamps_clock_rollback() {
        let dir = temp_dir("away-ms");
        let (mut store, _) = SaveStore::load(&dir, T0);
        store.cache_mut().meta.last_tick_ms = T0;
        assert_eq!(store.away_ms(T0), 0);
        assert_eq!(store.away_ms(T0 + 3 * 3_600_000), 3 * 3_600_000, "3h");
        assert_eq!(store.away_ms(T0 + 24 * 3_600_000), 24 * 3_600_000, "24h");
        // 墙钟回拨 → 0（绝不产生负时长）
        assert_eq!(store.away_ms(T0 - 5_000_000), 0);
        // lastTickMs 为 0（未 tick 过的档）→ 与 1970 纪元比较，仍非负
        store.cache_mut().meta.last_tick_ms = 0;
        assert!(store.away_ms(T0) > 0);
        assert_eq!(store.away_ms(-1), 0);
    }

    /// 锚点判定：`lastTickMs == 0` = 「从未 tick」（全新安装），消费方必须据此跳过补偿。
    #[test]
    fn session_anchor_is_false_until_first_tick_is_recorded() {
        let dir = temp_dir("anchor");
        let (mut store, _) = SaveStore::load(&dir, T0);
        assert!(!store.has_session_anchor(), "全新默认档没有 tick 锚点");
        let mut engine = engine();
        engine.state.last_tick_ms = T0;
        store.capture_from(&engine, T0);
        assert!(store.has_session_anchor(), "落盘一次后应具备锚点");
        assert_eq!(store.away_ms(T0), 0);

        store.cache_mut().meta.last_tick_ms = 0;
        assert!(!store.has_session_anchor());
    }

    #[test]
    fn capture_records_last_tick_for_offline_compensation() {
        let dir = temp_dir("capture-meta");
        let (mut store, _) = SaveStore::load(&dir, T0);
        let mut engine = engine();
        engine.state.last_tick_ms = T0 - 60_000;
        engine.state.today = "2026-09-15".to_string();
        assert!(store.capture_from(&engine, T0));
        store.flush_force(0).expect("落盘");
        assert_eq!(store.cache().meta.last_tick_ms, T0 - 60_000);
        assert_eq!(store.last_seen_ms(), T0);

        let (reloaded, _) = SaveStore::load(&dir, T0 + 1);
        assert_eq!(reloaded.away_ms(T0), 60_000, "重启后离线上界可复算");
    }

    // -- fixtures --------------------------------------------------------------

    /// 夹具 `save_v2.json`：一份合法 v2 档，可直载（供 S5-M2 / Gate 复用）。
    #[test]
    fn fixture_save_v2_loads_as_current_version() {
        let dir = temp_dir("fixture-v2");
        let src = fixtures_dir().join("save_v2.json");
        assert!(src.is_file(), "缺少夹具：{}", src.display());
        fs::copy(&src, dir.join(SAVE_FILE)).expect("复制夹具失败（测试前置）");

        let (store, outcome) = SaveStore::load(&dir, T0);
        assert_eq!(outcome.status, LoadStatus::Loaded, "{:?}", outcome.warnings);
        assert!(store.cache().is_current());
        // 夹具刻意带上「非默认」的段 A 值与段 B/C/D 值，保证「真解析」而非「撞默认」。
        assert_eq!(store.cache().emotion.neglect.level, 3);
        assert!(store.cache().emotion.neglect.p > 30.0);
        assert_eq!(store.cache().values.mood, 41.0);
        assert_eq!(store.cache().pet.name, "心月狐");
        assert_eq!(store.cache().pet.catchphrase.frequency, CatchphraseFrequency::Low);
        assert_eq!(store.cache().economy["coin"], 260);
        assert!(store.cache().emotion.neglect.pending_since_ms.is_none());
    }

    /// 夹具 `save_corrupt.json`：非法 JSON，必须走隔离 + 恢复链（AC-14 输入件）。
    #[test]
    fn fixture_save_corrupt_triggers_recovery_chain() {
        let dir = temp_dir("fixture-corrupt");
        let src = fixtures_dir().join("save_corrupt.json");
        assert!(src.is_file(), "缺少夹具：{}", src.display());
        fs::copy(&src, dir.join(SAVE_FILE)).expect("复制夹具失败（测试前置）");

        let (store, outcome) = SaveStore::load(&dir, T0);
        assert!(
            matches!(outcome.status, LoadStatus::IsolatedCorrupt { .. }),
            "损坏夹具应走隔离 + 默认档：{:?}",
            outcome.status
        );
        assert_eq!(store.cache(), &SaveFileV2::default());
        assert!(store.is_writable());
    }

    // -- 隔离命名 --------------------------------------------------------------

    #[test]
    fn isolate_paths_avoid_collision_within_same_millisecond() {
        let dir = temp_dir("isolate-collision");
        let save = dir.join(SAVE_FILE);
        fs::write(&save, "x").expect("写档失败（测试前置）");
        let mut warnings = Vec::new();
        let first = isolate(&save, "corrupt", T0, &mut warnings).expect("首次隔离");
        assert!(first.is_file());
        fs::write(&save, "x").expect("重建主档失败（测试前置）");
        let second = isolate(&save, "corrupt", T0, &mut warnings).expect("同刻二次隔离");
        assert_ne!(first, second, "同毫秒隔离必须换名，不得互相覆盖");
        assert!(first.is_file() && second.is_file());
    }

    #[test]
    fn isolate_missing_file_is_noop() {
        let dir = temp_dir("isolate-missing");
        let mut warnings = Vec::new();
        assert!(isolate(&dir.join(SAVE_FILE), "corrupt", T0, &mut warnings).is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn v1_bak_path_matches_doc_contract() {
        let p = v1_bak_path(Path::new("save.json"));
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "save.json.v1bak");
    }

    /// S5-M4 / FR-8-4：重置为默认档必须**立即落盘**（不等 30s 定时），且内容与内置默认一致。
    #[test]
    fn reset_to_default_replaces_cache_and_writes_immediately() {
        let dir = temp_dir("reset-to-default");
        let (mut store, _) = SaveStore::load(&dir, T0);
        // 先制造一份非默认内容并落盘，确保「重置」真的覆盖了它。
        store.cache_mut().values.mood = 3.0;
        store.cache_mut().pet.name = "X".to_string();
        store.flush_force(M0).expect("前置落盘失败");
        assert_ne!(store.cache().values.mood, SaveFileV2::default().values.mood);

        let wrote = store.reset_to_default(M0 + 1).expect("重置落盘失败");
        assert!(wrote, "重置必须真的写盘");
        assert_eq!(*store.cache(), SaveFileV2::default(), "缓存应回到内置默认档");
        assert!(store.is_writable(), "显式重置后必须可写（用户意志高于 v1 保护）");

        // 重载验证：磁盘上的档已被替换成默认档（不是内存幻觉）。
        let (reloaded, outcome) = SaveStore::load(&dir, T0 + 2);
        assert_eq!(reloaded.cache().values.mood, SaveFileV2::default().values.mood);
        assert_eq!(outcome.status, LoadStatus::Loaded);
        assert!(reloaded.cache().pet.name.is_empty(), "重置后不得残留用户名");
    }

    /// S5-M4：导入备份档必须先把当前主档留一份 `.bak`（导入错了还能退回来），
    /// 且拒绝非法 JSON / 版本不符的备份。
    #[test]
    fn import_from_backs_up_current_and_rejects_bad_source() {
        let dir = temp_dir("import-from");
        let (mut store, _) = SaveStore::load(&dir, T0);
        store.cache_mut().values.mood = 11.0;
        store.flush_force(M0).expect("前置落盘失败");

        if let Some(parent) = store.save_path().parent() {
            let source = parent.join("save.corrupt.9.json");
            // 非法 JSON → 拒绝，且不得改动主档。
            fs::write(&source, "{ not json ").expect("写坏源失败");
            assert!(store.import_from(&source, M0 + 1).is_err(), "非法 JSON 必须被拒绝");
            assert_eq!(store.cache().values.mood, 11.0, "拒绝导入时不得动主档");

            // 版本不符（v=1）→ 拒绝（v1 迁移归 S8-M7，导入不得绕过）。
            let wrong = SaveFileV2 { v: 1, ..SaveFileV2::default() };
            fs::write(&source, serde_json::to_string(&wrong).expect("可序列化"))
                .expect("写源失败");
            assert!(store.import_from(&source, M0 + 2).is_err(), "版本不符必须被拒绝");

            // 合法 v2 备份 → 成功导入 + 落盘 + 原主档进 `.bak`。
            let mut good = SaveFileV2::default();
            good.values.mood = 42.0;
            fs::write(&source, serde_json::to_string(&good).expect("可序列化"))
                .expect("写源失败");
            store.import_from(&source, M0 + 3).expect("合法备份应导入成功");
            assert_eq!(store.cache().values.mood, 42.0);

            let bak = store.save_path().with_extension(BAK_SUFFIX);
            assert!(bak.is_file(), "导入前必须先留下 .bak");
            let text = fs::read_to_string(&bak).expect("读 .bak 失败");
            let prev: SaveFileV2 = serde_json::from_str(&text).expect("旧主档应可解析");
            assert_eq!(prev.values.mood, 11.0, ".bak 保存的应是导入前的主档");
        } else {
            panic!("存档路径应有父目录");
        }
    }

    #[test]
    fn load_outcome_describe_covers_all_statuses() {
        let statuses = [
            LoadStatus::Fresh,
            LoadStatus::Loaded,
            LoadStatus::RecoveredFromBak { isolated: Some(PathBuf::from("save.corrupt.1.json")) },
            LoadStatus::RecoveredFromBak { isolated: None },
            LoadStatus::IsolatedCorrupt { isolated: PathBuf::from("save.corrupt.2.json") },
            LoadStatus::IsolatedFuture { isolated: PathBuf::from("save.future.3.json"), found: 7 },
            LoadStatus::MigrationPending { found: 1 },
        ];
        for status in statuses {
            let outcome = LoadOutcome { status: status.clone(), warnings: Vec::new() };
            let text = outcome.describe();
            assert!(!text.is_empty(), "{status:?}");
            assert_eq!(outcome.needs_notice(), !outcome.is_healthy());
        }
    }
}
