//! `emotion::busyness`：忙碌档位与 `busynessFactor` / `P_Cap` 求解（`02 §5.1` / §5.2 因子 F4）。
//!
//! ## 四档判定（`01 §6.11.2` F4 / §5.7 `busyness`）
//!
//! | 档位 | 因子 | `P_Cap` | 触发 |
//! |---|---|---|---|
//! | 深度忙碌 `Deep` | 0.3 | 12 | 高强度键鼠**或**深度应用（哈希命中）**或**全屏会议 |
//! | 一般忙碌 `Busy` | 0.5 | 28 | 中等强度输入 / 全屏前台 |
//! | 轻度使用 `Light` | 1.0 | 120 | 其余（含**无任何可用样本**） |
//! | 摸鱼 `Slack` | 1.2 | 120 | 低强度输入 + 摸鱼应用（浏览器 / 播放器 / 游戏）哈希命中 |
//!
//! ## 隐私口径（Q-18 / `02 §5.6`）
//!
//! 前台进程**只以 FNV-1a64 类别哈希**进入判定（`ActivitySample::foreground_hash`），
//! 白名单同样**只存哈希字符串**（支持 `0x…` 十六进制或十进制）；本模块不接触、不缓存、
//! 不记录任何进程名明文。出厂 `deepApps` / `slackApps` 均为**空集** ⇒ 类别判定不参与、
//! 档位完全由输入强度决定（隐私优先的保守默认，登记见台账口径登记）。
//!
//! ## 平滑（`busyness.smoothingSec`）
//!
//! 因子按**定长窗口移动平均**平滑（默认 20s ≈ 20 个 1Hz 样本），避免击键间歇造成档位抖动。
//! 窗口首个样本会**填满整窗**（而不是从 0 爬升），保证「恒定档位」下平滑输出与瞬时值
//! 逐字相等 —— 这是 AC-16 / AC-17 标定算例可复算的前提。
//!
//! ## 时间纪律（C3）
//!
//! 零时钟：`now_ms` / 样本均由调用方注入；不读墙钟、不做逐 tick 累加。

use crate::config::model::BusynessCfg;
use crate::perception::ActivitySample;

/// 忙碌档位数（四档）。
pub const BUSYNESS_LEVEL_COUNT: usize = 4;

/// 忙碌档位（`02 §5.7` 四档，ID 与配置键前缀一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum BusynessLevel {
    /// 深度忙碌（因子 0.3 / 封顶 12）。
    Deep,
    /// 一般忙碌（因子 0.5 / 封顶 28）。
    Busy,
    /// 轻度使用（因子 1.0 / 封顶 120）——**默认档**，也是「无样本」的退化档。
    #[default]
    Light,
    /// 摸鱼（因子 1.2 / 封顶 120）。
    Slack,
}

impl BusynessLevel {
    /// 四档全量（遍历 / 参数化测试用）。
    pub const ALL: [Self; BUSYNESS_LEVEL_COUNT] =
        [Self::Deep, Self::Busy, Self::Light, Self::Slack];

    /// 稳定 ID（诊断日志 / 快照投影用；`02 §5.6` ⑥ 只记档位不记原始量）。
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Deep => "deep",
            Self::Busy => "busy",
            Self::Light => "light",
            Self::Slack => "slack",
        }
    }

    /// ID → 档位（未知返回 `None`，配置 / 存档容错）。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.id() == id)
    }

    /// 该档是否「用户明显在忙」（深度 / 一般）——供 Mood 活跃期口径与诊断使用。
    #[must_use]
    pub const fn is_busy(self) -> bool {
        matches!(self, Self::Deep | Self::Busy)
    }

    /// 该档是否「用户与电脑有互动」（非离场口径的活跃判定，`02 §5.7 mood`）。
    #[must_use]
    pub const fn is_interactive(self) -> bool {
        matches!(self, Self::Light | Self::Slack)
    }
}

/// 忙碌求解结果（因子 + 封顶 + 档位）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BusynessOutput {
    /// 平滑后的 `busynessFactor`。
    pub factor: f32,
    /// 本档 `P_Cap`（12 / 28 / 120）。
    pub cap: f32,
    /// 瞬时档位（未平滑，供诊断 / AC-17 前置）。
    pub level: BusynessLevel,
}

impl Default for BusynessOutput {
    fn default() -> Self {
        Self { factor: 1.0, cap: 120.0, level: BusynessLevel::Light }
    }
}

/// 档位 → `P_Cap`（`02 §5.7`：12 / 28 / 120）。
#[must_use]
pub fn cap_of(level: BusynessLevel, cfg: &BusynessCfg) -> f32 {
    match level {
        BusynessLevel::Deep => cfg.cap_deep as f32,
        BusynessLevel::Busy => cfg.cap_busy as f32,
        BusynessLevel::Light | BusynessLevel::Slack => cfg.cap_free as f32,
    }
}

/// 档位 → 因子（`02 §5.7`：0.3 / 0.5 / 1.0 / 1.2）。
#[must_use]
pub fn factor_of(level: BusynessLevel, cfg: &BusynessCfg) -> f32 {
    match level {
        BusynessLevel::Deep => cfg.factor_deep,
        BusynessLevel::Busy => cfg.factor_busy,
        BusynessLevel::Light => cfg.factor_light,
        BusynessLevel::Slack => cfg.factor_slack,
    }
}

/// 解析白名单项为哈希（`0x…` 十六进制或十进制；非法项返回 `None` 并跳过）。
#[must_use]
pub fn parse_hash(spec: &str) -> Option<u64> {
    let s = spec.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

/// 哈希是否命中白名单（**只看哈希**，Q-18；空白名单恒 `false`）。
#[must_use]
pub fn hits_whitelist(hash: u64, list: &[String]) -> bool {
    list.iter().any(|spec| parse_hash(spec) == Some(hash))
}

/// 瞬时档位判定（纯函数，冻结顺序：深度 → 摸鱼 → 一般 → 轻度）。
///
/// `None` / 全空样本 ⇒ [`BusynessLevel::Light`]（不可用即「视为无信息」，
/// **不得**把 `None` 当成 0 强度参与其它分支）。
#[must_use]
pub fn classify(sample: Option<&ActivitySample>, cfg: &BusynessCfg) -> BusynessLevel {
    let Some(s) = sample else {
        return BusynessLevel::Light;
    };
    if s.is_empty() {
        return BusynessLevel::Light;
    }
    let kps = s.key_kps.unwrap_or(0.0).max(0.0);
    let cpm = s.clicks_per_min.unwrap_or(0.0).max(0.0);
    let mpm = s.move_px_per_min.unwrap_or(0.0).max(0.0);
    let deep_app = s.foreground_hash.is_some_and(|h| hits_whitelist(h, &cfg.deep_apps));
    let slack_app = s.foreground_hash.is_some_and(|h| hits_whitelist(h, &cfg.slack_apps));

    // ① 深度：高强度键鼠 / 深度应用 / 全屏会议。
    if deep_app
        || kps >= cfg.kps_deep_threshold
        || cpm >= cfg.clicks_per_min_deep as f32
        || mpm >= cfg.move_px_per_min_deep as f32
        || s.fullscreen == Some(true)
    {
        return BusynessLevel::Deep;
    }
    // ② 摸鱼：低强度 + 摸鱼应用（B-3「你明明在看屏幕…却不看我」）。
    if slack_app {
        return BusynessLevel::Slack;
    }
    // ③ 一般忙碌：中等强度输入。
    if kps >= cfg.kps_busy_threshold {
        return BusynessLevel::Busy;
    }
    BusynessLevel::Light
}

/// 因子移动平均平滑器（`busyness.smoothingSec` 定长窗口）。
#[derive(Clone, Debug, Default)]
pub struct BusynessSmoother {
    /// 窗口内因子样本（容量 = `smoothingSec`，最多 [`BusynessSmoother::MAX_WINDOW`]）。
    window: Vec<f32>,
    /// 窗口容量（随配置；`0` ⇒ 不平滑）。
    capacity: usize,
    /// 窗口起点时刻（绝对锚定；`None` = 未初始化）。
    start_ms: Option<i64>,
}

impl BusynessSmoother {
    /// 窗口容量上限（防御异常配置造成无界内存）。
    pub const MAX_WINDOW: usize = 600;

    /// 新建（容量按 `smoothingSec` 取 1Hz 样本数，钳 `[1, MAX_WINDOW]`）。
    #[must_use]
    pub fn new(cfg: &BusynessCfg) -> Self {
        Self { window: Vec::new(), capacity: Self::capacity_of(cfg), start_ms: None }
    }

    /// 由配置取窗口容量。
    #[must_use]
    pub fn capacity_of(cfg: &BusynessCfg) -> usize {
        (cfg.smoothing_sec as usize).clamp(1, Self::MAX_WINDOW)
    }

    /// 配置热更新后同步容量（窗口超容即裁掉最旧样本）。
    pub fn resize(&mut self, cfg: &BusynessCfg) {
        self.capacity = Self::capacity_of(cfg);
        while self.window.len() > self.capacity {
            self.window.remove(0);
        }
    }

    /// 当前窗口样本数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.window.len()
    }

    /// 窗口是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.window.is_empty()
    }

    /// 推入一个瞬时因子，返回平滑值。
    ///
    /// 首个样本**填满整窗**：保证恒定档位下平滑输出与瞬时值逐字相等（AC-16/17 可复算）。
    pub fn push(&mut self, factor: f32, now_ms: i64) -> f32 {
        let v = if factor.is_finite() { factor } else { 1.0 };
        if self.window.is_empty() && self.start_ms.is_none() {
            self.start_ms = Some(now_ms);
            self.window = vec![v; self.capacity];
            return v;
        }
        if self.window.len() >= self.capacity {
            self.window.remove(0);
        }
        self.window.push(v);
        let sum: f32 = self.window.iter().sum();
        sum / self.window.len() as f32
    }
}

/// 忙碌求解器（持有平滑状态；判定本身无状态）。
#[derive(Clone, Debug)]
pub struct BusynessSolver {
    smoother: BusynessSmoother,
}

impl BusynessSolver {
    /// 由配置构造。
    #[must_use]
    pub fn new(cfg: &BusynessCfg) -> Self {
        Self { smoother: BusynessSmoother::new(cfg) }
    }

    /// 配置热更新（平滑窗口容量随之调整）。
    pub fn sync_cfg(&mut self, cfg: &BusynessCfg) {
        self.smoother.resize(cfg);
    }

    /// 只读：平滑窗口样本数（诊断 / 测试）。
    #[must_use]
    pub fn window_len(&self) -> usize {
        self.smoother.len()
    }

    /// 求解：瞬时档位 → 平滑因子 → 封顶。
    ///
    /// **`activity_sensing == false`（隐私关闭 / Q-18）时调用方应传 `None`**，
    /// 此时恒为轻度档、因子 1.0、封顶 120（退化为纯时间模型，`02 §5.6` ④）。
    pub fn solve(
        &mut self,
        sample: Option<&ActivitySample>,
        cfg: &BusynessCfg,
        now_ms: i64,
    ) -> BusynessOutput {
        let level = classify(sample, cfg);
        let factor = self.smoother.push(factor_of(level, cfg), now_ms);
        BusynessOutput { factor, cap: cap_of(level, cfg), level }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BusynessCfg {
        BusynessCfg::default()
    }

    fn sample(kps: f32) -> ActivitySample {
        ActivitySample { key_kps: Some(kps), ..ActivitySample::default() }
    }

    #[test]
    fn no_sample_degrades_to_light() {
        let c = cfg();
        assert_eq!(classify(None, &c), BusynessLevel::Light);
        assert_eq!(classify(Some(&ActivitySample::default()), &c), BusynessLevel::Light);
    }

    #[test]
    fn deep_by_key_rate() {
        let c = cfg();
        assert_eq!(classify(Some(&sample(2.5)), &c), BusynessLevel::Deep);
        assert_eq!(classify(Some(&sample(2.49)), &c), BusynessLevel::Busy);
    }

    #[test]
    fn busy_by_key_rate() {
        let c = cfg();
        assert_eq!(classify(Some(&sample(1.0)), &c), BusynessLevel::Busy);
        assert_eq!(classify(Some(&sample(0.99)), &c), BusynessLevel::Light);
    }

    #[test]
    fn fullscreen_is_deep() {
        let c = cfg();
        let s = ActivitySample { fullscreen: Some(true), ..ActivitySample::default() };
        assert_eq!(classify(Some(&s), &c), BusynessLevel::Deep);
    }

    #[test]
    fn deep_app_whitelist_by_hash_only() {
        let mut c = cfg();
        c.deep_apps = vec!["0xdeadbeef".to_string()];
        let s = ActivitySample { foreground_hash: Some(0xdead_beef), ..ActivitySample::default() };
        assert_eq!(classify(Some(&s), &c), BusynessLevel::Deep);
        // 未命中 → 退化轻度（白名单空 / 不命中均不参与）
        let s2 = ActivitySample { foreground_hash: Some(0x1234), ..ActivitySample::default() };
        assert_eq!(classify(Some(&s2), &c), BusynessLevel::Light);
    }

    #[test]
    fn slack_app_low_input_is_slack() {
        let mut c = cfg();
        c.slack_apps = vec!["42".to_string()];
        let s = ActivitySample { foreground_hash: Some(42), ..ActivitySample::default() };
        assert_eq!(classify(Some(&s), &c), BusynessLevel::Slack);
        assert!((factor_of(BusynessLevel::Slack, &c) - 1.2).abs() < 1e-6);
        assert!((cap_of(BusynessLevel::Slack, &c) - 120.0).abs() < 1e-6);
    }

    #[test]
    fn deep_beats_slack_when_whitelists_overlap() {
        let mut c = cfg();
        c.slack_apps = vec!["7".to_string()];
        c.deep_apps = vec!["7".to_string()];
        let s = ActivitySample { foreground_hash: Some(7), ..ActivitySample::default() };
        assert_eq!(classify(Some(&s), &c), BusynessLevel::Deep);
    }

    #[test]
    fn caps_match_frozen_table() {
        let c = cfg();
        assert_eq!(cap_of(BusynessLevel::Deep, &c), 12.0);
        assert_eq!(cap_of(BusynessLevel::Busy, &c), 28.0);
        assert_eq!(cap_of(BusynessLevel::Light, &c), 120.0);
    }

    #[test]
    fn empty_whitelists_never_match() {
        let c = cfg();
        assert!(!hits_whitelist(0, &c.deep_apps));
        assert!(parse_hash("0x0").is_some());
        assert!(parse_hash("abc").is_none());
        assert_eq!(parse_hash("0x10"), Some(16));
        assert_eq!(parse_hash(" 16 "), Some(16));
        assert_eq!(parse_hash("0xDEADBEEF"), Some(0xdead_beef));
        // `from_str_radix` 不接受下划线分隔：带下划线的写法按非法项跳过
        assert!(parse_hash("0xdead_beef").is_none());
    }

    #[test]
    fn smoother_fills_window_on_first_sample() {
        let c = cfg();
        let mut s = BusynessSolver::new(&c);
        let out = s.solve(Some(&sample(3.0)), &c, 0);
        assert_eq!(out.level, BusynessLevel::Deep);
        assert!((out.factor - 0.3).abs() < 1e-6, "首样本须填满整窗，不得从 0 爬升");
        assert_eq!(s.window_len(), BusynessSmoother::capacity_of(&c));
    }

    #[test]
    fn smoother_averages_level_change() {
        let c = cfg();
        let mut s = BusynessSolver::new(&c);
        // 前 20 拍恒定轻度 → 1.0
        for i in 0..20 {
            let _ = s.solve(None, &c, i * 1000);
        }
        // 切深度：第 21 拍均值为 (19×1.0 + 0.3)/20
        let out = s.solve(Some(&sample(3.0)), &c, 20_000);
        let want = (19.0 * 1.0 + 0.3) / 20.0;
        assert!((out.factor - want).abs() < 1e-5, "got {} want {want}", out.factor);
        assert_eq!(out.level, BusynessLevel::Deep, "档位是瞬时量，不受平滑影响");
    }

    #[test]
    fn smoothing_window_is_bounded() {
        let mut c = cfg();
        c.smoothing_sec = 0;
        let mut s = BusynessSolver::new(&c);
        for i in 0..5 {
            let _ = s.solve(None, &c, i * 1000);
        }
        assert_eq!(s.window_len(), 1, "容量钳到下限 1");
        c.smoothing_sec = 100_000;
        s.sync_cfg(&c);
        assert_eq!(s.window_len(), 1);
        let _ = s.solve(None, &c, 9_000);
        assert!(s.window_len() <= BusynessSmoother::MAX_WINDOW);
    }

    #[test]
    fn level_ids_roundtrip_and_cover_all() {
        for l in BusynessLevel::ALL {
            assert_eq!(BusynessLevel::from_id(l.id()), Some(l));
        }
        assert_eq!(BusynessLevel::from_id("nope"), None);
    }
}
