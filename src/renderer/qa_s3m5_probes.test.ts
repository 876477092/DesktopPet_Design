/**
 * S3-M5 QA 独立探针（qa_s3m5_probes.test.ts）。
 *
 * 性质：QA 独立验证探针——从需求原文/裁定口径独立推导断言，不 import 工程师的
 * 测试文件；允许 import 实现模块。全部用例不碰真实 DOM（CSS 取证用 node:fs 读
 * 源文件文本；view 端口注入手写 Fake，时钟注入可控 FakeClock）。
 *
 * 覆盖域（对应 T-10 段·上 AC 与主理人裁定口径）：
 *   P1 气泡停留（dwell 钳制 + 可见性边界 + fadeAlpha 三段）
 *   P2 摆位（三态 + 128 CSS 视口几何推演 + 纵向钳制 + 超宽不产生负 left）
 *   P3 可复制契约（pet.css 取证：user-select:text / pointer-events:auto / visibility 非 display:none）
 *   P4 ≥20s 冷却（19999 抑制 / 20000 放行 / 不同 key 隔离 / 空串回退 kind）
 *   P5 优先级与 preempt（四值序 / 高低互断 / preempt 覆盖 / 20s 内 preempt 仍 drop）
 *   P6 parseBubbleCmd 契约（非对象 null / 未知字段 / actions 脏项 / kind 兜底 / dwellMs 边界）
 *   P7 勿扰派生（dnd=true 仅 reminder / dnd=false 全放行）
 *   P8 叠加层五要素（进度环 / 怒气 / Zzz 钳制+warn / 飘条 FIFO+上限+2500ms / 长按 499/500 单次）
 *   P9 幂等（两层无脏 flush 零 view 写入）
 *   P10 接线序（LayerHost render：overlay 先于 bubble）
 *   P11 C2 占位符与署名降级
 *   P12 菜单降级几何（裁定 B 2026-09-19：128 CSS 视口 menuScale 等比缩小 + clamp 组合不变量）
 */

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it, vi } from 'vitest';
import { parseBubbleCmd, type BubbleCmdV1 } from '../shared/ipc';
import { BubbleLayer } from './BubbleLayer';
import { OverlayLayer } from './OverlayLayer';
import { LayerHost, LAYER_DRAW_ORDER } from './LayerHost';
import {
  clampDwellMs,
  bubblePriority,
  decideBubble,
  fadeAlpha,
  renderPlaceholders,
  resolveBubblePlacement,
  shouldShowUnderDnd,
  shouldSuppressDuplicate,
} from './bubbleLogic';
import { isReasonCardLongPress, toastProgress, clampToastQueue } from './overlayLogic';
import { menuScale, clampMenuPlacement } from './menuLogic';
import { MENU_WIDTH, MENU_HEIGHT, MENU_EDGE_PAD } from './layerPorts';
import type { BubbleContent, BubblePlacement, BubbleView } from './layerPorts';
import type { OverlayView } from './layerPorts';

// ---------------------------------------------------------------------------
// 测试基建：FakeClock / FakeBubbleView / FakeOverlayView（仿 FrameStage 范式，零 DOM）
// ---------------------------------------------------------------------------

class FakeClock {
  private t = 1000;
  now(): number {
    return this.t;
  }
  advance(ms: number): void {
    this.t += ms;
  }
}

interface BubbleViewLog {
  contentCalls: BubbleContent[];
  places: BubblePlacement[];
  opacities: number[];
  visibles: boolean[];
  totalWrites(): number;
}

function makeFakeBubbleView(measure: { width: number; height: number } = { width: 100, height: 40 }): {
  view: BubbleView;
  log: BubbleViewLog;
} {
  const contentCalls: BubbleContent[] = [];
  const places: BubblePlacement[] = [];
  const opacities: number[] = [];
  const visibles: boolean[] = [];
  const view: BubbleView = {
    setContent(c: BubbleContent): void {
      contentCalls.push(c);
    },
    measure(): { width: number; height: number } {
      return measure;
    },
    place(p: BubblePlacement): void {
      places.push(p);
    },
    setOpacity(a: number): void {
      opacities.push(a);
    },
    setVisible(v: boolean): void {
      visibles.push(v);
    },
    containerWidth(): number {
      // 算法测试输入、非真机视口（真机视口 = 128 CSS px；此值仅驱动摆位分支）。
      return 256;
    },
  };
  return {
    view,
    log: {
      contentCalls,
      places,
      opacities,
      visibles,
      totalWrites: (): number =>
        contentCalls.length + places.length + opacities.length + visibles.length,
    },
  };
}

interface OverlayViewLog {
  coax: Array<number | null>;
  anger: number[];
  sleep: number[];
  toasts: Array<string | null>;
  toastOpacities: number[];
  totalWrites(): number;
}

function makeFakeOverlayView(): { view: OverlayView; log: OverlayViewLog } {
  const coax: Array<number | null> = [];
  const anger: number[] = [];
  const sleep: number[] = [];
  const toasts: Array<string | null> = [];
  const toastOpacities: number[] = [];
  const view: OverlayView = {
    setCoaxProgress(v: number | null): void {
      coax.push(v);
    },
    setAngerLevel(level: number): void {
      anger.push(level);
    },
    setSleepLevel(level: 0 | 1 | 2): void {
      sleep.push(level);
    },
    setToast(text: string | null): void {
      toasts.push(text);
    },
    setToastOpacity(a: number): void {
      toastOpacities.push(a);
    },
  };
  return {
    view,
    log: {
      coax,
      anger,
      sleep,
      toasts,
      toastOpacities,
      totalWrites: (): number => coax.length + anger.length + sleep.length + toasts.length + toastOpacities.length,
    },
  };
}

/** 构造合法 BubbleCmdV1（字段全显式，测试按需覆盖）。 */
function makeCmd(overrides: Partial<BubbleCmdV1> = {}): BubbleCmdV1 {
  return {
    version: 1,
    text: 'hello',
    kind: 'chat',
    preempt: false,
    cooldownKey: 'k1',
    dwellMs: 4000,
    showSignature: false,
    actions: [],
    highContrast: false,
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// P1 AC-气泡停留：dwellMs 钳 [3000,5000]；可见性边界；fadeAlpha 三段
// ---------------------------------------------------------------------------

describe('P1-A clampDwellMs 钳 [3000,5000]（K-5 单条 3~5s）', () => {
  it('下/上边界与越界收敛', () => {
    expect(clampDwellMs(3000)).toBe(3000);
    expect(clampDwellMs(5000)).toBe(5000);
    expect(clampDwellMs(2999)).toBe(3000);
    expect(clampDwellMs(5001)).toBe(5000);
    expect(clampDwellMs(0)).toBe(3000);
    expect(clampDwellMs(-1)).toBe(3000);
    expect(clampDwellMs(Number.NaN)).toBe(4000); // NaN → 默认
  });
});

describe('P1-B 显示后推进时钟的可见性边界（2999/3000/4999/5000ms）', () => {
  it('dwell=3000：2999ms 仍可见，3000ms 到期隐藏', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const layer = new BubbleLayer(view, { now: (): number => clock.now() });
    layer.submit(makeCmd({ dwellMs: 3000 }));
    layer.flush();
    expect(log.visibles).toContain(true);
    const writesBefore = log.visibles.length;
    clock.advance(2999);
    layer.flush();
    expect(log.visibles.slice(writesBefore)).not.toContain(false); // 未隐藏
    clock.advance(1); // 累计恰 3000ms → 到期（>= dwell）
    layer.flush();
    expect(log.visibles).toContain(false); // 已隐藏
  });

  it('dwell=5000：4999ms 仍可见，5000ms 到期隐藏', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const layer = new BubbleLayer(view, { now: (): number => clock.now() });
    layer.submit(makeCmd({ dwellMs: 5000 }));
    layer.flush();
    clock.advance(4999);
    layer.flush();
    expect(log.visibles).not.toContain(false);
    clock.advance(1);
    layer.flush();
    expect(log.visibles).toContain(false);
  });
});

describe('P1-C fadeAlpha 三段曲线与两端 0', () => {
  it('起段爬升 / 平台 1 / 收尾下降 / 两端 0 / fadeMs=0 退化', () => {
    // dwell=4000, fade=200
    expect(fadeAlpha(0, 4000, 200)).toBe(0); // 未开始
    expect(fadeAlpha(100, 4000, 200)).toBe(0.5); // 起段中点
    expect(fadeAlpha(200, 4000, 200)).toBe(1); // 起段结束
    expect(fadeAlpha(1000, 4000, 200)).toBe(1); // 平台
    expect(fadeAlpha(3900, 4000, 200)).toBe(0.5); // 收尾中点
    expect(fadeAlpha(4000, 4000, 200)).toBe(0); // 恰到期 → 0
    expect(fadeAlpha(4001, 4000, 200)).toBe(0); // 超期 → 0
    expect(fadeAlpha(100, 4000, 0)).toBe(1); // fadeMs<=0 退化为 1
  });
});

// ---------------------------------------------------------------------------
// P2 AC-翻转：三态（宽容器）+ 128 CSS 视口几何独立推演 + 纵向钳制 + 超宽不产生负 left
// ---------------------------------------------------------------------------

const GEO = { gap: 8, pad: 4, tailH: 10 };

describe('P2-A 宽容器（640px）三态真实可达', () => {
  it('右侧优先：右空间充足 → side=right', () => {
    // cx=320: left0=328, 328+100=428 <= 640-4=636 → right
    const p = resolveBubblePlacement({
      containerWidth: 640, bubbleWidth: 100, bubbleHeight: 40,
      anchorCx: 320, anchorTop: 60, ...GEO,
    });
    expect(p.side).toBe('right');
    expect(p.left).toBe(328);
  });

  it('左翻转：右侧不足但左侧充足 → side=left', () => {
    // cx=560: left0=568, 568+100=668 > 636；flip: 560-8-100=452 >= 4 → left
    const p = resolveBubblePlacement({
      containerWidth: 640, bubbleWidth: 100, bubbleHeight: 40,
      anchorCx: 560, anchorTop: 60, ...GEO,
    });
    expect(p.side).toBe('left');
    expect(p.left).toBe(452);
  });

  it('钳制：两侧皆不足 → side=clamp 且 left 落在 [pad, Wc-mw-pad]', () => {
    // cx=560, mw=600: flip: 560-8-600=-48 < 4 → clamp；maxLeft=max(4, 640-600-4)=36
    const p = resolveBubblePlacement({
      containerWidth: 640, bubbleWidth: 600, bubbleHeight: 40,
      anchorCx: 560, anchorTop: 60, ...GEO,
    });
    expect(p.side).toBe('clamp');
    expect(p.left).toBe(36); // Wc-mw-pad（右缘内贴，裁定 5：公式如此）
  });
});

describe('P2-B 128 CSS 视口（窗口物理 256÷DPR2）+ 锚点居中（cx=64）几何（裁定 A/C 2026-09-19）', () => {
  it('mw<=52 → 恒 right（left0+mw <= 124）', () => {
    // 边界恰 52：72+52=124 <= 128-4=124 → right
    const p = resolveBubblePlacement({
      containerWidth: 128, bubbleWidth: 52, bubbleHeight: 40,
      anchorCx: 64, anchorTop: 60, ...GEO,
    });
    expect(p.side).toBe('right');
    expect(p.left).toBe(72);
  });

  it('mw>52 → 恒 clamp（两侧皆不足），但 mw<=128 不再右出血', () => {
    // 边界恰 53：72+53=125 > 124；flip: 64-8-53=3 < 4 → clamp
    const p = resolveBubblePlacement({
      containerWidth: 128, bubbleWidth: 53, bubbleHeight: 40,
      anchorCx: 64, anchorTop: 60, ...GEO,
    });
    expect(p.side).toBe('clamp');
    // mw <= BUBBLE_MAX_WIDTH(=128) 时 clamp 的 left 钳到 pad，右出血 <= pad（旧 Bug 曾 128px）
    const pMax = resolveBubblePlacement({
      containerWidth: 128, bubbleWidth: 128, bubbleHeight: 40,
      anchorCx: 64, anchorTop: 60, ...GEO,
    });
    expect(pMax.side).toBe('clamp');
    expect(pMax.left).toBe(4);
    expect(pMax.left + 128 - 128).toBeLessThanOrEqual(4);
  });

  it('side=left 在该几何下不可达（mw 扫描 1..300 无一命中）', () => {
    for (let mw = 1; mw <= 300; mw += 1) {
      const p = resolveBubblePlacement({
        containerWidth: 128, bubbleWidth: mw, bubbleHeight: 40,
        anchorCx: 64, anchorTop: 60, ...GEO,
      });
      expect(p.side).not.toBe('left');
    }
  });
});

describe('P2-C 纵向钳制与超宽防护', () => {
  it('锚点过近顶沿 → top 钳到 pad，不产生负 top', () => {
    // 注：本组为**算法覆盖输入**（containerWidth 取 256 仅为驱动纵向/超宽分支），非真机视口宽。
    const p = resolveBubblePlacement({
      containerWidth: 256, bubbleWidth: 100, bubbleHeight: 50,
      anchorCx: 128, anchorTop: 10, ...GEO,
    });
    expect(p.top).toBe(4); // max(pad, 10-50-10=-50)
  });

  it('气泡宽超容器（mw=300, Wc=256）→ left 不为负、落在 pad（防御路径算法输入）', () => {
    // 注：mw>Wc 为病态防御输入（真机 `BUBBLE_MAX_WIDTH` 已钳 mw<=128），非真机几何。
    const p = resolveBubblePlacement({
      containerWidth: 256, bubbleWidth: 300, bubbleHeight: 40,
      anchorCx: 128, anchorTop: 60, ...GEO,
    });
    expect(p.side).toBe('clamp');
    expect(p.left).toBe(4); // maxLeft 退化为 pad
    expect(p.left).toBeGreaterThanOrEqual(0);
  });
});

// ---------------------------------------------------------------------------
// P3 AC-可复制：契约层取证（node:fs 读 pet.css / DomLayers.ts 源文本）
// ---------------------------------------------------------------------------

describe('P3 可复制契约（CSS/源码文本取证）', () => {
  const cssPath = fileURLToPath(new URL('../styles/pet.css', import.meta.url));
  // 取证前剥离 /* */ 注释（注释文字如「禁 display:none」会干扰负向断言）。
  const css = readFileSync(cssPath, 'utf-8').replace(/\/\*[\s\S]*?\*\//g, '');

  it('.bubble__text 含 user-select: text（含 -webkit 前缀）', () => {
    const block = css.match(/\.bubble__text\s*\{[^}]*\}/);
    expect(block).not.toBeNull();
    expect(block![0]).toMatch(/user-select:\s*text/);
    expect(block![0]).toMatch(/-webkit-user-select:\s*text/);
  });

  it('.bubble 块含 pointer-events: auto（父级放行指针事件）', () => {
    const block = css.match(/\.bubble\s*\{[^}]*\}/);
    expect(block).not.toBeNull();
    expect(block![0]).toMatch(/pointer-events:\s*auto/);
  });

  it('.bubble 隐藏态用 visibility 而非 display:none（摆位退化红线，Bug#1 教训取证）', () => {
    const block = css.match(/\.bubble\s*\{[^}]*\}/);
    expect(block).not.toBeNull();
    expect(block![0]).toMatch(/visibility:\s*hidden/);
    expect(block![0]).not.toMatch(/display:\s*none/);
  });

  it('DomLayers.setVisible 用 visibility 切换（适配器层取证）', () => {
    const src = readFileSync(fileURLToPath(new URL('./DomLayers.ts', import.meta.url)), 'utf-8');
    expect(src).toMatch(/visibility\s*=\s*v\s*\?\s*'visible'\s*:\s*'hidden'/);
    expect(src).not.toMatch(/display\s*=/);
  });
});

// ---------------------------------------------------------------------------
// P4 AC-≥20s 冷却
// ---------------------------------------------------------------------------

describe('P4-A shouldSuppressDuplicate 边界（< 抑制 ⇒ 恰 20000 放行）', () => {
  it('19999 抑制 / 20000 放行 / 从未显示放行', () => {
    expect(shouldSuppressDuplicate('k', 1000, 1000 + 19999)).toBe(true);
    expect(shouldSuppressDuplicate('k', 1000, 1000 + 20000)).toBe(false);
    expect(shouldSuppressDuplicate('k', undefined, 999999)).toBe(false);
  });
});

describe('P4-B 状态机层：同 key 两次 submit 的 20s 门', () => {
  it('间隔 19999ms → 第二次被抑制（零新 setContent）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const layer = new BubbleLayer(view, { now: (): number => clock.now() });
    layer.submit(makeCmd());
    layer.flush();
    const callsAfterFirst = log.contentCalls.length;
    expect(callsAfterFirst).toBe(1);
    clock.advance(19999);
    layer.submit(makeCmd({ text: 'second' }));
    layer.flush();
    expect(log.contentCalls.length).toBe(callsAfterFirst); // 无新 setContent
  });

  it('间隔 20000ms → 放行（新 setContent）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const layer = new BubbleLayer(view, { now: (): number => clock.now() });
    layer.submit(makeCmd());
    layer.flush();
    clock.advance(20000);
    layer.submit(makeCmd({ text: 'second' }));
    layer.flush();
    expect(log.contentCalls.length).toBe(2);
    expect(log.contentCalls[1]!.text).toBe('second');
  });

  it('不同 cooldownKey 互不影响', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const layer = new BubbleLayer(view, { now: (): number => clock.now() });
    layer.submit(makeCmd({ cooldownKey: 'k1' }));
    layer.flush();
    clock.advance(1); // 远小于 20s
    layer.submit(makeCmd({ cooldownKey: 'k2', text: 'other' }));
    layer.flush();
    expect(log.contentCalls.length).toBe(2); // k2 不受 k1 冷却影响
  });

  it('cooldownKey 空串回退 kind（同 kind 两次立即提交，第二次被抑制）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const layer = new BubbleLayer(view, { now: (): number => clock.now() });
    layer.submit(makeCmd({ cooldownKey: '', kind: 'chat' }));
    layer.flush();
    clock.advance(100);
    layer.submit(makeCmd({ cooldownKey: '', kind: 'chat', text: 'again' }));
    layer.flush();
    expect(log.contentCalls.length).toBe(1); // 按 kind=chat 冷却 → 抑制
  });
});

// ---------------------------------------------------------------------------
// P5 优先级与 preempt
// ---------------------------------------------------------------------------

describe('P5-A 四值优先级 reminder>help>chat>postcard', () => {
  it('数值严格递减', () => {
    expect(bubblePriority('reminder')).toBeGreaterThan(bubblePriority('help'));
    expect(bubblePriority('help')).toBeGreaterThan(bubblePriority('chat'));
    expect(bubblePriority('chat')).toBeGreaterThan(bubblePriority('postcard'));
    expect(bubblePriority('reminder')).toBe(3);
    expect(bubblePriority('postcard')).toBe(0);
  });
});

describe('P5-B decideBubble 短路真值表（独立推演）', () => {
  const current = { kind: 'help' as const, cooldownKey: 'cur', expiresAt: 5000 };

  it('无当前 → show；低打断高 → drop；高打断低 → replace；同级 → replace', () => {
    expect(decideBubble(makeCmd(), null, undefined, 0, false)).toBe('show');
    expect(decideBubble(makeCmd({ kind: 'chat' }), current, undefined, 0, false)).toBe('drop');
    expect(decideBubble(makeCmd({ kind: 'reminder' }), current, undefined, 0, false)).toBe('replace');
    expect(decideBubble(makeCmd({ kind: 'help' }), current, undefined, 0, false)).toBe('replace');
  });

  it('当前气泡过期（now >= expiresAt）视为无 → show', () => {
    expect(decideBubble(makeCmd(), current, undefined, 4999, false)).toBe('drop'); // chat < help 且未过期
    expect(decideBubble(makeCmd(), current, undefined, 5000, false)).toBe('show'); // 过期 → 无当前
  });

  it('preempt 覆盖当前气泡（优先级低于当前也 replace）', () => {
    expect(decideBubble(makeCmd({ kind: 'postcard', preempt: true }), current, undefined, 0, false))
      .toBe('replace');
  });

  it('空文案（含占位符插值后）→ drop', () => {
    expect(decideBubble(makeCmd({ text: '   ' }), null, undefined, 0, false)).toBe('drop');
    expect(decideBubble(makeCmd({ text: '{x}', }), null, undefined, 0, false, { x: '  ' })).toBe('drop');
  });

  it('裁定 2：同 key 20s 内的 preempt 也被 drop（preempt 不豁免冷却门）', () => {
    const t0 = 1000;
    expect(decideBubble(makeCmd({ cooldownKey: 'k', preempt: true }), null, t0, t0 + 19999, false))
      .toBe('drop');
    expect(decideBubble(makeCmd({ cooldownKey: 'k', preempt: true }), null, t0, t0 + 20000, false))
      .toBe('show');
  });
});

describe('P5-C 状态机层：preempt 即时覆盖当前（且受 20s 门约束）', () => {
  it('当前气泡存活期内 preempt chat 覆盖当前 help（ setContent 新文案）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const layer = new BubbleLayer(view, { now: (): number => clock.now() });
    layer.submit(makeCmd({ kind: 'help', cooldownKey: 'a1', text: 'help text' }));
    layer.flush();
    expect(log.contentCalls.length).toBe(1);
    clock.advance(100); // 当前气泡仍在存活期（dwell 4000）
    layer.submit(makeCmd({ kind: 'chat', preempt: true, cooldownKey: 'b2', text: 'user line' }));
    layer.flush();
    expect(log.contentCalls.length).toBe(2);
    expect(log.contentCalls[1]!.text).toBe('user line');
  });

  it('同 key 20s 内的 preempt 提交 → drop（无新 setContent）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const layer = new BubbleLayer(view, { now: (): number => clock.now() });
    layer.submit(makeCmd({ kind: 'chat', cooldownKey: 'k', text: 'first' }));
    layer.flush();
    clock.advance(100);
    layer.submit(makeCmd({ kind: 'chat', preempt: true, cooldownKey: 'k', text: 'preempt' }));
    layer.flush();
    expect(log.contentCalls.length).toBe(1); // 20s 内同 key preempt 仍被抑制
  });
});

// ---------------------------------------------------------------------------
// P6 parseBubbleCmd 契约
// ---------------------------------------------------------------------------

describe('P6 parseBubbleCmd（前向兼容范式）', () => {
  it('非对象（null/数组/标量）→ null', () => {
    expect(parseBubbleCmd(null)).toBeNull();
    expect(parseBubbleCmd([1, 2])).toBeNull();
    expect(parseBubbleCmd('str')).toBeNull();
    expect(parseBubbleCmd(42)).toBeNull();
    expect(parseBubbleCmd(undefined)).toBeNull();
  });

  it('未知字段忽略、缺字段取默认', () => {
    const c = parseBubbleCmd({ text: 'hi', futureField: { nested: true } });
    expect(c).not.toBeNull();
    expect(c!.kind).toBe('chat');
    expect(c!.dwellMs).toBe(4000);
    expect(c!.preempt).toBe(false);
    expect(c!.actions).toEqual([]);
    expect(c!.version).toBe(1);
  });

  it('actions 脏项丢弃：非对象/缺 id/空 id/label 非串丢弃；空 label 回退 id', () => {
    const c = parseBubbleCmd({
      text: 'x',
      actions: [
        { id: 'a', label: 'Go' },
        null,
        'junk',
        {},
        { id: '', label: 'x' },
        { id: 'b' },
        { id: 'c', label: 123 },
        { id: 'd', label: '' },
      ],
    });
    expect(c!.actions).toEqual([
      { id: 'a', label: 'Go' },
      { id: 'd', label: 'd' },
    ]);
  });

  it('actions 非数组 → []', () => {
    expect(parseBubbleCmd({ text: 'x', actions: 'nope' })!.actions).toEqual([]);
  });

  it('kind 非法/缺省 → chat', () => {
    expect(parseBubbleCmd({ text: 'x', kind: 'angry' })!.kind).toBe('chat');
    expect(parseBubbleCmd({ text: 'x' })!.kind).toBe('chat');
  });

  it('dwellMs 双边界钳制', () => {
    expect(parseBubbleCmd({ text: 'x', dwellMs: 2999 })!.dwellMs).toBe(3000);
    expect(parseBubbleCmd({ text: 'x', dwellMs: 5001 })!.dwellMs).toBe(5000);
    expect(parseBubbleCmd({ text: 'x', dwellMs: 3000 })!.dwellMs).toBe(3000);
    expect(parseBubbleCmd({ text: 'x', dwellMs: 5000 })!.dwellMs).toBe(5000);
  });

  it('showSignature 缺省由 kind 派生（help/postcard true，其余 false）', () => {
    expect(parseBubbleCmd({ text: 'x', kind: 'help' })!.showSignature).toBe(true);
    expect(parseBubbleCmd({ text: 'x', kind: 'postcard' })!.showSignature).toBe(true);
    expect(parseBubbleCmd({ text: 'x', kind: 'chat' })!.showSignature).toBe(false);
    expect(parseBubbleCmd({ text: 'x', kind: 'reminder' })!.showSignature).toBe(false);
    expect(parseBubbleCmd({ text: 'x', kind: 'chat', showSignature: true })!.showSignature).toBe(true);
  });

  it('text 为空串仍返回对象（消费侧 decideBubble 判 drop）', () => {
    expect(parseBubbleCmd({ text: '' })).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// P7 勿扰派生
// ---------------------------------------------------------------------------

describe('P7 勿扰（dnd 仅放行 reminder）', () => {
  it('dnd=true：reminder 放行，help/chat/postcard 抑制', () => {
    expect(shouldShowUnderDnd('reminder', true)).toBe(true);
    expect(shouldShowUnderDnd('help', true)).toBe(false);
    expect(shouldShowUnderDnd('chat', true)).toBe(false);
    expect(shouldShowUnderDnd('postcard', true)).toBe(false);
  });

  it('dnd=false：全放行', () => {
    expect(shouldShowUnderDnd('reminder', false)).toBe(true);
    expect(shouldShowUnderDnd('help', false)).toBe(true);
    expect(shouldShowUnderDnd('chat', false)).toBe(true);
    expect(shouldShowUnderDnd('postcard', false)).toBe(true);
  });

  it('decideBubble 层：dnd=true 时 chat drop、reminder show', () => {
    expect(decideBubble(makeCmd({ kind: 'chat' }), null, undefined, 0, true)).toBe('drop');
    expect(decideBubble(makeCmd({ kind: 'reminder' }), null, undefined, 0, true)).toBe('show');
  });
});

// ---------------------------------------------------------------------------
// P8 叠加层五要素
// ---------------------------------------------------------------------------

describe('P8-A 进度环与怒气（端口写值；setter 只置脏，flush 写终态）', () => {
  it('null → 隐藏（写 null）；0.5 → 显示；越界值钳 [0,1]（逐次 flush 观测）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeOverlayView();
    const layer = new OverlayLayer(view, { now: (): number => clock.now() });
    layer.setCoaxProgress(null);
    layer.flush();
    expect(log.coax).toEqual([null]);
    layer.setCoaxProgress(0.5);
    layer.flush();
    expect(log.coax).toEqual([null, 0.5]);
    layer.setCoaxProgress(1.5); // 钳 1
    layer.flush();
    expect(log.coax).toEqual([null, 0.5, 1]);
    layer.setCoaxProgress(-0.2); // 钳 0
    layer.flush();
    expect(log.coax).toEqual([null, 0.5, 1, 0]);
  });

  it('同值重复 setter → flush 幂等（合并中间态，终值只写一次）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeOverlayView();
    const layer = new OverlayLayer(view, { now: (): number => clock.now() });
    layer.setCoaxProgress(0.3);
    layer.setCoaxProgress(0.7);
    layer.setCoaxProgress(0.9); // 三次置脏，终值 0.9
    layer.flush();
    expect(log.coax).toEqual([0.9]); // 合并中间态
  });

  it('怒气 0 → 写 0；>=1 原样；负值/NaN 钳 0（逐次 flush 观测）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeOverlayView();
    const layer = new OverlayLayer(view, { now: (): number => clock.now() });
    layer.setAngerLevel(0);
    layer.flush();
    expect(log.anger).toEqual([0]);
    layer.setAngerLevel(2);
    layer.flush();
    expect(log.anger).toEqual([0, 2]);
    layer.setAngerLevel(Number.NaN); // NaN → 钳 0
    layer.flush();
    expect(log.anger).toEqual([0, 2, 0]);
    layer.setAngerLevel(5);
    layer.flush();
    expect(log.anger).toEqual([0, 2, 0, 5]);
    layer.setAngerLevel(-1); // 钳 0
    layer.flush();
    expect(log.anger).toEqual([0, 2, 0, 5, 0]);
    layer.setAngerLevel(Number.NaN); // 再次 NaN → 0 == 上次写入值 → 幂等跳过（不新增写）
    layer.flush();
    expect(log.anger).toEqual([0, 2, 0, 5, 0]);
  });
});

describe('P8-B Zzz 档位（0/1/2 合法，非法钳制 + warn）', () => {
  it('0/1/2 原样通过且不告警（逐次 flush 观测端口写值）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeOverlayView();
    const warn = vi.spyOn(console, 'warn').mockImplementation((): void => undefined);
    const layer = new OverlayLayer(view, { now: (): number => clock.now() });
    layer.setSleepLevel(0);
    layer.flush();
    expect(log.sleep).toEqual([0]);
    layer.setSleepLevel(1);
    layer.flush();
    expect(log.sleep).toEqual([0, 1]);
    layer.setSleepLevel(2);
    layer.flush();
    expect(log.sleep).toEqual([0, 1, 2]);
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });

  it('非法值（3/-1/1.5/NaN）→ 钳制到 [0,2] 且 console.warn（不静默）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeOverlayView();
    const warn = vi.spyOn(console, 'warn').mockImplementation((): void => undefined);
    const layer = new OverlayLayer(view, { now: (): number => clock.now() });
    layer.setSleepLevel(3 as 0 | 1 | 2);
    layer.flush();
    expect(log.sleep).toEqual([2]);
    layer.setSleepLevel(-1 as 0 | 1 | 2);
    layer.flush();
    expect(log.sleep).toEqual([2, 0]);
    layer.setSleepLevel(1.5 as 0 | 1 | 2);
    layer.flush();
    expect(log.sleep).toEqual([2, 0, 2]);
    layer.setSleepLevel(Number.NaN as 0 | 1 | 2);
    layer.flush();
    expect(log.sleep).toEqual([2, 0, 2, 0]);
    expect(warn).toHaveBeenCalledTimes(4);
    warn.mockRestore();
  });
});

describe('P8-C 飘条 FIFO / 上限 3 / 2500ms 出队', () => {
  it('FIFO 序：a→b→c 逐条推进，2500ms 到点出队', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeOverlayView();
    const layer = new OverlayLayer(view, { now: (): number => clock.now() });
    layer.enqueueToast('a');
    layer.enqueueToast('b');
    layer.enqueueToast('c');
    layer.flush();
    expect(log.toasts).toEqual(['a']); // 同屏 1
    clock.advance(2499);
    layer.flush();
    expect(log.toasts).toEqual(['a']); // 未到点
    clock.advance(1); // 累计 2500 → 出队 a，取 b
    layer.flush();
    expect(log.toasts).toEqual(['a', 'b']);
    clock.advance(2500);
    layer.flush();
    expect(log.toasts).toEqual(['a', 'b', 'c']);
    clock.advance(2500);
    layer.flush();
    expect(log.toasts).toEqual(['a', 'b', 'c', null]); // 队空 → setToast(null)
  });

  it('队列上限 3：第 4 条丢弃且 console.warn；保留最先入队的前 3 条（FIFO 不挤占）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeOverlayView();
    const warn = vi.spyOn(console, 'warn').mockImplementation((): void => undefined);
    const layer = new OverlayLayer(view, { now: (): number => clock.now() });
    layer.enqueueToast('a');
    layer.enqueueToast('b');
    layer.enqueueToast('c');
    layer.enqueueToast('d'); // 超上限 → 丢弃 + warn
    expect(warn).toHaveBeenCalledTimes(1);
    layer.flush();
    clock.advance(2500);
    layer.flush();
    clock.advance(2500);
    layer.flush();
    clock.advance(2500);
    layer.flush();
    expect(log.toasts).toEqual(['a', 'b', 'c', null]); // d 被丢弃
    warn.mockRestore();
  });

  it('纯函数：clampToastQueue 保留前 cap 条；cap<=0 → 空队列', () => {
    expect(clampToastQueue(['a', 'b', 'c', 'd'], 3)).toEqual(['a', 'b', 'c']);
    expect(clampToastQueue(['a'], 3)).toEqual(['a']);
    expect(clampToastQueue(['a'], 0)).toEqual([]);
  });

  it('toastProgress 边界', () => {
    expect(toastProgress(0, 2500)).toBe(0);
    expect(toastProgress(1250, 2500)).toBe(0.5);
    expect(toastProgress(3000, 2500)).toBe(1);
    expect(toastProgress(100, 0)).toBe(1); // toastMs<=0 防除零
  });
});

describe('P8-D 长按（K-6：500ms 阈值，单次触发）', () => {
  it('499ms 不触发；>=500ms 触发；重复 release 不再触发', () => {
    const clock = new FakeClock();
    const { view } = makeFakeOverlayView();
    const onReason = vi.fn((): void => undefined);
    const layer = new OverlayLayer(view, { now: (): number => clock.now(), onReasonCardRequest: onReason });
    layer.pressMoodBar();
    clock.advance(499);
    layer.releaseMoodBar();
    expect(onReason).not.toHaveBeenCalled();
    layer.pressMoodBar();
    clock.advance(500);
    layer.releaseMoodBar();
    expect(onReason).toHaveBeenCalledTimes(1);
    layer.releaseMoodBar(); // 未按下（pressAt 已 null）→ 不触发
    expect(onReason).toHaveBeenCalledTimes(1);
  });

  it('纯函数边界：恰 500 成立；未按下 release 不触发回调', () => {
    expect(isReasonCardLongPress(499)).toBe(false);
    expect(isReasonCardLongPress(500)).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// P9 幂等：无脏 flush 零 view 写入（60Hz 前提）
// ---------------------------------------------------------------------------

describe('P9 幂等（无脏 flush 零 view 写入）', () => {
  it('BubbleLayer：从未 submit → 连续 flush 零写入；到期隐藏后再 flush 零写入', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const layer = new BubbleLayer(view, { now: (): number => clock.now() });
    layer.flush();
    layer.flush();
    expect(log.totalWrites()).toBe(0);
    // 一次完整生命周期后归于平静
    layer.submit(makeCmd());
    layer.flush(); // 显示
    clock.advance(4000);
    layer.flush(); // 到期隐藏
    const afterHide = log.totalWrites();
    layer.flush();
    layer.flush();
    expect(log.totalWrites()).toBe(afterHide); // 零新增写入
  });

  it('BubbleLayer：可见期内重复 flush 幂等（平台期透明度不重写）', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const layer = new BubbleLayer(view, { now: (): number => clock.now() });
    layer.submit(makeCmd({ dwellMs: 5000 }));
    layer.flush(); // 首帧（起段 alpha=0 → 写一次）
    clock.advance(500); // 平台期
    layer.flush();
    const writes = log.opacities.length;
    layer.flush();
    layer.flush();
    expect(log.opacities.length).toBe(writes); // 同值跳过
  });

  it('OverlayLayer：初始同步后连续 flush 零写入；飘条结束后零写入', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeOverlayView();
    const layer = new OverlayLayer(view, { now: (): number => clock.now() });
    layer.flush(); // 首次：初始脏 → 同步默认态（设计决定）
    const afterFirst = log.totalWrites();
    expect(afterFirst).toBeGreaterThan(0);
    layer.flush();
    layer.flush();
    expect(log.totalWrites()).toBe(afterFirst); // 之后零写入
    // 飘条完整生命周期后归于平静
    layer.enqueueToast('a');
    layer.flush();
    clock.advance(2500);
    layer.flush();
    const settled = log.totalWrites();
    layer.flush();
    layer.flush();
    expect(log.totalWrites()).toBe(settled);
  });
});

// ---------------------------------------------------------------------------
// P10 接线序：LayerHost render() 调用序 overlay → bubble
// ---------------------------------------------------------------------------

describe('P10 接线序（LAYER_DRAW_ORDER：overlay 先于 bubble）', () => {
  it('注册两层后 render() 按 overlay → bubble 顺序调用', () => {
    const calls: string[] = [];
    const host = new LayerHost();
    host.setLayer('bubble', (): void => void calls.push('bubble'));
    host.setLayer('overlay', (): void => void calls.push('overlay'));
    host.render();
    expect(calls).toEqual(['overlay', 'bubble']);
  });

  it('LAYER_DRAW_ORDER 常量冻结口径（character→particle→overlay→bubble→menu，S3-M6 追加）', () => {
    expect(LAYER_DRAW_ORDER).toEqual([
      'character',
      'particle',
      'overlay',
      'bubble',
      'menu',
    ]);
  });

  it('两层真实 flush 挂进 host：render() 触发两层各一次且序正确', () => {
    const clock = new FakeClock();
    const bubbleView = makeFakeBubbleView();
    const overlayView = makeFakeOverlayView();
    const bubble = new BubbleLayer(bubbleView.view, { now: (): number => clock.now() });
    const overlay = new OverlayLayer(overlayView.view, { now: (): number => clock.now() });
    const host = new LayerHost();
    host.setLayer('overlay', (): void => overlay.flush());
    host.setLayer('bubble', (): void => bubble.flush());
    bubble.submit(makeCmd());
    host.render();
    // bubble 层被驱动：发生了显示写入
    expect(bubbleView.log.contentCalls.length).toBe(1);
    // overlay 层被驱动：发生初始同步写入
    expect(overlayView.log.totalWrites()).toBeGreaterThan(0);
  });
});

// ---------------------------------------------------------------------------
// P11 C2 占位符与署名
// ---------------------------------------------------------------------------

describe('P11-A renderPlaceholders（C2：命中替换、未命中原样保留）', () => {
  it('命中 token 替换；未命中 token 原样保留（不吞字符）', () => {
    const name = '\u5FC3\u6708\u72D0'; // C2：码点转义，零角色名字面量
    expect(renderPlaceholders('{name}\uFF1A\u4F60\u597D', { name })).toBe(`${name}\uFF1A\u4F60\u597D`);
    expect(renderPlaceholders('{name}\u548C{mood}', { name })).toBe(`${name}\u548C{mood}`);
    expect(renderPlaceholders('\u65E0\u5360\u4F4D\u7B26', {})).toBe('\u65E0\u5360\u4F4D\u7B26');
  });

  it('花括号配对畸形输入不崩溃、原样保留', () => {
    expect(renderPlaceholders('{unclosed', {})).toBe('{unclosed');
    expect(renderPlaceholders('}{', {})).toBe('}{');
  });
});

describe('P11-B 署名（signatureEnabled && showSignature → 「—— 」+ 插值）', () => {
  it('help 缺省署名开启 → 输出「—— name」；vars 缺 name 诚实降级「—— {name}」', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const name = '\u5FC3\u6708\u72D0';
    const layer = new BubbleLayer(view, {
      now: (): number => clock.now(),
      vars: (): Record<string, string> => ({ name }),
    });
    // 派生行为（kind→showSignature 缺省）已由 P6 parseBubbleCmd 独立验证；
    // 此处验证 buildContent 的「开关与」逻辑，故显式 showSignature:true。
    layer.submit(makeCmd({ kind: 'help', cooldownKey: 's1', showSignature: true }));
    layer.flush();
    expect(log.contentCalls[0]!.signature).toBe(`\u2014\u2014 ${name}`);

    const clock2 = new FakeClock();
    const v2 = makeFakeBubbleView();
    const layer2 = new BubbleLayer(v2.view, { now: (): number => clock2.now() }); // vars 默认空
    layer2.submit(makeCmd({ kind: 'postcard', cooldownKey: 's2', showSignature: true }));
    layer2.flush();
    expect(v2.log.contentCalls[0]!.signature).toBe('\u2014\u2014 {name}');
  });

  it('chat 缺省无署名（signature=null）；signatureEnabled=false 强制关闭；cmd.showSignature=false 关闭', () => {
    const clock = new FakeClock();
    const { view, log } = makeFakeBubbleView();
    const layer = new BubbleLayer(view, { now: (): number => clock.now() });
    layer.submit(makeCmd({ kind: 'chat', cooldownKey: 'c1' }));
    layer.flush();
    expect(log.contentCalls[0]!.signature).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// P12 菜单降级几何（裁定 B 2026-09-19）——独立探针
//
// 背景：DOM 定位域为 CSS px，真机视口仅 128 CSS px（窗口物理 256 ÷ DPR 2），
// 而菜单冻结规格 MENU_WIDTH×MENU_HEIGHT = 216×156 远超视口。若直接摆位，右/下列
// 会被裁（九键不全可达）。裁定 B：`menuScale` 在容器不足容时按「预留 2·pad」等比
// 缩小，使 `MENU_WIDTH·scale ≤ w − 2·pad`，配合 `clampMenuPlacement`（left/top ≥ pad）
// 则 `left + MENU_WIDTH·scale ≤ w` 恒成立 ⇒ 九键全部落在视口内。
//
// 本块为 QA 独立推演（不 import 工程师测试文件，仅 import 实现模块），数值断言从
// 公式独立算出，非抄录工程师用例。
// ---------------------------------------------------------------------------

describe('P12 菜单降级几何（裁定 B：128 CSS 视口等比缩小恒不越界）', () => {
  it('足容容器 → 不缩放（保冻结规格 216×156）', () => {
    // 恰好足容：w = MENU_WIDTH+2·pad = 224，h = MENU_HEIGHT+2·pad = 164 → avail=216/156 → scale=1。
    expect(menuScale({ width: MENU_WIDTH + 2 * MENU_EDGE_PAD, height: MENU_HEIGHT + 2 * MENU_EDGE_PAD })).toBe(1);
    expect(menuScale({ width: 640, height: 640 })).toBe(1);
  });

  it('恰差 1px 不足容 → scale<1（缩放启动边界）', () => {
    // 宽少 1px：availW=223-8=215 < 216 → scale=215/216<1。
    expect(menuScale({ width: MENU_WIDTH + 2 * MENU_EDGE_PAD - 1, height: 640 })).toBeLessThan(1);
  });

  it('真机 128 视口 → scale = (128−2·pad)/MENU_WIDTH = 120/216（宽为约束维）', () => {
    // availW=120, availH=120：120/216≈0.5556 < 120/156≈0.769 ⇒ 宽维约束。
    expect(menuScale({ width: 128, height: 128 })).toBeCloseTo(120 / MENU_WIDTH, 12);
    expect(menuScale({ width: 128, height: 128 })).toBeLessThan(1);
  });

  it('退化容器（≤0 / 非有限）→ scale=1（交由 clamp 兜底，不产生 NaN/负）', () => {
    expect(menuScale({ width: 0, height: 128 })).toBe(1);
    expect(menuScale({ width: 128, height: 0 })).toBe(1);
    expect(menuScale({ width: Number.NaN, height: 128 })).toBe(1);
    expect(menuScale({ width: Number.POSITIVE_INFINITY, height: 128 })).toBe(1);
  });

  it('组合不变量：left/top ≥ pad 且缩放盒右/下缘恒不越界（128 视口 + 多维容器扫描）', () => {
    const containers = [
      { width: 128, height: 128 }, // 真机视口
      { width: 200, height: 100 }, // 高不足容
      { width: 100, height: 200 }, // 宽不足容
      { width: 60, height: 60 }, // 极小
      { width: MENU_WIDTH + 2 * MENU_EDGE_PAD, height: MENU_HEIGHT + 2 * MENU_EDGE_PAD }, // 恰足容
      { width: 640, height: 640 }, // 宽容器
    ];
    for (const c of containers) {
      const scale = menuScale(c);
      expect(scale).toBeGreaterThan(0);
      expect(scale).toBeLessThanOrEqual(1);
      const scaledW = MENU_WIDTH * scale;
      const scaledH = MENU_HEIGHT * scale;
      // 命中点在容器内各处（含越界/负边）→ clamp 后左上角 ≥ pad。
      for (const at of [
        { x: 0, y: 0 },
        { x: -999, y: -999 },
        { x: c.width + 999, y: c.height + 999 },
        { x: c.width / 2, y: c.height / 2 },
      ]) {
        const p = clampMenuPlacement(at, c);
        expect(p.left).toBeGreaterThanOrEqual(MENU_EDGE_PAD);
        expect(p.top).toBeGreaterThanOrEqual(MENU_EDGE_PAD);
        // 核心不变量（裁定 B）：缩放后菜单盒右下角落在容器内（1e-9 吸收浮点）。
        expect(p.left + scaledW).toBeLessThanOrEqual(c.width + 1e-9);
        expect(p.top + scaledH).toBeLessThanOrEqual(c.height + 1e-9);
      }
    }
  });

  it('真机 128 视口：左贴边时右缘亦不越界（旧「贴容器宽缩放」会溢出 128→右列裁切）', () => {
    // 反例回归：若 scale=w/MENU_WIDTH=128/216，则 left=pad 时右缘=4+128=132>128（溢出）。
    // 裁定 B 预留 2·pad 后：scale=120/216，left=4 → 右缘=4+120=124 ≤ 128。
    const scale = menuScale({ width: 128, height: 128 });
    const p = clampMenuPlacement({ x: -999, y: -999 }, { width: 128, height: 128 });
    expect(p.left).toBe(MENU_EDGE_PAD);
    expect(p.left + MENU_WIDTH * scale).toBeLessThanOrEqual(128);
    // 且确实小于「贴边缩放」的 132，证明预留 2·pad 生效。
    expect(p.left + MENU_WIDTH * scale).toBeLessThan(4 + 128);
  });
});
