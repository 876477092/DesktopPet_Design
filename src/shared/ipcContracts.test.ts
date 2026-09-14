/**
 * IPC 契约测试（S3-M6，T-10 段 · 下）：`ParticleCmd v1` / `MenuCmd v1` 解析函数。
 *
 * 契约先行范式（与 `parseRenderFrameCmd` 同口径，`02 §7.4`）：
 * - 正常载荷保真；
 * - 缺失/非法字段兜底（kind→heart、count→[1,60]、坐标→0）；
 * - 前向兼容（未知字段忽略）；
 * - 防御畸形载荷（非对象 / 数组 / null → null）。
 *
 * 零 DOM 零时钟零 setTimeout（C3）；纯函数直测。
 */
import { describe, expect, it } from 'vitest';

import {
  COAX_CMD_VERSION,
  MENU_CMD_VERSION,
  PARTICLE_BURST_CAP,
  PARTICLE_CMD_VERSION,
  parseCoaxCmd,
  parseMenuCmd,
  parseParticleCmd,
} from './ipc';

describe('parseParticleCmd（pet://fx 契约 v1）', () => {
  it('正常载荷保真（kind/count/version 原样）', () => {
    const cmd = parseParticleCmd({ version: 1, kind: 'dust', count: 8 });
    expect(cmd).not.toBeNull();
    expect(cmd?.version).toBe(1);
    expect(cmd?.kind).toBe('dust');
    expect(cmd?.count).toBe(8);
  });

  it('五类 kind 全部放行', () => {
    for (const kind of ['heart', 'star', 'dust', 'tear', 'anger'] as const) {
      const cmd = parseParticleCmd({ version: 1, kind });
      expect(cmd?.kind).toBe(kind);
    }
  });

  it('kind 缺失/非法 → 兜底 heart（永不产出未知类别）', () => {
    expect(parseParticleCmd({ count: 5 })?.kind).toBe('heart');
    expect(parseParticleCmd({ kind: 'rainbow', count: 5 })?.kind).toBe('heart');
    expect(parseParticleCmd({ kind: 42, count: 5 })?.kind).toBe('heart');
  });

  it('count 缺失 → 缺省 12；非有限 → 缺省后再钳', () => {
    expect(parseParticleCmd({ kind: 'heart' })?.count).toBe(12);
    expect(parseParticleCmd({ kind: 'heart', count: Number.NaN })?.count).toBe(12);
    expect(parseParticleCmd({ kind: 'heart', count: Number.POSITIVE_INFINITY })?.count).toBe(12);
  });

  it('count 钳 [1, 60]：0→1、负数→1、小数截断、100→60（上限与 PARTICLE_BURST_CAP 一致）', () => {
    expect(parseParticleCmd({ kind: 'heart', count: 0 })?.count).toBe(1);
    expect(parseParticleCmd({ kind: 'heart', count: -3 })?.count).toBe(1);
    expect(parseParticleCmd({ kind: 'heart', count: 7.9 })?.count).toBe(7);
    expect(parseParticleCmd({ kind: 'heart', count: 100 })?.count).toBe(PARTICLE_BURST_CAP);
    expect(PARTICLE_BURST_CAP).toBe(60);
  });

  it('version 缺失 → 兜底 1（前向兼容：旧生产者不带 version 也可解析）', () => {
    expect(parseParticleCmd({ kind: 'star' })?.version).toBe(PARTICLE_CMD_VERSION);
  });

  it('未知字段忽略（v2 新增字段不致解析失败）', () => {
    const cmd = parseParticleCmd({ kind: 'tear', count: 2, futureField: 'x' });
    expect(cmd?.kind).toBe('tear');
    expect(cmd?.count).toBe(2);
  });

  it('畸形载荷防御：null / 数组 / 标量 / undefined → null', () => {
    expect(parseParticleCmd(null)).toBeNull();
    expect(parseParticleCmd(undefined)).toBeNull();
    expect(parseParticleCmd([1, 2])).toBeNull();
    expect(parseParticleCmd('heart')).toBeNull();
    expect(parseParticleCmd(42)).toBeNull();
  });
});

describe('parseMenuCmd（pet://menu 契约 v1）', () => {
  it('正常载荷保真（屏幕物理 + 窗口内 CSS 坐标）', () => {
    const cmd = parseMenuCmd({ version: 1, screenX: 1920, screenY: 1080, localX: 120, localY: 80 });
    expect(cmd).toEqual({
      version: 1,
      screenX: 1920,
      screenY: 1080,
      localX: 120,
      localY: 80,
    });
  });

  it('坐标缺失 → 兜底 0（消费侧 clampMenuPlacement 会钳回容器内，不越界弹出）', () => {
    expect(parseMenuCmd({})).toEqual({
      version: MENU_CMD_VERSION,
      screenX: 0,
      screenY: 0,
      localX: 0,
      localY: 0,
    });
  });

  it('坐标非有限 → 兜底 0（NaN / Infinity 防御）', () => {
    const cmd = parseMenuCmd({ screenX: Number.NaN, localX: Number.POSITIVE_INFINITY });
    expect(cmd?.screenX).toBe(0);
    expect(cmd?.localX).toBe(0);
  });

  it('字符串坐标不接受（类型错兜底 0，不做隐式转换）', () => {
    expect(parseMenuCmd({ localX: '120' })?.localX).toBe(0);
  });

  it('version 缺失 → 兜底 1；未知字段忽略', () => {
    const cmd = parseMenuCmd({ localX: 5, future: true });
    expect(cmd?.version).toBe(MENU_CMD_VERSION);
    expect(cmd?.localX).toBe(5);
  });

  it('畸形载荷防御：null / 数组 / 标量 → null', () => {
    expect(parseMenuCmd(null)).toBeNull();
    expect(parseMenuCmd([{ localX: 1 }])).toBeNull();
    expect(parseMenuCmd('menu')).toBeNull();
    expect(parseMenuCmd(0)).toBeNull();
  });
});

describe('parseCoaxCmd（pet://coax 契约 v1，S4-M3）', () => {
  it('进度环进行中：active + ratio 保真', () => {
    const cmd = parseCoaxCmd({
      version: 1,
      active: true,
      away: false,
      step: 'stroke',
      ratio: 0.4,
      succeeded: false,
    });
    expect(cmd).toEqual({
      version: 1,
      active: true,
      away: false,
      step: 'stroke',
      ratio: 0.4,
      succeeded: false,
      reason: null,
    });
  });

  it('三部曲完成：succeeded 标志 + 进度环隐藏', () => {
    const cmd = parseCoaxCmd({ active: false, step: 'idle', ratio: 0, succeeded: true });
    expect(cmd?.succeeded).toBe(true);
    expect(cmd?.active).toBe(false);
    expect(cmd?.reason).toBeNull();
  });

  it('失败：reason 保真（三态白名单）', () => {
    expect(parseCoaxCmd({ reason: 'interrupted' })?.reason).toBe('interrupted');
    expect(parseCoaxCmd({ reason: 'timeout' })?.reason).toBe('timeout');
    expect(parseCoaxCmd({ reason: 'abandoned' })?.reason).toBe('abandoned');
  });

  it('非法 reason → null（不误报失败）', () => {
    expect(parseCoaxCmd({ reason: 'exploded' })?.reason).toBeNull();
    expect(parseCoaxCmd({ reason: 42 })?.reason).toBeNull();
  });

  it('离家态：away 保真（step=away）', () => {
    const cmd = parseCoaxCmd({ active: false, away: true, step: 'away' });
    expect(cmd?.away).toBe(true);
    expect(cmd?.step).toBe('away');
  });

  it('缺失 / 非法字段兜底（step→idle、ratio→0 并钳 [0,1]、布尔→false）', () => {
    expect(parseCoaxCmd({})).toEqual({
      version: COAX_CMD_VERSION,
      active: false,
      away: false,
      step: 'idle',
      ratio: 0,
      succeeded: false,
      reason: null,
    });
    expect(parseCoaxCmd({ step: 'teleport', ratio: 7 })?.step).toBe('idle');
    expect(parseCoaxCmd({ ratio: 7 })?.ratio).toBe(1);
    expect(parseCoaxCmd({ ratio: -3 })?.ratio).toBe(0);
    expect(parseCoaxCmd({ ratio: Number.NaN })?.ratio).toBe(0);
    expect(parseCoaxCmd({ active: 'yes' })?.active).toBe(false);
  });

  it('前向兼容：未知字段忽略', () => {
    const cmd = parseCoaxCmd({ ratio: 0.5, futureField: { a: 1 } });
    expect(cmd?.ratio).toBe(0.5);
  });

  it('畸形载荷防御：null / 数组 / 标量 → null', () => {
    expect(parseCoaxCmd(null)).toBeNull();
    expect(parseCoaxCmd([{ ratio: 1 }])).toBeNull();
    expect(parseCoaxCmd('coax')).toBeNull();
    expect(parseCoaxCmd(0)).toBeNull();
  });
});
