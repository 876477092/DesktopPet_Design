/**
 * 气泡层状态机（S3-M5，T-10 段 · 上；`02 §3` BubbleLayer.ts / `03 §2 S3-M5`）。
 *
 * 职责：消费 `pet://bubble` 解析后的 `BubbleCmdV1`——仲裁（`decideBubble`）→ 计时
 * （停留 / 双侧淡入淡出）→ 摆位（`resolveBubblePlacement`）→ 经极窄端口 `BubbleView`
 * 落 DOM。所有几何 / 优先级 / 冷却逻辑在 `bubbleLogic.ts`（纯函数直测）。
 *
 * ⚠️ **本层禁 `setTimeout`**：Rust 停帧期间气泡不会自行消失，隐藏只发生在
 * `flush()` 被 `LayerHost.render()`（Rust 帧 tick）驱动时。**唯一时间真源 =
 * 构造注入的 `now()`**（默认 `performance.now`；禁 `Date.now` / 墙钟，C3）；
 * 重绘由外部驱动，本层无任何自循环 / 定时器（设计 §3 Q2/Q7）。
 *
 * 惰性 flush（设计 §3 Q7 / 主理人裁定 §4.7）：
 *   - `submit` / `dismiss` 只改内部状态 + 置脏标记，**不直接写 view**；
 *   - 所有 view 写入只发生在 `flush()` 内；新脏时按序
 *     `setContent → measure → resolveBubblePlacement → place → setVisible(true) → setOpacity`；
 *   - 无脏且无可见气泡 → `flush()` 立即返回（零 view 写入，60Hz 廉价）；
 *   - 可见期每次 flush 按 `fadeAlpha` 更新透明度（与上次相同则跳过，幂等）。
 *
 * 下游兼容：输入在 T-13 由 Rust `lines.rs` 产出（`03 §2 S4-M5`），API 已按此预留。
 * 本卡边界：不做台词池抽取 / 3min 求助冷却 / 道歉状态机 / 小面板本体。
 */

import type { BubbleCmdV1, BubbleKind } from '../shared/ipc';
import {
  BUBBLE_FADE_MS,
  BUBBLE_GAP,
  BUBBLE_PAD,
  BUBBLE_ANCHOR_TOP_DEFAULT,
  BUBBLE_TAIL_H,
  type BubbleContent,
  type BubbleView,
} from './layerPorts';
import {
  clampDwellMs,
  decideBubble,
  fadeAlpha,
  renderPlaceholders,
  resolveBubblePlacement,
} from './bubbleLogic';

/** 宠物头顶锚点（`resolveBubblePlacement` 入参；默认居中 / 待真机标定）。 */
export interface BubbleAnchor {
  readonly cx: number;
  readonly top: number;
}

/** 构造选项（全部可选；测试注入 Fake 时钟 / 可控开关）。 */
export interface BubbleLayerOptions {
  /** 单调时钟读数（默认 `performance.now`；C3：仅本地过渡时长，不读墙钟）。 */
  now?: () => number;
  /** C2 占位符变量供给（默认空；`{name}` 未命中时诚实保留原 token）。 */
  vars?: () => Record<string, string>;
  /** 勿扰开关（默认 `false`）。 */
  dnd?: () => boolean;
  /** 署名开关（默认 `true`——需求是「可关」）。 */
  signatureEnabled?: () => boolean;
  /** 用户高对比设置（默认 `false`，与 `cmd.highContrast` 取或）。 */
  highContrast?: () => boolean;
  /** 头顶锚点（默认容器宽/2 + `BUBBLE_ANCHOR_TOP_DEFAULT`）。 */
  anchor?: () => BubbleAnchor;
}

/** 当前可见气泡的层内状态。 */
interface ActiveBubble {
  readonly cmd: BubbleCmdV1;
  /** 已回退的冷却分组键（`cooldownKey` 空串 → `kind`）。 */
  readonly cooldownKey: string;
  readonly shownAt: number;
  readonly dwellMs: number;
  readonly expiresAt: number;
}

/**
 * 气泡层（仲裁 + 计时 + 摆位，惰性 flush）。
 *
 * @param view 极窄端口（真实 `DomBubbleView` / 测试 `FakeBubbleView`）
 * @param opts 构造选项（见 [`BubbleLayerOptions`]）
 */
export class BubbleLayer {
  private readonly now: () => number;
  private readonly vars: () => Record<string, string>;
  private readonly dnd: () => boolean;
  private readonly signatureEnabled: () => boolean;
  private readonly highContrast: () => boolean;
  private readonly anchor: () => BubbleAnchor;

  /** 当前可见气泡（`null` = 无；到期 / dismiss 后置 null）。 */
  private current: ActiveBubble | null = null;
  /** 冷却分组键 → 上次显示时刻（`decideBubble` 的 ≥20s 门）。 */
  private readonly lastShownAt = new Map<string, number>();
  /** 内容脏标记：需要重写 setContent/measure/place/setVisible(true)。 */
  private needContent = false;
  /** 隐藏脏标记：需要写 setVisible(false)。 */
  private needHide = false;
  /** 上次写入的不透明度（幂等比对；`null` = 未写 / 需强制写）。 */
  private lastAlpha: number | null = null;

  constructor(
    private readonly view: BubbleView,
    opts: BubbleLayerOptions = {},
  ) {
    this.now = opts.now ?? ((): number => performance.now());
    this.vars = opts.vars ?? ((): Record<string, string> => ({}));
    this.dnd = opts.dnd ?? ((): boolean => false);
    this.signatureEnabled = opts.signatureEnabled ?? ((): boolean => true);
    this.highContrast = opts.highContrast ?? ((): boolean => false);
    // 宠物头顶锚点待真机标定（见设计 §9-1）：默认居中 + 占位 top。
    this.anchor =
      opts.anchor ??
      ((): BubbleAnchor => ({
        cx: this.view.containerWidth() / 2,
        top: BUBBLE_ANCHOR_TOP_DEFAULT,
      }));
  }

  /**
   * 消费一条气泡命令（`pet://bubble` 解析后的入口，含仲裁 / 冷却）。
   *
   * `'show'`/`'replace'` 命中后：`lastShownAt.set(key, now)`、`shownAt = now`、
   * `expiresAt = now + clampDwellMs(dwellMs)`，并置内容脏（DOM 写入延后到 `flush()`）；
   * `'drop'` → 忽略（零状态变更、零 view 写入）。
   */
  submit(cmd: BubbleCmdV1): void {
    const now = this.now();
    const key = cmd.cooldownKey !== '' ? cmd.cooldownKey : cmd.kind;
    const decision = decideBubble(
      cmd,
      this.currentState(),
      this.lastShownAt.get(key),
      now,
      this.dnd(),
      this.vars(),
    );
    if (decision === 'drop') {
      return;
    }
    const dwellMs = clampDwellMs(cmd.dwellMs);
    this.current = {
      cmd,
      cooldownKey: key,
      shownAt: now,
      dwellMs,
      expiresAt: now + dwellMs,
    };
    this.lastShownAt.set(key, now);
    this.needContent = true;
    this.needHide = false;
  }

  /**
   * 惰性 flush（`LayerHost` draw 回调；每帧调用须廉价且幂等）。
   *
   * - 到期（`now - shownAt >= dwellMs`）→ 置隐藏脏；下次（本帧内）写 `setVisible(false)`；
   * - 无脏且无可见气泡 → 立即返回（零 view 写入）；
   * - 内容脏 → 按序 setContent → measure → resolveBubblePlacement → place → setVisible(true)；
   * - 可见期 → 按 `fadeAlpha` 更新透明度（值未变则跳过）。
   */
  flush(): void {
    const now = this.now();

    // 1) 到期判定（惰性隐藏：不靠定时器，靠帧 tick 推进）。
    if (this.current !== null && now - this.current.shownAt >= this.current.dwellMs) {
      this.current = null;
      this.needContent = false;
      this.needHide = true;
    }

    // 2) 脏检查：无脏且无可见气泡 → 零 DOM 写入。
    if (this.current === null && !this.needContent && !this.needHide) {
      return;
    }

    // 3) 隐藏路径。
    if (this.current === null) {
      if (this.needHide) {
        this.view.setVisible(false);
        this.needHide = false;
        this.lastAlpha = null;
      }
      return;
    }

    // 4) 内容脏：重摆（先 setContent 后 measure——隐藏态须保持布局盒，否则尺寸全 0）。
    if (this.needContent) {
      this.view.setContent(this.buildContent(this.current.cmd));
      const size = this.view.measure();
      const anchor = this.anchor();
      const placement = resolveBubblePlacement({
        containerWidth: this.view.containerWidth(),
        bubbleWidth: size.width,
        bubbleHeight: size.height,
        anchorCx: anchor.cx,
        anchorTop: anchor.top,
        gap: BUBBLE_GAP,
        pad: BUBBLE_PAD,
        tailH: BUBBLE_TAIL_H,
      });
      this.view.place(placement);
      this.view.setVisible(true);
      this.needContent = false;
      this.lastAlpha = null; // 强制本帧写一次透明度。
    }

    // 5) 可见期：双侧淡入淡出（幂等——与上次值相同则跳过）。
    const alpha = fadeAlpha(now - this.current.shownAt, this.current.dwellMs, BUBBLE_FADE_MS);
    if (alpha !== this.lastAlpha) {
      this.view.setOpacity(alpha);
      this.lastAlpha = alpha;
    }
  }

  /** 立即隐藏（如用户交互）；DOM 写入同样延后到 `flush()`（守「draw=惰性 flush」）。 */
  dismiss(): void {
    if (this.current === null && !this.needContent && !this.needHide) {
      return;
    }
    this.current = null;
    this.needContent = false;
    this.needHide = true;
  }

  /** 当前气泡的仲裁视图（`decideBubble` 入参；已过期由其内部按 `expiresAt` 判定）。 */
  private currentState(): { kind: BubbleKind; cooldownKey: string; expiresAt: number } | null {
    const c = this.current;
    return c === null
      ? null
      : { kind: c.cmd.kind, cooldownKey: c.cooldownKey, expiresAt: c.expiresAt };
  }

  /** 组装 `BubbleContent`（文案插值 / 署名 / 高对比取或 / 按钮透传）。 */
  private buildContent(cmd: BubbleCmdV1): BubbleContent {
    const vars = this.vars();
    const signature =
      this.signatureEnabled() && cmd.showSignature
        ? '\u2014\u2014 ' + renderPlaceholders('{name}', vars)
        : null;
    return {
      text: renderPlaceholders(cmd.text, vars),
      signature,
      highContrast: this.highContrast() || cmd.highContrast,
      actions: cmd.actions,
    };
  }
}
