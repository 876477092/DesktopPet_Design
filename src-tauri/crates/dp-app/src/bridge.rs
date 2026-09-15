//! `dp-app/src/bridge.rs` —— `pet://` 事件桥接与渲染帧命令（S2-M2 重写，T-04 段 · 下）。
//!
//! 职责（**仅此三项**，其余一律不在此模块）：
//!   1. **`RenderFrameCmd` v1 载荷定义**（`02 §7.6`：core → 宠物窗口，渲染 tick 2~60Hz），
//!      结构前向可解析（v2 在 S9-M1 升级）；**载荷结构相对 S2-M1 不变**（C8：
//!      不新增未登记事件，字段保持前向兼容）；
//!   2. **`pet://frame` 事件通道**：`spawn_frame_player` 启动真实动作播放器
//!      （`dp-core::anim` 的 [`ActionCatalog`] + [`ActionPlayer`]）——按目录依次/循环
//!      播放批次 A 已启用动作（29 条），tick 节拍 = fps 档位（K-4：默认空闲 6fps，
//!      可切 2/4/6/15/30/60，CPU 随档位变化），动画帧号 = elapsed × 动作 fps；
//!   3. **`atlas_png` 最小命令**：前端经 `invoke('atlas_png', { name })` 读取图集 PNG
//!      字节（自定义 command，**不走 asset 协议网络面**，C9）；
//!   4. **播放指令通道**（S4 前清障 B15-④）：[`PlaybackChannel`] 为 core-loop →
//!      播放器线程的**进程内**指令/回报通道（`Play` / `Stop` 指令、`Finished`
//!      回报），播放器以覆盖态优先播放仲裁器指定动作，非循环动作播完后回报
//!      `Finished` 供 core-loop 推进仲裁链（入队语义不变、新增出队起播）。
//!      进程内状态**不是** `pet://` 事件，不在 C8 事件登记面。
//!
//! 边界：
//!   - 不做仲裁器（S2-M3）——本播放器只做「目录序轮播」，动作选择决策归仲裁器；
//!   - 交叉淡入由前端 FrameRenderer 固定 150ms 线性实现（本阶段不读 animation.json
//!     的 fadeMs/easing，S9-M3 才升级 150~250ms 缓动）——切换时旧动作末帧与新动作
//!     首帧由前端叠加，v1 载荷保持单帧（C8 冻结）；
//!   - 图集缺失 / 目录加载失败按 `02 §7.4` 降级：告警 + 无帧推送 / 图集顺序轮播，
//!     不 panic。
//!
//! ## 跨模块硬约束
//!   - **C8**：事件名 `pet://frame` 已登记 `02 §7.6`（S1 骨架期登记），不新增未登记事件；
//!   - **C3**：tick 节拍用 [`std::time::Instant`] 单调钟且**绝对时间锚定**
//!     （第 k tick deadline = start + k×间隔；AC-11：固定 sleep 过冲逐 tick 累加）；
//!   - **C1**：路径一律相对资源目录解析（dev 兜底相对工程根），无盘符字面量；
//!   - **C9**：`atlas_png` 仅读本机资源文件，零网络；
//!   - **前向兼容**：`#[serde(default)]`（缺字段取默认值）+ serde 默认忽略未知字段
//!     （未加 `deny_unknown_fields`）⇒ S9-M1 的 v2 新增字段不致 v1 解析失败。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use dp_core::anim::{
    ActionCatalog, ActionPlayer, FpsTier, PlayItem, PlayerFrame,
    player::{cmd_mirror, frame_index_for},
};
use dp_core::config::ActionCfg;

use crate::hit_latest::HitFeed;

/// `pet://frame` 事件名（`02 §7.6`；前端对应 `PET_EVENT.FRAME`，C8）。
pub const FRAME_EVENT: &str = "pet://frame";

/// `RenderFrameCmd` 载荷结构版本（v1；v2 在 S9-M1 升级）。
pub const FRAME_CMD_VERSION: u32 = 1;

/// 轮播驻留时长（毫秒）：循环动作每条播放 2s 后切换下一动作（目录序轮播节拍）。
pub const DWELL_MS: u64 = 2_000;

/// `RenderFrameCmd` v1（`02 §7.6`）。
///
/// 字段名与 `02` 文档既有词汇对齐（RV-12 `ACT-` 前缀 / atlas.json
/// `png/columns/rows/frameW/frameH` / K-4 `mirror`、fps 档位 / §6.5 alpha），
/// 未自造与文档冲突的字段名。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RenderFrameCmd {
    /// 载荷结构版本（当前恒为 [`FRAME_CMD_VERSION`]）。
    pub version: u32,
    /// 动作 ID（RV-12：`ACT-` 前缀；由动作播放器按目录轮播下发）。
    pub action_id: String,
    /// 图集引用：atlas.json 的 `png` 文件名（相对图集目录，前端经 `atlas_png` 读取）。
    pub atlas_png: String,
    /// 帧序号（0 起连续，行主序横向图集）。
    pub frame_index: u32,
    /// 横向列数（atlas.json `columns`，子矩形推算用）。
    pub columns: u32,
    /// 行数（atlas.json `rows`，子矩形推算用）。
    pub rows: u32,
    /// 单帧物理宽（atlas.json `frameW`，2x 导出 256）。
    pub frame_w: u32,
    /// 单帧物理高（atlas.json `frameH`）。
    pub frame_h: u32,
    /// 镜像（K-4：`mirror=true` → 渲染端水平翻转；规则 = 动作元数据 mirror + 朝向，
    /// S2-M2 起由播放器产出）。
    pub mirror: bool,
    /// 整体不透明度（0.0~1.0，`02 §6.5` 渲染时序）。
    pub alpha: f32,
    /// 帧率档位提示（K-4：2~60；当前 tick 档位，供降帧显示与调试）。
    pub fps: u32,
}

impl Default for RenderFrameCmd {
    /// 缺字段默认值（`#[serde(default)]` 的前向兼容基线）：
    /// `alpha=1.0`（完全不透明）、`fps=6`（K-4 空闲档），其余零值/空串。
    fn default() -> Self {
        Self {
            version: FRAME_CMD_VERSION,
            action_id: String::new(),
            atlas_png: String::new(),
            frame_index: 0,
            columns: 0,
            rows: 0,
            frame_w: 0,
            frame_h: 0,
            mirror: false,
            alpha: 1.0,
            fps: FpsTier::default().fps(),
        }
    }
}

/// 图集内子矩形（物理像素；与 `dp-assets::atlas::Rect` 同布局语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRect {
    /// 子矩形左上 X（相对图集左上）。
    pub x: u32,
    /// 子矩形左上 Y。
    pub y: u32,
    /// 宽。
    pub w: u32,
    /// 高。
    pub h: u32,
}

impl RenderFrameCmd {
    /// 第 `frame_index` 帧在图集中的子矩形（横向行主序，与
    /// `dp-assets::atlas::AtlasMeta::frame_rect` 同布局；纯函数，单测覆盖）。
    ///
    /// 布局非法（`columns`/`rows` 为 0，或索引越界）→ `None`，调用方降级跳帧不 panic
    /// （`02 §7.4`）。
    #[must_use]
    pub fn frame_rect(&self) -> Option<FrameRect> {
        if self.columns == 0 || self.rows == 0 {
            return None;
        }
        if self.frame_w == 0 || self.frame_h == 0 {
            return None;
        }
        let capacity = u64::from(self.columns) * u64::from(self.rows);
        if u64::from(self.frame_index) >= capacity {
            return None;
        }
        let col = self.frame_index % self.columns;
        let row = self.frame_index / self.columns;
        Some(FrameRect {
            x: col * self.frame_w,
            y: row * self.frame_h,
            w: self.frame_w,
            h: self.frame_h,
        })
    }
}

/// 由图集元数据 + 播放器一帧拼装 `RenderFrameCmd` v1（纯函数，单测覆盖）。
///
/// - 布局字段（png/columns/rows/frameW/frameH）取自图集元数据；
/// - `mirror` / `frame_index` / `action_id` 取自播放器输出（K-4 镜像规则）；
/// - `alpha` 恒 1.0（整体不透明度由设置模块 S3+ 接管）；
/// - `fps` 为当前 tick 档位（K-4 档位提示，与动作自身帧率正交）。
#[must_use]
pub fn frame_cmd(meta: &dp_assets::atlas::AtlasMeta, frame: &PlayerFrame, tier_fps: u32) -> RenderFrameCmd {
    RenderFrameCmd {
        version: FRAME_CMD_VERSION,
        action_id: frame.action_id.clone(),
        atlas_png: meta.png.clone(),
        frame_index: frame.frame_index,
        columns: meta.columns,
        rows: meta.rows,
        frame_w: meta.frame_w,
        frame_h: meta.frame_h,
        mirror: frame.mirror,
        alpha: 1.0,
        fps: tier_fps,
    }
}

/// fps 档位运行时槽：S3+ 模块（托盘/仲裁器）经 `app.state::<FrameTierHandle>()`
/// 切换 K-4 档位，播放器 tick 循环每 tick 读取并即时生效（CPU 随档位变化）。
#[derive(Clone)]
pub struct FrameTierHandle {
    /// 档位在 [`FpsTier::ALL`] 中的下标（原子槽；越界防御取模）。
    tier_index: Arc<AtomicU8>,
}

impl FrameTierHandle {
    /// 以指定档位初始化槽。
    #[must_use]
    pub fn new(tier: FpsTier) -> Self {
        let index = FpsTier::ALL.iter().position(|t| *t == tier).unwrap_or(0);
        Self { tier_index: Arc::new(AtomicU8::new(index as u8)) }
    }

    /// 读取当前档位（越界防御：取模回落合法档）。
    #[must_use]
    pub fn get(&self) -> FpsTier {
        let index = usize::from(self.tier_index.load(Ordering::Relaxed));
        FpsTier::ALL[index % FpsTier::ALL.len()]
    }

    /// 切换档位（非登记档 → 拒绝并返回 `false`，档位保持不变）。
    pub fn set(&self, tier: FpsTier) {
        if let Some(index) = FpsTier::ALL.iter().position(|t| *t == tier) {
            self.tier_index.store(index as u8, Ordering::Relaxed);
        }
    }
}

/// 解析图集目录（`<resource_dir>/resources/atlas`，兼容个别打包形态去掉外层目录；
/// dev 兜底：工程根 `resources/atlas`（经 `CARGO_MANIFEST_DIR` 相对上溯，C1 无盘符字面量））。
///
/// 资源目录由 `tauri.conf.json` 的 `bundle.resources = ["../resources", "../assets"]`
/// 随安装包分发；所有候选都无 `atlas.json` → `None`（调用方降级，不 panic）。
/// `pub(crate)`：S3-M2 掩码库构建（`hit_latest::spawn_mask_build`）复用同一候选链。
#[must_use]
pub(crate) fn resolve_atlas_dir(app: &AppHandle) -> Option<PathBuf> {
    let root = app.path().resource_dir().ok()?;
    let candidates = [
        root.join("resources").join("atlas"),
        root.join("atlas"),
        // dev 兜底：tauri dev / cargo run 时 resource_dir 可能不含图集，
        // 回落工程根 resources/atlas（S2-M2 冒烟产物路径，gen-atlas.mjs --out）。
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/atlas"),
    ];
    candidates.into_iter().find(|c| c.join("atlas.json").is_file())
}

/// 解析配置目录（`<resource_dir>/resources/config`，同图集口径的打包/dev 兼容候选）。
#[must_use]
fn resolve_config_dir(app: &AppHandle) -> Option<PathBuf> {
    let root = app.path().resource_dir().ok()?;
    let candidates = [
        root.join("resources").join("config"),
        root.join("config"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/config"),
    ];
    candidates.into_iter().find(|c| c.join("actions.json").is_file())
}

/// 加载图集元数据（atlas.json → [`dp_assets::atlas::AtlasFile`]，含结构校验）。
fn load_atlas(dir: &Path) -> Option<dp_assets::atlas::AtlasFile> {
    let bytes = std::fs::read(dir.join("atlas.json")).ok()?;
    match dp_assets::atlas::AtlasFile::from_json_bytes(&bytes) {
        Ok(atlas) => Some(atlas),
        Err(err) => {
            eprintln!("[dp-app] bridge atlas.json 解析失败：{err}");
            None
        }
    }
}

/// 由动作目录 + 图集元数据派生轮播播放项（纯函数，单测覆盖）。
///
/// 仅派生「已启用 × 图集可用」的动作（批次 A 29 条 × 图集交付集合的交集），
/// disabled 动作不派生（S2-M3 仲裁器跳过的数据基础）。
#[must_use]
fn build_play_items(catalog: &ActionCatalog, atlas: &dp_assets::atlas::AtlasFile) -> Vec<PlayItem> {
    catalog.play_items(&|id| atlas.find(id).map(|m| m.frame_count), DWELL_MS)
}

/// 目录不可用时的降级播放项（R19：图集顺序轮播，不空转不崩）。
///
/// 每个图集动作按 6fps（K-4 空闲档）整段循环派生——目录元数据缺失时无法得知
/// 每动作 fps/loopRange，取保守全帧循环；图集为空 → 空列表（调用方退出线程）。
#[must_use]
fn fallback_items_from_atlas(atlas: &dp_assets::atlas::AtlasFile) -> Vec<PlayItem> {
    atlas
        .actions
        .iter()
        .filter(|m| m.frame_count > 0)
        .map(|m| {
            let action = ActionCfg {
                id: m.action_id.clone(),
                fps: FpsTier::default().fps(),
                looping: true,
                mirror: true,
                ..ActionCfg::default()
            };
            PlayItem::from_action(&action, m.frame_count, DWELL_MS)
        })
        .collect()
}

/// 启动动作播放器（S2-M2 真实实现，替换 S2-M1 冒烟发射器；装配点 `lib.rs`）。
///
/// 装配顺序（任一步不可用 → 打印降级日志并返回空句柄，前端保持被动等待）：
///   1. 解析图集目录并加载 atlas.json（结构校验）；
///   2. 解析配置目录加载动作目录（`ActionCatalog::load`，复用 `ConfigService`）；
///      目录不可用 → 降级为图集顺序轮播（[`fallback_items_from_atlas`]，R19）；
///   3. 派生播放项（启用 ∩ 图集可用）→ 空则退出；
///   4. 注册 [`FrameTierHandle`]（S3+ 切档入口）并启动 `dp-frame-player` 线程。
///
/// tick 节拍：**绝对时间锚定**（第 k tick deadline = start + k×档位间隔，
/// AC-11 教训：固定 sleep 过冲会逐 tick 累加）；切档时重新锚定（相位重置）。
///
/// 注：**故意不标 `#[must_use]`**（同 `supervisor::spawn` 口径），装配点丢弃句柄
/// 不触发 `unused_must_use`（`-D warnings` 下会致构建失败）。
pub fn spawn_frame_player(app: AppHandle) -> JoinHandle<()> {
    let Some(dir) = resolve_atlas_dir(&app) else {
        eprintln!(
            "[dp-app] bridge 未发现图集（atlas.json），动作播放器不启动（运行 gen-atlas.mjs 生成后重试）"
        );
        return empty_handle();
    };
    let Some(atlas) = load_atlas(&dir) else {
        eprintln!("[dp-app] bridge 图集元数据不可用，动作播放器不启动");
        return empty_handle();
    };

    // 动作目录：优先 actions.json（批次 A 启用视图）；不可用降级图集顺序轮播（R19）。
    let (items, catalog_source) = match resolve_config_dir(&app)
        .and_then(|cfg_dir| ActionCatalog::load(&cfg_dir).ok())
    {
        Some(catalog) => {
            let items = build_play_items(&catalog, &atlas);
            (items, "actions.json 目录")
        }
        None => (fallback_items_from_atlas(&atlas), "图集降级轮播"),
    };
    if items.is_empty() {
        eprintln!("[dp-app] bridge 无可播放动作（启用动作与图集交集为空），播放器退出");
        return empty_handle();
    }

    // 图集元数据按动作 ID 建表（cmd 拼装用；图集规模小，一次性建表）。
    let metas: HashMap<String, dp_assets::atlas::AtlasMeta> = atlas
        .actions
        .iter()
        .map(|m| (m.action_id.clone(), m.clone()))
        .collect();

    eprintln!(
        "[dp-app] bridge 动作播放器装配完成：source={catalog_source} items={} atlas={}",
        items.len(),
        dir.display()
    );

    // 档位槽：默认空闲 6fps；S3+ 经 app.state::<FrameTierHandle>() 切档。
    let tier_handle = FrameTierHandle::new(FpsTier::default());
    app.manage(tier_handle.clone());

    // S3-M2：HIT_LATEST 帧推喂养侧（每帧 publish 动作/帧号/镜像；句柄未注册 → None，
    // 命中侧保持 bbox 回退）。装配点 lib.rs 保证已在 core-loop 前 manage HitLatestHandle。
    let feed = app.try_state::<crate::hit_latest::HitLatestHandle>().map(|h| h.feed());

    // S4 前清障 B15-④：播放指令通道（装配点 lib.rs 需先 manage 再 spawn；未注册
    // → None，播放器维持纯目录序轮播，与既有行为完全一致）。
    let playback = app.try_state::<PlaybackChannel>().map(|c| (*c).clone());

    match std::thread::Builder::new()
        .name("dp-frame-player".to_string())
        .spawn(move || run_player_loop(app, items, metas, tier_handle, feed, playback))
    {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("[dp-app] bridge 播放器线程启动失败，降级为无帧推送：{err}");
            empty_handle()
        }
    }
}

/// 线程启动失败的降级句柄（立即结束的空线程；同 `supervisor::spawn` 口径）。
fn empty_handle() -> JoinHandle<()> {
    std::thread::spawn(|| {})
}

/// 播放器循环体（在 `dp-frame-player` 线程内运行）。
///
/// 每 tick：读档位槽（切档即时生效并重锚）→ 排空播放指令（B15-④）→
/// **覆盖态优先**（仲裁器指定动作：帧号 = 覆盖起点 elapsed × 动作 fps；非循环
/// 动作播完回报 `Finished` 并回落轮播重锚）→ 否则 `player.frame_at(start.elapsed())`
/// 目录序轮播 → 查图集元数据拼装 `RenderFrameCmd` v1 → 广播。广播失败仅降级为
/// 日志，不中断循环（`02 §7.4.2`）。
///
/// S3-M2 起：每帧广播前向 `feed`（`HIT_LATEST` 推喂侧）publish 当帧
/// （`action_id` / 帧号 / 镜像），供钩子回调做像素级三态判定；`feed` 缺席
/// （句柄未注册）→ 静默跳过，命中回退 bbox，不影响播放主链路。
///
/// B15-④（S4 前清障）：`playback` 缺席（通道未注册）→ 跳过指令排空与回报，
/// 行为与 S3 版本完全一致（纯轮播，回归安全）。
fn run_player_loop(
    app: AppHandle,
    items: Vec<PlayItem>,
    metas: HashMap<String, dp_assets::atlas::AtlasMeta>,
    tier_handle: FrameTierHandle,
    feed: Option<HitFeed>,
    playback: Option<PlaybackChannel>,
) {
    let mut player = ActionPlayer::new(items);
    let mut tier = tier_handle.get();
    let _ = player.set_tier_fps(tier.fps());

    // 时间来源：单调钟 `Instant`（C3「单调节拍可直读」口径，同 supervisor；
    // 2026-09-12 复核裁定见 `dp-core/src/lib.rs` 文件头）；绝对锚定网格。
    let mut start = Instant::now();
    let mut tick: u64 = 0;
    // 覆盖态（B15-④）：仲裁器指定动作的起播锚；`None` = 目录序轮播。
    let mut override_play: Option<OverridePlay> = None;

    eprintln!(
        "[dp-app] bridge 动作播放器启动：actions={} tier={:?}({}fps) interval={:?} playback={}",
        player.items().len(),
        tier,
        tier.fps(),
        player.tick_interval(),
        playback.is_some()
    );

    loop {
        tick = tick.wrapping_add(1);
        sleep_until_tick(start, tick, player.tick_interval());

        // 档位热切换（K-4：2/4/6/15/30/60 可切，CPU 随档位变化）：
        // 切档重锚（start/k 归零），避免旧网格与新间隔混排造成节拍抖动。
        let want = tier_handle.get();
        if want != tier {
            tier = want;
            let _ = player.set_tier_fps(tier.fps());
            start = Instant::now();
            tick = 0;
            eprintln!("[dp-app] bridge fps 档位切换：{:?}（{}fps）", tier, tier.fps());
            continue;
        }

        // B15-④：排空播放指令（core-loop → 播放器）。Play 命中 → 覆盖起播
        // （替换既有覆盖，最近指令优先）；未命中 → 立即回报 Finished（防链卡死）；
        // Stop → 清覆盖并重锚轮播基线（无覆盖时为 no-op）。
        if let Some(channel) = &playback {
            while let Some(order) = channel.try_pop_order() {
                match order {
                    PlaybackOrder::Play { action_id } => {
                        handle_play_order(channel, player.items(), &action_id, &mut override_play);
                    }
                    PlaybackOrder::Stop => {
                        if override_play.take().is_some() {
                            start = Instant::now();
                            tick = 0;
                        }
                    }
                }
            }
        }

        // 覆盖态优先：仲裁器指定动作的帧输出（与轮播基线正交的独立锚）。
        if let Some(over) = &override_play {
            let item = &player.items()[over.index];
            let action_ms = over.start.elapsed().as_millis() as u64;
            if !item.looping && action_ms >= item.segment_ms() {
                // 非循环动作播完：回报 Finished（core-loop 据此推进仲裁链）、
                // 清覆盖、重锚轮播基线，本 tick 不再出帧。
                if let Some(channel) = &playback {
                    channel
                        .push_report(PlaybackReport::Finished { action_id: item.action_id.clone() });
                }
                override_play = None;
                start = Instant::now();
                tick = 0;
                continue;
            }
            // 循环覆盖动作无自然终点，播至 Stop / 下一次 Play。
            let frame = PlayerFrame {
                action_id: item.action_id.clone(),
                frame_index: frame_index_for(item, action_ms),
                mirror: cmd_mirror(item, player.facing()),
                action_fps: item.fps,
            };
            publish_frame(&app, &feed, &metas, &frame, tier.fps());
            continue;
        }

        let Some(frame) = player.frame_at(start.elapsed()) else {
            continue;
        };
        publish_frame(&app, &feed, &metas, &frame, tier.fps());
    }
}

/// 单帧发布（覆盖态与轮播态共用出口）：查图集元数据拼装 `RenderFrameCmd` v1、
/// 喂 HIT_LATEST、广播 `pet://frame`。元数据缺失 → 静默跳帧（降级不 panic）。
fn publish_frame(
    app: &AppHandle,
    feed: &Option<HitFeed>,
    metas: &HashMap<String, dp_assets::atlas::AtlasMeta>,
    frame: &PlayerFrame,
    tier_fps: u32,
) {
    let Some(meta) = metas.get(&frame.action_id) else {
        return;
    };
    let cmd = frame_cmd(meta, frame, tier_fps);
    // S3-M2：帧推喂 HIT_LATEST（钩子像素命中读当帧掩码；未知动作 / 掩码库
    // 未就绪由 feed 侧计数丢弃；feed 缺席 → 静默跳过，命中回退 bbox）。C8：零新事件。
    if let Some(feed) = feed {
        feed.publish(&frame.action_id, frame.frame_index, frame.mirror);
    }
    // S5-M4：整体不透明度（`01 FR-7-6`）在**每帧**从原子句柄读取后写入载荷——
    // 这样设置页改透明度后**下一帧**即生效（无需等业务档、无需重建播放器）。
    let cmd = RenderFrameCmd {
        alpha: app
            .try_state::<FrameAlpha>()
            .map_or(cmd.alpha, |handle| handle.alpha()),
        ..cmd
    };
    if let Err(err) = app.emit(FRAME_EVENT, &cmd) {
        eprintln!("[dp-app] bridge 广播 {FRAME_EVENT} 降级：{err}");
    }
}

/// 睡到第 `tick` 个 tick 的绝对锚定 deadline（`start + tick × step`）。
///
/// 越过 deadline 则不补偿、不忙等（同 `supervisor::sleep_until_tick` 策略；
/// 该函数私有，为避免跨模块耦合此处保留等价小实现）。
fn sleep_until_tick(start: Instant, tick: u64, step: Duration) {
    let target = start + Duration::from_millis(tick.saturating_mul(step.as_millis() as u64));
    if let Some(remaining) = target.checked_duration_since(Instant::now()) {
        std::thread::sleep(remaining);
    }
}

/// 图集文件名合法性校验（纯函数，单测覆盖）。
///
/// 仅允许纯文件名：非空、无路径分隔符（`/`、`\`）、无相对引用（`..`）、
/// 无盘符/冒号（`:`）。`:` 必须拒绝：Windows 下 `Path::join` 遇到带盘符的
/// 相对路径（如 `C:evil.png`）会**整体替换基目录**，`fs::read` 将解析到该
/// 盘符当前工作目录，逃逸图集目录（QA 实证）。
fn validate_atlas_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.contains(':')
    {
        return Err(format!("非法图集文件名：{name}"));
    }
    Ok(())
}

/// 读取图集 PNG 字节的最小命令（前端 `invoke('atlas_png', { name })`）。
///
/// - 仅允许**纯文件名**（经 [`validate_atlas_name`] 校验，防路径穿越）；
/// - 图集目录不可用时返回可读错误（前端降级，不 panic）；
/// - 返回 `Vec<u8>`（JSON 字节数组）；原始字节通道优化留待后续评估。
///
/// # Errors
/// 图集目录缺失 / 文件名非法 / 读取失败时返回中文可读错误串（`02 §7.4.3`）。
#[tauri::command]
pub fn atlas_png(app: AppHandle, name: String) -> Result<Vec<u8>, String> {
    validate_atlas_name(&name)?;
    let dir = resolve_atlas_dir(&app)
        .ok_or_else(|| "图集目录不可用（尚未生成或未随包分发图集资源）".to_string())?;
    let path = dir.join(&name);
    std::fs::read(&path).map_err(|err| format!("读取图集失败（{}）：{err}", path.display()))
}

// ---------------------------------------------------------------------------
// ParticleCmd / MenuCmd v1（S3-M6，T-10 段 · 下；`02 §7.6` 已登记事件）
// ---------------------------------------------------------------------------

/// `pet://fx` 事件名（`02 §7.6` S3-M6 登记；前端对应 `PET_EVENT.FX`，C8）。
pub const FX_EVENT: &str = "pet://fx";

/// `pet://menu` 事件名（`02 §7.6` S3-M6 登记；前端对应 `PET_EVENT.MENU`，C8）。
pub const MENU_EVENT: &str = "pet://menu";

/// `pet://state` 事件名（`02 §7.6` 已登记；S4-M2 起启用，1Hz 全量 `PetSnapshotV2`）。
///
/// **真源 = `dp_core::event::EVENT_STATE`**（C8：事件名定义在内核，此处仅为本层
/// 便捷别名，两者必须同串——见 `bridge` 模块单测 `pet_event_names_align_core`）。
pub const STATE_EVENT: &str = dp_core::event::EVENT_STATE;

/// `pet://emotion` 事件名（`02 §7.6` 已登记；S4-M2 起启用，阶段迁移详情）。
///
/// **真源 = `dp_core::event::EVENT_EMOTION`**（同 [`STATE_EVENT`] 口径）。
pub const EMOTION_EVENT: &str = dp_core::event::EVENT_EMOTION;

/// `pet://coax` 事件名（`02 §7.6` **S4-M3 登记 2026-09-14**；道歉三部曲进度环 + 离家态）。
///
/// **真源 = `dp_core::event::EVENT_COAX`**（同 [`STATE_EVENT`] 口径，C8）。
pub const COAX_EVENT: &str = dp_core::event::EVENT_COAX;

/// `pet://bubble` 事件名（`02 §7.6` 已登记；**S4-M5 起启用**——Rust 侧生产者补齐）。
///
/// **真源 = `dp_core::event::EVENT_BUBBLE`**（同 [`STATE_EVENT`] 口径，C8）。
/// 载荷类型 = `dp_core::event::BubbleWire`（与前端 `BubbleCmdV1` 九字段同构）。
pub const BUBBLE_EVENT: &str = dp_core::event::EVENT_BUBBLE;

/// `ParticleCmd` / `MenuCmd` 载荷结构版本（v1；本批 Rust 即生产者，与
/// `src/shared/ipc.ts` 的 `PARTICLE_CMD_VERSION` / `MENU_CMD_VERSION` 同源）。
pub const PARTICLE_CMD_VERSION: u32 = 1;
pub const MENU_CMD_VERSION: u32 = 1;

/// 单次迸发上限（`02 §5.22` ParticlesCfg `maxPerBurst=60`；与前端
/// `PARTICLE_BURST_CAP` 同源口径，发射侧先钳，消费端再钳兜底）。
pub const PARTICLE_BURST_CAP: u32 = 60;

/// 粒子类别（`01 附录` / `02 §3` 五类；serde 小写线上格式，与前端 `ParticleKind` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ParticleKind {
    /// 爱心（比心 / 抚摸 / 戳痒上浮）。
    #[default]
    Heart,
    /// 星散。
    Star,
    /// 尘土（甩出落地 / 单击微反馈）。
    Dust,
    /// 泪滴（触发映射预留）。
    Tear,
    /// 怒气（触发映射预留）。
    Anger,
}

/// `ParticleCmd` v1（`pet://fx` 载荷；camelCase 线上格式）。
///
/// 粒子锚点（头顶偏右）由前端按容器尺寸推导，载荷不携带坐标——Rust 侧
/// 无需窗口内度量。前向兼容：`#[serde(default)]` + 默认忽略未知字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ParticleCmd {
    /// 载荷结构版本（当前恒为 [`PARTICLE_CMD_VERSION`]）。
    pub version: u32,
    /// 粒子类别。
    pub kind: ParticleKind,
    /// 迸发数量（发射侧已钳 `[1, 60]`）。
    pub count: u32,
}

impl Default for ParticleCmd {
    /// 缺字段默认值：爱心 12（与前端缺省口径一致）。
    fn default() -> Self {
        Self { version: PARTICLE_CMD_VERSION, kind: ParticleKind::Heart, count: 12 }
    }
}

/// `MenuCmd` v1（`pet://menu` 载荷；camelCase 线上格式）。
///
/// - `screenX` / `screenY`：右键命中点的**屏幕物理像素**（钩子域透传，诊断用）；
/// - `localX` / `localY`：同一命中点换算到**宠物窗口内 CSS 像素**（前端菜单
///   摆位直接用；物理 ÷ 所在屏 scale 由 core-loop 完成，与 RV-17 口径一致）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MenuCmd {
    /// 载荷结构版本（当前恒为 [`MENU_CMD_VERSION`]）。
    pub version: u32,
    /// 命中点屏幕物理 X。
    pub screen_x: f64,
    /// 命中点屏幕物理 Y。
    pub screen_y: f64,
    /// 命中点窗口内 CSS X（菜单摆位锚点）。
    pub local_x: f64,
    /// 命中点窗口内 CSS Y。
    pub local_y: f64,
}

impl Default for MenuCmd {
    /// 缺字段默认值：全零（前端 `clampMenuPlacement` 会钳回容器内，不越界弹出）。
    fn default() -> Self {
        Self { version: MENU_CMD_VERSION, screen_x: 0.0, screen_y: 0.0, local_x: 0.0, local_y: 0.0 }
    }
}

/// 拼装粒子命令（纯函数，单测覆盖）：数量钳 `[1, PARTICLE_BURST_CAP]`，非法
/// 输入（0 / 超限）保守取 1 / 上限，不 panic。
#[must_use]
pub fn particle_cmd(kind: ParticleKind, count: u32) -> ParticleCmd {
    let count = if count == 0 { 1 } else { count.min(PARTICLE_BURST_CAP) };
    ParticleCmd { version: PARTICLE_CMD_VERSION, kind, count }
}

/// 拼装菜单命令（纯函数，单测覆盖）：屏幕物理命中点 + 窗口物理矩形左上角 +
/// 所在屏 scale → 窗口内 CSS 坐标；scale 非法（非有限 / ≤0）防御取 1.0（物理
/// 当 CSS，退化不 panic）。
#[must_use]
pub fn menu_cmd(win_left: i32, win_top: i32, scale: f32, hit_x: i32, hit_y: i32) -> MenuCmd {
    let factor = if scale.is_finite() && scale > 0.0 { f64::from(scale) } else { 1.0 };
    MenuCmd {
        version: MENU_CMD_VERSION,
        screen_x: f64::from(hit_x),
        screen_y: f64::from(hit_y),
        local_x: f64::from(hit_x - win_left) / factor,
        local_y: f64::from(hit_y - win_top) / factor,
    }
}

// ---------------------------------------------------------------------------
// 播放指令通道（S4 前清障 B15-④：仲裁器 → 播放器接线；进程内，非 C8 事件面）
// ---------------------------------------------------------------------------

/// 播放指令/回报队列容量（有界：超过即丢最旧并计数，防 core-loop 快速提交时
/// 无界增长；16 ≫ 单次交互链深度 3，正常流量不触界）。
pub const PLAYBACK_CHANNEL_CAP: usize = 16;

/// core-loop → 播放器的播放指令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackOrder {
    /// 起播指定动作（覆盖态：打断轮播，优先播放；循环动作播至 [`PlaybackOrder::Stop`]）。
    Play {
        /// 动作 ID（RV-12：`ACT-` 前缀）。
        action_id: String,
    },
    /// 停播当前覆盖动作并回落目录序轮播（对无覆盖态为 no-op）。
    Stop,
}

/// 播放器 → core-loop 的播放回报。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackReport {
    /// 覆盖动作播完（非循环动作到末帧语义完成）。core-loop 收到后按
    /// `on_action_finished` 语义推进仲裁链（链尾推进；入队语义不变）。
    Finished {
        /// 播完的动作 ID。
        action_id: String,
    },
}

/// 播放器覆盖态：仲裁器指定动作的起播锚（播放项下标 + 单调钟起点，C3）。
#[derive(Debug)]
struct OverridePlay {
    /// 播放项下标（`ActionPlayer::items()` 内）。
    index: usize,
    /// 起播时刻（覆盖期帧号 = elapsed × 动作 fps，与轮播基线正交）。
    start: Instant,
}

/// 进程内播放指令/回报通道（core-loop 持发送侧视图、播放器线程持消费侧视图，
/// 同一 [`Clone`] 对象经 `Arc<Mutex<_>>` 共享）。
///
/// - **有界**：队列满 → 丢最旧并累计 [`PlaybackChannel::dropped_orders`]
///   （「最后一条指令」最能代表当前播放意图，故保最新）；
/// - **锁中毒降级**：对端 panic 时指令/回报静默丢弃，播放器保持轮播
///   （`02 §7.4` 降级口径，不 panic）；
/// - **非 C8 事件面**：进程内状态，不经过 `pet://` 事件桥，无需登记。
#[derive(Debug, Default, Clone)]
pub struct PlaybackChannel {
    /// 指令队列（core-loop push / 播放器 pop）。
    orders: Arc<Mutex<VecDeque<PlaybackOrder>>>,
    /// 回报队列（播放器 push / core-loop drain）。
    reports: Arc<Mutex<VecDeque<PlaybackReport>>>,
    /// 累计丢弃的指令数（可观测性：正常流量应为 0）。
    dropped_orders: Arc<AtomicU64>,
}

impl PlaybackChannel {
    /// 投递播放指令（有界：满则丢最旧并计数）。
    pub fn push_order(&self, order: PlaybackOrder) {
        if let Ok(mut queue) = self.orders.lock() {
            if queue.len() >= PLAYBACK_CHANNEL_CAP {
                queue.pop_front();
                self.dropped_orders.fetch_add(1, Ordering::Relaxed);
            }
            queue.push_back(order);
        }
    }

    /// 取一条播放指令（无指令 → `None`；播放器 tick 循环每 tick 排空）。
    #[must_use]
    pub fn try_pop_order(&self) -> Option<PlaybackOrder> {
        let mut queue = self.orders.lock().ok()?;
        queue.pop_front()
    }

    /// 投递播放回报（同样有界，满丢最旧；回报丢失仅影响链推进时序，core-loop
    /// 下一链动作提交会重同步）。
    pub fn push_report(&self, report: PlaybackReport) {
        if let Ok(mut queue) = self.reports.lock() {
            if queue.len() >= PLAYBACK_CHANNEL_CAP {
                queue.pop_front();
            }
            queue.push_back(report);
        }
    }

    /// 排空全部回报（core-loop 每 tick 调用；返回序 = 投递序）。
    #[must_use]
    pub fn drain_reports(&self) -> Vec<PlaybackReport> {
        match self.reports.lock() {
            Ok(mut queue) => queue.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// 累计丢弃的指令数（正常流量应为 0；非零说明 core-loop 提交速率超播放节拍）。
    #[must_use]
    pub fn dropped_orders(&self) -> u64 {
        self.dropped_orders.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// core-loop 入站指令通道（S4-M4：设置页 / 托盘 → core-loop；进程内，非 C8 事件面）
// ---------------------------------------------------------------------------

/// 入站指令队列容量（有界：满丢最旧并计数；8 ≫ 人工触发速率）。
pub const CORE_INPUT_CAP: usize = 8;

/// dp-app（设置页 / 托盘）→ core-loop 的入站指令（S4-M4 起；S5-M4 扩充）。
///
/// **非 C8 事件面**：进程内状态，不经 `pet://` 事件桥，无需登记；与
/// [`PlaybackChannel`] 同为「壳 → 核」的反向通道（core-loop 单线程 Actor，入站在
/// logic 档 drain）。
///
/// 注：`ResetAllData` / `ImportSave` / `Shutdown` **不再 `Copy`**（携带存档文件名 / 补丁）；
/// 队列本身 clone 语义不变（`VecDeque` 里按值存）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreInput {
    /// 设置页「重置情绪」→ `EmotionEngine::force_lower`（`02 §5.23` R18 兜底）。
    ResetEmotion,
    /// 托盘「把心月狐找回来」→ L5 找回走回（`01 §6.5.2`）。
    RecallRunaway,
    /// 设置页「重置全部数据」（`01 FR-8-4`）→ 清档重建 + 重启进程。
    ///
    /// 走 core-loop 的原因：存档**唯一写者**是 core-loop，命令层直写会破坏单写者语义。
    ResetAllData,
    /// 设置页「导入存档」（`02 §5 K-7`）→ 用存档目录内的备份档覆盖主档 + 重启进程。
    ///
    /// 载荷为**备份文件名**（不是完整路径）：命令层只允许白名单目录内的文件名，
    /// core-loop 再拼路径，双重收口防路径穿越。
    ImportSave {
        /// 备份文件名（如 `save.json.bak` / `save.corrupt.1700000000000.json`）。
        file: String,
    },
    /// 托盘「退出」（`01 FR-1-10`）→ 强制落盘后再退出（**退出前存档确认**，S5-M4）。
    ///
    /// 顺序由 core-loop 保证：`flush_force` 成功（或降级）后才 `app.exit(0)`。
    Shutdown,
}

/// 入站指令通道（dp-app 持发送侧视图、core-loop 持消费侧视图，同一 [`Clone`]
/// 对象经 `Arc<Mutex<_>>` 共享）。
#[derive(Debug, Default, Clone)]
pub struct CoreInputChannel {
    /// 指令队列（dp-app push / core-loop drain）。
    queue: Arc<Mutex<VecDeque<CoreInput>>>,
    /// 累计丢弃的指令数（可观测性：正常流量应为 0）。
    dropped: Arc<AtomicU64>,
}

impl CoreInputChannel {
    /// 投递入站指令（有界：满则丢最旧并计数）。
    pub fn push(&self, input: CoreInput) {
        if let Ok(mut queue) = self.queue.lock() {
            if queue.len() >= CORE_INPUT_CAP {
                queue.pop_front();
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            queue.push_back(input);
        }
    }

    /// 排空全部入站指令（core-loop 每 logic 档调用；返回序 = 投递序）。
    #[must_use]
    pub fn drain(&self) -> Vec<CoreInput> {
        match self.queue.lock() {
            Ok(mut queue) => queue.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// 累计丢弃的指令数（正常流量应为 0）。
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// 设置状态（S5-M4：设置页 → 应用层 → core-loop 的热更新单一真源）
// ---------------------------------------------------------------------------

/// 设置快照载荷版本（v1；与前端 `SETTINGS_VERSION` 同源）。
pub const SETTINGS_VERSION: u32 = 1;

/// `pet://config` 事件名（**真源 = `dp_core::event::EVENT_CONFIG`**；C8 禁字面量）。
pub const CONFIG_EVENT: &str = dp_core::event::EVENT_CONFIG;

/// 提醒偏好（`01 FR-10-2`；有效值 = 存档用户值 ⊕ `schedule.json` 出厂默认）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemindersSnapshot {
    /// 久坐提醒开关。
    pub sedentary_enabled: bool,
    /// 久坐提醒间隔（分钟）。
    pub sedentary_interval_min: u32,
    /// 喝水提醒开关。
    pub water_enabled: bool,
    /// 喝水提醒间隔（分钟）。
    pub water_interval_min: u32,
    /// 间隔取值域下界（分钟；来自 `schedule.json`，UI 不硬编码）。
    pub interval_min_min: u32,
    /// 间隔取值域上界（分钟）。
    pub interval_max_min: u32,
    /// 点击「知道了」是否重新计时。
    pub ack_resets_timer: bool,
}

/// 设置快照（**有效值**：`settings.json` 出厂默认 ⊕ 存档 B 段用户改动）。
///
/// 消费面：
///   - 设置窗口：`settings_get` 命令返回本结构（UI 直接渲染，不读任何配置文件的数值）；
///   - core-loop：`settings_apply` 后经 [`SettingsState::take_pending`] 取补丁，落地到引擎与存档；
///   - `02 §7.6` `pet://config`：只广播**摘要**（[`dp_core::event::ConfigSetWire`]），
///     全量快照走命令按需拉取（避免 1KB+ 载荷在每次改动时全量广播）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSnapshot {
    /// 载荷版本。
    pub version: u32,
    /// 变更序号（单调递增；每次 `settings_apply` +1）。
    pub revision: u64,
    /// 设置服务是否可写（`false` = 存档未装配 / 待迁移，UI 进只读预览）。
    pub writable: bool,
    // --- 身份（FR-7-1 / C2） ---
    /// 有效角色名（存档 `pet.name` 非空则取之，否则取 `character.json.defaultName`）。
    pub name: String,
    /// 出厂默认名（UI「恢复默认」按钮用；C2 唯一来源）。
    pub default_name: String,
    // --- 外观（FR-7-2 / FR-7-6 / FR-7-7） ---
    /// 当前缩放百分比。
    pub scale_percent: u32,
    /// 缩放下界。
    pub scale_min_percent: u32,
    /// 缩放上界。
    pub scale_max_percent: u32,
    /// 缩放步进。
    pub scale_step_percent: u32,
    /// 当前不透明度百分比。
    pub opacity_percent: u32,
    /// 不透明度下界。
    pub opacity_min_percent: u32,
    /// 不透明度上界。
    pub opacity_max_percent: u32,
    /// 界面语言（`zh-CN` / `en-US`）。
    pub language: String,
    // --- 声音（FR-7-3） ---
    /// 主音量百分比。
    pub master_volume_percent: u32,
    /// 音量下界。
    pub volume_min_percent: u32,
    /// 音量上界。
    pub volume_max_percent: u32,
    /// 是否静音。
    pub muted: bool,
    // --- 行为（FR-7-4 / FR-1-2 / FR-1-9） ---
    /// 自动走动。
    pub auto_roam: bool,
    /// 漫游节奏当前值（`01 §8.3`「节奏」）。
    pub roam_pace: f32,
    /// 漫游节奏档位候选值（`settings.json.roam.paceOptions`；UI 不硬编码）。
    pub roam_pace_options: Vec<f32>,
    /// 勿扰模式（`01 FR-10-4`）。
    pub do_not_disturb: bool,
    /// 轻松模式（`01 §6.5.2` Q-E；S4-M3/M4 遗留的设置接线）。
    pub easy_coax_mode: bool,
    /// 鼠标穿透（`01 FR-1-6`；真实窗口态由平台层持有，本字段是设置侧镜像）。
    pub click_through: bool,
    /// 置顶策略（`Always` / `BelowFullscreen` / `Never`）。
    pub always_on_top_policy: String,
    /// 开机自启（真实注册表状态与设置项可能短暂不一致，见 S5-M4 裁定）。
    pub autostart: bool,
    // --- 互动（FR-7-9 / FR-7-10 / Q-18） ---
    /// 情绪敏感度当前值。
    pub sensitivity_value: f32,
    /// 敏感度三档可选值（来自 `emotion.json.sensitivity` 兼容面 → `settings.json.emotion`）。
    pub sensitivity_options: Vec<f32>,
    /// 口头禅开关（FR-7-10）。
    pub catchphrase_enabled: bool,
    /// 口头禅频率档位（`off` / `low` / `standard` / `high`）。
    pub catchphrase_frequency: String,
    /// 单击微反馈开关。
    pub click_feedback_enabled: bool,
    /// 活跃感知开关（Q-18）。
    pub activity_sensing: bool,
    // --- 提醒（FR-10-2） ---
    /// 提醒偏好（含取值域）。
    pub reminders: RemindersSnapshot,
}

/// 提醒偏好补丁（`None` = 不改）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RemindersPatch {
    /// 久坐提醒开关。
    pub sedentary_enabled: Option<bool>,
    /// 久坐提醒间隔（分钟）。
    pub sedentary_interval_min: Option<u32>,
    /// 喝水提醒开关。
    pub water_enabled: Option<bool>,
    /// 喝水提醒间隔（分钟）。
    pub water_interval_min: Option<u32>,
}

impl RemindersPatch {
    /// 是否无字段需要改动（全 `None`）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sedentary_enabled.is_none()
            && self.sedentary_interval_min.is_none()
            && self.water_enabled.is_none()
            && self.water_interval_min.is_none()
    }
}

/// 设置补丁（`01 FR-7`；`None` = 不改该字段）。
///
/// 只允许**已定义设置项**（`03 S5-M3` 禁止顺手新增）：字段集合与
/// [`SettingsSnapshot`] 的「可改项」一一对应，由 `settings_patch_touches_only_defined_fields`
/// 单测与前端 `SettingsPatchV1` 共同锁定。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsPatch {
    /// 角色名（FR-7-1；空串 = 恢复出厂默认名）。
    pub name: Option<String>,
    /// 缩放百分比。
    pub scale_percent: Option<u32>,
    /// 不透明度百分比。
    pub opacity_percent: Option<u32>,
    /// 界面语言。
    pub language: Option<String>,
    /// 主音量百分比。
    pub master_volume_percent: Option<u32>,
    /// 静音。
    pub muted: Option<bool>,
    /// 自动走动。
    pub auto_roam: Option<bool>,
    /// 漫游节奏（`01 §8.3`）。
    pub roam_pace: Option<f32>,
    /// 勿扰模式。
    pub do_not_disturb: Option<bool>,
    /// 轻松模式（`01 §6.5.2`）。
    pub easy_coax_mode: Option<bool>,
    /// 鼠标穿透。
    pub click_through: Option<bool>,
    /// 置顶策略。
    pub always_on_top_policy: Option<String>,
    /// 开机自启。
    pub autostart: Option<bool>,
    /// 情绪敏感度。
    pub sensitivity_value: Option<f32>,
    /// 口头禅开关。
    pub catchphrase_enabled: Option<bool>,
    /// 口头禅频率档位。
    pub catchphrase_frequency: Option<String>,
    /// 单击微反馈开关。
    pub click_feedback_enabled: Option<bool>,
    /// 活跃感知开关。
    pub activity_sensing: Option<bool>,
    /// 提醒偏好。
    pub reminders: Option<RemindersPatch>,
}

impl SettingsPatch {
    /// 是否无字段需要改动（全 `None`）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// 补丁涉及的分组名（`pet://config` 载荷的 `changed` 字段；保序去重）。
    ///
    /// 分组名与 `settings.json` 顶层键同字面量（`appearance` / `audio` / `behavior` /
    /// `interaction` / `reminders`）+ `pet`（身份）。**不含 `performance`**：渲染后端切换
    /// 归 S6-M1，本卡不开放该设置项（`03 S5-M3`「不新增未定义设置项」）。
    #[must_use]
    pub fn groups(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        let mut push = |group: &'static str, hit: bool| {
            if hit && !out.contains(&group) {
                out.push(group);
            }
        };
        push("pet", self.name.is_some());
        push(
            "appearance",
            self.scale_percent.is_some() || self.opacity_percent.is_some() || self.language.is_some(),
        );
        push("audio", self.master_volume_percent.is_some() || self.muted.is_some());
        push(
            "behavior",
            self.auto_roam.is_some()
                || self.roam_pace.is_some()
                || self.do_not_disturb.is_some()
                || self.easy_coax_mode.is_some()
                || self.click_through.is_some()
                || self.always_on_top_policy.is_some()
                || self.autostart.is_some(),
        );
        push(
            "interaction",
            self.sensitivity_value.is_some()
                || self.catchphrase_enabled.is_some()
                || self.catchphrase_frequency.is_some()
                || self.click_feedback_enabled.is_some()
                || self.activity_sensing.is_some(),
        );
        push("reminders", self.reminders.is_some_and(|r| !r.is_empty()));
        out
    }
}

/// core-loop 待消费的一次设置变更。
#[derive(Clone, Debug, PartialEq)]
pub struct PendingSettings {
    /// 变更后的 revision（广播用）。
    pub revision: u64,
    /// 本次合并后的补丁（core-loop 应用面）。
    pub patch: SettingsPatch,
}

/// 设置状态（**应用层唯一设置真源**，`Arc<RwLock<_>>` 共享）。
///
/// ## 为什么不放进 core-loop
/// 设置窗口是**请求/响应**式消费（`settings_get` 必须立刻返回），而 core-loop 是
/// 1Hz 业务档 + 20Hz 逻辑档的单线程 Actor：把设置放进 core-loop 会让每次「读设置」
/// 都退化成跨线程往返 + 阻塞等待。故设置快照放在 app 层（读写皆快），
/// **core-loop 只消费补丁**（[`Self::take_pending`]），并在自己那一档把它落进引擎与存档。
///
/// ## 一致性口径
/// - 补丁**合并**后入队（连续拖动滑块不会堆积成 N 次落盘）；
/// - `revision` 单调递增（消费端只比较是否变化）；
/// - 写失败（锁中毒）退化为「静默不生效」而非 panic（`02 §7.4.2`）。
#[derive(Debug, Clone, Default)]
pub struct SettingsState {
    /// 当前快照 + 待消费补丁。
    inner: Arc<RwLock<SettingsInner>>,
}

/// [`SettingsState`] 的内部数据。
#[derive(Debug, Default)]
struct SettingsInner {
    /// 当前有效快照。
    snapshot: SettingsSnapshot,
    /// 待 core-loop 消费的合并补丁（`None` = 无待处理变更）。
    pending: Option<SettingsPatch>,
}

impl SettingsState {
    /// 以初始快照构造（装配期由 `build_state` 从配置 + 存档推导）。
    #[must_use]
    pub fn new(snapshot: SettingsSnapshot) -> Self {
        Self {
            inner: Arc::new(RwLock::new(SettingsInner { snapshot, pending: None })),
        }
    }

    /// 只读快照。
    #[must_use]
    pub fn snapshot(&self) -> SettingsSnapshot {
        match self.inner.read() {
            Ok(inner) => inner.snapshot.clone(),
            // 锁中毒：返回默认快照（消费端会以 `writable=false` 呈现只读预览）。
            Err(_) => SettingsSnapshot::default(),
        }
    }

    /// 应用补丁：合并进快照 + 入队待 core-loop 消费；返回新的 `revision`。
    ///
    /// 返回 `None` 仅当锁中毒（调用方按失败处理，不谎报成功）。
    pub fn apply(&self, patch: &SettingsPatch) -> Option<u64> {
        let Ok(mut inner) = self.inner.write() else {
            return None;
        };
        merge_patch(&mut inner.snapshot, patch);
        inner.snapshot.revision = inner.snapshot.revision.saturating_add(1);
        let revision = inner.snapshot.revision;
        match inner.pending.as_mut() {
            Some(current) => merge_patch_value(current, patch),
            None => inner.pending = Some(patch.clone()),
        }
        Some(revision)
    }

    /// 整体替换快照（重置 / 导入存档后刷新；`pending` 保留不动）。
    pub fn replace_snapshot(&self, snapshot: SettingsSnapshot) {
        if let Ok(mut inner) = self.inner.write() {
            inner.snapshot = snapshot;
        }
    }

    /// 取出并清空待消费补丁（core-loop 业务档每拍调用一次）。
    #[must_use]
    pub fn take_pending(&self) -> Option<PendingSettings> {
        let Ok(mut inner) = self.inner.write() else {
            return None;
        };
        let patch = inner.pending.take()?;
        Some(PendingSettings { revision: inner.snapshot.revision, patch })
    }
}

impl Default for SettingsSnapshot {
    /// 兜底快照（锁中毒 / 设置服务未装配时返回）：全部取产品默认，`writable = false`。
    ///
    /// 数值与 `settings.json` / `schedule.json` 出厂默认一致（由
    /// `settings_snapshot_default_matches_schedule_defaults` 单测锁定）。
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            revision: 0,
            writable: false,
            name: String::new(),
            default_name: String::new(),
            scale_percent: 100,
            scale_min_percent: 50,
            scale_max_percent: 200,
            scale_step_percent: 10,
            opacity_percent: 100,
            opacity_min_percent: 60,
            opacity_max_percent: 100,
            language: "zh-CN".to_string(),
            master_volume_percent: 80,
            volume_min_percent: 0,
            volume_max_percent: 100,
            muted: false,
            auto_roam: true,
            roam_pace: 1.0,
            roam_pace_options: vec![0.7, 1.0, 1.3],
            do_not_disturb: false,
            easy_coax_mode: false,
            click_through: false,
            always_on_top_policy: "Always".to_string(),
            autostart: false,
            sensitivity_value: 1.0,
            sensitivity_options: vec![0.7, 1.0, 1.3],
            catchphrase_enabled: true,
            catchphrase_frequency: "standard".to_string(),
            click_feedback_enabled: true,
            activity_sensing: true,
            reminders: RemindersSnapshot {
                sedentary_enabled: true,
                sedentary_interval_min: 45,
                water_enabled: true,
                water_interval_min: 45,
                interval_min_min: 15,
                interval_max_min: 180,
                ack_resets_timer: true,
            },
        }
    }
}

/// 把补丁合并进快照（仅覆盖 `Some` 字段；数值按取值域夹紧）。
///
/// 夹紧放在**应用层**而不是 UI：UI 可被绕过（前端先行 / 手工 invoke），
/// 边界校验必须在真正写状态的地方（与「不信任客户端输入」同口径）。
pub fn merge_patch(snapshot: &mut SettingsSnapshot, patch: &SettingsPatch) {
    if let Some(name) = &patch.name {
        snapshot.name = name.trim().to_string();
    }
    if let Some(value) = patch.scale_percent {
        snapshot.scale_percent = value.clamp(snapshot.scale_min_percent, snapshot.scale_max_percent);
    }
    if let Some(value) = patch.opacity_percent {
        snapshot.opacity_percent =
            value.clamp(snapshot.opacity_min_percent, snapshot.opacity_max_percent);
    }
    if let Some(value) = &patch.language {
        // 语言白名单在 `shared/i18n` 侧，此处只做「非空」守门（未知语言由 UI 回退默认）。
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            snapshot.language = trimmed.to_string();
        }
    }
    if let Some(value) = patch.master_volume_percent {
        snapshot.master_volume_percent =
            value.clamp(snapshot.volume_min_percent, snapshot.volume_max_percent);
    }
    if let Some(value) = patch.muted {
        snapshot.muted = value;
    }
    if let Some(value) = patch.auto_roam {
        snapshot.auto_roam = value;
    }
    if let Some(value) = patch.roam_pace {
        if value.is_finite() {
            // 夹紧到档位候选值域（`settings.json.roam.paceOptions` 首末项）。
            let lo = snapshot.roam_pace_options.first().copied().unwrap_or(0.5);
            let hi = snapshot.roam_pace_options.last().copied().unwrap_or(2.0);
            snapshot.roam_pace = value.clamp(lo.min(hi), hi.max(lo));
        }
    }
    if let Some(value) = patch.do_not_disturb {
        snapshot.do_not_disturb = value;
    }
    if let Some(value) = patch.easy_coax_mode {
        snapshot.easy_coax_mode = value;
    }
    if let Some(value) = patch.click_through {
        snapshot.click_through = value;
    }
    if let Some(value) = &patch.always_on_top_policy {
        if let Some(normalized) = normalize_topmost_policy(value) {
            snapshot.always_on_top_policy = normalized.to_string();
        }
    }
    if let Some(value) = patch.autostart {
        snapshot.autostart = value;
    }
    if let Some(value) = patch.sensitivity_value {
        if value.is_finite() {
            // 夹紧到档位可选值域（`settings.json.emotion.sensitivityOptions` 的首末项）。
            let lo = snapshot.sensitivity_options.first().copied().unwrap_or(0.5);
            let hi = snapshot.sensitivity_options.last().copied().unwrap_or(1.6);
            snapshot.sensitivity_value = value.clamp(lo.min(hi), hi.max(lo));
        }
    }
    if let Some(value) = patch.catchphrase_enabled {
        snapshot.catchphrase_enabled = value;
    }
    if let Some(value) = &patch.catchphrase_frequency {
        if let Some(normalized) = normalize_catchphrase_frequency(value) {
            snapshot.catchphrase_frequency = normalized.to_string();
        }
    }
    if let Some(value) = patch.click_feedback_enabled {
        snapshot.click_feedback_enabled = value;
    }
    if let Some(value) = patch.activity_sensing {
        snapshot.activity_sensing = value;
    }
    if let Some(reminders) = patch.reminders {
        let lo = snapshot.reminders.interval_min_min;
        let hi = snapshot.reminders.interval_max_min;
        if let Some(value) = reminders.sedentary_enabled {
            snapshot.reminders.sedentary_enabled = value;
        }
        if let Some(value) = reminders.sedentary_interval_min {
            snapshot.reminders.sedentary_interval_min = value.clamp(lo, hi);
        }
        if let Some(value) = reminders.water_enabled {
            snapshot.reminders.water_enabled = value;
        }
        if let Some(value) = reminders.water_interval_min {
            snapshot.reminders.water_interval_min = value.clamp(lo, hi);
        }
    }
}

/// 补丁合并（`None` 字段由 `b` 覆盖；用于把多次补丁并成一次消费）。
fn merge_patch_value(base: &mut SettingsPatch, patch: &SettingsPatch) {
    macro_rules! take_some {
        ($field:ident) => {
            if patch.$field.is_some() {
                base.$field = patch.$field.clone();
            }
        };
    }
    take_some!(name);
    take_some!(scale_percent);
    take_some!(opacity_percent);
    take_some!(language);
    take_some!(master_volume_percent);
    take_some!(muted);
    take_some!(auto_roam);
    take_some!(roam_pace);
    take_some!(do_not_disturb);
    take_some!(easy_coax_mode);
    take_some!(click_through);
    take_some!(always_on_top_policy);
    take_some!(autostart);
    take_some!(sensitivity_value);
    take_some!(catchphrase_enabled);
    take_some!(catchphrase_frequency);
    take_some!(click_feedback_enabled);
    take_some!(activity_sensing);
    if let Some(next) = patch.reminders {
        let mut merged = base.reminders.unwrap_or_default();
        if next.sedentary_enabled.is_some() {
            merged.sedentary_enabled = next.sedentary_enabled;
        }
        if next.sedentary_interval_min.is_some() {
            merged.sedentary_interval_min = next.sedentary_interval_min;
        }
        if next.water_enabled.is_some() {
            merged.water_enabled = next.water_enabled;
        }
        if next.water_interval_min.is_some() {
            merged.water_interval_min = next.water_interval_min;
        }
        base.reminders = Some(merged);
    }
}

/// 置顶策略字面量归一化（`02 K-1` 三态；未知值 → `None`，保持原值）。
#[must_use]
pub fn normalize_topmost_policy(value: &str) -> Option<&'static str> {
    match value.trim() {
        "Always" | "always" => Some("Always"),
        "BelowFullscreen" | "belowFullscreen" => Some("BelowFullscreen"),
        "Never" | "never" => Some("Never"),
        _ => None,
    }
}

/// 口头禅频率字面量归一化（L-03 枚举四档；未知值 → `None`，保持原值）。
#[must_use]
pub fn normalize_catchphrase_frequency(value: &str) -> Option<&'static str> {
    match value.trim() {
        "off" => Some("off"),
        "low" => Some("low"),
        "standard" => Some("standard"),
        "high" => Some("high"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 渲染帧不透明度（S5-M4：`01 FR-7-6` 主体不透明度 → `RenderFrameCmd.alpha`）
// ---------------------------------------------------------------------------

/// 渲染帧整体不透明度句柄（原子百分比；帧播放器每帧读取）。
///
/// 为什么不用「设置状态读锁」：帧循环是 2~60Hz 的热路径，读锁虽轻仍会与设置写入
/// 争用；一个 `AtomicU32`（百分比）足够表达，且读侧无锁、无分配。
///
/// 窗口本身不做 `WS_EX_LAYERED` alpha 调整（`02 §2.3` 定版口径：透明合成交给
/// WebView2 侧），故不透明度**只在渲染帧上生效**——这正是 `FR-7-6` 的「主体不透明度」。
#[derive(Debug)]
pub struct FrameAlpha {
    /// 百分比（0~100；越界由 [`Self::set_percent`] 夹紧）。
    percent: std::sync::atomic::AtomicU32,
}

impl Default for FrameAlpha {
    fn default() -> Self {
        Self { percent: std::sync::atomic::AtomicU32::new(100) }
    }
}

impl FrameAlpha {
    /// 设置百分比（夹紧到 `[min, 100]`；`min` 由 `settings.json.appearance.opacityMinPercent` 决定）。
    pub fn set_percent(&self, percent: u32, min: u32) {
        let clamped = percent.clamp(min.min(100), 100);
        self.percent.store(clamped, Ordering::Relaxed);
    }

    /// 读取当前百分比。
    #[must_use]
    pub fn percent(&self) -> u32 {
        self.percent.load(Ordering::Relaxed)
    }

    /// 读取当前 alpha（`0.0~1.0`）。
    #[must_use]
    pub fn alpha(&self) -> f32 {
        self.percent() as f32 / 100.0
    }
}

// ---------------------------------------------------------------------------
// 存档目录与备份清单（S5-M4：设置页「数据」Tab 的消费面）
// ---------------------------------------------------------------------------

/// 存档目录名（`01 FR-8-1`：`%APPDATA%\DesktopPet`；与 `tauri.conf.json.productName` 一致，
/// 由 `coreloop` 单测锁定；C1：无盘符字面量）。
pub const SAVE_DIR_NAME: &str = "DesktopPet";

/// 存档主文件名（与 `dp_core::save::store::SAVE_FILE` 同源）。
pub const SAVE_FILE_NAME: &str = "save.json";

/// 解析存档目录（`PathResolver::data_dir()` = `%APPDATA%` + [`SAVE_DIR_NAME`]）。
///
/// 目录**允许不存在**（首次运行由原子写建立）；解析失败（平台 API 不可用）→ `None`，
/// 调用方退化为「不载档不落盘 / 只读预览」（`02 §7.4.2` 降级不崩）。
#[must_use]
pub fn resolve_save_dir(app: &AppHandle) -> Option<PathBuf> {
    let base = app.path().data_dir().ok()?;
    Some(base.join(SAVE_DIR_NAME))
}

/// 备份档用途分类（设置页「数据」Tab 的显示口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SaveBackupKind {
    /// 上一次成功落盘的完整备份（`save.json.bak`）——**可导入**。
    LastGood,
    /// 损坏档隔离副本（`save.corrupt.<ts>.json`）——**可导入**（用户可手工修好再导回）。
    Corrupt,
    /// 未来版本隔离副本（`save.future.<ts>.json`）——不可导入（版本高于本程序）。
    Future,
    /// v1 迁移备份（`save.json.v1bak`）——**待 S8-M7 迁移**，本卡不可导入。
    V1Migration,
}

impl SaveBackupKind {
    /// 是否为「本卡可导入」的类别。
    ///
    /// `Future`（版本过高）与 `V1Migration`（需迁移）都**不可导入**：
    /// 前者导入即触发「版本过高 → 再隔离」的死循环，后者导入即触发
    /// `MigrationPending`（本卡按裁定①禁写盘），两者都会把用户推进更差的状态。
    #[must_use]
    pub fn importable(self) -> bool {
        matches!(self, Self::LastGood | Self::Corrupt)
    }

    /// 不可导入时的原因说明（i18n 键的语义标签，前端再本地化）。
    #[must_use]
    pub fn blocked_reason(self) -> Option<&'static str> {
        match self {
            Self::LastGood | Self::Corrupt => None,
            Self::Future => Some("futureVersion"),
            Self::V1Migration => Some("pendingMigration"),
        }
    }
}

/// 一份候选备份（`settings` 命令的返回项）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveBackup {
    /// 文件名（相对存档目录；导入时回传的也是它——**只允许文件名**，防路径穿越）。
    pub file: String,
    /// 用途分类。
    pub kind: SaveBackupKind,
    /// 字节数（0 = 读取失败 / 空文件）。
    pub size_bytes: u64,
    /// 当前实现是否允许导入。
    pub importable: bool,
    /// 不可导入的原因键（可导入时为 `None`）。
    pub blocked_reason: Option<&'static str>,
}

/// 扫描存档目录，列出可导入的候选备份（`01 FR-8-2`「自动备份恢复」的用户手工入口）。
///
/// 识别规则（与 `dp_core::save` 的文件命名口径一一对应）：
///   - `save.json.bak` → [`SaveBackupKind::LastGood`]
///   - `save.corrupt.*.json` → [`SaveBackupKind::Corrupt`]
///   - `save.future.*.json` → [`SaveBackupKind::Future`]
///   - `save.json.v1bak` → [`SaveBackupKind::V1Migration`]
///
/// 排序：先按「可导入优先」，再按类别、文件名倒序（时间戳倒序 ⇒ 最新在前）。
/// 目录不存在 / 不可读 → `[]`（UI 显示「未发现可用备份」，非错误）。
#[must_use]
pub fn list_save_backups(dir: &Path) -> Vec<SaveBackup> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<SaveBackup> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(kind) = classify_backup(&name) else {
            continue;
        };
        let size_bytes = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        out.push(SaveBackup {
            file: name,
            kind,
            size_bytes,
            importable: kind.importable(),
            blocked_reason: kind.blocked_reason(),
        });
    }
    out.sort_by(|a, b| {
        b.importable
            .cmp(&a.importable)
            .then_with(|| kind_rank(a.kind).cmp(&kind_rank(b.kind)))
            .then_with(|| b.file.cmp(&a.file))
    });
    out
}

/// 文件名 → 备份类别（非备份文件 → `None`；纯函数，可单测）。
///
/// 注意：只接受**纯文件名**（含路径分隔符的一律拒绝），这是路径穿越的第一道收口。
#[must_use]
pub fn classify_backup(name: &str) -> Option<SaveBackupKind> {
    if name.contains('/') || name.contains('\\') || name.contains(':') {
        return None;
    }
    if name == "save.json.bak" {
        return Some(SaveBackupKind::LastGood);
    }
    if name == "save.json.v1bak" {
        return Some(SaveBackupKind::V1Migration);
    }
    let stripped = name.strip_prefix("save.")?.strip_suffix(".json")?;
    if stripped.starts_with("corrupt.") {
        return Some(SaveBackupKind::Corrupt);
    }
    if stripped.starts_with("future.") {
        return Some(SaveBackupKind::Future);
    }
    None
}

/// 备份类别排序权重（可导入类别靠前）。
fn kind_rank(kind: SaveBackupKind) -> u8 {
    match kind {
        SaveBackupKind::LastGood => 0,
        SaveBackupKind::Corrupt => 1,
        SaveBackupKind::Future => 2,
        SaveBackupKind::V1Migration => 3,
    }
}

/// 推导**有效设置快照**（`settings.json` / `schedule.json` / `emotion.json` / `character.json`
/// 的出厂默认 ⊕ 存档 B 段用户值）。
///
/// 划分口径（`02 §3`：安装目录资源只读）：
/// - **出厂默认与取值域**（上下界 / 步进 / 档位候选值）永远来自配置束；
/// - **用户改动后的有效值**来自存档 `pet.*` / `settings.*`（B 段，S5-M1 已冻结结构）；
/// - 无存档（纯逻辑模式 / `%APPDATA%` 不可解析）→ 全部取配置默认 + `writable = false`
///   （设置页进只读预览，而不是显示一份"看起来能改"的假设置）。
#[must_use]
pub fn build_settings_snapshot(
    bundle: &dp_core::config::ConfigBundle,
    save: Option<&dp_core::save::SaveStore>,
) -> SettingsSnapshot {
    let cfg = &bundle.settings;
    let sched = &bundle.schedule;
    let fallback = SettingsSnapshot::default();
    let default_name = bundle.character.default_name.clone();

    let Some(store) = save else {
        return SettingsSnapshot {
            name: String::new(),
            default_name,
            scale_min_percent: cfg.appearance.scale_min_percent,
            scale_max_percent: cfg.appearance.scale_max_percent,
            scale_step_percent: cfg.appearance.scale_step_percent,
            opacity_min_percent: cfg.appearance.opacity_min_percent,
            opacity_max_percent: cfg.appearance.opacity_max_percent,
            volume_min_percent: cfg.audio.volume_min_percent,
            volume_max_percent: cfg.audio.volume_max_percent,
            sensitivity_options: cfg.emotion.sensitivity_options.clone(),
            roam_pace_options: cfg.roam.pace_options.clone(),
            reminders: RemindersSnapshot {
                sedentary_enabled: sched.reminders.sedentary_enabled,
                sedentary_interval_min: sched.reminders.sedentary_interval_min,
                water_enabled: sched.reminders.water_enabled,
                water_interval_min: sched.reminders.water_interval_min,
                interval_min_min: sched.reminders.interval_min_min,
                interval_max_min: sched.reminders.interval_max_min,
                ack_resets_timer: sched.reminders.ack_resets_timer,
            },
            writable: false,
            ..fallback
        };
    };

    let cached = store.cache();
    let pet = &cached.pet;
    let set = &cached.settings;
    SettingsSnapshot {
        version: SETTINGS_VERSION,
        revision: 0,
        writable: store.is_writable(),
        name: pet.name.clone(),
        default_name,
        scale_percent: set.appearance.scale_percent,
        scale_min_percent: cfg.appearance.scale_min_percent,
        scale_max_percent: cfg.appearance.scale_max_percent,
        scale_step_percent: cfg.appearance.scale_step_percent,
        opacity_percent: set.appearance.opacity_percent,
        opacity_min_percent: cfg.appearance.opacity_min_percent,
        opacity_max_percent: cfg.appearance.opacity_max_percent,
        language: set.appearance.language.clone(),
        master_volume_percent: set.audio.master_volume_percent,
        volume_min_percent: cfg.audio.volume_min_percent,
        volume_max_percent: cfg.audio.volume_max_percent,
        muted: set.audio.muted,
        auto_roam: set.behavior.auto_roam,
        roam_pace: set.behavior.roam_pace,
        roam_pace_options: cfg.roam.pace_options.clone(),
        do_not_disturb: set.behavior.do_not_disturb,
        easy_coax_mode: set.behavior.easy_coax_mode,
        click_through: set.behavior.click_through,
        always_on_top_policy: set.behavior.always_on_top_policy.clone(),
        autostart: set.behavior.autostart,
        sensitivity_value: set.emotion_sensitivity,
        sensitivity_options: cfg.emotion.sensitivity_options.clone(),
        catchphrase_enabled: pet.catchphrase.enabled,
        catchphrase_frequency: pet.catchphrase.frequency.as_cfg_name().to_string(),
        click_feedback_enabled: cfg.interaction.click_feedback.enabled,
        activity_sensing: set.privacy.activity_sensing,
        reminders: RemindersSnapshot {
            sedentary_enabled: set.reminders.sedentary_enabled,
            sedentary_interval_min: set.reminders.sedentary_interval_min,
            water_enabled: set.reminders.water_enabled,
            water_interval_min: set.reminders.water_interval_min,
            interval_min_min: sched.reminders.interval_min_min,
            interval_max_min: sched.reminders.interval_max_min,
            ack_resets_timer: sched.reminders.ack_resets_timer,
        },
    }
}

/// 校验「导入存档」请求的文件名是否合法（**第二道收口**：只允许清单内的候选）。
///
/// 返回 `Ok(PathBuf)` = 存档目录内的绝对路径；`Err` = 中文可读原因（前端直接展示）。
pub fn resolve_backup_path(dir: &Path, file: &str) -> Result<PathBuf, String> {
    let Some(kind) = classify_backup(file) else {
        return Err(format!("不是可识别的存档备份文件名：{file}"));
    };
    if !kind.importable() {
        return Err(match kind {
            SaveBackupKind::Future => "该备份来自更高版本的存档，无法导入".to_string(),
            _ => "v1 旧档需先迁移（归 S8-M7），本版本不支持导入".to_string(),
        });
    }
    let path = dir.join(file);
    if !path.is_file() {
        return Err(format!("备份文件不存在：{file}"));
    }
    Ok(path)
}

/// 存档健康状态快照（供设置页「数据」Tab 展示；`save_status` 命令的返回内核）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveStatusInfo {
    /// 加载落点词表名（与 `dp_core::save::LoadStatus` 一一对应）。
    pub state: String,
    /// 是否健康。
    pub healthy: bool,
    /// 是否需要提示用户。
    pub needs_notice: bool,
    /// 是否允许写盘。
    pub writable: bool,
    /// 最近一次落盘的墙钟毫秒。
    pub last_seen_ms: i64,
}

impl Default for SaveStatusInfo {
    fn default() -> Self {
        Self {
            state: "unknown".to_string(),
            healthy: false,
            needs_notice: true,
            writable: false,
            last_seen_ms: 0,
        }
    }
}

/// core-loop 装配期写入的存档健康状态句柄（设置页按需读取，不引入新事件面）。
///
/// 为什么用**命令 + 句柄**而不是新事件：`02 §7.6` 没有「存档状态」事件，新增事件名
/// 需走 C8 登记流程且要改 3 处文档；而「数据」Tab 是**按需查询**场景（打开设置页才需要），
/// 请求/响应天然合适（S5-M1 裁定 ④ 指定的承载面）。
#[derive(Debug, Clone, Default)]
pub struct SaveStatusHandle {
    /// 当前状态（core-loop 唯一写者）。
    inner: Arc<Mutex<SaveStatusInfo>>,
}

impl SaveStatusHandle {
    /// 读取当前状态（锁中毒 → 默认值，即「需提示」，保守方向）。
    #[must_use]
    pub fn get(&self) -> SaveStatusInfo {
        match self.inner.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => SaveStatusInfo::default(),
        }
    }

    /// 写入状态（core-loop 装配期 / 每次成功落盘后刷新 `last_seen_ms`）。
    pub fn set(&self, info: SaveStatusInfo) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = info;
        }
    }

    /// 只刷新 `last_seen_ms`（落盘后调用；避免每次落盘都重建整个结构）。
    pub fn note_flush(&self, last_seen_ms: i64) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.last_seen_ms = last_seen_ms;
        }
    }
}

/// 处理一条 `Play` 指令（纯逻辑，单测覆盖）：动作在播放项中 → 覆盖起播
/// （替换既有覆盖）；未命中（动作不在图集 / 未启用）→ **立即回报 `Finished`**
/// 防链卡死——仲裁器等待链尾 `Finished` 才推进，静默吞掉会让链永久停摆。
fn handle_play_order(
    channel: &PlaybackChannel,
    items: &[PlayItem],
    action_id: &str,
    override_play: &mut Option<OverridePlay>,
) {
    match items.iter().position(|item| item.action_id == action_id) {
        Some(index) => {
            *override_play = Some(OverridePlay { index, start: Instant::now() });
        }
        None => {
            channel.push_report(PlaybackReport::Finished { action_id: action_id.to_string() });
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试（纯逻辑：v1 序列化 / 前向兼容 / 子矩形 / 播放器拼装 / 降级派生）
// ---------------------------------------------------------------------------


#[cfg(test)]
mod tests {
    use super::*;
    use dp_assets::atlas::AtlasMeta;

    /// 工程根：dp-app 位于 crates/dp-app，上溯三级（无盘符字面量，C1）。
    fn resources_config_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/config")
    }

    /// C8：本层 `pet://` 事件名别名必须与 `dp-core::event` 真源逐字一致。
    ///
    /// 别名存在的意义是让 `dp-app` 不必到处写 `dp_core::event::…`；一旦有人把别名
    /// 改成字面量又与内核不同步，`pet://` 白名单就会静默分叉——本测例即该护栏。
    #[test]
    fn pet_event_names_align_core() {
        assert_eq!(STATE_EVENT, dp_core::event::EVENT_STATE);
        assert_eq!(EMOTION_EVENT, dp_core::event::EVENT_EMOTION);
        assert_eq!(COAX_EVENT, dp_core::event::EVENT_COAX);
        assert_eq!(BUBBLE_EVENT, dp_core::event::EVENT_BUBBLE);
        assert_eq!(STATE_EVENT, "pet://state");
        assert_eq!(EMOTION_EVENT, "pet://emotion");
        assert_eq!(COAX_EVENT, "pet://coax");
        assert_eq!(BUBBLE_EVENT, "pet://bubble");
        // 与既有 S3-M6 事件名同为 `pet://<域>` 形态（C8 命名纪律）。
        for name in [
            FRAME_EVENT,
            FX_EVENT,
            MENU_EVENT,
            STATE_EVENT,
            EMOTION_EVENT,
            COAX_EVENT,
            BUBBLE_EVENT,
        ] {
            assert!(name.starts_with("pet://"), "{name} 必须为 pet:// 命名空间");
            assert!(!name.contains(' '), "{name} 不得含空格");
        }
    }

    /// 构造图集元数据快捷工厂。
    fn meta(action_id: &str, png: &str, frame_count: u32) -> AtlasMeta {
        AtlasMeta {
            action_id: action_id.to_string(),
            png: png.to_string(),
            frame_w: 256,
            frame_h: 256,
            columns: frame_count,
            rows: 1,
            frame_count,
            ..AtlasMeta::default()
        }
    }

    #[test]
    fn atlas_name_validation_rejects_traversal_and_drive_escaping() {
        // 合法名：纯文件名。
        assert_eq!(validate_atlas_name("ACT-M-01_idle.png"), Ok(()));

        // 路径分隔符 / 相对引用。
        assert!(validate_atlas_name("a/b.png").is_err());
        assert!(validate_atlas_name("a\\b.png").is_err());
        assert!(validate_atlas_name("../atlas.json").is_err());
        assert!(validate_atlas_name("").is_err());

        // 盘符冒号逃逸（QA 实证：Windows Path::join 遇 "C:evil.png" 整体替换基目录）。
        assert!(validate_atlas_name("C:evil.png").is_err(), "盘符相对路径必须拒绝");
        assert!(validate_atlas_name("C:\\evil.png").is_err());
        assert!(validate_atlas_name("evil:png").is_err(), "任意冒号一律拒绝");
        // 错误信息为中文可读格式（02 §7.4.3）。
        let err = validate_atlas_name("C:evil.png").expect_err("应被拒绝");
        assert!(err.contains("非法图集文件名"), "{err}");
    }

    #[test]
    fn render_frame_cmd_v1_roundtrip_is_camel_case() {
        let cmd = RenderFrameCmd {
            version: FRAME_CMD_VERSION,
            action_id: "ACT-M-01".to_string(),
            atlas_png: "ACT-M-01_idle.png".to_string(),
            frame_index: 3,
            columns: 8,
            rows: 1,
            frame_w: 256,
            frame_h: 256,
            mirror: false,
            alpha: 1.0,
            fps: 6,
        };
        let json = serde_json::to_string(&cmd).expect("v1 应可序列化");
        // 线上格式为 camelCase（`02 §7.6` / atlas.json 同词汇）。
        assert!(json.contains("\"actionId\":\"ACT-M-01\""), "{json}");
        assert!(json.contains("\"atlasPng\":\"ACT-M-01_idle.png\""), "{json}");
        assert!(json.contains("\"frameIndex\":3"), "{json}");
        assert!(json.contains("\"frameW\":256"), "{json}");
        assert!(!json.contains("action_id"), "{json}");

        let back: RenderFrameCmd = serde_json::from_str(&json).expect("应可反序列化");
        assert_eq!(back, cmd);
    }

    #[test]
    fn render_frame_cmd_v1_tolerates_unknown_fields() {
        // 前向兼容核心（卡片要点 4）：v2 新增字段（示例 futureField）不得致 v1 解析失败。
        let json = r#"{
            "version": 2,
            "actionId": "ACT-T-02",
            "atlasPng": "ACT-T-02_heart.png",
            "frameIndex": 5,
            "columns": 2,
            "rows": 6,
            "frameW": 256,
            "frameH": 256,
            "mirror": true,
            "alpha": 0.5,
            "fps": 60,
            "futureField": { "clip": "x", "slots": [1, 2] }
        }"#;
        let cmd: RenderFrameCmd = serde_json::from_str(json).expect("未知字段应被容忍");
        assert_eq!(cmd.action_id, "ACT-T-02");
        assert_eq!(cmd.frame_index, 5);
        assert!(cmd.mirror);
        assert!((cmd.alpha - 0.5).abs() < 1e-6);
        assert_eq!(cmd.fps, 60);
    }

    #[test]
    fn render_frame_cmd_v1_missing_fields_fall_back_to_defaults() {
        let cmd: RenderFrameCmd = serde_json::from_str("{}").expect("缺字段应取默认值");
        assert_eq!(cmd.version, FRAME_CMD_VERSION);
        assert_eq!(cmd.action_id, "");
        assert_eq!(cmd.atlas_png, "");
        assert_eq!(cmd.frame_index, 0);
        assert!(!cmd.mirror);
        assert!((cmd.alpha - 1.0).abs() < 1e-6, "默认 alpha 应为 1.0");
        assert_eq!(cmd.fps, FpsTier::default().fps(), "默认档位为 K-4 空闲 6fps");
    }

    #[test]
    fn frame_rect_layout_row_major_and_out_of_range() {
        let base = RenderFrameCmd {
            frame_index: 3,
            columns: 8,
            rows: 1,
            frame_w: 256,
            frame_h: 256,
            ..RenderFrameCmd::default()
        };
        assert_eq!(
            base.frame_rect(),
            Some(FrameRect { x: 768, y: 0, w: 256, h: 256 })
        );

        // 多行：columns=2, rows=3 → index 4 = (0, 512)。
        let multi = RenderFrameCmd {
            frame_index: 4,
            columns: 2,
            rows: 3,
            frame_w: 256,
            frame_h: 256,
            ..RenderFrameCmd::default()
        };
        assert_eq!(
            multi.frame_rect(),
            Some(FrameRect { x: 0, y: 512, w: 256, h: 256 })
        );

        // 越界 / 零列 → None（降级不 panic）。
        let over = RenderFrameCmd { frame_index: 8, columns: 8, rows: 1, ..base.clone() };
        assert_eq!(over.frame_rect(), None);
        let zero = RenderFrameCmd { columns: 0, rows: 0, ..base.clone() };
        assert_eq!(zero.frame_rect(), None);
        let zero_size = RenderFrameCmd { frame_w: 0, frame_h: 0, ..base };
        assert_eq!(zero_size.frame_rect(), None);
    }

    #[test]
    fn frame_cmd_joins_atlas_meta_and_player_frame() {
        let m = meta("ACT-M-02", "ACT-M-02_walk.png", 8);
        let frame = PlayerFrame {
            action_id: "ACT-M-02".to_string(),
            frame_index: 5,
            mirror: true,
            action_fps: 12,
        };
        let cmd = frame_cmd(&m, &frame, 6);
        assert_eq!(cmd.version, FRAME_CMD_VERSION);
        assert_eq!(cmd.action_id, "ACT-M-02");
        assert_eq!(cmd.atlas_png, "ACT-M-02_walk.png");
        assert_eq!(cmd.frame_index, 5);
        assert_eq!(cmd.columns, 8);
        assert_eq!(cmd.rows, 1);
        assert_eq!(cmd.frame_w, 256);
        assert_eq!(cmd.frame_h, 256);
        assert!(cmd.mirror, "镜像取自播放器输出（K-4 规则）");
        assert!((cmd.alpha - 1.0).abs() < 1e-6);
        assert_eq!(cmd.fps, 6, "fps 为当前 tick 档位（非动作帧率 12）");

        // 序列化后仍是登记的 v1 线上结构（C8 冻结）。
        let json = serde_json::to_string(&cmd).expect("应可序列化");
        let value: serde_json::Value = serde_json::from_str(&json).expect("应为合法 JSON");
        for key in [
            "version", "actionId", "atlasPng", "frameIndex", "columns", "rows", "frameW",
            "frameH", "mirror", "alpha", "fps",
        ] {
            assert!(value.as_object().expect("对象").contains_key(key), "缺字段 {key}");
        }
    }

    #[test]
    fn build_play_items_intersects_enabled_and_atlas() {
        let catalog =
            ActionCatalog::load(&resources_config_dir()).expect("工程默认配置应可加载");
        // 图集仅覆盖三个启用动作 + 一个 disabled 动作（ACT-N-01 饿腹，批次 B）。
        let atlas = dp_assets::atlas::AtlasFile {
            version: 1,
            actions: vec![
                meta("ACT-M-01", "ACT-M-01_idle.png", 4),
                meta("ACT-M-02", "ACT-M-02_walk.png", 8),
                meta("ACT-E-01", "ACT-E-01_happy_spin.png", 6),
                meta("ACT-N-01", "ACT-N-01_hungry.png", 6),
            ],
        };
        let items = build_play_items(&catalog, &atlas);
        let ids: Vec<&str> = items.iter().map(|i| i.action_id.as_str()).collect();
        assert_eq!(ids, ["ACT-M-01", "ACT-M-02", "ACT-E-01"], "启用 ∩ 图集，保持目录序");
        assert!(
            items.iter().all(|i| catalog.is_enabled(&i.action_id)),
            "播放项不得包含 disabled 动作"
        );
        // 帧数与 fps 派生自图集/元数据。
        assert_eq!(items[0].frame_count, 4);
        assert_eq!(items[1].fps, 12, "ACT-M-02 行走 12fps（actions.json）");
    }

    #[test]
    fn fallback_items_derive_looping_from_atlas() {
        let atlas = dp_assets::atlas::AtlasFile {
            version: 1,
            actions: vec![
                meta("ACT-M-01", "ACT-M-01_idle.png", 4),
                meta("ACT-Z-00", "empty.png", 0),
            ],
        };
        let items = fallback_items_from_atlas(&atlas);
        assert_eq!(items.len(), 1, "frame_count=0 的动作不派生");
        assert_eq!(items[0].action_id, "ACT-M-01");
        assert!(items[0].looping, "降级项按整段循环");
        assert_eq!(items[0].fps, FpsTier::default().fps(), "降级帧率取 K-4 空闲档");
    }

    #[test]
    fn empty_play_items_report() {
        // 空图集 → 目录派生与降级派生均为空（调用方退出线程）。
        let atlas = dp_assets::atlas::AtlasFile::default();
        let catalog = ActionCatalog::default();
        assert!(build_play_items(&catalog, &atlas).is_empty());
        assert!(fallback_items_from_atlas(&atlas).is_empty());
    }

    #[test]
    fn frame_tier_handle_switches_and_defends() {
        let handle = FrameTierHandle::new(FpsTier::Idle);
        assert_eq!(handle.get(), FpsTier::Idle);

        handle.set(FpsTier::HighLoad);
        assert_eq!(handle.get(), FpsTier::HighLoad, "切档即时生效（tick 循环每 tick 读取）");

        handle.set(FpsTier::Active);
        assert_eq!(handle.get(), FpsTier::Active);

        // 克隆共享同一槽（app.state 与播放器线程共享）。
        let clone = handle.clone();
        handle.set(FpsTier::Sleep);
        assert_eq!(clone.get(), FpsTier::Sleep);
    }

    // -- S3-M6：ParticleCmd / MenuCmd v1（pet://fx / pet://menu 线上契约）--------

    #[test]
    fn particle_cmd_v1_roundtrip_is_camel_case_with_lowercase_kind() {
        let cmd = particle_cmd(ParticleKind::Dust, 8);
        let json = serde_json::to_string(&cmd).expect("v1 应可序列化");
        assert!(json.contains("\"kind\":\"dust\""), "{json}");
        assert!(json.contains("\"count\":8"), "{json}");
        assert!(json.contains("\"version\":1"), "{json}");
        assert!(!json.contains("screen_x"), "{json}");

        let back: ParticleCmd = serde_json::from_str(&json).expect("应可反序列化");
        assert_eq!(back, cmd);
    }

    #[test]
    fn particle_cmd_count_clamped_and_defaults_forward_compatible() {
        // 数量钳 [1, 60]：0→1、超限→上限。
        assert_eq!(particle_cmd(ParticleKind::Heart, 0).count, 1);
        assert_eq!(particle_cmd(ParticleKind::Heart, 500).count, PARTICLE_BURST_CAP);
        assert_eq!(particle_cmd(ParticleKind::Heart, 60).count, 60);

        // 缺字段取默认值（前向兼容基线）。
        let back: ParticleCmd = serde_json::from_str("{}").expect("缺字段应取默认");
        assert_eq!(back, ParticleCmd::default());
        assert_eq!(back.kind, ParticleKind::Heart);
        assert_eq!(back.count, 12);

        // 未知字段忽略（v2 不致解析失败）。
        let back: ParticleCmd =
            serde_json::from_str(r#"{"kind":"tear","count":2,"future":true}"#)
                .expect("未知字段应被容忍");
        assert_eq!(back.kind, ParticleKind::Tear);
        assert_eq!(back.count, 2);
    }

    // -- S4 前清障 B15-④：播放指令通道 -----------------------------------------

    /// 构造单个播放项的快捷工厂（tests 内最小 PlayItem）。
    fn play_item(id: &str, fps: u32, looping: bool, frames: u32) -> PlayItem {
        PlayItem {
            action_id: id.to_string(),
            fps,
            looping,
            loop_range: None,
            mirror: true,
            frame_count: frames,
            dwell_ms: 2_000,
        }
    }

    #[test]
    fn playback_channel_bounds_orders_counts_drops_and_keeps_fifo() {
        let channel = PlaybackChannel::default();
        for i in 0..(PLAYBACK_CHANNEL_CAP + 4) {
            channel.push_order(PlaybackOrder::Play { action_id: format!("ACT-{i:02}") });
        }
        assert_eq!(channel.dropped_orders(), 4, "超容量丢最旧并计数");

        // 丢最旧保最新：先弹出的是第 5 条（下标 4）。
        let first = channel.try_pop_order().expect("应有指令");
        assert_eq!(first, PlaybackOrder::Play { action_id: format!("ACT-{:02}", 4) });

        // 排空后恰剩 CAP 条，再取为 None。
        let mut count = 1;
        while channel.try_pop_order().is_some() {
            count += 1;
        }
        assert_eq!(count, PLAYBACK_CHANNEL_CAP, "队列容量恒有界");
        assert_eq!(channel.try_pop_order(), None);

        // Stop 也是合法指令（通道要能承载；播放器侧无覆盖时为 no-op）。
        channel.push_order(PlaybackOrder::Stop);
        assert_eq!(channel.try_pop_order(), Some(PlaybackOrder::Stop));
    }

    #[test]
    fn playback_channel_reports_drain_in_order_and_clone_shares() {
        let channel = PlaybackChannel::default();
        channel.push_report(PlaybackReport::Finished { action_id: "ACT-T-07".into() });
        channel.push_report(PlaybackReport::Finished { action_id: "ACT-T-08".into() });

        // 克隆共享同一底层队列（core-loop 与播放器两侧视图一致）。
        let mirror = channel.clone();
        let drained = mirror.drain_reports();
        assert_eq!(drained.len(), 2, "投递序 = 排空序");
        assert_eq!(drained[0], PlaybackReport::Finished { action_id: "ACT-T-07".into() });
        assert_eq!(drained[1], PlaybackReport::Finished { action_id: "ACT-T-08".into() });
        assert!(channel.drain_reports().is_empty(), "排空即清空（幂等）");
    }

    #[test]
    fn core_input_channel_bounds_counts_drops_and_drains_fifo() {
        let channel = CoreInputChannel::default();
        for _ in 0..(CORE_INPUT_CAP + 2) {
            channel.push(CoreInput::ResetEmotion);
        }
        assert_eq!(channel.dropped(), 2, "超容量丢最旧并计数");
        assert_eq!(channel.drain().len(), CORE_INPUT_CAP, "容量恒有界");
        assert!(channel.drain().is_empty(), "排空即清空（幂等）");

        channel.push(CoreInput::RecallRunaway);
        channel.push(CoreInput::ResetEmotion);
        assert_eq!(
            channel.drain(),
            vec![CoreInput::RecallRunaway, CoreInput::ResetEmotion],
            "投递序 = 排空序"
        );
    }

    #[test]
    fn handle_play_order_miss_reports_finished_and_hit_takes_override() {
        let channel = PlaybackChannel::default();
        let items = vec![play_item("ACT-M-01", 6, true, 4)];
        let mut over: Option<OverridePlay> = None;

        // 未命中：立即回报 Finished，防仲裁链卡死（静默吞掉会让链永久停摆）。
        handle_play_order(&channel, &items, "ACT-T-07", &mut over);
        assert!(over.is_none(), "未命中不得进入覆盖态");
        assert_eq!(
            channel.drain_reports(),
            vec![PlaybackReport::Finished { action_id: "ACT-T-07".into() }]
        );

        // 命中：覆盖起播（记录播放项下标），不回报。
        handle_play_order(&channel, &items, "ACT-M-01", &mut over);
        assert_eq!(over.as_ref().map(|o| o.index), Some(0));
        assert!(channel.drain_reports().is_empty());

        // 覆盖期再 Play：替换覆盖（最近指令优先），仍不回报。
        handle_play_order(&channel, &items, "ACT-M-01", &mut over);
        assert_eq!(over.as_ref().map(|o| o.index), Some(0));
        assert!(channel.drain_reports().is_empty());
    }

    #[test]
    fn menu_cmd_v1_roundtrip_and_coordinate_conversion() {
        // 2x 屏（scale=2.0）：物理 (1000, 500)，窗口左上 (900, 400) → 窗口内 CSS (50, 50)。
        let cmd = menu_cmd(900, 400, 2.0, 1000, 500);
        let json = serde_json::to_string(&cmd).expect("v1 应可序列化");
        assert!(json.contains("\"screenX\":1000"), "{json}");
        assert!(json.contains("\"localX\":50"), "{json}");
        assert!(!json.contains("screen_x"), "{json}");

        let back: MenuCmd = serde_json::from_str(&json).expect("应可反序列化");
        assert_eq!(back, cmd);

        // scale 非法防御：取 1.0（物理当 CSS，退化不 panic）。
        let degenerate = menu_cmd(900, 400, 0.0, 1000, 500);
        assert!((degenerate.local_x - 100.0).abs() < 1e-9);
        let nan = menu_cmd(900, 400, f32::NAN, 1000, 500);
        assert!((nan.local_y - 100.0).abs() < 1e-9);

        // 缺字段取默认值 + 未知字段忽略。
        let empty: MenuCmd = serde_json::from_str("{}").expect("缺字段应取默认");
        assert_eq!(empty, MenuCmd::default());
        let extra: MenuCmd = serde_json::from_str(r#"{"localX":7,"future":1}"#)
            .expect("未知字段应被容忍");
        assert!((extra.local_x - 7.0).abs() < 1e-9);
    }
}
