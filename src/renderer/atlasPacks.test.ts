/**
 * atlasPacks 分包规划单测（S9-M1 要点 2；S10 接入真实产出校验）。
 *
 * 冻结口径：批次 A 生存 29 动作 → 4 包；全量 53 动作 → 7 包（默认每包 8 动作，
 * 受帧回退图集单图 ≤2048×2048 硬约束：256×256 帧 ⇒ 8 行）。
 * S10 增量：读真实 `resources/atlas/atlas.json`，校验「分包产出 ↔ 前端切片契约」
 * 一致（包内行主序线性号经 `computeFrameRect` 还原为包内正确单元）。
 */
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import { computeFrameRect } from './AtlasCache';
import { packCount, planAtlasPacks } from './atlasPacks';

/** 生成 n 个动作 ID（ACT-001 … ACT-n）。 */
function actions(n: number): string[] {
  return Array.from({ length: n }, (_, i) => `ACT-${String(i + 1).padStart(3, '0')}`);
}

describe('planAtlasPacks（帧回退图集分包）', () => {
  it('批次 A 29 动作 → 4 包', () => {
    expect(packCount(actions(29))).toBe(4);
  });

  it('全量 53 动作 → 7 包', () => {
    expect(packCount(actions(53))).toBe(7);
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

describe('分包产出与前端切片契约一致（S10 集成）', () => {
  const atlasPath = fileURLToPath(new URL('../../resources/atlas/atlas.json', import.meta.url));

  /** 真实产出可能尚未生成（空仓库 CI）——用 it.skipIf 守卫。 */
  const hasAtlas = existsSync(atlasPath);

  interface PackRef {
    readonly png: string;
    readonly columns: number;
    readonly rows: number;
    readonly row: number;
  }
  interface Entry {
    readonly actionId: string;
    readonly png: string;
    readonly columns: number;
    readonly rows: number;
    readonly frameCount: number;
    readonly frameW: number;
    readonly frameH: number;
    readonly pack?: PackRef;
  }
  interface AtlasFile {
    readonly version: number;
    readonly actions: Entry[];
  }

  const atlas: AtlasFile | null = hasAtlas
    ? (JSON.parse(readFileSync(atlasPath, 'utf-8')) as AtlasFile)
    : null;

  it.skipIf(!hasAtlas)('每个动作均带 pack 子对象，包名与 planAtlasPacks 口径一致', () => {
    const file = atlas as AtlasFile;
    expect(file.actions.length).toBe(53);
    for (const a of file.actions) {
      expect(a.pack, `${a.actionId} 缺 pack`).toBeDefined();
      expect(a.pack?.png).toMatch(/^atlas-pack-\d+\.png$/);
      expect(a.png).toBe(a.pack?.png);
    }
    // 包总数 = 7（全量 53 / 每包 8）。
    const packs = new Set(file.actions.map((a) => a.pack?.png));
    expect(packs.size).toBe(7);
  });

  it.skipIf(!hasAtlas)('包内行主序线性号经 computeFrameRect 落到本动作所在行', () => {
    const file = atlas as AtlasFile;
    for (const a of file.actions) {
      const pack = a.pack as PackRef;
      for (let local = 0; local < a.frameCount; local++) {
        const linear = pack.row * pack.columns + local;
        const rect = computeFrameRect(linear, pack.columns, pack.rows, a.frameW, a.frameH);
        expect(rect, `${a.actionId}#${local} 应可切片`).not.toBeNull();
        // 期望列 = local（动作占本行第 0..frameCount-1 列），期望行 = pack.row。
        expect(rect?.sx).toBe(local * a.frameW);
        expect(rect?.sy).toBe(pack.row * a.frameH);
      }
    }
  });

  it.skipIf(!hasAtlas)('每包行宽 ≥ 包内各动作帧数（校验行内不溢出）', () => {
    const file = atlas as AtlasFile;
    const byPack = new Map<string, { columns: number; maxFrames: number }>();
    for (const a of file.actions) {
      const pack = a.pack as PackRef;
      const prev = byPack.get(pack.png) ?? { columns: pack.columns, maxFrames: 0 };
      prev.maxFrames = Math.max(prev.maxFrames, a.frameCount);
      expect(pack.columns).toBe(prev.columns);
      byPack.set(pack.png, prev);
    }
    for (const [png, info] of byPack) {
      expect(info.columns, `${png} 行宽`).toBeGreaterThanOrEqual(info.maxFrames);
    }
  });
});
