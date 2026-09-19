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

/** 固定交叉淡入时长（毫秒，S2-M2 卡片口径：本阶段恒 150ms）。 */
export const CROSSFADE_MS = 150;

/**
 * S9-M1：交叉淡入时长区间（毫秒，`01 §6.15` / `animation.json fadeMs{min,max}`）。
 * 每次动作切换在区间内取一值（默认 150~250），配合缓动曲线，避免逐次等长的机械感。
 */
export const FADE_MS_MIN = 150;
export const FADE_MS_MAX = 250;

/** 淡入进度缓动曲线（`t ∈ [0,1)` → 新帧加权 `[0,1)`，纯函数）。 */
export type CrossfadeEasing = (t: number) => number;

/** 线性缓动（S2-M2 既有口径，默认值，保持既有单测行为不变）。 */
export const linearEasing: CrossfadeEasing = (t) => t;

/** easeInOutCubic（`animation.json easing.default`；S9-M1 起交叉淡入默认缓动）。 */
export const easeInOutCubic: CrossfadeEasing = (t) =>
  t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;

/**
 * 在 `[min,max]` 内为本次动作切换选一个交叉淡入时长（纯函数，确定性可测）。
 *
 * `rng` 返回 `[0,1)`；越界/非有限钳到区间。默认 150~250ms（S9-M1 升档）。
 */
export function pickFadeMs(
  rng: () => number,
  min: number = FADE_MS_MIN,
  max: number = FADE_MS_MAX,
): number {
  if (!Number.isFinite(min) || !Number.isFinite(max) || max <= min) {
    return Number.isFinite(min) ? min : CROSSFADE_MS;
  }
  const u = Number.isFinite(rng()) ? Math.min(1, Math.max(0, rng())) : 0;
  return Math.round(min + u * (max - min));
}

/** 淡入期间新旧两帧的不透明度组合（互补，`prevAlpha + nextAlpha === 1`）。 */
export interface CrossfadeAlphas {
  /** 旧动作末帧 alpha（随时间衰减至 0）。 */
  readonly prevAlpha: number;
  /** 新动作首帧 alpha（随时间上升至 1）。 */
  readonly nextAlpha: number;
}

/**
 * 计算淡入开始后 `elapsedMs` 时刻的新旧帧 alpha（纯函数，vitest 单测覆盖）。
 *
 * - `elapsedMs <= 0` → 旧帧 1.0 / 新帧 0.0（淡入起点）；
 * - `0 < elapsedMs < fadeMs` → 按 `easing` 取新帧加权（默认线性互补）；
 * - `elapsedMs >= fadeMs` → `null`（淡入结束，调用方回落单帧绘制）；
 * - `fadeMs <= 0` → 恒 `null`（无过渡，直接硬切）。
 *
 * S9-M1：新增可选 `easing`（默认 [`linearEasing`]，与 S2-M2 既有行为完全一致；
 * 调用方传 [`easeInOutCubic`] 即升级为缓动交叉淡入）。
 */
export function crossfadeAlphas(
  elapsedMs: number,
  fadeMs: number = CROSSFADE_MS,
  easing: CrossfadeEasing = linearEasing,
): CrossfadeAlphas | null {
  if (!Number.isFinite(elapsedMs) || !Number.isFinite(fadeMs) || fadeMs <= 0) {
    return null;
  }
  const elapsed = Math.max(0, elapsedMs);
  if (elapsed >= fadeMs) {
    return null;
  }
  const t = elapsed / fadeMs;
  const nextAlpha = Number.isFinite(easing(t)) ? easing(t) : t;
  return { prevAlpha: 1 - nextAlpha, nextAlpha };
}
