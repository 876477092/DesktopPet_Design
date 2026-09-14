/**
 * `computeFrameRect` 单测（S2-M2 补 vitest：帧子矩形换算纯函数）。
 *
 * 与 Rust `dp-assets::atlas::AtlasMeta::frame_rect` / `bridge::RenderFrameCmd::
 * frame_rect` 同布局语义（横向行主序），两端断言一致即镜像/布局一致性收口。
 */
import { describe, expect, it } from 'vitest';

import { computeFrameRect } from './AtlasCache';

describe('computeFrameRect（横向行主序图集子矩形）', () => {
  it('单行 8 列：index 0/3/7', () => {
    expect(computeFrameRect(0, 8, 1, 256, 256)).toEqual({ sx: 0, sy: 0, sw: 256, sh: 256 });
    expect(computeFrameRect(3, 8, 1, 256, 256)).toEqual({ sx: 768, sy: 0, sw: 256, sh: 256 });
    expect(computeFrameRect(7, 8, 1, 256, 256)).toEqual({ sx: 1792, sy: 0, sw: 256, sh: 256 });
  });

  it('多行（2 列 × 3 行）：index 4 = (0, 512)', () => {
    expect(computeFrameRect(4, 2, 3, 256, 256)).toEqual({ sx: 0, sy: 512, sw: 256, sh: 256 });
    expect(computeFrameRect(5, 2, 3, 256, 256)).toEqual({ sx: 256, sy: 512, sw: 256, sh: 256 });
  });

  it('越界 → null（调用方跳帧降级）', () => {
    expect(computeFrameRect(8, 8, 1, 256, 256)).toBeNull();
    expect(computeFrameRect(-1, 8, 1, 256, 256)).toBeNull();
  });

  it('非法布局（0 列 / 0 行 / 0 尺寸）→ null', () => {
    expect(computeFrameRect(0, 0, 1, 256, 256)).toBeNull();
    expect(computeFrameRect(0, 8, 0, 256, 256)).toBeNull();
    expect(computeFrameRect(0, 8, 1, 0, 256)).toBeNull();
    expect(computeFrameRect(0, 8, 1, 256, 0)).toBeNull();
  });

  it('非整数入参 → null（防御）', () => {
    expect(computeFrameRect(1.5, 8, 1, 256, 256)).toBeNull();
    expect(computeFrameRect(1, 8.5, 1, 256, 256)).toBeNull();
    expect(computeFrameRect(1, 8, 1, Number.NaN, 256)).toBeNull();
  });
});
