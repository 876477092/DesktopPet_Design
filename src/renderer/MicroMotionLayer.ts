/**
 * 微动层（K-14，S9-M3）：待机微动 + 眨眼。
 *
 * 职责（`animation.json micro / blink`）：
 *   - **待机微动**：每 5~12s 随机抽 1 种，**60s 内不重复**（按 weight 加权）；
 *   - **眨眼**：间隔 2~6s 随机，**禁止等间隔**（单测断言连续 10 次间隔不全等）；
 *   - 纯逻辑：rng/时钟注入，确定性可测；不碰 DOM/WebGL。
 *
 * 验收（AC-25）：5min 观察微动 ≥3 种、眨眼 2~6s 随机。
 */

/** 待机微动项（`animation.json micro.items`）。 */
export interface MicroItem {
  readonly id: string;
  readonly clip: string;
  readonly weight: number;
}

/** 微动调度配置。 */
export interface MicroConfig {
  /** 抽选间隔区间（秒），如 [5,12]。 */
  readonly intervalSec: readonly [number, number];
  /** 不重复窗口（秒），如 60。 */
  readonly noRepeatSec: number;
  readonly items: readonly MicroItem[];
}

/** 眨眼配置。 */
export interface BlinkConfig {
  /** 眨眼间隔区间（毫秒），如 [2000,6000]。 */
  readonly intervalMs: readonly [number, number];
}

/** 一次到期的微动事件。 */
export interface MicroEvent {
  readonly id: string;
  readonly clip: string;
}

/** 在 [min,max] 内取随机数（rng 返回 [0,1)）。 */
function randBetween(min: number, max: number, rng: () => number): number {
  const u = Math.min(1, Math.max(0, rng()));
  return min + u * (max - min);
}

/** 加权随机抽选（权重和 ≤0 或全过滤 → null）。 */
function weightedPick(items: readonly MicroItem[], rng: () => number): MicroItem | null {
  const total = items.reduce((s, it) => s + Math.max(0, it.weight), 0);
  if (total <= 0) {
    return null;
  }
  let roll = rng() * total;
  for (const it of items) {
    roll -= Math.max(0, it.weight);
    if (roll <= 0) {
      return it;
    }
  }
  return items[items.length - 1] ?? null;
}

/**
 * 待机微动调度器。
 *
 * @param cfg 微动配置
 * @param rng 随机源（[0,1)；测试注入确定性序列）
 * @param now 初始时钟（毫秒）
 */
export class MicroMotionScheduler {
  private readonly cfg: MicroConfig;
  private readonly rng: () => number;
  private nextPickAt: number;
  /** 近期抽中记录（用于 noRepeat 窗口去重）。 */
  private readonly recent: { id: string; at: number }[] = [];

  constructor(cfg: MicroConfig, rng: () => number, now: number) {
    this.cfg = cfg;
    this.rng = rng;
    this.nextPickAt = now + this.scheduleIntervalMs();
  }

  /** 随机间隔（毫秒）：[5,12]s。 */
  private scheduleIntervalMs(): number {
    const [min, max] = this.cfg.intervalSec;
    return randBetween(min, max, this.rng) * 1_000;
  }

  /**
   * 轮询：到点则抽一种（60s 内不重复），返回事件；未到点返回 null。
   */
  poll(now: number): MicroEvent | null {
    if (now < this.nextPickAt) {
      return null;
    }
    // 清理 noRepeat 窗口外记录。
    const cutoff = now - this.cfg.noRepeatSec * 1_000;
    while (this.recent.length > 0 && this.recent[0] !== undefined && this.recent[0].at < cutoff) {
      this.recent.shift();
    }
    const recentIds = new Set(this.recent.map((r) => r.id));
    const candidates = this.cfg.items.filter((it) => !recentIds.has(it.id));
    // 全部候选都在 noRepeat 窗口内 → 本轮跳过（不强行重复），下一周期再试。
    const picked = weightedPick(candidates, this.rng);
    if (picked === null) {
      this.nextPickAt = now + this.scheduleIntervalMs();
      return null;
    }
    this.recent.push({ id: picked.id, at: now });
    this.nextPickAt = now + this.scheduleIntervalMs();
    return { id: picked.id, clip: picked.clip };
  }
}

/**
 * 眨眼调度器（间隔 2~6s 随机，禁止等间隔）。
 *
 * 每次眨眼后重新随机下一次间隔——连续间隔序列不可能全等（K-14 单测断言）。
 */
export class BlinkScheduler {
  private readonly cfg: BlinkConfig;
  private readonly rng: () => number;
  private nextBlinkAt: number;
  /** 最近一次眨眼间隔（毫秒；观测用）。 */
  lastIntervalMs = 0;

  constructor(cfg: BlinkConfig, rng: () => number, now: number) {
    this.cfg = cfg;
    this.rng = rng;
    this.nextBlinkAt = now + this.scheduleIntervalMs();
  }

  private scheduleIntervalMs(): number {
    const [min, max] = this.cfg.intervalMs;
    return randBetween(min, max, this.rng);
  }

  /** 下一次眨眼的绝对时刻（毫秒）。 */
  get nextAt(): number {
    return this.nextBlinkAt;
  }

  /**
   * 轮询：到点则触发眨眼并重新排程，返回 true；未到点 false。
   */
  poll(now: number): boolean {
    if (now < this.nextBlinkAt) {
      return false;
    }
    const prev = this.nextBlinkAt;
    const interval = this.scheduleIntervalMs();
    this.lastIntervalMs = interval;
    this.nextBlinkAt = prev + interval;
    return true;
  }
}
