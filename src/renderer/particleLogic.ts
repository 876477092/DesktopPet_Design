/**
 * 粒子层纯逻辑（S3-M6，T-10 段 · 下；`02 §3` ParticleLayer / `01 附录`）。
 *
 * 职责：迸发生成（确定性 LCG 种子随机）→ 逐帧推进（位置 = 初速 × 年龄 + 重力
 * 位移，寿命到期剔除）→ 渲染快照。全部纯函数 + 注入时钟（C3：`nowMs` 由
 * `ParticleLayer` 注入单调读数，本模块零时钟零 setTimeout）。
 *
 * 规格出处：五类 `heart/star/dust/tear/anger`（`01 附录` / `02 §3`）；单次上限
 * 60（`02 §5.22` ParticlesCfg）；寿命 1.2s（同上）；爱心色 `#FF8FB1`（样式层）。
 * 速度/重力为表现层自由参数（文档未冻结数值），按类别给默认档。
 */

import type { ParticleCmdV1, ParticleKind } from '../shared/ipc';
import { PARTICLE_BURST_CAP } from '../shared/ipc';
import { PARTICLE_LIFETIME_MS, type ParticleRenderItem } from './layerPorts';

/** 单颗粒子（层内活体；`x0/y0` 为迸发锚点，`vx/vy` 初速 px/s，`gravity` px/s²）。 */
export interface Particle {
  /** 生命周期内唯一 id（DOM 差量收敛键）。 */
  readonly id: number;
  readonly kind: ParticleKind;
  /** 迸发锚点 X（CSS px）。 */
  readonly x0: number;
  /** 迸发锚点 Y（CSS px）。 */
  readonly y0: number;
  /** 初速 X（px/s）。 */
  readonly vx: number;
  /** 初速 Y（px/s；向下为正）。 */
  readonly vy: number;
  /** 重力加速度（px/s²；仅 dust 下坠使用，其余 0）。 */
  readonly gravity: number;
  /** 迸发时刻（注入单调毫秒）。 */
  readonly bornAt: number;
}

/** 迸发锚点（CSS px，相对 `#pet-overlay-root`）。 */
export interface ParticleAnchor {
  readonly x: number;
  readonly y: number;
}

/**
 * 迸发数量钳制：非有限 → 1；截断整数后钳 `[1, 60]`（`02 §5.22` maxPerBurst）。
 */
export function clampBurstCount(count: number): number {
  if (!Number.isFinite(count)) {
    return 1;
  }
  return Math.min(PARTICLE_BURST_CAP, Math.max(1, Math.trunc(count)));
}

/**
 * 确定性 LCG 随机源（`[0,1)`；种子由层内注入——默认取迸发时刻单调毫秒，
 * 测试注入固定 now 即得完全确定的粒子序列，无 `Math.random` 不可测性）。
 */
export function createRng(seed: number): () => number {
  // 32 位 LCG（Numerical Recipes 常数）；种子 0 先混一步避免全零退化。
  let state = (Math.trunc(seed) ^ 0x9e3779b9) >>> 0;
  return (): number => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 4294967296;
  };
}

/** 区间取值（含下界不含上界；rng 单调可控故测试可复现）。 */
function span(rng: () => number, min: number, max: number): number {
  return min + rng() * (max - min);
}

/**
 * 按类别生成一颗粒子的运动参数（表现层默认档；文档未冻结数值）。
 *
 * - `heart`：向上漂浮（爱心迸发，比心/抚摸/戳痒）；
 * - `star`：全向星散；
 * - `dust`：横向溅起 + 重力回落（甩出落地尘土）；
 * - `tear`：小幅度下落（泪滴，本阶段触发映射预留）；
 * - `anger`：快速全向迸发（怒气，本阶段触发映射预留）。
 */
function velocityFor(kind: ParticleKind, rng: () => number): Pick<Particle, 'vx' | 'vy' | 'gravity'> {
  switch (kind) {
    case 'heart': {
      return { vx: span(rng, -20, 20), vy: -span(rng, 40, 90), gravity: 0 };
    }
    case 'star': {
      const angle = rng() * Math.PI * 2;
      const speed = span(rng, 60, 140);
      return { vx: Math.cos(angle) * speed, vy: Math.sin(angle) * speed * 0.8, gravity: 0 };
    }
    case 'dust': {
      const angle = rng() * Math.PI; // 上半圆溅起，重力拉回
      const speed = span(rng, 40, 120);
      return { vx: Math.cos(angle) * speed, vy: -Math.abs(Math.sin(angle)) * speed * 0.6, gravity: 220 };
    }
    case 'tear': {
      return { vx: span(rng, -10, 10), vy: span(rng, 30, 60), gravity: 0 };
    }
    case 'anger': {
      const angle = rng() * Math.PI * 2;
      const speed = span(rng, 100, 180);
      return { vx: Math.cos(angle) * speed, vy: Math.sin(angle) * speed, gravity: 0 };
    }
  }
}

/**
 * 生成一次迸发（`count` 已由调用方经 `clampBurstCount` 钳制；本函数不再钳，
 * 保持纯函数单一职责）。
 */
export function spawnBurst(
  cmd: Pick<ParticleCmdV1, 'kind' | 'count'>,
  anchor: ParticleAnchor,
  nowMs: number,
  rng: () => number,
  firstId: number,
): Particle[] {
  const out: Particle[] = [];
  for (let i = 0; i < cmd.count; i += 1) {
    const v = velocityFor(cmd.kind, rng);
    out.push({
      id: firstId + i,
      kind: cmd.kind,
      x0: anchor.x,
      y0: anchor.y,
      vx: v.vx,
      vy: v.vy,
      gravity: v.gravity,
      bornAt: nowMs,
    });
  }
  return out;
}

/** 剔除寿命到期粒子（惰性回收：由帧 tick 驱动的 flush 调用，无定时器）。 */
export function advanceParticles(list: readonly Particle[], nowMs: number): Particle[] {
  return list.filter((p) => nowMs - p.bornAt < PARTICLE_LIFETIME_MS);
}

/** 年龄秒（非有限/负值防御为 0）。 */
function ageSec(p: Particle, nowMs: number): number {
  const ms = nowMs - p.bornAt;
  if (!Number.isFinite(ms) || ms <= 0) {
    return 0;
  }
  return ms / 1000;
}

/** 当前渲染快照（位置含重力二次项；透明度 1→0 线性衰减）。 */
export function renderItems(
  list: readonly Particle[],
  nowMs: number,
): ParticleRenderItem[] {
  return list.map((p) => {
    const t = ageSec(p, nowMs);
    const x = p.x0 + p.vx * t;
    const y = p.y0 + p.vy * t + 0.5 * p.gravity * t * t;
    const life = t * 1000 / PARTICLE_LIFETIME_MS;
    const opacity = Math.min(1, Math.max(0, 1 - life));
    return { id: p.id, kind: p.kind, x, y, opacity };
  });
}
