/**
 * 活动派遣目录展示面（S10-M1；`01 §8.4` 打工 / 学习 / 旅游；`02 §5.13`）。
 *
 * 与 `shopCatalog.ts` 同纪律：`resources/config/activities.json` 是静态配置，前端只
 * 渲染派遣按钮；**前置校验与结算权威全在 core-loop**（`pet_dispatch` 在 Rust 侧以当时
 * 内核快照组装 `DispatchCheck`，拒绝只记日志）。前端不校验体力 / 亲密度 / 安静时段。
 *
 * `kind` 与 Rust `pet_dispatch` 第一参数同取值域：`work | study | travel`。
 */
import rawActivities from '../../resources/config/activities.json';

/** 一类可派遣活动的一个选项。 */
export interface DispatchOption {
  /** 类别（Rust `pet_dispatch` kind）。 */
  readonly kind: 'work' | 'study' | 'travel';
  /** 定义 ID（`W-01` / `CRS-01` / `TR-01`）。 */
  readonly defId: string;
  /** 展示名。 */
  readonly name: string;
  /** 可选时长（分钟；旅游为配置固定值，取首项）。 */
  readonly durations: readonly number[];
}

function asString(v: unknown, fallback = ''): string {
  return typeof v === 'string' ? v : fallback;
}

function asNumber(v: unknown, fallback = 0): number {
  return typeof v === 'number' && Number.isFinite(v) ? v : fallback;
}

function asDurationOptions(raw: unknown, fixed: number): number[] {
  if (Array.isArray(raw) && raw.length > 0) {
    const nums = raw.map((n) => Math.trunc(asNumber(n))).filter((n) => n > 0);
    if (nums.length > 0) return nums;
  }
  return fixed > 0 ? [fixed] : [];
}

/** 把 `activities.json` 规范化为三类派遣选项（脏项丢弃；永不抛错）。 */
function normalize(raw: unknown): DispatchOption[] {
  const root = (raw ?? {}) as { activity?: Record<string, unknown> };
  const act = root.activity ?? {};
  const out: DispatchOption[] = [];

  const push = (
    kind: DispatchOption['kind'],
    items: unknown,
    durKey: string,
    fixedKey?: string,
  ) => {
    if (!Array.isArray(items)) return;
    for (const it of items as Record<string, unknown>[]) {
      if (it === null || typeof it !== 'object') continue;
      const id = asString(it.id);
      if (id.length === 0) continue;
      const fixed = fixedKey !== undefined ? asNumber(it[fixedKey]) : 0;
      out.push({
        kind,
        defId: id,
        name: asString(it.name, id),
        durations: asDurationOptions(it[durKey], fixed),
      });
    }
  };

  push('work', act.jobs, 'durationOptions');
  push('study', act.courses, 'durationOptions');
  push('travel', act.trips, 'durationOptions', 'durationMin');
  return out;
}

/** 全部可派遣活动（构建期静态打包）。 */
export const DISPATCH_OPTIONS: readonly DispatchOption[] = normalize(rawActivities);

/** 按类别过滤。 */
export function optionsByKind(kind: DispatchOption['kind']): readonly DispatchOption[] {
  return DISPATCH_OPTIONS.filter((o) => o.kind === kind);
}
