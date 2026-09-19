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

/**
 * 设定画布**呈现尺寸**（CSS px == DIP）与**位图尺寸**（物理 px）。
 *
 * 两条尺寸各司其职、**禁止混用**（`02 §4.4` 视觉契约）：
 * - CSS 呈现边长 = `cssPx`（宠物逻辑尺寸 128，1 CSS px == 1 DIP）；
 * - 位图边长 = `resizeCanvasToWindow(canvas, bitmapLogicalPx)` = `bitmapLogicalPx × dpr`
 *   （`bitmapLogicalPx` 为**位图逻辑输入**，宠物 = `128 × 2 = 256`，保证高 DPR 物理像素充足）。
 *
 * ⚠️ 该函数是 S10 真机回归 Bug 的**单一修复点与回归护栏**：历史上曾把
 * 「位图密度倍数」误乘进 CSS 尺寸（256 CSS px），而窗口仅 256 物理 px（DPR=2 时
 * 视口 = 128 CSS px）→ 画布溢出视口 2 倍，宠物只露左上 1/4（狐被裁到底右角）。
 * 任何「CSS 尺寸 ≠ cssPx」的偏离都由此函数拦截（见 `coords.test.ts`）。
 *
 * @param canvas           目标画布
 * @param cssPx            CSS 逻辑边长（DIP；宠物 = 128），**直接写入 style，不再乘任何系数**
 * @param bitmapLogicalPx  位图逻辑输入边长（宠物 = 256 = 128 × 2）；`resizeCanvasToWindow` 再乘 DPR
 * @returns 实际写入的位图边长（物理像素）= `bitmapLogicalPx × dpr`
 */
export function applyCanvasSizing(
  canvas: HTMLCanvasElement,
  cssPx: number,
  bitmapLogicalPx: number,
): number {
  canvas.style.width = `${cssPx}px`;
  canvas.style.height = `${cssPx}px`;
  return resizeCanvasToWindow(canvas, bitmapLogicalPx);
}
