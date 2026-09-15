//! 配置中心：`ConfigService::load_all` —— 外置 JSON 的加载、合并与校验。
//!
//! 份数：**七份**（S1-M5 六份 + S5-M5 新增 `schedule.json`）。
//!
//! 时序锚点：`02 §6.1`（`load_all` 配置加载 + 默认合并）；R19 兜底（`02 行 2434`）：
//! 加载失败 → 内置默认启动，不崩。字段缺省由 serde `default` 补齐并记 warning
//! （老包升级不崩，`03` S1-M5 要点 1）。
//!
//! 错误分界：
//!   - **文件级问题**（缺失 / 读取失败 / 解析失败 / 顶层字段缺失或未知）→
//!     降级为该文件的内置默认 + `tracing::warn` + 返回值 `warnings` 通道，不 `Err`；
//!   - **结构级问题**（needs.coupling 规则成环 / 动作 ID 空或重复）→ 返回
//!     `Err(ConfigError)`，由调用方决定降级默认启动（`02 §5.10`：新增成环规则在
//!     配置加载阶段即失败，CI 可断言）。
//!
//! 测试资源路径：`env!("CARGO_MANIFEST_DIR")` 上溯三级到工程根（无盘符字面量，C1）。

pub mod model;

pub use model::{
    ActionCfg, ActionsConfig, AnimationConfig, CharacterConfig, DegradeCfg, DegradeCpuCfg,
    DegradeFpsCfg, DegradeMemoryCfg, EmotionConfig, NeedsConfig, ScheduleConfig, SettingsConfig,
};

use std::path::Path;

use serde::de::DeserializeOwned;
use thiserror::Error;
use tracing::warn;

// ---------------------------------------------------------------------------
// 错误与捆绑
// ---------------------------------------------------------------------------

/// 配置加载的结构级错误（`#[non_exhaustive]`，`02 §7.4`）。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// needs.coupling 规则 `target → when` 引用成环（`02 §5.10` 构建期拓扑检查）。
    #[error("needs.coupling 规则引用成环：{chain}")]
    CouplingCycle {
        /// 成环链路（如 `satiety -> moodDecay -> mood -> satiety`）。
        chain: String,
    },
    /// 其他无效配置（如动作 ID 空 / 重复）。
    #[error("配置无效（{file}）：{reason}")]
    InvalidConfig {
        /// 出错文件名。
        file: String,
        /// 原因说明。
        reason: String,
    },
}

/// 配置的捆绑结果（S5-M5 起 **七份**：新增 `schedule.json`）。
#[derive(Debug, Clone, Default)]
pub struct ConfigBundle {
    /// settings.json。
    pub settings: SettingsConfig,
    /// character.json。
    pub character: CharacterConfig,
    /// actions.json。
    pub actions: ActionsConfig,
    /// emotion.json。
    pub emotion: EmotionConfig,
    /// needs.json。
    pub needs: NeedsConfig,
    /// animation.json。
    pub animation: AnimationConfig,
    /// schedule.json（`01 FR-10`；S5-M5 交付）。
    pub schedule: ScheduleConfig,
}

impl ConfigBundle {
    /// 全部 53 条动作元数据（含 disabled）。
    pub fn actions(&self) -> &[ActionCfg] {
        &self.actions.actions
    }

    /// 已启用动作（批次 A：`disabled == false`）。
    pub fn enabled_actions(&self) -> impl Iterator<Item = &ActionCfg> {
        self.actions.actions.iter().filter(|a| !a.disabled)
    }
}

/// 配置服务门面（无状态；加载结果每次独立返回）。
#[derive(Debug, Clone, Copy, Default)]
pub struct ConfigService;

impl ConfigService {
    /// 读取 `config_dir` 下的七份 JSON，与内置默认合并。
    ///
    /// 语义：
    ///   1. 文件缺失 / 读取失败 / JSON 解析失败 → 使用该文件的内置 `Default`，
    ///      `tracing::warn` 并向 `warnings` 追加一条（R19：不崩）；
    ///   2. 顶层字段缺失（老包升级）→ serde `default` 补齐 + warning；
    ///      顶层未知字段（含拼写错误）→ 忽略 + warning（避免「改 JSON 行为即变」
    ///      因拼写错误静默失效）；
    ///   3. needs.coupling 规则成环 / 动作 ID 空或重复 → 返回 `Err`（调用方降级默认）。
    ///
    /// 返回：`(捆绑配置, 告警列表)`。
    pub fn load_all(config_dir: &Path) -> Result<(ConfigBundle, Vec<String>), ConfigError> {
        let mut warnings: Vec<String> = Vec::new();

        let settings = load_one::<SettingsConfig>(
            config_dir,
            "settings.json",
            SETTINGS_TOP_FIELDS,
            &mut warnings,
        );
        let character = load_one::<CharacterConfig>(
            config_dir,
            "character.json",
            CHARACTER_TOP_FIELDS,
            &mut warnings,
        );
        let actions =
            load_one::<ActionsConfig>(config_dir, "actions.json", ACTIONS_TOP_FIELDS, &mut warnings);
        let emotion = load_one::<EmotionConfig>(
            config_dir,
            "emotion.json",
            EMOTION_TOP_FIELDS,
            &mut warnings,
        );
        let needs =
            load_one::<NeedsConfig>(config_dir, "needs.json", NEEDS_TOP_FIELDS, &mut warnings);
        let animation = load_one::<AnimationConfig>(
            config_dir,
            "animation.json",
            ANIMATION_TOP_FIELDS,
            &mut warnings,
        );
        let schedule = load_one::<ScheduleConfig>(
            config_dir,
            "schedule.json",
            SCHEDULE_TOP_FIELDS,
            &mut warnings,
        );

        validate_actions(&actions)?;
        check_coupling_cycles(&needs)?;

        Ok((
            ConfigBundle { settings, character, actions, emotion, needs, animation, schedule },
            warnings,
        ))
    }
}

// ---------------------------------------------------------------------------
// 各文件顶层字段清单（camelCase；与 model.rs 字段一一对应）
// ---------------------------------------------------------------------------

/// settings.json 顶层字段。
pub const SETTINGS_TOP_FIELDS: &[&str] = &[
    "version", "appearance", "audio", "behavior", "roam", "interaction", "emotion", "privacy",
];
/// character.json 顶层字段。
pub const CHARACTER_TOP_FIELDS: &[&str] = &[
    "version", "defaultName", "catchphrase", "linePools", "renderer", "slots", "expressions",
    "personalityText",
];
/// actions.json 顶层字段。
pub const ACTIONS_TOP_FIELDS: &[&str] = &["version", "actions"];
/// emotion.json 顶层字段。
pub const EMOTION_TOP_FIELDS: &[&str] = &[
    "version", "dimensions", "sensitivity", "presence", "busyness", "rhythm", "needs", "rough",
    "personality", "adapt", "thresholds", "confirm", "relief", "mood", "inertia", "levels",
    "offline", "activity", "coax",
];
/// needs.json 顶层字段。
pub const NEEDS_TOP_FIELDS: &[&str] =
    &["version", "dimensions", "bands", "eventDeltas", "bath", "coupling"];
/// animation.json 顶层字段。
pub const ANIMATION_TOP_FIELDS: &[&str] = &[
    "version", "easing", "fadeMs", "physics", "micro", "breath", "blink", "gaze", "squash",
    "particles", "degrade",
];
/// schedule.json 顶层字段（S5-M5：`01 FR-10` 提醒默认间隔 + 勿扰默认行为）。
pub const SCHEDULE_TOP_FIELDS: &[&str] = &["version", "reminders", "doNotDisturb"];

// ---------------------------------------------------------------------------
// 内部实现
// ---------------------------------------------------------------------------

/// 加载单份配置：缺失 / 损坏 → 内置默认 + 告警；顶层字段差异 → 告警。
///
/// 告警粒度为**顶层字段**（深层字段缺失由 serde 直接补默认，不逐层告警）。
fn load_one<T>(dir: &Path, file: &str, known_top_fields: &[&str], warnings: &mut Vec<String>) -> T
where
    T: DeserializeOwned + Default,
{
    let path = dir.join(file);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            warn!("配置 {} 读取失败（{}），使用内置默认值", file, err);
            warnings.push(format!("{file}: 读取失败（{err}），已用内置默认值"));
            return T::default();
        }
    };

    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            warn!("配置 {} JSON 解析失败（{}），使用内置默认值", file, err);
            warnings.push(format!("{file}: JSON 解析失败（{err}），已用内置默认值"));
            return T::default();
        }
    };

    if let Some(map) = value.as_object() {
        for field in known_top_fields {
            if !map.contains_key(*field) {
                warn!("配置 {} 缺少顶层字段 {}，已用默认值补齐", file, field);
                warnings.push(format!("{file}: 缺少顶层字段 {field}，已用默认值补齐"));
            }
        }
        for key in map.keys() {
            if !known_top_fields.contains(&key.as_str()) {
                warn!("配置 {} 存在未知顶层字段 {}，已忽略（请核对拼写）", file, key);
                warnings.push(format!("{file}: 未知顶层字段 {key}，已忽略"));
            }
        }
    }

    match serde_json::from_value::<T>(value) {
        Ok(parsed) => parsed,
        Err(err) => {
            warn!("配置 {} 反序列化失败（{}），使用内置默认值", file, err);
            warnings.push(format!("{file}: 反序列化失败（{err}），已用内置默认值"));
            T::default()
        }
    }
}

/// 动作元数据校验：ID 非空且不重复（运行时按 ID 查表，重复/空 ID 属结构级错误）。
fn validate_actions(actions: &ActionsConfig) -> Result<(), ConfigError> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for action in &actions.actions {
        if action.id.is_empty() {
            return Err(ConfigError::InvalidConfig {
                file: "actions.json".to_string(),
                reason: "存在空的动作 ID".to_string(),
            });
        }
        if !seen.insert(action.id.as_str()) {
            return Err(ConfigError::InvalidConfig {
                file: "actions.json".to_string(),
                reason: format!("动作 ID 重复：{}", action.id),
            });
        }
    }
    Ok(())
}

/// target 输出量经求解后**影响**的 when 源维度（`02 §5.10`）。
///
/// 仅 `moodDecay → mood`（S3：Mood 衰减 × moodDecayMul）与
/// `energyRecover → energy`（S2：Energy 恢复 × energyRecoverMul）会把修正
/// 回写到快照维度；其余 target（dispatch / speed / jobReward / ...）不回写六维。
fn target_influence(target: &str) -> Option<&'static str> {
    match target {
        "moodDecay" => Some("mood"),
        "energyRecover" => Some("energy"),
        _ => None,
    }
}

/// 解析 `when` 条件表达式的**源维度名**（取比较符左侧标识符，如 `satiety<20` → `satiety`）。
fn when_source_dim(when: &str) -> Option<String> {
    let ops = ["<=", ">=", "==", "!=", "<", ">"];
    for op in ops {
        if let Some(pos) = when.find(op) {
            let left = when[..pos].trim();
            if !left.is_empty() {
                return Some(left.to_string());
            }
        }
    }
    None
}

/// needs.coupling 构建期拓扑检查（`02 §5.10`：禁止 `target → source` 成环）。
///
/// 图的节点 = 快照维度（satiety/cleanliness/energy/mood/affinity）与 target 原名；
/// 边 = `when 源维度 → target`（规则施加修正），及 `target → target_influence 维度`
/// （修正回写快照）。存在环即返回 `Err`。
pub fn check_coupling_cycles(needs: &NeedsConfig) -> Result<(), ConfigError> {
    // 邻接表：节点名 → 后继集合。
    let mut edges: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    let mut add_edge = |from: &str, to: &str| {
        edges.entry(from.to_string()).or_default().insert(to.to_string());
    };

    for rule in &needs.coupling.rules {
        let Some(source) = when_source_dim(&rule.when) else {
            continue; // 无法解析的条件跳过（不误报）
        };
        add_edge(&source, &rule.target);
        if let Some(dim) = target_influence(&rule.target) {
            add_edge(&rule.target, dim);
        }
    }

    // DFS 三色标环检测（0 = 白，1 = 灰，2 = 黑）。
    let mut color: std::collections::HashMap<String, u8> = std::collections::HashMap::new();
    let mut stack: Vec<String> = Vec::new();

    for root in edges.keys().cloned().collect::<Vec<_>>() {
        if color.get(&root).copied().unwrap_or(0) != 0 {
            continue;
        }
        stack.clear();
        stack.push(root.clone());
        while let Some(node) = stack.last().cloned() {
            match color.get(&node).copied().unwrap_or(0) {
                0 => {
                    color.insert(node.clone(), 1);
                    if let Some(next) = edges.get(&node) {
                        for succ in next.clone() {
                            match color.get(&succ).copied().unwrap_or(0) {
                                1 => {
                                    // 成环：从栈中截取环链路。
                                    let start = stack.iter().position(|n| *n == succ).unwrap_or(0);
                                    let mut chain: Vec<String> = stack[start..].to_vec();
                                    chain.push(succ);
                                    return Err(ConfigError::CouplingCycle {
                                        chain: chain.join(" -> "),
                                    });
                                }
                                0 => stack.push(succ),
                                _ => {}
                            }
                        }
                    }
                }
                1 => {
                    color.insert(node, 2);
                    stack.pop();
                }
                _ => {
                    stack.pop();
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::*;

    /// 工程根：dp-core 位于 crates/dp-core，上溯三级（无盘符字面量，C1）。
    fn repo_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../")
    }

    fn resources_config_dir() -> std::path::PathBuf {
        repo_root().join("resources").join("config")
    }

    fn resources_schema_dir() -> std::path::PathBuf {
        repo_root().join("resources").join("schema")
    }

    /// 临时目录（按测试名隔离；C3：不使用时钟，目录名用测试名区分）。
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("dp-core-s1m5-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败（测试前置）");
        dir
    }

    /// 把工程根七份配置复制到临时目录，便于逐项改写。
    fn copy_defaults(dir: &Path) {
        for file in [
            "settings.json",
            "character.json",
            "actions.json",
            "emotion.json",
            "needs.json",
            "animation.json",
            "schedule.json",
        ] {
            let src = resources_config_dir().join(file);
            let dst = dir.join(file);
            std::fs::copy(&src, &dst).expect("复制默认配置失败（测试前置）");
        }
    }

    #[test]
    fn load_all_reads_seven_files_from_resources() {
        let (bundle, warnings) = ConfigService::load_all(&resources_config_dir())
            .expect("七份默认配置应能加载成功");

        // 顶层字段与 JSON 完全一致时不应产生告警。
        assert!(warnings.is_empty(), "默认包不应产生告警：{warnings:?}");

        // settings：K-3 RV-18 重力单一真源 + §5.23 + Q-18。
        assert_eq!(bundle.settings.interaction.gravity_px_per_sec2, 2400.0);
        assert_eq!(bundle.settings.interaction.unavailable_presence_factor, 0.05);
        assert!(bundle.settings.interaction.tray_fallback_enabled);
        assert_eq!(bundle.settings.interaction.unreachable_natural_floor_level, 4);
        assert_eq!(bundle.settings.interaction.unreachable_hold_sec, 180);
        assert_eq!(bundle.settings.interaction.unreachable_no_negative_sec, 7200);
        assert!(bundle.settings.privacy.activity_sensing);
        assert_eq!(bundle.settings.appearance.language, "zh-CN");
        assert_eq!(bundle.settings.appearance.scale_min_percent, 50);
        assert_eq!(bundle.settings.appearance.scale_max_percent, 200);
        assert_eq!(bundle.settings.appearance.scale_step_percent, 10);
        assert_eq!(bundle.settings.roam.decision_interval_sec, [5, 30]);
        assert_eq!(bundle.settings.roam.cursor_avoid_radius_px, 150);

        // character：C2 以码点构造期望值；L-02 count=13。
        let expected_name: String = ['\u{5FC3}', '\u{6708}', '\u{72D0}'].iter().collect();
        assert_eq!(bundle.character.default_name, expected_name);
        assert_eq!(bundle.character.line_pools.count, 13);
        assert_eq!(bundle.character.line_pools.keys.len(), 13);

        // needs：C-01~C-16 共 16 条 + smoothSec=5。
        assert_eq!(bundle.needs.coupling.rules.len(), 16);
        assert_eq!(bundle.needs.coupling.smooth_sec, 5);
        assert_eq!(bundle.needs.dimensions.satiety.decay_per_min, -0.05);

        // emotion：阈值 5/15/30/60/120 + 六档。
        assert_eq!(bundle.emotion.thresholds.l1, 5);
        assert_eq!(bundle.emotion.thresholds.l5, 120);
        assert_eq!(bundle.emotion.levels.len(), 6);
        assert_eq!(bundle.emotion.sensitivity.rate_clamp.max, 1.6);

        // animation：本阶段 physics.parts 为空数组。
        assert!(bundle.animation.physics.parts.is_empty());
        assert!(bundle.animation.physics.enabled);
        assert_eq!(bundle.animation.fade_ms.default, 200);

        // schedule（S5-M5）：FR-10-2 默认 45min 间隔 + FR-10-4 勿扰默认关。
        assert!(bundle.schedule.reminders.sedentary_enabled);
        assert_eq!(bundle.schedule.reminders.sedentary_interval_min, 45);
        assert_eq!(bundle.schedule.reminders.water_interval_min, 45);
        assert_eq!(bundle.schedule.reminders.interval_min_min, 15);
        assert_eq!(bundle.schedule.reminders.interval_max_min, 180);
        assert!(bundle.schedule.reminders.ack_resets_timer);
        assert!(!bundle.schedule.do_not_disturb.default_on);
        assert!(bundle.schedule.do_not_disturb.pause_bubbles);
        assert!(bundle.schedule.do_not_disturb.keep_idle_anim);
        assert!(bundle.schedule.do_not_disturb.mute_audio);

        // actions：53 条全量；批次 A 29 条启用、B/C 24 条禁用。
        assert_eq!(bundle.actions().len(), 53);
        assert_eq!(bundle.enabled_actions().count(), 29);
        assert_eq!(bundle.actions().iter().filter(|a| a.disabled).count(), 24);
        // R-A 演出类：ACT-N-02/07/09/10/12/14。
        let performances: Vec<&str> = bundle
            .actions()
            .iter()
            .filter(|a| a.performance)
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(
            performances,
            ["ACT-N-02", "ACT-N-07", "ACT-N-09", "ACT-N-10", "ACT-N-12", "ACT-N-14"]
        );
    }

    /// AC①（03 卡片行 478）：Given emotion.json 改一个阈值；When 加载；Then 读出值随之变化。
    #[test]
    fn emotion_threshold_change_is_visible_via_load_all() {
        let dir = temp_dir("emotion-threshold-change");
        copy_defaults(&dir);
        let path = dir.join("emotion.json");
        let text = std::fs::read_to_string(&path).expect("读取默认 emotion.json 失败");
        let patched = text.replace("\"l3\": 30", "\"l3\": 45");
        assert_ne!(text, patched, "默认 emotion.json 应包含 \"l3\": 30");
        std::fs::write(&path, patched).expect("写回 emotion.json 失败");

        let (bundle, _) =
            ConfigService::load_all(&dir).expect("修改后的配置应能加载成功");
        assert_eq!(bundle.emotion.thresholds.l3, 45);
    }

    #[test]
    fn missing_files_fall_back_to_builtin_defaults() {
        let dir = temp_dir("missing-files");
        let (bundle, warnings) =
            ConfigService::load_all(&dir).expect("空目录应降级默认而非报错");
        assert_eq!(warnings.len(), 7, "七份缺失文件各记一条告警：{warnings:?}");
        assert_eq!(bundle.emotion.thresholds.l4, 60);
        assert_eq!(bundle.settings.interaction.gravity_px_per_sec2, 2400.0);
        assert_eq!(bundle.needs.coupling.rules.len(), 16);
        assert_eq!(bundle.schedule.reminders.sedentary_interval_min, 45);
        assert!(bundle.actions().is_empty());
        // 内置默认名同样以码点构造比对（C2）。
        let expected_name: String = ['\u{5FC3}', '\u{6708}', '\u{72D0}'].iter().collect();
        assert_eq!(bundle.character.default_name, expected_name);
    }

    #[test]
    fn corrupt_json_falls_back_to_defaults() {
        let dir = temp_dir("corrupt-json");
        copy_defaults(&dir);
        std::fs::write(dir.join("animation.json"), "{ not valid json ").expect("写坏文件失败");
        let (bundle, warnings) = ConfigService::load_all(&dir).expect("损坏文件应降级默认");
        assert_eq!(bundle.animation.fade_ms.default, 200);
        assert!(warnings.iter().any(|w| w.starts_with("animation.json:")));
    }

    #[test]
    fn missing_top_field_is_reported_and_defaulted() {
        let dir = temp_dir("missing-top-field");
        copy_defaults(&dir);
        let path = dir.join("emotion.json");
        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path).expect("读取 emotion.json 失败"),
        )
        .expect("默认 emotion.json 应为合法 JSON");
        let mut map = value.as_object().expect("顶层应为对象").clone();
        map.remove("thresholds");
        std::fs::write(&path, serde_json::to_string(&map).expect("序列化失败"))
            .expect("写回失败");

        let (bundle, warnings) = ConfigService::load_all(&dir).expect("缺字段应补默认");
        assert_eq!(bundle.emotion.thresholds, ThresholdsCfg::default());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("thresholds") && w.contains("补齐")),
            "应包含缺字段告警：{warnings:?}"
        );
    }

    #[test]
    fn unknown_top_field_is_reported() {
        let dir = temp_dir("unknown-field");
        copy_defaults(&dir);
        let path = dir.join("settings.json");
        let text = std::fs::read_to_string(&path).expect("读取 settings.json 失败");
        let patched = text.replace(
            "\"privacy\"",
            "\"privacyX\"",
        );
        assert_ne!(text, patched, "应能注入未知字段");
        std::fs::write(&path, patched).expect("写回失败");

        let (_, warnings) = ConfigService::load_all(&dir).expect("未知字段应忽略而非报错");
        assert!(
            warnings.iter().any(|w| w.contains("privacyX")),
            "应包含未知字段告警：{warnings:?}"
        );
    }

    /// needs.coupling 成环 → Err（`02 §5.10`：新增成环规则在配置加载阶段即失败）。
    #[test]
    fn coupling_cycle_returns_err() {
        let dir = temp_dir("coupling-cycle");
        copy_defaults(&dir);
        let path = dir.join("needs.json");
        let text = std::fs::read_to_string(&path).expect("读取 needs.json 失败");
        // 注入成环规则：mood → satiety（与默认 C-01 的 satiety → moodDecay → mood 构成环）。
        let patched = text.replace(
            "\"rules\": [",
            "\"rules\": [ { \"id\": \"X-01\", \"when\": \"mood<30\", \"target\": \"satiety\", \"op\": \"mul\", \"value\": 0.9 },",
        );
        assert_ne!(text, patched, "应能注入成环规则");
        std::fs::write(&path, patched).expect("写回失败");

        let err = ConfigService::load_all(&dir).expect_err("成环配置必须返回 Err");
        match err {
            ConfigError::CouplingCycle { chain } => {
                assert!(chain.contains("satiety"), "环链应包含 satiety：{chain}");
            }
            other => panic!("期望 CouplingCycle，实际：{other:?}"),
        }
    }

    #[test]
    fn default_coupling_rules_have_no_cycle() {
        let needs = NeedsConfig::default();
        check_coupling_cycles(&needs).expect("默认 C-01~C-16 必须无环");
    }

    #[test]
    fn duplicate_action_id_returns_err() {
        let dir = temp_dir("duplicate-action");
        copy_defaults(&dir);
        let path = dir.join("actions.json");
        let text = std::fs::read_to_string(&path).expect("读取 actions.json 失败");
        // 复制首条动作造成重复 ID。
        let value: serde_json::Value =
            serde_json::from_str(&text).expect("默认 actions.json 应为合法 JSON");
        let mut map = value.as_object().expect("顶层应为对象").clone();
        let list = map
            .get_mut("actions")
            .and_then(|v| v.as_array_mut())
            .expect("actions 应为数组");
        let first = list[0].clone();
        list.insert(1, first);
        std::fs::write(&path, serde_json::to_string(&map).expect("序列化失败"))
            .expect("写回失败");

        let err = ConfigService::load_all(&dir).expect_err("重复 ID 必须返回 Err");
        match err {
            ConfigError::InvalidConfig { file, reason } => {
                assert_eq!(file, "actions.json");
                assert!(reason.contains("重复"), "{reason}");
            }
            other => panic!("期望 InvalidConfig，实际：{other:?}"),
        }
    }

    #[test]
    fn empty_action_id_returns_err() {
        let dir = temp_dir("empty-action-id");
        copy_defaults(&dir);
        let path = dir.join("actions.json");
        let text = std::fs::read_to_string(&path).expect("读取 actions.json 失败");
        // 将首个动作的 ID 整串清空（ACT-M-01 → ""），触发结构级校验错误。
        let patched = text.replacen("\"id\": \"ACT-M-01\"", "\"id\": \"\"", 1);
        assert_ne!(text, patched, "应能清空首个动作 ID");
        std::fs::write(&path, patched).expect("写回失败");

        let err = ConfigService::load_all(&dir).expect_err("空 ID 必须返回 Err");
        assert!(matches!(err, ConfigError::InvalidConfig { .. }));
    }

    /// schema 产物：七份存在、合法 JSON、与配置文件一一对应。
    #[test]
    fn schema_files_match_config_files() {
        for base in [
            "settings", "character", "actions", "emotion", "needs", "animation", "schedule",
        ] {
            let cfg = resources_config_dir().join(format!("{base}.json"));
            let schema = resources_schema_dir().join(format!("{base}.schema.json"));
            assert!(cfg.exists(), "缺少配置文件：{}", cfg.display());
            assert!(schema.exists(), "缺少 schema：{}", schema.display());
            let text = std::fs::read_to_string(&schema).expect("读取 schema 失败");
            let value: serde_json::Value =
                serde_json::from_str(&text).expect("schema 应为合法 JSON");
            assert!(
                value.get("$schema").is_some() || value.get("type").is_some(),
                "{base}.schema.json 应具备 JSON Schema 基本结构"
            );
        }
    }
}
