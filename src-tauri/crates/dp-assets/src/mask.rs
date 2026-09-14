//! 像素级命中掩码：alpha → 1bit，下采样 2x（`02 §5 K-2` / §4.4 行 876）。
//!
//! 口径（K-2 行 927）：`256×256 → 128×128 → 2KB/帧`——
//!   1. 源帧为 2x 导出 PNG（RGBA8，256×256）；
//!   2. 每 2×2 源像素块内**任一**像素 `alpha >= threshold` → 掩码位 1（保守命中，
//!      半透明边缘不漏判）；否则 0；
//!   3. 行主序 1bit 打包：位序 MSB-first，`128×128/8 = 2048` 字节/帧。
//!
//! 查询域：`bit_at(x, y)` / `bit_at_mirrored(x, y, frame_w)` 的 `x/y` 均为
//! **源 2x 帧像素坐标**（与钩子回调里的窗口局部物理坐标一致），内部除以 2 定位掩码位。
//!
//! 镜像（`02 §5 K-4` 行 1014）：`mirror=true` 时命中源按 `frameW - x` 映射；
//! 离散像素索引域下精确化为 `frameW - 1 - x`（保证镜像一致性单测逐位对应）。

use tracing::warn;

/// 下采样倍率（K-2：2x 源 → 掩码减半）。
const DOWNSAMPLE: u32 = 2;

/// 单帧命中掩码（1bit，下采样后 128×128 → 2048 字节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitMask {
    /// 源帧宽（物理像素）。
    src_w: u32,
    /// 源帧高（物理像素）。
    src_h: u32,
    /// 掩码宽（= src_w / 2）。
    mask_w: u32,
    /// 掩码高（= src_h / 2）。
    mask_h: u32,
    /// 1bit 打包数据（行主序，MSB-first）。
    bits: Vec<u8>,
}

impl HitMask {
    /// 空掩码（非法输入降级结果：任何查询都返回 false）。
    pub fn empty() -> Self {
        Self { src_w: 0, src_h: 0, mask_w: 0, mask_h: 0, bits: Vec::new() }
    }

    /// 掩码宽（下采样后）。
    pub fn mask_w(&self) -> u32 {
        self.mask_w
    }

    /// 掩码高（下采样后）。
    pub fn mask_h(&self) -> u32 {
        self.mask_h
    }

    /// 源帧宽。
    pub fn src_w(&self) -> u32 {
        self.src_w
    }

    /// 源帧高。
    pub fn src_h(&self) -> u32 {
        self.src_h
    }

    /// 打包字节数（256×256 源 → 2048）。
    pub fn packed_len(&self) -> usize {
        self.bits.len()
    }

    /// 是否为空掩码。
    pub fn is_empty(&self) -> bool {
        self.bits.is_empty()
    }

    /// 查询源帧坐标 `(x, y)` 是否命中。
    pub fn bit_at(&self, x: u32, y: u32) -> bool {
        if x >= self.src_w || y >= self.src_h {
            return false;
        }
        let mx = x / DOWNSAMPLE;
        let my = y / DOWNSAMPLE;
        self.mask_bit(mx, my)
    }

    /// 查询**镜像帧**坐标 `(x, y)` 是否命中（`02 §5 K-4`：按 `frameW - x` 映射）。
    ///
    /// 镜像渲染后物理位置 `x` 处的像素对应原图 `frameW - 1 - x`；
    /// `x >= frame_w` 或 `frame_w != src_w` 时返回 false。
    pub fn bit_at_mirrored(&self, x: u32, y: u32, frame_w: u32) -> bool {
        if frame_w == 0 || frame_w != self.src_w || x >= frame_w {
            return false;
        }
        self.bit_at(frame_w - 1 - x, y)
    }

    /// 查询掩码坐标位（越界 false）。
    fn mask_bit(&self, mx: u32, my: u32) -> bool {
        if mx >= self.mask_w || my >= self.mask_h {
            return false;
        }
        let index = (my * self.mask_w + mx) as usize;
        let byte = self.bits[index >> 3];
        (byte >> (7 - (index & 7))) & 1 == 1
    }
}

/// 掩码构建器。
#[derive(Debug, Clone, Copy, Default)]
pub struct MaskBuilder;

impl MaskBuilder {
    /// 从 RGBA 源数据构建命中掩码。
    ///
    /// 参数：`rgba`（`len == w * h * 4`，行主序 RGBA8）、`w` / `h`（源帧尺寸，
    /// 须为 2 的倍数）、`threshold`（alpha 命中阈值，`02 §7.7-3`：32）。
    ///
    /// 非法输入（长度不符 / 尺寸非 2 的倍数）→ 返回空掩码并 `warn`
    /// （`02 §7.4`：平台与资源失败一律降级不崩溃）。
    pub fn from_rgba(rgba: &[u8], w: u32, h: u32, threshold: u8) -> HitMask {
        if w == 0 || h == 0 || w % DOWNSAMPLE != 0 || h % DOWNSAMPLE != 0 {
            warn!("掩码构建失败：尺寸 {w}×{h} 必须为 {} 的倍数，降级为空掩码", DOWNSAMPLE);
            return HitMask::empty();
        }
        let expected = (w as usize) * (h as usize) * 4;
        if rgba.len() != expected {
            warn!(
                "掩码构建失败：数据长度 {} 与 {w}×{h}×4={expected} 不符，降级为空掩码",
                rgba.len()
            );
            return HitMask::empty();
        }

        let mask_w = w / DOWNSAMPLE;
        let mask_h = h / DOWNSAMPLE;
        let mut bits = vec![0u8; ((mask_w as usize) * (mask_h as usize)).div_ceil(8)];
        let row_bytes = (w as usize) * 4;

        for my in 0..mask_h {
            for mx in 0..mask_w {
                let mut hit = false;
                'block: for dy in 0..DOWNSAMPLE {
                    for dx in 0..DOWNSAMPLE {
                        let sx = (mx * DOWNSAMPLE + dx) as usize;
                        let sy = (my * DOWNSAMPLE + dy) as usize;
                        let alpha = rgba[sy * row_bytes + sx * 4 + 3];
                        if alpha >= threshold {
                            hit = true;
                            break 'block;
                        }
                    }
                }
                if hit {
                    let index = (my * mask_w + mx) as usize;
                    bits[index >> 3] |= 0x80 >> (index & 7);
                }
            }
        }

        HitMask { src_w: w, src_h: h, mask_w, mask_h, bits }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造纯 alpha 图案：`(x, y)` 处 alpha 为 `alpha_of(x, y)`，RGB 恒 0。
    fn rgba_with_alpha(w: u32, h: u32, alpha_of: impl Fn(u32, u32) -> u8) -> Vec<u8> {
        let mut data = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                data[((y * w + x) * 4 + 3) as usize] = alpha_of(x, y);
            }
        }
        data
    }

    /// K-2 口径：256×256 → 128×128 → 2048 字节/帧。
    #[test]
    fn packed_len_is_2048_bytes_for_256_frame() {
        let rgba = rgba_with_alpha(256, 256, |_, _| 255);
        let mask = MaskBuilder::from_rgba(&rgba, 256, 256, 32);
        assert_eq!(mask.mask_w(), 128);
        assert_eq!(mask.mask_h(), 128);
        assert_eq!(mask.packed_len(), 2048);
        assert!(!mask.is_empty());
        assert!(mask.bit_at(0, 0));
        assert!(mask.bit_at(255, 255));
        assert!(!mask.bit_at(256, 0));
    }

    /// 阈值边界（alpha=31 不命中 / 32 命中，threshold=32）。
    #[test]
    fn alpha_threshold_boundary() {
        let rgba = rgba_with_alpha(8, 8, |x, _| if x == 0 { 31 } else if x == 2 { 32 } else { 0 });
        let mask = MaskBuilder::from_rgba(&rgba, 8, 8, 32);
        // x∈[0,1] 掩码列 0：块内最大 alpha=31 < 32 → 不命中。
        assert!(!mask.bit_at(0, 0));
        assert!(!mask.bit_at(1, 0));
        // x∈[2,3] 掩码列 1：alpha=32 ≥ 32 → 命中。
        assert!(mask.bit_at(2, 0));
        assert!(mask.bit_at(3, 0));
        // 其余为 0。
        assert!(!mask.bit_at(4, 0));
    }

    /// 2×2 块内任一像素命中即整块命中（保守判定）。
    #[test]
    fn any_pixel_in_block_hits() {
        let rgba = rgba_with_alpha(8, 8, |x, y| if x == 3 && y == 3 { 200 } else { 0 });
        let mask = MaskBuilder::from_rgba(&rgba, 8, 8, 32);
        assert!(mask.bit_at(2, 2), "块 (2,2) 内 (3,3) 命中应传导");
        assert!(mask.bit_at(3, 3));
        assert!(!mask.bit_at(4, 2));
    }

    /// 镜像一致性（K-4：掩码镜像一致性单测必测）：不对称图案（仅左上点亮），
    /// 镜像查询与原图水平翻转逐位对应。
    #[test]
    fn mirrored_query_matches_horizontal_flip_bitwise() {
        // 16×16 源（掩码 8×8）：仅左上角 4×4 区域点亮（不对称）。
        let rgba = rgba_with_alpha(16, 16, |x, y| {
            if x < 4 && y < 4 {
                255
            } else if x >= 12 && y >= 12 {
                120 // 右下另一独立小斑，保证图案完全不对称
            } else {
                0
            }
        });
        let mask = MaskBuilder::from_rgba(&rgba, 16, 16, 32);
        assert_eq!(mask.mask_w(), 8);

        for y in 0..16u32 {
            for x in 0..16u32 {
                let mirrored = mask.bit_at_mirrored(x, y, 16);
                let flipped_source = mask.bit_at(15 - x, y);
                assert_eq!(
                    mirrored, flipped_source,
                    "镜像查询 ({x},{y}) 应等于原图翻转位 ({},{})",
                    15 - x,
                    y
                );
            }
        }

        // 语义抽查：左上图案镜像后出现在右上。
        assert!(mask.bit_at_mirrored(15, 0, 16), "左上点亮经镜像应在右上命中");
        assert!(!mask.bit_at_mirrored(0, 0, 16), "原左上位置镜像后应无图案");
    }

    /// 非法输入 → 空掩码 + 不 panic（降级不崩，02 §7.4）。
    #[test]
    fn invalid_input_degrades_to_empty_mask() {
        let short = vec![0u8; 8 * 8 * 4 - 1];
        assert!(MaskBuilder::from_rgba(&short, 8, 8, 32).is_empty());
        // 尺寸非 2 的倍数。
        let odd = vec![0u8; 7 * 7 * 4];
        assert!(MaskBuilder::from_rgba(&odd, 7, 7, 32).is_empty());
        // 空输入。
        assert!(MaskBuilder::from_rgba(&[], 0, 0, 32).is_empty());
    }
}
