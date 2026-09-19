/**
 * 表情差分（FR-15-8，S9-M4；PRD 附录 A.5 共 20 个）。
 *
 * - 前 12 个为核心（v1.0+v1.1）；后 8 个（#13~#20）为 P2 分批项，本卡登记目录、
 *   标记 `deferred`，接入随正式美术资产分批（不阻塞核心切换）。
 * - 骨骼路径：表情 = head 槽位 attachment 切 UV（换装同构，增量 ≈0）；
 *   帧回退：选对应表情帧变体。两条路径共用本目录与热区语义（AC-10）。
 */

/** 单条表情差分（PRD A.5）。 */
export interface Expression {
  /** 稳定 id（slug）。 */
  readonly id: string;
  /** 中文名。 */
  readonly name: string;
  /** 挂点槽位（`head`，PRD A.5）。 */
  readonly slot: 'head';
  /** 对应状态/动作（A.5 列）。 */
  readonly trigger: string;
  /** P2 延后（后 8 个）。 */
  readonly deferred: boolean;
}

/** 20 表情目录（PRD A.5 全量；#13~#20 deferred）。 */
export const EXPRESSIONS: readonly Expression[] = [
  { id: 'calm', name: '平静', slot: 'head', trigger: 'ACT-M-01', deferred: false },
  { id: 'curious', name: '好奇', slot: 'head', trigger: 'ACT-T-10/ACT-P-01/04', deferred: false },
  { id: 'happy', name: '开心', slot: 'head', trigger: 'ACT-E-01/ACT-M-04', deferred: false },
  { id: 'shy', name: '害羞脸红', slot: 'head', trigger: 'ACT-T-03', deferred: false },
  { id: 'bored', name: '无聊', slot: 'head', trigger: 'ACT-I-03', deferred: false },
  { id: 'tearful', name: '委屈含泪', slot: 'head', trigger: 'ACT-E-02', deferred: false },
  { id: 'sulking', name: '生闷气', slot: 'head', trigger: 'ACT-E-03', deferred: false },
  { id: 'angry', name: '生气冒烟', slot: 'head', trigger: 'ACT-E-04', deferred: false },
  { id: 'sleepy', name: '困倦', slot: 'head', trigger: 'ACT-I-02', deferred: false },
  { id: 'sleeping', name: '睡眠', slot: 'head', trigger: 'ACT-I-05', deferred: false },
  { id: 'surprise', name: '惊喜', slot: 'head', trigger: 'ACT-S-04/ACT-E-06', deferred: false },
  { id: 'glare_back', name: '回头瞪', slot: 'head', trigger: 'ACT-E-05', deferred: false },
  { id: 'beg_feed', name: '讨食', slot: 'head', trigger: 'ACT-N-01', deferred: true },
  { id: 'eating', name: '吃饭', slot: 'head', trigger: 'ACT-N-02', deferred: true },
  { id: 'fed', name: '吃饱满足', slot: 'head', trigger: 'ACT-N-03', deferred: true },
  { id: 'dirty', name: '脏污嫌弃', slot: 'head', trigger: 'ACT-N-04/05', deferred: true },
  { id: 'bathing', name: '洗澡舒服', slot: 'head', trigger: 'ACT-N-07', deferred: true },
  { id: 'working', name: '打工认真', slot: 'head', trigger: 'ACT-N-09/W-*', deferred: true },
  { id: 'travel', name: '旅游兴奋', slot: 'head', trigger: 'ACT-N-12/TR-*', deferred: true },
  { id: 'money_grin', name: '数钱奸笑', slot: 'head', trigger: 'ACT-N-15', deferred: true },
];

/** 表情切换接收器（骨骼适配器 setSkin / 帧变体选择）。 */
export interface ExpressionSink {
  /** 应用某表情（head 槽 UV 切换）。 */
  applyExpression(id: string): void;
}

/**
 * 表情控制器：校验目录 → 派发到 sink。
 *
 * 未登记 id → 拒绝并告警（不静默切错表情）；deferred 表情登记但按 P2 分批。
 */
export class ExpressionController {
  private currentId = 'calm';

  constructor(private readonly sink: ExpressionSink) {
    this.sink.applyExpression(this.currentId);
  }

  /** 当前表情 id。 */
  get current(): string {
    return this.currentId;
  }

  /** 全部可切换表情（20 个全量登记；deferred 标记 P2 分批）。 */
  static all(): readonly Expression[] {
    return EXPRESSIONS;
  }

  /** 核心表情数（前 12）。 */
  static coreCount(): number {
    return EXPRESSIONS.filter((e) => !e.deferred).length;
  }

  /**
   * 切表情。
   * @returns 是否成功（未登记 id 返回 false）。
   */
  set(id: string): boolean {
    const expr = EXPRESSIONS.find((e) => e.id === id);
    if (expr === undefined) {
      console.warn(`[ExpressionController] 未登记表情：${id}，保持 ${this.currentId}`);
      return false;
    }
    this.currentId = expr.id;
    this.sink.applyExpression(expr.id);
    return true;
  }
}

// ---------------------------------------------------------------------------
// bbox 热区（AC-10 复测：狐耳次级热区悬停有效，点击落主判定）
// ---------------------------------------------------------------------------

/** 归一化热区矩形（u∈[0,1] 横、v∈[0,1] 纵，与帧/骨骼坐标系一致）。 */
export interface HotzoneRect {
  readonly u0: number;
  readonly v0: number;
  readonly u1: number;
  readonly v1: number;
}

/** 热区判定结果。 */
export type HotzoneHit = 'ear' | 'body' | null;

/** 狐耳次级热区（头顶两侧；PRD L5 头部挂点）。 */
export const EAR_HOTZONE: HotzoneRect = { u0: 0.3, v0: 0.0, u1: 0.7, v1: 0.3 };
/** 主判定区（躯干；点击落此）。 */
export const BODY_HOTZONE: HotzoneRect = { u0: 0.15, v0: 0.25, u1: 0.85, v1: 1.0 };

function inRect(u: number, v: number, r: HotzoneRect): boolean {
  return u >= r.u0 && u <= r.u1 && v >= r.v0 && v <= r.v1;
}

/**
 * 解析热区（AC-10 复测口径）：
 *   - 狐耳次级热区命中 → `'ear'`（悬停有效）；
 *   - 其余主判定区命中 → `'body'`（点击落主判定）；
 *   - 都不中 → `null`（穿透）。
 *
 * `mirror` 时 u 按 `1-u` 镜像（与帧路径 `frameW - 1 - x`、骨骼 skelhit `hit_mirrored` 同语义）。
 */
export function resolveHotzone(u: number, v: number, mirror: boolean): HotzoneHit {
  const uu = mirror ? 1 - u : u;
  if (inRect(uu, v, EAR_HOTZONE)) {
    return 'ear';
  }
  if (inRect(uu, v, BODY_HOTZONE)) {
    return 'body';
  }
  return null;
}
