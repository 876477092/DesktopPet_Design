/**
 * 气泡层 / 叠加层的 DOM 适配器（S3-M5，T-10 段 · 上）。
 *
 * 职责（设计 §3 Q5）：**唯一触碰 `document` 的代码**——把 `layerPorts.ts` 的端口
 * `BubbleView` / `OverlayView` 实现为真实 DOM 操作（`DomBubbleView` / `DomOverlayView`）。
 * 仅在 `main.ts` 装配期构造一次，**不进单测**（与 `WebGLStage` 不进 `FrameRenderer.test.ts`
 * 同处置）；所有几何 / 时序 / 优先级逻辑在 `bubbleLogic.ts` / `overlayLogic.ts`（纯函数直测）。
 *
 * 关键约束（设计 §4.6，易错点）：
 *   - `.bubble` 隐藏态**只用 `visibility:hidden; opacity:0`，禁 `display:none`**——
 *     否则 `getBoundingClientRect()` 全 0，`measure()` 退化导致摆位失效；
 *   - 根容器 `#pet-overlay-root`：`position:fixed; inset:0; pointer-events:none`
 *     （样式见 `src/styles/pet.css`）；子元素需交互（按钮点击、文字选中）时**显式**
 *     `pointer-events:auto`。
 *
 * 零网络（C9）/ 零新增事件（C8）：本文件不做任何 IPC / 网络调用。
 */

import type { BubbleActionV1 } from '../shared/ipc';
import type {
  BubbleContent,
  BubblePlacement,
  BubbleView,
  MenuItemId,
  MenuItemSpec,
  MenuPlacement,
  MenuView,
  OverlayView,
  ParticleRenderItem,
  ParticleView,
} from './layerPorts';
import { menuScale } from './menuLogic';

/** 高对比样式类。 */
const HC_CLASS = 'bubble--hc';
/** 三种摆位类（切换前先清理）。 */
const SIDE_CLASSES: readonly string[] = ['bubble--right', 'bubble--left', 'bubble--clamp'];

/** 钳制到 [0,1]（不透明度防御）。 */
function clamp01(a: number): number {
  if (!Number.isFinite(a)) {
    return 0;
  }
  return Math.min(1, Math.max(0, a));
}

/**
 * 气泡 DOM 适配器：承载 `.bubble` 子树（文案 / 署名 / 按钮 / 摆位 / 透明度）。
 *
 * @param root     承载容器 `#pet-overlay-root`
 * @param onAction 按钮点击回调（动作标识经此交出，不新增 `pet://` 事件，C8）
 */
export class DomBubbleView implements BubbleView {
  private readonly bubble: HTMLDivElement;
  private readonly textEl: HTMLSpanElement;
  private readonly signatureEl: HTMLSpanElement;
  private readonly actionsEl: HTMLDivElement;

  constructor(
    private readonly root: HTMLElement,
    private readonly onAction?: (action: BubbleActionV1) => void,
  ) {
    this.bubble = document.createElement('div');
    this.bubble.className = 'bubble';
    this.bubble.style.visibility = 'hidden';
    this.bubble.style.opacity = '0';

    this.textEl = document.createElement('span');
    this.textEl.className = 'bubble__text';

    this.signatureEl = document.createElement('span');
    this.signatureEl.className = 'bubble__signature';
    this.signatureEl.hidden = true;

    this.actionsEl = document.createElement('div');
    this.actionsEl.className = 'bubble__actions';
    this.actionsEl.hidden = true;

    this.bubble.append(this.textEl, this.signatureEl, this.actionsEl);
    this.root.appendChild(this.bubble);
  }

  /** 写入内容：文案 / 署名（null 隐藏）/ 高对比 / 按钮（重建并绑定 click）。 */
  setContent(c: BubbleContent): void {
    this.textEl.textContent = c.text;

    if (c.signature === null) {
      this.signatureEl.textContent = '';
      this.signatureEl.hidden = true;
    } else {
      this.signatureEl.textContent = c.signature;
      this.signatureEl.hidden = false;
    }

    this.bubble.classList.toggle(HC_CLASS, c.highContrast);

    this.actionsEl.replaceChildren();
    for (const action of c.actions) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'bubble__action';
      button.textContent = action.label;
      button.addEventListener('click', () => this.onAction?.(action));
      this.actionsEl.appendChild(button);
    }
    this.actionsEl.hidden = c.actions.length === 0;
  }

  /** 读取气泡自身尺寸（CSS px；隐藏态保持 visibility:hidden ⇒ 仍有布局盒）。 */
  measure(): { width: number; height: number } {
    const rect = this.bubble.getBoundingClientRect();
    return { width: rect.width, height: rect.height };
  }

  /** 摆位：写 left/top 并切换 `--right/--left/--clamp` 尾巴朝向类。 */
  place(p: BubblePlacement): void {
    this.bubble.style.left = `${p.left}px`;
    this.bubble.style.top = `${p.top}px`;
    for (const cls of SIDE_CLASSES) {
      this.bubble.classList.remove(cls);
    }
    this.bubble.classList.add(`bubble--${p.side}`);
  }

  /** 设置不透明度（钳 [0,1]）。 */
  setOpacity(a: number): void {
    this.bubble.style.opacity = String(clamp01(a));
  }

  /** 显示 / 隐藏（**用 visibility 切换，禁动 display**）。 */
  setVisible(v: boolean): void {
    this.bubble.style.visibility = v ? 'visible' : 'hidden';
  }

  /** 承载容器内容盒宽（CSS px）。 */
  containerWidth(): number {
    return this.root.clientWidth;
  }
}

/**
 * 叠加层 DOM 适配器：承载进度环 / 怒气 / Zzz / 成就飘条四个子元素。
 *
 * @param root 承载容器 `#pet-overlay-root`
 */
export class DomOverlayView implements OverlayView {
  private readonly coaxEl: HTMLDivElement;
  private readonly angerEl: HTMLDivElement;
  private readonly sleepEl: HTMLDivElement;
  private readonly toastEl: HTMLDivElement;

  constructor(root: HTMLElement) {
    this.coaxEl = document.createElement('div');
    this.coaxEl.className = 'overlay-coax';
    this.coaxEl.hidden = true;

    this.angerEl = document.createElement('div');
    this.angerEl.className = 'overlay-anger';
    this.angerEl.hidden = true;

    this.sleepEl = document.createElement('div');
    this.sleepEl.className = 'overlay-sleep';
    this.sleepEl.hidden = true;

    this.toastEl = document.createElement('div');
    this.toastEl.className = 'overlay-toast';
    this.toastEl.hidden = true;

    root.append(this.coaxEl, this.angerEl, this.sleepEl, this.toastEl);
  }

  /** 和好进度环：`null` 隐藏，否则写 CSS 变量 `--coax-progress`。 */
  setCoaxProgress(v: number | null): void {
    if (v === null) {
      this.coaxEl.hidden = true;
      return;
    }
    this.coaxEl.hidden = false;
    this.coaxEl.style.setProperty('--coax-progress', String(clamp01(v)));
  }

  /** 怒气符号：`<1` 隐藏，`≥1` 显示。 */
  setAngerLevel(level: number): void {
    const shown = Number.isFinite(level) && level >= 1;
    this.angerEl.hidden = !shown;
    this.angerEl.textContent = shown ? '!!' : '';
  }

  /** Zzz：`0` 隐藏 / `1` 普通 / `2` 放大。 */
  setSleepLevel(level: 0 | 1 | 2): void {
    if (level === 0) {
      this.sleepEl.hidden = true;
      this.sleepEl.textContent = '';
      this.sleepEl.classList.remove('overlay-sleep--big');
      return;
    }
    this.sleepEl.hidden = false;
    this.sleepEl.textContent = 'Zzz';
    this.sleepEl.classList.toggle('overlay-sleep--big', level === 2);
  }

  /** 成就飘条：`null` 隐藏，否则写文案显示。 */
  setToast(text: string | null): void {
    if (text === null) {
      this.toastEl.hidden = true;
      this.toastEl.textContent = '';
      return;
    }
    this.toastEl.textContent = text;
    this.toastEl.hidden = false;
  }

  /** 飘条不透明度（钳 [0,1]）。 */
  setToastOpacity(a: number): void {
    this.toastEl.style.opacity = String(clamp01(a));
  }
}

/**
 * 粒子层 DOM 适配器（S3-M6）：承载 `.pet-particle-layer` 子树。
 *
 * `render` 做差量收敛：以粒子 id 为键增删改节点（活粒子 ≤60，规模可控）；
 * 空列表 = 清场。子元素 `pointer-events:none`（粒子永不拦截交互）。
 *
 * @param root 承载容器 `#pet-overlay-root`
 */
export class DomParticleView implements ParticleView {
  private readonly layer: HTMLDivElement;
  private readonly nodes = new Map<number, HTMLDivElement>();

  constructor(root: HTMLElement) {
    this.layer = document.createElement('div');
    this.layer.className = 'pet-particle-layer';
    // 粒子层绘制序最低（character 之上、overlay 之下，02 §6.5）：先于其他视图 append。
    root.prepend(this.layer);
  }

  /** 渲染快照整体收敛（幂等：同输入重复调用无额外副作用）。 */
  render(items: readonly ParticleRenderItem[]): void {
    const alive = new Set<number>(items.map((it) => it.id));
    for (const [id, node] of this.nodes) {
      if (!alive.has(id)) {
        node.remove();
        this.nodes.delete(id);
      }
    }
    for (const it of items) {
      let node = this.nodes.get(it.id);
      if (node === undefined) {
        node = document.createElement('div');
        node.className = `particle particle--${it.kind}`;
        this.layer.appendChild(node);
        this.nodes.set(it.id, node);
      }
      node.style.transform = `translate(${it.x}px, ${it.y}px)`;
      node.style.opacity = String(it.opacity);
    }
  }
}

/**
 * 右键菜单 DOM 适配器（S3-M6）：承载 `.pet-menu-root`（背板 + 3×3 面板）。
 *
 * - 面板按钮按 `MenuItemSpec` 重建（九项固定，重建成本可忽略）；
 * - 灰化项 `disabled` + `title` 显示条件文案（§8.2「灰化并显示条件」）；
 * - 背板点击 → `onClose`（空白处关闭）；可用项点击 → `onCommand`（命令交出）；
 * - 隐藏态只用 visibility（与气泡同纪律），保留布局盒。
 *
 * @param root 承载容器 `#pet-overlay-root`
 */
export class DomMenuView implements MenuView {
  private readonly rootEl: HTMLDivElement;
  private readonly panel: HTMLDivElement;
  private readonly backdrop: HTMLDivElement;
  private readonly onCommand?: (id: MenuItemId) => void;
  private readonly onClose?: () => void;

  constructor(
    root: HTMLElement,
    handlers: { onCommand?: (id: MenuItemId) => void; onClose?: () => void } = {},
  ) {
    this.onCommand = handlers.onCommand;
    this.onClose = handlers.onClose;

    this.rootEl = document.createElement('div');
    this.rootEl.className = 'pet-menu-root';
    this.rootEl.style.visibility = 'hidden';

    this.backdrop = document.createElement('div');
    this.backdrop.className = 'pet-menu-backdrop';
    this.backdrop.addEventListener('click', () => this.onClose?.());

    this.panel = document.createElement('div');
    this.panel.className = 'pet-menu';

    this.rootEl.append(this.backdrop, this.panel);
    root.appendChild(this.rootEl);
  }

  /** 显示菜单：重建九宫格按钮 + 摆位 + 降级缩放 + 可见。 */
  show(items: readonly MenuItemSpec[], at: MenuPlacement): void {
    this.panel.replaceChildren();
    for (const item of items) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'pet-menu__item';
      button.disabled = !item.enabled;
      if (item.reason !== null) {
        button.title = item.reason;
      }
      const emoji = document.createElement('span');
      emoji.className = 'pet-menu__emoji';
      emoji.textContent = item.emoji;
      const label = document.createElement('span');
      label.className = 'pet-menu__label';
      label.textContent = item.label;
      button.append(emoji, label);
      if (item.enabled) {
        button.addEventListener('click', () => this.onCommand?.(item.id));
      }
      this.panel.appendChild(button);
    }
    this.panel.style.left = `${at.left}px`;
    this.panel.style.top = `${at.top}px`;
    // 裁定 B：容器装不下冻结规格（216×156）时等比缩小，保九个按钮全部落在视口内。
    // transform-origin 取左上角（默认 center 会因缩放把面板移出左/上边界）。
    const scale = menuScale(this.containerSize());
    this.panel.style.transformOrigin = 'top left';
    this.panel.style.transform = scale < 1 ? `scale(${scale})` : '';
    this.rootEl.style.visibility = 'visible';
  }

  /** 隐藏菜单（visibility 切换，禁 display:none——与气泡同纪律）。 */
  hide(): void {
    this.rootEl.style.visibility = 'hidden';
  }

  /** 承载容器内容盒尺寸（CSS px；摆位钳制用）。 */
  containerSize(): { width: number; height: number } {
    return { width: this.rootEl.clientWidth, height: this.rootEl.clientHeight };
  }
}
