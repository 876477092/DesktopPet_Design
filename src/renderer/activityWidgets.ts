/**
 * 活动卡 / 明信片挂件（S8-M3，T-10 段 · 下）。
 *
 * 职责（`01 §6.13` 外出活动系统 / `02 §5.16` 明信片）：
 *   1. `ActivityCard`：进行中活动展示（类别名 / 进度条 / 剩余时间 / 召回按钮），
 *      数据源为 `pet://state` 快照的 `activity` 段（1Hz，`02 §7.6` 已登记）；
 *   2. `PostcardWidget`：旅游明信片弹出（`postcardsSent` 增量触发；**D-2：只驻留
 *      1 张**，新到覆盖旧张；默认位置 = 出发前坐标 `origin_vdc`，`None` = 右下角）；
 *   3. 纯函数逻辑（`formatRemaining` / `postcardText` / `nextPostcardMs`）可直测，
 *      DOM 类仅在 `main.ts` 装配期构造一次，不进单测（与 `DomLayers.ts` 同处置）。
 *
 * 约束：C3 前端不读系统时钟（剩余时间直接用快照 `remainingMs`，不换算）；
 * C8 零新增事件（只消费已登记的 `pet://state`）；C9 零网络。召回经回调交出
 * （`main.ts` 调 `invoke('pet_recall')`），不新增事件。
 */

import type { ActivitySnapshotV1 } from '../shared/ipc';

/** 活动卡根类名。 */
const CARD_CLASS = 'activity-card';
/** 明信片根类名。 */
const POSTCARD_CLASS = 'postcard';

/** 类别 → 中文名（C2：非角色名，纯类别词）。 */
const KIND_LABELS: Readonly<Record<string, string>> = {
  work: '打工',
  study: '学习',
  travel: '旅游',
};

/** 类别 → 表情（挂件角标）。 */
const KIND_ICONS: Readonly<Record<string, string>> = {
  work: '💼',
  study: '📖',
  travel: '🏖️',
};

/** 非有限数兜底。 */
function num(value: number, fallback: number): number {
  return Number.isFinite(value) ? value : fallback;
}

/**
 * 剩余毫秒 → 展示文本（纯函数，可单测）。
 *
 * - `remainingMs ≤ 0` → `'即将回来'`；
 * - `≥ 1h` → `'x 小时 y 分'`；否则 `'x 分钟'`（向下取整，秒级不进位）。
 */
export function formatRemaining(remainingMs: number): string {
  const ms = Math.max(0, Math.trunc(num(remainingMs, 0)));
  if (ms === 0) {
    return '即将回来';
  }
  const totalMin = Math.floor(ms / 60_000);
  const hours = Math.floor(totalMin / 60);
  const minutes = totalMin % 60;
  if (hours >= 1) {
    return minutes > 0 ? `${hours} 小时 ${minutes} 分` : `${hours} 小时`;
  }
  return `${minutes} 分钟`;
}

/**
 * 明信片文案（纯函数，可单测）。
 *
 * 与 Rust `dp_activity::postcard::postcard_text` 同义（旅游第 N 张）；前端本地生成，
 * 避免为纯文案走 IPC（`C9` 零新增网络面）。
 */
export function postcardText(defId: string, sent: number): string {
  const n = Math.max(1, Math.trunc(num(sent, 1)));
  if (defId === 'TR-01') {
    return `海边明信片 · 第 ${n} 张——海风把信吹回来了`;
  }
  return `旅行明信片 · 第 ${n} 张`;
}

/**
 * 明信片驻留时长（毫秒；`02 §5.16` D-2 驻留 1 张，弹出后停留可手动关闭）。
 */
export const POSTCARD_DWELL_MS = 20_000;

/**
 * 活动卡 DOM 适配器：承载 `.activity-card` 子树（进行中活动信息 + 召回按钮）。
 *
 * @param root       承载容器 `#pet-overlay-root`
 * @param onRecall   召回按钮回调（`main.ts` 调 `invoke('pet_recall')`，C8）
 */
export class ActivityCard {
  private readonly el: HTMLDivElement;
  private readonly iconEl: HTMLSpanElement;
  private readonly titleEl: HTMLSpanElement;
  private readonly progressEl: HTMLDivElement;
  private readonly timeEl: HTMLSpanElement;
  private readonly recallBtn: HTMLButtonElement;
  private readonly onRecall?: () => void;
  private visible = false;

  constructor(root: HTMLElement, onRecall?: () => void) {
    this.onRecall = onRecall;

    this.el = document.createElement('div');
    this.el.className = CARD_CLASS;
    this.el.style.visibility = 'hidden';
    this.el.style.opacity = '0';

    this.iconEl = document.createElement('span');
    this.iconEl.className = 'activity-card__icon';

    this.titleEl = document.createElement('span');
    this.titleEl.className = 'activity-card__title';

    this.progressEl = document.createElement('div');
    this.progressEl.className = 'activity-card__progress';

    this.timeEl = document.createElement('span');
    this.timeEl.className = 'activity-card__time';

    this.recallBtn = document.createElement('button');
    this.recallBtn.type = 'button';
    this.recallBtn.className = 'activity-card__recall';
    this.recallBtn.textContent = '提前召回';
    this.recallBtn.addEventListener('click', () => this.onRecall?.());

    this.el.append(this.iconEl, this.titleEl, this.progressEl, this.timeEl, this.recallBtn);
    root.appendChild(this.el);
  }

  /** 更新快照（`null` / 未运行 → 隐藏）。 */
  setSnapshot(snap: ActivitySnapshotV1 | null): void {
    if (snap === null || !snap.running || snap.instance === null) {
      this.hide();
      return;
    }
    const ins = snap.instance;
    this.iconEl.textContent = KIND_ICONS[ins.kind] ?? '🏃';
    this.titleEl.textContent = `${KIND_LABELS[ins.kind] ?? ins.kind}中 · ${ins.defId}`;
    this.progressEl.style.setProperty('--progress', String(ins.progressRatio));
    this.timeEl.textContent = formatRemaining(ins.remainingMs);
    this.visible = true;
    this.el.style.visibility = 'visible';
    this.el.style.opacity = '1';
  }

  /** 隐藏（visibility 切换，禁 display:none——与气泡同纪律）。 */
  hide(): void {
    if (!this.visible) {
      return;
    }
    this.visible = false;
    this.el.style.visibility = 'hidden';
    this.el.style.opacity = '0';
  }
}

/**
 * 明信片挂件 DOM 适配器：旅游途中按 `postcardsSent` **增量**弹出明信片。
 *
 * - 只驻留 1 张（新到覆盖旧张，`02 §5.16` D-2）；
 * - 位置固定右下角（`origin_vdc` 快照未透传坐标，取默认，D-2 允许）；
 * - 20s 自动收起 + 手动关闭按钮；
 * - 幂等：同 `postcardsSent` 重复快照不重复弹出。
 *
 * @param root 承载容器 `#pet-overlay-root`
 */
export class PostcardWidget {
  private readonly el: HTMLDivElement;
  private readonly textEl: HTMLSpanElement;
  private readonly closeBtn: HTMLButtonElement;
  private lastSent = 0;
  private lastDefId = '';
  private hideTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(root: HTMLElement) {
    this.el = document.createElement('div');
    this.el.className = POSTCARD_CLASS;
    this.el.style.visibility = 'hidden';
    this.el.style.opacity = '0';

    this.textEl = document.createElement('span');
    this.textEl.className = 'postcard__text';

    this.closeBtn = document.createElement('button');
    this.closeBtn.type = 'button';
    this.closeBtn.className = 'postcard__close';
    this.closeBtn.textContent = '×';
    this.closeBtn.setAttribute('aria-label', '关闭明信片');
    this.closeBtn.addEventListener('click', () => this.dismiss());

    this.el.append(this.textEl, this.closeBtn);
    root.appendChild(this.el);
  }

  /** 更新快照：`postcardsSent` 增加 → 弹出新明信片（增量触发，幂等）。 */
  setSnapshot(snap: ActivitySnapshotV1 | null): void {
    if (snap === null || !snap.running || snap.instance === null) {
      return;
    }
    const ins = snap.instance;
    if (ins.postcardsSent <= this.lastSent && ins.defId === this.lastDefId) {
      return;
    }
    if (ins.postcardsSent < this.lastSent) {
      // 新活动会话（sent 回退）：重置基线，避免上一程的计数误触发。
      this.lastSent = 0;
    }
    this.lastSent = ins.postcardsSent;
    this.lastDefId = ins.defId;

    this.textEl.textContent = postcardText(ins.defId, ins.postcardsSent);
    this.el.style.visibility = 'visible';
    this.el.style.opacity = '1';
    if (this.hideTimer !== null) {
      clearTimeout(this.hideTimer);
    }
    this.hideTimer = setTimeout(() => this.dismiss(), POSTCARD_DWELL_MS);
  }

  /** 收起（清计时器；visibility 切换）。 */
  dismiss(): void {
    if (this.hideTimer !== null) {
      clearTimeout(this.hideTimer);
      this.hideTimer = null;
    }
    this.el.style.visibility = 'hidden';
    this.el.style.opacity = '0';
  }
}
