/**
 * 骨骼渲染器（S9-M2 接入框架，`ICharacterRenderer <|.. SkeletonRenderer`，K-14）。
 *
 * 定位（P2-3 依赖拆解①「接入框架」）：
 *   - 用**窄适配器端口** [`SpineRuntimeAdapter`] 包住 Spine 4.2 运行时（首选）/
 *     Cubism（次选），本类不含任何 Spine 专有 API，单测注入 Fake 即可；
 *   - 正式美术资产绑定（②）为 SG-M2 人工步骤，本框架**不依赖**正式资产；
 *   - **换装只切 UV**（setSkin 切 region），增量 0、无重解码（验收：内存增量 ≈0）；
 *   - 加载期 `isReady()=false`，由 [`BackendSwitcher`] 走帧回退（3s 未就绪不黑屏）。
 *
 * 与帧路径同语义：`draw(cmd)` 按 `actionId` 切动画、`paint()` 出当前姿态、
 * `mirror/alpha` 透传。
 */
import type { RenderFrameCmdV1 } from '../shared/ipc';
import type { ICharacterRenderer } from './ICharacterRenderer';

/** Spine 运行时窄端口（真实实装 = spine-webgl 4.2 SceneRenderer 薄封装；测试注入 Fake）。 */
export interface SpineRuntimeAdapter {
  /** 运行时 + 资产是否加载完成。 */
  isLoaded(): boolean;
  /** 异步加载运行时与骨骼资产（幂等）。 */
  load(): Promise<void>;
  /** 按动作 ID 切动画（loop 参数化）。 */
  setAnimation(actionId: string, looping: boolean): void;
  /** 换装 = 切皮肤（region=切 UV，无重解码）。 */
  setSkin(skinName: string): void;
  /** 出当前姿态（alpha/镜像透传给 GPU）。 */
  render(opts: { alpha: number; mirror: boolean }): void;
}

/** 骨架后端就绪结果回调。 */
export interface SkeletonRendererHooks {
  /** 加载失败（降级：BackendSwitcher 3s 后定格帧回退）。 */
  onLoadError?: (err: unknown) => void;
}

export class SkeletonRenderer implements ICharacterRenderer {
  readonly kind = 'skeleton' as const;

  private loaded = false;
  private loadError: unknown = null;
  private looping = true;
  private currentAction: string | null = null;

  constructor(
    private readonly adapter: SpineRuntimeAdapter,
    private readonly hooks: SkeletonRendererHooks = {},
  ) {
    // 启动即异步加载（不阻塞构造；加载期帧路径兜底，K-14）。
    void this.adapter
      .load()
      .then(() => {
        this.loaded = true;
      })
      .catch((err: unknown) => {
        this.loadError = err;
        this.hooks.onLoadError?.(err);
      });
  }

  /** 骨架是否就绪（运行时 + 资产加载完成）。 */
  isReady(): boolean {
    return this.loaded;
  }

  /** 最近一次加载错误（观测/降级遥测用）。 */
  get error(): unknown {
    return this.loadError;
  }

  /** 被动入口：按 `actionId` 切动画（未就绪时由 BackendSwitcher 路由帧路径，本类不被调用）。 */
  draw(cmd: RenderFrameCmdV1): void {
    if (this.currentAction !== cmd.actionId) {
      this.currentAction = cmd.actionId;
      this.adapter.setAnimation(cmd.actionId, this.looping);
    }
  }

  /** 同步出当前姿态（注册为 LayerHost character 层后由 switcher 路由）。 */
  paint(): void {
    this.adapter.render({ alpha: 1, mirror: false });
  }

  /** 换装：只切皮肤（UV），即时、无重解码（验收：内存增量 ≈0）。 */
  setSkin(skinName: string): void {
    this.adapter.setSkin(skinName);
  }
}
