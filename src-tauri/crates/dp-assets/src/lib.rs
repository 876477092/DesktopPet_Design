//! `dp-assets`：资源加载（图集 / 掩码 / LRU 缓存）。
//!
//! 承载内容（`02 §3`）：`atlas/mask/skelhit/cache`；本模块（S1-M5 / T-03）落地
//! `atlas.rs`（atlas.json 帧矩形解析）、`mask.rs`（alpha→1bit 掩码，下采样 2x）、
//! `cache.rs`（`LruAtlasCache` 字节上限驱逐）、`skelhit.rs`（骨骼 32×32 覆盖率表，
//! S9-M2 随 `scripts/gen-skeleton.mjs` 落地）。
//!
//! 契约要点（`03 §4.4` / `02 §4.4` / §5 K-2 / K-4）：
//!   - 图集 LRU 硬上限 **64MB**（C10，2026-09-13 现场修复：原 48MB → 64MB）；
//!   - 掩码 256×256 → 128×128 下采样 → **2KB/帧** 1bit 打包；
//!   - 镜像查询按 `frameW - x` 映射（K-4 行 1014），镜像一致性单测必测；
//!   - `skelhit` 为每 clip 的 32×32 覆盖率位图（4KB/clip），后续模块落地；
//!   - 资源只读目录为 `<install>\resources\`（`03 §0.1`）；代码中禁止盘符字面量（C1）。

pub mod atlas;
pub mod cache;
pub mod mask;
pub mod skelhit;

use thiserror::Error;

/// 资源加载错误（`#[non_exhaustive]`，`02 §7.4`）。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AssetError {
    /// atlas.json 内容无效（解析失败 / 字段非法）。
    #[error("atlas.json 无效：{0}")]
    AtlasJson(String),
    /// 帧索引越界。
    #[error("图集帧索引越界：index={index} frame_count={frame_count}")]
    FrameIndexOutOfRange {
        /// 越界索引。
        index: u32,
        /// 图集声明的帧总数。
        frame_count: u32,
    },
    /// PNG 解码失败。
    #[error("PNG 解码失败：{0}")]
    PngDecode(String),
    /// 帧尺寸不符（要求 256×256，见 `02 §11.1 Q-B` / §7.7-3）。
    #[error("无效帧尺寸：{w}×{h}（要求 256×256）")]
    InvalidFrameSize {
        /// 实际宽。
        w: u32,
        /// 实际高。
        h: u32,
    },
}

/// 解码后的 RGBA 位图（行主序，4 通道 8bit）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedRgba {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// RGBA 数据（`len == width * height * 4`）。
    pub data: Vec<u8>,
}

/// 解码 PNG 为 RGBA 位图。
///
/// 仅接受 PNG-32（RGBA8，`02 §7.7-3`）；其他色彩类型由 `image` crate 拒绝或
/// 归一化为 RGBA 后返回。
pub fn decode_png_rgba(png: &[u8]) -> Result<DecodedRgba, AssetError> {
    let img = image::load_from_memory(png).map_err(|err| AssetError::PngDecode(err.to_string()))?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(DecodedRgba { width, height, data: rgba.into_raw() })
}
