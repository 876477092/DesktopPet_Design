/**
 * 右键菜单层状态机（S3-M6，T-10 段 · 下；`02 §3` MenuLayer / `01 §8.2`）。
 *
 * 职责：消费 `pet://menu` 解析后的 `MenuCmdV1`（右键单击命中，Up 触发）——
 * 打开（摆位钳制）→ 经极窄端口 `MenuView` 落 DOM；点击菜单项 / 空白处关闭。
 * 菜单项规格在 `menuLogic.ts`（纯函数直测）；本层只做开合状态编排。
 *
 * ⚠️ **本层禁 `setTimeout`**：关闭路径由 `flush()` 被 `LayerHost.render()`
 * （Rust 帧 tick）驱动（主理人裁定：关闭路径也要 flush 幂等）。无独立时钟（C3）。
 *
 * 惰性 flush（S3-M5 范式延续）：`open` / `close` 只置脏；`show` 在打开期只写
 * 一次（内容与摆位不随帧变化），`hide` 恰好写一次后回「无脏早退」态。
 *
 * 命令出口：菜单项点击经 `choose()` → 关闭（惰性）+ `onCommand` 回调交出
 * （`hide` → `menu_command` IPC，收口 `dp-app/src/commands.rs`；灰化项不可达
 * 不产命令）。
 */

import type { MenuItemId, MenuView } from './layerPorts';
import { buildMenuItems, clampMenuPlacement } from './menuLogic';

/** 构造选项（全部可选；测试注入 Fake view / 命令回调）。 */
export interface MenuLayerOptions {
  /** 菜单项点击回调（仅可用项可达；`hide` 由装配层转 `menu_command` IPC）。 */
  onCommand?: (id: MenuItemId) => void;
}

/**
 * 右键菜单层（开合状态 + 惰性 flush）。
 *
 * @param view 极窄端口（真实 `DomMenuView` / 测试 `FakeMenuView`）
 * @param opts 构造选项（见 [`MenuLayerOptions`]）
 */
export class MenuLayer {
  private readonly onCommand: ((id: MenuItemId) => void) | undefined;

  /** 是否处于打开态（needShow/needHide 为待写 view 的脏标记）。 */
  private opened = false;
  private needShow = false;
  private needHide = false;
  /** 待摆位锚点（窗口内 CSS px；open 时暂存，flush 时钳制）。 */
  private pendingAt: { readonly x: number; readonly y: number } = { x: 0, y: 0 };

  constructor(
    private readonly view: MenuView,
    opts: MenuLayerOptions = {},
  ) {
    this.onCommand = opts.onCommand;
  }

  /**
   * 打开菜单（`pet://menu` 命令入口；`at` 为窗口内 CSS px 命中点）。
   * 打开期再 open：刷新锚点并重新置 needShow → 下次 flush 重摆位到新命中点。
   */
  open(at: { readonly x: number; readonly y: number }): void {
    this.pendingAt = at;
    this.opened = true;
    this.needShow = true;
    this.needHide = false;
  }

  /**
   * 关闭菜单（点击空白处 / 菜单项）。关闭路径同样惰性：只置脏，DOM 写入
   * 延后到 `flush()`；未打开且无待写时幂等（零 view 写入）。
   */
  close(): void {
    if (!this.opened && !this.needShow && !this.needHide) {
      return;
    }
    this.opened = false;
    this.needShow = false;
    this.needHide = true;
  }

  /**
   * 菜单项点击（由 `DomMenuView` 的按钮回调交出）：先关闭（惰性 flush）再交
   * 命令回调——保证命令执行时菜单已进入关闭待写态。
   */
  choose(id: MenuItemId): void {
    this.close();
    this.onCommand?.(id);
  }

  /**
   * 惰性 flush（`LayerHost` draw 回调；每帧调用须廉价且幂等）。
   *
   * - 打开期：needShow 时按序 `show(items, clampMenuPlacement(...))` 后清脏
   *   （打开期内容不变，后续 tick 零 view 写入）；
   * - 关闭期：needHide 恰好写一次 `hide()`；
   * - 无脏 → 立即返回（零 view 写入）。
   */
  flush(): void {
    if (this.opened) {
      if (this.needShow) {
        const size = this.view.containerSize();
        this.view.show(buildMenuItems(), clampMenuPlacement(this.pendingAt, size));
        this.needShow = false;
      }
      return;
    }
    if (this.needHide) {
      this.view.hide();
      this.needHide = false;
    }
  }

  /** 是否处于打开态（诊断视图）。 */
  isOpen(): boolean {
    return this.opened;
  }
}
