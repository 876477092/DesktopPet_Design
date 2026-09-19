/**
 * 渲染后端切换器（K-14，S9-M1）。
 *
 * 职责：在「骨架主路径」与「帧回退路径」间做**单一真相**路由——
 *   - 启动即把入帧路由到 `fallback`（帧路径），保证骨架运行时加载期屏幕不空、
 *     **不黑屏**；
 *   - 每次被动推帧时检查主路径是否 `isReady()`：就绪即提升为主路由（行为一致）；
 *   - 距启动超过 [`SKELETON_READY_TIMEOUT_MS`]（3s）主路径仍未就绪 → **定格回退**
 *     （此后不再尝试提升，主路径后续就绪也不抢占，避免中途硬切）。
 *
 * 驱动模型（K-4）：无 rAF 自循环——就绪判定随 Rust 侧被动推帧推进（`draw` 时读
 * 注入时钟）；`paint` 只把当前激活后端同步合成。单测注入假时钟与假后端。
 */
import type { RenderFrameCmdV1 } from '../shared/ipc';
import { SKELETON_READY_TIMEOUT_MS, type ICharacterRenderer } from './ICharacterRenderer';

/** 切换结果回调（观测/遥测用；不参与绘制决策）。 */
export interface SwitcherHooks {
  /** 主路径就绪、已提升为当前路由（`kind` 为主路径种类）。 */
  onPromote?: (kind: ICharacterRenderer['kind']) => void;
  /** 3s 超时主路径仍未就绪、定格在帧回退路径。 */
  onFallbackLatched?: (reason: 'timeout') => void;
}

export class BackendSwitcher {
  private readonly primary: ICharacterRenderer;
  private readonly fallback: ICharacterRenderer;
  private readonly hooks: SwitcherHooks;
  private readonly now: () => number;
  private readonly timeoutMs: number;
  private readonly armedAt: number;
  /** 是否已定格：此后 active 不再变化。 */
  private latched = false;
  private active: ICharacterRenderer;

  /**
   * @param primary   骨架主路径（S9-M2 `SkeletonRenderer`）；加载期 `isReady()=false`
   * @param fallback  帧回退路径（`FrameRenderer`，恒就绪）
   * @param hooks     提升/回退观测回调
   * @param now       单调时钟（默认 `performance.now`；C3：不读墙钟）
   * @param timeoutMs 主路径就绪超时（默认 3s，K-14）
   */
  constructor(
    primary: ICharacterRenderer,
    fallback: ICharacterRenderer,
    hooks: SwitcherHooks = {},
    now: () => number = () => performance.now(),
    timeoutMs: number = SKELETON_READY_TIMEOUT_MS,
  ) {
    this.primary = primary;
    this.fallback = fallback;
    this.hooks = hooks;
    this.now = now;
    this.timeoutMs = timeoutMs;
    this.armedAt = now();
    // 启动即走帧路径：骨架加载期屏幕持续有内容（不黑屏）。
    this.active = fallback;
  }

  /** 当前实际绘制的后端种类（观测/QA 用）。 */
  get activeKind(): ICharacterRenderer['kind'] {
    return this.active.kind;
  }

  /** 是否已定格在帧回退（3s 超时未提升）。 */
  get isFallbackLatched(): boolean {
    return this.latched && this.active === this.fallback;
  }

  /**
   * 被动推帧入口：先按当前时钟推进就绪判定，再路由给激活后端。
   *
   * 主路径启动即就绪 → 第一帧即提升；否则随推帧轮询，就绪即提升，超时即定格。
   */
  draw(cmd: RenderFrameCmdV1): void {
    this.maybeAdvance();
    this.active.draw(cmd);
  }

  /** 同步绘制当前激活后端的就绪内容（注册为 `LayerHost` character 层）。 */
  paint(): void {
    this.maybeAdvance();
    this.active.paint();
  }

  /** 就绪状态推进（纯判定；无副作用除回调）。 */
  private maybeAdvance(): void {
    if (this.latched) {
      return;
    }
    if (this.primary.isReady()) {
      this.active = this.primary;
      this.latched = true;
      this.hooks.onPromote?.(this.primary.kind);
      return;
    }
    if (this.now() - this.armedAt >= this.timeoutMs) {
      // 3s 未就绪：定格在帧回退（保持 active=fallback），不再提升。
      this.latched = true;
      this.hooks.onFallbackLatched?.('timeout');
    }
  }
}
