//! `dp-app/src/hit_latest.rs` —— S3-M2 像素命中 `HIT_LATEST` 数据源与双实现。
//!
//! 依据：`gate/arch-audit/2026-09-13-S3M2M3-实现设计.md` 裁定1~5（trait 落
//! dp-core / 掩码数据源方案A / HIT_LATEST 组合句柄 / 判定层双实现 / 次级热区
//! 三态数据流）+ 主理人修正（`HitTest::outcome` 三态覆写）。
//!
//! ## 结构（裁定3：组合而非扩展 `PetBBoxHandle`；端口在上、数据在 app）
//! - [`FrameCursor`]：帧快照（`Arc<AtomicU64>` 打包 `[mirror:1][pad:31][index:32]`，
//!   一次 Acquire load 得一致快照，避免 mirror/帧撕裂；`index` 高 16 位 = 动作
//!   条目下标、低 16 位 = 帧序号，`INDEX_NONE` = 未发布）；
//! - [`MaskStore`] / [`MaskEntry`]：启动后台线程一次性构建的**全量**帧掩码库
//!   （约 29-53 个 PNG 各解码一次；全量 ≈0.5MB 常驻，无 LRU——规模不值，裁定2）；
//!   条目不可变，读侧零锁；`base` = 条目下标（cursor 组合编码的动作段）；
//! - [`MaskHitSource`]：`dp-app` 自有类型组合 `dp-assets::HitMask` 实现
//!   `dp-core::interaction::hit::HitSource`（孤儿规则合规，dp-assets 零新依赖，
//!   workspace 依赖图零变化）；**掩码查询逻辑零复制**（F-05）；
//! - [`HitLatestHandle`]：bbox 粗筛（复用 `PetBBoxHandle` 4×`AtomicI32` relaxed 读）
//!   + 帧快照 + 掩码读的三态句柄；钩子回调热路径**三件套全零锁零分配**。
//!
//! ## 数据流（设计 §4）
//! 写路径A（启动）：`spawn_mask_build` 后台线程 → atlas.json+PNG → `MaskStore`
//! → `OnceLock::set`（一次性，写侧仅启动期）；
//! 写路径B（帧）：bridge 播放器线程 → [`HitFeed::publish`] → cursor Release store；
//! 写路径C（bbox）：coreloop render 档 → [`HitLatestHandle::store_bbox`]（既有口径）；
//! 读路径（钩子回调，dp-hook 线程）：`MSLLHOOKSTRUCT.pt` → 粗筛 → 局部换算 →
//! 比例映射（`frame_px = local_px × frame_w / bbox_w`，WebGLStage 1:1 整画布，
//! 支持 scalePercent）→ [`MaskHitSource::test`] → Hit/Hover/Miss。
//!
//! ## 回退语义（=S3-M1 行为，裁定2 / 设计 §3）
//! store 未就绪 / 未发布帧 / 动作或帧越界 / 掩码为空（解码失败）→ 一律回退
//! `Hit`（过粗筛即命中），与 [`dp_core::interaction::hit::BboxHitSource`] 行为
//! 一致——「切换 HitSource 实现，事件路由行为一致」（卡片验收）的降级面。
//!
//! ## 边界（红线）
//! C1 全部路径相对解析（atlas 目录候选链复用 `bridge`，无盘符字面量）；C9 零网络
//! （读本机资源）；F-05 掩码构建/查询全复用 dp-assets，零第二份位图逻辑；
//! 不新增 `pet://` 事件（C8）；根 Cargo.toml 零改动（零新增依赖/feature）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use dp_assets::atlas::{AtlasFile, AtlasMeta, Rect as AtlasRect};
use dp_assets::mask::{HitMask, MaskBuilder};
use dp_core::interaction::hit::{HitRect, HitResult, HitSource};

use crate::ports::PetBBoxHandle;

// Windows 专有：钩子命中端口覆写（真三态）。`FrameCursor` / `MaskStore` /
// `MaskHitSource` / `HitFeed` / 构建器本身跨平台可编译可测。
#[cfg(windows)]
use dp_platform::win::hook::{HitOutcome, HitTest};

// ---------------------------------------------------------------------------
// FrameCursor：帧快照（mirror + 组合帧索引，一次原子读）
// ---------------------------------------------------------------------------

/// 未发布帧哨兵（低 32 位全 1；动作/帧段均 `0xFFFF`，不与合法值冲突）。
pub const FRAME_INDEX_NONE: u32 = u32::MAX;

/// mirror 标记位（bit 63；设计打包 `[mirror:1][pad:31][index:32]`）。
const MIRROR_BIT: u64 = 1 << 63;

/// 低 32 位组合索引掩码（高 16 位 = 动作条目下标，低 16 位 = 帧序号）。
const INDEX_MASK: u64 = 0xFFFF_FFFF;

/// 帧游标（打包原子槽；写侧 Release、读侧 Acquire，一次 load 得一致快照）。
///
/// `index` 打包：`(action_index << 16) | frame_index`——`MaskHitSource` 据此
/// O(1) 定位条目与帧（次级热区同条目直取，避免平铺反查的 O(n)）。
#[derive(Clone)]
pub struct FrameCursor(Arc<AtomicU64>);

impl FrameCursor {
    /// 构造（初始未发布帧）。
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicU64::new(u64::from(FRAME_INDEX_NONE))))
    }

    /// 发布一帧快照（bridge 播放器写侧；`frame_index` 超 16 位由调用方防御）。
    pub fn publish(&self, action_index: u32, frame_index: u32, mirror: bool) {
        let mut packed = (u64::from(action_index) << 16) | u64::from(frame_index & 0xFFFF);
        if mirror {
            packed |= MIRROR_BIT;
        }
        self.0.store(packed, Ordering::Release);
    }

    /// 读取一致快照：`(组合索引, mirror)`；未发布 → `FRAME_INDEX_NONE`。
    #[must_use]
    pub fn snapshot(&self) -> (u32, bool) {
        let packed = self.0.load(Ordering::Acquire);
        ((packed & INDEX_MASK) as u32, packed & MIRROR_BIT != 0)
    }
}

impl Default for FrameCursor {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// MaskStore / MaskEntry：启动期一次性构建、进程期不可变的帧掩码库
// ---------------------------------------------------------------------------

/// 单动作掩码条目（帧掩码按图集帧序对齐；不可变）。
#[derive(Debug)]
struct MaskEntry {
    /// 动作 ID（RV-12 `ACT-` 前缀）。
    action_id: String,
    /// 条目下标（= cursor 组合编码的动作段；构建期自填，与 `base_of` 一致）。
    base: u32,
    /// 帧掩码（下标 = 图集帧序号；解码失败帧为空掩码占位，索引不错位）。
    frames: Vec<HitMask>,
    /// 单帧物理宽（atlas `frameW`，2x 导出 256）。
    frame_w: u32,
    /// 单帧物理高。
    frame_h: u32,
    /// 次级热区（帧像素域半开矩形；atlas `secondary` 同构换算，裁定5）。
    secondaries: Vec<HitRect>,
}

/// 全量帧掩码库（启动一次性构建；读侧经 `Arc<OnceLock<Arc<MaskStore>>>` 零锁读）。
///
/// `pub(crate)`：仅因 `spawn_mask_build` / `store_slot` / `MaskHitSource::new`
/// 签名需要；字段保持私有，外部只能经 [`MaskHitSource`] 与 [`HitLatestHandle`]
/// 间接读（实现细节不外泄 crate）。`Debug` 供 `OnceLock::set(...).expect(...)`
/// 单测断言（`Result::expect` 要求错误载荷 `Debug`）。
#[derive(Debug)]
pub(crate) struct MaskStore {
    /// 动作条目（下标即 cursor 动作段）。
    entries: Vec<MaskEntry>,
    /// 动作 ID → 条目下标（`HitFeed` 发布侧查表；与 `entry.base` 一致）。
    base_of: HashMap<String, u32>,
}

impl MaskStore {
    /// 由条目组装（派生 `base_of`；构建期保证 `base == 下标`）。
    fn from_entries(entries: Vec<MaskEntry>) -> Self {
        let base_of =
            entries.iter().map(|e| (e.action_id.clone(), e.base)).collect();
        Self { entries, base_of }
    }
}

/// 由图集元数据 + PNG 载入器构建掩码库（依赖注入缝隙：单测以假 loader 免落盘）。
///
/// `loader(png 文件名) → 解码后 RGBA 位图`；失败 → 该动作全帧空掩码（索引对齐，
/// 查询回退 bbox 行为，裁定2）。
fn build_store_with(
    atlas: &AtlasFile,
    loader: &dyn Fn(&str) -> Option<dp_assets::DecodedRgba>,
) -> MaskStore {
    let mut entries = Vec::with_capacity(atlas.actions.len());
    for meta in &atlas.actions {
        let mut entry = build_entry(meta, loader);
        // 回填条目下标（cursor 组合编码的动作段；与 from_entries 派生的 base_of 一致）。
        entry.base = entries.len() as u32;
        entries.push(entry);
    }
    MaskStore::from_entries(entries)
}

/// 构建单动作条目（PNG 缺失 / 帧矩形越界 → 空掩码占位，保持帧序对齐）。
fn build_entry(
    meta: &AtlasMeta,
    loader: &dyn Fn(&str) -> Option<dp_assets::DecodedRgba>,
) -> MaskEntry {
    let base = 0u32; // 由调用方 push 顺序决定；此处占位，from_entries 前统一回填。
    let secondaries = meta
        .secondary
        .iter()
        .map(|s| HitRect { x: s.x as i32, y: s.y as i32, w: s.w as i32, h: s.h as i32 })
        .collect();
    let Some(full) = loader(meta.source_png()) else {
        eprintln!(
            "[dp-app] hit_latest 动作 {} PNG 不可用，全帧回退 bbox 判定",
            meta.action_id
        );
        return MaskEntry {
            action_id: meta.action_id.clone(),
            base,
            frames: vec![HitMask::empty(); meta.frame_count as usize],
            frame_w: meta.frame_w,
            frame_h: meta.frame_h,
            secondaries,
        };
    };
    // F-05：掩码构建复用 dp-assets::mask::MaskBuilder（阈值取 atlas hit.threshold）。
    // S10：`pack` 存在时按包内坐标切片（source_frame_rect），否则等价旧 frame_rect。
    let threshold = meta.hit.threshold;
    let frames = (0..meta.frame_count)
        .map(|i| {
            match meta.source_frame_rect(i) {
                Ok(rect) => match extract_frame_rgba(&full, rect) {
                    Some(rgba) => MaskBuilder::from_rgba(&rgba, meta.frame_w, meta.frame_h, threshold),
                    None => HitMask::empty(),
                },
                Err(_) => HitMask::empty(), // 帧矩形越界（atlas 校验兜底，防御）
            }
        })
        .collect();
    MaskEntry {
        action_id: meta.action_id.clone(),
        base,
        frames,
        frame_w: meta.frame_w,
        frame_h: meta.frame_h,
        secondaries,
    }
}

/// 图集整图 → 单帧子矩形 RGBA 搬运（行拷贝；掩码算法仍全在 dp-assets，F-05）。
fn extract_frame_rgba(full: &dp_assets::DecodedRgba, rect: AtlasRect) -> Option<Vec<u8>> {
    if rect.w == 0 || rect.h == 0 {
        return None;
    }
    let x_end = rect.x.checked_add(rect.w)?;
    let y_end = rect.y.checked_add(rect.h)?;
    if x_end > full.width || y_end > full.height {
        return None;
    }
    let row_bytes = rect.w as usize * 4;
    let mut out = vec![0u8; row_bytes * rect.h as usize];
    for row in 0..rect.h {
        let src = ((rect.y + row) as usize * full.width as usize + rect.x as usize) * 4;
        let dst = row as usize * row_bytes;
        out[dst..dst + row_bytes].copy_from_slice(&full.data[src..src + row_bytes]);
    }
    Some(out)
}

/// 生产构建：读 `atlas_dir/atlas.json` + 各 PNG → [`MaskStore`]。
///
/// atlas.json 缺失 / 结构非法 → `None`（调用方不 set，命中永久 bbox 回退）；
/// 单 PNG 失败 → 该动作回退（裁定2）。
fn build_mask_store(atlas_dir: &Path) -> Option<Arc<MaskStore>> {
    let bytes = std::fs::read(atlas_dir.join("atlas.json")).ok()?;
    let atlas = AtlasFile::from_json_bytes(&bytes).ok()?;
    let loader = |name: &str| -> Option<dp_assets::DecodedRgba> {
        let png = std::fs::read(atlas_dir.join(name)).ok()?;
        dp_assets::decode_png_rgba(&png).ok()
    };
    Some(Arc::new(build_store_with(&atlas, &loader)))
}

/// 后台线程构建掩码库并写入 `store` 槽（启动装配点调用；一次性）。
///
/// 口径同 `supervisor::spawn`：**故意不标 `#[must_use]`**；线程启动失败降级
/// `None`（命中保持 bbox 回退，不 panic 主线程）。线程名 `dp-mask-build`。
/// `pub(crate)`：签名含实现私有的 [`MaskStore`] 槽类型，不外泄 crate。
pub(crate) fn spawn_mask_build(
    atlas_dir: PathBuf,
    store: Arc<OnceLock<Arc<MaskStore>>>,
) -> Option<JoinHandle<()>> {
    let spawned = std::thread::Builder::new()
        .name("dp-mask-build".to_string())
        .spawn(move || {
            let started = Instant::now();
            match build_mask_store(&atlas_dir) {
                Some(built) => {
                    let actions = built.entries.len();
                    let frames: usize = built.entries.iter().map(|e| e.frames.len()).sum();
                    if store.set(built).is_ok() {
                        eprintln!(
                            "[dp-app] hit_latest 掩码库就绪：actions={actions} frames={frames} 耗时={}ms",
                            started.elapsed().as_millis()
                        );
                    } else {
                        eprintln!("[dp-app] hit_latest 掩码库槽位已占用（重复构建防御），丢弃本次结果");
                    }
                }
                None => {
                    eprintln!("[dp-app] hit_latest 掩码库构建失败（atlas 缺失/损坏），命中保持 bbox 回退");
                }
            }
        });
    match spawned {
        Ok(handle) => Some(handle),
        Err(err) => {
            eprintln!("[dp-app] hit_latest 掩码构建线程启动失败，命中保持 bbox 回退：{err}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// MaskHitSource：dp-app 自有类型实现 dp-core HitSource（孤儿规则合规）
// ---------------------------------------------------------------------------

/// 掩码命中源（裁定1：组合 `dp-assets::HitMask` + 实现 dp-core trait；
/// store 未就绪 / 帧无效 / 空掩码 → 回退 `Hit`，=S3-M1 行为）。
pub struct MaskHitSource {
    /// 掩码库只读槽（写侧启动期 set 一次；`OnceLock::get` 为原子读）。
    store: Arc<OnceLock<Arc<MaskStore>>>,
    /// 当前帧快照（与句柄共享同一 `Arc<AtomicU64>`）。
    cursor: FrameCursor,
}

impl MaskHitSource {
    /// 构造（store 槽 + cursor 由句柄派生共享；`pub(crate)`——签名含实现私有的
    /// [`MaskStore`] 槽类型，生产构造走 [`HitLatestHandle::new`]）。
    #[must_use]
    pub(crate) fn new(store: Arc<OnceLock<Arc<MaskStore>>>, cursor: FrameCursor) -> Self {
        Self { store, cursor }
    }

    /// 解析当前帧快照对应条目（`(条目, 帧掩码, 组合索引, mirror)`；任一无效 → None）。
    fn resolve(&self) -> Option<(&MaskEntry, &HitMask)> {
        let store = self.store.get()?;
        let (index, _mirror) = self.cursor.snapshot();
        if index == FRAME_INDEX_NONE {
            return None;
        }
        let entry = store.entries.get((index >> 16) as usize)?;
        let mask = entry.frames.get((index & 0xFFFF) as usize)?;
        Some((entry, mask))
    }
}

impl HitSource for MaskHitSource {
    fn test(&self, frame_x: i32, frame_y: i32, mirror: bool) -> HitResult {
        // 回退链（设计 §3）：store 未就绪 / index 无效 → Hit（回退）。
        let Some((entry, mask)) = self.resolve() else {
            return HitResult::Hit;
        };
        if mask.is_empty() {
            // 空掩码 = 解码失败编码 → bbox 回退（裁定2），非 Miss。
            return HitResult::Hit;
        }
        if frame_x < 0 || frame_y < 0 {
            return HitResult::Miss; // 负坐标防御（比例映射钳界前兜底）
        }
        let (fx, fy) = (frame_x as u32, frame_y as u32);
        // 主热区：掩码位（镜像按 K-4 `frameW - 1 - x` 映射查询）。
        let hit = if mirror {
            mask.bit_at_mirrored(fx, fy, entry.frame_w)
        } else {
            mask.bit_at(fx, fy)
        };
        if hit {
            return HitResult::Hit;
        }
        // 次级热区（裁定5）：掩码位=0 但落在任一次级矩形 → Hover（悬停有效、
        // 点击落主判定）。镜像时屏幕 x 同步翻转（与掩码镜像口径一致）。
        let sx = if mirror && fx < entry.frame_w {
            entry.frame_w - 1 - fx
        } else {
            fx
        };
        if entry.secondaries.iter().any(|r| r.contains(sx as i32, frame_y)) {
            return HitResult::Hover;
        }
        HitResult::Miss
    }
}

// ---------------------------------------------------------------------------
// HitLatestHandle：钩子三态句柄（bbox 粗筛 + 帧快照 + 掩码读，零锁零分配）
// ---------------------------------------------------------------------------

/// `HIT_LATEST` 命中句柄（组合 [`PetBBoxHandle`] 粗筛 + 掩码精确判定）。
///
/// 钩子回调热路径三件套全零锁零分配：bbox 4×`AtomicI32` relaxed 读 → cursor
/// 一次 Acquire load → `OnceLock::get` + 不可变条目查询。写者：
/// bbox = coreloop render 档（[`HitLatestHandle::store_bbox`]）；
/// 帧 = bridge 播放器（[`HitLatestHandle::feed`]）。
///
/// `Clone`（全字段共享：`Arc` 槽 + `FrameCursor` 克隆共享同一原子槽）——装配点
/// 需要同一句柄克隆给 core-loop（写 bbox）、`HookService`（钩子读）、`app.manage`。
#[derive(Clone)]
pub struct HitLatestHandle {
    /// bbox 粗筛（唯一矩形真源仍在 `ports.rs`；组合复用，裁定3）。
    bbox: PetBBoxHandle,
    /// 掩码命中源（真三态判定）。
    source: Arc<MaskHitSource>,
    /// 帧快照（与 source、feed 共享同一 `Arc<AtomicU64>`）。
    cursor: FrameCursor,
    /// 掩码库槽（后台构建线程写侧 set 一次）。
    store: Arc<OnceLock<Arc<MaskStore>>>,
}

impl HitLatestHandle {
    /// 构造（bbox 句柄 + 自建 store 槽与 cursor；掩码库随后由 `spawn_mask_build` 填充）。
    #[must_use]
    pub fn new(bbox: PetBBoxHandle) -> Self {
        let store: Arc<OnceLock<Arc<MaskStore>>> = Arc::new(OnceLock::new());
        let cursor = FrameCursor::new();
        let source = Arc::new(MaskHitSource::new(Arc::clone(&store), cursor.clone()));
        Self { bbox, source, cursor, store }
    }

    /// 掩码库槽（装配点传给 `spawn_mask_build`；一次性写侧。`pub(crate)`——
    /// 签名含实现私有的 [`MaskStore`] 槽类型）。
    #[must_use]
    pub(crate) fn store_slot(&self) -> Arc<OnceLock<Arc<MaskStore>>> {
        Arc::clone(&self.store)
    }

    /// 写入 bbox 四分量（物理像素；coreloop render 档委托入口，既有口径不变）。
    pub fn store_bbox(&self, left: i32, top: i32, right: i32, bottom: i32) {
        self.bbox.store(left, top, right, bottom);
    }

    /// 帧推喂养侧（bridge 播放器每帧 publish；动作→索引映射由掩码库承载）。
    #[must_use]
    pub fn feed(&self) -> HitFeed {
        HitFeed {
            cursor: self.cursor.clone(),
            store: Arc::clone(&self.store),
            unknown: Arc::new(AtomicU64::new(0)),
            stale: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 三态命中判定（钩子回调唯一读点；粗筛 + 比例映射 + 精确判定一气呵成）。
    ///
    /// 回退序（=S3-M1 行为）：bbox 尺寸非法/粗筛未过 → `Miss`；store 未就绪 /
    /// 未发布帧 / 条目或帧越界 / 空掩码 → `Hit`。
    #[must_use]
    pub fn outcome_at(&self, x: i32, y: i32) -> HitOutcomeForTest {
        let (l, t, r, b) = self.bbox.load();
        let (bw, bh) = (i64::from(r - l), i64::from(b - t));
        if bw <= 0 || bh <= 0 || x < l || x >= r || y < t || y >= b {
            // 粗筛未过（半开区间，与 `PetBBoxHandle::contains` 逐位同式）→ Miss。
            return HitOutcomeForTest::Miss;
        }
        // 回退链：掩码不可用 → 过粗筛即 Hit（裁定2 / 设计 §3）。
        let Some(store) = self.store.get() else {
            return HitOutcomeForTest::Hit;
        };
        let (index, mirror) = self.cursor.snapshot();
        if index == FRAME_INDEX_NONE {
            return HitOutcomeForTest::Hit;
        }
        let entry = store.entries.get((index >> 16) as usize);
        let Some(entry) = entry else { return HitOutcomeForTest::Hit };
        let Some(mask) = entry.frames.get((index & 0xFFFF) as usize) else {
            return HitOutcomeForTest::Hit;
        };
        if mask.is_empty() {
            return HitOutcomeForTest::Hit;
        }
        if entry.frame_w == 0 || entry.frame_h == 0 {
            return HitOutcomeForTest::Hit;
        }
        // 比例映射（裁定3）：frame_px = local_px × frame_w / bbox_w（100% 缩放恒等；
        // i64 中间量防溢出，负 local 不会出现——粗筛已保证局部 ∈ [0, bbox)）。
        let fx = (i64::from(x - l) * i64::from(entry.frame_w) / bw) as i32;
        let fy = (i64::from(y - t) * i64::from(entry.frame_h) / bh) as i32;
        match self.source.test(fx, fy, mirror) {
            HitResult::Hit => HitOutcomeForTest::Hit,
            HitResult::Hover => HitOutcomeForTest::Hover,
            HitResult::Miss => HitOutcomeForTest::Miss,
        }
    }
}

/// 三态结果的跨平台镜像（`dp-platform::win::hook::HitOutcome` 仅 windows 编译；
/// 本类型让核心判定逻辑与单测跨平台可用，windows impl 一一映射）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitOutcomeForTest {
    /// 未命中。
    Miss,
    /// 仅次级热区。
    Hover,
    /// 主热区命中。
    Hit,
}

#[cfg(windows)]
impl HitTest for HitLatestHandle {
    fn contains(&self, x: i32, y: i32) -> bool {
        // 粗筛口径：三态判定非 Miss 即「落在命中域」（与 S3-M1 contains 语义衔接）。
        self.outcome_at(x, y) != HitOutcomeForTest::Miss
    }

    fn outcome(&self, x: i32, y: i32) -> HitOutcome {
        // 真三态覆写（主理人修正）：掩码 Hit/Hover/Miss 一一映射钩子路由枚举。
        match self.outcome_at(x, y) {
            HitOutcomeForTest::Miss => HitOutcome::Miss,
            HitOutcomeForTest::Hover => HitOutcome::Hover,
            HitOutcomeForTest::Hit => HitOutcome::Hit,
        }
    }
}

// ---------------------------------------------------------------------------
// HitFeed：bridge 播放器写侧（逐帧推喂）
// ---------------------------------------------------------------------------

/// 帧推喂养侧（`HitLatestHandle::feed()` 产出；bridge 播放器线程持有）。
///
/// `publish` 为帧率级（≤60Hz）路径：`OnceLock::get` + HashMap 查表 + 一次
/// Release store；掩码库未就绪 / 未知动作 / 帧号越界 → 计数丢弃（不阻塞、
/// 不分配、不 panic）。
#[derive(Clone)]
pub struct HitFeed {
    /// 帧游标（与句柄共享）。
    cursor: FrameCursor,
    /// 掩码库槽（动作 → 条目下标映射来源）。
    store: Arc<OnceLock<Arc<MaskStore>>>,
    /// 未知动作发布计数（诊断）。
    unknown: Arc<AtomicU64>,
    /// 掩码库未就绪 / 帧号越界丢弃计数（诊断）。
    stale: Arc<AtomicU64>,
}

impl HitFeed {
    /// 发布当前帧（`action_id` + 帧序号 + 镜像；未知动作静默丢弃并计数）。
    pub fn publish(&self, action_id: &str, frame_index: u32, mirror: bool) {
        let Some(store) = self.store.get() else {
            self.stale.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let Some(&action_index) = store.base_of.get(action_id) else {
            self.unknown.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if frame_index > 0xFFFF {
            self.stale.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.cursor.publish(action_index, frame_index, mirror);
    }

    /// 未知动作发布累计（诊断视图）。
    #[must_use]
    pub fn unknown_publishes(&self) -> u64 {
        self.unknown.load(Ordering::Relaxed)
    }

    /// 未就绪 / 越界丢弃累计（诊断视图）。
    #[must_use]
    pub fn stale_publishes(&self) -> u64 {
        self.stale.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// 单元测试（FrameCursor 打包 / 掩码三态 / 双实现回退一致 / 比例映射 / HitFeed）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dp_assets::atlas::{AnchorCfg, HitCfg, SecondaryRegionCfg};
    use dp_assets::DecodedRgba;

    /// 构造 32×16 alpha 图案 RGBA（**横排双帧**，帧宽 16）：两帧各自左上 4×4 点亮
    /// （255）、其余 0——容下 `frame_rect(0/1)` 两个子矩形（单帧图会令帧 1 越界成空掩码）。
    fn rgba_body() -> DecodedRgba {
        let mut data = vec![0u8; 32 * 16 * 4];
        for y in 0..4u32 {
            for x in 0..4u32 {
                data[((y * 32 + x) * 4 + 3) as usize] = 255; // 帧 0 左上
                data[((y * 32 + 16 + x) * 4 + 3) as usize] = 255; // 帧 1 左上
            }
        }
        DecodedRgba { width: 32, height: 16, data }
    }

    /// 单动作 atlas 元数据（frameW=16、次级热区右下 4×4）。
    fn meta(action_id: &str) -> AtlasMeta {
        AtlasMeta {
            action_id: action_id.to_string(),
            png: format!("{action_id}.png"),
            frame_w: 16,
            frame_h: 16,
            columns: 2,
            rows: 1,
            frame_count: 2,
            anchor: AnchorCfg::default(),
            hit: HitCfg { threshold: 32 },
            secondary: vec![SecondaryRegionCfg {
                name: "ear_r".to_string(),
                x: 12,
                y: 12,
                w: 4,
                h: 4,
            }],
            pack: None,
        }
    }

    /// 手工掩码库：动作 A 帧0/帧1 同图案（左上点亮）。
    fn store_with_body() -> Arc<MaskStore> {
        let atlas = AtlasFile { version: 1, actions: vec![meta("ACT-A")] };
        let body = rgba_body();
        let loader = |name: &str| -> Option<DecodedRgba> {
            if name == "ACT-A.png" { Some(body.clone()) } else { None }
        };
        Arc::new(build_store_with(&atlas, &loader))
    }

    /// 包图案（32×32，2 行 × 2 帧/行，帧 16×16）：仅**第 1 行**（y=16..20, x=0..4）点亮。
    fn pack_body() -> DecodedRgba {
        let mut data = vec![0u8; 32 * 32 * 4];
        for y in 16..20u32 {
            for x in 0..4u32 {
                data[((y * 32 + x) * 4 + 3) as usize] = 255; // 包第 1 行左上 4×4
            }
        }
        DecodedRgba { width: 32, height: 32, data }
    }

    #[test]
    fn pack_meta_mask_slices_from_pack_row() {
        // 动作落在包第 1 行（pack.row=1、行宽 2、共 2 行）：帧 0 应切出 y=16 行区，
        // 命中包内点亮的 4×4；若误按旧坐标（y=0）切，则掩码全空（本测守住分包切片）。
        let mut m = meta("ACT-A");
        m.png = "atlas-pack-0.png".to_string();
        m.pack = Some(dp_assets::atlas::PackRef {
            png: "atlas-pack-0.png".to_string(),
            columns: 2,
            rows: 2,
            row: 1,
        });
        let atlas = AtlasFile { version: 1, actions: vec![m] };
        let body = pack_body();
        let loader = |name: &str| -> Option<DecodedRgba> {
            if name == "atlas-pack-0.png" { Some(body.clone()) } else { None }
        };
        let store = build_store_with(&atlas, &loader);
        // 帧 0 掩码：应切出包第 1 行（y=16 段）→ 局部 (2,2) 命中点亮区。
        let frame0 = &store.entries[0].frames[0];
        assert!(frame0.bit_at(2, 2), "分包坐标切片应命中包第 1 行点亮区（y=16 段）");
        // 局部 (2,10) 在帧 16×16 外，回退 false（越界不 panic）；关键对照：
        // 若误按旧坐标（y=0，即包行 0）切，则帧 0 掩码将全空、本断言反向失败。
        assert!(!frame0.is_empty(), "帧 0 掩码不得为空（证明切到包行 1 而非空行 0）");
    }

    fn slot_with(store: Option<Arc<MaskStore>>) -> Arc<OnceLock<Arc<MaskStore>>> {
        let slot: Arc<OnceLock<Arc<MaskStore>>> = Arc::new(OnceLock::new());
        if let Some(s) = store {
            slot.set(s).expect("单测槽位一次性写入");
        }
        slot
    }

    // -- FrameCursor 打包 / 解包 -------------------------------------------------

    #[test]
    fn frame_cursor_pack_roundtrip_and_mirror() {
        let c = FrameCursor::new();
        assert_eq!(c.snapshot(), (FRAME_INDEX_NONE, false), "初始未发布");
        c.publish(3, 7, true);
        let (index, mirror) = c.snapshot();
        assert_eq!(index >> 16, 3, "高 16 位 = 动作下标");
        assert_eq!(index & 0xFFFF, 7, "低 16 位 = 帧序号");
        assert!(mirror, "镜像位");
        c.publish(0, 0, false);
        let (index, mirror) = c.snapshot();
        assert_eq!((index >> 16, index & 0xFFFF, mirror), (0, 0, false));
    }

    #[test]
    fn frame_cursor_clone_shares_slot() {
        let c = FrameCursor::new();
        let clone = c.clone();
        c.publish(1, 2, false);
        assert_eq!(clone.snapshot(), (1 << 16 | 2, false), "克隆共享同一原子槽");
    }

    // -- MaskStore 构建（注入 loader；成功 / 失败 / base 编码） -------------------

    #[test]
    fn build_store_maps_frames_and_secondaries() {
        let store = store_with_body();
        assert_eq!(store.entries.len(), 1);
        let entry = &store.entries[0];
        assert_eq!(entry.base, 0, "条目下标自填");
        assert_eq!(entry.frames.len(), 2, "帧掩码按 frameCount 对齐");
        assert_eq!(store.base_of.get("ACT-A"), Some(&0));
        // 帧0 左上点亮 → 掩码位命中。
        assert!(entry.frames[0].bit_at(2, 2));
        assert!(!entry.frames[0].bit_at(8, 8));
        // 次级热区换算（帧域半开矩形）。
        assert_eq!(entry.secondaries.len(), 1);
        assert!(entry.secondaries[0].contains(13, 13));
    }

    #[test]
    fn build_store_degrades_missing_png_to_empty_masks() {
        let atlas = AtlasFile { version: 1, actions: vec![meta("ACT-A"), meta("ACT-B")] };
        let loader =
            |name: &str| -> Option<DecodedRgba> { (name == "ACT-B.png").then(rgba_body) };
        let store = build_store_with(&atlas, &loader);
        // ACT-A PNG 缺失 → 全帧空掩码（索引对齐）；ACT-B 正常。
        assert!(store.entries[0].frames.iter().all(HitMask::is_empty));
        assert!(store.entries[1].frames.iter().all(|m| !m.is_empty()));
        assert_eq!(store.entries[1].base, 1, "base = 条目下标");
        assert_eq!(store.base_of.get("ACT-A"), Some(&0), "缺失动作仍可发布（回退语义）");
    }

    // -- MaskHitSource 三态（掩码 / 次级 / 回退 / 镜像） --------------------------

    fn source_on(store: Option<Arc<MaskStore>>, action: u32, frame: u32, mirror: bool) -> MaskHitSource {
        let cursor = FrameCursor::new();
        cursor.publish(action, frame, mirror);
        MaskHitSource::new(slot_with(store), cursor)
    }

    #[test]
    fn mask_hit_source_three_states() {
        let store = store_with_body();
        let s = source_on(Some(store), 0, 0, false);
        assert_eq!(s.test(2, 2, false), HitResult::Hit, "掩码位=1 → Hit");
        assert_eq!(s.test(13, 13, false), HitResult::Hover, "掩码=0 落次级矩形 → Hover");
        assert_eq!(s.test(8, 8, false), HitResult::Miss, "掩码=0 且无次级 → Miss");
        assert_eq!(s.test(999, 999, false), HitResult::Miss, "越界 → Miss（非回退）");
    }

    #[test]
    fn mask_hit_source_mirror_maps_secondary_and_mask() {
        let store = store_with_body();
        let s = source_on(Some(store), 0, 0, true);
        // 原图左上点亮 → 镜像后屏幕右上 (13,2) 命中。
        assert_eq!(s.test(13, 2, true), HitResult::Hit, "镜像掩码命中");
        assert_eq!(s.test(2, 2, true), HitResult::Miss, "原位镜像后无图案");
        // 次级矩形在原图右下 → 镜像后屏幕左下 (2,13)。
        assert_eq!(s.test(2, 13, true), HitResult::Hover, "镜像次级热区悬停有效");
    }

    #[test]
    fn mask_hit_source_fallbacks_match_bbox_hit_source() {
        // 双实现回退一致（卡片验收「切换 HitSource 实现行为一致」的降级面）：
        // store 未就绪 / 未发布帧 / 空掩码帧 → 与 BboxHitSource 同点同帧同结果。
        let fallback = dp_core::interaction::hit::BboxHitSource;
        let no_store = source_on(None, 0, 0, false);
        let no_frame = source_on(Some(store_with_body()), 9, 9, false);
        let empty_mask = {
            let atlas = AtlasFile { version: 1, actions: vec![meta("ACT-A")] };
            let loader = |name: &str| -> Option<DecodedRgba> { (name == "__").then(rgba_body) };
            let store = Arc::new(build_store_with(&atlas, &loader));
            source_on(Some(store), 0, 0, false)
        };
        for s in [&no_store, &no_frame, &empty_mask] {
            for (x, y) in [(0i32, 0i32), (8, 8), (255, 255)] {
                assert_eq!(s.test(x, y, false), fallback.test(x, y, false), "回退点 ({x},{y})");
                assert_eq!(s.test(x, y, true), fallback.test(x, y, true), "镜像回退点");
            }
        }
    }

    // -- HitLatestHandle：粗筛 / 比例映射 / 三态 / contains 口径 ------------------

    #[test]
    fn handle_miss_outside_bbox_and_hit_fallback_without_store() {
        let bbox = PetBBoxHandle::new();
        bbox.store(100, 200, 356, 456); // 256×256
        let handle = HitLatestHandle::new(bbox);
        // 粗筛外 → Miss（含边界右/下开区间）。
        assert_eq!(handle.outcome_at(99, 300), HitOutcomeForTest::Miss);
        assert_eq!(handle.outcome_at(356, 300), HitOutcomeForTest::Miss);
        assert_eq!(handle.outcome_at(200, 199), HitOutcomeForTest::Miss);
        assert_eq!(handle.outcome_at(200, 456), HitOutcomeForTest::Miss);
        // 粗筛内但 store 未就绪 / 无帧 → 回退 Hit（=S3-M1 行为）。
        assert_eq!(handle.outcome_at(120, 220), HitOutcomeForTest::Hit);
        assert!(handle.contains(120, 220));
        assert!(!handle.contains(0, 0), "contains = outcome != Miss");
    }

    #[test]
    fn handle_scales_local_to_frame_and_routes_three_states() {
        let bbox = PetBBoxHandle::new();
        bbox.store(100, 200, 356, 456); // 256×256 窗口、16×16 帧 → 比例 16:1
        let handle = HitLatestHandle::new(bbox);
        handle.store_slot().set(store_with_body()).expect("单测一次性 set");
        handle.feed().publish("ACT-A", 0, false);

        // frame = local × 16 / 256（整数除法）：
        // screen (102,202) → frame (0,0) → 掩码亮 → Hit。
        assert_eq!(handle.outcome_at(102, 202), HitOutcomeForTest::Hit);
        // screen (300,400) → local (200,200) → frame (12,12) → 掩码暗落次级矩形 → Hover。
        assert_eq!(handle.outcome_at(300, 400), HitOutcomeForTest::Hover);
        // screen (200,300) → local (100,100) → frame (6,6) → 掩码暗且无次级 → Miss。
        assert_eq!(handle.outcome_at(200, 300), HitOutcomeForTest::Miss);
    }

    #[test]
    fn handle_scales_frame_into_larger_window() {
        // 比例映射：窗口 512×512、帧 16×16 → 32px 屏幕 = 1px 帧（整数除法）。
        let bbox = PetBBoxHandle::new();
        bbox.store(0, 0, 512, 512);
        let handle = HitLatestHandle::new(bbox);
        handle.store_slot().set(store_with_body()).expect("单测一次性 set");
        handle.feed().publish("ACT-A", 0, false);
        // screen (4,4) → frame (0,0) → Hit；(200,4) → frame (6,0) → Miss（图案仅左上 4×4）。
        assert_eq!(handle.outcome_at(4, 4), HitOutcomeForTest::Hit);
        assert_eq!(handle.outcome_at(200, 4), HitOutcomeForTest::Miss);
        // screen (400,400) → frame (12,12) → 次级 Hover。
        assert_eq!(handle.outcome_at(400, 400), HitOutcomeForTest::Hover);
    }

    #[test]
    fn handle_degenerate_bbox_is_miss() {
        let bbox = PetBBoxHandle::new(); // 全零（未刷新）
        let handle = HitLatestHandle::new(bbox);
        assert_eq!(handle.outcome_at(0, 0), HitOutcomeForTest::Miss, "零尺寸 bbox 恒 Miss");
    }

    #[cfg(windows)]
    #[test]
    fn hook_hit_test_impl_overrides_outcome_to_true_three_states() {
        use dp_platform::win::hook::HitOutcome;
        let bbox = PetBBoxHandle::new();
        bbox.store(0, 0, 256, 256);
        let handle = HitLatestHandle::new(bbox);
        handle.store_slot().set(store_with_body()).expect("单测一次性 set");
        handle.feed().publish("ACT-A", 0, false);
        let dyn_hit: Arc<dyn HitTest> = Arc::new(handle);
        // 钩子路由读点：三态经 trait 对象产出（主理人修正）。
        // 256 窗 / 16 帧 → 比例 16:1：(2,2)→frame(0,0) Hit；(208,208)→frame(13,13) 次级
        // Hover；(96,96)→frame(6,6) Miss。
        assert_eq!(dyn_hit.outcome(2, 2), HitOutcome::Hit);
        assert_eq!(dyn_hit.outcome(208, 208), HitOutcome::Hover);
        assert_eq!(dyn_hit.outcome(96, 96), HitOutcome::Miss);
        assert!(!dyn_hit.contains(96, 96));
        assert!(dyn_hit.contains(2, 2));
    }

    // -- HitFeed：发布 / 未知动作 / 未就绪计数 -----------------------------------

    #[test]
    fn feed_publishes_known_actions_and_counts_unknown() {
        let store_slot = slot_with(Some(store_with_body()));
        let cursor = FrameCursor::new();
        let feed = HitFeed {
            cursor: cursor.clone(),
            store: Arc::clone(&store_slot),
            unknown: Arc::new(AtomicU64::new(0)),
            stale: Arc::new(AtomicU64::new(0)),
        };
        feed.publish("ACT-A", 1, true);
        // ACT-A 是条目 0：组合索引 = 0<<16 | 1 = 1（写成字面量 1 避免 identity_op）。
        assert_eq!(cursor.snapshot(), (1, true), "已知动作 → cursor 更新");
        feed.publish("ACT-UNKNOWN", 0, false);
        assert_eq!(feed.unknown_publishes(), 1, "未知动作计数丢弃");
        assert_eq!(cursor.snapshot().0, 1, "未知动作不改写 cursor");
        feed.publish("ACT-A", 0x1_0000, false);
        assert_eq!(feed.stale_publishes(), 1, "帧号超 16 位丢弃");
        // 未就绪槽位（独立 feed）。
        let empty_slot: Arc<OnceLock<Arc<MaskStore>>> = Arc::new(OnceLock::new());
        let cold = HitFeed {
            cursor: FrameCursor::new(),
            store: empty_slot,
            unknown: Arc::new(AtomicU64::new(0)),
            stale: Arc::new(AtomicU64::new(0)),
        };
        cold.publish("ACT-A", 0, false);
        assert_eq!(cold.stale_publishes(), 1, "掩码库未就绪丢弃");
    }

    // -- extract_frame_rgba（子矩形搬运边界） ------------------------------------

    #[test]
    fn extract_frame_rgba_copies_rows_and_defends_bounds() {
        // 4×2 全亮图，取右上 2×2 子矩形。
        let data = vec![255u8; 4 * 2 * 4];
        let full = DecodedRgba { width: 4, height: 2, data };
        let sub = extract_frame_rgba(&full, AtlasRect { x: 2, y: 0, w: 2, h: 2 }).expect("合法子矩形");
        assert_eq!(sub, vec![255u8; 2 * 2 * 4]);
        // 越界 / 零尺寸 → None。
        assert!(extract_frame_rgba(&full, AtlasRect { x: 3, y: 0, w: 2, h: 2 }).is_none());
        assert!(extract_frame_rgba(&full, AtlasRect { x: 0, y: 0, w: 0, h: 2 }).is_none());
    }

    // ---- QA 探针（清单④：FrameCursor 并发一致性 / 镜像边界列） ----

    #[test]
    fn qa_probe_frame_cursor_concurrent_publish_snapshot_never_tears() {
        use std::sync::atomic::AtomicBool;
        let cursor = FrameCursor::new();
        let reader_cursor = cursor.clone();
        let done = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&done);
        // 读线程（模拟钩子回调读点）：只做 Acquire load 快照。
        let reader = std::thread::spawn(move || {
            let mut seen = std::collections::HashSet::new();
            while !stop.load(Ordering::Relaxed) {
                seen.insert(reader_cursor.snapshot());
            }
            seen
        });
        // 写侧（模拟 bridge 播放器）：在两帧（含镜像翻转）间反复发布。
        for _ in 0..50_000 {
            cursor.publish(1, 2, false);
            cursor.publish(3, 4, true);
        }
        done.store(true, Ordering::Relaxed);
        let seen = reader.join().expect("读线程应正常结束");
        // 一致性：任何快照只能是「完整发布值」或初始哨兵——若 mirror 位与组合
        // 索引撕裂（不同源拼接），将观测到集合外的值。
        let allowed = [
            (FRAME_INDEX_NONE, false),
            ((1 << 16) | 2, false),
            ((3 << 16) | 4, true),
        ];
        for snap in &seen {
            assert!(allowed.contains(snap), "观测到撕裂快照：{snap:?}（合法集 {allowed:?}）");
        }
        assert!(seen.contains(&allowed[1]), "应观测到帧 (1,2) 非镜像");
        assert!(seen.contains(&allowed[2]), "应观测到帧 (3,4) 镜像");
    }

    #[test]
    fn qa_probe_mirror_boundary_columns_map_exactly_and_never_underflow() {
        // 次级热区贴原图最左列（x∈[0,2)）：镜像映射 sx = frameW-1-fx 的端点行为。
        let mut m = meta("ACT-A");
        m.secondary =
            vec![SecondaryRegionCfg { name: "ear_l".to_string(), x: 0, y: 12, w: 2, h: 4 }];
        let store = {
            let atlas = AtlasFile { version: 1, actions: vec![m] };
            let loader =
                |name: &str| -> Option<DecodedRgba> { (name == "ACT-A.png").then(rgba_body) };
            Arc::new(build_store_with(&atlas, &loader))
        };
        let mirrored = source_on(Some(Arc::clone(&store)), 0, 0, true);
        // 掩码镜像端点：屏幕最右列 fx=15 → 原图第 0 列（图案点亮）→ Hit。
        assert_eq!(mirrored.test(15, 2, true), HitResult::Hit, "fx=frameW-1 映射原图第 0 列");
        // 次级镜像端点：屏幕 fx=15 → sx=0 → 落次级矩形 → Hover。
        assert_eq!(mirrored.test(15, 13, true), HitResult::Hover, "sx=frameW-1-fx 端点映射");
        // fx=frameW（16）：守卫 fx < frameW 失败 → 不执行 frameW-1-fx（防下溢），
        // sx 原样越界 → Miss（且不得 panic）。
        assert_eq!(mirrored.test(16, 13, true), HitResult::Miss, "越界列不参与镜像映射");
        // 非镜像对照：原图最左两列直接落次级 → Hover。
        let plain = source_on(Some(store), 0, 0, false);
        assert_eq!(plain.test(0, 13, false), HitResult::Hover, "非镜像直接命中左列次级");
        assert_eq!(plain.test(1, 13, false), HitResult::Hover);
    }
}
