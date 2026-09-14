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
