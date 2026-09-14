/**
 * 图层容器与绘制顺序编排（`02 §3` LayerHost / §6.5 渲染时序）。
 *
 * 绘制顺序（`02 §6.5`「合成绘制 角色层 粒子层 叠加层 气泡层」）：
 *   character（角色）→ particle（粒子）→ overlay（叠加）→ bubble（气泡）。
 *
 * S3-M6 追加（主理人裁定「菜单置于 bubble 层之上或最高 z 序」）：末位追加
 * `menu`（右键菜单）——绘制序 = 合成序的追加尾，不改变既有四层冻结口径；
 * DOM z-index（pet.css）与之一致（menu 最高）。
 *
 * S2-M1 边界：character 层（`FrameRenderer`）先实装；S3-M5 接入 overlay/bubble、
 * S3-M6 接入 particle/menu，未注册的层在 render 中跳过，不引入空绘制开销。
 */

/** 图层名（`02 §3` / §6.5；S3-M6 追加 menu；postcardWidget 外出挂件在 T-24 段并入 overlay 前评估）。 */
export type LayerName = 'character' | 'particle' | 'overlay' | 'bubble' | 'menu';

/** 绘制顺序（§6.5 冻结口径 + S3-M6 追加 menu 尾；顺序数组即绘制序）。 */
export const LAYER_DRAW_ORDER: readonly LayerName[] = [
  'character',
  'particle',
  'overlay',
  'bubble',
  'menu',
];

/** 单层同步绘制回调（无返回值；异步准备由各层自行完成后再注册/触发 render）。 */
export type LayerDrawable = () => void;

/**
 * 图层编排器：按 [`LAYER_DRAW_ORDER`] 顺序调用已注册层的同步绘制回调。
 *
 * 所有绘制回调必须是**同步**的——帧位图等异步资源的就绪由各层内部管理
 * （如 `FrameRenderer` 在 `pet://frame` 载荷解析完成后更新「最近就绪帧」），
 * `render` 只做按序合成，保证无硬切与顺序稳定。
 */
export class LayerHost {
  private readonly layers = new Map<LayerName, LayerDrawable>();

  /**
   * 注册 / 注销一层。
   *
   * @param name 层名（必须在 [`LAYER_DRAW_ORDER`] 内，顺序由常量数组决定）
   * @param draw 绘制回调；传 `null` 注销该层
   */
  setLayer(name: LayerName, draw: LayerDrawable | null): void {
    if (draw === null) {
      this.layers.delete(name);
      return;
    }
    this.layers.set(name, draw);
  }

  /** 按绘制顺序合成一帧（未注册的层跳过）。 */
  render(): void {
    for (const name of LAYER_DRAW_ORDER) {
      const draw = this.layers.get(name);
      if (draw !== undefined) {
        draw();
      }
    }
  }
}
