/**
 * atlasPacks 分包规划单测（S9-M1 要点 2）。
 *
 * 冻结口径：批次 A 生存 29 动作 → 4 包；全量 53 动作 → 6 包（默认每包 9 动作）。
 */
import { describe, expect, it } from 'vitest';

import { packCount, planAtlasPacks } from './atlasPacks';

/** 生成 n 个动作 ID（ACT-001 … ACT-n）。 */
function actions(n: number): string[] {
  return Array.from({ length: n }, (_, i) => `ACT-${String(i + 1).padStart(3, '0')}`);
}

describe('planAtlasPacks（帧回退图集分包）', () => {
  it('批次 A 29 动作 → 4 包', () => {
    expect(packCount(actions(29))).toBe(4);
  });

  it('全量 53 动作 → 6 包', () => {
    expect(packCount(actions(53))).toBe(6);
  });

  it('分包均衡：末包不超过每包上限，且目录序稳定', () => {
    const assigned = planAtlasPacks(actions(20), 9);
    // 20 / 9 → 包 0(9) 包 1(9) 包 2(2)。
    const byPack = [0, 0, 0];
    for (const a of assigned) {
      byPack[a.packIndex] = (byPack[a.packIndex] ?? 0) + 1;
    }
    expect(byPack).toEqual([9, 9, 2]);
    expect(assigned[0]?.packName).toBe('atlas-pack-0.png');
    expect(assigned[assigned.length - 1]?.packIndex).toBe(2);
  });

  it('空输入 → 0 包；非法每包数回退默认', () => {
    expect(packCount([])).toBe(0);
    expect(planAtlasPacks(actions(5), 0)).toHaveLength(5);
    expect(planAtlasPacks([], 9)).toHaveLength(0);
  });
});
