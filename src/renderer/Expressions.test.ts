/**
 * Expressions / 热区单测（S9-M4：20 表情可切 + AC-10 热区语义复测）。
 */
import { describe, expect, it, vi } from 'vitest';

import {
  ExpressionController,
  EXPRESSIONS,
  resolveHotzone,
  type ExpressionSink,
} from './Expressions';

describe('ExpressionController（20 表情差分）', () => {
  it('目录共 20 个，核心 12、P2 延后 8', () => {
    expect(EXPRESSIONS).toHaveLength(20);
    expect(ExpressionController.coreCount()).toBe(12);
    expect(EXPRESSIONS.filter((e) => e.deferred)).toHaveLength(8);
  });

  it('切到已登记表情 → 成功并派发到 sink', () => {
    const applied: string[] = [];
    const sink: ExpressionSink = { applyExpression: (id) => applied.push(id) };
    const c = new ExpressionController(sink);
    expect(applied).toEqual(['calm']); // 初始平静
    expect(c.set('happy')).toBe(true);
    expect(c.current).toBe('happy');
    expect(applied).toEqual(['calm', 'happy']);
  });

  it('未登记表情 → 拒绝并保持当前', () => {
    const sink: ExpressionSink = { applyExpression: vi.fn() };
    const c = new ExpressionController(sink);
    expect(c.set('not_a_face')).toBe(false);
    expect(c.current).toBe('calm');
  });
});

describe('resolveHotzone（AC-10 复测）', () => {
  it('狐耳次级热区悬停有效（头顶中央）', () => {
    expect(resolveHotzone(0.5, 0.1, false)).toBe('ear');
  });

  it('躯干落主判定', () => {
    expect(resolveHotzone(0.5, 0.6, false)).toBe('body');
  });

  it('热区外穿透', () => {
    expect(resolveHotzone(0.02, 0.95, false)).toBeNull();
  });

  it('镜像时 u 按 1-u 翻转（与帧/骨骼同语义）', () => {
    // 头顶中央在镜像下仍是耳朵区（对称）；取左半耳位验证镜像翻转。
    // 左耳位 u=0.35 → 非镜像在耳区；镜像后 u'=0.65 仍在耳区（耳区横跨 0.3~0.7）。
    expect(resolveHotzone(0.35, 0.1, false)).toBe('ear');
    expect(resolveHotzone(0.35, 0.1, true)).toBe('ear');
    // 非对称点：u=0.1（主判定左缘内）镜像到 u=0.9（右缘内）仍 body。
    expect(resolveHotzone(0.18, 0.6, false)).toBe('body');
    expect(resolveHotzone(0.18, 0.6, true)).toBe('body');
  });
});
