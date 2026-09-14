/**
 * 右键菜单层测试（S3-M6，T-10 段 · 下）。
 *
 * 覆盖：纯逻辑（九项冻结序 / 灰化口径 / 摆位钳制）+ 层编排（惰性 flush 的
 * show/hide 恰写一次、幂等关闭、choose 命令出口）。**零 DOM**——视图注入
 * Fake 记录调用；零时钟零 setTimeout（C3）。
 */
import { describe, expect, it } from 'vitest';

import { MENU_HEIGHT, MENU_WIDTH, type MenuItemId, type MenuItemSpec, type MenuPlacement, type MenuView } from './layerPorts';
import { MenuLayer } from './MenuLayer';
import { MENU_ITEM_ORDER, buildMenuItems, clampMenuPlacement } from './menuLogic';

/** 假菜单视图：记录 show/hide 调用序与参数（不碰 DOM）。 */
class FakeMenuView implements MenuView {
  readonly shown: Array<{ items: MenuItemSpec[]; at: MenuPlacement }> = [];
  readonly hides: number[] = [];
  /** 模拟容器内容盒尺寸（默认 256×256 宠物逻辑容器）。 */
  constructor(public width = 256, public height = 256) {}
  show(items: readonly MenuItemSpec[], at: MenuPlacement): void {
    this.shown.push({ items: [...items], at: { ...at } });
  }
  hide(): void {
    this.hides.push(this.shown.length);
  }
  containerSize(): { width: number; height: number } {
    return { width: this.width, height: this.height };
  }
}

describe('menuLogic 纯逻辑（01 §8.2 冻结口径）', () => {
  it('九项 id 与 3×3 行主序逐项对齐（勿重排）', () => {
    const items = buildMenuItems();
    expect(items.map((it) => it.id)).toEqual([
      'stroke',
      'feed',
      'bath',
      'dispatch',
      'shop',
      'attrs',
      'photo',
      'settings',
      'hide',
    ]);
    expect(items.map((it) => it.id)).toEqual([...MENU_ITEM_ORDER]);
    expect(items).toHaveLength(9);
  });

  it('仅 hide 可实接；其余八项灰化且带条件文案（不写死版本号）', () => {
    const items = buildMenuItems();
    for (const it of items) {
      if (it.id === 'hide') {
        expect(it.enabled).toBe(true);
        expect(it.reason).toBeNull();
      } else {
        expect(it.enabled).toBe(false);
        expect(it.reason).not.toBeNull();
        expect(it.reason?.length ?? 0).toBeGreaterThan(0);
        // 条件文案口径：不出现版本号样式（v1/V1/1.x）。
        expect(it.reason).not.toMatch(/v1|V1|\d+\.\d+/);
      }
    }
  });

  it('九项文案与图样冻结（label + emoji）', () => {
    const byId = new Map(buildMenuItems().map((it) => [it.id, it]));
    expect(byId.get('stroke')?.label).toBe('抚摸');
    expect(byId.get('feed')?.label).toBe('喂食');
    expect(byId.get('bath')?.label).toBe('洗澡');
    expect(byId.get('dispatch')?.label).toBe('派遣');
    expect(byId.get('shop')?.label).toBe('商城');
    expect(byId.get('attrs')?.label).toBe('属性');
    expect(byId.get('photo')?.label).toBe('拍照');
    expect(byId.get('settings')?.label).toBe('设置');
    expect(byId.get('hide')?.label).toBe('隐藏');
    // emoji 冻结：🤚🍙🛁💼🏪📊📷⚙👻
    expect(byId.get('stroke')?.emoji).toBe('\u{1F91A}');
    expect(byId.get('feed')?.emoji).toBe('\u{1F359}');
    expect(byId.get('bath')?.emoji).toBe('\u{1F6C1}');
    expect(byId.get('dispatch')?.emoji).toBe('\u{1F4BC}');
    expect(byId.get('shop')?.emoji).toBe('\u{1F3EA}');
    expect(byId.get('attrs')?.emoji).toBe('\u{1F4CA}');
    expect(byId.get('photo')?.emoji).toBe('\u{1F4F7}');
    expect(byId.get('settings')?.emoji).toBe('\u{2699}');
    expect(byId.get('hide')?.emoji).toBe('\u{1F47B}');
  });

  it('clampMenuPlacement：命中点优先；越界向内回收；容器小于菜单钳到安全间隙', () => {
    const big = { width: 400, height: 400 }; // 400 足容 216×156
    // 普通命中点：直接作为左上角。
    expect(clampMenuPlacement({ x: 100, y: 80 }, big)).toEqual({ left: 100, top: 80 });
    // 右/下越界：回收使菜单不越容器。
    expect(clampMenuPlacement({ x: 300, y: 300 }, big)).toEqual({
      left: 400 - MENU_WIDTH - 4,
      top: 400 - MENU_HEIGHT - 4,
    });
    // 左/上越界（负数）：钳到安全间隙。
    expect(clampMenuPlacement({ x: -10, y: -10 }, big)).toEqual({ left: 4, top: 4 });
    // 容器小于菜单：钳到安全间隙（退化安全，不越界不 panic）。
    expect(clampMenuPlacement({ x: 10, y: 10 }, { width: 10, height: 10 })).toEqual({
      left: 4,
      top: 4,
    });
  });
});

describe('MenuLayer 层编排（惰性 flush：show/hide 恰写一次）', () => {
  it('open → flush 恰写一次 show（九项 + 钳制摆位）；后续 tick 不重写', () => {
    const view = new FakeMenuView(400, 400); // 足容菜单，{100,80} 不触发钳制
    const layer = new MenuLayer(view);
    expect(layer.isOpen()).toBe(false);

    layer.open({ x: 100, y: 80 });
    layer.flush();
    expect(view.shown).toHaveLength(1);
    expect(view.shown[0]?.items).toHaveLength(9);
    expect(view.shown[0]?.at).toEqual({ left: 100, top: 80 });

    layer.flush(); // 打开期内容不变：零 view 写入
    layer.flush();
    expect(view.shown).toHaveLength(1);
    expect(layer.isOpen()).toBe(true);
  });

  it('close → flush 恰写一次 hide；再 flush 幂等；未打开时 close 零 view 写入', () => {
    const view = new FakeMenuView();
    const layer = new MenuLayer(view);

    layer.close(); // 从未打开：幂等，零写入
    layer.flush();
    expect(view.hides).toHaveLength(0);

    layer.open({ x: 10, y: 10 });
    layer.close();
    layer.flush();
    expect(view.hides).toHaveLength(1);
    expect(layer.isOpen()).toBe(false);

    layer.flush(); // 关闭完成后回早退态
    layer.flush();
    expect(view.hides).toHaveLength(1);
  });

  it('打开期再 open：刷新锚点，flush 重写 show（重摆位到新命中点）', () => {
    const view = new FakeMenuView(500, 500); // 足容菜单，{200,200} 不触发钳制
    const layer = new MenuLayer(view);
    layer.open({ x: 10, y: 10 });
    layer.flush();
    expect(view.shown).toHaveLength(1);
    layer.open({ x: 200, y: 200 }); // 已打开再 open → 重摆位
    layer.flush();
    expect(view.shown).toHaveLength(2);
    expect(view.shown[1]?.at).toEqual({ left: 200, top: 200 });
    // 重摆位后 close → flush 仍只写一次 hide。
    layer.close();
    layer.flush();
    expect(view.hides).toHaveLength(1);
  });

  it('打开期未 flush 前 close：show 与 hide 都不落（view 零写入，符合惰性语义）', () => {
    const view = new FakeMenuView();
    const layer = new MenuLayer(view);
    layer.open({ x: 5, y: 5 });
    layer.close(); // needShow 清、needHide 置
    layer.flush();
    expect(view.shown).toHaveLength(0);
    expect(view.hides).toHaveLength(1);
  });

  it('choose(hide)：先关闭（惰性）再交命令回调；灰化项在 view 层不可达（层不校验）', () => {
    const view = new FakeMenuView();
    const received: MenuItemId[] = [];
    const layer = new MenuLayer(view, { onCommand: (id): void => void received.push(id) });
    layer.open({ x: 20, y: 20 });
    layer.flush();
    expect(view.shown).toHaveLength(1);

    layer.choose('hide');
    expect(received).toEqual(['hide']);
    layer.flush(); // choose 内部已 close → flush 落 hide
    expect(view.hides).toHaveLength(1);

    layer.choose('hide'); // 已关闭态 choose：close 幂等 + 回调仍交出（防御性）
    expect(received).toEqual(['hide', 'hide']);
    layer.flush();
    expect(view.hides).toHaveLength(1);
  });

  it('flush 摆位经 clampMenuPlacement 钳制（贴边命中点不越容器）', () => {
    // 容器 200×100：宽高均不足容菜单 → 左上均钳到安全间隙。
    const view = new FakeMenuView(200, 100);
    const layer = new MenuLayer(view);
    layer.open({ x: 150, y: 150 });
    layer.flush();
    expect(view.shown[0]?.at).toEqual({ left: 4, top: 4 });
  });
});
