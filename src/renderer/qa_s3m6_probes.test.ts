/**
 * S3-M6 / S4 前清障 QA 独立探针（严过关 · QA 第 1 轮）。
 *
 * 独立于 MenuLayer.test.ts / ParticleLayer.test.ts 的断言口径，专攻边界格：
 *   - Q1 菜单触发/关闭幂等：toggle 反复开关的 view 写入精确计数；
 *     close 后未 flush 再 open（hide 被取消、菜单保持可见）。
 *   - Q2 粒子寿命恰 1200ms 边界（1199 活 / 1200 剔除）与清场后 flush 零写入。
 *   - Q3 第 61 个粒子（单次钳 60）与「上限口径 = 每次迸发」非全屏总量。
 *   - Q4 flush 幂等：同 tick 重复 flush 快照相等（状态不漂移）。
 *
 * 零 DOM / 零 setTimeout / 注入手动单调时钟（C3）。
 */
import { describe, expect, it } from 'vitest';

import type { ParticleCmdV1 } from '../shared/ipc';
import { MenuLayer } from './MenuLayer';
import { ParticleLayer } from './ParticleLayer';
import {
  PARTICLE_LIFETIME_MS,
  type MenuItemId,
  type MenuItemSpec,
  type MenuPlacement,
  type MenuView,
  type ParticleRenderItem,
  type ParticleView,
} from './layerPorts';

/** 手动单调时钟。 */
class FakeClock {
  private t = 0;
  now = (): number => this.t;
  tick = (ms: number): void => {
    this.t += ms;
  };
}

/** 假菜单视图（记录调用序）。 */
class ProbeMenuView implements MenuView {
  showCalls = 0;
  hideCalls = 0;
  shownAt: MenuPlacement[] = [];
  show(_items: readonly MenuItemSpec[], at: MenuPlacement): void {
    this.showCalls += 1;
    this.shownAt.push({ ...at });
  }
  hide(): void {
    this.hideCalls += 1;
  }
  containerSize(): { width: number; height: number } {
    return { width: 256, height: 256 };
  }
}

/** 假粒子视图（记录快照）。 */
class ProbeParticleView implements ParticleView {
  readonly snapshots: ParticleRenderItem[][] = [];
  render(items: readonly ParticleRenderItem[]): void {
    this.snapshots.push([...items]);
  }
}

function fx(kind: ParticleCmdV1['kind'], count: number): ParticleCmdV1 {
  return { version: 1, kind, count };
}

describe('QA 探针：菜单触发与关闭幂等', () => {
  it('Q1a 反复开关 5 轮：show/hide 恰各 5 次，终态回无脏早退（再 flush 零写入）', () => {
    const view = new ProbeMenuView();
    const menu = new MenuLayer(view);
    for (let round = 0; round < 5; round += 1) {
      menu.open({ x: 10, y: 10 });
      menu.flush();
      menu.close();
      menu.flush();
    }
    expect(view.showCalls).toBe(5);
    expect(view.hideCalls).toBe(5);
    // 幂等收口：再多 flush 亦零写入。
    menu.flush();
    menu.flush();
    expect(view.showCalls).toBe(5);
    expect(view.hideCalls).toBe(5);
    expect(menu.isOpen()).toBe(false);
  });

  it('Q1b close 后未 flush 再 open：hide 被取消、show 重写（菜单不闪烁消失）', () => {
    const view = new ProbeMenuView();
    const menu = new MenuLayer(view);
    menu.open({ x: 0, y: 0 });
    menu.flush();
    expect(view.showCalls).toBe(1);
    // 同帧内 close → open（用户手滑双击右键）：hide 不得落 view。
    menu.close();
    menu.open({ x: 200, y: 200 });
    menu.flush();
    expect(view.hideCalls).toBe(0); // close 被 open 覆盖 → 不写 hide
    expect(view.showCalls).toBe(2); // open 重写 show 到新命中点
    expect(menu.isOpen()).toBe(true);
    // 越界命中点 (200,200) 须经 clampMenuPlacement 钳回容器（256×256 − 菜单 216×156 − pad 4）。
    expect(view.shownAt[1]).toMatchObject({ left: 36, top: 96 });
    // 收尾关闭正常。
    menu.close();
    menu.flush();
    expect(view.hideCalls).toBe(1);
  });

  it('Q1c choose(hide) 幂等口径：命令恰交一次；随后 flush 恰写一次 hide', () => {
    const view = new ProbeMenuView();
    const commands: MenuItemId[] = [];
    const menu = new MenuLayer(view, { onCommand: (id) => commands.push(id) });
    menu.open({ x: 8, y: 8 });
    menu.flush();
    menu.choose('hide');
    menu.flush();
    menu.flush();
    expect(commands).toEqual(['hide']);
    expect(view.hideCalls).toBe(1); // choose 触发的关闭经 flush 恰写一次 hide
    expect(menu.isOpen()).toBe(false);
  });
});

describe('QA 探针：粒子生命周期边界', () => {
  it('Q2 恰 1200ms：age=1199 活、age=1200 剔除且恰好一次空列表清场', () => {
    const clock = new FakeClock();
    const view = new ProbeParticleView();
    const layer = new ParticleLayer(view, { now: clock.now, seed: () => 42 });
    clock.tick(1_000);
    layer.submit(fx('heart', 3));
    clock.tick(PARTICLE_LIFETIME_MS - 1); // age = 1199ms
    layer.flush();
    expect(view.snapshots.at(-1)).toHaveLength(3);
    expect(layer.liveCount()).toBe(3);
    clock.tick(1); // age = 1200ms（恰边界：not < 1200 → 剔除）
    layer.flush();
    expect(view.snapshots.at(-1)).toHaveLength(0);
    expect(layer.liveCount()).toBe(0);
    const emptyWrites = view.snapshots.filter((s) => s.length === 0).length;
    expect(emptyWrites).toBe(1); // 全灭恰好一次空列表清场
    // 清场后回无脏早退：再 flush 零写入。
    const total = view.snapshots.length;
    layer.flush();
    layer.flush();
    expect(view.snapshots.length).toBe(total);
  });

  it('Q3 第 61 个：单次迸发 61 → 恰 60；两次 60 叠加 → 120（上限为每次迸发口径）', () => {
    const clock = new FakeClock();
    const view = new ProbeParticleView();
    const layer = new ParticleLayer(view, { now: clock.now, seed: () => 7 });
    layer.submit(fx('star', 61));
    expect(layer.liveCount()).toBe(60); // 单次上限 60（第 61 个被钳掉）
    layer.submit(fx('star', 60));
    expect(layer.liveCount()).toBe(120); // 上限是 maxPerBurst 而非同屏总量（02 §5.22）
    // 寿命到期后双双清空，无残留。
    clock.tick(PARTICLE_LIFETIME_MS + 1);
    layer.flush();
    expect(layer.liveCount()).toBe(0);
  });

  it('Q4 flush 幂等：同 tick 重复 flush 快照相等（状态不漂移）', () => {
    const clock = new FakeClock();
    const view = new ProbeParticleView();
    const layer = new ParticleLayer(view, { now: clock.now, seed: () => 3 });
    layer.submit(fx('dust', 8));
    layer.flush();
    const a = view.snapshots.at(-1);
    layer.flush();
    const b = view.snapshots.at(-1);
    expect(a).toEqual(b);
    // 推进半寿命后 opacity 衰减但位置确定（LCG 种子固定）。
    clock.tick(PARTICLE_LIFETIME_MS / 2);
    layer.flush();
    const mid = view.snapshots.at(-1);
    expect(mid).toHaveLength(8);
    expect(mid?.every((it) => it.opacity > 0 && it.opacity < 1)).toBe(true);
  });
});
