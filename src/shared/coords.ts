/**
 * 共享坐标系工具（`02 §7.2`，RV-17 修正版）。
 *
 * 三套坐标系：
 * - **PLC**：单帧左上，贴图原始像素（命中掩码 / 覆盖率表 / 锚点）。
 * - **VDC**：虚拟桌面左上（可为负），96-dpi 逻辑像素 —— **内核唯一运动坐标系**。
 * - **SPC**：所在显示器物理左上，物理像素 —— 写窗口与绘制使用。
 *
 * 换算（RV-17，必须带上「所在显示器原点偏移」，否则跨屏点位系统性错位，AC-13 必失败）：
 *   SPC = (VDC − originVdc(monitor)) × scale(monitor)
 *
 * ⚠️ 本模块只提供**纯函数换算**；`MonitorInfo` 的真实数据来源（`DisplayService`）
 * 由 S1-M2 的 `dp-platform/win/display.rs` 提供，届时经 IPC 注入前端（TODO-S1-M2）。
 */

/** 二维点（无单位语义，具体单位由使用的坐标系决定）。 */
export interface Point {
  readonly x: number;
  readonly y: number;
}

/** 虚拟桌面坐标系（VDC）中的点，单位：96-dpi 逻辑像素。 */
export type VdcPoint = Point;

/** 屏幕物理坐标系（SPC）中的点，单位：物理像素。 */
export type SpcPoint = Point;

/** 显示器描述（`02 §7.2` 换算所需的最小信息集）。 */
export interface MonitorInfo {
  /** 显示器标识（Windows 下为 HMONITOR 的稳定包装值）。 */
  readonly id: string;
  /** 显示器左上角在 VDC 中的坐标（可为负）。 */
  readonly originVdc: VdcPoint;
  /** DPI 缩放系数（1.0 = 96dpi）。 */
  readonly scale: number;
}

/** VDC → SPC：`SPC = (VDC − originVdc) × scale`。 */
export function vdcToSpc(vdc: VdcPoint, monitor: MonitorInfo): SpcPoint {
  return {
    x: (vdc.x - monitor.originVdc.x) * monitor.scale,
    y: (vdc.y - monitor.originVdc.y) * monitor.scale,
  };
}

/** SPC → VDC（上式的逆换算）。 */
export function spcToVdc(spc: SpcPoint, monitor: MonitorInfo): VdcPoint {
  return {
    x: monitor.originVdc.x + spc.x / monitor.scale,
    y: monitor.originVdc.y + spc.y / monitor.scale,
  };
}

/**
 * 按 DPR 调整画布位图尺寸。
 *
 * @param canvas    目标画布
 * @param logicalPx 逻辑边长（CSS 像素）
 * @returns 实际写入的位图边长（物理像素）
 */
export function resizeCanvasToWindow(canvas: HTMLCanvasElement, logicalPx: number): number {
  const dpr = window.devicePixelRatio > 0 ? window.devicePixelRatio : 1;
  const physicalPx = Math.max(1, Math.round(logicalPx * dpr));
  canvas.width = physicalPx;
  canvas.height = physicalPx;
  return physicalPx;
}
