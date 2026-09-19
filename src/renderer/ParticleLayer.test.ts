/**
 * 粒子层测试（S3-M6，T-10 段 · 下）。
 *
 * 覆盖：纯逻辑（钳制 / 确定性 LCG / 迸发 / 推进 / 渲染快照）+ 层编排
 * （惰性 flush / 无脏早退 / 寿命清场 / 上限 60）。**零 DOM**——视图注入
 * Fake 记录调用；**注入单调时钟**（FakeClock，禁墙钟 C3）；**零 setTimeout**。
 */
import { describe, expect, it } from 'vitest';

import type { ParticleCmdV1 } from '../shared/ipc';
import { PARTICLE_BURST_CAP } from '../shared/ipc';
import { ParticleLayer } from './ParticleLayer';
import {
  PARTICLE_ANCHOR_CENTER_X,
  PARTICLE_ANCHOR_OFFSET_X,
  PARTICLE_LIFETIME_MS,
  PET_LOGICAL_WIDTH,
  type ParticleRenderItem,
  type ParticleView,
} from './layerPorts';
import {
  advanceParticles,
  clampBurstCount,
  createRng,
  renderItems,
  spawnBurst,
  type Particle,
  type ParticleAnchor,
} from './particleLogic';

/** 手动单调时钟（帧 tick 模拟；无 setTimeout、无墙钟）。箭头属性绑定 this（可安全解构传参）。 */
class FakeClock {
  private t = 0;
  now = (): number => this.t;
  /** 推进（LayerHost 帧 tick 的测试替身）。 */
  tick = (ms: number): void => {
    this.t += ms;
  };
}

/** 假粒子视图：记录每次 render 快照（不碰 DOM）。 */
class FakeParticleView implements ParticleView {
  readonly snapshots: ParticleRenderItem[][] = [];
  render(items: readonly ParticleRenderItem[]): void {
    this.snapshots.push([...items]);
  }
}

/** 迸发命令快捷工厂。 */
function fx(kind: ParticleCmdV1['kind'], count: number): ParticleCmdV1 {
  return { version: 1, kind, count };
}

const ORIGIN: ParticleAnchor = { x: 100, y: 40 };

describe('particleLogic 纯逻辑', () => {
  it('clampBurstCount：非有限→1；截断整数后钳 [1, 60]', () => {
    expect(clampBurstCount(Number.NaN)).toBe(1);
    expect(clampBurstCount(Number.POSITIVE_INFINITY)).toBe(1);
    expect(clampBurstCount(0)).toBe(1);
    expect(clampBurstCount(-5)).toBe(1);
    expect(clampBurstCount(3.9)).toBe(3);
    expect(clampBurstCount(PARTICLE_BURST_CAP + 1)).toBe(PARTICLE_BURST_CAP);
  });

  it('createRng：同种子序列完全确定（无 Math.random 不可测性）', () => {
    const a = createRng(42);
    const b = createRng(42);
    const seqA = [a(), a(), a(), a()];
    const seqB = [b(), b(), b(), b()];
    expect(seqA).toEqual(seqB);
    for (const v of seqA) {
      expect(v).toBeGreaterThanOrEqual(0);
      expect(v).toBeLessThan(1);
    }
    expect(createRng(1)()).not.toBe(createRng(2)());
  });

  it('createRng：种子 0 不退化全零（先混一步）', () => {
    const rng = createRng(0);
    const first = rng();
    expect(first).toBeGreaterThan(0);
  });

  it('spawnBurst：数量、id 连续、锚点透传、bornAt 注入', () => {
    const rng = createRng(7);
    const burst: Particle[] = spawnBurst({ kind: 'heart', count: 5 }, ORIGIN, 1000, rng, 11);
    expect(burst).toHaveLength(5);
    expect(burst.map((p) => p.id)).toEqual([11, 12, 13, 14, 15]);
    for (const p of burst) {
      expect(p.x0).toBe(ORIGIN.x);
      expect(p.y0).toBe(ORIGIN.y);
      expect(p.bornAt).toBe(1000);
      expect(p.kind).toBe('heart');
    }
  });

  it('heart 初速向上（vy < 0，爱心上浮）；dust 带重力（落地尘土回落）', () => {
    const hearts = spawnBurst({ kind: 'heart', count: 20 }, ORIGIN, 0, createRng(3), 1);
    for (const p of hearts) {
      expect(p.vy).toBeLessThan(0);
      expect(p.gravity).toBe(0);
    }
    const dusts = spawnBurst({ kind: 'dust', count: 20 }, ORIGIN, 0, createRng(3), 1);
    for (const p of dusts) {
      expect(p.gravity).toBeGreaterThan(0);
      expect(p.vy).toBeLessThanOrEqual(0); // 上半圆溅起
    }
  });

  it('advanceParticles：寿命（1.2s）到期剔除，未到期保留', () => {
    const burst = spawnBurst({ kind: 'star', count: 4 }, ORIGIN, 0, createRng(9), 1);
    expect(advanceParticles(burst, PARTICLE_LIFETIME_MS - 1)).toHaveLength(4);
    expect(advanceParticles(burst, PARTICLE_LIFETIME_MS)).toHaveLength(0);
    expect(PARTICLE_LIFETIME_MS).toBe(1200);
  });

  it('renderItems：位置含重力二次项（dust 下坠）、opacity 随年龄线性衰减且钳 [0,1]', () => {
    const burst = spawnBurst({ kind: 'dust', count: 1 }, ORIGIN, 0, createRng(5), 1);
    const dust = burst[0];
    if (dust === undefined) {
      throw new Error('spawnBurst(count=1) 应产出恰 1 颗粒子');
    }
    const t = 0.5; // 0.5s
    const items = renderItems([dust], 500);
    expect(items[0]?.x).toBeCloseTo(dust.x0 + dust.vx * t, 6);
    expect(items[0]?.y).toBeCloseTo(dust.y0 + dust.vy * t + 0.5 * dust.gravity * t * t, 6);
    expect(items[0]?.opacity).toBeCloseTo(1 - (500 / PARTICLE_LIFETIME_MS), 6);

    // 超龄粒子透明度钳 0（advance 已剔除，此处防御直调）。
    const stale = renderItems([dust], PARTICLE_LIFETIME_MS * 2);
    expect(stale[0]?.opacity).toBe(0);
  });
});

describe('ParticleLayer 层编排（惰性 flush + 帧 tick 驱动）', () => {
  it('submit 不写 view（置脏）；无脏 flush 早退（零 view 写入）', () => {
    const clock = new FakeClock();
    const view = new FakeParticleView();
    const layer = new ParticleLayer(view, { now: clock.now, seed: () => 7 });

    layer.flush(); // 未 submit：无脏早退
    expect(view.snapshots).toHaveLength(0);

    layer.submit(fx('heart', 6)); // 只置内部状态
    expect(view.snapshots).toHaveLength(0);

    layer.flush(); // 置脏后才写
    expect(view.snapshots).toHaveLength(1);
    expect(view.snapshots[0]).toHaveLength(6);
  });

  it('存活期逐帧 flush 推进（数量不变、位置随 tick 变化）', () => {
    const clock = new FakeClock();
    const view = new FakeParticleView();
    const layer = new ParticleLayer(view, { now: clock.now, seed: () => 7 });
    layer.submit(fx('heart', 4));

    layer.flush();
    const first = view.snapshots[0] ?? [];
    clock.tick(100);
    layer.flush();
    const second = view.snapshots[1] ?? [];

    expect(view.snapshots).toHaveLength(2);
    expect(second).toHaveLength(4);
    // heart 上浮：Y 随 tick 减小（首个粒子对比）。
    expect(second[0]?.y).toBeLessThan(first[0]?.y ?? Number.POSITIVE_INFINITY);
    expect(layer.liveCount()).toBe(4);
  });

  it('寿命到期：最后一次 flush 恰写一次空列表清场，随后回无脏早退态（幂等）', () => {
    const clock = new FakeClock();
    const view = new FakeParticleView();
    const layer = new ParticleLayer(view, { now: clock.now, seed: () => 7 });
    layer.submit(fx('star', 3));
    layer.flush();
    expect(layer.liveCount()).toBe(3);

    clock.tick(PARTICLE_LIFETIME_MS); // 越过寿命
    layer.flush();
    expect(layer.liveCount()).toBe(0);
    expect(view.snapshots).toHaveLength(2);
    expect(view.snapshots[1]).toHaveLength(0); // 恰一次清场

    layer.flush(); // 全灭后无脏早退
    layer.flush();
    expect(view.snapshots).toHaveLength(2);
  });

  it('单次迸发上限 60：submit 大 count 后活粒子数恰为上限', () => {
    const clock = new FakeClock();
    const view = new FakeParticleView();
    const layer = new ParticleLayer(view, { now: clock.now, seed: () => 7 });
    layer.submit(fx('anger', 500));
    expect(layer.liveCount()).toBe(PARTICLE_BURST_CAP);
  });

  it('id 跨迸发自增（DOM 差量收敛键唯一）；两迸发可叠加共存', () => {
    const clock = new FakeClock();
    const view = new FakeParticleView();
    const layer = new ParticleLayer(view, { now: clock.now, seed: () => 7 });
    layer.submit(fx('heart', 3));
    layer.submit(fx('tear', 2));
    layer.flush();
    const items = view.snapshots[0] ?? [];
    expect(items).toHaveLength(5);
    expect(new Set(items.map((it) => it.id)).size).toBe(5);
  });

  it('默认锚点：视口 CSS 中线 64 + 头顶偏右 24 / 顶部 32（待真机标定）；锚点选项可覆盖', () => {
    const clock = new FakeClock();
    const view = new FakeParticleView();
    const layer = new ParticleLayer(view, { now: clock.now, seed: () => 7 });
    layer.submit(fx('heart', 1));
    layer.flush();
    const item = view.snapshots[0]?.[0];
    // renderItems 快照 t=0 时位置即锚点。
    expect(item?.x).toBe(PARTICLE_ANCHOR_CENTER_X + PARTICLE_ANCHOR_OFFSET_X);
    expect(item?.x).toBe(64 + 24);
    expect(item?.y).toBe(32);

    const custom: ParticleAnchor = { x: 55, y: 66 };
    const view2 = new FakeParticleView();
    const layer2 = new ParticleLayer(view2, {
      now: clock.now,
      seed: () => 7,
      anchor: () => custom,
    });
    layer2.submit(fx('heart', 1));
    layer2.flush();
    expect(view2.snapshots[0]?.[0]?.x).toBe(55);
    expect(view2.snapshots[0]?.[0]?.y).toBe(66);
  });

  it('几何自洽：默认锚点必须落在视口 CSS 宽内（S10 回归护栏，防 128/256 域混用）', () => {
    // 视口 CSS 宽 = 窗口物理 256 ÷ DPR 2 = 128（`PET_LOGICAL_WIDTH` 即此视口宽）。
    // 锚点 x = 中线 + 偏移；若中线误取 128（历史 Bug），x=152 > 128 → 粒子整批不可见。
    const anchoredX = PARTICLE_ANCHOR_CENTER_X + PARTICLE_ANCHOR_OFFSET_X;
    expect(anchoredX).toBeLessThan(PET_LOGICAL_WIDTH);
    // 中线本身也必须在视口内（严格小于右边界）。
    expect(PARTICLE_ANCHOR_CENTER_X).toBeLessThan(PET_LOGICAL_WIDTH);
    expect(PARTICLE_ANCHOR_CENTER_X).toBe(PET_LOGICAL_WIDTH / 2);
  });
});
