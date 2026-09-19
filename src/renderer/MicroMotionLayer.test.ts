/**
 * MicroMotionLayer 单测（S9-M3：5~12s 微动 / 60s 不重复 / 眨眼 2~6s 非等间隔）。
 */
import { describe, expect, it } from 'vitest';

import { BlinkScheduler, MicroMotionScheduler, type MicroItem } from './MicroMotionLayer';

/** 构造循环 rng（按序列循环取 [0,1)）。 */
function seqRng(values: number[]): () => number {
  let i = 0;
  return () => {
    const v = values[i % values.length] ?? 0;
    i += 1;
    return v;
  };
}

const ITEMS: MicroItem[] = [
  { id: 'ear_twitch', clip: 'anim_micro_ear', weight: 25 },
  { id: 'tail_swing', clip: 'anim_micro_tail', weight: 20 },
  { id: 'weight_shift', clip: 'anim_micro_shift', weight: 15 },
  { id: 'hum', clip: 'anim_micro_hum', weight: 10 },
  { id: 'look_around', clip: 'anim_micro_look', weight: 8 },
];

describe('MicroMotionScheduler（待机微动）', () => {
  it('5 分钟观察 ≥3 种微动', () => {
    // rng 固定取 0.5 → 间隔固定 8.5s（5~12 中点），便于确定性观察。
    const rng = seqRng([0.5]);
    const sched = new MicroMotionScheduler(
      { intervalSec: [5, 12], noRepeatSec: 60, items: ITEMS },
      rng,
      0,
    );
    const seen = new Set<string>();
    for (let now = 0; now <= 300_000; now += 1_000) {
      const ev = sched.poll(now);
      if (ev) seen.add(ev.id);
    }
    expect(seen.size).toBeGreaterThanOrEqual(3);
  });

  it('60s 内不重复同一种微动（滑窗）', () => {
    const rng = seqRng([0.5]);
    const sched = new MicroMotionScheduler(
      { intervalSec: [5, 12], noRepeatSec: 60, items: ITEMS },
      rng,
      0,
    );
    const picks: { id: string; at: number }[] = [];
    let now = 0;
    for (let k = 0; k < 12; k += 1) {
      const ev = sched.poll(now);
      if (ev) {
        // 本次选中项不应在「最近 60s 内」出现过。
        const dup = picks.some((p) => p.id === ev.id && now - p.at < 60_000);
        expect(dup).toBe(false);
        picks.push({ id: ev.id, at: now });
      }
      now += 8_500; // 固定间隔 8.5s
    }
  });
});

describe('BlinkScheduler（眨眼非等间隔）', () => {
  it('连续 10 次眨眼间隔不全等（禁止等间隔）', () => {
    // rng 用变化序列，保证间隔随机。
    const rng = seqRng([0.1, 0.3, 0.5, 0.7, 0.9, 0.2, 0.4, 0.6, 0.8, 0.25]);
    const blink = new BlinkScheduler({ intervalMs: [2000, 6000] }, rng, 0);
    const intervals: number[] = [];
    let now = 0;
    for (let k = 0; k < 10; k += 1) {
      // 快进到下一次眨眼。
      now = blink.nextAt;
      expect(blink.poll(now)).toBe(true);
      intervals.push(blink.lastIntervalMs);
    }
    // 全部落在 [2000,6000]。
    for (const iv of intervals) {
      expect(iv).toBeGreaterThanOrEqual(2000);
      expect(iv).toBeLessThanOrEqual(6000);
    }
    // 不全等。
    const allEqual = intervals.every((v) => v === intervals[0]);
    expect(allEqual).toBe(false);
  });
});
