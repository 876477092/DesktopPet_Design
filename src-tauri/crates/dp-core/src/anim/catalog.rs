//! 动作目录（S2-M2，卡片要点 1）。
//!
//! 加载 `actions.json` 的 53 条全量元数据（ID 一律 `ACT-*`，RV-12），
//! **复用** `dp-core::config` 的 [`ActionsConfig`] / [`ActionCfg`] 模型
//! （S1-M5 已冻结字段结构与语义），本模块不重复定义 53 条字段结构。
//!
//! 职责：
//!   - 全量目录视图 [`ActionCatalog::all`]（含 24 条 `disabled=true` 未交付动作）；
//!   - 已启用视图 [`ActionCatalog::enabled`]（批次 A 29 条）——**仅做标记暴露**，
//!     「跳过 disabled」的仲裁决策归 S2-M3；
//!   - 派生 anim 播放视图 [`PlayItem`](crate::anim::player::PlayItem)
//!     （mirror / fps / loopRange / 图集帧数注入）。
//!
//! 降级口径（R19）：actions.json 加载失败时由调用方（dp-app 装配点）降级为
//! 图集顺序轮播，本 crate 不 panic。

use std::path::Path;

use tracing::warn;

use crate::anim::player::PlayItem;
use crate::config::{ActionCfg, ActionsConfig, ConfigError, ConfigService};

/// 动作 ID 合法前缀（RV-12：`ACT-<类>-<两位序号>`）。
pub const ACTION_ID_PREFIX: &str = "ACT-";

/// 动作目录：`actions.json` 全量元数据的运行时只读视图。
#[derive(Debug, Clone, Default)]
pub struct ActionCatalog {
    /// 全量动作元数据（含 `disabled=true`，保持 actions.json 文件序）。
    actions: Vec<ActionCfg>,
    /// 非 `ACT-` 前缀的 ID 记录（防御性告警依据；不阻断加载）。
    invalid_ids: Vec<String>,
}

impl ActionCatalog {
    /// 从既有动作元数据列表构建目录（测试与自定义装配入口）。
    ///
    /// 非 `ACT-` 前缀的 ID 记录 [`ActionCatalog::invalid_ids`] 并 `tracing::warn`
    /// （不阻断：结构级校验（空/重复 ID）已在 `ConfigService::load_all` 前置）。
    #[must_use]
    pub fn from_actions(actions: Vec<ActionCfg>) -> Self {
        let invalid_ids: Vec<String> = actions
            .iter()
            .filter(|a| !a.id.starts_with(ACTION_ID_PREFIX))
            .map(|a| a.id.clone())
            .collect();
        for id in &invalid_ids {
            warn!("动作 ID 缺少 {ACTION_ID_PREFIX} 前缀（RV-12）：{id}");
        }
        Self { actions, invalid_ids }
    }

    /// 从 `ActionsConfig` 构建目录（复用 S1-M5 配置模型，克隆元数据）。
    #[must_use]
    pub fn from_config(config: &ActionsConfig) -> Self {
        Self::from_actions(config.actions.clone())
    }

    /// 经 [`ConfigService::load_all`] 从配置目录加载（复用六份配置的统一入口：
    /// 缺字段补默认 + 告警；结构级错误（空/重复 ID / coupling 环）原样上抛）。
    ///
    /// # Errors
    /// `actions.json` 结构级无效（动作 ID 空或重复）时返回 [`ConfigError`]。
    pub fn load(config_dir: &Path) -> Result<Self, ConfigError> {
        let (bundle, _warnings) = ConfigService::load_all(config_dir)?;
        Ok(Self::from_config(&bundle.actions))
    }

    /// 全量动作元数据（53 条；含 disabled，保持文件序）。
    #[must_use]
    pub fn all(&self) -> &[ActionCfg] {
        &self.actions
    }

    /// 已启用动作（批次 A：`disabled == false`，29 条）。
    ///
    /// 注意：这只是「目录视角」的过滤视图；**仲裁决策（何时播哪个）归 S2-M3**。
    pub fn enabled(&self) -> impl Iterator<Item = &ActionCfg> {
        self.actions.iter().filter(|a| !a.disabled)
    }

    /// 按 ID 查找动作元数据（全量域，含 disabled——调用方自行判断 `disabled`）。
    #[must_use]
    pub fn find(&self, action_id: &str) -> Option<&ActionCfg> {
        self.actions.iter().find(|a| a.id == action_id)
    }

    /// 动作是否已启用（不在目录中 → `false`）。
    #[must_use]
    pub fn is_enabled(&self, action_id: &str) -> bool {
        self.find(action_id).is_some_and(|a| !a.disabled)
    }

    /// 非 `ACT-` 前缀的 ID 记录（构建期防御性告警产物）。
    #[must_use]
    pub fn invalid_ids(&self) -> &[String] {
        &self.invalid_ids
    }

    /// 派生播放项列表（批次 A 已启用动作 × 图集帧数注入）。
    ///
    /// - `frame_counts`：动作 ID → 图集帧数（由 dp-assets `AtlasFile::find` 的
    ///   `frameCount` 注入；返回 `None` 表示图集中无该动作 → 跳过该动作）；
    /// - `dwell_ms`：循环动作轮播驻留时长（毫秒）；
    /// - disabled 动作一律不派生（S2-M3 仲裁器跳过的数据基础）。
    #[must_use]
    pub fn play_items(
        &self,
        frame_counts: &dyn Fn(&str) -> Option<u32>,
        dwell_ms: u64,
    ) -> Vec<PlayItem> {
        let mut items = Vec::new();
        for action in self.enabled() {
            let Some(frame_count) = frame_counts(&action.id) else {
                warn!("动作 {} 已启用但图集中无元数据，跳过派生播放项", action.id);
                continue;
            };
            items.push(PlayItem::from_action(action, frame_count, dwell_ms));
        }
        items
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::player::PlayItem;

    /// 工程根：dp-core 位于 crates/dp-core，上溯三级（无盘符字面量，C1）。
    fn repo_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../")
    }

    fn resources_config_dir() -> std::path::PathBuf {
        repo_root().join("resources").join("config")
    }

    #[test]
    fn load_reads_53_actions_from_resources() {
        let catalog = ActionCatalog::load(&resources_config_dir()).expect("默认配置应可加载");
        assert_eq!(catalog.all().len(), 53, "actions.json 应为 53 条全量元数据");
        assert!(catalog.invalid_ids().is_empty(), "默认包不应有非 ACT- 前缀 ID");
    }

    #[test]
    fn batch_a_has_29_enabled_and_24_disabled() {
        let catalog = ActionCatalog::load(&resources_config_dir()).expect("默认配置应可加载");
        // S8-M4：批次 C 活动 8 条（ACT-N-09~16）已启用 → 37 启用 / 16 禁用。
        assert_eq!(catalog.enabled().count(), 37, "S8-M4 后应为 37 条启用动作（批次 A 29 + 活动 8）");
        assert_eq!(catalog.all().iter().filter(|a| a.disabled).count(), 16, "未交付应为 16 条（B 8 + S/P 8）");
        // 启用动作类别 = 批次 A（move/idle/interact/emotion）+ 批次 C 活动（activity）。
        for action in catalog.enabled() {
            assert!(
                matches!(action.category.as_str(), "move" | "idle" | "interact" | "emotion" | "activity"),
                "启用动作 {} 类别应为批次 A/活动：{}",
                action.id,
                action.category
            );
        }
    }

    #[test]
    fn all_ids_have_act_prefix() {
        let catalog = ActionCatalog::load(&resources_config_dir()).expect("默认配置应可加载");
        for action in catalog.all() {
            assert!(
                action.id.starts_with(ACTION_ID_PREFIX),
                "动作 ID 一律 ACT-* 前缀（RV-12）：{}",
                action.id
            );
        }
    }

    #[test]
    fn find_and_is_enabled_cover_both_domains() {
        let catalog = ActionCatalog::load(&resources_config_dir()).expect("默认配置应可加载");
        // 启用域：ACT-M-01（站立）。
        let m01 = catalog.find("ACT-M-01").expect("ACT-M-01 应在目录中");
        assert_eq!(m01.fps, 6, "ACT-M-01 站立 6fps（01 §6.3.2）");
        assert!(catalog.is_enabled("ACT-M-01"));
        // disabled 域：可 find 但 is_enabled=false（仲裁器跳过的数据基础）。
        let disabled = catalog.all().iter().find(|a| a.disabled).expect("应存在 disabled 动作");
        assert!(catalog.find(&disabled.id).is_some(), "disabled 动作仍保留在目录");
        assert!(!catalog.is_enabled(&disabled.id));
        // 不存在的 ID。
        assert!(catalog.find("ACT-Z-99").is_none());
        assert!(!catalog.is_enabled("ACT-Z-99"));
    }

    #[test]
    fn play_items_derive_only_enabled_with_atlas_frames() {
        let catalog = ActionCatalog::load(&resources_config_dir()).expect("默认配置应可加载");
        // 图集帧数注入：ACT-M-01 → 4 帧；其余动作一律无图集（跳过）。
        let items = catalog.play_items(
            &|id| if id == "ACT-M-01" { Some(4) } else { None },
            2_000,
        );
        assert_eq!(items.len(), 1, "仅图集可用的启用动作被派生");
        let item: &PlayItem = &items[0];
        assert_eq!(item.action_id, "ACT-M-01");
        assert_eq!(item.frame_count, 4);
        assert_eq!(item.fps, 6, "fps 取自动作元数据（非档位）");
        assert_eq!(item.loop_range, Some([0, 3]));
        assert!(item.looping);
        assert!(item.mirror, "ACT-M-01 元数据 mirror=true");

        // 全量图集注入 → 37 条播放项（disabled 一律不派生；S8-M4 批次 C 活动 8 条启用）。
        let all: Vec<&str> = catalog.all().iter().map(|a| a.id.as_str()).collect();
        let counts = &move |id: &str| -> Option<u32> {
            if all.contains(&id) { Some(8) } else { None }
        };
        let items = catalog.play_items(counts, 2_000);
        assert_eq!(items.len(), 37, "S8-M4 后全量派生应为 37 条（29 + 活动 8）");
        assert!(
            items.iter().all(|i| catalog.is_enabled(&i.action_id)),
            "播放项不得包含 disabled 动作"
        );
    }

    #[test]
    fn empty_catalog_is_valid_default() {
        let catalog = ActionCatalog::default();
        assert!(catalog.all().is_empty());
        assert_eq!(catalog.enabled().count(), 0);
        assert!(catalog.play_items(&|_| Some(4), 1_000).is_empty());
    }
}
