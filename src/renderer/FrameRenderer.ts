/**
 * 帧动画渲染器（`02 §5 K-4` / K-14 `FrameRenderer`，S2-M2 更新）。
 *
 * 职责：**被动推帧绘制**——消费 `pet://frame` 事件的 `RenderFrameCmd` v1：
 *   取帧（AtlasCache）→ 子矩形（atlas.json 布局）→ 镜像/alpha → 交给
 *   `WebGLStage` 合成。**无 rAF 自循环**（K-4：渲染 tick 由 Rust 侧驱动）。
 *
 * S2-M2 增量（卡片要点 1 / B10）：
 *   - **150ms 交叉淡入**（固定线性，不读 animation.json 的 fadeMs/easing，
 *     S9-M3 才升级 150~250ms 缓动）：动作切换时维护「上一就绪帧 + 首个新帧
 *     到达时刻」，150ms 内新旧帧双绘、alpha 线性互补（[`crossfadeAlphas`]）；
 *     v1 载荷保持单帧（C8 冻结），过渡所需的两份信息均由前端自持；
 *   - **B10 驱逐守卫**：订阅 `AtlasCache.onEvict`，位图被 LRU `close()` 后立即
 *     失效对应就绪帧；paint 前再校验位图有效性（width/height>0）兜底。
 *
 * 边界（S2-M2 卡片）：
 *   - 不做仲裁器（S2-M3）；`mirror` 只按载荷透传给舞台（规则在 Rust 播放器）；
 *   - 图集缺失按 `02 §7.4` 降级：告警 + 跳帧。
 *
 * 并发纪律：`draw` 为异步路径（位图首次加载），采用 **latest-wins**——绘制进行中
 * 到达的新帧覆盖暂存帧，完成后只绘制最新一帧，杜绝乱序撕裂与硬切回退。
 */

import type { RenderFrameCmdV1 } from '../shared/ipc';
import { computeFrameRect, type FrameSubRect } from './AtlasCache';
import { CROSSFADE_MS, crossfadeAlphas, linearEasing, type CrossfadeEasing } from './crossfade';
import type { DrawOptions } from './WebGLStage';
import type { ICharacterRenderer } from './ICharacterRenderer';

/** 渲染后端种类标识（K-14 `ICharacterRenderer.kind` 的帧回退实现取值）。 */
export const RENDERER_KIND_FRAME = 'frame';

/** 渲染舞台最小接口（真实 `WebGLStage` 结构满足；测试注入替身）。 */
export interface FrameStage {
  /** 同 [`WebGLStage.drawSubRect`]（含交叉淡入叠绘选项）。 */
  drawSubRect(
    bitmap: ImageBitmap,
    atlasName: string,
    rect: FrameSubRect,
    mirror: boolean,
    alpha: number,
    opts?: DrawOptions,
  ): void;
}

/** 就绪帧缓存最小接口（真实 `AtlasCache` 结构满足；B10 驱逐通知可选）。 */
export interface FrameAtlasCache {
  /** 取图集位图（命中即触碰；未命中解码，失败 → null）。 */
  get(name: string): Promise<ImageBitmap | null>;
  /** B10：注册位图驱逐监听（返回注销函数）；缺省表示无驱逐通知能力。 */
  onEvict?(listener: (name: string) => void): () => void;
}

/** 最近就绪帧：同步绘制所需的全部信息（位图就绪后冻结）。 */
interface ReadyFrame {
  readonly bitmap: ImageBitmap;
  /** 动作 ID（交叉淡入触发依据：动作切换才淡入，同动作翻帧硬切）。 */
  readonly actionId: string;
  readonly atlasName: string;
  readonly rect: FrameSubRect;
  readonly mirror: boolean;
  readonly alpha: number;
}

/**
 * 帧动画渲染器（帧回退实现，`ICharacterRenderer <|.. FrameRenderer`，K-14）。
 *
 * S9-M1：实现 [`ICharacterRenderer`]——`isReady()` 恒 `true`（帧路径无异步加载期）；
 * 交叉淡入时长/缓动可配（默认 150ms 线性，与 S2-M2 既有行为一致；传入 150~250ms
 * 区间 + 缓动曲线即升级）。
 *
 * @param stage        渲染舞台（三级探测结果，由装配方注入）
 * @param cache        图集位图缓存
 * @param onFrameReady 就绪帧更新后的回调（装配方接到 `LayerHost.render()` 做按序合成）
 * @param now          单调时钟读数（默认 `performance.now`；仅测本地过渡时长，
 *                     C3 边界：不读墙钟、不落业务计时）
 * @param fade         S9-M1 交叉淡入配置（默认 150ms 线性）
 */
export class FrameRenderer implements ICharacterRenderer {
  /** 渲染后端种类（K-14：`kind` 标识，骨骼实现为 S9-M1 段）。 */
  readonly kind = RENDERER_KIND_FRAME;

  private readyFrame: ReadyFrame | null = null;
  /** 交叉淡入的旧动作末帧（淡入结束即丢弃）。 */
  private fadeFrom: ReadyFrame | null = null;
  /** 首个新动作帧到达时刻（时钟读数；null = 无进行中的淡入）。 */
  private fadeStart: number | null = null;
  private pendingCmd: RenderFrameCmdV1 | null = null;
  private drawing = false;

  /** 本次动作切换采用的交叉淡入时长（S9-M1；构造期固化，默认 150ms）。 */
  private readonly fadeMs: number;
  /** 本次动作切换采用的交叉淡入缓动（S9-M1；默认线性）。 */
  private readonly fadeEasing: CrossfadeEasing;

  constructor(
    private readonly stage: FrameStage,
    private readonly cache: FrameAtlasCache,
    private readonly onFrameReady: () => void,
    private readonly now: () => number = () => performance.now(),
    fade: { ms?: number; easing?: CrossfadeEasing } = {},
  ) {
    // B10：位图被 AtlasCache LRU close() 驱逐 → 立即失效引用它的就绪帧，
    // 否则 paint 将使用已关闭的 ImageBitmap（真实风险：>12 图集触发驱逐）。
    this.cache.onEvict?.((name) => this.invalidateAtlas(name));
    this.fadeMs = Number.isFinite(fade.ms ?? CROSSFADE_MS) ? (fade.ms as number) : CROSSFADE_MS;
    this.fadeEasing = fade.easing ?? linearEasing;
  }

  /** 帧路径恒就绪（K-14：无运行时加载期，BackendSwitcher 回退判定用）。 */
  isReady(): boolean {
    return true;
  }

  /** 失效引用指定图集的就绪帧（B10 驱逐回调主体）。 */
  private invalidateAtlas(name: string): void {
    if (this.readyFrame?.atlasName === name) {
      this.readyFrame = null;
    }
    if (this.fadeFrom?.atlasName === name) {
      this.fadeFrom = null;
      this.fadeStart = null;
    }
  }

  /**
   * 消费一帧命令（被动入口，由 `pet://frame` 订阅方调用）。
   *
   * 结构不完整（`columns`/`rows` 为 0）或子矩形非法 → 告警跳帧（`02 §7.4`）；
   * 图集不可用 → 告警跳帧。
   */
  draw(cmd: RenderFrameCmdV1): void {
    if (this.drawing) {
      // latest-wins：只保留最新帧，避免异步加载期间的乱序回退。
      this.pendingCmd = cmd;
      return;
    }
    this.drawing = true;
    void this.consume(cmd).finally(() => {
      this.drawing = false;
      const next = this.pendingCmd;
      this.pendingCmd = null;
      if (next !== null) {
        this.draw(next);
      }
    });
  }

  /**
   * 同步绘制当前就绪帧（注册为 `LayerHost` 的 character 层回调）。
   *
   * - 尚无就绪帧（启动后未收到 `pet://frame`）→ 空操作，保持透明窗口无残留；
   * - B10 兜底：paint 前校验位图有效性（`close()` 后 width/height 归零），
   *   失效帧丢弃并跳过本次绘制；
   * - 淡入进行中（动作切换后 150ms 内）：旧动作末帧与新帧双绘、alpha 线性互补；
   *   超时后回落单帧绘制。
   */
  paint(): void {
    const frame = this.readyFrame;
    if (frame === null) {
      return;
    }
    if (!isBitmapValid(frame.bitmap)) {
      // B10 兜底：驱逐通知丢失（如监听器注册前的驱逐）也能自愈。
      this.readyFrame = null;
      console.warn('[FrameRenderer] 就绪帧位图已失效（疑似被 LRU 驱逐），丢弃并等待下一帧');
      return;
    }

    const fade = this.currentFade();
    if (fade === null || this.fadeFrom === null || !isBitmapValid(this.fadeFrom.bitmap)) {
      if (fade !== null) {
        // 旧帧位图已失效：淡入退化为直接展示新帧。
        this.fadeFrom = null;
        this.fadeStart = null;
      }
      this.stage.drawSubRect(frame.bitmap, frame.atlasName, frame.rect, frame.mirror, frame.alpha);
      return;
    }

    const alphas = fade;
    // 双绘：先旧动作末帧（清画布），再叠新动作首帧（不清画布）。
    this.stage.drawSubRect(
      this.fadeFrom.bitmap,
      this.fadeFrom.atlasName,
      this.fadeFrom.rect,
      this.fadeFrom.mirror,
      this.fadeFrom.alpha * alphas.prevAlpha,
      { clear: true },
    );
    this.stage.drawSubRect(
      frame.bitmap,
      frame.atlasName,
      frame.rect,
      frame.mirror,
      frame.alpha * alphas.nextAlpha,
      { clear: false },
    );
  }

  /** 当前淡入的新旧 alpha（未在淡入中 / 已超时 → null 并清理过渡状态）。 */
  private currentFade(): { prevAlpha: number; nextAlpha: number } | null {
    if (this.fadeStart === null || this.fadeFrom === null) {
      return null;
    }
    const alphas = crossfadeAlphas(
      this.now() - this.fadeStart,
      this.fadeMs,
      this.fadeEasing,
    );
    if (alphas === null) {
      this.fadeFrom = null;
      this.fadeStart = null;
      return null;
    }
    return alphas;
  }

  /** 异步消费：解析命令 → 取位图 → 更新就绪帧 → 触发合成回调。 */
  private async consume(cmd: RenderFrameCmdV1): Promise<void> {
    if (cmd.columns <= 0 || cmd.rows <= 0) {
      console.warn('[FrameRenderer] 载荷缺图集布局（columns/rows=0），跳帧：', cmd.actionId);
      return;
    }
    const rect = computeFrameRect(
      cmd.frameIndex,
      cmd.columns,
      cmd.rows,
      cmd.frameW,
      cmd.frameH,
    );
    if (rect === null) {
      console.warn('[FrameRenderer] 帧子矩形非法，跳帧：index=', cmd.frameIndex);
      return;
    }
    if (cmd.atlasPng.length === 0) {
      console.warn('[FrameRenderer] 载荷缺图集引用（atlasPng 为空），跳帧：', cmd.actionId);
      return;
    }

    const bitmap = await this.cache.get(cmd.atlasPng);
    if (bitmap === null) {
      // 02 §7.4：图集缺失降级——告警 + 跳帧。
      console.warn('[FrameRenderer] 图集不可用，跳帧：', cmd.atlasPng);
      return;
    }

    const next: ReadyFrame = {
      bitmap,
      actionId: cmd.actionId,
      atlasName: cmd.atlasPng,
      rect,
      mirror: cmd.mirror,
      alpha: cmd.alpha,
    };

    // 交叉淡入触发（S2-M2）：动作切换才淡入；同动作翻帧硬切（不逐帧叠影）。
    if (
      this.readyFrame !== null &&
      this.readyFrame.actionId !== next.actionId
    ) {
      this.fadeFrom = this.readyFrame;
      this.fadeStart = this.now();
    }

    this.readyFrame = next;
    this.onFrameReady();
  }
}

/** B10 位图有效性：`close()` 后 width/height 归零（HTML 规范定义的关闭语义）。 */
function isBitmapValid(bitmap: ImageBitmap): boolean {
  return bitmap.width > 0 && bitmap.height > 0;
}
