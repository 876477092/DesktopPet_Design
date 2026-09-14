/**
 * `parseRenderFrameCmd` / `parseBubbleCmd` 单测（S2-M1 引入；S3-M5 增 BubbleCmd v1 契约）。
 *
 * 覆盖：正常载荷、缺字段默认、未知字段忽略（前向兼容）、镜像透传、
 * alpha/fps 钳制、非对象载荷拒绝；气泡载荷的字段透传 / 枚举兜底 / 钳制 /
 * actions 脏数据丢弃 / 非对象拒绝。
 */
import { describe, expect, it } from 'vitest';

import {
  BUBBLE_CMD_VERSION,
  BUBBLE_DWELL_MAX_MS,
  BUBBLE_DWELL_MIN_MS,
  FRAME_CMD_VERSION,
  parseBubbleCmd,
  parseRenderFrameCmd,
} from './ipc';

describe('parseRenderFrameCmd（RenderFrameCmd v1 前向兼容解析）', () => {
  it('完整载荷逐字段透传（含镜像）', () => {
    const cmd = parseRenderFrameCmd({
      version: 1,
      actionId: 'ACT-M-02',
      atlasPng: 'ACT-M-02_walk.png',
      frameIndex: 5,
      columns: 8,
      rows: 1,
      frameW: 256,
      frameH: 256,
      mirror: true,
      alpha: 0.8,
      fps: 15,
    });
    expect(cmd).not.toBeNull();
    expect(cmd?.actionId).toBe('ACT-M-02');
    expect(cmd?.atlasPng).toBe('ACT-M-02_walk.png');
    expect(cmd?.frameIndex).toBe(5);
    expect(cmd?.columns).toBe(8);
    expect(cmd?.rows).toBe(1);
    expect(cmd?.frameW).toBe(256);
    expect(cmd?.frameH).toBe(256);
    expect(cmd?.mirror).toBe(true);
    expect(cmd?.alpha).toBeCloseTo(0.8);
    expect(cmd?.fps).toBe(15);
    expect(cmd?.version).toBe(FRAME_CMD_VERSION);
  });

  it('空对象：缺字段取默认值（alpha=1.0、fps=6、mirror=false）', () => {
    const cmd = parseRenderFrameCmd({});
    expect(cmd).not.toBeNull();
    expect(cmd?.actionId).toBe('');
    expect(cmd?.atlasPng).toBe('');
    expect(cmd?.frameIndex).toBe(0);
    expect(cmd?.mirror).toBe(false);
    expect(cmd?.alpha).toBe(1);
    expect(cmd?.fps).toBe(6);
  });

  it('未知字段直接忽略（v2 前向兼容）', () => {
    const cmd = parseRenderFrameCmd({
      actionId: 'ACT-T-02',
      futureField: { clip: 'x', slots: [1, 2] },
    });
    expect(cmd?.actionId).toBe('ACT-T-02');
    expect(cmd?.frameIndex).toBe(0);
  });

  it('alpha 钳制到 [0,1]，非有限取默认', () => {
    expect(parseRenderFrameCmd({ alpha: 2 })?.alpha).toBe(1);
    expect(parseRenderFrameCmd({ alpha: -3 })?.alpha).toBe(0);
    expect(parseRenderFrameCmd({ alpha: Number.NaN })?.alpha).toBe(1);
  });

  it('fps 钳制到 K-4 档位范围 [2,60]', () => {
    expect(parseRenderFrameCmd({ fps: 120 })?.fps).toBe(60);
    expect(parseRenderFrameCmd({ fps: 1 })?.fps).toBe(2);
    expect(parseRenderFrameCmd({ fps: Number.NaN })?.fps).toBe(6);
    expect(parseRenderFrameCmd({ fps: 2.9 })?.fps).toBe(2);
  });

  it('mirror 非布尔取 false（防御）', () => {
    expect(parseRenderFrameCmd({ mirror: 'yes' })?.mirror).toBe(false);
    expect(parseRenderFrameCmd({ mirror: 1 })?.mirror).toBe(false);
  });

  it('非对象载荷（null / 数组 / 标量）→ null，调用方跳帧降级', () => {
    expect(parseRenderFrameCmd(null)).toBeNull();
    expect(parseRenderFrameCmd([1, 2])).toBeNull();
    expect(parseRenderFrameCmd('cmd')).toBeNull();
    expect(parseRenderFrameCmd(42)).toBeNull();
    expect(parseRenderFrameCmd(undefined)).toBeNull();
  });
});

describe('parseBubbleCmd（BubbleCmd v1 前向兼容解析，S3-M5）', () => {
  it('完整载荷逐字段透传（9 字段契约）', () => {
    const cmd = parseBubbleCmd({
      version: 1,
      text: '你好',
      kind: 'help',
      preempt: true,
      cooldownKey: 'pool:help',
      dwellMs: 4500,
      showSignature: true,
      actions: [{ id: 'feed', label: '去喂食' }],
      highContrast: true,
    });
    expect(cmd).not.toBeNull();
    expect(cmd?.version).toBe(BUBBLE_CMD_VERSION);
    expect(cmd?.text).toBe('你好');
    expect(cmd?.kind).toBe('help');
    expect(cmd?.preempt).toBe(true);
    expect(cmd?.cooldownKey).toBe('pool:help');
    expect(cmd?.dwellMs).toBe(4500);
    expect(cmd?.showSignature).toBe(true);
    expect(cmd?.actions).toEqual([{ id: 'feed', label: '去喂食' }]);
    expect(cmd?.highContrast).toBe(true);
  });

  it('空对象：缺字段取默认（kind=chat、dwellMs=4000、showSignature 按 kind 派生）', () => {
    const cmd = parseBubbleCmd({});
    expect(cmd).not.toBeNull();
    expect(cmd?.text).toBe('');
    expect(cmd?.kind).toBe('chat');
    expect(cmd?.preempt).toBe(false);
    expect(cmd?.cooldownKey).toBe('');
    expect(cmd?.dwellMs).toBe(4000);
    expect(cmd?.showSignature).toBe(false); // chat → 不署名
    expect(cmd?.actions).toEqual([]);
    expect(cmd?.highContrast).toBe(false);
  });

  it('showSignature 缺省由 kind 派生（help/postcard → true）', () => {
    expect(parseBubbleCmd({ kind: 'help' })?.showSignature).toBe(true);
    expect(parseBubbleCmd({ kind: 'postcard' })?.showSignature).toBe(true);
    expect(parseBubbleCmd({ kind: 'reminder' })?.showSignature).toBe(false);
    expect(parseBubbleCmd({ kind: 'chat' })?.showSignature).toBe(false);
  });

  it('未知字段直接忽略（v2 前向兼容）', () => {
    const cmd = parseBubbleCmd({ text: 'hi', futureField: { slots: [1, 2] } });
    expect(cmd?.text).toBe('hi');
    expect((cmd as unknown as Record<string, unknown>).futureField).toBeUndefined();
  });

  it('dwellMs 钳制到 [3000,5000]：2999→3000、5001→5000、非有限→默认', () => {
    expect(parseBubbleCmd({ dwellMs: 2999 })?.dwellMs).toBe(BUBBLE_DWELL_MIN_MS);
    expect(parseBubbleCmd({ dwellMs: 5001 })?.dwellMs).toBe(BUBBLE_DWELL_MAX_MS);
    expect(parseBubbleCmd({ dwellMs: Number.NaN })?.dwellMs).toBe(4000);
    expect(parseBubbleCmd({ dwellMs: 'fast' })?.dwellMs).toBe(4000);
  });

  it('kind 非法值兜底 chat（含缺字段 / 类型错）', () => {
    expect(parseBubbleCmd({ kind: 'shout' })?.kind).toBe('chat');
    expect(parseBubbleCmd({ kind: 3 })?.kind).toBe('chat');
    expect(parseBubbleCmd({ kind: null })?.kind).toBe('chat');
  });

  it('actions 脏数据丢弃：非数组→[]；元素非对象 / 缺 id / label 非字符串→丢弃；label 空串回退 id', () => {
    expect(parseBubbleCmd({ actions: 'feed' })?.actions).toEqual([]);
    expect(
      parseBubbleCmd({
        actions: [
          'junk',
          null,
          { label: 'no-id' },
          { id: 'a', label: 42 },
          { id: '', label: 'empty-id' },
          { id: 'ok', label: '' },
          { id: 'ok2', label: '按钮' },
        ],
      })?.actions,
    ).toEqual([
      { id: 'ok', label: 'ok' }, // label 空串 → 回退 id
      { id: 'ok2', label: '按钮' },
    ]);
  });

  it('cooldownKey 空串 / 缺省 → 保留空串（消费侧回退 kind）', () => {
    expect(parseBubbleCmd({ cooldownKey: '' })?.cooldownKey).toBe('');
    expect(parseBubbleCmd({})?.cooldownKey).toBe('');
    expect(parseBubbleCmd({ cooldownKey: 42 })?.cooldownKey).toBe('');
  });

  it('布尔字段非布尔兜底（preempt / highContrast → false）', () => {
    expect(parseBubbleCmd({ preempt: 'yes' })?.preempt).toBe(false);
    expect(parseBubbleCmd({ highContrast: 1 })?.highContrast).toBe(false);
  });

  it('非对象载荷（null / 数组 / 标量）→ null，调用方 warn 跳过', () => {
    expect(parseBubbleCmd(null)).toBeNull();
    expect(parseBubbleCmd([1, 2])).toBeNull();
    expect(parseBubbleCmd('bubble')).toBeNull();
    expect(parseBubbleCmd(42)).toBeNull();
    expect(parseBubbleCmd(undefined)).toBeNull();
  });

  it('text 为空串时仍返回对象（消费侧 decideBubble 判 drop）', () => {
    const cmd = parseBubbleCmd({ text: '' });
    expect(cmd).not.toBeNull();
    expect(cmd?.text).toBe('');
  });
});
