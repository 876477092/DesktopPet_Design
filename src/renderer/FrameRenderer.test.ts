/**
 * `FrameRenderer` 单测（S2-M2：镜像透传 / 交叉淡入 / B10 驱逐失效）。
 *
 * 舞台与缓存均为最小替身（无 DOM / WebGL），时钟注入可控读数——
 * 覆盖卡片要求的三类纯逻辑行为：
 *   1. `cmd.mirror` 透传舞台（规则在 Rust 播放器，前端不判定朝向）；
 *   2. 动作切换 150ms 内双绘、alpha 线性互补；同动作翻帧不淡入；
 *   3. B10：缓存驱逐通知 → 就绪帧失效；paint 前位图有效性兜底。
 */
import { describe, expect, it } from 'vitest';

import type { RenderFrameCmdV1 } from '../shared/ipc';
import { FrameRenderer, type FrameStage } from './FrameRenderer';
import type { DrawOptions, FrameSubRect } from './WebGLStage';

/** 假位图（无 ImageBitmap 运行时；`close()` 模拟关闭语义 = 尺寸归零）。 */
class FakeBitmap {
  width = 256;
  height = 256;
  closed = false;
  close(): void {
    this.closed = true;
    this.width = 0;
    this.height = 0;
  }
  asBitmap(): ImageBitmap {
    return this as unknown as ImageBitmap;
  }
}

/** 绘制调用记录。 */
interface DrawCall {
  readonly bitmap: ImageBitmap;
  readonly atlasName: string;
  readonly rect: FrameSubRect;
  readonly mirror: boolean;
  readonly alpha: number;
  readonly opts: DrawOptions;
}

/** 假舞台：记录 drawSubRect 调用。 */
class FakeStage implements FrameStage {
  readonly calls: DrawCall[] = [];
  drawSubRect(
    bitmap: ImageBitmap,
    atlasName: string,
    rect: FrameSubRect,
    mirror: boolean,
    alpha: number,
    opts: DrawOptions = {},
  ): void {
    this.calls.push({ bitmap, atlasName, rect, mirror, alpha, opts });
  }
}

/** 假缓存：按名取位图，可手动触发驱逐通知（B10）。 */
class FakeCache {
  private readonly bitmaps = new Map<string, FakeBitmap>();
  private listener: ((name: string) => void) | null = null;

  put(name: string, bitmap: FakeBitmap): void {
    this.bitmaps.set(name, bitmap);
  }

  async get(name: string): Promise<ImageBitmap | null> {
    const bmp = this.bitmaps.get(name);
    return bmp ? bmp.asBitmap() : null;
  }

  /** 模拟 AtlasCache LRU 驱逐（close + 通知）。 */
  evict(name: string): void {
    this.bitmaps.get(name)?.close();
    this.listener?.(name);
  }

  onEvict(listener: (name: string) => void): () => void {
    this.listener = listener;
    return () => {
      this.listener = null;
    };
  }
}

/** 可控单调时钟（C3 边界：仅本地过渡时长测量）。 */
class FakeClock {
  private t = 0;
  now = (): number => this.t;
  advance(ms: number): void {
    this.t += ms;
  }
}

/** 等待异步 consume 链路完成（微任务 + 宏任务各一拍）。 */
async function flush(): Promise<void> {
  await Promise.resolve();
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
}

/** 命令快捷工厂。 */
function cmd(actionId: string, atlasPng: string, frameIndex = 0, mirror = false): RenderFrameCmdV1 {
  return {
    version: 1,
    actionId,
    atlasPng,
    frameIndex,
    columns: 4,
    rows: 1,
    frameW: 256,
    frameH: 256,
    mirror,
    alpha: 1,
    fps: 6,
  };
}


describe('FrameRenderer 镜像透传（K-4：规则在 Rust 播放器，前端不判定）', () => {
  it('cmd.mirror=true 原样透传给舞台；同动作翻帧不触发淡入', async () => {
    const stage = new FakeStage();
    const cache = new FakeCache();
    cache.put('ACT-M-02_walk.png', new FakeBitmap());
    const renderer = new FrameRenderer(stage, cache, () => {}, new FakeClock().now);

    renderer.draw(cmd('ACT-M-02', 'ACT-M-02_walk.png', 0, true));
    await flush();
    renderer.draw(cmd('ACT-M-02', 'ACT-M-02_walk.png', 1, true));
    await flush();

    renderer.paint();
    expect(stage.calls).toHaveLength(1);
    expect(stage.calls[0]?.mirror).toBe(true);
    expect(stage.calls[0]?.atlasName).toBe('ACT-M-02_walk.png');
    expect(stage.calls[0]?.rect).toEqual({ sx: 256, sy: 0, sw: 256, sh: 256 });
    expect(stage.calls[0]?.opts.clear).not.toBe(false);
  });

  it('cmd.mirror=false / alpha 透传', async () => {
    const stage = new FakeStage();
    const cache = new FakeCache();
    cache.put('ACT-M-01_idle.png', new FakeBitmap());
    const renderer = new FrameRenderer(stage, cache, () => {}, new FakeClock().now);

    const c = { ...cmd('ACT-M-01', 'ACT-M-01_idle.png'), alpha: 0.7 };
    renderer.draw(c);
    await flush();
    renderer.paint();
    expect(stage.calls[0]?.mirror).toBe(false);
    expect(stage.calls[0]?.alpha).toBeCloseTo(0.7);
  });
});

describe('FrameRenderer 交叉淡入（S2-M2：固定 150ms 线性）', () => {
  it('动作切换立即 paint：双绘且 alpha 互补（和为 1）、先清屏后叠绘', async () => {
    const clock = new FakeClock();
    const stage = new FakeStage();
    const cache = new FakeCache();
    cache.put('ACT-M-01_idle.png', new FakeBitmap());
    cache.put('ACT-M-02_walk.png', new FakeBitmap());
    const renderer = new FrameRenderer(stage, cache, () => {}, clock.now);

    renderer.draw(cmd('ACT-M-01', 'ACT-M-01_idle.png'));
    await flush();
    clock.advance(50);
    renderer.draw(cmd('ACT-M-02', 'ACT-M-02_walk.png'));
    await flush();
    clock.advance(25); // 淡入进行到 25ms（next=25/150）。

    renderer.paint();
    expect(stage.calls).toHaveLength(2);
    const [prev, next] = stage.calls as [DrawCall, DrawCall];
    expect(prev.atlasName).toBe('ACT-M-01_idle.png');
    expect(prev.opts.clear).toBe(true);
    expect(prev.alpha).toBeCloseTo(125 / 150, 6);
    expect(next.atlasName).toBe('ACT-M-02_walk.png');
    expect(next.opts.clear).toBe(false);
    expect(next.alpha).toBeCloseTo(25 / 150, 6);
    expect(prev.alpha + next.alpha).toBeCloseTo(1, 10);
  });

  it('淡入超过 150ms：回落单帧绘制', async () => {
    const clock = new FakeClock();
    const stage = new FakeStage();
    const cache = new FakeCache();
    cache.put('A.png', new FakeBitmap());
    cache.put('B.png', new FakeBitmap());
    const renderer = new FrameRenderer(stage, cache, () => {}, clock.now);

    renderer.draw(cmd('ACT-A', 'A.png'));
    await flush();
    clock.advance(10);
    renderer.draw(cmd('ACT-B', 'B.png'));
    await flush();
    clock.advance(150); // 到达淡入边界。

    renderer.paint();
    expect(stage.calls).toHaveLength(1);
    expect(stage.calls[0]?.atlasName).toBe('B.png');
    expect(stage.calls[0]?.alpha).toBeCloseTo(1);

    // 淡入状态已清理：再次 paint 不再双绘。
    renderer.paint();
    expect(stage.calls).toHaveLength(2);
    expect(stage.calls[1]?.alpha).toBeCloseTo(1);
  });

  it('同一动作翻帧不淡入（不逐帧叠影）', async () => {
    const clock = new FakeClock();
    const stage = new FakeStage();
    const cache = new FakeCache();
    cache.put('A.png', new FakeBitmap());
    const renderer = new FrameRenderer(stage, cache, () => {}, clock.now);

    renderer.draw(cmd('ACT-A', 'A.png', 0));
    await flush();
    clock.advance(10);
    renderer.draw(cmd('ACT-A', 'A.png', 1));
    await flush();
    renderer.paint();
    expect(stage.calls).toHaveLength(1);
  });

  it('不同图集但同动作 ID：不淡入（淡入以动作切换为触发）', async () => {
    const clock = new FakeClock();
    const stage = new FakeStage();
    const cache = new FakeCache();
    cache.put('A.png', new FakeBitmap());
    const renderer = new FrameRenderer(stage, cache, () => {}, clock.now);

    renderer.draw(cmd('ACT-A', 'A.png', 0));
    await flush();
    renderer.draw(cmd('ACT-A', 'A.png', 1));
    await flush();
    renderer.paint();
    expect(stage.calls).toHaveLength(1);
  });
});

describe('FrameRenderer B10 驱逐守卫', () => {
  it('缓存驱逐通知 → 就绪帧失效，paint 空操作', async () => {
    const stage = new FakeStage();
    const cache = new FakeCache();
    cache.put('A.png', new FakeBitmap());
    const renderer = new FrameRenderer(stage, cache, () => {}, new FakeClock().now);

    renderer.draw(cmd('ACT-A', 'A.png'));
    await flush();
    renderer.paint();
    expect(stage.calls).toHaveLength(1);

    // LRU 驱逐 A.png（close + 通知）。
    cache.evict('A.png');
    renderer.paint();
    expect(stage.calls).toHaveLength(1);
  });

  it('paint 前位图有效性兜底（通知丢失场景自愈）', async () => {
    const stage = new FakeStage();
    const cache = new FakeCache();
    const bmp = new FakeBitmap();
    cache.put('A.png', bmp);
    const renderer = new FrameRenderer(stage, cache, () => {}, new FakeClock().now);

    renderer.draw(cmd('ACT-A', 'A.png'));
    await flush();
    bmp.close(); // 模拟通知丢失：位图被外部 close。
    renderer.paint();
    expect(stage.calls).toHaveLength(0);
  });

  it('淡入中被驱逐：旧帧失效退化为新帧单绘（不崩不花屏）', async () => {
    const clock = new FakeClock();
    const stage = new FakeStage();
    const cache = new FakeCache();
    cache.put('A.png', new FakeBitmap());
    cache.put('B.png', new FakeBitmap());
    const renderer = new FrameRenderer(stage, cache, () => {}, clock.now);

    renderer.draw(cmd('ACT-A', 'A.png'));
    await flush();
    renderer.draw(cmd('ACT-B', 'B.png'));
    await flush();
    cache.evict('A.png'); // 旧动作图集被驱逐。

    renderer.paint();
    expect(stage.calls).toHaveLength(1);
    expect(stage.calls[0]?.atlasName).toBe('B.png');
  });

  it('非法载荷跳帧（布局缺失 / atlasPng 为空 / 子矩形越界）', async () => {
    const stage = new FakeStage();
    const cache = new FakeCache();
    cache.put('A.png', new FakeBitmap());
    const renderer = new FrameRenderer(stage, cache, () => {}, new FakeClock().now);

    renderer.draw({ ...cmd('ACT-A', 'A.png'), columns: 0, rows: 0 });
    await flush();
    renderer.draw({ ...cmd('ACT-A', 'A.png'), atlasPng: '' });
    await flush();
    renderer.draw({ ...cmd('ACT-A', 'A.png'), frameIndex: 99 });
    await flush();
    renderer.paint();
    expect(stage.calls).toHaveLength(0);
  });

  it('图集不可用（cache.get → null）跳帧', async () => {
    const stage = new FakeStage();
    const cache = new FakeCache();
    const renderer = new FrameRenderer(stage, cache, () => {}, new FakeClock().now);
    renderer.draw(cmd('ACT-A', 'missing.png'));
    await flush();
    renderer.paint();
    expect(stage.calls).toHaveLength(0);
  });
});
