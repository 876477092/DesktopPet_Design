import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { listen as tauriListen, type UnlistenFn as TauriUnlistenFn } from '@tauri-apps/api/event';
import type { PetEventName } from './types';

/**
 * IPC（invoke / event）薄封装（S2-M1 重写，替换 S1 占位实现）。
 *
 * 约束：
 * - 禁止引入任何网络相关调用（C9）；capabilities 仅申请 `core:event:default`
 *   （pet://frame 监听，见 `src-tauri/capabilities/default.json` 登记理由）；
 * - 事件名一律取自 `PET_EVENT`（C8）；
 * - 时间相关载荷一律由 Rust 侧产出，前端不自行读取系统时钟（C3——本模块无任何
 *   `Date` / `performance.now` / `SystemTime` 等价物）。
 */

/** 事件监听句柄，用于卸载监听。 */
export type UnlistenFn = () => void;

/**
 * 监听来自内核的事件（`@tauri-apps/api/event` 真实接线，S2-M1）。
 *
 * @param event   事件名，必须是 `02 §7.6` 已登记项（取自 `PET_EVENT`，C8）
 * @param handler 事件回调，载荷为反序列化后的任意结构（由调用方解析）
 * @returns 卸载函数
 */
export async function listenEvent<T>(
  event: PetEventName,
  handler: (payload: T) => void,
): Promise<UnlistenFn> {
  const unlisten: TauriUnlistenFn = await tauriListen<T>(event, (evt) => {
    handler(evt.payload);
  });
  return unlisten;
}

/**
 * 调用内核命令（`@tauri-apps/api/core` 真实接线，S2-M1）。
 *
 * @param command 命令名（snake_case，与 `dp-app` 侧 `#[tauri::command]` 一一对应）
 * @param args    命令参数
 * @returns 命令返回值
 */
export async function invokeCommand<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  return tauriInvoke<T>(command, args);
}

// ---------------------------------------------------------------------------
// RenderFrameCmd v1（`02 §7.6`；Rust 侧定义见 `dp-app/src/bridge.rs`，两端同构）
// ---------------------------------------------------------------------------

/** `RenderFrameCmd` 载荷结构版本（v1；v2 在 S9-M1 升级，本阶段前向兼容）。 */
export const FRAME_CMD_VERSION = 1;

/** `RenderFrameCmd` v1（camelCase 线上格式，与 Rust `#[serde(rename_all = "camelCase")]` 对齐）。 */
export interface RenderFrameCmdV1 {
  /** 载荷结构版本（v1）。 */
  readonly version: number;
  /** 动作 ID（RV-12：`ACT-` 前缀）。 */
  readonly actionId: string;
  /** 图集引用：atlas.json 的 `png` 文件名（经 `atlas_png` 命令读取字节）。 */
  readonly atlasPng: string;
  /** 帧序号（0 起连续，行主序横向图集）。 */
  readonly frameIndex: number;
  /** 横向列数（atlas.json `columns`）。 */
  readonly columns: number;
  /** 行数（atlas.json `rows`）。 */
  readonly rows: number;
  /** 单帧物理宽（atlas.json `frameW`）。 */
  readonly frameW: number;
  /** 单帧物理高（atlas.json `frameH`）。 */
  readonly frameH: number;
  /** 镜像（K-4：true → 渲染端水平翻转）。 */
  readonly mirror: boolean;
  /** 整体不透明度（0.0~1.0）。 */
  readonly alpha: number;
  /** 帧率档位提示（K-4：2~60）。 */
  readonly fps: number;
}

/** 非有限数值兜底为默认值（TS 侧解析容忍，S2-M1 要点 4）。 */
function num(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

/** 非空字符串兜底为默认值。 */
function str(value: unknown, fallback: string): string {
  return typeof value === 'string' && value.length > 0 ? value : fallback;
}

/** 钳制到 [min, max]。 */
function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/**
 * 解析 `pet://frame` 载荷为 `RenderFrameCmd` v1（纯函数，可单测）。
 *
 * 前向兼容策略（与 Rust 侧 `#[serde(default)]` 对齐）：
 * - 缺失字段取默认值（`alpha=1.0`、`fps=6` 等）；
 * - 未知字段直接忽略（v2 新增字段不致解析失败）；
 * - 载荷非对象（null / 数组 / 标量）→ `null`，调用方跳帧降级（`02 §7.4`）。
 */
export function parseRenderFrameCmd(raw: unknown): RenderFrameCmdV1 | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const o = raw as Record<string, unknown>;
  const fps = Math.trunc(clamp(num(o.fps, 6), 2, 60));
  return {
    version: num(o.version, FRAME_CMD_VERSION),
    actionId: str(o.actionId, ''),
    atlasPng: str(o.atlasPng, ''),
    frameIndex: Math.max(0, Math.trunc(num(o.frameIndex, 0))),
    columns: Math.max(0, Math.trunc(num(o.columns, 0))),
    rows: Math.max(0, Math.trunc(num(o.rows, 0))),
    frameW: Math.max(0, Math.trunc(num(o.frameW, 0))),
    frameH: Math.max(0, Math.trunc(num(o.frameH, 0))),
    mirror: typeof o.mirror === 'boolean' ? o.mirror : false,
    alpha: clamp(num(o.alpha, 1.0), 0.0, 1.0),
    fps,
  };
}

// ---------------------------------------------------------------------------
// BubbleCmd v1（`pet://bubble`，`02 §7.6`；Rust 侧生产者为 T-13/S4-M5，两端同构）
// ---------------------------------------------------------------------------

/** `BubbleCmd` 载荷结构版本（v1；前端先行契约，Rust 侧届时镜像本表 `#[serde(rename_all)]`）。 */
export const BUBBLE_CMD_VERSION = 1;

/** 气泡类别（`01 §6.5.4` / §6.12.6 / §6.16.3-4）：提醒 / 求助 / 对话·系统闲聊 / 明信片。 */
export type BubbleKind = 'reminder' | 'help' | 'chat' | 'postcard';

/** 合法类别集合（枚举兜底用；顺序即优先级参考，数值见 `bubblePriority`）。 */
const BUBBLE_KINDS: readonly BubbleKind[] = ['reminder', 'help', 'chat', 'postcard'];

/** 气泡停留时长下界（毫秒，`02 §5 K-5`：单条 3~5s）。 */
export const BUBBLE_DWELL_MIN_MS = 3000;
/** 气泡停留时长上界（毫秒）。 */
export const BUBBLE_DWELL_MAX_MS = 5000;
/** 气泡停留时长默认值（毫秒）。 */
export const BUBBLE_DWELL_DEFAULT_MS = 4000;

/** 气泡快捷按钮（`01 §6.12.6 ④`，如「去喂食 / 稍后」；点击经回调交出，不新增事件）。 */
export interface BubbleActionV1 {
  /** 动作标识（如 `'feed'` / `'later'`）。 */
  readonly id: string;
  /** 按钮文案。 */
  readonly label: string;
}

/**
 * `BubbleCmd` v1（camelCase 线上格式，与 Rust `#[serde(rename_all = "camelCase")]` 对齐）。
 *
 * ⚠️ Rust 侧尚无生产者；本结构为**前端先行契约**（`03 §2 S3-M5`；生产者属 T-13/S4-M5）。
 */
export interface BubbleCmdV1 {
  /** 载荷结构版本（v1）。 */
  readonly version: number;
  /** 文案原文；生产端已按 C2 渲染 `{name}`，前端 `renderPlaceholders` 兜底。 */
  readonly text: string;
  /** 类别（定优先级 / 署名 / 勿扰派生）。 */
  readonly kind: BubbleKind;
  /** 用户交互台词 → 即时覆盖系统台词（`01 §6.5.4`）。 */
  readonly preempt: boolean;
  /** ≥20s 冷却分组键（"同状态"）；空串时消费侧回退 `kind`。 */
  readonly cooldownKey: string;
  /** 停留时长（毫秒，钳到 `[3000,5000]`）。 */
  readonly dwellMs: number;
  /** 显示署名「—— {name}」（缺省由 `kind` 派生；再与用户开关取与）。 */
  readonly showSignature: boolean;
  /** 快捷按钮列表。 */
  readonly actions: readonly BubbleActionV1[];
  /** 高对比（与用户设置取或）。 */
  readonly highContrast: boolean;
}

/** 非布尔兜底为默认值。 */
function bool(value: unknown, fallback: boolean): boolean {
  return typeof value === 'boolean' ? value : fallback;
}

/** 枚举兜底：不在允许集合内（含缺字段 / 类型错）取默认值。 */
function oneOf<T extends string>(value: unknown, allowed: readonly T[], fallback: T): T {
  return typeof value === 'string' && (allowed as readonly string[]).includes(value)
    ? (value as T)
    : fallback;
}

/**
 * 解析 `actions`（`BubbleActionV1[]`）：前向兼容 + 脏项丢弃。
 * - 非数组 → `[]`；
 * - 元素非对象 / `id` 非非空字符串 / `label` 非字符串 → 丢弃该项；
 * - `label` 为空串 → 回退为 `id`。
 */
function parseActions(raw: unknown): BubbleActionV1[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  const out: BubbleActionV1[] = [];
  for (const item of raw) {
    if (item === null || typeof item !== 'object' || Array.isArray(item)) {
      continue;
    }
    const o = item as Record<string, unknown>;
    if (typeof o.id !== 'string' || o.id.length === 0) {
      continue;
    }
    if (typeof o.label !== 'string') {
      continue;
    }
    out.push({ id: o.id, label: o.label.length > 0 ? o.label : o.id });
  }
  return out;
}

/**
 * 解析 `pet://bubble` 载荷为 `BubbleCmd` v1（纯函数，可单测）。
 *
 * 前向兼容策略（与 `parseRenderFrameCmd` 同范式）：
 * - 缺失字段取默认值（`kind='chat'`、`dwellMs=4000` 钳 `[3000,5000]` 等）；
 * - `showSignature` 缺省时由 `kind` 派生（`help`/`postcard` → `true`）；
 * - 未知字段直接忽略；`cooldownKey` 缺省为空串（消费侧回退 `kind`）；
 * - 载荷非对象（null / 数组 / 标量）→ `null`，调用方 `console.warn` + 跳过（`02 §7.4`）。
 *
 * 注：`text` 为空串时**仍返回对象**（与 `FrameRenderer`「解析宽松、消费侧校验」一致，
 * 由消费侧 `decideBubble` 判 `drop`）。
 */
export function parseBubbleCmd(raw: unknown): BubbleCmdV1 | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const o = raw as Record<string, unknown>;
  const kind = oneOf(o.kind, BUBBLE_KINDS, 'chat');
  const showSignature =
    o.showSignature === undefined
      ? kind === 'help' || kind === 'postcard'
      : bool(o.showSignature, false);
  return {
    version: num(o.version, BUBBLE_CMD_VERSION),
    text: str(o.text, ''),
    kind,
    preempt: bool(o.preempt, false),
    cooldownKey: typeof o.cooldownKey === 'string' ? o.cooldownKey : '',
    dwellMs: clamp(num(o.dwellMs, BUBBLE_DWELL_DEFAULT_MS), BUBBLE_DWELL_MIN_MS, BUBBLE_DWELL_MAX_MS),
    showSignature,
    actions: parseActions(o.actions),
    highContrast: bool(o.highContrast, false),
  };
}

// ---------------------------------------------------------------------------
// ParticleCmd v1（`pet://fx`，`02 §7.6` S3-M6 登记；Rust 侧生产者见 `dp-app`
// coreloop 意图映射，两端同构）
// ---------------------------------------------------------------------------

/** `ParticleCmd` 载荷结构版本（v1；本卡即生产者卡，两端同批落地）。 */
export const PARTICLE_CMD_VERSION = 1;

/**
 * 单次迸发上限（`01 附录` / `02 §5.22` ParticlesCfg `maxPerBurst=60`）。
 * 与 Rust 侧 `ParticlesCfg` 同源口径；解析时钳制，层内不再放宽。
 */
export const PARTICLE_BURST_CAP = 60;

/** 粒子类别（`01 附录` / `02 §3` 五类）。 */
export type ParticleKind = 'heart' | 'star' | 'dust' | 'tear' | 'anger';

/** 合法类别集合（枚举兜底用）。 */
const PARTICLE_KINDS: readonly ParticleKind[] = ['heart', 'star', 'dust', 'tear', 'anger'];

/** 缺省迸发数量（载荷缺 `count` 时的兜底；爱心迸发常用档）。 */
const PARTICLE_COUNT_DEFAULT = 12;

/**
 * `ParticleCmd` v1（camelCase 线上格式，与 Rust `#[serde(rename_all = "camelCase")]` 对齐）。
 *
 * 粒子锚点（头顶偏右）由前端按容器尺寸推导（与 `BubbleLayer` anchor 范式一致），
 * 载荷不携带坐标——Rust 侧无需窗口内度量。
 */
export interface ParticleCmdV1 {
  /** 载荷结构版本（v1）。 */
  readonly version: number;
  /** 粒子类别。 */
  readonly kind: ParticleKind;
  /** 迸发数量（解析时钳到 `[1, 60]`）。 */
  readonly count: number;
}

/**
 * 解析 `pet://fx` 载荷为 `ParticleCmd` v1（纯函数，可单测）。
 *
 * 前向兼容策略（与 `parseBubbleCmd` 同范式）：`kind` 不在五类集合内（含缺字段 /
 * 类型错）回退 `'heart'`；`count` 非有限取缺省后钳 `[1, 60]`；未知字段忽略；
 * 载荷非对象 → `null`，调用方 `console.warn` + 跳过（`02 §7.4`）。
 */
export function parseParticleCmd(raw: unknown): ParticleCmdV1 | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const o = raw as Record<string, unknown>;
  return {
    version: num(o.version, PARTICLE_CMD_VERSION),
    kind: oneOf(o.kind, PARTICLE_KINDS, 'heart'),
    count: clamp(Math.trunc(num(o.count, PARTICLE_COUNT_DEFAULT)), 1, PARTICLE_BURST_CAP),
  };
}

// ---------------------------------------------------------------------------
// MenuCmd v1（`pet://menu`，`02 §7.6` S3-M6 登记；Rust 侧生产者为 core-loop
// 右键命中路由，两端同构）
// ---------------------------------------------------------------------------

/** `MenuCmd` 载荷结构版本（v1）。 */
export const MENU_CMD_VERSION = 1;

/**
 * `MenuCmd` v1（camelCase 线上格式，与 Rust `#[serde(rename_all = "camelCase")]` 对齐）。
 *
 * - `screenX` / `screenY`：右键命中点的**屏幕物理像素**（钩子域透传，诊断/跨窗口用）；
 * - `localX` / `localY`：同一命中点换算到**宠物窗口内 CSS 像素**（前端菜单摆位直接用；
 *   物理像素 ÷ 所在屏 scale 由 Rust 侧完成，与 RV-17 换算口径一致）。
 */
export interface MenuCmdV1 {
  /** 载荷结构版本（v1）。 */
  readonly version: number;
  /** 命中点屏幕物理 X。 */
  readonly screenX: number;
  /** 命中点屏幕物理 Y。 */
  readonly screenY: number;
  /** 命中点窗口内 CSS X（菜单摆位锚点）。 */
  readonly localX: number;
  /** 命中点窗口内 CSS Y（菜单摆位锚点）。 */
  readonly localY: number;
}

/**
 * 解析 `pet://menu` 载荷为 `MenuCmd` v1（纯函数，可单测）。
 *
 * 前向兼容策略（与 `parseRenderFrameCmd` 同范式）：坐标字段非有限取 0（菜单摆位
 * 由消费侧 `clampMenuPlacement` 钳回容器内，0 值不会越界弹出）；未知字段忽略；
 * 载荷非对象 → `null`，调用方 `console.warn` + 跳过（`02 §7.4`）。
 */
export function parseMenuCmd(raw: unknown): MenuCmdV1 | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const o = raw as Record<string, unknown>;
  return {
    version: num(o.version, MENU_CMD_VERSION),
    screenX: num(o.screenX, 0),
    screenY: num(o.screenY, 0),
    localX: num(o.localX, 0),
    localY: num(o.localY, 0),
  };
}

// ---------------------------------------------------------------------------
// CoaxCmd v1（`pet://coax`，`02 §7.6` S4-M3 登记；Rust 侧生产者为 `dp-core`
// 事件映射层，两端同构）
// ---------------------------------------------------------------------------

/** `CoaxCmd` 载荷结构版本（v1）。 */
export const COAX_CMD_VERSION = 1;

/** 三部曲子状态（`02 §4.3` `CoaxStep`）。 */
export type CoaxStep = 'idle' | 'call' | 'stroke' | 'heart' | 'runaway' | 'away';

/** 合法子状态集合（枚举兜底用）。 */
const COAX_STEPS: readonly CoaxStep[] = ['idle', 'call', 'stroke', 'heart', 'runaway', 'away'];

/** 三部曲失败原因（`02 §4.3` `CoaxFailReason`）。 */
export type CoaxFailReason = 'interrupted' | 'timeout' | 'abandoned';

/** 合法失败原因集合（枚举兜底用）。 */
const COAX_FAIL_REASONS: readonly CoaxFailReason[] = ['interrupted', 'timeout', 'abandoned'];

/**
 * `CoaxCmd` v1（camelCase 线上格式，与 Rust `#[serde(rename_all = "camelCase")]` 对齐）。
 *
 * 单一事件承载三部曲全部**表现态**：
 * - `active` 为 `true` 时按 `ratio` 显示和好进度环（`step ∈ call/stroke/heart`）；
 * - `active` 为 `false` 时隐藏进度环（`idle` / 成功 / 失败 / 离家段）；
 * - `away` 为 `true` 表示已离家（宠物窗口由 Rust 侧隐藏，前端无需处理）；
 * - `succeeded` / `reason` 供表现层（气泡 / 音效，归 S4-M5 / S4-M6）使用。
 */
export interface CoaxCmdV1 {
  /** 载荷结构版本（v1）。 */
  readonly version: number;
  /** 进度环是否可见。 */
  readonly active: boolean;
  /** 是否已离家（窗口应隐藏）。 */
  readonly away: boolean;
  /** 子状态。 */
  readonly step: CoaxStep;
  /** 进度环比例 0..=1（解析时钳制）。 */
  readonly ratio: number;
  /** 是否三部曲完成。 */
  readonly succeeded: boolean;
  /** 失败原因（成功 / 进行中为 `null`）。 */
  readonly reason: CoaxFailReason | null;
}

/**
 * 解析 `pet://coax` 载荷为 `CoaxCmd` v1（纯函数，可单测）。
 *
 * 前向兼容策略（与 `parseMenuCmd` 同范式）：布尔字段非布尔取 `false`；`step` 不在
 * 六态集合内（含缺字段 / 类型错）回退 `'idle'`；`ratio` 非有限取 0 后钳 `[0,1]`；
 * `reason` 非法 / 缺省 → `null`（不代表成功，成功另有 `succeeded` 标志）；未知字段
 * 忽略；载荷非对象 → `null`，调用方 `console.warn` + 跳过（`02 §7.4`）。
 */
export function parseCoaxCmd(raw: unknown): CoaxCmdV1 | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const o = raw as Record<string, unknown>;
  const reason =
    typeof o.reason === 'string' && (COAX_FAIL_REASONS as readonly string[]).includes(o.reason)
      ? (o.reason as CoaxFailReason)
      : null;
  return {
    version: num(o.version, COAX_CMD_VERSION),
    active: bool(o.active, false),
    away: bool(o.away, false),
    step: oneOf(o.step, COAX_STEPS, 'idle'),
    ratio: clamp(num(o.ratio, 0), 0, 1),
    succeeded: bool(o.succeeded, false),
    reason,
  };
}

// ---------------------------------------------------------------------------
// SettingsSnapshot / SettingsPatch v1（设置页 ⇄ 应用层；`01 FR-7`，S5-M3/M4）
// ---------------------------------------------------------------------------

/** 设置快照载荷版本（v1；与 Rust 侧 `bridge::SETTINGS_VERSION` 同源）。 */
export const SETTINGS_VERSION = 1;

/** 置顶策略三态（`02 K-1`；字面量与 `settings.json` / 存档 B 段一致）。 */
export type TopmostPolicy = 'Always' | 'BelowFullscreen' | 'Never';

/** 合法置顶策略集合。 */
const TOPMOST_POLICIES: readonly TopmostPolicy[] = ['Always', 'BelowFullscreen', 'Never'];

/** 口头禅频率四档（L-03 枚举；作废 `"1:3"` 字符串比例）。 */
export type CatchphraseFrequency = 'off' | 'low' | 'standard' | 'high';

/** 合法频率档位集合。 */
const CATCHPHRASE_FREQUENCIES: readonly CatchphraseFrequency[] = ['off', 'low', 'standard', 'high'];

/** 提醒偏好（`01 FR-10-2`；含取值域，UI 不硬编码数值）。 */
export interface RemindersSnapshotV1 {
  readonly sedentaryEnabled: boolean;
  readonly sedentaryIntervalMin: number;
  readonly waterEnabled: boolean;
  readonly waterIntervalMin: number;
  readonly intervalMinMin: number;
  readonly intervalMaxMin: number;
  readonly ackResetsTimer: boolean;
}

/**
 * 设置快照 v1（`settings_get` 返回；**有效值** = settings.json 出厂默认 ⊕ 存档 B 段）。
 *
 * 约束（`03 S5-M3`「不在 UI 硬编码数值」）：
 * - 上下界 / 步进 / 档位候选值**一律随快照下发**，UI 只渲染不发明；
 * - `writable === false` → 设置服务不可用，UI 进只读预览（改不了但看得见）。
 */
export interface SettingsSnapshotV1 {
  readonly version: number;
  readonly revision: number;
  readonly writable: boolean;
  readonly name: string;
  readonly defaultName: string;
  readonly scalePercent: number;
  readonly scaleMinPercent: number;
  readonly scaleMaxPercent: number;
  readonly scaleStepPercent: number;
  readonly opacityPercent: number;
  readonly opacityMinPercent: number;
  readonly opacityMaxPercent: number;
  readonly language: string;
  readonly masterVolumePercent: number;
  readonly volumeMinPercent: number;
  readonly volumeMaxPercent: number;
  readonly muted: boolean;
  readonly autoRoam: boolean;
  readonly roamPace: number;
  readonly roamPaceOptions: readonly number[];
  readonly doNotDisturb: boolean;
  readonly easyCoaxMode: boolean;
  readonly clickThrough: boolean;
  readonly alwaysOnTopPolicy: TopmostPolicy;
  readonly autostart: boolean;
  readonly sensitivityValue: number;
  readonly sensitivityOptions: readonly number[];
  readonly catchphraseEnabled: boolean;
  readonly catchphraseFrequency: CatchphraseFrequency;
  readonly clickFeedbackEnabled: boolean;
  readonly activitySensing: boolean;
  readonly reminders: RemindersSnapshotV1;
}

/** 提醒偏好补丁（缺省 = 不改）。 */
export interface RemindersPatchV1 {
  readonly sedentaryEnabled?: boolean;
  readonly sedentaryIntervalMin?: number;
  readonly waterEnabled?: boolean;
  readonly waterIntervalMin?: number;
}

/**
 * 设置补丁 v1（`settings_apply` 入参；**只含已定义设置项**）。
 *
 * Rust 侧为 `Option<T>` + `serde(default)`：缺省字段 = 不改；未知字段被忽略
 * （与配置加载同容错口径）。`performance.renderer` 刻意**不在此列**（归 S6-M1）。
 */
export interface SettingsPatchV1 {
  readonly name?: string;
  readonly scalePercent?: number;
  readonly opacityPercent?: number;
  readonly language?: string;
  readonly masterVolumePercent?: number;
  readonly muted?: boolean;
  readonly autoRoam?: boolean;
  readonly roamPace?: number;
  readonly doNotDisturb?: boolean;
  readonly easyCoaxMode?: boolean;
  readonly clickThrough?: boolean;
  readonly alwaysOnTopPolicy?: TopmostPolicy;
  readonly autostart?: boolean;
  readonly sensitivityValue?: number;
  readonly catchphraseEnabled?: boolean;
  readonly catchphraseFrequency?: CatchphraseFrequency;
  readonly clickFeedbackEnabled?: boolean;
  readonly activitySensing?: boolean;
  readonly reminders?: RemindersPatchV1;
}

/** 设置快照兜底（`settings_get` 失败 / 载荷非法时使用；与 Rust `SettingsSnapshot::default` 同值域）。 */
export const SETTINGS_FALLBACK: SettingsSnapshotV1 = {
  version: SETTINGS_VERSION,
  revision: 0,
  writable: false,
  name: '',
  defaultName: '',
  scalePercent: 100,
  scaleMinPercent: 50,
  scaleMaxPercent: 200,
  scaleStepPercent: 10,
  opacityPercent: 100,
  opacityMinPercent: 60,
  opacityMaxPercent: 100,
  language: 'zh-CN',
  masterVolumePercent: 80,
  volumeMinPercent: 0,
  volumeMaxPercent: 100,
  muted: false,
  autoRoam: true,
  roamPace: 1.0,
  roamPaceOptions: [0.7, 1.0, 1.3],
  doNotDisturb: false,
  easyCoaxMode: false,
  clickThrough: false,
  alwaysOnTopPolicy: 'Always',
  autostart: false,
  sensitivityValue: 1.0,
  sensitivityOptions: [0.7, 1.0, 1.3],
  catchphraseEnabled: true,
  catchphraseFrequency: 'standard',
  clickFeedbackEnabled: true,
  activitySensing: true,
  reminders: {
    sedentaryEnabled: true,
    sedentaryIntervalMin: 45,
    waterEnabled: true,
    waterIntervalMin: 45,
    intervalMinMin: 15,
    intervalMaxMin: 180,
    ackResetsTimer: true,
  },
};

/** 解析数值数组（非数组 → 空；元素非有限数 → 丢弃）。 */
function numArray(value: unknown): number[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter((item): item is number => typeof item === 'number' && Number.isFinite(item));
}

/** 解析提醒偏好（缺字段取兜底值；间隔不下钳——上界来自快照自身，避免与配置双真源）。 */
function parseReminders(raw: unknown): RemindersSnapshotV1 {
  const fallback = SETTINGS_FALLBACK.reminders;
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return fallback;
  }
  const o = raw as Record<string, unknown>;
  return {
    sedentaryEnabled: bool(o.sedentaryEnabled, fallback.sedentaryEnabled),
    sedentaryIntervalMin: Math.max(0, Math.trunc(num(o.sedentaryIntervalMin, fallback.sedentaryIntervalMin))),
    waterEnabled: bool(o.waterEnabled, fallback.waterEnabled),
    waterIntervalMin: Math.max(0, Math.trunc(num(o.waterIntervalMin, fallback.waterIntervalMin))),
    intervalMinMin: Math.max(1, Math.trunc(num(o.intervalMinMin, fallback.intervalMinMin))),
    intervalMaxMin: Math.max(1, Math.trunc(num(o.intervalMaxMin, fallback.intervalMaxMin))),
    ackResetsTimer: bool(o.ackResetsTimer, fallback.ackResetsTimer),
  };
}

/**
 * 解析 `settings_get` 返回值为 `SettingsSnapshotV1`（纯函数，可单测）。
 *
 * 前向兼容策略（与既有各 `parse*` 同范式）：缺失 / 类型错的字段取 [`SETTINGS_FALLBACK`]；
 * 枚举字段（置顶策略 / 频率档位）取白名单兜底；未知字段忽略；
 * **载荷非对象 → `null`**，调用方回退 [`SETTINGS_FALLBACK`] + 只读预览。
 */
export function parseSettingsSnapshot(raw: unknown): SettingsSnapshotV1 | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const o = raw as Record<string, unknown>;
  const f = SETTINGS_FALLBACK;
  const options = numArray(o.sensitivityOptions);
  const paceOptions = numArray(o.roamPaceOptions);
  return {
    version: num(o.version, SETTINGS_VERSION),
    revision: Math.max(0, Math.trunc(num(o.revision, 0))),
    writable: bool(o.writable, false),
    name: typeof o.name === 'string' ? o.name : f.name,
    defaultName: typeof o.defaultName === 'string' ? o.defaultName : f.defaultName,
    scalePercent: Math.trunc(num(o.scalePercent, f.scalePercent)),
    scaleMinPercent: Math.trunc(num(o.scaleMinPercent, f.scaleMinPercent)),
    scaleMaxPercent: Math.trunc(num(o.scaleMaxPercent, f.scaleMaxPercent)),
    scaleStepPercent: Math.max(1, Math.trunc(num(o.scaleStepPercent, f.scaleStepPercent))),
    opacityPercent: Math.trunc(num(o.opacityPercent, f.opacityPercent)),
    opacityMinPercent: Math.trunc(num(o.opacityMinPercent, f.opacityMinPercent)),
    opacityMaxPercent: Math.trunc(num(o.opacityMaxPercent, f.opacityMaxPercent)),
    language: str(o.language, f.language),
    masterVolumePercent: Math.trunc(num(o.masterVolumePercent, f.masterVolumePercent)),
    volumeMinPercent: Math.trunc(num(o.volumeMinPercent, f.volumeMinPercent)),
    volumeMaxPercent: Math.trunc(num(o.volumeMaxPercent, f.volumeMaxPercent)),
    muted: bool(o.muted, f.muted),
    autoRoam: bool(o.autoRoam, f.autoRoam),
    roamPace: num(o.roamPace, f.roamPace),
    roamPaceOptions: paceOptions.length > 0 ? paceOptions : f.roamPaceOptions,
    doNotDisturb: bool(o.doNotDisturb, f.doNotDisturb),
    easyCoaxMode: bool(o.easyCoaxMode, f.easyCoaxMode),
    clickThrough: bool(o.clickThrough, f.clickThrough),
    alwaysOnTopPolicy: oneOf(o.alwaysOnTopPolicy, TOPMOST_POLICIES, f.alwaysOnTopPolicy),
    autostart: bool(o.autostart, f.autostart),
    sensitivityValue: num(o.sensitivityValue, f.sensitivityValue),
    sensitivityOptions: options.length > 0 ? options : f.sensitivityOptions,
    catchphraseEnabled: bool(o.catchphraseEnabled, f.catchphraseEnabled),
    catchphraseFrequency: oneOf(o.catchphraseFrequency, CATCHPHRASE_FREQUENCIES, f.catchphraseFrequency),
    clickFeedbackEnabled: bool(o.clickFeedbackEnabled, f.clickFeedbackEnabled),
    activitySensing: bool(o.activitySensing, f.activitySensing),
    reminders: parseReminders(o.reminders),
  };
}

// ---------------------------------------------------------------------------
// SaveStatus v1（设置页「数据」Tab；`save_status` 命令返回值，S5-M4）
// ---------------------------------------------------------------------------

/** 备份档用途分类（与 Rust `bridge::SaveBackupKind` 同词表）。 */
export type SaveBackupKind = 'lastGood' | 'corrupt' | 'future' | 'v1Migration';

/** 合法备份类别集合。 */
const SAVE_BACKUP_KINDS: readonly SaveBackupKind[] = ['lastGood', 'corrupt', 'future', 'v1Migration'];

/** 一份候选备份。 */
export interface SaveBackupV1 {
  /** 备份文件名（导入时原样回传；只允许文件名，防路径穿越）。 */
  readonly file: string;
  /** 用途分类。 */
  readonly kind: SaveBackupKind;
  /** 字节数。 */
  readonly sizeBytes: number;
  /** 是否允许导入。 */
  readonly importable: boolean;
  /** 不可导入的原因键（前端本地化为文案；可导入时为 `null`）。 */
  readonly blockedReason: string | null;
}

/** 存档健康状态（`save_status` 返回值）。 */
export interface SaveStatusV1 {
  readonly path: string;
  readonly exists: boolean;
  readonly state: string;
  readonly healthy: boolean;
  readonly needsNotice: boolean;
  readonly writable: boolean;
  readonly available: boolean;
  readonly lastSeenMs: number;
  readonly backups: readonly SaveBackupV1[];
}

/** 解析备份清单（脏项丢弃：缺 `file` 的一律不要）。 */
function parseBackups(raw: unknown): SaveBackupV1[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  const out: SaveBackupV1[] = [];
  for (const item of raw) {
    if (item === null || typeof item !== 'object' || Array.isArray(item)) {
      continue;
    }
    const o = item as Record<string, unknown>;
    if (typeof o.file !== 'string' || o.file.length === 0) {
      continue;
    }
    out.push({
      file: o.file,
      kind: oneOf(o.kind, SAVE_BACKUP_KINDS, 'lastGood'),
      sizeBytes: Math.max(0, Math.trunc(num(o.sizeBytes, 0))),
      importable: bool(o.importable, false),
      blockedReason: typeof o.blockedReason === 'string' ? o.blockedReason : null,
    });
  }
  return out;
}

/**
 * 解析 `save_status` 返回值为 `SaveStatusV1`（纯函数，可单测）。
 *
 * 载荷非对象 → `null`（调用方显示「存档状态不可用」，不 panic）。
 */
export function parseSaveStatus(raw: unknown): SaveStatusV1 | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const o = raw as Record<string, unknown>;
  return {
    path: typeof o.path === 'string' ? o.path : '',
    exists: bool(o.exists, false),
    state: str(o.state, 'unknown'),
    healthy: bool(o.healthy, false),
    needsNotice: bool(o.needsNotice, true),
    writable: bool(o.writable, false),
    available: bool(o.available, false),
    lastSeenMs: num(o.lastSeenMs, 0),
    backups: parseBackups(o.backups),
  };
}

// ---------------------------------------------------------------------------
// ConfigCmd v1（`pet://config`，`02 §7.6` S5-M4 起启用；Rust 侧生产者为
// `dp_core::event::wire_for_config`，两端同构）
// ---------------------------------------------------------------------------

/** `pet://config` 载荷结构版本（v1；与 Rust `CONFIG_WIRE_VERSION` 同源）。 */
export const CONFIG_CMD_VERSION = 1;

/**
 * `pet://config` 载荷 v1（**摘要**，非全量配置）。
 *
 * 消费口径：只需比较 `revision` 是否变化 → 变化则用 `settings_get` 重新拉全量快照。
 * 这样「每次改动」的广播载荷恒定 ~80B，与配置规模无关。
 */
export interface ConfigCmdV1 {
  /** 载荷结构版本（v1）。 */
  readonly version: number;
  /** 变更序号（单调递增；0 = 首次装载）。 */
  readonly revision: number;
  /** 本次变更涉及的分组名（`pet` / `appearance` / `audio` / `behavior` / `interaction` / `reminders`）。 */
  readonly changed: readonly string[];
  /** 是否已持久化（`false` = 仅内存生效）。 */
  readonly persisted: boolean;
}

/**
 * 解析 `pet://config` 载荷为 `ConfigCmdV1`（纯函数，可单测）。
 *
 * 前向兼容策略：`changed` 非数组 → `[]`（非字符串项丢弃）；未知分组名**原样保留**
 * （消费端忽略即可，便于诊断「新版本发了我不认识的分组」）；载荷非对象 → `null`。
 */
export function parseConfigCmd(raw: unknown): ConfigCmdV1 | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const o = raw as Record<string, unknown>;
  const changed = Array.isArray(o.changed)
    ? o.changed.filter((item): item is string => typeof item === 'string')
    : [];
  return {
    version: num(o.version, CONFIG_CMD_VERSION),
    revision: Math.max(0, Math.trunc(num(o.revision, 0))),
    changed,
    persisted: bool(o.persisted, false),
  };
}

// ---------------------------------------------------------------------------
// ActivitySnapshot v1（`pet://state` 的 `activity` 段；`02 §7.6` 已登记
// `pet://activity` 为迁移事件，本阶段快照驱动——活动卡 / 明信片挂件共用）
// ---------------------------------------------------------------------------

/** 活动阶段（与 Rust `dp_activity::model::ActivityPhase` 同词表）。 */
export type ActivityPhase = 'idle' | 'preparing' | 'running' | 'returning' | 'settled' | 'aborted';

/** 合法阶段集合（枚举兜底用）。 */
const ACTIVITY_PHASES: readonly ActivityPhase[] = [
  'idle',
  'preparing',
  'running',
  'returning',
  'settled',
  'aborted',
];

/** 活动实例（`pet://state.activity.instance`；camelCase 线上格式）。 */
export interface ActivityInstanceV1 {
  /** 活动类别（work / study / travel）。 */
  readonly kind: string;
  /** 定义 ID（W-01 / CRS-01 / TR-01）。 */
  readonly defId: string;
  /** 已完成比例 0..=1（解析时钳制）。 */
  readonly progressRatio: number;
  /** 剩余毫秒（≥0）。 */
  readonly remainingMs: number;
  /** 已收明信片张数（旅游）。 */
  readonly postcardsSent: number;
  /** 深夜延后（D-1：end 推到次日 07:00）。 */
  readonly deferredSettle: boolean;
}

/** `pet://state.activity` 段（v1）。 */
export interface ActivitySnapshotV1 {
  /** 阶段。 */
  readonly phase: ActivityPhase;
  /** 是否有进行中的活动（`phase ∈ preparing/running/returning`）。 */
  readonly running: boolean;
  /** 活动实例（`null` = 无）。 */
  readonly instance: ActivityInstanceV1 | null;
}

/**
 * 解析 `pet://state` 载荷的 `activity` 段为 `ActivitySnapshotV1`（纯函数，可单测）。
 *
 * 前向兼容策略（与既有 `parse*` 同范式）：`phase` 不在六态集合内回退 `'idle'`；
 * `instance` 非对象 → `null`；数值字段非有限取 0；未知字段忽略；载荷非对象 → `null`，
 * 调用方 `console.warn` + 跳过（`02 §7.4`）。
 */
export function parseActivitySnapshot(raw: unknown): ActivitySnapshotV1 | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const o = raw as Record<string, unknown>;
  const phase = oneOf(o.phase, ACTIVITY_PHASES, 'idle');
  let instance: ActivityInstanceV1 | null = null;
  if (o.instance !== null && typeof o.instance === 'object' && !Array.isArray(o.instance)) {
    const ins = o.instance as Record<string, unknown>;
    instance = {
      kind: typeof ins.kind === 'string' ? ins.kind : '',
      defId: typeof ins.defId === 'string' ? ins.defId : '',
      progressRatio: clamp(num(ins.progressRatio, 0), 0, 1),
      remainingMs: Math.max(0, Math.trunc(num(ins.remainingMs, 0))),
      postcardsSent: Math.max(0, Math.trunc(num(ins.postcardsSent, 0))),
      deferredSettle: bool(ins.deferredSettle, false),
    };
  }
  return {
    phase,
    running: bool(o.running, phase !== 'idle' && phase !== 'settled' && phase !== 'aborted'),
    instance,
  };
}

// ---------------------------------------------------------------------------
// PetSnapshotV1（`pet://state`；S10-M1 引入前端消费面；Rust 真源见
// `dp_core::event::PetSnapshotV2`，两端同构，serde camelCase）
// ---------------------------------------------------------------------------

/** 六维数值投影（`values` 段）。 */
export interface ValuesSnapshotV1 {
  /** 心情 0~100。 */
  readonly mood: number;
  /** 体力 0~100。 */
  readonly energy: number;
  /** 展示无聊度 0~100。 */
  readonly boredom: number;
  /** 饱食度 0~100。 */
  readonly satiety: number;
  /** 清洁度 0~100。 */
  readonly cleanliness: number;
  /** 亲密度等级 Lv1~10。 */
  readonly affinityLevel: number;
  /** 亲密度当前级内经验。 */
  readonly affinityExp: number;
  /** 升级所需经验（满级为 0）。 */
  readonly affinityExpNext: number;
}

/** 冷落压力七因子投影（`neglect.factors`）。 */
export interface FactorsSnapshotV1 {
  readonly presence: number;
  readonly busyness: number;
  readonly personality: number;
  readonly rhythm: number;
  readonly needs: number;
  readonly rough: number;
  readonly adapt: number;
  /** 原始乘积（未乘敏感度）。 */
  readonly product: number;
}

/** 原因卡条目方向。 */
export type NeglectReasonDir = 'faster' | 'slower';

/** 原因卡条目（`neglect.reasons`；S10-M2 ReasonCard 消费）。 */
export interface NeglectReasonV1 {
  readonly factorKey: string;
  readonly label: string;
  readonly weight: number;
  readonly dir: NeglectReasonDir;
  readonly advice?: string;
}

/** 冷落压力投影（`neglect`）。 */
export interface NeglectSnapshotV1 {
  /** 当前 P。 */
  readonly p: number;
  /** 当前封顶。 */
  readonly cap: number;
  /** 生效档位 0..=5。 */
  readonly level: number;
  /** 生效速率（含敏感度）。 */
  readonly ratePerMin: number;
  readonly factors: FactorsSnapshotV1;
  readonly sensitivity: { readonly value: number };
  readonly reasons: readonly NeglectReasonV1[];
}

/** 经济投影（`economy`）。 */
export interface EconomySnapshotV1 {
  /** 心币余额。 */
  readonly coin: number;
  /** 今日已赚。 */
  readonly todayEarned: number;
  /** 每日入账硬顶。 */
  readonly dailyCap: number;
}

/** 背包条目。 */
export interface InventoryItemV1 {
  readonly itemId: string;
  readonly qty: number;
}

/** 性格投影（`personality`）。 */
export interface PersonalitySnapshotV1 {
  readonly text: string;
  readonly rerollLeft: number;
  readonly canReroll: boolean;
}

/**
 * `pet://state` 全量快照（S10-M1：属性 / 活动 / 商城 / 背包 / 相册·装饰 / 原因卡的
 * 唯一数据源；Rust 真源 `dp_core::event::PetSnapshotV2`）。
 *
 * - `decor` 恒为长度 5 的数组，空位为 `null`，占用为商品 ID（`02 §5 DECOR_SLOTS=5`）；
 * - `album` 为照片条目数组（形状由后端决定，前端只读渲染，不假设字段）；
 * - `state` 为展示情绪档（idle / happy / ...，仅用于原因卡显隐）。
 */
export interface PetSnapshotV2 {
  readonly v: number;
  readonly values: ValuesSnapshotV1;
  readonly neglect: NeglectSnapshotV1;
  readonly personality: PersonalitySnapshotV1;
  readonly activity: ActivitySnapshotV1 | null;
  readonly economy: EconomySnapshotV1;
  readonly inventory: readonly InventoryItemV1[];
  readonly decor: readonly (string | null)[];
  readonly album: readonly unknown[];
  readonly state: string;
}

/** 解析七因子（缺字段取 1.0，与 Rust `FactorsSnapshot::default` 同值域）。 */
function parseFactors(raw: unknown): FactorsSnapshotV1 {
  const o = (raw ?? {}) as Record<string, unknown>;
  const one = (key: string): number => num(o[key], 1.0);
  return {
    presence: one('presence'),
    busyness: one('busyness'),
    personality: one('personality'),
    rhythm: one('rhythm'),
    needs: one('needs'),
    rough: one('rough'),
    adapt: one('adapt'),
    product: num(o.product, 1.0),
  };
}

/** 解析原因卡条目数组（脏项丢弃）。 */
function parseReasons(raw: unknown): NeglectReasonV1[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  const out: NeglectReasonV1[] = [];
  for (const item of raw) {
    if (item === null || typeof item !== 'object' || Array.isArray(item)) {
      continue;
    }
    const o = item as Record<string, unknown>;
    out.push({
      factorKey: str(o.factorKey, ''),
      label: str(o.label, ''),
      weight: num(o.weight, 0),
      dir: oneOf(o.dir, ['faster', 'slower'] as const, 'faster'),
      advice: typeof o.advice === 'string' && o.advice.length > 0 ? o.advice : undefined,
    });
  }
  return out;
}

/**
 * 解析 `pet://state` 载荷为 [`PetSnapshotV2`]（纯函数，可单测）。
 *
 * 前向兼容策略（与既有各 `parse*` 同范式）：缺段取 Rust 同名默认；`activity` 复用
 * [`parseActivitySnapshot`]；`decor` 非数组 → 空 5 槽全 null；`album` 非数组 → 空；
 * 未知字段忽略；载荷非对象 → `null`（调用方保留上一份已知快照，不闪空白）。
 */
export function parsePetSnapshotV2(raw: unknown): PetSnapshotV2 | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const o = raw as Record<string, unknown>;

  const valuesRaw = (o.values ?? {}) as Record<string, unknown>;
  const values: ValuesSnapshotV1 = {
    mood: num(valuesRaw.mood, 0),
    energy: num(valuesRaw.energy, 0),
    boredom: num(valuesRaw.boredom, 0),
    satiety: num(valuesRaw.satiety, 0),
    cleanliness: num(valuesRaw.cleanliness, 0),
    affinityLevel: Math.max(0, Math.trunc(num(valuesRaw.affinityLevel, 1))),
    affinityExp: num(valuesRaw.affinityExp, 0),
    affinityExpNext: num(valuesRaw.affinityExpNext, 0),
  };

  const neglectRaw = (o.neglect ?? {}) as Record<string, unknown>;
  const sensRaw = (neglectRaw.sensitivity ?? {}) as Record<string, unknown>;
  const neglect: NeglectSnapshotV1 = {
    p: num(neglectRaw.p, 0),
    cap: num(neglectRaw.cap, 0),
    level: Math.max(0, Math.min(5, Math.trunc(num(neglectRaw.level, 0)))),
    ratePerMin: num(neglectRaw.ratePerMin, 0),
    factors: parseFactors(neglectRaw.factors),
    sensitivity: { value: num(sensRaw.value, 1.0) },
    reasons: parseReasons(neglectRaw.reasons),
  };

  const persRaw = (o.personality ?? {}) as Record<string, unknown>;
  const personality: PersonalitySnapshotV1 = {
    text: typeof persRaw.text === 'string' ? persRaw.text : '',
    rerollLeft: Math.max(0, Math.trunc(num(persRaw.rerollLeft, 0))),
    canReroll: bool(persRaw.canReroll, false),
  };

  const activity = parseActivitySnapshot(o.activity);

  const econRaw = (o.economy ?? {}) as Record<string, unknown>;
  const economy: EconomySnapshotV1 = {
    coin: Math.trunc(num(econRaw.coin, 0)),
    todayEarned: Math.max(0, Math.trunc(num(econRaw.todayEarned, 0))),
    dailyCap: Math.max(0, Math.trunc(num(econRaw.dailyCap, 0))),
  };

  const inventory: InventoryItemV1[] = Array.isArray(o.inventory)
    ? (o.inventory as unknown[])
        .filter(
          (it): it is Record<string, unknown> =>
            it !== null && typeof it === 'object' && !Array.isArray(it),
        )
        .map((it) => ({
          itemId: str(it.itemId, ''),
          qty: Math.max(0, Math.trunc(num(it.qty, 0))),
        }))
        .filter((it) => it.itemId.length > 0)
    : [];

  // decor 恒 5 槽；非数组 / 脏项 → null。
  const decorRaw = Array.isArray(o.decor) ? o.decor : [];
  const decor: (string | null)[] = Array.from({ length: 5 }, (_, i) => {
    const slot = decorRaw[i];
    return typeof slot === 'string' && slot.length > 0 ? slot : null;
  });

  const album: unknown[] = Array.isArray(o.album) ? [...o.album] : [];

  return {
    v: Math.max(0, Math.trunc(num(o.v, 0))),
    values,
    neglect,
    personality,
    activity,
    economy,
    inventory,
    decor,
    album,
    state: typeof o.state === 'string' ? o.state : 'idle',
  };
}
