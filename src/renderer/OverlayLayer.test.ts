/**
 * 叠加层单测（S3-M5：overlayLogic 纯函数直测 + OverlayLayer 状态机覆盖）。
 *
 * 范式（仿 `FrameRenderer.test.ts`）：`FakeOverlayView` 记录全部 view 调用、
 * `FakeClock` 注入可控读数——**完全不碰 DOM**。
 *
 * 覆盖：长按阈值 499/500 边界、飘条进度、队列钳制、Zzz 档位规范化；
 * 进度环/怒气/Zzz 状态切换与幂等写、飘条 FIFO / 同屏 1 / 上限 3 丢弃不静默 /
 * 到点出队、长按触发一次、无脏 flush 零写入。
 */
import { describe, expect, it, vi } from 'vitest';

import { TOAST_MS, type OverlayView } from './layerPorts';
import {
  clampToastQueue,
  isReasonCardLongPress,
  normalizeSleepLevel,
  toastProgress,
} from './overlayLogic';
import { OverlayLayer } from './OverlayLayer';

// ---------------------------------------------------------------------------
// 纯函数：overlayLogic
// ---------------------------------------------------------------------------

describe('isReasonCardLongPress（02 §5 K-6：长按阈值 500ms）', () => {
  it('恰为 500ms 成立；不足不成立', () => {
    expect(isReasonCardLongPress(500)).toBe(true);
    expect(isReasonCardLongPress(499)).toBe(false);
    expect(isReasonCardLongPress(501)).toBe(true);
    expect(isReasonCardLongPress(0)).toBe(false);
  });

  it('负值 / 非有限输入不成立；自定义阈值生效', () => {
    expect(isReasonCardLongPress(-1)).toBe(false);
    expect(isReasonCardLongPress(Number.NaN)).toBe(false);
    expect(isReasonCardLongPress(100, 100)).toBe(true);
    expect(isReasonCardLongPress(99, 100)).toBe(false);
  });
});

describe('toastProgress（飘条进度 0~1）', () => {
  it('线性推进并钳到 [0,1]', () => {
    expect(toastProgress(0, TOAST_MS)).toBe(0);
    expect(toastProgress(TOAST_MS / 2, TOAST_MS)).toBeCloseTo(0.5, 10);
    expect(toastProgress(TOAST_MS, TOAST_MS)).toBe(1);
    expect(toastProgress(TOAST_MS + 500, TOAST_MS)).toBe(1);
  });

  it('toastMs <= 0 → 1（防除零）；elapsed 非有限/负 → 0', () => {
    expect(toastProgress(100, 0)).toBe(1);
    expect(toastProgress(100, -1)).toBe(1);
    expect(toastProgress(Number.NaN, TOAST_MS)).toBe(0);
    expect(toastProgress(-5, TOAST_MS)).toBe(0);
  });
});

describe('clampToastQueue（FIFO 保留前 cap 条）', () => {
  it('超出上限截断（保留最先入队的），未超出原样复制', () => {
    expect(clampToastQueue(['a', 'b', 'c', 'd'], 3)).toEqual(['a', 'b', 'c']);
    expect(clampToastQueue(['a', 'b'], 3)).toEqual(['a', 'b']);
    expect(clampToastQueue<string>([], 3)).toEqual([]);
  });

  it('cap <= 0 或非有限 → 空队列', () => {
    expect(clampToastQueue(['a'], 0)).toEqual([]);
    expect(clampToastQueue(['a'], -1)).toEqual([]);
    expect(clampToastQueue(['a'], Number.NaN)).toEqual([]);
  });
});

describe('normalizeSleepLevel（01 §6.5.5：Zzz 档 0/1/2）', () => {
  it('合法档原样通过且不标脏', () => {
    expect(normalizeSleepLevel(0)).toEqual({ level: 0, invalid: false });
    expect(normalizeSleepLevel(1)).toEqual({ level: 1, invalid: false });
    expect(normalizeSleepLevel(2)).toEqual({ level: 2, invalid: false });
  });

  it('非法值钳到合法档并标脏（NaN / 负数 / 小数 / 越界 / 非数值）', () => {
    expect(normalizeSleepLevel(-1)).toEqual({ level: 0, invalid: true });
    expect(normalizeSleepLevel(3)).toEqual({ level: 2, invalid: true });
    expect(normalizeSleepLevel(1.5)).toEqual({ level: 2, invalid: true });
    expect(normalizeSleepLevel(1.4)).toEqual({ level: 1, invalid: true });
    expect(normalizeSleepLevel(Number.NaN)).toEqual({ level: 0, invalid: true });
    expect(normalizeSleepLevel('big')).toEqual({ level: 0, invalid: true });
  });
});

// ---------------------------------------------------------------------------
// OverlayLayer 状态机（FakeOverlayView + FakeClock，不碰 DOM）
// ---------------------------------------------------------------------------

/** view 调用记录。 */
type OverlayOp =
  | { op: 'setCoaxProgress'; value: number | null }
  | { op: 'setAngerLevel'; level: number }
  | { op: 'setSleepLevel'; level: 0 | 1 | 2 }
  | { op: 'setToast'; text: string | null }
  | { op: 'setToastOpacity'; alpha: number };

/** 假视图：记录全部调用（含顺序与次数）。 */
class FakeOverlayView implements OverlayView {
  readonly ops: OverlayOp[] = [];

  setCoaxProgress(value: number | null): void {
    this.ops.push({ op: 'setCoaxProgress', value });
  }
  setAngerLevel(level: number): void {
    this.ops.push({ op: 'setAngerLevel', level });
  }
  setSleepLevel(level: 0 | 1 | 2): void {
    this.ops.push({ op: 'setSleepLevel', level });
  }
  setToast(text: string | null): void {
    this.ops.push({ op: 'setToast', text });
  }
  setToastOpacity(alpha: number): void {
    this.ops.push({ op: 'setToastOpacity', alpha });
  }

  private lastOp(name: OverlayOp['op']): OverlayOp | undefined {
    for (let i = this.ops.length - 1; i >= 0; i--) {
      const op = this.ops[i];
      if (op !== undefined && op.op === name) {
        return op;
      }
    }
    return undefined;
  }
  lastCoax(): number | null | undefined {
    const op = this.lastOp('setCoaxProgress');
    return op !== undefined && op.op === 'setCoaxProgress' ? op.value : undefined;
  }
  lastAnger(): number | undefined {
    const op = this.lastOp('setAngerLevel');
    return op !== undefined && op.op === 'setAngerLevel' ? op.level : undefined;
  }
  lastSleep(): 0 | 1 | 2 | undefined {
    const op = this.lastOp('setSleepLevel');
    return op !== undefined && op.op === 'setSleepLevel' ? op.level : undefined;
  }
  lastToast(): string | null | undefined {
    const op = this.lastOp('setToast');
    return op !== undefined && op.op === 'setToast' ? op.text : undefined;
  }
  /** 非空飘条文案序列（FIFO 断言用）。 */
  toastTexts(): string[] {
    const out: string[] = [];
    for (const op of this.ops) {
      if (op.op === 'setToast' && op.text !== null) {
        out.push(op.text);
      }
    }
    return out;
  }
  countOf(name: OverlayOp['op']): number {
    return this.ops.filter((op) => op.op === name).length;
  }
}

/** 可控单调时钟（C3）。 */
class FakeClock {
  private t = 0;
  readonly now = (): number => this.t;
  advance(ms: number): void {
    this.t += ms;
  }
}

describe('OverlayLayer 五要素（惰性 flush，幂等写）', () => {
  it('进度环：null 隐藏、0.5 显示；同值重复 flush 不重复写（幂等）', () => {
    const clock = new FakeClock();
    const view = new FakeOverlayView();
    const layer = new OverlayLayer(view, { now: clock.now });

    layer.flush(); // 初始同步：写默认 null。
    expect(view.lastCoax()).toBeNull();
    const afterInit = view.countOf('setCoaxProgress');

    layer.setCoaxProgress(0.5);
    layer.flush();
    expect(view.lastCoax()).toBe(0.5);

    layer.flush();
    layer.flush();
    expect(view.countOf('setCoaxProgress')).toBe(afterInit + 1); // 同值跳过。
  });

  it('怒气：0 隐藏、≥1 显示；负值 / 非有限钳为 0', () => {
    const clock = new FakeClock();
    const view = new FakeOverlayView();
    const layer = new OverlayLayer(view, { now: clock.now });

    layer.setAngerLevel(0);
    layer.flush();
    expect(view.lastAnger()).toBe(0);

    layer.setAngerLevel(2);
    layer.flush();
    expect(view.lastAnger()).toBe(2);

    layer.setAngerLevel(-3);
    layer.flush();
    expect(view.lastAnger()).toBe(0);

    layer.setAngerLevel(Number.NaN);
    layer.flush();
    expect(view.lastAnger()).toBe(0);
  });

  it('Zzz：0/1/2 直通；非法档钳制且 console.warn 不静默', () => {
    const clock = new FakeClock();
    const view = new FakeOverlayView();
    const layer = new OverlayLayer(view, { now: clock.now });
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});

    layer.setSleepLevel(1);
    layer.flush();
    expect(view.lastSleep()).toBe(1);
    layer.setSleepLevel(2);
    layer.flush();
    expect(view.lastSleep()).toBe(2);
    layer.setSleepLevel(0);
    layer.flush();
    expect(view.lastSleep()).toBe(0);
    expect(warn).not.toHaveBeenCalled();

    const tooBig = 5 as unknown as 0 | 1 | 2;
    layer.setSleepLevel(tooBig);
    layer.flush();
    expect(view.lastSleep()).toBe(2);
    expect(warn).toHaveBeenCalledTimes(1);

    const negative = -1 as unknown as 0 | 1 | 2;
    layer.setSleepLevel(negative);
    layer.flush();
    expect(view.lastSleep()).toBe(0);
    expect(warn).toHaveBeenCalledTimes(2);

    const fractional = 1.5 as unknown as 0 | 1 | 2;
    layer.setSleepLevel(fractional);
    layer.flush();
    expect(view.lastSleep()).toBe(2);
    expect(warn).toHaveBeenCalledTimes(3);

    const notANumber = Number.NaN as unknown as 0 | 1 | 2;
    layer.setSleepLevel(notANumber);
    layer.flush();
    expect(view.lastSleep()).toBe(0);
    expect(warn).toHaveBeenCalledTimes(4);

    warn.mockRestore();
  });

  it('飘条 FIFO 顺序推进（同屏 1）', () => {
    const clock = new FakeClock();
    const view = new FakeOverlayView();
    const layer = new OverlayLayer(view, { now: clock.now });

    layer.enqueueToast('A');
    layer.enqueueToast('B');
    layer.enqueueToast('C');
    layer.flush();
    expect(view.toastTexts()).toEqual(['A']);

    clock.advance(TOAST_MS);
    layer.flush();
    expect(view.toastTexts()).toEqual(['A', 'B']);

    clock.advance(TOAST_MS);
    layer.flush();
    expect(view.toastTexts()).toEqual(['A', 'B', 'C']);
  });

  it('队列上限 3：第 4 条被丢弃且 console.warn 不静默', () => {
    const clock = new FakeClock();
    const view = new FakeOverlayView();
    const layer = new OverlayLayer(view, { now: clock.now });
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});

    layer.enqueueToast('t1');
    layer.enqueueToast('t2');
    layer.enqueueToast('t3');
    layer.enqueueToast('t4');
    expect(warn).toHaveBeenCalledTimes(1);
    warn.mockRestore();

    for (let i = 0; i < 3; i++) {
      layer.flush();
      clock.advance(TOAST_MS);
      layer.flush();
    }
    expect(view.toastTexts()).toEqual(['t1', 't2', 't3']); // t4 永不显示。
  });

  it('到点出队并 setToast(null)；未到点保持显示', () => {
    const clock = new FakeClock();
    const view = new FakeOverlayView();
    const layer = new OverlayLayer(view, { now: clock.now });

    layer.enqueueToast('done');
    layer.flush();
    expect(view.lastToast()).toBe('done');

    clock.advance(TOAST_MS - 1);
    layer.flush();
    expect(view.lastToast()).toBe('done'); // 未到点不出队。

    clock.advance(1);
    layer.flush();
    expect(view.lastToast()).toBeNull();
  });

  it('长按：499ms 不触发；≥500ms 触发且只触发一次', () => {
    const clock = new FakeClock();
    const view = new FakeOverlayView();
    let requests = 0;
    const layer = new OverlayLayer(view, {
      now: clock.now,
      onReasonCardRequest: () => {
        requests += 1;
      },
    });

    layer.pressMoodBar();
    clock.advance(499);
    layer.releaseMoodBar();
    expect(requests).toBe(0);

    layer.pressMoodBar();
    clock.advance(500);
    layer.releaseMoodBar();
    expect(requests).toBe(1);

    layer.releaseMoodBar(); // 重复抬起不重复触发。
    expect(requests).toBe(1);
  });

  it('未按下直接抬起不触发回调', () => {
    const clock = new FakeClock();
    const view = new FakeOverlayView();
    let requests = 0;
    const layer = new OverlayLayer(view, {
      now: clock.now,
      onReasonCardRequest: () => {
        requests += 1;
      },
    });
    layer.releaseMoodBar();
    expect(requests).toBe(0);
  });

  it('无脏且无进行中飘条：flush 后续调用零 view 写入（60Hz 廉价）', () => {
    const clock = new FakeClock();
    const view = new FakeOverlayView();
    const layer = new OverlayLayer(view, { now: clock.now });

    layer.flush(); // 初始同步。
    const writes = view.ops.length;
    expect(writes).toBeGreaterThan(0);

    layer.flush();
    layer.flush();
    expect(view.ops.length).toBe(writes); // 幂等：零新增写入。
  });
});
