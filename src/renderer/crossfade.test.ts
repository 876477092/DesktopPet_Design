/**
 * 交叉淡入纯函数单测（S2-M2：固定 150ms 线性，卡片口径）。
 *
 * 覆盖：0/150ms 边界、新旧 alpha 互补（和恒为 1）、负值/非有限防御、fadeMs<=0。
 */
import { describe, expect, it } from 'vitest';

import { CROSSFADE_MS, crossfadeAlphas } from './crossfade';

describe('crossfadeAlphas（150ms 固定线性交叉淡入）', () => {
  it('默认时长为卡片口径的 150ms', () => {
    expect(CROSSFADE_MS).toBe(150);
  });

  it('起点（0ms）：旧帧 1.0 / 新帧 0.0', () => {
    expect(crossfadeAlphas(0)).toEqual({ prevAlpha: 1, nextAlpha: 0 });
  });

  it('中点（75ms）：0.5 / 0.5', () => {
    expect(crossfadeAlphas(75)).toEqual({ prevAlpha: 0.5, nextAlpha: 0.5 });
  });

  it('边界内（149ms）：线性互补且未结束', () => {
    const alphas = crossfadeAlphas(149);
    expect(alphas).not.toBeNull();
    expect(alphas?.prevAlpha).toBeCloseTo(1 / 150, 6);
    expect(alphas?.nextAlpha).toBeCloseTo(149 / 150, 6);
  });

  it('边界（150ms）及之后：null（淡入结束，回落单帧）', () => {
    expect(crossfadeAlphas(150)).toBeNull();
    expect(crossfadeAlphas(151)).toBeNull();
    expect(crossfadeAlphas(10_000)).toBeNull();
  });

  it('任意时刻新旧 alpha 之和恒为 1', () => {
    for (let ms = 0; ms <= 150; ms += 7) {
      const alphas = crossfadeAlphas(ms);
      if (alphas === null) {
        expect(ms).toBeGreaterThanOrEqual(150);
        continue;
      }
      expect(alphas.prevAlpha + alphas.nextAlpha).toBeCloseTo(1, 10);
    }
  });

  it('负 elapsed 视为起点（防御）', () => {
    expect(crossfadeAlphas(-1)).toEqual({ prevAlpha: 1, nextAlpha: 0 });
  });

  it('非有限输入返回 null（防御）', () => {
    expect(crossfadeAlphas(Number.NaN)).toBeNull();
    expect(crossfadeAlphas(Number.POSITIVE_INFINITY)).toBeNull();
  });

  it('fadeMs <= 0 或非有限：恒 null（无过渡硬切）', () => {
    expect(crossfadeAlphas(0, 0)).toBeNull();
    expect(crossfadeAlphas(0, -1)).toBeNull();
    expect(crossfadeAlphas(0, Number.NaN)).toBeNull();
    expect(crossfadeAlphas(0, Number.POSITIVE_INFINITY)).toBeNull();
  });

  it('自定义 fadeMs 生效（如未来 S9-M3 升级 250ms 的口子）', () => {
    expect(crossfadeAlphas(125, 250)).toEqual({ prevAlpha: 0.5, nextAlpha: 0.5 });
    expect(crossfadeAlphas(250, 250)).toBeNull();
  });
});
