/**
 * 粒子层状态机（S3-M6，T-10 段 · 下；`02 §3` ParticleLayer / `03 §2 S3-M6`）。
 *
 * 职责：消费 `pet://fx` 解析后的 `ParticleCmdV1`——迸发（钳上限 60）→ 逐帧推进
 * → 经极窄端口 `ParticleView` 落 DOM。运动/寿命逻辑在 `particleLogic.ts`
 * （纯函数直测）；本层只做状态编排。
 *
 * ⚠️ **本层禁 `setTimeout`**：粒子寿命到期不靠定时器，靠 `flush()` 被
 * `LayerHost.render()`（Rust 帧 tick）驱动。**唯一时间真源 = 构造注入的
 * `now()`**（默认 `performance.now`；禁 `Date.now` / 墙钟，C3）。
 *
 * 惰性 flush（S3-M5 范式延续）：
 *   - 无活粒子且无新迸发（无脏）→ `flush()` 立即返回（零 view 写入，60Hz 廉价）；
 *   - 有活粒子 → 每 tick `renderItems` 整体收敛 view（粒子 ≤60，规模可控）；
 *   - 全灭 → 恰好写一次空列表清场，随后回到「无脏早退」态（幂等，无脏早退）。
 *
 * 绘制序：particle 层在 character 与 overlay 之间（`LAYER_DRAW_ORDER` 冻结口径），
 * 天然不遮挡眼睛与文字气泡（`02 §6.5`）。
 */

import type { ParticleCmdV1 } from '../shared/ipc';
import {
  PARTICLE_ANCHOR_OFFSET_X,
  PARTICLE_ANCHOR_TOP_DEFAULT,
  type ParticleView,
} from './layerPorts';
import {
  advanceParticles,
  clampBurstCount,
  createRng,
  renderItems,
  spawnBurst,
  type Particle,
  type ParticleAnchor,
} from './particleLogic';

/** 构造选项（全部可选；测试注入 Fake 时钟 / 可控锚点 / 固定种子）。 */
export interface ParticleLayerOptions {
  /** 单调时钟读数（默认 `performance.now`；C3：仅本地运动推进，不读墙钟）。 */
  now?: () => number;
  /** 迸发锚点（默认容器中线偏右 + 头顶占位高度；待真机标定，见设计 §9-1）。 */
  anchor?: () => ParticleAnchor;
  /** LCG 种子（默认取迸发时刻单调毫秒；测试注入固定值求确定性）。 */
  seed?: () => number;
}

/**
 * 粒子层（迸发 + 帧驱动推进 + 惰性 flush）。
 *
 * @param view 极窄端口（真实 `DomParticleView` / 测试 `FakeParticleView`）
 * @param opts 构造选项（见 [`ParticleLayerOptions`]）
 */
export class ParticleLayer {
  private readonly now: () => number;
  private readonly anchor: () => ParticleAnchor;
  private readonly seed: () => number;

  /** 活粒子（迸发入列、到期由 flush 剔除）。 */
  private particles: Particle[] = [];
  /** 下一个粒子 id（跨迸发自增，保 DOM 收敛键唯一）。 */
  private nextId = 1;
  /** 脏标记：有新迸发或仍有活粒子（flush 需要写 view）。 */
  private dirty = false;

  constructor(
    private readonly view: ParticleView,
    opts: ParticleLayerOptions = {},
  ) {
    this.now = opts.now ?? ((): number => performance.now());
    this.anchor =
      opts.anchor ??
      ((): ParticleAnchor => ({
        x: this.viewAnchorX() + PARTICLE_ANCHOR_OFFSET_X,
        y: PARTICLE_ANCHOR_TOP_DEFAULT,
      }));
    this.seed = opts.seed ?? ((): number => this.now());
  }

  /** 默认锚点的横向基线（容器中线；经 view 不可得时取 128 逻辑宽中线）。 */
  private viewAnchorX(): number {
    // ParticleView 端口不暴露容器尺寸（粒子锚点无需精确测量）；取渲染约定宽。
    // 128 逻辑宽 ×2 导出 = 256 CSS px 的中线；真机标定后经 anchor 选项覆盖。
    return 128;
  }

  /**
   * 消费一条粒子命令（`pet://fx` 解析后的入口）。
   *
   * 数量经 `clampBurstCount` 钳 `[1, 60]`；只置内部状态 + 置脏，**不直接写 view**
   * （DOM 写入延后到 `flush()`，守「draw=惰性 flush」）。
   */
  submit(cmd: ParticleCmdV1): void {
    const now = this.now();
    const count = clampBurstCount(cmd.count);
    const burst = spawnBurst(
      { kind: cmd.kind, count },
      this.anchor(),
      now,
      createRng(this.seed()),
      this.nextId,
    );
    this.nextId += count;
    this.particles = this.particles.concat(burst);
    this.dirty = true;
  }

  /**
   * 惰性 flush（`LayerHost` draw 回调；每帧调用须廉价且幂等）。
   *
   * - 无活粒子且无脏 → 立即返回（零 view 写入）；
   * - 到期剔除（寿命 1.2s，帧 tick 驱动）→ 全灭时恰好写一次空列表清场；
   * - 有活粒子 → 以渲染快照整体收敛 view。
   */
  flush(): void {
    if (this.particles.length === 0 && !this.dirty) {
      return; // 无脏早退（60Hz 廉价）
    }
    const now = this.now();
    this.particles = advanceParticles(this.particles, now);
    this.view.render(renderItems(this.particles, now));
    // 仍有活粒子 → 保持脏（逐帧推进）；全灭 → 清场完成后回早退态。
    this.dirty = this.particles.length > 0;
  }

  /** 当前活粒子数（诊断视图）。 */
  liveCount(): number {
    return this.particles.length;
  }
}
