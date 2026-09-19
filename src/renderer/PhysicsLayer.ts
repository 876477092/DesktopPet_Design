/**
 * 物理次级动画层（K-14，S9-M3）。
 *
 * 职责：8 个次级部件（尾巴/耳朵/衣摆等）的弹簧链物理——
 *   - **固定 8ms 子步**：`step` 把可变 `dt` 切成整数个 8ms 子步（残差累积到下一帧），
 *     保证稳定性与「快速甩动不撕裂」（K-14）；
 *   - 每子步 Verlet 积分 + 距离约束投影，相邻部件段长偏离 rest 长度 ≤ ε（无撕裂判据）；
 *   - `physics.level=primaryOnly`（`animation.json` 降级档）：次级部件冻结在 rest 位，
 *     只保留主锚点运动（低端机降级，`02 §7.4`）。
 *
 * 纯逻辑：无 DOM/WebGL/时钟副作用；位置为逻辑坐标，rng/时钟注入，单测可确定性复现。
 */

/** 物理档级（`animation.json physics.level`）。 */
export type PhysicsLevel = 'full' | 'primaryOnly';

/** 单个次级部件（Verlet：当前位 + 上一位）。 */
export interface PhysicsPart {
  readonly id: string;
  x: number;
  y: number;
  /** 上一子步位置（Verlet 积分用）。 */
  px: number;
  py: number;
  /** 与上一部件（或锚点）的静止段长。 */
  restLen: number;
}

/** 固定子步（毫秒，K-14：8ms）。 */
export const PHYSICS_SUBSTEP_MS = 8;

/** 阻尼（Verlet：速度保留系数；<1 抑制振荡发散）。 */
const DAMPING = 0.9;

/** 距离约束投影迭代次数（迭代越多越不撕裂；8 部件链需充分收敛）。 */
const CONSTRAINT_ITERATIONS = 16;

/**
 * 弹簧链物理层。
 *
 * @param parts  8 个部件（部件 0 跟随锚点，其余依次成链）
 * @param level  降级档（primaryOnly 时次级部件冻结）
 * @param subStepMs 固定子步（默认 8ms）
 */
export class PhysicsLayer {
  private readonly parts: PhysicsPart[];
  private level: PhysicsLevel;
  private readonly subStepMs: number;
  /** 残差时间（不足一个子步的部分累积到下一帧）。 */
  private accumulatorMs = 0;
  /** 重力/下垂外力（逻辑单位/子步²；微妙甩动下垂，避免能量积聚）。 */
  private gravity = 0.05;

  constructor(parts: PhysicsPart[], level: PhysicsLevel = 'full', subStepMs: number = PHYSICS_SUBSTEP_MS) {
    this.parts = parts.map((p) => ({ ...p }));
    this.level = level;
    this.subStepMs = subStepMs > 0 ? subStepMs : PHYSICS_SUBSTEP_MS;
  }

  /** 当前物理档级。 */
  get currentLevel(): PhysicsLevel {
    return this.level;
  }

  /** 切档（运行时降级/恢复）。 */
  setLevel(level: PhysicsLevel): void {
    this.level = level;
  }

  /** 部件快照（渲染层读取；返回拷贝，外部改不到内部状态）。 */
  partsSnapshot(): ReadonlyArray<{ id: string; x: number; y: number }> {
    return this.parts.map((p) => ({ id: p.id, x: p.x, y: p.y }));
  }

  /**
   * 推进 `dtMs`：切成整数个固定子步，残差累积。
   *
   * @param dtMs     帧间隔（毫秒）
   * @param anchor   主锚点位置（部件 0 被约束跟随）
   */
  step(dtMs: number, anchor: { x: number; y: number }): void {
    if (!Number.isFinite(dtMs) || dtMs <= 0) {
      return;
    }
    if (this.level === 'primaryOnly') {
      // 降级：次级部件直接对齐锚点（冻结），不积分。
      for (const p of this.parts) {
        p.x = anchor.x;
        p.y = anchor.y;
        p.px = anchor.x;
        p.py = anchor.y;
      }
      return;
    }

    this.accumulatorMs += dtMs;
    let steps = Math.floor(this.accumulatorMs / this.subStepMs);
    // 防失控：单帧最多 250ms 等效（约 31 子步），丢弃过量累积。
    if (steps > 31) {
      steps = 31;
    }
    this.accumulatorMs -= steps * this.subStepMs;

    for (let i = 0; i < steps; i += 1) {
      this.substep(anchor);
    }
  }

  /** 单个固定子步：Verlet 积分 + 距离约束。 */
  private substep(anchor: { x: number; y: number }): void {
    // 部件 0 钉在锚点（主锚点运动直接驱动链）。
    const root = this.parts[0];
    if (root === undefined) {
      return;
    }
    root.x = anchor.x;
    root.y = anchor.y;
    root.px = anchor.x;
    root.py = anchor.y;

    // Verlet 积分部件 1..n：x' = x + (x - px)*damping + gravity。
    for (let i = 1; i < this.parts.length; i += 1) {
      const p = this.parts[i];
      if (p === undefined) {
        continue;
      }
      const vx = (p.x - p.px) * DAMPING;
      const vy = (p.y - p.py) * DAMPING;
      p.px = p.x;
      p.py = p.y;
      p.x += vx;
      p.y += vy + this.gravity;
    }

    // 距离约束投影（迭代）：保持相邻段长 = restLen，消除撕裂。
    for (let iter = 0; iter < CONSTRAINT_ITERATIONS; iter += 1) {
      this.projectConstraints();
    }
  }

  /** 距离约束：相邻部件拉回 restLen（部件 0 钉锚点）。 */
  private projectConstraints(): void {
    for (let i = 0; i < this.parts.length - 1; i += 1) {
      const a = this.parts[i];
      const b = this.parts[i + 1];
      if (a === undefined || b === undefined) {
        continue;
      }
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const dist = Math.hypot(dx, dy) || 1e-6;
      const diff = (dist - b.restLen) / dist;
      // 部件 a（i=0 是锚点）钉住：b 移动 100%；其余按 1/2 分配。
      const bWeight = i === 0 ? 1 : 0.5;
      b.x -= dx * diff * bWeight;
      b.y -= dy * diff * bWeight;
      if (i !== 0) {
        a.x += dx * diff * (1 - bWeight);
        a.y += dy * diff * (1 - bWeight);
      }
    }
  }

  /**
   * 当前最大段长偏离比例（无撕裂判据；K-14：快速甩动后仍 ≤ 5%）。
   */
  maxStretchRatio(): number {
    let max = 0;
    for (let i = 0; i < this.parts.length - 1; i += 1) {
      const a = this.parts[i];
      const b = this.parts[i + 1];
      if (a === undefined || b === undefined) {
        continue;
      }
      const dist = Math.hypot(b.x - a.x, b.y - a.y);
      const rest = b.restLen || 1e-6;
      max = Math.max(max, Math.abs(dist - rest) / rest);
    }
    return max;
  }
}

/**
 * 由配置构建一条 8 部件链（`animation.json physics.parts`；缺省占位 8 段）。
 *
 * @param anchor 锚点（角色中心）
 * @param segLen 每段静止长度（逻辑单位）
 */
export function buildDefaultChain(
  anchor: { x: number; y: number },
  segLen = 6,
): PhysicsPart[] {
  const ids = ['tail_0', 'tail_1', 'tail_2', 'ear_l', 'ear_r', 'hem_l', 'hem_r', 'scarf'];
  return ids.map((id, i) => ({
    id,
    x: anchor.x,
    y: anchor.y - i * segLen,
    px: anchor.x,
    py: anchor.y - i * segLen,
    restLen: segLen,
  }));
}
