/**
 * `parsePetSnapshotV2` / shopCatalog / activityCatalog 单测（S10-M1/M2）。
 *
 * 覆盖：六维透传、neglect 七因子与 reasons 前向兼容、economy/inventory 归一、
 * decor 恒 5 槽（脏输入补 null）、album 非数组降级、空载荷拒绝；目录归一化的脏项丢弃。
 */
import { describe, expect, it } from 'vitest';

import { parsePetSnapshotV2 } from './ipc';
import { SHOP_CATALOG, isPlaceableDecor, placeableDecorItems } from './shopCatalog';
import { DISPATCH_OPTIONS, optionsByKind } from './activityCatalog';

describe('parsePetSnapshotV2（pet://state 前向兼容解析）', () => {
  it('完整载荷逐字段透传', () => {
    const snap = parsePetSnapshotV2({
      v: 2,
      values: {
        mood: 80,
        energy: 60,
        boredom: 10,
        satiety: 55,
        cleanliness: 70,
        affinityLevel: 3,
        affinityExp: 42,
        affinityExpNext: 100,
      },
      neglect: {
        p: 12.5,
        cap: 100,
        level: 2,
        ratePerMin: 0.4,
        factors: { presence: 0.8, busyness: 1.2, product: 0.96 },
        sensitivity: { value: 1.5 },
        reasons: [
          { factorKey: 'presence', label: '你不在', weight: 0.6, dir: 'faster', advice: '多陪陪她' },
        ],
      },
      personality: { text: '慢热', rerollLeft: 1, canReroll: true },
      activity: null,
      economy: { coin: 120, todayEarned: 30, dailyCap: 350 },
      inventory: [{ itemId: 'FOOD_RICEBALL', qty: 2 }],
      decor: ['FURN_LANTERN', null, 'FURN_PLANT', null, null],
      album: [{ name: '海边' }],
      state: 'idle',
    });

    expect(snap).not.toBeNull();
    expect(snap?.values.mood).toBe(80);
    expect(snap?.values.affinityLevel).toBe(3);
    expect(snap?.neglect.p).toBeCloseTo(12.5);
    expect(snap?.neglect.factors.presence).toBe(0.8);
    expect(snap?.neglect.reasons).toHaveLength(1);
    expect(snap?.neglect.reasons[0]?.advice).toBe('多陪陪她');
    expect(snap?.economy.coin).toBe(120);
    expect(snap?.inventory).toEqual([{ itemId: 'FOOD_RICEBALL', qty: 2 }]);
    expect(snap?.decor).toHaveLength(5);
    expect(snap?.decor[0]).toBe('FURN_LANTERN');
    expect(snap?.decor[1]).toBeNull();
    expect(snap?.album).toHaveLength(1);
  });

  it('缺段取默认、未知字段忽略', () => {
    const snap = parsePetSnapshotV2({ v: 2, unknownFuture: { x: 1 } });
    expect(snap).not.toBeNull();
    expect(snap?.values.mood).toBe(0);
    expect(snap?.neglect.level).toBe(0);
    expect(snap?.economy.coin).toBe(0);
    expect(snap?.inventory).toEqual([]);
    expect(snap?.decor).toHaveLength(5);
    expect(snap?.decor.every((s) => s === null)).toBe(true);
    expect(snap?.album).toEqual([]);
  });

  it('decor 非数组 / 脏项 → 补成 5 个 null', () => {
    const snap = parsePetSnapshotV2({ v: 2, decor: 'not-an-array', album: 42 });
    expect(snap?.decor).toEqual([null, null, null, null, null]);
    expect(snap?.album).toEqual([]);
  });

  it('level 钳 0..5、dir 未知兜底 faster', () => {
    const snap = parsePetSnapshotV2({
      v: 2,
      neglect: { p: 0, cap: 0, level: 9, ratePerMin: 0, reasons: [{ factorKey: 'a', label: 'A', weight: 1, dir: 'sideways' }] },
    });
    expect(snap?.neglect.level).toBe(5);
    expect(snap?.neglect.reasons[0]?.dir).toBe('faster');
  });

  it('非对象载荷 → null（调用方保留上一份快照）', () => {
    expect(parsePetSnapshotV2(null)).toBeNull();
    expect(parsePetSnapshotV2([1, 2, 3])).toBeNull();
    expect(parsePetSnapshotV2('oops')).toBeNull();
  });
});

describe('shopCatalog（静态目录归一化）', () => {
  it('目录非空且字段完整', () => {
    expect(SHOP_CATALOG.length).toBeGreaterThan(0);
    for (const item of SHOP_CATALOG) {
      expect(item.id.length).toBeGreaterThan(0);
      expect(item.name.length).toBeGreaterThan(0);
      expect(item.price).toBeGreaterThanOrEqual(0);
    }
  });

  it('furniture 分类识别为可摆放', () => {
    const decor = placeableDecorItems();
    expect(decor.length).toBeGreaterThan(0);
    for (const item of decor) {
      expect(isPlaceableDecor(item)).toBe(true);
      expect(item.category).toBe('furniture');
    }
  });
});

describe('activityCatalog（派遣目录归一化）', () => {
  it('三类都有至少一个派遣项', () => {
    expect(optionsByKind('work').length).toBeGreaterThan(0);
    expect(optionsByKind('study').length).toBeGreaterThan(0);
    expect(optionsByKind('travel').length).toBeGreaterThan(0);
    expect(DISPATCH_OPTIONS.length).toBeGreaterThanOrEqual(3);
  });

  it('每项都有非空 defId 与至少一个正时长', () => {
    for (const opt of DISPATCH_OPTIONS) {
      expect(opt.defId.length).toBeGreaterThan(0);
      expect(opt.durations.length).toBeGreaterThan(0);
      expect(opt.durations.every((d) => d > 0)).toBe(true);
    }
  });
});
