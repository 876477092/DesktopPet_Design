/**
 * 气泡层纯函数（S3-M5，T-10 段 · 上）。
 *
 * 职责（设计 §3 Q5）：把气泡的**几何 / 时序 / 优先级 / 冷却 / 插值**逻辑全部抽为
 * 无 `this`、无 DOM、无系统时钟的**可导出纯函数**，供 `BubbleLayer` 消费并直接单测。
 *
 * 需求溯源：
 *   - 头顶偏右 / 越界翻转 / 宽 ≤ 宠物宽×2 → `resolveBubblePlacement`（`01 §6.5.4`，Q4）；
 *   - 同状态间隔 ≥20s → `shouldSuppressDuplicate`（`01 §6.5.4`，Q3）；
 *   - 交互台词即时覆盖系统台词 / 优先级 提醒>求助>闲聊 → `decideBubble`（`01 §6.5.4`/§6.16.3）；
 *   - 勿扰仅提醒类 → `shouldShowUnderDnd`（`01 §6.5.4`，按 `kind` 派生，非线上字段）；
 *   - 单条 3~5s → `clampDwellMs`（`02 §5 K-5`）；
 *   - 署名占位符 → `renderPlaceholders`（`01 §6.16.4`，C2）。
 *
 * 本卡边界：**不做** 3 分钟求助间隔 / 拒绝翻倍（归 T-13/S4-M5）、粒子、菜单、情绪结算。
 */

import type { BubbleCmdV1, BubbleKind } from '../shared/ipc';
import type { BubblePlacement } from './layerPorts';
import {
  BUBBLE_COOLDOWN_MS,
  BUBBLE_DWELL_DEFAULT_MS,
  BUBBLE_DWELL_MAX_MS,
  BUBBLE_DWELL_MIN_MS,
} from './layerPorts';

/** 钳制到 [min, max]（模块私有）。 */
function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/** 气泡优先级（`01 §6.16.3`：提醒类 > 求助类 > 系统闲聊；明信片最低）。 */
const BUBBLE_PRIORITY: Record<BubbleKind, number> = {
  reminder: 3,
  help: 2,
  chat: 1,
  postcard: 0,
};

/**
 * 占位符替换（C2）：仅替换 `vars` 命中的 `{token}`，**未命中的 token 原样保留**（不吞字符）。
 *
 * 例：`renderPlaceholders('{name}：你好', {})` → `'{name}：你好'`（诚实降级，不硬编码角色名）。
 *
 * @param text 含 `{token}` 的文案
 * @param vars token → 替换值（缺 `name` 时 `{name}` 保留）
 */
export function renderPlaceholders(text: string, vars: Record<string, string>): string {
  return text.replace(/\{([^{}]*)\}/g, (whole, token: string) => {
    const value = Object.prototype.hasOwnProperty.call(vars, token) ? vars[token] : undefined;
    return value !== undefined ? value : whole;
  });
}

/**
 * 停留时长钳到 `[3000,5000]`（`02 §5 K-5`：单条 3~5s）。
 *
 * NaN → 默认 `4000`；±∞ 由钳制自然收敛到上/下界。
 */
export function clampDwellMs(ms: number): number {
  if (Number.isNaN(ms)) {
    return BUBBLE_DWELL_DEFAULT_MS;
  }
  return clamp(ms, BUBBLE_DWELL_MIN_MS, BUBBLE_DWELL_MAX_MS);
}

/** 气泡优先级数值（reminder=3 > help=2 > chat=1 > postcard=0）。 */
export function bubblePriority(kind: BubbleKind): number {
  return BUBBLE_PRIORITY[kind];
}

/**
 * 勿扰（DND）派生过滤（`01 §6.5.4`）：勿扰时**仅放行 `reminder`**（产品红线），
 * 其余类别抑制；非勿扰全放行。
 */
export function shouldShowUnderDnd(kind: BubbleKind, dnd: boolean): boolean {
  if (!dnd) {
    return true;
  }
  return kind === 'reminder';
}

/**
 * 同状态 ≥20s 冷却判定（`01 §6.5.4`）。
 *
 * @param cooldownKey 冷却分组键（空串表示无有效分组，**不抑制**——调用方应已回退 `kind`）
 * @param lastShownAt 该分组键上次显示时刻（`undefined` = 从未显示）
 * @param now         当前时钟读数
 * @param windowMs    冷却窗口（默认 `BUBBLE_COOLDOWN_MS = 20000`；**恰为 20000ms 时放行**）
 * @returns `true` 表示应抑制（距上次显示不足窗口）
 */
export function shouldSuppressDuplicate(
  cooldownKey: string,
  lastShownAt: number | undefined,
  now: number,
  windowMs: number = BUBBLE_COOLDOWN_MS,
): boolean {
  if (cooldownKey.length === 0) {
    return false;
  }
  if (lastShownAt === undefined) {
    return false;
  }
  return now - lastShownAt < windowMs;
}

/** 当前可见气泡的仲裁视图（`decideBubble` 入参）。 */
export interface CurrentBubbleState {
  readonly kind: BubbleKind;
  readonly cooldownKey: string;
  readonly expiresAt: number;
}

/**
 * 气泡仲裁（设计 §4.1 真值表，**求值顺序即短路顺序**）。
 *
 * 求值顺序：
 *   1. 文案为空（`renderPlaceholders(text, vars).trim() === ''`）→ `'drop'`；
 *   2. 勿扰且非 `reminder` → `'drop'`（勿扰仅放行提醒类，产品红线）；
 *   3. 同状态 ≥20s 冷却未过（对所有 `kind` 一律生效）→ `'drop'`；
 *   4. 无有效当前气泡（无 / 已过期视为无）→ `'show'`；
 *   5. `preempt`（用户交互台词即时覆盖系统台词）→ `'replace'`；
 *   6. 优先级 ≥ 当前 → `'replace'`；
 *   7. 否则（低优先级不得打断高优先级）→ `'drop'`。
 *
 * @param incoming    进入的气泡
 * @param current     当前可见气泡（`null` = 无）；`expiresAt <= now` 视为已过期（等同无）
 * @param lastShownAt 该分组键上次显示时刻（`undefined` = 从未）
 * @param now         当前时钟读数
 * @param dnd         是否勿扰
 * @param vars        占位符变量（默认空；用于空文案判定，与 `BubbleLayer` 同源）
 */
export function decideBubble(
  incoming: BubbleCmdV1,
  current: CurrentBubbleState | null,
  lastShownAt: number | undefined,
  now: number,
  dnd: boolean,
  vars: Record<string, string> = {},
): 'show' | 'replace' | 'drop' {
  const key = incoming.cooldownKey !== '' ? incoming.cooldownKey : incoming.kind;
  const effectiveCurrent = current !== null && now < current.expiresAt ? current : null;

  // 1) 文案为空 → drop
  if (renderPlaceholders(incoming.text, vars).trim() === '') {
    return 'drop';
  }
  // 2) 勿扰且非提醒类 → drop
  if (!shouldShowUnderDnd(incoming.kind, dnd)) {
    return 'drop';
  }
  // 3) 同状态 ≥20s 冷却未过 → drop
  if (shouldSuppressDuplicate(key, lastShownAt, now)) {
    return 'drop';
  }
  // 4) 无有效当前气泡 → show
  if (effectiveCurrent === null) {
    return 'show';
  }
  // 5) 交互台词即时覆盖 → replace
  if (incoming.preempt) {
    return 'replace';
  }
  // 6) 优先级 ≥ 当前 → replace
  if (bubblePriority(incoming.kind) >= bubblePriority(effectiveCurrent.kind)) {
    return 'replace';
  }
  // 7) 低优先级不得打断高优先级 → drop
  return 'drop';
}

/** `resolveBubblePlacement` 入参（全 CSS px）。 */
export interface PlacementInput {
  /** 承载容器内容盒宽（`#pet-overlay-root.clientWidth`）。 */
  readonly containerWidth: number;
  /** 气泡实测宽（`getBoundingClientRect().width`）。 */
  readonly bubbleWidth: number;
  /** 气泡实测高。 */
  readonly bubbleHeight: number;
  /** 宠物头顶锚点 x（默认容器宽/2）。 */
  readonly anchorCx: number;
  /** 宠物头顶锚点 y。 */
  readonly anchorTop: number;
  /** 水平间隙（`BUBBLE_GAP`）。 */
  readonly gap: number;
  /** 安全内边距（`BUBBLE_PAD`）。 */
  readonly pad: number;
  /** 尾巴高度（`BUBBLE_TAIL_H`）。 */
  readonly tailH: number;
}

/**
 * 气泡摆位（设计 §3 Q4）：右侧优先 → 左侧翻转 → 两侧不足则钳制；纵向越上边界向下钳制。
 *
 * ⚠️ **窗口几何约束（非缺陷）**：宠物窗口内容宽 = 256 CSS px、锚点默认居中（`cx=128`）时，
 * 右空间 = 左空间 = `128 - gap - pad = 116`。故 `bubbleWidth ≤ 116` 恒命中 `side='right'`；
 * `bubbleWidth > 116` 两侧皆不足 → 恒命中 `side='clamp'`；`side='left'` 在该几何下**不可达**。
 * 这是**窗口几何限制**（扩窗属窗口层 T-02/S7，设计 §9-2 已挂起），非摆位逻辑缺陷。
 * 宽容器 + 锚点靠右时三态才全部可达（见单测）。
 */
export function resolveBubblePlacement(input: PlacementInput): BubblePlacement {
  const {
    containerWidth: wc,
    bubbleWidth: mw,
    bubbleHeight: mh,
    anchorCx: cx,
    anchorTop: at,
    gap,
    pad,
    tailH,
  } = input;

  // 纵向：顶点越上边界即向下钳制。
  const top = Math.max(pad, at - mh - tailH);

  const left0 = cx + gap;
  if (left0 + mw <= wc - pad) {
    return { side: 'right', left: left0, top };
  }
  const leftFlipped = cx - gap - mw;
  if (leftFlipped >= pad) {
    return { side: 'left', left: leftFlipped, top };
  }
  // 两侧都不足 → 钳制（气泡宽超容器时 maxLeft 退化为 pad，left 不退化为负值）。
  const maxLeft = Math.max(pad, wc - mw - pad);
  return { side: 'clamp', left: clamp(left0, pad, maxLeft), top };
}

/**
 * 气泡淡入淡出透明度（设计 §4.2，双侧曲线）。
 *
 * - `elapsedMs <= 0 || elapsedMs >= dwellMs` → `0`（未开始 / 已到期，调用方据此隐藏）；
 * - 否则 `alpha = clamp(min(1, elapsedMs/fadeMs, (dwellMs-elapsedMs)/fadeMs), 0, 1)`；
 * - `fadeMs <= 0` → 退化为 `1`（防除零，无过渡）。
 *
 * @param elapsedMs 已显示时长（`now - shownAt`）
 * @param dwellMs   停留时长
 * @param fadeMs    单侧淡入淡出时长
 */
export function fadeAlpha(elapsedMs: number, dwellMs: number, fadeMs: number): number {
  if (
    !Number.isFinite(elapsedMs) ||
    !Number.isFinite(dwellMs) ||
    elapsedMs <= 0 ||
    elapsedMs >= dwellMs
  ) {
    return 0;
  }
  if (!Number.isFinite(fadeMs) || fadeMs <= 0) {
    return 1;
  }
  const a = Math.min(elapsedMs / fadeMs, (dwellMs - elapsedMs) / fadeMs);
  return clamp(a, 0, 1);
}
