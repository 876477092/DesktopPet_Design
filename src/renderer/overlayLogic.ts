/**
 * 叠加层纯函数（S3-M5，T-10 段 · 上）。
 *
 * 职责（设计 §3 Q5/Q6）：把叠加层的**时序判定**抽为无 `this`、无 DOM、无系统时钟的
 * 可导出纯函数，供 `OverlayLayer` 消费并直接单测：
 *   - 情绪原因卡长按阈值（`01 §6.11.9` / `02 §5 K-6`：500ms）→ `isReasonCardLongPress`；
 *   - 成就飘条进度（`01 §6.12.6`）→ `toastProgress`；
 *   - 飘条等待队列上限（FIFO、同屏 1）→ `clampToastQueue`；
 *   - Zzz 档位规范化（`01 §6.5.5`：0/1/2，非法值钳制）→ `normalizeSleepLevel`。
 *
 * 本卡边界：不做情绪数值结算（回退 50% 由上游 CoaxFlow 算，S4-M3）、道歉状态机、
 * 小面板本体；原因卡入口经回调交出（零新增事件，C8）。
 */

import { REASON_CARD_LONG_PRESS_MS } from './layerPorts';

/**
 * 情绪原因卡长按判定：`elapsedMs >= thresholdMs` 即成立（`02 §5 K-6` 默认 500ms）。
 *
 * 边界：恰为阈值 → 成立；非有限输入 → 不成立。
 */
export function isReasonCardLongPress(
  elapsedMs: number,
  thresholdMs: number = REASON_CARD_LONG_PRESS_MS,
): boolean {
  if (!Number.isFinite(elapsedMs) || !Number.isFinite(thresholdMs)) {
    return false;
  }
  return elapsedMs >= thresholdMs;
}

/**
 * 成就飘条进度（0~1）：`clamp(elapsedMs / toastMs, 0, 1)`。
 *
 * `toastMs <= 0` 或非有限 → `1`（即到即走，防除零）；`elapsedMs` 非有限 → `0`。
 */
export function toastProgress(elapsedMs: number, toastMs: number): number {
  if (!Number.isFinite(toastMs) || toastMs <= 0) {
    return 1;
  }
  if (!Number.isFinite(elapsedMs) || elapsedMs <= 0) {
    return 0;
  }
  return Math.min(1, elapsedMs / toastMs);
}

/**
 * 飘条等待队列钳制：FIFO 保留**最先入队的前 `cap` 条**，超出上限的新条目由调用方
 * 丢弃并告警（不静默，`02 §7.4`）。
 *
 * `cap <= 0` 或非有限 → 空队列。
 */
export function clampToastQueue<T>(queue: readonly T[], cap: number): readonly T[] {
  if (!Number.isFinite(cap) || cap <= 0) {
    return [];
  }
  const max = Math.trunc(cap);
  return queue.length <= max ? queue.slice() : queue.slice(0, max);
}

/** Zzz 档位规范化结果（`invalid` 供调用方告警，不静默）。 */
export interface SleepLevelNormalization {
  /** 钳制后的合法档位（0 隐藏 / 1 普通 / 2 放大）。 */
  readonly level: 0 | 1 | 2;
  /** 入参是否为非法档（非 0/1/2 的任何值，含 NaN / 负数 / 小数 / 越界）。 */
  readonly invalid: boolean;
}

/**
 * Zzz 档位规范化（`01 §6.5.5`）：恰为 `0|1|2` 原样通过；其余数值四舍五入后钳到
 * `[0,2]`（NaN / 非数值 → `0`），并标记 `invalid` 由调用方 `console.warn`。
 */
export function normalizeSleepLevel(value: unknown): SleepLevelNormalization {
  if (value === 0 || value === 1 || value === 2) {
    return { level: value, invalid: false };
  }
  const numeric = typeof value === 'number' && Number.isFinite(value) ? Math.round(value) : 0;
  const level = Math.min(2, Math.max(0, numeric)) as 0 | 1 | 2;
  return { level, invalid: true };
}
