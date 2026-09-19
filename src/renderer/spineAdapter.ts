/**
 * Spine 运行时适配器工厂（S9-M2）。
 *
 * 当前为**占位实现**：正式 Spine 4.2 运行时绑定（spine-webgl SceneRenderer 薄封装）
 * 与正式美术资产（skeleton.json/.atlas/.png）绑定是 SG-M2 人工步骤（P2-3 拆解②）。
 *
 * 在正式资产就绪前：占位适配器 `load()` 失败、`isLoaded()` 恒 `false`，
 * 于是 [`BackendSwitcher`] 在 3s 后自动定格在帧回退路径（不黑屏，K-14），
 * 帧路径先行开发不受影响（P2-3）。
 */
import type { SpineRuntimeAdapter } from './SkeletonRenderer';

/* eslint-disable @typescript-eslint/no-unused-vars -- 占位适配器：方法体故意留空，等正式 Spine 接入 */

/** 占位适配器：运行时/资产未就绪。 */
class PlaceholderSpineAdapter implements SpineRuntimeAdapter {
  isLoaded(): boolean {
    return false;
  }

  async load(): Promise<void> {
    // 正式 Spine 4.2 运行时 + 骨骼资产绑定未完成（S9-M2② 人工步骤）。
    throw new Error('Spine 运行时未绑定：等待 S9-M2② 正式美术资产接入');
  }

  setAnimation(_actionId: string, _looping: boolean): void {
    // 占位：未就绪时不会被调用（切换器路由帧路径）。
  }

  setSkin(_skinName: string): void {
    // 占位：换装 UV 切换在正式接入后实现。
  }

  render(_opts: { alpha: number; mirror: boolean }): void {
    // 占位：不绘制。
  }
}

/** 创建占位 Spine 适配器（正式接入后替换为 spine-webgl 薄封装实现）。 */
export function createPlaceholderSpineAdapter(): SpineRuntimeAdapter {
  return new PlaceholderSpineAdapter();
}
