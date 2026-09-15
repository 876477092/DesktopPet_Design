//! `emotion::lines`：台词库抽取、冷却、占位符渲染与气泡计划（**S4-M5**，`02 §5.8` / §7.6）。
//!
//! ## 本卡职责（`03 §2 S4-M5`）
//!
//!   1. **台词池抽取**：按池键取句，`linePools.count=13` 与实际 key 对齐（L-02，含 `idle` 池）；
//!   2. **冷却**：同池两句话之间需间隔 `selector.cooldownSec`（默认 20s，与前端
//!      `BUBBLE_COOLDOWN_MS` 同源口径），冷却未过直接不出话（**不刷新冷却**，避免被顶住）；
//!   3. **占位符渲染**：`{name}` / `{user}` / `{coin}` / `{item}` 替换；**未命中的 token
//!      原样保留**（诚实降级，绝不硬编码角色名 —— C2）；
//!   4. **气泡计划**：`PlannedBubble`（`pet://bubble` 载荷的语义中继），含求助气泡
//!      3min 间隔 + 拒绝翻倍 ≤15min 冷却（`01 §6.16.3` 气泡规则增量，收口 `03 §3.3` B16-①②）；
//!   5. **口头禅闸门接口预留**：`CatchphraseFrequency` 为**枚举档位**（L-03，作废 `"1:3"`
//!      字符串比例），闸门 `CatchphraseGate` 本卡只交付接口与滚动窗口判定，
//!      **实际注入时机归 S7-M8**。
//!
//! ## 与 S7-M8 的切割（禁止顺手改动）
//!
//!   - 口头禅**改写**（把无口头禅句改写成含口头禅句、位置偏置 `positionBias`）归 S7-M8；
//!     本卡只提供「本句是否允许携带口头禅」的判定闸门与枚举档位解析。
//!   - 台词库**重写**（13 池全量内容打磨 + 命名接入设置页）归 S7-M8；本卡按
//!     `01 §6.16.3` 示例落**骨架内容**（每池 6 条）并把池键冻结。
//!
//! ## 时间纪律（C3）与配置纪律（C7）
//!
//!   本模块**零时钟**：`now_ms` 一律由调用方注入；冷却 / 间隔数值全部取自
//!   `resources/config/lines.json` 的 `selector` 段，代码内不写死。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::model::{CatchphraseCfg, CatchphraseFrequencyCfg, EmotionLevelCfg, LinePoolsCfg};

// ---------------------------------------------------------------------------
// 常量（池键为冻结契约，`character.json.linePools.keys` 是唯一真源）
// ---------------------------------------------------------------------------

/// 台词库文件名（`02 §3`：`resources/config/lines.json`）。
pub const LINES_FILE: &str = "lines.json";

/// 台词库配置版本（`lines.json.version`）。
pub const LINES_CONFIG_VERSION: u32 = 1;

/// 13 个台词池键（`character.json.linePools.keys` 的镜像；`LinePoolsCfg::keys` 为真源）。
///
/// 冻结顺序（L-02）：`idle` 必须存在（`emotion.json.levels[0].linePool = "idle"` 已引用）。
pub const POOL_KEYS: [&str; 13] = [
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
];

/// 气泡停留时长下界（毫秒，`02 §5 K-5`：单条 3~5s；与前端 `BUBBLE_DWELL_MIN_MS` 同源）。
pub const BUBBLE_DWELL_MIN_MS: u64 = 3_000;

/// 气泡停留时长上界（毫秒；与前端 `BUBBLE_DWELL_MAX_MS` 同源）。
pub const BUBBLE_DWELL_MAX_MS: u64 = 5_000;

/// 气泡停留时长默认值（毫秒；与前端 `BUBBLE_DWELL_DEFAULT_MS` 同源）。
pub const BUBBLE_DWELL_DEFAULT_MS: u64 = 4_000;

/// 求助类气泡被拒绝后的判定窗口（秒，`01 §6.16.3`：60s 内无操作视为拒绝）。
pub const HELP_REJECT_WINDOW_SEC: u64 = 60;

/// 求助类气泡的快捷按钮：去喂食（`feed`）与稍后（`later`）。
pub const HELP_ACTIONS_FOOD: [&str; 2] = ["feed", "later"];

/// 求助类气泡的快捷按钮：去洗澡（`bath`）与稍后（`later`）。
pub const HELP_ACTIONS_BATH: [&str; 2] = ["bath", "later"];

// ---------------------------------------------------------------------------
// 配置形态（`lines.json`）
// ---------------------------------------------------------------------------

/// 抽取与冷却参数（`lines.json.selector`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SelectorCfg {
    /// 同池两句话之间的最小间隔（秒；与前端 20s 同状态冷却同源）。
    pub cooldown_sec: u64,
    /// 求助类气泡基准间隔（秒，`01 §6.16.3`：≥3 分钟）。
    pub help_interval_sec: u64,
    /// 被拒绝后的间隔放大倍数（`01 §6.16.3`：翻倍）。
    pub help_reject_backoff_factor: u32,
    /// 求助类气泡间隔上限（秒，`01 §6.16.3`：最多 15 分钟一次）。
    pub help_interval_max_sec: u64,
}

impl Default for SelectorCfg {
    fn default() -> Self {
        Self {
            cooldown_sec: 20,
            help_interval_sec: 180,
            help_reject_backoff_factor: 2,
            help_interval_max_sec: 900,
        }
    }
}

/// `lines.json` 根（`02 §5.8`：13 池 × ≥6 条，含 `idle`）。
///
/// 字段缺省由 serde `default` 兜底（R19：老包升级不崩）；**池内容不提供内置默认**
/// （内容归美术 / 文案，缺失时降级为空库，只少气泡不崩，`03 §0.5` 纪律 7 红线不涉及）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LinesConfig {
    /// 配置版本（恒 [`LINES_CONFIG_VERSION`]）。
    pub version: u32,
    /// 池键 → 台词列表。
    pub pools: BTreeMap<String, Vec<String>>,
    /// 抽取与冷却参数。
    pub selector: SelectorCfg,
    /// 气泡快捷按钮 id → 文案（C7：UI 文案外置，代码内不写死中文按钮名）。
    pub bubble_actions: BTreeMap<String, String>,
}

impl Default for LinesConfig {
    fn default() -> Self {
        Self {
            version: LINES_CONFIG_VERSION,
            pools: BTreeMap::new(),
            selector: SelectorCfg::default(),
            bubble_actions: BTreeMap::new(),
        }
    }
}

/// 台词库错误（`#[non_exhaustive]`，`02 §7.4`）。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LinesError {
    /// 文件读取失败。
    #[error("读取台词库失败（{path}）：{source}")]
    Io {
        /// 文件路径。
        path: String,
        /// 底层错误。
        source: std::io::Error,
    },
    /// JSON 解析失败。
    #[error("台词库 JSON 解析失败：{0}")]
    Parse(#[from] serde_json::Error),
    /// 池键数与 `character.json.linePools.count` 不一致（L-02）。
    #[error("台词池数量不符：lines.json 有 {actual} 池，character.json.linePools.count={expected}")]
    PoolCountMismatch {
        /// 实际池数。
        actual: usize,
        /// 配置声明池数。
        expected: usize,
    },
    /// `character.json.linePools.keys` 声明的池在 `lines.json` 中缺失。
    #[error("台词池缺失：lines.json 缺少 character.json.linePools.keys 声明的池 `{pool}`")]
    MissingPool {
        /// 缺失的池键。
        pool: String,
    },
    /// 池条数不足 `minPerPool`。
    #[error("台词池 `{pool}` 条数不足：{actual} < {min}")]
    TooFewLines {
        /// 池键。
        pool: String,
        /// 实际条数。
        actual: usize,
        /// 下限。
        min: u32,
    },
    /// 禁用口头禅的池里出现了口头禅（`01 §6.16.2` 禁用场景，出现即视为 Bug）。
    #[error("台词池 `{pool}` 属 catchphrase.forbidPools，却含 {count} 条口头禅台词")]
    ForbiddenCatchphrase {
        /// 池键。
        pool: String,
        /// 命中的条数。
        count: usize,
    },
    /// 必带口头禅的池里含口头禅的条数不足。
    #[error("台词池 `{pool}` 含口头禅条数不足：{actual} < {min}")]
    TooFewCatchphrase {
        /// 池键。
        pool: String,
        /// 实际条数。
        actual: usize,
        /// 下限。
        min: u32,
    },
    /// 无口头禅变体条数不足（R1 降频改写需要备用句）。
    #[error("台词池 `{pool}` 无口头禅变体条数不足：{actual} < {min}")]
    TooFewPlainVariant {
        /// 池键。
        pool: String,
        /// 实际条数。
        actual: usize,
        /// 下限。
        min: u32,
    },
}

// ---------------------------------------------------------------------------
// 占位符渲染（C2：角色名走 `{name}`，未命中 token 原样保留）
// ---------------------------------------------------------------------------

/// 占位符变量集合（`01 §6.16.3`：`{name}` / `{user}` / `{coin}` / `{item}`）。
///
/// 语义：**只替换已提供的变量**；未提供（`None`）的 token 原样留在文案里，
/// 与前端 `bubbleLogic.renderPlaceholders` **同口径**（诚实降级，绝不猜测角色名 —— C2）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaceholderVars {
    /// 角色名（`character.json.defaultName`，用户可在设置页改名 —— FR-7-1）。
    pub name: Option<String>,
    /// 用户称谓（`01 §6.16.2` R4：称呼用户固定为「你」或 `{user}`）。
    pub user: Option<String>,
    /// 心币余额。
    pub coin: Option<String>,
    /// 物品名。
    pub item: Option<String>,
}

impl PlaceholderVars {
    /// 空变量集（全部 token 原样保留）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入角色名（链式）。
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// 注入用户称谓（链式）。
    #[must_use]
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// 注入心币余额（链式）。
    #[must_use]
    pub fn with_coin(mut self, coin: impl Into<String>) -> Self {
        self.coin = Some(coin.into());
        self
    }

    /// 注入物品名（链式）。
    #[must_use]
    pub fn with_item(mut self, item: impl Into<String>) -> Self {
        self.item = Some(item.into());
        self
    }

    /// 取 token 对应值（未提供 → `None`）。
    #[must_use]
    pub fn get(&self, token: &str) -> Option<&str> {
        match token {
            "name" => self.name.as_deref(),
            "user" => self.user.as_deref(),
            "coin" => self.coin.as_deref(),
            "item" => self.item.as_deref(),
            _ => None,
        }
    }
}

/// 渲染 `{token}` 占位符（C2）。
///
/// 规则（与前端 `renderPlaceholders` 同口径）：
///   - 命中 `vars` 的 token → 替换为对应值；
///   - 未命中（含未知 token、形如 `{}` 的空 token）→ **原样保留**，不吞字符；
///   - 不递归替换（替换值里若含 `{x}` 不再展开，避免模板注入）。
#[must_use]
pub fn render_placeholders(text: &str, vars: &PlaceholderVars) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c != '{' {
            out.push(c);
            i += 1;
            continue;
        }
        // 找最近的 '}'，中途不得再出现 '{'（与前端 `\{([^{}]*)\}` 同构）。
        let mut j = i + 1;
        let mut closed = None;
        while j < bytes.len() {
            match bytes[j] {
                '}' => {
                    closed = Some(j);
                    break;
                }
                '{' => break,
                _ => j += 1,
            }
        }
        match closed {
            Some(end) => {
                let token: String = bytes[i + 1..end].iter().collect();
                match vars.get(&token) {
                    Some(value) => out.push_str(value),
                    None => {
                        // 未命中：原样保留整段 `{token}`（含花括号）。
                        for c in &bytes[i..=end] {
                            out.push(*c);
                        }
                    }
                }
                i = end + 1;
            }
            None => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 台词库
// ---------------------------------------------------------------------------

/// 台词库：池内容 + 冷却参数 + 气泡按钮文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinesLibrary {
    pools: BTreeMap<String, Vec<String>>,
    bubble_actions: BTreeMap<String, String>,
    selector: SelectorCfg,
}

impl Default for LinesLibrary {
    fn default() -> Self {
        Self::empty()
    }
}

impl LinesLibrary {
    /// 空库（配置缺失 / 损坏时的降级形态：只少气泡，不崩 —— 同 R19 口径）。
    #[must_use]
    pub fn empty() -> Self {
        Self {
            pools: BTreeMap::new(),
            bubble_actions: BTreeMap::new(),
            selector: SelectorCfg::default(),
        }
    }

    /// 解析 `lines.json` 文本。
    ///
    /// # Errors
    /// JSON 非法时返回 [`LinesError::Parse`]。
    pub fn from_json_str(text: &str) -> Result<Self, LinesError> {
        let cfg: LinesConfig = serde_json::from_str(text)?;
        Ok(Self::from_config(cfg))
    }

    /// 由已解析的 [`LinesConfig`] 构造。
    #[must_use]
    pub fn from_config(cfg: LinesConfig) -> Self {
        Self {
            pools: cfg.pools,
            bubble_actions: cfg.bubble_actions,
            selector: cfg.selector,
        }
    }

    /// 从配置目录读取 `lines.json`。
    ///
    /// # Errors
    /// 文件读取失败 → [`LinesError::Io`]；JSON 非法 → [`LinesError::Parse`]。
    pub fn load_dir(dir: &Path) -> Result<Self, LinesError> {
        let path = dir.join(LINES_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| LinesError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_json_str(&text)
    }

    /// 抽取与冷却参数（`lines.json.selector`）。
    #[must_use]
    pub fn selector(&self) -> &SelectorCfg {
        &self.selector
    }

    /// 取某池的全部台词（池不存在 → `None`）。
    #[must_use]
    pub fn pool(&self, key: &str) -> Option<&[String]> {
        self.pools.get(key).map(Vec::as_slice)
    }

    /// 全部池键（字典序，`BTreeMap` 天然有序；供诊断 / 一致性校验）。
    pub fn pool_keys(&self) -> impl Iterator<Item = &str> {
        self.pools.keys().map(String::as_str)
    }

    /// 池数量。
    #[must_use]
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// 气泡快捷按钮文案（id → 文案；未登记 → `None`，调用方回退 `id`）。
    #[must_use]
    pub fn bubble_action_label(&self, id: &str) -> Option<&str> {
        self.bubble_actions.get(id).map(String::as_str)
    }

    /// 与 `character.json` 交叉校验（L-02：`linePools.count` 与实际 key 对齐）。
    ///
    /// 口径（三条，逐池）：
    ///   1. 池数量必须等于 `line_pools.count`，且 `line_pools.keys` 声明的每个池都必须存在；
    ///   2. 每池条数 ≥ `line_pools.min_per_pool`；
    ///   3. 口头禅分布：`catchphrase.forbid_pools` 中的池**不得**出现口头禅 token
    ///      （`01 §6.16.2` 禁用场景「出现即视为 Bug」）；其余池含 token 的条数
    ///      ≥ `line_pools.min_with_catchphrase`，且**不含** token 的条数
    ///      ≥ `line_pools.min_plain_variant`（R1 降频改写需要备用句）。
    ///
    /// # Errors
    /// 任一条件不满足返回对应 [`LinesError`] 变体。
    pub fn validate(
        &self,
        line_pools: &LinePoolsCfg,
        catchphrase: &CatchphraseCfg,
    ) -> Result<(), LinesError> {
        if self.pools.len() != line_pools.count as usize {
            return Err(LinesError::PoolCountMismatch {
                actual: self.pools.len(),
                expected: line_pools.count as usize,
            });
        }
        let token = catchphrase.token.as_str();
        for key in &line_pools.keys {
            let Some(lines) = self.pools.get(key) else {
                return Err(LinesError::MissingPool { pool: key.clone() });
            };
            if lines.len() < line_pools.min_per_pool as usize {
                return Err(LinesError::TooFewLines {
                    pool: key.clone(),
                    actual: lines.len(),
                    min: line_pools.min_per_pool,
                });
            }
            let has_token = |s: &String| !token.is_empty() && s.contains(token);
            let with_cp = lines.iter().filter(|s| has_token(s)).count();
            let plain = lines.len() - with_cp;
            if catchphrase.forbid_pools.iter().any(|p| p == key) {
                if with_cp != 0 {
                    return Err(LinesError::ForbiddenCatchphrase { pool: key.clone(), count: with_cp });
                }
            } else if with_cp < line_pools.min_with_catchphrase as usize {
                return Err(LinesError::TooFewCatchphrase {
                    pool: key.clone(),
                    actual: with_cp,
                    min: line_pools.min_with_catchphrase,
                });
            }
            if plain < line_pools.min_plain_variant as usize {
                return Err(LinesError::TooFewPlainVariant {
                    pool: key.clone(),
                    actual: plain,
                    min: line_pools.min_plain_variant,
                });
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 抽取 + 冷却
// ---------------------------------------------------------------------------

/// 一次抽取结果（借用库内文本，零拷贝）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinePick<'a> {
    /// 池键。
    pub pool: &'a str,
    /// 池内下标。
    pub index: usize,
    /// 台词原文（**未**做占位符渲染）。
    pub text: &'a str,
}

/// 台词抽取器：同池冷却 + 确定性打散（相邻两次不重复同一句）。
///
/// 时间口径（C3）：`now_ms` 由调用方注入，本结构不读时钟。
#[derive(Debug, Clone, Default)]
pub struct LineSelector {
    cooldown_sec: u64,
    last_pick_ms: BTreeMap<String, i64>,
    last_index: BTreeMap<String, usize>,
}

impl LineSelector {
    /// 以冷却秒数构造（取自 [`SelectorCfg::cooldown_sec`]）。
    #[must_use]
    pub fn new(cooldown_sec: u64) -> Self {
        Self { cooldown_sec, last_pick_ms: BTreeMap::new(), last_index: BTreeMap::new() }
    }

    /// 由库参数构造。
    #[must_use]
    pub fn from_library(lib: &LinesLibrary) -> Self {
        Self::new(lib.selector().cooldown_sec)
    }

    /// 冷却秒数。
    #[must_use]
    pub fn cooldown_sec(&self) -> u64 {
        self.cooldown_sec
    }

    /// 该池当前是否已过冷却（从未抽取 → `true`）。
    #[must_use]
    pub fn is_ready(&self, pool: &str, now_ms: i64) -> bool {
        match self.last_pick_ms.get(pool) {
            None => true,
            Some(last) => now_ms - *last >= self.cooldown_window_ms(),
        }
    }

    /// 该池下次可抽取的时刻（毫秒；从未抽取 → `None`）。
    #[must_use]
    pub fn next_allowed_ms(&self, pool: &str) -> Option<i64> {
        self.last_pick_ms.get(pool).map(|last| last + self.cooldown_window_ms())
    }

    /// 冷却窗口（毫秒）。
    #[must_use]
    pub fn cooldown_window_ms(&self) -> i64 {
        (self.cooldown_sec as i64).saturating_mul(1_000)
    }

    /// 抽取一句：冷却未过 / 池缺失 / 池为空 → `None`。
    ///
    /// 命中时**记录本次时刻**（冷却从此刻起算）；未命中时**不刷新冷却**
    /// （否则「连点」会把冷却一直顶住，与前端 `shouldSuppressDuplicate` 同口径）。
    ///
    /// 打散口径：以 `(pool, now_ms)` 做确定性哈希取下标；若与上一次同池下标相同，
    /// 则顺延一位，保证连续两次不同句（确定性因此可单测）。
    pub fn pick<'a>(
        &mut self,
        lib: &'a LinesLibrary,
        pool: &'a str,
        now_ms: i64,
    ) -> Option<LinePick<'a>> {
        if !self.is_ready(pool, now_ms) {
            return None;
        }
        let lines = lib.pool(pool)?;
        if lines.is_empty() {
            return None;
        }
        let n = lines.len();
        let mut index = hash_index(pool, now_ms, n);
        if self.last_index.get(pool).is_some_and(|prev| *prev == index) {
            index = (index + 1) % n;
        }
        self.last_pick_ms.insert(pool.to_string(), now_ms);
        self.last_index.insert(pool.to_string(), index);
        Some(LinePick { pool, index, text: lines[index].as_str() })
    }
}

/// 确定性打散：FNV-1a 喂 `pool` → SplitMix64 终混 → 取模。
fn hash_index(pool: &str, now_ms: i64, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in pool.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^= now_ms as u64;
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    (h % n as u64) as usize
}

// ---------------------------------------------------------------------------
// 口头禅闸门（L-03 枚举档位；**接口预留，实际注入归 S7-M8**）
// ---------------------------------------------------------------------------

/// 口头禅频率档位（L-03：`off` / `low` / `standard` / `high`）。
///
/// **作废**存档里的 `"1:3"` 字符串比例写法（L-03 修正）：档位是真源，
/// 数值取自 [`CatchphraseFrequencyCfg`]（`CharacterConfig.catchphrase.frequency`）。
///
/// serde 口径（S5-M1 补）：`rename_all = "lowercase"`，**线上 / 存档均为档位字符串**
/// （`"off"` / `"low"` / `"standard"` / `"high"`），与 [`Self::as_cfg_name`] 逐字一致，
/// 保证存档 ↔ 配置 ↔ 前端三处同名同值（单一真源，避免出现第二个字符串映射表）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum CatchphraseFrequency {
    /// 关闭（永不带口头禅）。
    Off,
    /// 低频。
    Low,
    /// 标准（默认档）。
    #[default]
    Standard,
    /// 高频。
    High,
}

impl CatchphraseFrequency {
    /// 由配置字符串解析；未知字符串回退 [`Self::Standard`]（配置容错，不 panic）。
    #[must_use]
    pub fn from_cfg_name(name: &str) -> Self {
        match name {
            "off" => Self::Off,
            "low" => Self::Low,
            "high" => Self::High,
            _ => Self::Standard,
        }
    }

    /// 配置字符串（camelCase，与 `character.json.catchphrase.defaultFrequency` 同源）。
    #[must_use]
    pub fn as_cfg_name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Standard => "standard",
            Self::High => "high",
        }
    }

    /// 该档位的「每 N 句最多 1 句含口头禅」窗口宽度（R1）。
    ///
    /// `off` → `0`（关闭，闸门恒不放行）；其余取 [`CatchphraseFrequencyCfg`] 对应字段。
    #[must_use]
    pub fn window(&self, freq: &CatchphraseFrequencyCfg) -> u32 {
        match self {
            Self::Off => 0,
            Self::Low => freq.low,
            Self::Standard => freq.standard,
            Self::High => freq.high,
        }
    }
}

/// 口头禅闸门（R1 频率上限 + R3 冷却）。
///
/// **S4-M5 交付边界**：本卡只交付「本句是否允许携带口头禅」的判定接口；
/// 真正把无口头禅句**改写**成含口头禅句（含 `positionBias` 位置偏置与
/// `maxPerSentence` 单句上限）归 **S7-M8**（`01 §6.16.2`）。
#[derive(Debug, Clone)]
pub struct CatchphraseGate {
    enabled: bool,
    frequency: CatchphraseFrequency,
    min_interval_sec: u64,
    max_per_sentence: u32,
    /// 滚动窗口：最近 `window` 句是否已含口头禅（`true` = 含）。
    window: Vec<bool>,
    /// 上次放行口头禅的时刻。
    last_grant_ms: Option<i64>,
}

impl CatchphraseGate {
    /// 由 `character.json.catchphrase` 构造。
    #[must_use]
    pub fn from_cfg(cfg: &CatchphraseCfg) -> Self {
        Self {
            enabled: cfg.default_enabled,
            frequency: CatchphraseFrequency::from_cfg_name(&cfg.default_frequency),
            min_interval_sec: cfg.min_interval_sec,
            max_per_sentence: cfg.max_per_sentence,
            window: Vec::new(),
            last_grant_ms: None,
        }
    }

    /// 当前是否启用口头禅。
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 开关（设置页 FR-7-10 接线归 S5）。
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.window.clear();
        }
    }

    /// 当前档位。
    #[must_use]
    pub fn frequency(&self) -> CatchphraseFrequency {
        self.frequency
    }

    /// 切档（设置页 FR-7-10 接线归 S5）。
    pub fn set_frequency(&mut self, freq: CatchphraseFrequency) {
        self.frequency = freq;
    }

    /// 单句口头禅上限（R2）。
    #[must_use]
    pub fn max_per_sentence(&self) -> u32 {
        self.max_per_sentence
    }

    /// 判定「本句是否允许含口头禅」，并把本句记入滚动窗口。
    ///
    /// 判定顺序（任一不满足即不放行，R1/R3）：
    ///   1. 未启用 / 档位为 `off` / 窗口宽度为 0 → 不放行；
    ///   2. 距上次放行不足 `minIntervalSec` → 不放行（R3 冷却）；
    ///   3. 滚动窗口内已含口头禅 → 不放行（R1：「每 N 句最多 1 句」）。
    ///
    /// 返回值即「本句是否允许携带口头禅」；**放行结果同时记入滚动窗口**
    /// （正确实现下「允许」等价于「实际含」，故窗口记放行结果而非事后回填）。
    pub fn should_inject(&mut self, now_ms: i64, freq: &CatchphraseFrequencyCfg) -> bool {
        let width = self.frequency.window(freq) as usize;
        if width == 0 {
            self.window.clear();
            return false;
        }
        // MSRV（1.77.2）口径：用 `match` 而非 `Option::is_none_or`（后者 stable since 1.82）。
        let cooldown_ok = match self.last_grant_ms {
            None => true,
            Some(last) => now_ms - last >= (self.min_interval_sec as i64).saturating_mul(1_000),
        };
        let allow = self.enabled && self.window.iter().all(|used| !*used) && cooldown_ok;
        // 记入滚动窗口（保留最近 width 句）。
        self.window.push(allow);
        if self.window.len() > width {
            let excess = self.window.len() - width;
            self.window.drain(0..excess);
        }
        if allow {
            self.last_grant_ms = Some(now_ms);
        }
        allow
    }
}

// ---------------------------------------------------------------------------
// 气泡计划（`pet://bubble` 语义中继；线上载荷归 `dp-core::event`）
// ---------------------------------------------------------------------------

/// 气泡类别（`01 §6.5.4` / §6.12.6 / §6.16.3-4；serde 小写，与前端 `BubbleKind` 同源）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BubbleKind {
    /// 提醒类（勿扰下唯一放行类别）。
    Reminder,
    /// 求助类（讨食 / 求洗澡；带署名与快捷按钮）。
    Help,
    /// 对话 / 系统闲聊（默认）。
    #[default]
    Chat,
    /// 明信片（外出活动中）。
    Postcard,
}

impl BubbleKind {
    /// 由台词池键派生类别（唯一映射点）。
    ///
    /// 口径（`01 §6.16.3` 气泡规则增量）：
    ///   - `begFood` / `begBath` → [`BubbleKind::Help`]（主动求助气泡，优先级高于系统闲聊）；
    ///   - `outing` → [`BubbleKind::Postcard`]（明信片挂件）；
    ///   - `remind` / `system` → [`BubbleKind::Reminder`]（仅供 S7 提醒池复用）；
    ///   - 其余 → [`BubbleKind::Chat`]。
    #[must_use]
    pub fn for_pool(pool: &str) -> Self {
        match pool {
            "begFood" | "begBath" => Self::Help,
            "outing" => Self::Postcard,
            "remind" | "system" => Self::Reminder,
            _ => Self::Chat,
        }
    }

    /// 是否默认展示署名（`01 §6.16.4`：仅求助 / 明信片类显示）。
    #[must_use]
    pub fn default_show_signature(self) -> bool {
        matches!(self, Self::Help | Self::Postcard)
    }
}

/// 气泡快捷按钮（`01 §6.12.6 ④`；文案取自 `lines.json.bubbleActions`，代码不写死中文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BubbleAction {
    /// 按钮标识（如 `feed` / `bath` / `later`）。
    pub id: String,
    /// 按钮文案（**已渲染占位符**）。
    pub label: String,
}

/// 气泡计划：线上 `BubbleCmd` 载荷的语义中继（渲染后的文案 + 语义字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedBubble {
    /// 文案（**已**渲染 `{name}` 等占位符）。
    pub text: String,
    /// 类别。
    pub kind: BubbleKind,
    /// 用户交互台词 → 即时覆盖系统台词（`01 §6.5.4`）。
    pub preempt: bool,
    /// 冷却分组键（同状态 ≥20s 冷却；空串时消费侧回退 `kind`）。
    pub cooldown_key: String,
    /// 停留时长（毫秒，钳到 `[3000,5000]`）。
    pub dwell_ms: u64,
    /// 是否展示署名。
    pub show_signature: bool,
    /// 快捷按钮列表。
    pub actions: Vec<BubbleAction>,
    /// 高对比（与用户设置取或；消费侧裁决）。
    pub high_contrast: bool,
}

/// 求助类气泡冷却（`01 §6.16.3`：同类求助间隔 ≥3min，被拒绝后翻倍，最多 15min 一次）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpCooldown {
    base_ms: i64,
    factor: u32,
    max_ms: i64,
    interval_ms: i64,
    next_allowed_ms: i64,
}

impl HelpCooldown {
    /// 由 `lines.json.selector` 构造（`factor` 至少 1，避免「拒绝反而不退避」）。
    #[must_use]
    pub fn new(base_sec: u64, factor: u32, max_sec: u64) -> Self {
        let base_ms = (base_sec as i64).saturating_mul(1_000).max(0);
        let max_ms = (max_sec as i64).saturating_mul(1_000).max(base_ms);
        Self { base_ms, factor: factor.max(1), max_ms, interval_ms: base_ms, next_allowed_ms: 0 }
    }

    /// 由库参数构造。
    #[must_use]
    pub fn from_library(lib: &LinesLibrary) -> Self {
        let s = lib.selector();
        Self::new(s.help_interval_sec, s.help_reject_backoff_factor, s.help_interval_max_sec)
    }

    /// 基准间隔（毫秒）。
    #[must_use]
    pub fn base_ms(&self) -> i64 {
        self.base_ms
    }

    /// 当前生效间隔（毫秒；随拒绝逐次放大，封顶 `max`）。
    #[must_use]
    pub fn interval_ms(&self) -> i64 {
        self.interval_ms
    }

    /// 下次允许出现的时刻（毫秒；`0` = 从未出现过，立即可用）。
    #[must_use]
    pub fn next_allowed_ms(&self) -> i64 {
        self.next_allowed_ms
    }

    /// 当前是否可出求助气泡。
    #[must_use]
    pub fn is_ready(&self, now_ms: i64) -> bool {
        now_ms >= self.next_allowed_ms
    }

    /// 已出一次：下次允许时刻 = `now + 当前间隔`。
    pub fn on_shown(&mut self, now_ms: i64) {
        self.next_allowed_ms = now_ms.saturating_add(self.interval_ms);
    }

    /// 用户响应（喂食 / 洗澡）：间隔**回退到基准**（求助被满足，不再惩罚），并重排下次时刻。
    pub fn on_engaged(&mut self, now_ms: i64) {
        self.interval_ms = self.base_ms;
        self.next_allowed_ms = now_ms.saturating_add(self.interval_ms);
    }

    /// 被拒绝（`HELP_REJECT_WINDOW_SEC` 内无操作）：间隔 ×`factor` 并封顶，重排下次时刻。
    pub fn on_rejected(&mut self, now_ms: i64) {
        let grown = self.interval_ms.saturating_mul(i64::from(self.factor));
        self.interval_ms = grown.clamp(self.base_ms, self.max_ms);
        self.next_allowed_ms = now_ms.saturating_add(self.interval_ms);
    }
}

/// 气泡计划器：台词抽取 + 冷却 + 求助退避（S4-M5）。
#[derive(Debug, Clone)]
pub struct BubblePlanner {
    selector: LineSelector,
    help: HelpCooldown,
    bubble_actions: BTreeMap<String, String>,
}

impl Default for BubblePlanner {
    fn default() -> Self {
        Self {
            selector: LineSelector::default(),
            help: HelpCooldown::new(180, 2, 900),
            bubble_actions: BTreeMap::new(),
        }
    }
}

impl BubblePlanner {
    /// 由台词库构造（冷却 / 间隔参数全部取自 `lines.json.selector`）。
    #[must_use]
    pub fn from_library(lib: &LinesLibrary) -> Self {
        Self {
            selector: LineSelector::from_library(lib),
            help: HelpCooldown::from_library(lib),
            bubble_actions: lib.bubble_actions.clone(),
        }
    }

    /// 只读：台词抽取器（诊断 / 单测）。
    #[must_use]
    pub fn selector(&self) -> &LineSelector {
        &self.selector
    }

    /// 只读：求助冷却（诊断 / 单测）。
    #[must_use]
    pub fn help(&self) -> &HelpCooldown {
        &self.help
    }

    /// 拼装按钮列表（文案取自 `lines.json.bubbleActions`；未登记 → 回退 `id`）。
    fn actions_for(&self, ids: &[&str], vars: &PlaceholderVars) -> Vec<BubbleAction> {
        ids.iter()
            .map(|id| {
                let label = self.bubble_actions.get(*id).map_or_else(
                    || (*id).to_string(),
                    |raw| render_placeholders(raw, vars),
                );
                BubbleAction { id: (*id).to_string(), label }
            })
            .collect()
    }

    /// 由台词池产出气泡（冷却未过 → `None`）。
    ///
    /// 类别由 [`BubbleKind::for_pool`] 派生；求助类（`begFood` / `begBath`）额外受
    /// [`HelpCooldown`] 约束，并携带对应快捷按钮（`feed`/`later` 或 `bath`/`later`）。
    pub fn bubble_for_pool(
        &mut self,
        lib: &LinesLibrary,
        pool: &str,
        vars: &PlaceholderVars,
        now_ms: i64,
        preempt: bool,
    ) -> Option<PlannedBubble> {
        let kind = BubbleKind::for_pool(pool);
        if kind == BubbleKind::Help && !self.help.is_ready(now_ms) {
            return None;
        }
        let pick = self.selector.pick(lib, pool, now_ms)?;
        let text = render_placeholders(pick.text, vars);
        if kind == BubbleKind::Help {
            self.help.on_shown(now_ms);
        }
        let actions = match (kind, pool) {
            (BubbleKind::Help, "begBath") => self.actions_for(&HELP_ACTIONS_BATH, vars),
            (BubbleKind::Help, _) => self.actions_for(&HELP_ACTIONS_FOOD, vars),
            _ => Vec::new(),
        };
        Some(PlannedBubble {
            text,
            kind,
            preempt,
            cooldown_key: pool.to_string(),
            dwell_ms: BUBBLE_DWELL_DEFAULT_MS,
            show_signature: kind.default_show_signature(),
            actions,
            high_contrast: false,
        })
    }

    /// 由冷落档位产出气泡：**池键取自 `emotion.json.levels[level].linePool`**（配置驱动）。
    ///
    /// `level` 越界 → `None`（不 panic）。
    pub fn bubble_for_level(
        &mut self,
        lib: &LinesLibrary,
        levels: &[EmotionLevelCfg],
        level: u8,
        vars: &PlaceholderVars,
        now_ms: i64,
    ) -> Option<PlannedBubble> {
        let cfg = levels.get(usize::from(level))?;
        self.bubble_for_pool(lib, &cfg.line_pool, vars, now_ms, false)
    }

    /// 用户响应求助气泡（`feed` / `bath`）：间隔回退基准。
    pub fn on_bubble_action(&mut self, action_id: &str, now_ms: i64) {
        match action_id {
            "feed" | "bath" => self.help.on_engaged(now_ms),
            "later" => self.help.on_rejected(now_ms),
            _ => {}
        }
    }
}

/// 停留时长钳到 `[3000,5000]`（`02 §5 K-5`；与前端 `clampDwellMs` 同口径）。
#[must_use]
pub fn clamp_dwell_ms(ms: u64) -> u64 {
    ms.clamp(BUBBLE_DWELL_MIN_MS, BUBBLE_DWELL_MAX_MS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::CharacterConfig;

    fn resources_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/config")
    }

    fn library() -> LinesLibrary {
        LinesLibrary::load_dir(&resources_dir()).expect("lines.json 应可加载")
    }

    fn character() -> CharacterConfig {
        let text = std::fs::read_to_string(resources_dir().join("character.json"))
            .expect("character.json 应可读");
        serde_json::from_str(&text).expect("character.json 应可解析")
    }

    fn vars() -> PlaceholderVars {
        PlaceholderVars::new().with_name("阿狐").with_user("你").with_coin("120").with_item("饭团")
    }

    /// `resource` 路径以码点构造期望值，避免测试里硬编码角色名（C2 同口径）。
    fn name_cp() -> String {
        ['\u{5FC3}', '\u{6708}', '\u{72D0}'].iter().collect()
    }

    #[test]
    fn loads_13_pools_with_six_lines_each() {
        let lib = library();
        assert_eq!(lib.pool_count(), 13, "L-02：应有 13 个池");
        for key in POOL_KEYS {
            let pool = lib.pool(key).unwrap_or_else(|| panic!("池 {key} 应存在"));
            assert!(pool.len() >= 6, "池 {key} 应 ≥6 条，实际 {}", pool.len());
        }
    }

    #[test]
    fn pool_keys_const_matches_character_config() {
        let cfg = character();
        assert_eq!(
            POOL_KEYS.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
            cfg.line_pools.keys,
            "POOL_KEYS 必须与 character.json.linePools.keys 逐项一致"
        );
        assert_eq!(cfg.line_pools.count as usize, POOL_KEYS.len());
        // 库内池键集合（`BTreeMap` 字典序）须与配置声明的键集合等价（顺序由 POOL_KEYS 承载）。
        let mut from_lib = lib_pool_keys();
        from_lib.sort();
        let mut from_cfg = cfg.line_pools.keys.clone();
        from_cfg.sort();
        assert_eq!(from_lib, from_cfg);
    }

    fn lib_pool_keys() -> Vec<String> {
        library().pool_keys().map(str::to_string).collect()
    }

    #[test]
    fn default_name_is_code_point_built_and_config_backed() {
        let cfg = character();
        assert_eq!(cfg.default_name, name_cp(), "C2：默认名以码点构造");
    }

    #[test]
    fn validate_accepts_shipped_lines_json() {
        let cfg = character();
        library()
            .validate(&cfg.line_pools, &cfg.catchphrase)
            .expect("随包 lines.json 必须通过交叉校验");
    }

    #[test]
    fn validate_rejects_pool_count_mismatch() {
        let cfg = character();
        let mut pools = cfg.line_pools.clone();
        pools.count = 12;
        let err = library().validate(&pools, &cfg.catchphrase).expect_err("池数不符应报错");
        assert!(matches!(err, LinesError::PoolCountMismatch { actual: 13, expected: 12 }));
    }

    #[test]
    fn validate_rejects_missing_pool() {
        let cfg = character();
        let mut text = std::fs::read_to_string(resources_dir().join(LINES_FILE)).expect("可读");
        text = text.replace("\"idle\": [", "\"idleXX\": [");
        let lib = LinesLibrary::from_json_str(&text).expect("改写后仍可解析");
        let err = lib.validate(&cfg.line_pools, &cfg.catchphrase).expect_err("缺池应报错");
        assert!(matches!(err, LinesError::MissingPool { .. }));
    }

    #[test]
    fn validate_rejects_too_few_lines() {
        let cfg = character();
        let mut text = std::fs::read_to_string(resources_dir().join(LINES_FILE)).expect("可读");
        // 去掉 idle 池首条，令其不足 6 条。
        let needle = "      \"{name}在这儿陪着你～\",\n";
        assert!(text.contains(needle), "测试夹具应找到 idle 池首条");
        text = text.replacen(needle, "", 1);
        let lib = LinesLibrary::from_json_str(&text).expect("改写后仍可解析");
        let err = lib.validate(&cfg.line_pools, &cfg.catchphrase).expect_err("条数不足应报错");
        assert!(matches!(err, LinesError::TooFewLines { ref pool, actual: 5, min: 6 } if pool == "idle"));
    }

    #[test]
    fn validate_rejects_forbidden_catchphrase_in_angry_pool() {
        let cfg = character();
        let mut lib = library();
        let angry = lib.pools.get_mut("angry").expect("angry 池应存在");
        angry[0] = format!("{}{}", angry[0], cfg.catchphrase.token);
        let err = lib.validate(&cfg.line_pools, &cfg.catchphrase).expect_err("禁用池含口头禅应报错");
        assert!(matches!(err, LinesError::ForbiddenCatchphrase { ref pool, count: 1 } if pool == "angry"));
    }

    #[test]
    fn validate_rejects_too_few_catchphrase_and_plain() {
        let cfg = character();
        let token = cfg.catchphrase.token.clone();

        // ① 含口头禅条数不足：把 happy 池去掉口头禅 token。
        let mut lib = library();
        let happy = lib.pools.get_mut("happy").expect("happy 池应存在");
        for line in happy.iter_mut() {
            *line = line.replace(&token, "");
        }
        // 逐条去掉 token 后，可能出现重复句；`min_plain_variant` 仍满足，故命中含口头禅不足分支。
        let err = lib.validate(&cfg.line_pools, &cfg.catchphrase).expect_err("口头禅不足应报错");
        assert!(
            matches!(err, LinesError::TooFewCatchphrase { ref pool, .. } if pool == "happy"),
            "实际：{err}"
        );

        // ② 无口头禅变体不足：把 idle 池每句都塞入口头禅。
        let mut lib = library();
        let idle = lib.pools.get_mut("idle").expect("idle 池应存在");
        for line in idle.iter_mut() {
            if !line.contains(&token) {
                *line = format!("{token}{line}");
            }
        }
        let err = lib.validate(&cfg.line_pools, &cfg.catchphrase).expect_err("无口头禅变体不足应报错");
        assert!(matches!(err, LinesError::TooFewPlainVariant { ref pool, actual: 0, .. } if pool == "idle"));
    }

    #[test]
    fn render_placeholders_replaces_name_only_when_provided() {
        let with = render_placeholders("{name}回来啦", &vars());
        assert_eq!(with, "阿狐回来啦");
        let without = render_placeholders("{name}回来啦", &PlaceholderVars::new());
        assert_eq!(without, "{name}回来啦", "未提供 name 时 token 原样保留（C2 诚实降级）");
    }

    #[test]
    fn render_placeholders_handles_all_known_tokens() {
        let out = render_placeholders("{name}/{user}/{coin}/{item}", &vars());
        assert_eq!(out, "阿狐/你/120/饭团");
    }

    #[test]
    fn render_placeholders_keeps_unknown_and_malformed_tokens() {
        assert_eq!(render_placeholders("{mood}", &vars()), "{mood}");
        assert_eq!(render_placeholders("{}", &vars()), "{}", "空 token 原样保留");
        assert_eq!(render_placeholders("a{b", &vars()), "a{b", "未闭合花括号原样保留");
        assert_eq!(render_placeholders("{{name}}", &vars()), "{阿狐}", "不递归、按最内层替换");
    }

    #[test]
    fn render_placeholders_is_identity_for_plain_text() {
        assert_eq!(render_placeholders("没有占位符的句子", &vars()), "没有占位符的句子");
        assert_eq!(render_placeholders("", &vars()), "");
    }

    #[test]
    fn render_placeholders_does_not_recurse_into_values() {
        let v = PlaceholderVars::new().with_name("{user}");
        assert_eq!(render_placeholders("{name}", &v), "{user}", "替换值不再展开（防模板注入）");
    }

    #[test]
    fn pick_returns_line_from_requested_pool_and_renders() {
        let lib = library();
        let mut sel = LineSelector::from_library(&lib);
        let pick = sel.pick(&lib, "happy", 0).expect("首次抽取应命中");
        assert_eq!(pick.pool, "happy");
        assert!(lib.pool("happy").expect("池存在").contains(&pick.text.to_string()));
        assert_eq!(render_placeholders(pick.text, &vars()), pick.text.replace("{name}", "阿狐"));
    }

    #[test]
    fn pick_enforces_cooldown_without_refreshing_on_suppression() {
        let lib = library();
        let mut sel = LineSelector::from_library(&lib);
        let window = sel.cooldown_window_ms();
        assert!(sel.pick(&lib, "happy", 0).is_some());
        // 冷却内（含边界前 1ms）：拒绝，且不刷新冷却。
        assert!(sel.pick(&lib, "happy", window - 1).is_none());
        assert_eq!(sel.next_allowed_ms("happy"), Some(window), "被抑制不得刷新冷却");
        // 恰为窗口：放行。
        assert!(sel.pick(&lib, "happy", window).is_some());
    }

    #[test]
    fn pick_cooldown_is_per_pool() {
        let lib = library();
        let mut sel = LineSelector::from_library(&lib);
        assert!(sel.pick(&lib, "happy", 0).is_some());
        assert!(sel.pick(&lib, "bored", 0).is_some(), "不同池冷却互不影响");
        assert!(!sel.is_ready("happy", 1));
        assert!(!sel.is_ready("bored", 1));
        assert!(sel.is_ready("curious", 1), "未抽取过的池恒就绪");
    }

    #[test]
    fn pick_avoids_immediate_repeat_and_is_deterministic() {
        let lib = library();
        let window = LineSelector::from_library(&lib).cooldown_window_ms();
        let mut a = LineSelector::from_library(&lib);
        let mut b = LineSelector::from_library(&lib);
        let first = a.pick(&lib, "idle", 0).expect("首次应命中").index;
        let second = a.pick(&lib, "idle", window).expect("过冷后应命中").index;
        assert_ne!(first, second, "连续两次不得同句（确定性顺延）");
        // 确定性：同输入序列 → 同输出序列。
        let x = b.pick(&lib, "idle", 0).expect("首次应命中").index;
        let y = b.pick(&lib, "idle", window).expect("过冷后应命中").index;
        assert_eq!((x, y), (first, second));
    }

    #[test]
    fn pick_returns_none_for_unknown_pool() {
        let lib = library();
        let mut sel = LineSelector::from_library(&lib);
        assert!(sel.pick(&lib, "nope", 0).is_none());
    }

    #[test]
    fn empty_library_degrades_gracefully() {
        let lib = LinesLibrary::empty();
        let mut sel = LineSelector::from_library(&lib);
        assert_eq!(lib.pool_count(), 0);
        assert!(sel.pick(&lib, "happy", 0).is_none(), "空库抽取应返回 None 而非 panic");
        let mut planner = BubblePlanner::from_library(&lib);
        assert!(planner
            .bubble_for_pool(&lib, "happy", &vars(), 0, false)
            .is_none());
    }

    #[test]
    fn catchphrase_frequency_parses_enum_ranks() {
        assert_eq!(CatchphraseFrequency::from_cfg_name("off"), CatchphraseFrequency::Off);
        assert_eq!(CatchphraseFrequency::from_cfg_name("low"), CatchphraseFrequency::Low);
        assert_eq!(
            CatchphraseFrequency::from_cfg_name("standard"),
            CatchphraseFrequency::Standard
        );
        assert_eq!(CatchphraseFrequency::from_cfg_name("high"), CatchphraseFrequency::High);
        assert_eq!(
            CatchphraseFrequency::from_cfg_name("1:3"),
            CatchphraseFrequency::Standard,
            "L-03：字符串比例写法作废，非法值回退标准档"
        );
        assert_eq!(CatchphraseFrequency::Off.as_cfg_name(), "off");
        assert_eq!(CatchphraseFrequency::High.as_cfg_name(), "high");
    }

    #[test]
    fn catchphrase_gate_windows_and_cooldown() {
        let cfg = character();
        let freq = cfg.catchphrase.frequency.clone();
        let mut gate = CatchphraseGate::from_cfg(&cfg.catchphrase);
        assert_eq!(gate.frequency(), CatchphraseFrequency::Standard);
        assert_eq!(gate.max_per_sentence(), cfg.catchphrase.max_per_sentence);

        // 秒 → 毫秒（R3 冷却 25s，故样本取 30s 步长）。
        let s = |n: i64| n * 1_000;
        // 标准档 = 窗口 3 句：第 1 句放行 → 第 2~4 句不放行 → 窗口滑出后第 5 句放行。
        assert!(gate.should_inject(s(0), &freq), "第 1 句：窗口空 + 无上次放行");
        assert!(!gate.should_inject(s(30), &freq), "R1：窗口内已有口头禅");
        assert!(!gate.should_inject(s(60), &freq), "R1：窗口内仍有口头禅");
        assert!(!gate.should_inject(s(90), &freq), "R1：窗口滑出前最后一句仍不放行");
        assert!(gate.should_inject(s(120), &freq), "R1：窗口滑出后重新放行");

        // R3 冷却：窗口已满足但间隔不足 → 仍不放行。
        let mut gate2 = CatchphraseGate::from_cfg(&cfg.catchphrase);
        assert!(gate2.should_inject(s(0), &freq));
        for n in 1..=freq.standard as i64 {
            assert!(!gate2.should_inject(s(n), &freq), "窗口内不放行");
        }
        let min_interval = cfg.catchphrase.min_interval_sec as i64;
        assert!(
            !gate2.should_inject((min_interval - 1) * 1_000, &freq),
            "R3：minIntervalSec 未到，即使窗口滑出也不放行"
        );
        assert!(gate2.should_inject(min_interval * 1_000, &freq), "R3：恰达间隔放行");

        // off 档：恒不放行。
        let mut off = CatchphraseGate::from_cfg(&cfg.catchphrase);
        off.set_frequency(CatchphraseFrequency::Off);
        assert!(!off.should_inject(s(0), &freq));

        // 关闭开关：恒不放行。
        let mut disabled = CatchphraseGate::from_cfg(&cfg.catchphrase);
        disabled.set_enabled(false);
        assert!(!disabled.is_enabled());
        assert!(!disabled.should_inject(s(0), &freq));
    }

    #[test]
    fn bubble_kind_maps_pools() {
        assert_eq!(BubbleKind::for_pool("begFood"), BubbleKind::Help);
        assert_eq!(BubbleKind::for_pool("begBath"), BubbleKind::Help);
        assert_eq!(BubbleKind::for_pool("outing"), BubbleKind::Postcard);
        assert_eq!(BubbleKind::for_pool("remind"), BubbleKind::Reminder);
        assert_eq!(BubbleKind::for_pool("happy"), BubbleKind::Chat);
        assert!(BubbleKind::Help.default_show_signature());
        assert!(BubbleKind::Postcard.default_show_signature());
        assert!(!BubbleKind::Chat.default_show_signature());
    }

    #[test]
    fn bubble_for_pool_renders_name_and_sets_fields() {
        let lib = library();
        let mut planner = BubblePlanner::from_library(&lib);
        let b = planner
            .bubble_for_pool(&lib, "idle", &PlaceholderVars::new(), 0, false)
            .expect("首次应命中");
        assert_eq!(b.kind, BubbleKind::Chat);
        assert_eq!(b.cooldown_key, "idle");
        assert_eq!(b.dwell_ms, BUBBLE_DWELL_DEFAULT_MS);
        assert!(!b.show_signature);
        assert!(b.actions.is_empty());
        assert!(!b.text.contains("{name}"), "占位符应在生产端渲染");

        // 带上 name 变量：idle 池首条（含 {name}）应被替换（抽到哪条由哈希决定，故只断言无残留）。
        let b2 = planner.bubble_for_pool(&lib, "bored", &vars(), 60_000, true).expect("过冷后应命中");
        assert!(b2.preempt);
        assert!(!b2.text.contains("{name}"));
    }

    #[test]
    fn help_bubble_carries_actions_and_signature() {
        let lib = library();
        let mut planner = BubblePlanner::from_library(&lib);
        let b = planner
            .bubble_for_pool(&lib, "begFood", &PlaceholderVars::new(), 0, false)
            .expect("首次应命中");
        assert_eq!(b.kind, BubbleKind::Help);
        assert!(b.show_signature, "求助类默认展示署名");
        assert_eq!(b.actions.len(), 2);
        assert_eq!(b.actions[0].id, "feed");
        assert_eq!(b.actions[1].id, "later");
        assert!(!b.actions[0].label.is_empty(), "按钮文案来自 lines.json.bubbleActions");

        // 同类求助气泡间隔 3min：`begBath` 需在过冷后另取一次（同池冷却亦共用 selector）。
        let bath = planner
            .bubble_for_pool(&lib, "begBath", &PlaceholderVars::new(), planner.help().base_ms(), false)
            .expect("过求助冷却后应命中");
        assert_eq!(bath.actions[0].id, "bath");
    }

    #[test]
    fn help_bubble_blocked_within_interval_across_pools() {
        let lib = library();
        let mut planner = BubblePlanner::from_library(&lib);
        let base = planner.help().base_ms();
        assert!(planner
            .bubble_for_pool(&lib, "begFood", &PlaceholderVars::new(), 0, false)
            .is_some());
        // 讨食与求洗澡共用同一求助冷却（`01 §6.16.3`：同类求助气泡间隔 ≥3min）。
        assert!(planner
            .bubble_for_pool(&lib, "begBath", &PlaceholderVars::new(), base - 1, false)
            .is_none());
    }

    #[test]
    fn help_cooldown_backs_off_and_caps() {
        let lib = library();
        let mut planner = BubblePlanner::from_library(&lib);
        let base = planner.help().base_ms();
        // 3 分钟：刚出过 → 冷却内不再出。
        assert!(planner
            .bubble_for_pool(&lib, "begFood", &PlaceholderVars::new(), 0, false)
            .is_some());
        assert!(planner
            .bubble_for_pool(&lib, "begBath", &PlaceholderVars::new(), base - 1, false)
            .is_none(),
            "求助气泡 3min 内不得再出");
        assert!(planner
            .bubble_for_pool(&lib, "begBath", &PlaceholderVars::new(), base, false)
            .is_some());

        // 连续拒绝 → 翻倍并封顶 15min。
        let mut t = base * 2;
        let mut seen = vec![planner.help().interval_ms()];
        for _ in 0..4 {
            planner.on_bubble_action("later", t);
            seen.push(planner.help().interval_ms());
            t += planner.help().interval_ms();
        }
        assert_eq!(seen[1], base * 2, "拒绝一次 → 翻倍");
        assert_eq!(seen[2], base * 4, "拒绝两次 → 再翻倍");
        assert_eq!(*seen.last().expect("非空"), planner.help().base_ms() * 5, "封顶 900s = 180×5");
        assert_eq!(planner.help().interval_ms(), 900_000);

        // 用户响应 → 回退基准。
        planner.on_bubble_action("feed", t);
        assert_eq!(planner.help().interval_ms(), planner.help().base_ms());
        assert_eq!(planner.help().next_allowed_ms(), t + planner.help().base_ms());
    }

    #[test]
    fn help_cooldown_is_ready_initially_and_unknown_action_is_noop() {
        let lib = library();
        let mut planner = BubblePlanner::from_library(&lib);
        assert!(planner.help().is_ready(0));
        assert_eq!(planner.help().next_allowed_ms(), 0);
        let interval = planner.help().interval_ms();
        planner.on_bubble_action("unknown", 1_000);
        assert_eq!(planner.help().interval_ms(), interval, "未知按钮不改变退避");
    }

    #[test]
    fn help_cooldown_factor_zero_is_clamped_to_one() {
        let c = HelpCooldown::new(180, 0, 900);
        assert_eq!(c.factor, 1, "factor 至少 1，避免拒绝反而不退避");
        assert_eq!(c.base_ms(), 180_000);
        assert_eq!(c.interval_ms(), 180_000);
    }

    #[test]
    fn bubble_for_level_uses_emotion_level_pool() {
        let lib = library();
        let cfg = crate::config::model::EmotionConfig::default();
        let mut planner = BubblePlanner::from_library(&lib);
        let b = planner
            .bubble_for_level(&lib, &cfg.levels, 2, &vars(), 0)
            .expect("L2 应命中 aggrieved 池");
        assert_eq!(b.cooldown_key, "aggrieved");
        // 越界档位 → None（不 panic）。
        assert!(planner
            .bubble_for_level(&lib, &cfg.levels, 99, &vars(), 0)
            .is_none());
    }

    #[test]
    fn clamp_dwell_bounds() {
        assert_eq!(clamp_dwell_ms(0), BUBBLE_DWELL_MIN_MS);
        assert_eq!(clamp_dwell_ms(u64::MAX), BUBBLE_DWELL_MAX_MS);
        assert_eq!(clamp_dwell_ms(4_200), 4_200);
    }

    #[test]
    fn selectors_come_from_config_not_hardcoded() {
        // 冻结值反证：改配置即改行为（`03 §0.5` 纪律 6）。
        let mut cfg = LinesConfig::default();
        cfg.selector.cooldown_sec = 7;
        cfg.selector.help_interval_sec = 60;
        cfg.selector.help_reject_backoff_factor = 3;
        cfg.selector.help_interval_max_sec = 600;
        let lib = LinesLibrary::from_config(cfg);
        let planner = BubblePlanner::from_library(&lib);
        assert_eq!(planner.selector().cooldown_sec(), 7);
        assert_eq!(planner.help().base_ms(), 60_000);
        let mut c = HelpCooldown::new(60, 3, 600);
        c.on_rejected(0);
        assert_eq!(c.interval_ms(), 180_000);
    }
}
