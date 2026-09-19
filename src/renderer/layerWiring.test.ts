/**
 * 两层接入 `LayerHost` 的接线冒烟（S3-M5，T-04）。
 *
 * 覆盖（设计 §3 Q7）：`render()` 按绘制顺序调用两层 flush（overlay → bubble，
 * 与 `LAYER_DRAW_ORDER` 一致）；未注册层跳过；注销后不再调用。
 * 用 Fake 源记录调用序列——**不碰 DOM**。
 */
import { describe, expect, it } from 'vitest';

import type { BubbleCmdV1 } from '../shared/ipc';
import { BubbleLayer } from './BubbleLayer';
import { LAYER_DRAW_ORDER, LayerHost } from './LayerHost';
import { OverlayLayer } from './OverlayLayer';
import type { BubbleContent, BubblePlacement, BubbleView, OverlayView } from './layerPorts';

/** 气泡命令快捷工厂。 */
function bubbleCmd(overrides: Partial<BubbleCmdV1> = {}): BubbleCmdV1 {
  return {
    version: 1,
    text: 'hi',
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

/** 假气泡视图：首次内容写入时向共享日志压入层名（调用序列断言用）。 */
class RecordingBubbleView implements BubbleView {
  constructor(private readonly log: string[]) {}
  setContent(content: BubbleContent): void {
    void content;
    this.log.push('bubble');
  }
  measure(): { width: number; height: number } {
    return { width: 100, height: 40 };
  }
  place(placement: BubblePlacement): void {
    void placement;
  }
  setOpacity(alpha: number): void {
    void alpha;
  }
  setVisible(visible: boolean): void {
    void visible;
  }
  containerWidth(): number {
    // 算法测试输入、非真机视口（真机视口 = 128 CSS px；此值仅驱动摆位分支）。
    return 256;
  }
}

/** 假叠加视图：首次进度环写入时向共享日志压入层名。 */
class RecordingOverlayView implements OverlayView {
  constructor(private readonly log: string[]) {}
  setCoaxProgress(value: number | null): void {
    void value;
    this.log.push('overlay');
  }
  setAngerLevel(level: number): void {
    void level;
  }
  setSleepLevel(level: 0 | 1 | 2): void {
    void level;
  }
  setToast(text: string | null): void {
    void text;
  }
  setToastOpacity(alpha: number): void {
    void alpha;
  }
}

describe('LayerHost 两层接线（draw = 惰性 flush）', () => {
  it('render() 调用顺序为 overlay → bubble（与 LAYER_DRAW_ORDER 一致）', () => {
    const log: string[] = [];
    const now = (): number => 0;
    const bubble = new BubbleLayer(new RecordingBubbleView(log), { now });
    const overlay = new OverlayLayer(new RecordingOverlayView(log), { now });

    // 各置一次脏：气泡有内容待写、叠加层有进度待写 → 两层 flush 都会产生写入。
    bubble.submit(bubbleCmd());
    overlay.setCoaxProgress(0.5);

    const host = new LayerHost();
    host.setLayer('overlay', () => overlay.flush());
    host.setLayer('bubble', () => bubble.flush());
    host.render();

    expect(log).toContain('overlay');
    expect(log).toContain('bubble');
    expect(log.indexOf('overlay')).toBeLessThan(log.indexOf('bubble'));
  });

  it('LAYER_DRAW_ORDER 冻结口径：character → particle → overlay → bubble → menu（S3-M6 追加 menu 尾位）', () => {
    expect([...LAYER_DRAW_ORDER]).toEqual([
      'character',
      'particle',
      'overlay',
      'bubble',
      'menu',
    ]);
  });

  it('未注册的层跳过；setLayer(name, null) 注销后不再调用', () => {
    const log: string[] = [];
    const now = (): number => 0;
    const bubble = new BubbleLayer(new RecordingBubbleView(log), { now });
    bubble.submit(bubbleCmd());

    const host = new LayerHost();
    host.setLayer('bubble', () => bubble.flush());
    host.render(); // overlay 未注册 → 跳过。
    expect(log).toEqual(['bubble']);

    host.setLayer('bubble', null);
    bubble.submit(bubbleCmd({ cooldownKey: 'k2' })); // 再置脏也不再有层消费。
    host.render();
    expect(log).toEqual(['bubble']); // 计数不变（注销生效）。
  });
});
