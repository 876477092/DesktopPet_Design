/**
 * 帧回退图集分包规划（K-14，S9-M1 要点 2；`02 §4.4`）。
 *
 * 53 动作不打成单一大图集（单包超 LRU 字节上限风险 + 首帧解码延迟），按目录序
 * 均衡切包：
 *   - 批次 A（生存 29 动作）→ **4 包**；
 *   - 全量（53 动作）→ **6 包**。
 *
 * 纯函数：只做确定性均衡划分（Map 序即动作目录序），不碰文件/网络；
 * 前端 LRU 按包名取图，单包体积受 64MB 硬上限约束（`ATLAS_LRU_MAX_BYTES`）。
 */

/** 默认每包动作数上限（约 9 动作/包：29→4、53→6 的取整口径）。 */
export const DEFAULT_ACTIONS_PER_PACK = 9;

/** 一个动作所属分包的规划结果。 */
export interface PackAssignment {
  /** 动作 ID。 */
  readonly actionId: string;
  /** 分包序号（0 起，行主序均衡）。 */
  readonly packIndex: number;
  /** 包文件名片段，如 `atlas-pack-0.png`。 */
  readonly packName: string;
}

/**
 * 把动作 ID 列表按目录序均衡切包（纯函数）。
 *
 * @param actionIds        动作 ID（目录序；去重保序）
 * @param actionsPerPack   每包动作数上限（默认 9：29→4、53→6）
 * @returns 与输入等长、按原序的分包分配
 */
export function planAtlasPacks(
  actionIds: readonly string[],
  actionsPerPack: number = DEFAULT_ACTIONS_PER_PACK,
): PackAssignment[] {
  const per = Number.isInteger(actionsPerPack) && actionsPerPack > 0 ? actionsPerPack : DEFAULT_ACTIONS_PER_PACK;
  const seen = new Set<string>();
  const ordered: string[] = [];
  for (const id of actionIds) {
    if (typeof id === 'string' && id.length > 0 && !seen.has(id)) {
      seen.add(id);
      ordered.push(id);
    }
  }
  return ordered.map((actionId, i) => {
    const packIndex = Math.floor(i / per);
    return {
      actionId,
      packIndex,
      packName: `atlas-pack-${packIndex}.png`,
    };
  });
}

/** 分包总数（纯函数：批次 A 29→4、全量 53→6，默认每包 9 动作）。 */
export function packCount(
  actionIds: readonly string[],
  actionsPerPack: number = DEFAULT_ACTIONS_PER_PACK,
): number {
  const assigned = planAtlasPacks(actionIds, actionsPerPack);
  if (assigned.length === 0) {
    return 0;
  }
  const last = assigned[assigned.length - 1];
  return (last?.packIndex ?? 0) + 1;
}
