/**
 * 交叉淡入纯函数（S2-M2，卡片要点 1 / K-14 FrameRenderer）。
 *
 * 口径（卡片明确）：本阶段交叉淡入 **固定 150ms 线性**——不读取 animation.json
 * 的 fadeMs / floating easing（150~250ms 缓动升级归 S9-M3）。动作切换时旧动作
 * 末帧与新动作首帧双绘，alpha 线性互补（和恒为 1）。
 *
 * 时间来源：由调用方注入单调时钟读数（FrameRenderer 注入 `performance.now`，
 * C3 边界：仅用于本地过渡时长测量，不落业务计时、不读墙钟）。
 */

/** 固定交叉淡入时长（毫秒，卡片口径：本阶段恒 150ms）。 */
export const CROSSFADE_MS = 150;

/** 淡入期间新旧两帧的不透明度组合（线性互补，`prevAlpha + nextAlpha === 1`）。 */
export interface CrossfadeAlphas {
  /** 旧动作末帧 alpha（随时间线性衰减至 0）。 */
  readonly prevAlpha: number;
  /** 新动作首帧 alpha（随时间线性上升至 1）。 */
  readonly nextAlpha: number;
}

/**
 * 计算淡入开始后 `elapsedMs` 时刻的新旧帧 alpha（纯函数，vitest 单测覆盖）。
 *
 * - `elapsedMs <= 0` → 旧帧 1.0 / 新帧 0.0（淡入起点）；
 * - `0 < elapsedMs < fadeMs` → 线性互补（如 75ms → 0.5 / 0.5）；
 * - `elapsedMs >= fadeMs` → `null`（淡入结束，调用方回落单帧绘制）；
 * - `fadeMs <= 0` → 恒 `null`（无过渡，直接硬切）。
 */
export function crossfadeAlphas(elapsedMs: number, fadeMs: number = CROSSFADE_MS): CrossfadeAlphas | null {
  if (!Number.isFinite(elapsedMs) || !Number.isFinite(fadeMs) || fadeMs <= 0) {
    return null;
  }
  const elapsed = Math.max(0, elapsedMs);
  if (elapsed >= fadeMs) {
    return null;
  }
  const nextAlpha = elapsed / fadeMs;
  return { prevAlpha: 1 - nextAlpha, nextAlpha };
}
