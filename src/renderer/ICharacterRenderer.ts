/**
 * 角色渲染后端抽象（K-14 `ICharacterRenderer`，S9-M1）。
 *
 * 职责：把「角色怎么画」与「当前用哪条路径」解耦——
 *   - [`FrameRenderer`]：帧回退路径（批次 A 帧动画，恒就绪）；
 *   - `SkeletonRenderer`（S9-M2）：骨骼路径（Spine 4.2 运行时，加载期 `isReady=false`）。
 *
 * 切换由 [`BackendSwitcher`] 编排：骨架后端 3s 未就绪即自动停在帧路径，
 * 全程帧路径持续绘制（不黑屏，K-14）。
 */
import type { RenderFrameCmdV1 } from '../shared/ipc';

/** 渲染后端种类标识（K-14 `kind`；帧回退实现为 [`RENDERER_KIND_FRAME`]）。 */
export type RendererKind = 'frame' | 'skeleton';

/** 骨架后端就绪超时（毫秒，K-14：3s 未 ready 自动回退帧动画）。 */
export const SKELETON_READY_TIMEOUT_MS = 3_000;

/**
 * 角色渲染后端端口（`ICharacterRenderer <|.. FrameRenderer / SkeletonRenderer`）。
 *
 * 三方法语义在两条路径间保持一致：
 *   - [`draw`]：被动推帧入口（Rust 侧驱动，无 rAF 自循环，K-4）；
 *   - [`paint`]：同步绘制当前就绪内容（注册为 `LayerHost` 的 character 层回调）；
 *   - [`isReady`]：是否可绘制——帧路径恒 `true`，骨骼路径运行时/资产加载期 `false`。
 */
export interface ICharacterRenderer {
  /** 后端种类标识（K-14）。 */
  readonly kind: RendererKind;

  /** 被动入口：消费一帧命令（帧路径按 v1 载荷；骨骼路径以同语义驱动动作/相位）。 */
  draw(cmd: RenderFrameCmdV1): void;

  /** 同步绘制当前就绪内容（无就绪内容时空操作，保持透明无残留）。 */
  paint(): void;

  /** 是否已就绪可绘制（骨骼运行时加载期返回 `false`，供回退判定）。 */
  isReady(): boolean;
}
