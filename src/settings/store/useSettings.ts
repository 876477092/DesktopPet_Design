import { createContext, useContext, useEffect, useMemo, useRef, useSyncExternalStore } from 'react';

import {
  SETTINGS_FALLBACK,
  invokeCommand,
  listenEvent,
  parseConfigCmd,
  parseSaveStatus,
  parseSettingsSnapshot,
  type ConfigCmdV1,
  type SaveStatusV1,
  type SettingsPatchV1,
  type SettingsSnapshotV1,
} from '@shared/ipc';
import { createTranslator, resolveLocale, type Translator } from '@shared/i18n';
import { PET_EVENT } from '@shared/types';

/**
 * 设置面板状态层（`03 S5-M3` 交付物 `store/useSettings.ts`）。
 *
 * ## 与 S5-M4 的边界（本文件是唯一的耦合面）
 * UI 只依赖 [`SettingsPort`] 端口；**真实 IPC 实现由 [`createIpcSettingsPort`] 提供**
 * （`settings_get` / `settings_apply` / `save_command` / `settings_reset_all` /
 * `pet_emotion_command` + `pet://config` 订阅）。端口化带来两个直接收益：
 *   1. 组件与状态机可在 vitest 里用**假端口**完整驱动（零 WebView / 零 Tauri 运行时）；
 *   2. S5-M4 若调整命令命名或载荷，只需改这一个适配器。
 *
 * ## 生效时序（`01 FR-7-4`「每项即时生效」× `01 §8.3` 的「保存」按钮）
 * - 改一项 → 本地快照**立即**更新（UI 即时可见，AC）+ 累积进 `pending`；
 * - `debounceMs` 后自动提交（拖动滑块不会打出 N 次 IPC）；
 * - 点「保存」= 立即提交待发补丁（等同把防抖窗口提前结束，语义一致、无第二条写入路径）。
 *
 * ## 纪律
 * - **零硬编码数值**：一切上下界 / 步进 / 档位候选值取自服务端快照；本地只做钳制兜底；
 * - **零时钟读取**（只在防抖里用 `setTimeout` 计时，不读系统时间）；
 * - 提交失败 → 保留 `pending`（不丢用户改动）+ 记录 `error`；下次提交自动重试。
 */

/** 设置面板端口（真实实现见 [`createIpcSettingsPort`]；测试用假实现）。 */
export interface SettingsPort {
  /** 读取有效设置快照。 */
  load(): Promise<SettingsSnapshotV1>;
  /** 提交补丁，返回服务端合并后的快照。 */
  apply(patch: SettingsPatchV1): Promise<SettingsSnapshotV1>;
  /** 重置全部数据（清档重建 + 重启）。 */
  resetAll(): Promise<void>;
  /** 查询存档健康状态（「数据」Tab）。 */
  saveStatus(): Promise<SaveStatusV1 | null>;
  /** 导入指定备份档。 */
  importSave(file: string): Promise<void>;
  /** 情绪兜底命令（`reset` 重置情绪 / `recall` 找回）。 */
  emotionCommand(command: 'reset' | 'recall'): Promise<void>;
  /** 订阅 `pet://config`（返回退订函数）。 */
  subscribeConfig(listener: (cmd: ConfigCmdV1) => void): Promise<() => void>;
}

/** 设置面板可观察状态。 */
export interface SettingsStoreState {
  /** 当前快照（含未提交的本地改动）。 */
  readonly snapshot: SettingsSnapshotV1;
  /** 待提交补丁（`{}` = 无待提交）。 */
  readonly pending: SettingsPatchV1;
  /** 是否有待提交改动（UI 显示「未保存」）。 */
  readonly dirty: boolean;
  /** 是否正在首次加载。 */
  readonly loading: boolean;
  /** 是否正在提交。 */
  readonly saving: boolean;
  /** 最近一次失败原因（可读文案；无失败 = `null`）。 */
  readonly error: string | null;
  /** 最近一次成功提示键（i18n 键；无提示 = `null`）。 */
  readonly notice: string | null;
  /** 设置服务是否可写（`false` = 只读预览）。 */
  readonly writable: boolean;
  /** 存档健康状态（未加载 = `null`）。 */
  readonly saveStatus: SaveStatusV1 | null;
}

/** 初始状态（加载前）。 */
export function initialSettingsState(): SettingsStoreState {
  return {
    snapshot: SETTINGS_FALLBACK,
    pending: {},
    dirty: false,
    loading: true,
    saving: false,
    error: null,
    notice: null,
    writable: false,
    saveStatus: null,
  };
}

/** 合并补丁（浅合并；`reminders` 子对象按字段合并）。 */
export function mergePatches(
  base: SettingsPatchV1,
  next: SettingsPatchV1,
): SettingsPatchV1 {
  const merged: Record<string, unknown> = { ...base, ...next };
  if (base.reminders !== undefined || next.reminders !== undefined) {
    merged.reminders = { ...(base.reminders ?? {}), ...(next.reminders ?? {}) };
  }
  // 去掉显式 `undefined`（保持 `{}` = 无改动的可判定性）。
  for (const key of Object.keys(merged)) {
    if (merged[key] === undefined) {
      delete merged[key];
    }
  }
  return merged as SettingsPatchV1;
}

/** 补丁是否为空（无字段）。 */
export function isPatchEmpty(patch: SettingsPatchV1): boolean {
  return Object.keys(patch).length === 0;
}

/**
 * 把补丁**乐观**应用到本地快照（UI 即时反馈；服务端返回后以服务端值为准）。
 *
 * 只用「域内钳制」兜底（上界来自快照自身），不发明数值：真正的夹紧在 Rust 侧
 * （`bridge::merge_patch`），这里是「别让滑杆瞬间跳出去」的显示层保险。
 */
export function applyPatchLocally(
  snapshot: SettingsSnapshotV1,
  patch: SettingsPatchV1,
): SettingsSnapshotV1 {
  const next: Record<string, unknown> = { ...snapshot };
  const clamp = (value: number, min: number, max: number): number =>
    Math.min(max, Math.max(min, value));
  if (patch.name !== undefined) next.name = patch.name;
  if (patch.scalePercent !== undefined) {
    next.scalePercent = clamp(
      Math.trunc(patch.scalePercent),
      snapshot.scaleMinPercent,
      snapshot.scaleMaxPercent,
    );
  }
  if (patch.opacityPercent !== undefined) {
    next.opacityPercent = clamp(
      Math.trunc(patch.opacityPercent),
      snapshot.opacityMinPercent,
      snapshot.opacityMaxPercent,
    );
  }
  if (patch.language !== undefined) next.language = patch.language;
  if (patch.masterVolumePercent !== undefined) {
    next.masterVolumePercent = clamp(
      Math.trunc(patch.masterVolumePercent),
      snapshot.volumeMinPercent,
      snapshot.volumeMaxPercent,
    );
  }
  if (patch.muted !== undefined) next.muted = patch.muted;
  if (patch.autoRoam !== undefined) next.autoRoam = patch.autoRoam;
  if (patch.roamPace !== undefined) {
    const lo = snapshot.roamPaceOptions[0] ?? snapshot.roamPace;
    const hi = snapshot.roamPaceOptions[snapshot.roamPaceOptions.length - 1] ?? snapshot.roamPace;
    next.roamPace = clamp(patch.roamPace, Math.min(lo, hi), Math.max(lo, hi));
  }
  if (patch.doNotDisturb !== undefined) next.doNotDisturb = patch.doNotDisturb;
  if (patch.easyCoaxMode !== undefined) next.easyCoaxMode = patch.easyCoaxMode;
  if (patch.clickThrough !== undefined) next.clickThrough = patch.clickThrough;
  if (patch.alwaysOnTopPolicy !== undefined) next.alwaysOnTopPolicy = patch.alwaysOnTopPolicy;
  if (patch.autostart !== undefined) next.autostart = patch.autostart;
  if (patch.sensitivityValue !== undefined) next.sensitivityValue = patch.sensitivityValue;
  if (patch.catchphraseEnabled !== undefined) next.catchphraseEnabled = patch.catchphraseEnabled;
  if (patch.catchphraseFrequency !== undefined) {
    next.catchphraseFrequency = patch.catchphraseFrequency;
  }
  if (patch.clickFeedbackEnabled !== undefined) {
    next.clickFeedbackEnabled = patch.clickFeedbackEnabled;
  }
  if (patch.activitySensing !== undefined) next.activitySensing = patch.activitySensing;
  if (patch.reminders !== undefined) {
    const lo = snapshot.reminders.intervalMinMin;
    const hi = snapshot.reminders.intervalMaxMin;
    next.reminders = {
      ...snapshot.reminders,
      ...patch.reminders,
      sedentaryIntervalMin:
        patch.reminders.sedentaryIntervalMin === undefined
          ? snapshot.reminders.sedentaryIntervalMin
          : clamp(Math.trunc(patch.reminders.sedentaryIntervalMin), lo, hi),
      waterIntervalMin:
        patch.reminders.waterIntervalMin === undefined
          ? snapshot.reminders.waterIntervalMin
          : clamp(Math.trunc(patch.reminders.waterIntervalMin), lo, hi),
    };
  }
  return next as unknown as SettingsSnapshotV1;
}

/** 设置面板状态机（纯逻辑，零 React 依赖 → 可直接单测）。 */
export class SettingsStore {
  private state: SettingsStoreState = initialSettingsState();
  private readonly listeners = new Set<() => void>();
  private readonly port: SettingsPort;
  /** 防抖窗口（毫秒）；0 = 立即提交（测试用）。 */
  private readonly debounceMs: number;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private unlisten: (() => void) | null = null;
  private lastRevision = 0;

  constructor(port: SettingsPort, options: { debounceMs?: number } = {}) {
    this.port = port;
    this.debounceMs = options.debounceMs ?? 150;
  }

  /** `useSyncExternalStore` 的读函数（返回稳定引用：只在变更时换对象）。 */
  readonly getState = (): SettingsStoreState => this.state;

  /** `useSyncExternalStore` 的订阅函数。 */
  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /** 首次加载：拉快照 + 订阅 `pet://config` + 拉存档状态。 */
  async load(): Promise<void> {
    try {
      const raw: unknown = await this.port.load();
      const snapshot = parseSettingsSnapshot(raw) ?? SETTINGS_FALLBACK;
      this.lastRevision = snapshot.revision;
      this.setState({ snapshot, writable: snapshot.writable, loading: false, error: null });
    } catch (err) {
      // 降级：内置兜底 + 只读预览（`02 §7.4.2`：不阻断窗口显示）。
      this.setState({
        snapshot: SETTINGS_FALLBACK,
        writable: false,
        loading: false,
        error: describe(err),
      });
    }
    try {
      this.unlisten = await this.port.subscribeConfig((cmd) => this.onConfigEvent(cmd));
    } catch {
      // 订阅失败不影响设置读写（只是少了「别处改了设置也同步」的能力）。
      this.unlisten = null;
    }
    await this.refreshSaveStatus();
  }

  /** 释放订阅与防抖定时器（组件卸载）。 */
  dispose(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    if (this.unlisten !== null) {
      this.unlisten();
      this.unlisten = null;
    }
    this.listeners.clear();
  }

  /** 改一项设置：本地立即生效 + 防抖提交。 */
  patch(next: SettingsPatchV1): void {
    if (isPatchEmpty(next)) {
      return;
    }
    const pending = mergePatches(this.state.pending, next);
    this.setState({
      snapshot: applyPatchLocally(this.state.snapshot, next),
      pending,
      dirty: true,
      notice: null,
    });
    this.scheduleFlush();
  }

  /** 立即提交待发补丁（「保存」按钮 / 防抖到点）。 */
  async flush(): Promise<void> {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    const patch = this.state.pending;
    if (isPatchEmpty(patch) || !this.state.writable) {
      return;
    }
    this.setState({ saving: true, error: null });
    try {
      const raw: unknown = await this.port.apply(patch);
      const snapshot = parseSettingsSnapshot(raw);
      if (snapshot === null) {
        throw new Error('设置服务返回了无法解析的快照');
      }
      this.lastRevision = snapshot.revision;
      this.setState({
        snapshot,
        pending: {},
        dirty: false,
        saving: false,
        writable: snapshot.writable,
        notice: 'app.saved.hint',
      });
    } catch (err) {
      // 失败保留 pending（不丢用户改动），UI 显示错误；下次提交自动重试。
      this.setState({ saving: false, error: describe(err), dirty: true });
    }
  }

  /** 拉取存档健康状态（「数据」Tab）。 */
  async refreshSaveStatus(): Promise<void> {
    try {
      const raw: unknown = await this.port.saveStatus();
      this.setState({ saveStatus: parseSaveStatus(raw) });
    } catch (err) {
      this.setState({ saveStatus: null, error: describe(err) });
    }
  }

  /** 重置全部数据（清档重建；成功后进程会重启，UI 无需刷新）。 */
  async resetAll(): Promise<void> {
    this.setState({ error: null });
    try {
      await this.port.resetAll();
      this.setState({ notice: 'data.reset.done' });
    } catch (err) {
      this.setState({ error: describe(err) });
    }
  }

  /** 导入指定备份档。 */
  async importSave(file: string): Promise<void> {
    this.setState({ error: null });
    try {
      await this.port.importSave(file);
      this.setState({ notice: 'data.save.import.done' });
    } catch (err) {
      this.setState({ error: describe(err) });
    }
  }

  /** 情绪兜底命令（重置情绪 / 找回）。 */
  async emotionCommand(command: 'reset' | 'recall'): Promise<void> {
    this.setState({ error: null });
    try {
      await this.port.emotionCommand(command);
    } catch (err) {
      this.setState({ error: describe(err) });
    }
  }

  /** 清空提示（提示条自动消失 / 用户关闭）。 */
  clearNotice(): void {
    this.setState({ notice: null });
  }

  /** 清空错误。 */
  clearError(): void {
    this.setState({ error: null });
  }

  /** 收到 `pet://config`：revision 变了就重新拉快照（避免全量广播）。 */
  onConfigEvent(cmd: ConfigCmdV1 | null): void {
    if (cmd === null || cmd.revision === this.lastRevision) {
      return;
    }
    void this.reload();
  }

  /** 重新拉取快照（含存档状态）。 */
  async reload(): Promise<void> {
    try {
      const raw: unknown = await this.port.load();
      const snapshot = parseSettingsSnapshot(raw);
      if (snapshot === null) {
        return;
      }
      this.lastRevision = snapshot.revision;
      this.setState({ snapshot, writable: snapshot.writable, error: null });
    } catch (err) {
      this.setState({ error: describe(err) });
    }
  }

  /** 防抖调度（`debounceMs === 0` → 立即提交）。 */
  private scheduleFlush(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
    }
    if (this.debounceMs <= 0) {
      this.timer = null;
      void this.flush();
      return;
    }
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.flush();
    }, this.debounceMs);
  }

  /** 状态更新 + 通知订阅者。 */
  private setState(partial: Partial<SettingsStoreState>): void {
    this.state = { ...this.state, ...partial };
    for (const listener of this.listeners) {
      listener();
    }
  }
}

/** 可读化异常（字符串 / Error / 其它）。 */
export function describe(err: unknown): string {
  if (typeof err === 'string') {
    return err;
  }
  if (err instanceof Error) {
    return err.message;
  }
  return String(err);
}

/**
 * 真实 IPC 端口（S5-M4 命令面）。
 *
 * 命令名与 `dp-app/src/commands.rs` 的 `#[tauri::command]` 一一对应；
 * 事件名取自 `PET_EVENT.CONFIG`（C8：`02 §7.6` 已登记项）。
 */
export function createIpcSettingsPort(): SettingsPort {
  return {
    // 端口契约是**结构化**的（`SettingsSnapshotV1`），命令返回的是 `unknown`：
    // 适配器负责「解析 + 兜底」，让 store 永远拿到形状正确的对象（与各 `parse*` 同口径）。
    load: async () =>
      parseSettingsSnapshot(await invokeCommand<unknown>('settings_get')) ?? SETTINGS_FALLBACK,
    apply: async (patch) =>
      parseSettingsSnapshot(await invokeCommand<unknown>('settings_apply', { patch })) ??
      SETTINGS_FALLBACK,
    resetAll: async () => {
      await invokeCommand<void>('settings_reset_all');
    },
    saveStatus: async () => parseSaveStatus(await invokeCommand<unknown>('save_status')),
    importSave: (file) => invokeCommand<void>('save_command', { command: 'import', file }),
    emotionCommand: (command) =>
      invokeCommand<void>('pet_emotion_command', { command }),
    subscribeConfig: (listener) =>
      listenEvent<unknown>(PET_EVENT.CONFIG, (payload) => listener(parseConfigCmd(payload) ?? {
        version: 1,
        revision: 0,
        changed: [],
        persisted: false,
      })),
  };
}

/** 纯逻辑端口（无 Tauri 运行时的降级实现：只读兜底快照，提交一律失败提示）。 */
export function createNullSettingsPort(): SettingsPort {
  return {
    load: () => Promise.resolve(SETTINGS_FALLBACK),
    apply: () => Promise.reject(new Error('设置服务不可用（未运行在桌面端）')),
    resetAll: () => Promise.reject(new Error('设置服务不可用（未运行在桌面端）')),
    saveStatus: () => Promise.resolve(null),
    importSave: () => Promise.reject(new Error('设置服务不可用（未运行在桌面端）')),
    emotionCommand: () => Promise.reject(new Error('设置服务不可用（未运行在桌面端）')),
    subscribeConfig: () => Promise.resolve(() => undefined),
  };
}

// ---------------------------------------------------------------------------
// React 绑定（`useSyncExternalStore`，React 18 内置；零第三方状态库）
// ---------------------------------------------------------------------------

/** 设置面板 store 的 React 上下文（由 `App.tsx` 提供）。 */
export const SettingsContext = createContext<SettingsStore | null>(null);

/** 取 store（未包裹 Provider 时抛错——架构错误应显式暴露，不静默用兜底）。 */
export function useSettingsStore(): SettingsStore {
  const store = useContext(SettingsContext);
  if (store === null) {
    throw new Error('useSettingsStore 必须在 <SettingsContext.Provider> 内使用');
  }
  return store;
}

/** 订阅状态（任何字段变化都会触发重渲染）。 */
export function useSettingsState(): SettingsStoreState {
  const store = useSettingsStore();
  // 第三个参数为 SSR / 静态渲染用的服务端快照：与客户端一致（同一份内存状态），
  // 缺省会让 `useSyncExternalStore` 在 `renderToStaticMarkup` 下直接抛错
  // （单测与「设置界面结构冒烟」都依赖静态渲染，故必须显式给出）。
  return useSyncExternalStore(store.subscribe, store.getState, store.getState);
}

/**
 * 取当前语言的翻译函数。
 *
 * 语言取自**设置快照**（`appearance.language`）：由 `App` 在加载后调用
 * `store.load()` 取得；切换语言本身也是一次 `patch({ language })`，
 * 故切换后本 hook 自然返回新语言的翻译函数（`01 FR-7-7` 验收）。
 */
export function useTranslator(): Translator {
  const { snapshot } = useSettingsState();
  const locale = resolveLocale(snapshot.language);
  return useMemo(() => createTranslator(locale), [locale]);
}

/**
 * 创建并驱动一个设置面板 store（`App.tsx` 用；也可在测试里直接构造）。
 *
 * @param port    设置端口（默认真实 IPC；测试/纯浏览器环境传假端口）
 * @param options 防抖窗口（测试传 0 立即提交）
 */
export function useSettingsStoreLifecycle(
  port: SettingsPort,
  options: { debounceMs?: number } = {},
): SettingsStore {
  const ref = useRef<SettingsStore | null>(null);
  if (ref.current === null) {
    ref.current = new SettingsStore(port, options);
  }
  const store = ref.current;

  useEffect(() => {
    void store.load();
    return () => store.dispose();
  }, [store]);

  return store;
}

/** 设置面板只读/可写判定（多处复用，收口一处语义）。 */
export function useSettingsWritable(): boolean {
  const { writable, loading } = useSettingsState();
  return writable && !loading;
}
