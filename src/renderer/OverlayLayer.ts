/**
 * 叠加层状态机（S3-M5，T-10 段 · 上；`02 §3` OverlayLayer.ts / `03 §2 S3-M5`）。
 *
 * 职责：五要素（`01`）的显示状态管理，经极窄端口 `OverlayView` 落 DOM：
 *   - 和好进度环 `setCoaxProgress(v|null)`（**回退 50% 由上游 CoaxFlow 算**，本层只显示，
 *     绝不参与结算——防双份真相，吸取 F-05 教训；**S4-M3 起生产者为 `pet://coax`**，
 *     订阅接线在 `src/main.ts`）；
 *   - 怒气符号 `setAngerLevel(n)`（`0` 隐藏 / `≥1` 显示）；
 *   - Zzz `setSleepLevel(0|1|2)`（非法档钳制 + `console.warn`，`02 §7.4` 不静默）；
 *   - 成就飘条 `enqueueToast(text)`（FIFO、同屏 1、队列上限 3、超出丢弃且告警）；
 *   - 情绪原因卡入口 `pressMoodBar/releaseMoodBar`（长按 500ms → `onReasonCardRequest`
 *     回调，**零新增事件**，C8；卡本体归 T-26/S10-M2）。
 *
 * ⚠️ **本层禁 `setTimeout`**：飘条出队 / 进度推进不靠定时器，靠 `flush()` 被
 * `LayerHost.render()`（Rust 帧 tick）驱动。**唯一时间真源 = 注入 `now()`**（C3）。
 *
 * 惰性 flush（主理人裁定 §4.7）：五要素 setter **只置脏**，`flush()` 统一写 view；
 * 写前与上次写入值比对，相同则跳过（幂等、60Hz 廉价）。
 */

import type { OverlayView } from './layerPorts';
import { TOAST_MS, TOAST_QUEUE_CAP } from './layerPorts';
import {
  clampToastQueue,
  isReasonCardLongPress,
  normalizeSleepLevel,
  toastProgress,
} from './overlayLogic';

/** 构造选项（全部可选）。 */
export interface OverlayLayerOptions {
  /** 单调时钟读数（默认 `performance.now`；C3）。 */
  now?: () => number;
  /** 情绪原因卡请求回调（长按心情条触发；接线归 T-26）。 */
  onReasonCardRequest?: () => void;
}

/** 进行中的飘条。 */
interface ActiveToast {
  readonly text: string;
  readonly startAt: number;
}

/**
 * 叠加层（五要素状态 + 惰性 flush）。
 *
 * @param view 极窄端口（真实 `DomOverlayView` / 测试 `FakeOverlayView`）
 * @param opts 构造选项（见 [`OverlayLayerOptions`]）
 */
export class OverlayLayer {
  private readonly now: () => number;
  private readonly onReasonCardRequest: (() => void) | undefined;

  // —— 三要素（setter 只置脏，flush 统一写 view）——
  private coax: number | null = null;
  private anger = 0;
  private sleep: 0 | 1 | 2 = 0;
  private coaxDirty = true; // 初始置脏：首次 flush 同步一次视图默认态。
  private angerDirty = true;
  private sleepDirty = true;

  // —— 飘条：FIFO 等待队列 + 同屏 1 的当前项 ——
  private toastQueue: string[] = [];
  private currentToast: ActiveToast | null = null;

  // —— 情绪原因卡长按入口 ——
  private pressAt: number | null = null;

  // —— 上次写入 view 的值（幂等比对；`undefined` = 从未写过）——
  private lastCoax: number | null | undefined = undefined;
  private lastAnger: number | undefined = undefined;
  private lastSleep: 0 | 1 | 2 | undefined = undefined;
  private lastToast: string | null | undefined = undefined;
  private lastToastOpacity: number | undefined = undefined;

  constructor(
    private readonly view: OverlayView,
    opts: OverlayLayerOptions = {},
  ) {
    this.now = opts.now ?? ((): number => performance.now());
    this.onReasonCardRequest = opts.onReasonCardRequest;
  }

  /** 和好进度环（`null` 隐藏；数值钳 [0,1]，仅显示不结算）。 */
  setCoaxProgress(value01: number | null): void {
    this.coax =
      value01 === null || !Number.isFinite(value01)
        ? null
        : Math.min(1, Math.max(0, value01));
    this.coaxDirty = true;
  }

  /** 怒气符号（`0` 隐藏 / `≥1` 显示；负值 / 非有限钳为 0）。 */
  setAngerLevel(level: number): void {
    this.anger = Number.isFinite(level) ? Math.max(0, level) : 0;
    this.angerDirty = true;
  }

  /** Zzz 档（非法档钳到 `0|1|2` 并 `console.warn`——不静默，`02 §7.4`）。 */
  setSleepLevel(level: 0 | 1 | 2): void {
    const normalized = normalizeSleepLevel(level);
    if (normalized.invalid) {
      console.warn('[OverlayLayer] setSleepLevel 收到非法档位，已钳制：', level);
    }
    this.sleep = normalized.level;
    this.sleepDirty = true;
  }

  /**
   * 入队一条成就飘条（FIFO；等待队列上限 [`TOAST_QUEUE_CAP`]，超出丢弃并
   * `console.warn` 不静默；同屏并发恒 1，由 flush 逐条推进）。
   */
  enqueueToast(text: string): void {
    const next = clampToastQueue([...this.toastQueue, text], TOAST_QUEUE_CAP);
    if (next.length <= this.toastQueue.length) {
      console.warn('[OverlayLayer] 成就飘条等待队列已满（上限', TOAST_QUEUE_CAP, '），丢弃：', text);
      return;
    }
    this.toastQueue = [...next];
  }

  /** 心情条按下（记录按下时刻；时长判定在抬起时惰性求值）。 */
  pressMoodBar(): void {
    this.pressAt = this.now();
  }

  /**
   * 心情条抬起：长按 ≥ [`REASON_CARD_LONG_PRESS_MS`]（默认 500ms）→ 触发
   * `onReasonCardRequest` **且只触发一次**（未按下 / 重复抬起均不触发）。
   */
  releaseMoodBar(): void {
    const pressedAt = this.pressAt;
    this.pressAt = null;
    if (pressedAt === null) {
      return;
    }
    if (isReasonCardLongPress(this.now() - pressedAt)) {
      this.onReasonCardRequest?.();
    }
  }

  /**
   * 惰性 flush（`LayerHost` draw 回调；每帧调用须廉价且幂等）。
   *
   * - 无脏且无进行中 / 等待飘条 → 立即返回（零 view 写入）；
   * - 飘条：到点出队（`setToast(null)`）→ 取队首 → 按 `toastProgress` 推进透明度；
   * - 三要素：写前与上次值比对，相同则跳过。
   */
  flush(): void {
    const now = this.now();
    const hasToastWork = this.currentToast !== null || this.toastQueue.length > 0;
    if (!this.coaxDirty && !this.angerDirty && !this.sleepDirty && !hasToastWork) {
      return;
    }

    // 1) 到点出队（惰性：不靠定时器，靠帧 tick）。
    let expired = false;
    if (this.currentToast !== null && now - this.currentToast.startAt >= TOAST_MS) {
      this.currentToast = null;
      expired = true;
    }
    // 2) 取队首（同屏 1；FIFO）。
    if (this.currentToast === null && this.toastQueue.length > 0) {
      const next = this.toastQueue.shift();
      if (next !== undefined) {
        this.currentToast = { text: next, startAt: now };
      }
    }
    // 3) 推进当前飘条（或刚出队且无后续 → 隐藏）。
    if (this.currentToast !== null) {
      this.writeToast(this.currentToast.text);
      this.writeToastOpacity(toastProgress(now - this.currentToast.startAt, TOAST_MS));
    } else if (expired) {
      this.writeToast(null);
      this.writeToastOpacity(0);
    }

    // 4) 三要素（幂等写）。
    this.writeCoax(this.coax);
    this.coaxDirty = false;
    this.writeAnger(this.anger);
    this.angerDirty = false;
    this.writeSleep(this.sleep);
    this.sleepDirty = false;
  }

  /** 幂等写进度环。 */
  private writeCoax(v: number | null): void {
    if (v === this.lastCoax) {
      return;
    }
    this.view.setCoaxProgress(v);
    this.lastCoax = v;
  }

  /** 幂等写怒气。 */
  private writeAnger(v: number): void {
    if (v === this.lastAnger) {
      return;
    }
    this.view.setAngerLevel(v);
    this.lastAnger = v;
  }

  /** 幂等写 Zzz。 */
  private writeSleep(v: 0 | 1 | 2): void {
    if (v === this.lastSleep) {
      return;
    }
    this.view.setSleepLevel(v);
    this.lastSleep = v;
  }

  /** 幂等写飘条文案（`null` = 隐藏）。 */
  private writeToast(v: string | null): void {
    if (v === this.lastToast) {
      return;
    }
    this.view.setToast(v);
    this.lastToast = v;
  }

  /** 幂等写飘条透明度。 */
  private writeToastOpacity(v: number): void {
    if (v === this.lastToastOpacity) {
      return;
    }
    this.view.setToastOpacity(v);
    this.lastToastOpacity = v;
  }
}
