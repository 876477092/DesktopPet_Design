/**
 * BackendSwitcher 单测（S9-M1：双后端可切 + 3s 就绪超时回退，不黑屏）。
 *
 * 用假后端（ScriptedRenderer）+ 假时钟驱动：
 *   - 启动即走帧路径（加载期不空屏）；
 *   - 主路径就绪 → 提升为主路由；
 *   - 3s 未就绪 → 定格帧回退，后续就绪也不抢占。
 */
import { describe, expect, it, vi } from 'vitest';

import type { RenderFrameCmdV1 } from '../shared/ipc';
import { BackendSwitcher } from './BackendSwitcher';
import { SKELETON_READY_TIMEOUT_MS, type ICharacterRenderer } from './ICharacterRenderer';

/** 可脚本化的假后端：kind 固定、isReady 可置、draw/paint 记录调用。 */
class ScriptedRenderer implements ICharacterRenderer {
  ready: boolean;
  readonly draws: RenderFrameCmdV1[] = [];
  paintCount = 0;
  constructor(readonly kind: 'frame' | 'skeleton', ready: boolean) {
    this.ready = ready;
  }
  isReady(): boolean {
    return this.ready;
  }
  draw(cmd: RenderFrameCmdV1): void {
    this.draws.push(cmd);
  }
  paint(): void {
    this.paintCount += 1;
  }
}

/** 构造一帧合法 v1 命令。 */
function cmd(actionId: string): RenderFrameCmdV1 {
  return {
    version: 1,
    actionId,
    atlasPng: 'a.png',
    frameIndex: 0,
    columns: 4,
    rows: 2,
    frameW: 64,
    frameH: 64,
    mirror: false,
    alpha: 1,
    fps: 6,
  };
}

describe('BackendSwitcher（K-14 双后端切换 / 3s 回退）', () => {
  it('启动即走帧路径，不黑屏', () => {
    const skeleton = new ScriptedRenderer('skeleton', false);
    const frame = new ScriptedRenderer('frame', true);
    const t = 0;
    const sw = new BackendSwitcher(skeleton, frame, {}, () => t);
    expect(sw.activeKind).toBe('frame');
    sw.draw(cmd('A'));
    expect(frame.draws).toHaveLength(1);
    expect(skeleton.draws).toHaveLength(0);
  });

  it('主路径启动即就绪 → 第一帧提升为骨骼', () => {
    const skeleton = new ScriptedRenderer('skeleton', true);
    const frame = new ScriptedRenderer('frame', true);
    const onPromote = vi.fn();
    const sw = new BackendSwitcher(skeleton, frame, { onPromote }, () => 0);
    sw.draw(cmd('A'));
    expect(sw.activeKind).toBe('skeleton');
    expect(skeleton.draws).toHaveLength(1);
    expect(frame.draws).toHaveLength(0);
    expect(onPromote).toHaveBeenCalledWith('skeleton');
  });

  it('主路径在超时窗口内就绪 → 提升，此前走帧路径', () => {
    const skeleton = new ScriptedRenderer('skeleton', false);
    const frame = new ScriptedRenderer('frame', true);
    let t = 0;
    const sw = new BackendSwitcher(skeleton, frame, {}, () => t);
    sw.draw(cmd('A')); // t=0 未就绪 → 帧
    expect(sw.activeKind).toBe('frame');

    t = 1_500;
    skeleton.ready = true;
    sw.draw(cmd('B')); // t=1500 就绪 → 提升
    expect(sw.activeKind).toBe('skeleton');
    expect(frame.draws).toHaveLength(1);
    expect(skeleton.draws).toHaveLength(1);
  });

  it('3s 未就绪 → 定格帧回退，后续就绪也不抢占（不黑屏）', () => {
    const skeleton = new ScriptedRenderer('skeleton', false);
    const frame = new ScriptedRenderer('frame', true);
    const onFallbackLatched = vi.fn();
    let t = 0;
    const sw = new BackendSwitcher(
      skeleton,
      frame,
      { onFallbackLatched },
      () => t,
      SKELETON_READY_TIMEOUT_MS,
    );
    t = 3_000;
    sw.draw(cmd('A')); // 恰好超时
    expect(sw.activeKind).toBe('frame');
    expect(sw.isFallbackLatched).toBe(true);
    expect(onFallbackLatched).toHaveBeenCalledWith('timeout');

    skeleton.ready = true;
    t = 6_000;
    sw.draw(cmd('B'));
    expect(sw.activeKind).toBe('frame'); // 定格后不再提升
    expect(frame.draws).toHaveLength(2);
    expect(skeleton.draws).toHaveLength(0);
  });

  it('paint 透传给当前激活后端', () => {
    const skeleton = new ScriptedRenderer('skeleton', true);
    const frame = new ScriptedRenderer('frame', true);
    const sw = new BackendSwitcher(skeleton, frame, {}, () => 0);
    sw.paint();
    expect(skeleton.paintCount).toBe(1);
    expect(frame.paintCount).toBe(0);
  });
});
