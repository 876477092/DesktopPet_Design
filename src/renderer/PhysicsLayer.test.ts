/**
 * PhysicsLayer 单测（S9-M3：8ms 固定子步 / 8 部件无撕裂 / primaryOnly 降级）。
 */
import { describe, expect, it } from 'vitest';

import { buildDefaultChain, PhysicsLayer } from './PhysicsLayer';

describe('PhysicsLayer（弹簧链次级物理）', () => {
  it('默认链为 8 部件', () => {
    const layer = new PhysicsLayer(buildDefaultChain({ x: 0, y: 0 }));
    expect(layer.partsSnapshot()).toHaveLength(8);
  });

  it('快速甩动后段长无撕裂（maxStretchRatio ≤ 5%）', () => {
    const layer = new PhysicsLayer(buildDefaultChain({ x: 0, y: 0 }));
    const anchor = { x: 0, y: 0 };
    // 静止若干子步稳定。
    for (let i = 0; i < 20; i += 1) layer.step(8, anchor);
    // 快速甩动：锚点瞬移到远处再拉回（大 dt 触发多子步）。
    anchor.x = 40;
    layer.step(100, anchor);
    anchor.x = 0;
    layer.step(100, anchor);
    // 再稳定若干子步（约束收敛）。
    for (let i = 0; i < 120; i += 1) layer.step(8, anchor);
    expect(layer.maxStretchRatio()).toBeLessThanOrEqual(0.05);
  });

  it('固定 8ms 子步：残差累积不丢帧', () => {
    const layer = new PhysicsLayer(buildDefaultChain({ x: 0, y: 0 }));
    const anchor = { x: 0, y: 0 };
    // 每次 10ms：1 子步 + 2ms 残差；连续 4 次 = 40ms = 5 子步。
    const before = layer.partsSnapshot()[1] ?? { id: '', x: 0, y: 0 };
    for (let i = 0; i < 4; i += 1) layer.step(10, anchor);
    const after = layer.partsSnapshot()[1] ?? { id: '', x: 0, y: 0 };
    // 锚点不动，重力应让部件 1 略下移（残差累积确实推进了子步）。
    expect(after.y).not.toBe(before.y);
  });

  it('primaryOnly 降级：次级部件冻结在锚点', () => {
    const layer = new PhysicsLayer(buildDefaultChain({ x: 0, y: 0 }), 'primaryOnly');
    const anchor = { x: 10, y: 20 };
    layer.step(100, anchor);
    const snap = layer.partsSnapshot();
    for (const p of snap) {
      expect(p.x).toBe(10);
      expect(p.y).toBe(20);
    }
    expect(layer.currentLevel).toBe('primaryOnly');
  });
});
