import React, {
  createContext,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from 'react';

import { listenEvent, parsePetSnapshotV2, type PetSnapshotV2 } from '@shared/ipc';
import { PET_EVENT } from '@shared/types';

/**
 * 宠物实时快照 Context（S10-M1：属性 / 活动 / 商城 / 背包 / 相册·装饰 / 原因卡
 * 的统一数据源）。
 *
 * ## 与设置 store 的分工
 * - `useSettingsStore`：**用户偏好**（settings.json ⊕ 存档 B 段，写路径 `settings_apply`）；
 * - 本 Context：**运行态快照**（core-loop 每 1Hz 推 `pet://state`，只读；
 *   六维 / P 因子 / 经济 / 背包 / 活动 / 装饰槽 / 相册）。
 *
 * ## 降级
 * 非 Tauri 运行时（浏览器直开 settings.html / vitest）不订阅，`usePetSnapshot()`
 * 返回 `null` → 各页渲染空态 / 占位（`02 §7.4.2` 不阻断设置窗显示）。
 * 测试可用 `<PetSnapshotProvider value={固定快照}>` 直接注入。
 */
const PetSnapshotContext = createContext<PetSnapshotV2 | null>(null);

interface PetSnapshotProviderProps {
  children: ReactNode;
  /** 测试注入固定快照（缺省则订阅 `pet://state`）。 */
  value?: PetSnapshotV2 | null;
}

/** 是否运行在 Tauri WebView 内（与 `settings/main.tsx` 同判定口径）。 */
function hasTauriRuntime(): boolean {
  return typeof (globalThis as Record<string, unknown>).__TAURI_INTERNALS__ !== 'undefined';
}

/** 实时快照 Provider（挂在设置窗根部；订阅一次，全页共享）。 */
export function PetSnapshotProvider({
  children,
  value,
}: PetSnapshotProviderProps): React.ReactElement {
  const [live, setLive] = useState<PetSnapshotV2 | null>(null);

  useEffect(() => {
    if (value !== undefined || !hasTauriRuntime()) {
      return;
    }
    let unlisten: (() => void) | null = null;
    let disposed = false;
    void listenEvent<unknown>(PET_EVENT.STATE, (payload) => {
      const parsed = parsePetSnapshotV2(payload);
      if (parsed && !disposed) {
        setLive(parsed);
      }
    }).then((u) => {
      if (disposed) {
        u();
      } else {
        unlisten = u;
      }
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [value]);

  return (
    <PetSnapshotContext.Provider value={value !== undefined ? value : live}>
      {children}
    </PetSnapshotContext.Provider>
  );
}

/** 取最新实时快照（未连接内核时为 `null`）。 */
export function usePetSnapshot(): PetSnapshotV2 | null {
  return useContext(PetSnapshotContext);
}
