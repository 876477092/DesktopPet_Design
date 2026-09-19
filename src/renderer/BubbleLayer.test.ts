/**
 * 气泡层单测（S3-M5：bubbleLogic 纯函数直测 + BubbleLayer 状态机覆盖）。
 *
 * 范式（仿 `FrameRenderer.test.ts`）：`FakeBubbleView` 记录全部 view 调用、
 * `FakeClock` 注入可控读数——**完全不碰 DOM**（不引 jsdom/happy-dom）。
 *
 * 覆盖：
 *   - 纯函数：占位符 / 3~5s 钳制 / 优先级 / 勿扰派生 / §4.1 七步短路仲裁（含 20s 边界
 *     「恰好 20000ms 放行」）/ 摆位三态 + 128 CSS 视口几何 / 双侧淡入淡出曲线；
 *   - 状态机：show 时序（setContent 先于 measure/place/setVisible）、20s 冷却不重复
 *     setContent、preempt 覆盖、淡入 alpha、到期隐藏、无脏 flush 零写入、署名 / 高对比。
 */
import { describe, expect, it } from 'vitest';

import type { BubbleCmdV1 } from '../shared/ipc';
import {
  BUBBLE_COOLDOWN_MS,
  BUBBLE_MAX_WIDTH,
  PET_LOGICAL_WIDTH,
  type BubbleContent,
  type BubblePlacement,
  type BubbleView,
} from './layerPorts';
import {
  bubblePriority,
  clampDwellMs,
  decideBubble,
  fadeAlpha,
  renderPlaceholders,
  resolveBubblePlacement,
  shouldShowUnderDnd,
} from './bubbleLogic';
import { BubbleLayer } from './BubbleLayer';

/** C2：测试用角色名一律码点转义构造，源码零角色名字面量。 */
const NAME = '\u5FC3\u6708\u72D0';

/** 命令快捷工厂（9 字段契约默认值）。 */
function bubbleCmd(overrides: Partial<BubbleCmdV1> = {}): BubbleCmdV1 {
  return {
    version: 1,
    text: '你好',
    kind: 'chat',
    preempt: false,
    cooldownKey: '',
    dwellMs: 4000,
    showSignature: false,
    actions: [],
    highContrast: false,
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// 纯函数：renderPlaceholders / clampDwellMs / bubblePriority / shouldShowUnderDnd
// ---------------------------------------------------------------------------

describe('renderPlaceholders（C2 占位符：命中替换、未命中原样保留）', () => {
  it('vars 命中的 {token} 被替换', () => {
    expect(renderPlaceholders('你好，{name}', { name: NAME })).toBe('你好，' + NAME);
    expect(renderPlaceholders('{a}+{b}', { a: '1', b: '2' })).toBe('1+2');
  });

  it('未命中的 token 原样保留（不吞字符）', () => {
    expect(renderPlaceholders('{name}：你好', {})).toBe('{name}：你好');
    expect(renderPlaceholders('{unknown} {name}', { name: NAME })).toBe('{unknown} ' + NAME);
  });

  it('空 vars：所有 token 保留', () => {
    expect(renderPlaceholders('心情 {emotion:L2} 提醒 {name}', {})).toBe(
      '心情 {emotion:L2} 提醒 {name}',
    );
  });

  it('署名用码点转义构造（禁止角色名字面量）', () => {
    const signature = '\u2014\u2014 ' + renderPlaceholders('{name}', { name: NAME });
    expect(signature).toBe('\u2014\u2014 ' + NAME);
  });
});

describe('clampDwellMs（02 §5 K-5：单条 3~5s 钳制）', () => {
  it('区间内原样，越界钳到 [3000,5000]', () => {
    expect(clampDwellMs(3500)).toBe(3500);
    expect(clampDwellMs(2999)).toBe(3000);
    expect(clampDwellMs(5001)).toBe(5000);
    expect(clampDwellMs(0)).toBe(3000);
    expect(clampDwellMs(-100)).toBe(3000);
  });

  it('非有限值兜底默认 4000', () => {
    expect(clampDwellMs(Number.NaN)).toBe(4000);
    expect(clampDwellMs(Number.POSITIVE_INFINITY)).toBe(5000);
  });
});

describe('bubblePriority（提醒>求助>闲聊>明信片）', () => {
  it('四值映射与排序', () => {
    expect(bubblePriority('reminder')).toBe(3);
    expect(bubblePriority('help')).toBe(2);
    expect(bubblePriority('chat')).toBe(1);
    expect(bubblePriority('postcard')).toBe(0);
    expect(bubblePriority('reminder')).toBeGreaterThan(bubblePriority('help'));
    expect(bubblePriority('help')).toBeGreaterThan(bubblePriority('chat'));
    expect(bubblePriority('chat')).toBeGreaterThan(bubblePriority('postcard'));
  });
});

describe('shouldShowUnderDnd（勿扰仅放行 reminder，01 §6.5.4 产品红线）', () => {
  it('勿扰时仅 reminder 放行，其余类别抑制', () => {
    expect(shouldShowUnderDnd('reminder', true)).toBe(true);
    expect(shouldShowUnderDnd('help', true)).toBe(false);
    expect(shouldShowUnderDnd('chat', true)).toBe(false);
    expect(shouldShowUnderDnd('postcard', true)).toBe(false);
  });

  it('非勿扰全放行', () => {
    expect(shouldShowUnderDnd('reminder', false)).toBe(true);
    expect(shouldShowUnderDnd('help', false)).toBe(true);
    expect(shouldShowUnderDnd('chat', false)).toBe(true);
    expect(shouldShowUnderDnd('postcard', false)).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 纯函数：decideBubble（§4.1 七步短路真值表）
// ---------------------------------------------------------------------------

describe('decideBubble（七步短路真值表，逐分支）', () => {
  const now = 100000;

  it('分支 1：文案为空 → drop（renderPlaceholders 后 trim 判定）', () => {
    expect(decideBubble(bubbleCmd({ text: '' }), null, undefined, now, false)).toBe('drop');
    expect(decideBubble(bubbleCmd({ text: '   ' }), null, undefined, now, false)).toBe('drop');
  });

  it('分支 2：勿扰且非 reminder → drop；reminder 放行到后续分支', () => {
    expect(decideBubble(bubbleCmd({ kind: 'chat' }), null, undefined, now, true)).toBe('drop');
    expect(decideBubble(bubbleCmd({ kind: 'reminder' }), null, undefined, now, true)).toBe('show');
  });

  it('分支 3：同状态 20s 内 → drop（对所有 kind 一律生效，含 reminder / preempt）', () => {
    const lastShown = now - 1000;
    expect(decideBubble(bubbleCmd(), null, lastShown, now, false)).toBe('drop');
    expect(
      decideBubble(bubbleCmd({ kind: 'reminder', cooldownKey: 'k' }), null, lastShown, now, false),
    ).toBe('drop');
    expect(
      decideBubble(bubbleCmd({ preempt: true, cooldownKey: 'k' }), null, lastShown, now, false),
    ).toBe('drop');
    // 边界：恰好 20000ms 时放行（抑制判定用 `<`）。
    expect(
      decideBubble(bubbleCmd({ cooldownKey: 'k' }), null, now - BUBBLE_COOLDOWN_MS, now, false),
    ).toBe('show');
  });

  it('分支 4：无有效当前气泡 → show', () => {
    expect(decideBubble(bubbleCmd(), null, undefined, now, false)).toBe('show');
  });

  it('分支 5：preempt → replace（用户交互台词即时覆盖系统台词，正交于 kind）', () => {
    const current = { kind: 'reminder' as const, cooldownKey: 'r', expiresAt: now + 1000 };
    expect(decideBubble(bubbleCmd({ preempt: true }), current, undefined, now, false)).toBe(
      'replace',
    );
  });

  it('分支 6：优先级 ≥ 当前 → replace', () => {
    const current = { kind: 'chat' as const, cooldownKey: 'c', expiresAt: now + 1000 };
    expect(decideBubble(bubbleCmd({ kind: 'help' }), current, undefined, now, false)).toBe(
      'replace',
    );
    expect(decideBubble(bubbleCmd({ kind: 'chat' }), current, undefined, now, false)).toBe(
      'replace',
    );
  });

  it('分支 7：低优先级不得打断高优先级 → drop', () => {
    const current = { kind: 'reminder' as const, cooldownKey: 'r', expiresAt: now + 1000 };
    expect(decideBubble(bubbleCmd({ kind: 'chat' }), current, undefined, now, false)).toBe('drop');
    expect(decideBubble(bubbleCmd({ kind: 'postcard' }), current, undefined, now, false)).toBe(
      'drop',
    );
  });

  it('过期 current 视为无气泡（expiresAt <= now → effectiveCurrent = null）', () => {
    const expired = { kind: 'reminder' as const, cooldownKey: 'r', expiresAt: now };
    expect(decideBubble(bubbleCmd({ kind: 'chat' }), expired, undefined, now, false)).toBe('show');
  });
});

// ---------------------------------------------------------------------------
// 纯函数：resolveBubblePlacement（三态 + 128 CSS 视口几何 + 宽容器算法覆盖）
// ---------------------------------------------------------------------------

describe('resolveBubblePlacement（右侧优先 → 左侧翻转 → 钳制）', () => {
  const GAP = 8;
  const PAD = 4;
  const TAIL_H = 10;

  it('宽容器（640）三态真实可达：right / left / clamp（算法覆盖输入，非真机视口）', () => {
    // 注：containerWidth=640 为**算法覆盖输入**（令 left/clamp 分支可达），非真机视口宽（真机 128 CSS）。
    // right：锚点居中 + 小气泡 → 右侧放得下。
    expect(
      resolveBubblePlacement({
        containerWidth: 640, bubbleWidth: 100, bubbleHeight: 40,
        anchorCx: 320, anchorTop: 24, gap: GAP, pad: PAD, tailH: TAIL_H,
      }),
    ).toEqual({ side: 'right', left: 328, top: 4 });
    // left：锚点靠右 + 中等气泡 → 右侧放不下、左侧放得下。
    expect(
      resolveBubblePlacement({
        containerWidth: 640, bubbleWidth: 100, bubbleHeight: 40,
        anchorCx: 600, anchorTop: 24, gap: GAP, pad: PAD, tailH: TAIL_H,
      }),
    ).toEqual({ side: 'left', left: 492, top: 4 });
    // clamp：锚点居中 + 超大气泡 → 两侧都放不下（left 钳到右缘 640-400-4=236）。
    expect(
      resolveBubblePlacement({
        containerWidth: 640, bubbleWidth: 400, bubbleHeight: 40,
        anchorCx: 320, anchorTop: 24, gap: GAP, pad: PAD, tailH: TAIL_H,
      }),
    ).toEqual({ side: 'clamp', left: 236, top: 4 });
  });

  it('窗口几何（128 CSS 视口 + 锚点居中 cx=64）：mw≤52 → right；mw>52 → clamp；left 不可达', () => {
    // 视口内容宽 = 128 CSS px（窗口物理 256 ÷ DPR 2）；右空间 = 左空间 = 64 - 8 - 4 = 52。
    expect(
      resolveBubblePlacement({
        containerWidth: 128, bubbleWidth: 52, bubbleHeight: 40,
        anchorCx: 64, anchorTop: 24, gap: GAP, pad: PAD, tailH: TAIL_H,
      }).side,
    ).toBe('right');
    expect(
      resolveBubblePlacement({
        containerWidth: 128, bubbleWidth: 53, bubbleHeight: 40,
        anchorCx: 64, anchorTop: 24, gap: GAP, pad: PAD, tailH: TAIL_H,
      }).side,
    ).toBe('clamp');
    // 该几何下 left 恒不可达（居中锚点几何限制，非逻辑缺陷）；扫描含 mw=128（= BUBBLE_MAX_WIDTH）。
    for (let mw = 1; mw <= 128; mw += 5) {
      const p = resolveBubblePlacement({
        containerWidth: 128, bubbleWidth: mw, bubbleHeight: 40,
        anchorCx: 64, anchorTop: 24, gap: GAP, pad: PAD, tailH: TAIL_H,
      });
      expect(p.side === 'left').toBe(false);
    }
  });

  it('A 回归护栏：BUBBLE_MAX_WIDTH ≤ 视口宽，且长气泡 clamp 后不再右出血（历史 Bug 2026-09-19）', () => {
    // 历史 Bug：BUBBLE_MAX_WIDTH 曾误取 256（> 128 视口）→ mw>wc 时左右恒不足、left 翻转旁路、恒右出血 128px。
    expect(BUBBLE_MAX_WIDTH).toBeLessThanOrEqual(128);
    expect(BUBBLE_MAX_WIDTH).toBe(PET_LOGICAL_WIDTH);
    // mw = BUBBLE_MAX_WIDTH（=128）在 128 视口 clap：left 钳到 pad，右出血 ≤ pad（旧 Bug 为 128px）。
    const pMax = resolveBubblePlacement({
      containerWidth: 128, bubbleWidth: BUBBLE_MAX_WIDTH, bubbleHeight: 40,
      anchorCx: 64, anchorTop: 24, gap: GAP, pad: PAD, tailH: TAIL_H,
    });
    expect(pMax.side).toBe('clamp');
    expect(pMax.left).toBe(PAD);
    expect(pMax.left + BUBBLE_MAX_WIDTH - 128).toBeLessThanOrEqual(PAD); // 出血 ≤ 4px（旧 128px）
    // 常见文本宽（mw ≤ wc − 2·pad = 120）→ 完全落在视口内（left + mw ≤ wc − pad），零出血。
    for (const mw of [80, 100, 116, 120]) {
      const p = resolveBubblePlacement({
        containerWidth: 128, bubbleWidth: mw, bubbleHeight: 40,
        anchorCx: 64, anchorTop: 24, gap: GAP, pad: PAD, tailH: TAIL_H,
      });
      expect(p.left).toBeGreaterThanOrEqual(PAD);
      expect(p.left + mw).toBeLessThanOrEqual(128 - PAD);
    }
  });

  it('纵向上越界向下钳制（top = max(pad, anchorTop - mh - tailH)）', () => {
    expect(
      resolveBubblePlacement({
        containerWidth: 640, bubbleWidth: 100, bubbleHeight: 40,
        anchorCx: 320, anchorTop: 20, gap: GAP, pad: PAD, tailH: TAIL_H,
      }).top,
    ).toBe(PAD); // 20-40-10 = -30 → 钳到 4。
    expect(
      resolveBubblePlacement({
        containerWidth: 640, bubbleWidth: 100, bubbleHeight: 40,
        anchorCx: 320, anchorTop: 200, gap: GAP, pad: PAD, tailH: TAIL_H,
      }).top,
    ).toBe(150); // 200-40-10 = 150（不越界，原样）。
  });

  it('气泡宽超容器：钳制不退化为负值（left = pad）', () => {
    // 注：本用例为**防御路径算法覆盖输入**（mw > wc 的病态输入，测 clamp 的 maxLeft 退化保护），
    // 非真机几何（真机视口 128 CSS，且 `BUBBLE_MAX_WIDTH` 已钳 mw ≤ 128，不会出现 mw > wc 的实况）。
    const p = resolveBubblePlacement({
      containerWidth: 256, bubbleWidth: 300, bubbleHeight: 40,
      anchorCx: 128, anchorTop: 24, gap: GAP, pad: PAD, tailH: TAIL_H,
    });
    expect(p.side).toBe('clamp');
    expect(p.left).toBe(PAD);
    expect(p.left).toBeGreaterThanOrEqual(0);
  });
});

// ---------------------------------------------------------------------------
// 纯函数：fadeAlpha（§4.2 双侧淡入淡出）
// ---------------------------------------------------------------------------

describe('fadeAlpha（双侧淡入淡出：起 0 / 爬升 / 平台 / 收尾 / 到期 0 / fadeMs=0 退化）', () => {
  const DWELL = 4000;
  const FADE = 200;

  it('起点（elapsed<=0）→ 0', () => {
    expect(fadeAlpha(0, DWELL, FADE)).toBe(0);
    expect(fadeAlpha(-1, DWELL, FADE)).toBe(0);
  });

  it('淡入爬升段：线性（100ms → 0.5）', () => {
    expect(fadeAlpha(100, DWELL, FADE)).toBeCloseTo(0.5, 10);
  });

  it('平台段：alpha = 1', () => {
    expect(fadeAlpha(2000, DWELL, FADE)).toBe(1);
  });

  it('收尾下降段：线性（剩 100ms → 0.5）', () => {
    expect(fadeAlpha(DWELL - 100, DWELL, FADE)).toBeCloseTo(0.5, 10);
  });

  it('到期（elapsed >= dwellMs）→ 0（调用方据此隐藏）', () => {
    expect(fadeAlpha(DWELL, DWELL, FADE)).toBe(0);
    expect(fadeAlpha(DWELL + 1, DWELL, FADE)).toBe(0);
  });

  it('fadeMs <= 0 退化为 1（防除零）；alpha 恒在 [0,1]', () => {
    expect(fadeAlpha(1000, DWELL, 0)).toBe(1);
    expect(fadeAlpha(1000, DWELL, -1)).toBe(1);
    for (let ms = 1; ms < DWELL; ms += 137) {
      const a = fadeAlpha(ms, DWELL, FADE);
      expect(a).toBeGreaterThanOrEqual(0);
      expect(a).toBeLessThanOrEqual(1);
    }
  });
});

// ---------------------------------------------------------------------------
// BubbleLayer 状态机（FakeBubbleView + FakeClock，不碰 DOM）
// ---------------------------------------------------------------------------

/** view 调用记录。 */
type ViewOp =
  | { op: 'setContent'; content: BubbleContent }
  | { op: 'measure' }
  | { op: 'place'; placement: BubblePlacement }
  | { op: 'setOpacity'; alpha: number }
  | { op: 'setVisible'; visible: boolean }
  | { op: 'containerWidth' };

/** 假视图：记录全部调用（含顺序），尺寸/容器宽可控。 */
class FakeBubbleView implements BubbleView {
  readonly ops: ViewOp[] = [];
  width = 100;
  height = 40;
  /** 算法测试输入、非真机视口（真机视口 = 128 CSS px；此默认仅驱动摆位分支，各用例多显式覆写）。 */
  containerW = 256;

  setContent(content: BubbleContent): void {
    this.ops.push({ op: 'setContent', content });
  }
  measure(): { width: number; height: number } {
    this.ops.push({ op: 'measure' });
    return { width: this.width, height: this.height };
  }
  place(placement: BubblePlacement): void {
    this.ops.push({ op: 'place', placement });
  }
  setOpacity(alpha: number): void {
    this.ops.push({ op: 'setOpacity', alpha });
  }
  setVisible(visible: boolean): void {
    this.ops.push({ op: 'setVisible', visible });
  }
  containerWidth(): number {
    this.ops.push({ op: 'containerWidth' });
    return this.containerW;
  }

  /** 最近一次 setContent 的内容。 */
  lastContent(): BubbleContent | undefined {
    for (let i = this.ops.length - 1; i >= 0; i--) {
      const op = this.ops[i];
      if (op !== undefined && op.op === 'setContent') {
        return op.content;
      }
    }
    return undefined;
  }
  /** setContent 调用次数。 */
  contentCalls(): number {
    return this.ops.filter((op) => op.op === 'setContent').length;
  }
  /** 最近一次 setVisible 的参数。 */
  lastVisible(): boolean | undefined {
    for (let i = this.ops.length - 1; i >= 0; i--) {
      const op = this.ops[i];
      if (op !== undefined && op.op === 'setVisible') {
        return op.visible;
      }
    }
    return undefined;
  }
  /** 最近一次 setOpacity 的参数。 */
  lastOpacity(): number | undefined {
    for (let i = this.ops.length - 1; i >= 0; i--) {
      const op = this.ops[i];
      if (op !== undefined && op.op === 'setOpacity') {
        return op.alpha;
      }
    }
    return undefined;
  }
  /** 某操作首次出现的下标（无则 -1）。 */
  indexOf(opName: ViewOp['op']): number {
    return this.ops.findIndex((op) => op.op === opName);
  }
}

/** 可控单调时钟（C3：仅本地过渡时长）。 */
class FakeClock {
  private t = 0;
  readonly now = (): number => this.t;
  advance(ms: number): void {
    this.t += ms;
  }
}

describe('BubbleLayer 状态机（惰性 flush，禁 setTimeout）', () => {
  it('show 后按序 setContent → measure → place → setVisible(true)，并按锚点摆位', () => {
    const clock = new FakeClock();
    const view = new FakeBubbleView();
    const layer = new BubbleLayer(view, {
      now: clock.now,
      anchor: () => ({ cx: 128, top: 24 }),
    });

    layer.submit(bubbleCmd({ text: 'hi' }));
    layer.flush();

    const iSet = view.indexOf('setContent');
    const iMeasure = view.indexOf('measure');
    const iPlace = view.indexOf('place');
    const iShow = view.ops.findIndex((op) => op.op === 'setVisible' && op.visible);
    expect(iSet).toBeGreaterThanOrEqual(0);
    expect(iSet).toBeLessThan(iMeasure);
    expect(iMeasure).toBeLessThan(iPlace);
    expect(iPlace).toBeLessThan(iShow);
    // 256 容器 + 锚点居中 + 100 宽气泡 → right（136, 4）。
    const placeOp = view.ops[iPlace];
    expect(placeOp).toEqual({
      op: 'place',
      placement: { side: 'right', left: 136, top: 4 },
    });
    // 起点 alpha = 0（淡入自 0 起）。
    expect(view.lastOpacity()).toBe(0);
  });

  it('连续同 key 20s 内第二次 submit 不产生新的 setContent（冷却门）', () => {
    const clock = new FakeClock();
    const view = new FakeBubbleView();
    const layer = new BubbleLayer(view, { now: clock.now });

    layer.submit(bubbleCmd({ cooldownKey: 'pool:bored' }));
    layer.flush();
    expect(view.contentCalls()).toBe(1);

    clock.advance(1000);
    layer.submit(bubbleCmd({ cooldownKey: 'pool:bored' }));
    layer.flush();
    expect(view.contentCalls()).toBe(1); // 被 20s 冷却 drop，仅透明度推进。
  });

  it('preempt 覆盖：提交新内容并重摆（不同冷却键 → 不触发 20s 同状态门）', () => {
    const clock = new FakeClock();
    const view = new FakeBubbleView();
    const layer = new BubbleLayer(view, { now: clock.now });

    layer.submit(bubbleCmd({ text: '系统台词', cooldownKey: 'sys:chat' }));
    layer.flush();
    clock.advance(100);
    // 用户交互台词与系统台词分属不同 cooldownKey（同状态门按键判定），preempt 生效。
    layer.submit(bubbleCmd({ text: '交互台词', preempt: true, cooldownKey: 'user:chat' }));
    layer.flush();

    expect(view.contentCalls()).toBe(2);
    expect(view.lastContent()?.text).toBe('交互台词');
    expect(view.lastVisible()).toBe(true);
  });

  it('flush() 推进时钟后 alpha 落在 (0,1)', () => {
    const clock = new FakeClock();
    const view = new FakeBubbleView();
    const layer = new BubbleLayer(view, { now: clock.now });

    layer.submit(bubbleCmd());
    layer.flush();
    expect(view.lastOpacity()).toBe(0);

    clock.advance(100);
    layer.flush();
    const alpha = view.lastOpacity();
    expect(alpha).toBeCloseTo(0.5, 10);
    expect(alpha).toBeGreaterThan(0);
    expect(alpha).toBeLessThan(1);
  });

  it('到期后 setVisible(false)，且此后 flush 不再产生任何 view 写入', () => {
    const clock = new FakeClock();
    const view = new FakeBubbleView();
    const layer = new BubbleLayer(view, { now: clock.now });

    layer.submit(bubbleCmd());
    layer.flush();
    const writesAfterShow = view.ops.length;

    clock.advance(4000); // 恰到 dwellMs → 到期。
    layer.flush();
    expect(view.lastVisible()).toBe(false);

    const writes = view.ops.length;
    layer.flush();
    layer.flush();
    expect(view.ops.length).toBe(writes); // 无脏且无可见气泡 → 零写入（60Hz 廉价）。
    expect(writes).toBeGreaterThan(writesAfterShow);
  });

  it('无脏且无可见气泡时 flush() 不产生任何 view 写入（幂等/廉价性断言）', () => {
    const clock = new FakeClock();
    const view = new FakeBubbleView();
    const layer = new BubbleLayer(view, { now: clock.now });

    layer.flush();
    expect(view.ops).toHaveLength(0);

    layer.submit(bubbleCmd({ text: '' })); // 空文案 → drop，零状态变更。
    layer.flush();
    expect(view.ops).toHaveLength(0);
  });

  it('dismiss 后 flush 隐藏；隐藏态复用 visibility（不写 display）', () => {
    const clock = new FakeClock();
    const view = new FakeBubbleView();
    const layer = new BubbleLayer(view, { now: clock.now });

    layer.submit(bubbleCmd());
    layer.flush();
    layer.dismiss();
    layer.flush();
    expect(view.lastVisible()).toBe(false);
  });

  it('勿扰开启：chat 被 drop（零写入），reminder 正常显示', () => {
    const clock = new FakeClock();
    const view = new FakeBubbleView();
    const layer = new BubbleLayer(view, { now: clock.now, dnd: () => true });

    layer.submit(bubbleCmd({ kind: 'chat' }));
    layer.flush();
    expect(view.ops).toHaveLength(0);

    layer.submit(bubbleCmd({ kind: 'reminder', text: '提醒' }));
    layer.flush();
    expect(view.contentCalls()).toBe(1);
  });

  it('署名：signatureEnabled && showSignature → 「—— {name}」（名字经 renderPlaceholders）', () => {
    const clock = new FakeClock();
    const view = new FakeBubbleView();
    const layer = new BubbleLayer(view, {
      now: clock.now,
      vars: () => ({ name: NAME }),
    });

    layer.submit(bubbleCmd({ kind: 'help', showSignature: true }));
    layer.flush();
    expect(view.lastContent()?.signature).toBe('\u2014\u2014 ' + NAME);
  });

  it('署名降级与开关：vars 缺 name 诚实保留 {name}；signatureEnabled=false → null', () => {
    const clock = new FakeClock();
    const viewA = new FakeBubbleView();
    const layerA = new BubbleLayer(viewA, { now: clock.now }); // vars 默认空。
    layerA.submit(bubbleCmd({ kind: 'postcard', showSignature: true }));
    layerA.flush();
    expect(viewA.lastContent()?.signature).toBe('\u2014\u2014 {name}');

    const viewB = new FakeBubbleView();
    const layerB = new BubbleLayer(viewB, {
      now: clock.now,
      vars: () => ({ name: NAME }),
      signatureEnabled: () => false,
    });
    layerB.submit(bubbleCmd({ kind: 'postcard', showSignature: true }));
    layerB.flush();
    expect(viewB.lastContent()?.signature).toBeNull();
  });

  it('高对比取或：cmd.highContrast 与用户设置任一为真即生效', () => {
    const clock = new FakeClock();
    const viewA = new FakeBubbleView();
    const layerA = new BubbleLayer(viewA, { now: clock.now });
    layerA.submit(bubbleCmd({ highContrast: true }));
    layerA.flush();
    expect(viewA.lastContent()?.highContrast).toBe(true);

    const viewB = new FakeBubbleView();
    const layerB = new BubbleLayer(viewB, { now: clock.now, highContrast: () => true });
    layerB.submit(bubbleCmd({ highContrast: false }));
    layerB.flush();
    expect(viewB.lastContent()?.highContrast).toBe(true);
  });

  it('actions 透传到 content（点击交出由 DomBubbleView 的回调承担）', () => {
    const clock = new FakeClock();
    const view = new FakeBubbleView();
    const layer = new BubbleLayer(view, { now: clock.now });

    layer.submit(bubbleCmd({ actions: [{ id: 'feed', label: '去喂食' }] }));
    layer.flush();
    expect(view.lastContent()?.actions).toEqual([{ id: 'feed', label: '去喂食' }]);
  });
});
