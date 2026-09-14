/**
 * 事件总线事件名常量（`02 §7.6`）。
 *
 * 命名规范：`pet://<域>`。**新增事件必须先登记到 `02 §7.6` 再在此处添加**（C8）。
 */
export const PET_EVENT = {
  /** core → 宠物窗口：`RenderFrameCmd` v2，渲染 tick（2~60Hz）。 */
  FRAME: 'pet://frame',
  /** core → 宠物窗口：`BubbleCmd`，气泡变更时。 */
  BUBBLE: 'pet://bubble',
  /** core → 宠物窗口：`ParticleCmd`，交互粒子触发时（S3-M6 登记）。 */
  FX: 'pet://fx',
  /** core → 宠物窗口：`MenuCmd`，右键单击命中时（S3-M6 登记）。 */
  MENU: 'pet://menu',
  /** core → 设置窗口：`PetSnapshotV2`，1Hz。 */
  STATE: 'pet://state',
  /** core → 设置窗口：需求属性跨档时。 */
  NEEDS: 'pet://needs',
  /** core → 全部：`ActivitySnapshot`，状态迁移时。 */
  ACTIVITY: 'pet://activity',
  /** core → 设置窗口：金币 / 背包变更时。 */
  ECONOMY: 'pet://economy',
  /** core → 全部：情绪等级变更时。 */
  EMOTION: 'pet://emotion',
  /** core → 全部：配置摘要变更时。 */
  CONFIG: 'pet://config',
  /** core → 设置窗口：性能采样，5s。 */
  PERF: 'pet://perf',
  /** 平台 → app：托盘动作。 */
  TRAY: 'pet://tray',
  /** app → 全部：`SelfCheckReport` 摘要，30s 周期自检时（S1-M4 登记）。 */
  SELFCHECK: 'pet://window/selfcheck',
} as const;

/** 事件名联合类型（`02 §7.6` 全量清单）。 */
export type PetEventName = (typeof PET_EVENT)[keyof typeof PET_EVENT];
