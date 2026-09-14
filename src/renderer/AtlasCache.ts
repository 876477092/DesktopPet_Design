/**
 * 前端图集缓存：ImageBitmap LRU，与 Rust 侧 LRU 协作（`02 §3` / `03 §4.4`）。
 *
 * 协作口径（S2-M1 要点 3）：
 *   - Rust `dp-assets::cache::LruAtlasCache`：服务端解码帧缓存，64MB 硬上限（C10，2026-09-13 现场修复：原 48MB → 64MB）；
 *   - 本缓存：前端解码位图（ImageBitmap）缓存，**同口径 64MB 预算**，按字节上限
 *     LRU 驱逐、单条超上限拒绝插入（与 Rust 实现逐条对齐）；
 *   - 字节来源经 `atlas_png` 自定义命令（不走 asset 协议网络面，C9）。
 *
 * 可测性：字节预算驱逐与帧子矩形换算抽为纯函数（Map 迭代序即 LRU 序），
 * vitest 单测覆盖 `computeFrameRect` 与驱逐监听器接线（S2-M2 起 runner 已就位）。
 */

/** 前端图集 LRU 字节硬上限：64MB（C10 重估：A 批 29 图集解码后实测 ≈53.5MB，48MB 预算不足，2026-09-13 现场修复）。 */
export const ATLAS_LRU_MAX_BYTES = 64 * 1024 * 1024;

/** 图集帧子矩形（物理像素，与 `WebGLStage.FrameSubRect` 同构）。 */
export interface FrameSubRect {
  /** 子矩形左上 X（相对图集左上）。 */
  readonly sx: number;
  /** 子矩形左上 Y。 */
  readonly sy: number;
  /** 宽。 */
  readonly sw: number;
  /** 高。 */
  readonly sh: number;
}

/**
 * 计算第 `frameIndex` 帧在横向行主序图集中的子矩形（纯函数）。
 *
 * 布局与 Rust `dp-assets::atlas::AtlasMeta::frame_rect` 同构：
 * `col = index % columns`、`row = index / columns`。
 * 入参非法（0 列 / 0 行 / 0 尺寸 / 非整数 / 越界）→ `null`（调用方跳帧降级）。
 */
export function computeFrameRect(
  frameIndex: number,
  columns: number,
  rows: number,
  frameW: number,
  frameH: number,
): FrameSubRect | null {
  const integral =
    Number.isInteger(frameIndex) &&
    Number.isInteger(columns) &&
    Number.isInteger(rows) &&
    Number.isInteger(frameW) &&
    Number.isInteger(frameH);
  if (!integral || columns <= 0 || rows <= 0 || frameW <= 0 || frameH <= 0) {
    return null;
  }
  if (frameIndex < 0 || frameIndex >= columns * rows) {
    return null;
  }
  const col = frameIndex % columns;
  const row = Math.floor(frameIndex / columns);
  return { sx: col * frameW, sy: row * frameH, sw: frameW, sh: frameH };
}

/** 图集字节加载器（main.ts 经 `invokeCommand('atlas_png', { name })` 接线）。 */
export type AtlasByteLoader = (name: string) => Promise<ArrayBuffer | null>;

/**
 * 驱逐监听器（B10）：图集位图被 `close()` 归还显存后回调，参数为被驱逐的图集名。
 *
 * 消费方（FrameRenderer）收到通知后必须失效引用该位图的就绪帧，否则 paint 会
 * 使用已关闭（width/height 归零）的 ImageBitmap 导致绘制异常（>12 图集时 LRU
 * 驱逐是真实风险）。
 */
export type AtlasEvictListener = (name: string) => void;

/** 缓存条目：位图 + 解码后字节占用（`bytes = w × h × 4`）。 */
interface CacheEntry {
  readonly bitmap: ImageBitmap;
  readonly bytes: number;
}

/**
 * ImageBitmap LRU 缓存（字节上限驱逐，与 Rust LRU 同口径）。
 *
 * `get` 命中提升至队首（Map 删除重插 = LRU 触碰）；并发同键解码去重（pending 合并）；
 * 驱逐时 `close()` 位图归还显存。
 */
export class AtlasCache {
  private readonly entries = new Map<string, CacheEntry>();
  private readonly pending = new Map<string, Promise<ImageBitmap | null>>();
  /** B10 驱逐监听器集合（位图 close() 后逐一回调，异常互不影响）。 */
  private readonly evictListeners = new Set<AtlasEvictListener>();
  private totalBytes = 0;

  constructor(
    private readonly loadBytes: AtlasByteLoader,
    private readonly maxBytes: number = ATLAS_LRU_MAX_BYTES,
  ) {}

  /**
   * 注册驱逐监听器（B10）。返回注销函数（幂等）。
   */
  onEvict(listener: AtlasEvictListener): () => void {
    this.evictListeners.add(listener);
    return () => {
      this.evictListeners.delete(listener);
    };
  }

  /** 通知全部驱逐监听器（单个监听器异常不影响其余，防御性收口）。 */
  private notifyEvict(name: string): void {
    for (const listener of this.evictListeners) {
      try {
        listener(name);
      } catch (err) {
        console.warn('[AtlasCache] 驱逐监听器异常：', name, err);
      }
    }
  }

  /** 当前缓存的图集数量。 */
  get size(): number {
    return this.entries.size;
  }

  /** 当前占用字节数。 */
  get bytes(): number {
    return this.totalBytes;
  }

  /**
   * 取图集位图（命中即触碰；未命中经加载器取字节并解码）。
   *
   * 加载/解码失败 → `null`（调用方按 `02 §7.4` 降级：日志 + 跳帧，S2-M2 接占位帧）。
   */
  async get(name: string): Promise<ImageBitmap | null> {
    const hit = this.entries.get(name);
    if (hit !== undefined) {
      this.touch(name, hit);
      return hit.bitmap;
    }

    let inflight = this.pending.get(name);
    if (inflight === undefined) {
      inflight = this.decode(name).finally(() => {
        this.pending.delete(name);
      });
      this.pending.set(name, inflight);
    }
    return inflight;
  }

  /** 清空缓存并释放全部位图（逐条通知驱逐监听器，B10）。 */
  clear(): void {
    for (const [name, entry] of this.entries) {
      entry.bitmap.close();
      this.notifyEvict(name);
    }
    this.entries.clear();
    this.totalBytes = 0;
  }

  /** 取字节 → 解码 → 入缓存（失败路径静默降级为 `null`，告警交调用方）。 */
  private async decode(name: string): Promise<ImageBitmap | null> {
    const buf = await this.loadBytes(name);
    if (buf === null) {
      return null;
    }
    try {
      const bitmap = await createImageBitmap(new Blob([buf], { type: 'image/png' }));
      this.insert(name, bitmap);
      return bitmap;
    } catch (err) {
      console.warn(`[AtlasCache] 图集解码失败：${name}`, err);
      return null;
    }
  }

  /** 插入条目并按字节上限驱逐（单条超上限拒绝，与 Rust LRU 同口径）。 */
  private insert(name: string, bitmap: ImageBitmap): boolean {
    const bytes = bitmap.width * bitmap.height * 4;
    if (this.maxBytes <= 0 || bytes > this.maxBytes) {
      console.warn(`[AtlasCache] 图集 ${name} 占用 ${bytes} 字节超过上限 ${this.maxBytes}，拒绝插入`);
      bitmap.close();
      return false;
    }

    const old = this.entries.get(name);
    if (old !== undefined) {
      this.totalBytes -= old.bytes;
      old.bitmap.close();
      this.notifyEvict(name);
    }
    this.entries.set(name, { bitmap, bytes });
    this.totalBytes += bytes;

    while (this.totalBytes > this.maxBytes) {
      const oldest = this.entries.keys().next();
      if (oldest.done === true) {
        break;
      }
      const victim = this.entries.get(oldest.value);
      this.entries.delete(oldest.value);
      if (victim !== undefined) {
        this.totalBytes -= victim.bytes;
        victim.bitmap.close();
        this.notifyEvict(oldest.value);
        console.warn(`[AtlasCache] LRU 驱逐：${oldest.value}（${victim.bytes} 字节）`);
      }
    }
    return true;
  }

  /** LRU 触碰：删除重插至队首（Map 迭代序 = 访问序）。 */
  private touch(name: string, entry: CacheEntry): void {
    this.entries.delete(name);
    this.entries.set(name, entry);
  }
}
