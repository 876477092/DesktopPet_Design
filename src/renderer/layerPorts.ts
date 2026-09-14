/**
 * 气泡层 / 叠加层的 DOM 端口与共享 UI 常量（S3-M5，T-10 段 · 上）。
 *
 * 职责（`02 §3` BubbleLayer.ts / OverlayLayer.ts；`03 §2 S3-M5`）：
 *   定义两层依赖的**极窄端口** `BubbleView` / `OverlayView`，使几何 / 时序 / 状态机
 *   逻辑与 DOM 彻底解耦（端口/适配器，设计 §3 Q5）；真实 DOM 实现见 `DomLayers.ts`
 *   （唯一触碰 `document` 的代码），单测注入手写 Fake（仿 `FrameStage`）。
 *
 * 常量出处：
 *   - 停留 3~5s（`02 §5 K-5`）→ `BUBBLE_DWELL_[MIN/MAX/DEFAULT]_MS`（与线上契约同源，
 *     从 `shared/ipc.ts` 转发，避免双份魔法数）；
 *   - 同状态间隔 ≥20s（`01 §6.5.4`）→ `BUBBLE_COOLDOWN_MS`；
 *   - 长按阈值 500ms（`02 §5 K-6`）→ `REASON_CARD_LONG_PRESS_MS`；
 *   - 成就飘条（`01 §6.12.6`）→ `TOAST_MS` / `TOAST_QUEUE_CAP`；
 *   - 气泡宽 ≤ 宠物宽×2（`01 §4.4`：128 逻辑×2 = 256 CSS px）→ `BUBBLE_MAX_WIDTH`。
 *
 * 本卡边界：不做粒子 / 右键菜单 / 点击 DSP 反馈 / 情绪数值结算 / 台词抽取 / 道歉状态机 /
 * 桌面气泡小面板本体（设计 §0 会话边界）。
 */

import type { BubbleActionV1 } from '../shared/ipc';

/** 停留时长三态（与线上契约 `BubbleCmdV1.dwellMs` 同源）。 */
export {
  BUBBLE_DWELL_MIN_MS,
  BUBBLE_DWELL_MAX_MS,
  BUBBLE_DWELL_DEFAULT_MS,
} from '../shared/ipc';

// —— 共享 UI 常量（前端表现层常量，参照既有 `CROSSFADE_MS` 范式；非业务数值配置）——

/** 同状态气泡最小间隔（毫秒，`01 §6.5.4`：≥20s）。 */
export const BUBBLE_COOLDOWN_MS = 20000;
/** 情绪原因卡长按阈值（毫秒，`02 §5 K-6`）。 */
export const REASON_CARD_LONG_PRESS_MS = 500;
/** 成就飘条单项显示时长（毫秒）。 */
export const TOAST_MS = 2500;
/** 成就飘条等待队列上限（同屏并发恒 1）。 */
export const TOAST_QUEUE_CAP = 3;
/** 宠物逻辑宽（CSS px，`02 §4.4` 视觉契约 128×128 逻辑）。 */
export const PET_LOGICAL_WIDTH = 128;
/** 气泡最大宽（= 宠物逻辑宽×2 = 256 CSS px，恰为宠物窗口内容宽）。 */
export const BUBBLE_MAX_WIDTH = PET_LOGICAL_WIDTH * 2;
/** 气泡淡入淡出时长（毫秒，双侧）。 */
export const BUBBLE_FADE_MS = 200;

/** 气泡与锚点的水平间隙（CSS px）。 */
export const BUBBLE_GAP = 8;
/** 气泡与容器边缘的安全内边距（CSS px）。 */
export const BUBBLE_PAD = 4;
/** 气泡尾巴高度（CSS px，纵向让位用）。 */
export const BUBBLE_TAIL_H = 10;
/** 宠物头顶锚点默认纵向坐标（CSS px）。 */
export const BUBBLE_ANCHOR_TOP_DEFAULT = 24;
// 宠物头顶锚点待真机标定（见设计 §9-1）：`anchorTop=24` 为占位默认，产品/美术校准后覆盖。

/** 气泡摆位结果（右侧优先 → 左侧翻转 → 两侧不足则钳制）。 */
export interface BubblePlacement {
  /** 摆位分支：右侧 / 左侧 / 钳制（尾巴朝向由 `.bubble--{side}` 控制）。 */
  readonly side: 'right' | 'left' | 'clamp';
  /** 气泡左边界（CSS px，相对 `#pet-overlay-root`）。 */
  readonly left: number;
  /** 气泡上边界（CSS px，相对 `#pet-overlay-root`）。 */
  readonly top: number;
}

/** 气泡内容（由 `BubbleLayer` 计算、`DomBubbleView` 落 DOM；字段定死，勿自创）。 */
export interface BubbleContent {
  /** 文案（已 `renderPlaceholders`）。 */
  readonly text: string;
  /** 署名（已含「—— 」前缀），或 `null` 表示不显示。 */
  readonly signature: string | null;
  /** 高对比样式开关（`.bubble--hc`）。 */
  readonly highContrast: boolean;
  /** 快捷按钮（点击经 `DomBubbleView` 的 `onAction` 回调交出）。 */
  readonly actions: readonly BubbleActionV1[];
}

/** 气泡视图端口（真实实现 `DomBubbleView`，测试注入 Fake）。 */
export interface BubbleView {
  /** 写入内容（文案 / 署名 / 高对比 / 按钮）。 */
  setContent(c: BubbleContent): void;
  /** 读取气泡自身尺寸（CSS px，`getBoundingClientRect`）。 */
  measure(): { width: number; height: number };
  /** 摆位（锚点 + 翻转 + 钳制结果）。 */
  place(p: BubblePlacement): void;
  /** 设置不透明度（0~1）。 */
  setOpacity(a: number): void;
  /** 显示 / 隐藏（隐藏须用 visibility，禁 display:none，否则 `measure()` 退化）。 */
  setVisible(v: boolean): void;
  /**
   * 承载容器内容盒宽（`#pet-overlay-root.clientWidth`，CSS px）。
   *
   * 供 `resolveBubblePlacement` 的 `containerWidth` 入参使用——层类拿不到容器尺寸，
   * `measure()` 只测气泡自身。**不用** `window.innerWidth`（缩放语义歧义）。
   */
  containerWidth(): number;
}

/** 叠加层视图端口（真实实现 `DomOverlayView`，测试注入 Fake）。 */
export interface OverlayView {
  /** 和好进度环（`null` 隐藏；0~1）。 */
  setCoaxProgress(v: number | null): void;
  /** 怒气符号（`0` 隐藏；`≥1` 显示）。 */
  setAngerLevel(level: number): void;
  /** Zzz 睡眠档（`0` 隐藏 / `1` `Zzz` / `2` `Zzz（大）`）。 */
  setSleepLevel(level: 0 | 1 | 2): void;
  /** 成就飘条（`null` 隐藏）。 */
  setToast(text: string | null): void;
  /** 飘条不透明度（0~1）。 */
  setToastOpacity(a: number): void;
}

// ---------------------------------------------------------------------------
// 粒子层端口（S3-M6，T-10 段 · 下；`02 §3` ParticleLayer / §5.22 ParticlesCfg）
// ---------------------------------------------------------------------------

/** 单次迸发上限（`01 附录` / `02 §5.22`：60；与线上契约同源转发）。 */
export { PARTICLE_BURST_CAP } from '../shared/ipc';

/** 粒子寿命（毫秒，`01 附录` / `02 §5.22`：1.2s；到期由帧 tick 驱动的 flush 惰性回收）。 */
export const PARTICLE_LIFETIME_MS = 1200;

/** 爱心粒子色（`01 附录 A.3`：`#FF8FB1`；与 `--pet-heart-pink` 同源）。 */
export const PARTICLE_HEART_COLOR = '#FF8FB1';

/** 粒子锚点横向偏移（CSS px；「头顶偏右」= 容器中线 + 此值，待真机标定）。 */
export const PARTICLE_ANCHOR_OFFSET_X = 24;
/** 粒子锚点纵向默认坐标（CSS px；头顶高度占位，待真机标定，见设计 §9-1）。 */
export const PARTICLE_ANCHOR_TOP_DEFAULT = 32;

/** 单颗粒子的渲染快照（`ParticleLayer` 计算、`DomParticleView` 落 DOM）。 */
export interface ParticleRenderItem {
  /** 生命周期内唯一 id（burst 内自增，DOM 节点差量收敛键）。 */
  readonly id: number;
  /** 粒子类别（决定样式类与色彩）。 */
  readonly kind: ParticleKindV;
  /** 当前 X（CSS px，相对 `#pet-overlay-root`；含迸发初速 × 年龄 + 重力位移）。 */
  readonly x: number;
  /** 当前 Y（CSS px）。 */
  readonly y: number;
  /** 不透明度（1 → 0 随年龄线性衰减）。 */
  readonly opacity: number;
}

/** 粒子类别（自 `shared/ipc` 转发，避免双份枚举）。 */
export type ParticleKindV = 'heart' | 'star' | 'dust' | 'tear' | 'anger';

/** 粒子层视图端口（真实实现 `DomParticleView`，测试注入 Fake）。 */
export interface ParticleView {
  /**
   * 以渲染快照**整体收敛**粒子 DOM（差量增删改；空列表 = 清场）。
   * 幂等：同输入重复调用不产生额外副作用。
   */
  render(items: readonly ParticleRenderItem[]): void;
}

// ---------------------------------------------------------------------------
// 右键菜单端口（S3-M6，T-10 段 · 下；`01 §8.2` 九项列表式菜单）
// ---------------------------------------------------------------------------

/** 菜单宽（CSS px；3 列 × 64 + 间距 + 内边距）。 */
export const MENU_WIDTH = 216;
/** 菜单高（CSS px；3 行 × 44 + 间距 + 内边距）。 */
export const MENU_HEIGHT = 156;
/** 菜单与容器边缘的安全间隙（CSS px，钳制摆位用）。 */
export const MENU_EDGE_PAD = 4;

/** 菜单项 id（`01 §8.2` 九项；顺序即 3×3 行主序）。 */
export type MenuItemId =
  | 'stroke'
  | 'feed'
  | 'bath'
  | 'dispatch'
  | 'shop'
  | 'attrs'
  | 'photo'
  | 'settings'
  | 'hide';

/** 菜单项规格（`buildMenuItems` 产出、`DomMenuView` 落 DOM；字段定死，勿自创）。 */
export interface MenuItemSpec {
  /** 菜单项 id（命令路由键）。 */
  readonly id: MenuItemId;
  /** 文案（C2 不涉角色名，可直接中文字面量）。 */
  readonly label: string;
  /** 图标 emoji（`01 §8.2` 冻结图样）。 */
  readonly emoji: string;
  /** 是否可用（本阶段仅 `hide` 可实接，其余灰化）。 */
  readonly enabled: boolean;
  /** 灰化条件文案（`enabled=true` 时为 `null`；§8.2「灰化并显示条件」）。 */
  readonly reason: string | null;
}

/** 菜单摆位结果（容器内左上角，已钳制）。 */
export interface MenuPlacement {
  /** 左边界（CSS px，相对 `#pet-overlay-root`）。 */
  readonly left: number;
  /** 上边界（CSS px）。 */
  readonly top: number;
}

/** 右键菜单视图端口（真实实现 `DomMenuView`，测试注入 Fake）。 */
export interface MenuView {
  /** 显示菜单（重建九宫格按钮并摆位）。 */
  show(items: readonly MenuItemSpec[], at: MenuPlacement): void;
  /** 隐藏菜单（visibility 切换，保留布局盒）。 */
  hide(): void;
  /** 承载容器内容盒尺寸（`#pet-overlay-root`，摆位钳制用）。 */
  containerSize(): { width: number; height: number };
}
