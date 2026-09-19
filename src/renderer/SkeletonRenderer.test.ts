/**
 * SkeletonRenderer 单测（S9-M2 接入框架）。
 *
 * 用 Fake Spine 适配器验证：加载期 isReady=false、加载后 true、draw 按 actionId
 * 切动画（同动作不重复切）、setSkin 只调适配器换装（UV，无重解码语义）。
 */
import { describe, expect, it, vi } from 'vitest';

import type { RenderFrameCmdV1 } from '../shared/ipc';
import { SkeletonRenderer, type SpineRuntimeAdapter } from './SkeletonRenderer';

/** Fake 适配器：可脚本化加载成功/失败。 */
class FakeAdapter implements SpineRuntimeAdapter {
  loaded = false;
  fail = false;
  animations: string[] = [];
  skins: string[] = [];
  renders: { alpha: number; mirror: boolean }[] = [];

  isLoaded(): boolean {
    return this.loaded;
  }
  async load(): Promise<void> {
    if (this.fail) throw new Error('spine load failed');
    this.loaded = true;
  }
  setAnimation(actionId: string): void {
    this.animations.push(actionId);
  }
  setSkin(skinName: string): void {
    this.skins.push(skinName);
  }
  render(opts: { alpha: number; mirror: boolean }): void {
    this.renders.push(opts);
  }
}

function cmd(actionId: string): RenderFrameCmdV1 {
  return {
    version: 1,
    actionId,
    atlasPng: 'skel',
    frameIndex: 0,
    columns: 1,
    rows: 1,
    frameW: 128,
    frameH: 128,
    mirror: false,
    alpha: 1,
    fps: 6,
  };
}

describe('SkeletonRenderer（S9-M2 接入框架）', () => {
  it('加载期 isReady=false，加载完成后 true', async () => {
    const adapter = new FakeAdapter();
    const r = new SkeletonRenderer(adapter);
    expect(r.kind).toBe('skeleton');
    expect(r.isReady()).toBe(false);
    // 等待异步 load 完成。
    await vi.waitFor(() => expect(r.isReady()).toBe(true));
  });

  it('draw 按 actionId 切动画，同动作不重复切', async () => {
    const adapter = new FakeAdapter();
    const r = new SkeletonRenderer(adapter);
    await vi.waitFor(() => expect(r.isReady()).toBe(true));
    r.draw(cmd('ACT-M-02'));
    r.draw(cmd('ACT-M-02')); // 同动作：不应重复切
    r.draw(cmd('ACT-I-01'));
    expect(adapter.animations).toEqual(['ACT-M-02', 'ACT-I-01']);
  });

  it('setSkin 只走适配器换装（UV，无重解码语义）', () => {
    const adapter = new FakeAdapter();
    const r = new SkeletonRenderer(adapter);
    r.setSkin('fancy');
    expect(adapter.skins).toEqual(['fancy']);
  });

  it('加载失败 → isReady 保持 false 且回调 onLoadError', async () => {
    const adapter = new FakeAdapter();
    adapter.fail = true;
    const onLoadError = vi.fn();
    const r = new SkeletonRenderer(adapter, { onLoadError });
    await vi.waitFor(() => expect(onLoadError).toHaveBeenCalled());
    expect(r.isReady()).toBe(false);
  });

  it('paint 透传 render 选项', async () => {
    const adapter = new FakeAdapter();
    const r = new SkeletonRenderer(adapter);
    await vi.waitFor(() => expect(r.isReady()).toBe(true));
    r.paint();
    expect(adapter.renders).toEqual([{ alpha: 1, mirror: false }]);
  });
});
