import './styles/pet.css';
import { resizeCanvasToWindow } from './shared/coords';
import {
  invokeCommand,
  listenEvent,
  parseBubbleCmd,
  parseCoaxCmd,
  parseMenuCmd,
  parseParticleCmd,
  parseRenderFrameCmd,
} from './shared/ipc';
import { PET_EVENT } from './shared/types';
import { AtlasCache } from './renderer/AtlasCache';
import { BubbleLayer } from './renderer/BubbleLayer';
import { DomBubbleView, DomMenuView, DomOverlayView, DomParticleView } from './renderer/DomLayers';
import { FrameRenderer } from './renderer/FrameRenderer';
import { LayerHost } from './renderer/LayerHost';
import { MenuLayer } from './renderer/MenuLayer';
import { OverlayLayer } from './renderer/OverlayLayer';
import { ParticleLayer } from './renderer/ParticleLayer';
import { WebGLStage } from './renderer/WebGLStage';

/**
 * 宠物窗口引导（S2-M1 重写，T-04 段 · 上；S3-M5 增气泡/叠加层装配）。
 *
 * 职责（`02 §3` main.ts / S2-M1 卡片）：
 *   1. 画布初始化与 **DPI 变更重建**（`resizeCanvasToWindow` → `WebGLStage.resize` →
 *      `LayerHost.render` 重绘最后一帧，保证 DPI 变更后无错位）；
 *   2. `pet://frame` 被动推帧订阅（`02 §7.6`）：解析 `RenderFrameCmd` v1 →
 *      `FrameRenderer.draw`，**无 rAF 自循环**（K-4：渲染 tick 由 Rust 侧驱动）；
 *   3. 渲染装配：`WebGLStage` 三级探测（K-14）→ `AtlasCache` → `FrameRenderer` →
 *      `LayerHost` 图层编排；
 *   4. S3-M5：装配 `DomBubbleView`/`DomOverlayView` + `BubbleLayer`/`OverlayLayer`，
 *      注册 overlay/bubble 两层（draw = 惰性 flush）+ `pet://bubble` 订阅
 *      （解析失败 warn 跳过，`02 §7.4`；零新增事件，C8）；
 *   5. S3-M6：装配 `DomParticleView`/`DomMenuView` + `ParticleLayer`/`MenuLayer`，
 *      注册 particle/menu 两层 + `pet://fx`/`pet://menu` 订阅（均已登记 `02 §7.6`）；
 *   6. S4-M3：订阅 `pet://coax`（**S4-M3 登记**）——和好进度环由上游 `CoaxFlow` 结算，
 *      本层只把 `ratio` 交给 `OverlayLayer.setCoaxProgress`（`active=false` 即隐藏）；
 *   7. S6-M2：帧绘制回执——每帧合成完成后节流（500ms）invoke `frame_receipt`，
 *      供 Rust 侧渲染看门狗计数（K-8：连续 3 次 5s 无回执 → 自动重建渲染）。
 *
 * 约束：C3 前端不读系统时钟（本文件无时间逻辑；节流用 `performance.now()` 单调钟）；
 * C8 事件名取自 `PET_EVENT`；C9 仅使用已登记的本地能力（`core:event:default` +
 * 自定义命令 `atlas_png` / `menu_command` / `frame_receipt`）。
 */
const PET_CANVAS_ID = 'pet-canvas';
const PET_OVERLAY_ROOT_ID = 'pet-overlay-root';

/** 宠物逻辑尺寸（`02 §4.4` 视觉契约：128×128 逻辑，导出 2x）。 */
const LOGICAL_SIZE = 128;
const LOGICAL_SCALE = 2;

/** S6-M2 帧回执节流间隔（毫秒，单调钟）：2 次/秒，足够看门狗判定存活，不刷 IPC。 */
const FRAME_RECEIPT_THROTTLE_MS = 500;

/**
 * S6-M2：帧绘制回执（K-8 看门狗计数源）。
 *
 * 在「合成完成」回调（`onFrameReady` → `host.render()` 之后）调用；500ms 节流
 * （`performance.now()` 单调钟，C3 不读墙钟）。命令未注册（旧二进制 / 看门狗
 * 装配缺失）→ invoke 失败静默跳过，不阻塞帧循环（`02 §7.4` 可读降级）。
 */
function createFrameReceipt(): { onFrameRendered(): void } {
  let lastSentMs = -Infinity;
  return {
    onFrameRendered(): void {
      const now = performance.now();
      if (now - lastSentMs < FRAME_RECEIPT_THROTTLE_MS) {
        return;
      }
      lastSentMs = now;
      void invokeCommand<void>('frame_receipt').catch(() => {
        // 看门狗未注册 / 调用失败：静默跳过（渲染不中断，看门狗自愈不触发）。
      });
    },
  };
}

/** 图集 PNG 字节加载器：经最小自定义命令 `atlas_png`（不走 asset 协议网络面，C9）。 */
async function loadAtlasBytes(name: string): Promise<ArrayBuffer | null> {
  try {
    const bytes = await invokeCommand<number[]>('atlas_png', { name });
    if (!Array.isArray(bytes) || bytes.length === 0) {
      return null;
    }
    return new Uint8Array(bytes).buffer as ArrayBuffer;
  } catch (err) {
    // 02 §7.4.3：invoke 失败必须可读降级——图集缺失时前端持续跳帧，不崩溃不静默吞错。
    console.warn('[pet] atlas_png 调用降级：', err);
    return null;
  }
}

function locateCanvas(): HTMLCanvasElement {
  const canvas = document.getElementById(PET_CANVAS_ID);
  if (!(canvas instanceof HTMLCanvasElement)) {
    throw new Error(`未找到画布元素 #${PET_CANVAS_ID}`);
  }
  return canvas;
}

/** 叠加层/气泡层 DOM 根（S3-M5；`index.html` 中置于 canvas 之后）。 */
function locateOverlayRoot(): HTMLElement {
  const root = document.getElementById(PET_OVERLAY_ROOT_ID);
  if (root === null) {
    throw new Error(`未找到叠加层根容器 #${PET_OVERLAY_ROOT_ID}`);
  }
  return root;
}

async function bootstrapPetWindow(): Promise<void> {
  const canvas = locateCanvas();

  // 逻辑尺寸先定死，随后按 DPR 放大位图，保证物理像素充足。
  canvas.style.width = `${LOGICAL_SIZE * LOGICAL_SCALE}px`;
  canvas.style.height = `${LOGICAL_SIZE * LOGICAL_SCALE}px`;

  // 渲染装配（K-14 三级探测在 Stage.create 内完成，失败自动降级 Canvas2D）。
  const stage = WebGLStage.create(canvas);
  const cache = new AtlasCache(loadAtlasBytes);
  const host = new LayerHost();
  // S6-M2：合成完成后上报帧回执（节流见 createFrameReceipt；看门狗计数源）。
  const receipt = createFrameReceipt();
  const renderer = new FrameRenderer(stage, cache, () => {
    host.render();
    receipt.onFrameRendered();
  });
  host.setLayer('character', () => renderer.paint());

  // S3-M5 装配：DOM 两视图（唯一 document 触点，DomLayers.ts）+ 两层状态机；
  // draw = 惰性 flush（Q7），DOM 顺序/z-index 与 LAYER_DRAW_ORDER 一致（overlay 先、bubble 后）。
  // 动作/原因卡入口走回调（C8 零新增事件）；消费者接线归 T-13/T-26。
  const overlayRoot = locateOverlayRoot();

  // S3-M6 装配：粒子层视图须最先 append（绘制序最低，character 之上、overlay 之下）；
  // 菜单层视图最后 append（z 序最高，bubble 之上）。
  const particleView = new DomParticleView(overlayRoot);
  const bubbleView = new DomBubbleView(overlayRoot, (action) =>
    console.info('[pet] 气泡动作：', action.id),
  );
  const overlayView = new DomOverlayView(overlayRoot);

  // S3-M6：右键菜单命令收口——「隐藏」经既有命令通道调 `menu_command`
  // （Rust 侧 MenuCommand 路由复用托盘可见态镜像；其余项灰化不可达）。
  const menuView = new DomMenuView(overlayRoot, {
    onCommand: (id) => menu.choose(id),
    onClose: () => menu.close(),
  });

  const bubble = new BubbleLayer(bubbleView);
  const overlay = new OverlayLayer(overlayView, {
    onReasonCardRequest: () => console.info('[pet] 请求情绪原因卡（T-26 接入）'),
  });
  const particles = new ParticleLayer(particleView);
  const menu = new MenuLayer(menuView, {
    onCommand: (id) => {
      if (id === 'hide') {
        void invokeCommand('menu_command', { command: 'hide' }).catch((err: unknown) =>
          console.warn('[pet] menu_command 调用降级：', err),
        );
        return;
      }
      // 灰化项不可达（按钮 disabled）；此分支仅防御性记录。
      console.info('[pet] 菜单项未实装：', id);
    },
  });
  host.setLayer('particle', () => particles.flush());
  host.setLayer('overlay', () => overlay.flush());
  host.setLayer('bubble', () => bubble.flush());
  host.setLayer('menu', () => menu.flush());

  // DPI / 缩放变化：重建位图并重绘最后一帧（DPI 变更后重建无错位）。
  // 注：重建后 GL 纹理仍有效（贴图不随画布位图重置），重绘即可复原。
  const applySize = (): void => {
    resizeCanvasToWindow(canvas, LOGICAL_SIZE * LOGICAL_SCALE);
    stage.resize(canvas.width, canvas.height);
    host.render();
  };
  applySize();
  window.addEventListener('resize', applySize);
  window.matchMedia('(resolution: 1dppx)').addEventListener('change', applySize);

  // pet://frame 被动推帧（02 §7.6，C8）——解析容忍缺字段 / 忽略未知字段（前向兼容），
  // 解析失败跳帧降级（02 §7.4）。
  const unlistenFrame = await listenEvent<unknown>(PET_EVENT.FRAME, (payload) => {
    const cmd = parseRenderFrameCmd(payload);
    if (cmd === null) {
      console.warn('[pet] pet://frame 载荷解析失败，跳帧');
      return;
    }
    renderer.draw(cmd);
  });

  // pet://bubble 被动订阅（02 §7.6，C8）——解析失败 warn 跳过（02 §7.4，不静默吞错）；
  // 空文案/冷却/勿扰等消费侧校验由 BubbleLayer.submit 内的 decideBubble 承担。
  const unlistenBubble = await listenEvent<unknown>(PET_EVENT.BUBBLE, (payload) => {
    const cmd = parseBubbleCmd(payload);
    if (cmd === null) {
      console.warn('[pet] pet://bubble 载荷解析失败，跳过');
      return;
    }
    bubble.submit(cmd);
  });

  // pet://fx 被动订阅（S3-M6，02 §7.6 已登记）——迸发数量/类别已由 Rust 发射侧钳制，
  // 前端解析再钳兜底；解析失败 warn 跳过（02 §7.4）。
  const unlistenFx = await listenEvent<unknown>(PET_EVENT.FX, (payload) => {
    const cmd = parseParticleCmd(payload);
    if (cmd === null) {
      console.warn('[pet] pet://fx 载荷解析失败，跳过');
      return;
    }
    particles.submit(cmd);
  });

  // pet://menu 被动订阅（S3-M6，02 §7.6 已登记）——右键单击命中（Up 触发）弹菜单；
  // 摆位由 MenuLayer.flush 经 clampMenuPlacement 钳回容器内。
  const unlistenMenu = await listenEvent<unknown>(PET_EVENT.MENU, (payload) => {
    const cmd = parseMenuCmd(payload);
    if (cmd === null) {
      console.warn('[pet] pet://menu 载荷解析失败，跳过');
      return;
    }
    menu.open({ x: cmd.localX, y: cmd.localY });
  });

  // pet://coax 被动订阅（S4-M3，02 §7.6 已登记）——道歉三部曲进度环 + 离家态。
  // 上游 CoaxFlow 已完成中断回退（回退 50%）等全部结算，本层**只显示**（防双份真相）。
  const unlistenCoax = await listenEvent<unknown>(PET_EVENT.COAX, (payload) => {
    const cmd = parseCoaxCmd(payload);
    if (cmd === null) {
      console.warn('[pet] pet://coax 载荷解析失败，跳过');
      return;
    }
    overlay.setCoaxProgress(cmd.active ? cmd.ratio : null);
  });

  window.addEventListener('beforeunload', () => {
    unlistenFrame();
    unlistenBubble();
    unlistenFx();
    unlistenMenu();
    unlistenCoax();
  });
}

void bootstrapPetWindow();
