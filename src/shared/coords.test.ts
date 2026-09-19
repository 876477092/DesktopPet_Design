/**
 * 画布尺寸换算单测（`02 §7.2` / `02 §4.4`）。
 *
 * 覆盖两大不变量（S10 真机回归 Bug 的回归护栏）：
 *   G1 **CSS 呈现边长恒 == 逻辑边长**（宠物 = 128 CSS px）——绝不乘「位图密度倍数」。
 *      历史 Bug：误把 ×2 乘进 `canvas.style.width` → 256 CSS px，而窗口仅 256 物理 px
 *      （DPR=2 时视口 = 128 CSS px）→ 画布溢出视口 2 倍，宠物只露左上 1/4（狐被裁到底右角）。
 *   G2 **位图边长 == CSS 边长 × DPR**（物理像素充足，高 DPR 不糊）。
 *
 * **零真实 DOM**（与既有测试同纪律：视图/元素以最小 Fake 注入，不依赖 jsdom）——
 * `applyCanvasSizing` / `resizeCanvasToWindow` 只读写 `canvas.style.width/height`
 * （CSS 字符串）与 `canvas.width/height`（位图数字），故 FakeCanvas 即可精确取证。
 */
import { afterEach, describe, expect, it } from 'vitest';

import { applyCanvasSizing, resizeCanvasToWindow } from './coords';

/** 宠物逻辑尺寸（`02 §4.4`：128×128 逻辑，导出 2x）——与 `src/main.ts` 同值。 */
const LOGICAL_SIZE = 128;
/** 位图密度倍数（`src/main.ts` `BITMAP_SCALE`）——**仅进位图，绝不进 CSS**。 */
const BITMAP_SCALE = 2;

/**
 * 最小画布替身：只需 `style.width/height`（CSS 串）与 `width/height`（位图数字）。
 * 用 `Object.defineProperty` 让 `width/height` 语义与真实 canvas 一致（数字属性）。
 */
class FakeCanvas {
  style: { width: string; height: string } = { width: '', height: '' };
  width = 0;
  height = 0;
}

/** 原始 DPR（用例可能改写；afterEach 恢复，避免污染其它文件）。 */
const ORIGINAL_DPR = globalThis.window?.devicePixelRatio;

/** 设定 `window.devicePixelRatio`（node 环境下 `window` 可能不存在，惰性建对象）。 */
function setDpr(dpr: number): void {
  const w = (globalThis as { window?: { devicePixelRatio?: number } }).window ?? {};
  (globalThis as { window?: unknown }).window = w;
  w.devicePixelRatio = dpr;
}

afterEach(() => {
  if (ORIGINAL_DPR === undefined) {
    const w = (globalThis as { window?: { devicePixelRatio?: number } }).window;
    if (w !== undefined) {
      delete w.devicePixelRatio;
    }
  } else if (globalThis.window !== undefined) {
    globalThis.window.devicePixelRatio = ORIGINAL_DPR;
  }
});

describe('resizeCanvasToWindow（位图边长 = 逻辑 × DPR）', () => {
  it('DPR=2：128 逻辑 → 256 位图', () => {
    setDpr(2);
    const canvas = new FakeCanvas();
    const physical = resizeCanvasToWindow(canvas as unknown as HTMLCanvasElement, LOGICAL_SIZE);
    expect(physical).toBe(256);
    expect(canvas.width).toBe(256);
    expect(canvas.height).toBe(256);
  });

  it('DPR=1：128 逻辑 → 128 位图', () => {
    setDpr(1);
    const canvas = new FakeCanvas();
    const physical = resizeCanvasToWindow(canvas as unknown as HTMLCanvasElement, LOGICAL_SIZE);
    expect(physical).toBe(128);
    expect(canvas.width).toBe(128);
  });

  it('DPR=1.5：128 逻辑 → 192 位图（四舍五入）', () => {
    setDpr(1.5);
    const canvas = new FakeCanvas();
    const physical = resizeCanvasToWindow(canvas as unknown as HTMLCanvasElement, LOGICAL_SIZE);
    expect(physical).toBe(192);
  });

  it('非法 DPR（0 / 负 / NaN）回退 1，不产生 0 尺寸位图', () => {
    for (const bad of [0, -3, Number.NaN]) {
      setDpr(bad);
      const canvas = new FakeCanvas();
      const physical = resizeCanvasToWindow(canvas as unknown as HTMLCanvasElement, LOGICAL_SIZE);
      expect(physical).toBe(128);
    }
  });
});

describe('applyCanvasSizing（CSS 呈现尺寸与位图尺寸解耦 —— 回归护栏）', () => {
  it('G1：CSS 边长 == 逻辑边长（128），**不乘** BITMAP_SCALE', () => {
    setDpr(2);
    const canvas = new FakeCanvas();
    applyCanvasSizing(
      canvas as unknown as HTMLCanvasElement,
      LOGICAL_SIZE,
      LOGICAL_SIZE * BITMAP_SCALE,
    );
    expect(canvas.style.width).toBe('128px');
    expect(canvas.style.height).toBe('128px');
    // 显式盯死历史 Bug：绝不为 256（128 × BITMAP_SCALE）。
    expect(canvas.style.width).not.toBe(`${LOGICAL_SIZE * BITMAP_SCALE}px`);
  });

  it('G2：位图边长 == 位图逻辑输入 × DPR（256 × 2 = 512 物理 px）', () => {
    setDpr(2);
    const canvas = new FakeCanvas();
    const physical = applyCanvasSizing(
      canvas as unknown as HTMLCanvasElement,
      LOGICAL_SIZE,
      LOGICAL_SIZE * BITMAP_SCALE,
    );
    expect(physical).toBe(512);
    // 位图边长 == 位图逻辑输入 × DPR；CSS 仍恒 128（两条轴已解耦）。
    const cssPx = Number.parseInt(canvas.style.width, 10);
    expect(cssPx).toBe(LOGICAL_SIZE);
    expect(canvas.width).toBe(LOGICAL_SIZE * BITMAP_SCALE * 2);
    expect(canvas.height).toBe(LOGICAL_SIZE * BITMAP_SCALE * 2);
  });

  it('G2 泛化：DPR=1 时位图 == 位图逻辑输入（256），CSS 仍 128', () => {
    setDpr(1);
    const canvas = new FakeCanvas();
    const physical = applyCanvasSizing(
      canvas as unknown as HTMLCanvasElement,
      LOGICAL_SIZE,
      LOGICAL_SIZE * BITMAP_SCALE,
    );
    expect(canvas.style.width).toBe('128px');
    expect(physical).toBe(256);
  });

  it('返回值为位图边长（供 stage.resize 复用），与 canvas.width 一致', () => {
    setDpr(2);
    const canvas = new FakeCanvas();
    const physical = applyCanvasSizing(
      canvas as unknown as HTMLCanvasElement,
      LOGICAL_SIZE,
      LOGICAL_SIZE * BITMAP_SCALE,
    );
    expect(physical).toBe(canvas.width);
  });

  it('CSS 呈现边长与位图输入解耦：即便位图输入翻倍，CSS 恒 == cssPx', () => {
    setDpr(1);
    const canvas = new FakeCanvas();
    applyCanvasSizing(canvas as unknown as HTMLCanvasElement, 128, 512);
    expect(canvas.style.width).toBe('128px');
    expect(canvas.width).toBe(512);
  });
});
