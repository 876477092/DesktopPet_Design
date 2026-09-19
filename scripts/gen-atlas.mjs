#!/usr/bin/env node
/**
 * gen-atlas.mjs —— 序列帧 → 横向图集 + atlas.json（S1-M5 真实实现，`02 §7.7`）。
 *
 * 行为：
 *   1. 递归扫描 `--src` 目录下命名规范为 `action_state_direction_frame.png`
 *      （direction ∈ {l, r}，**仅交付 l**，运行时镜像）的序列帧；
 *   2. 按「帧所在目录 + 动作 + 状态」分组，帧序号必须 0 起连续（缺号补透明帧并
 *      告警，重号报错）；横向打包为透明底 PNG-32 图集；
 *   3. 输出 `<ACTION-ID>_<state>.png` + `atlas.json`（含
 *      `version/actions[].{actionId,png,frameW,frameH,columns,rows,frameCount,
 *      anchor,hit,secondary}`），**禁止手写 atlas.json**；
 *   4. 单帧物理尺寸必须 256×256（逻辑 128×128 的 2x 导出，`02 §11.1 Q-B`），
 *      不符 → 报错退出非零；
 *   5. `atlas.json` 结构与 `dp-assets/src/atlas.rs` 的 `AtlasFile` **同构冻结**
 *      （两端任一改动必须同步另一端）。
 *
 * S10 增量（分包机制，方案 A）：
 *   - `--pack` 开启后，把逐动作横排图集按「每包 ≤ `--per-pack` 动作（默认 8）」合成
 *     **网格包 PNG** `atlas-pack-<i>.png`（每动作占一行，行主序；行内为该动作的横向
 *     帧序列，不足行宽的尾部补透明列）；`atlas.json` 每条新增可选 `pack` 子对象
 *     `{ png, columns, rows, row }`（`columns`=包网格行宽 stride，`rows`=包总行数，
 *     `row`=动作所在行），**未开启时不产出 `pack` 字段**（与旧结构字节兼容）；
 *     每包 ≤8 动作（256×256 帧 ⇒ 2048/256=8 行），包边长硬上限 2048×2048，
 *     `composePack()` 对超限**直接 throw**（把文档纪律变成代码硬约束）；
 *   - 前端/内核仍按「行主序 `col = index % columns`」切片：包坐标下用
 *     `frameIndex = row × packColumns + localIndex`、`columns = packColumns`、
 *     `rows = packRows` 即得正确 (sx, sy)（`computeFrameRect` 零改动）。
 *     逐动作横排语义（`png=单动作文件`、`columns=动作帧数`、`rows=1`）由 `pack`
 *     缺失时的旧路径保持不变。
 *
 * 参数：
 *   --src <dir>        序列帧根目录（默认 `assets/sprites`）
 *   --out <dir>        图集输出目录（默认：各组帧所在目录下的 `atlas/` 子目录）
 *   --threshold n      alpha 命中阈值（默认 32，`02 §7.7-3`）
 *   --pack             开启分包合成（默认关；关闭时逐动作横排输出旧结构）
 *   --per-pack n       每包动作数上限（默认 8，受 2048×2048 硬上限约束；仅 `--pack` 生效）
 *
 * 约束：
 *   - 零 npm 依赖：PNG 编解码用 `node:zlib` 手写实现（编码每个扫描行滤波器
 *     字节恒 0；解码支持 8bit RGBA/RGB 非交错 + 滤波 0~4）；
 *   - 输入目录不存在 / 无帧 → 打印指引并以 0 退出（空仓库可跑，CI 可执行）；
 *   - 脚本内禁止盘符字面量（C1）；路径一律相对工程根解析。
 *
 * @module scripts/gen-atlas.mjs
 */

import { Buffer } from 'node:buffer';
import process from 'node:process';
import { deflateSync, inflateSync } from 'node:zlib';
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** 退出码：0 = 成功（含空输入），1 = 失败。 */
const EXIT_OK = 0;
const EXIT_FAIL = 1;

/** 单帧物理尺寸（逻辑 128×128 的 2x 导出，`02 §7.7-3`）。 */
const FRAME_SIZE = 256;
/** 默认 alpha 命中阈值。 */
const DEFAULT_THRESHOLD = 32;
/** 默认锚点：底部中心（物理像素）。 */
const ANCHOR = { x: FRAME_SIZE / 2, y: FRAME_SIZE };
/** 单包图集边长硬上限（`02 §7.7` / `01 §...`：帧回退图集单图 ≤2048×2048；256×256 帧 ⇒ ≤8 行）。 */
const ATLAS_MAX_PX = 2048;
/** 默认每包动作数上限（与前端 `atlasPacks.DEFAULT_ACTIONS_PER_PACK` 同口径：29→4、53→7）。 */
const DEFAULT_ACTIONS_PER_PACK = 8;

const scriptDir = dirname(fileURLToPath(import.meta.url));
/** 工程根（repo/）。 */
const repoRoot = resolve(scriptDir, '..');

// ---------------------------------------------------------------------------
// PNG 编解码（零依赖实现；仅覆盖美术导出规范内的 PNG-32/24 8bit 非交错）
// ---------------------------------------------------------------------------

/** CRC32 查表。 */
const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    table[n] = c >>> 0;
  }
  return table;
})();

/** 计算 CRC32。 */
function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) {
    c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  }
  return (c ^ 0xffffffff) >>> 0;
}

/** 打包一个 PNG chunk（长度 + 类型 + 数据 + CRC）。 */
function pngChunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, body, crc]);
}

/** 编码 RGBA8 位图为 PNG（每扫描行滤波器字节恒 0）。 */
function encodePng(width, height, rgba) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // color type: RGBA
  ihdr[10] = 0; // compression
  ihdr[11] = 0; // filter
  ihdr[12] = 0; // interlace

  const stride = width * 4;
  const raw = Buffer.alloc(height * (stride + 1));
  for (let y = 0; y < height; y++) {
    const rowStart = y * (stride + 1);
    raw[rowStart] = 0; // 滤波器 None
    rgba.copy(raw, rowStart + 1, y * stride, (y + 1) * stride);
  }

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    pngChunk('IHDR', ihdr),
    pngChunk('IDAT', deflateSync(raw, { level: 6 })),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

/** Paeth 预测器（PNG 滤波 4）。 */
function paethPredictor(a, b, c) {
  const p = a + b - c;
  const pa = Math.abs(p - a);
  const pb = Math.abs(p - b);
  const pc = Math.abs(p - c);
  if (pa <= pb && pa <= pc) return a;
  if (pb <= pc) return b;
  return c;
}

/** 解码 PNG（8bit RGBA/RGB 非交错，滤波 0~4）。返回 { width, height, rgba }。 */
function decodePng(buf) {
  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  if (buf.length < 8 || !buf.subarray(0, 8).equals(signature)) {
    throw new Error('缺少 PNG 签名');
  }

  let width = 0;
  let height = 0;
  let bitDepth = 0;
  let colorType = -1;
  let interlace = -1;
  const idatParts = [];
  const bytesPerPixelMap = { 2: 3, 6: 4 };

  let offset = 8;
  while (offset + 8 <= buf.length) {
    const dataLen = buf.readUInt32BE(offset);
    const type = buf.subarray(offset + 4, offset + 8).toString('ascii');
    const data = buf.subarray(offset + 8, offset + 8 + dataLen);
    if (type === 'IHDR') {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      bitDepth = data[8];
      colorType = data[9];
      interlace = data[12];
    } else if (type === 'IDAT') {
      idatParts.push(data);
    } else if (type === 'IEND') {
      break;
    }
    offset += 12 + dataLen;
  }

  if (width <= 0 || height <= 0) throw new Error('IHDR 尺寸非法');
  if (bitDepth !== 8) throw new Error(`仅支持 8bit 位深，实际 ${bitDepth}`);
  if (!(colorType in bytesPerPixelMap)) {
    throw new Error(`仅支持 RGB(2)/RGBA(6) 色彩类型，实际 ${colorType}`);
  }
  if (interlace !== 0) throw new Error('不支持 Adam7 交错 PNG');

  const bpp = bytesPerPixelMap[colorType];
  const raw = inflateSync(Buffer.concat(idatParts));
  const stride = width * bpp;
  const expected = height * (stride + 1);
  if (raw.length !== expected) {
    throw new Error(`IDAT 解压长度 ${raw.length} ≠ 期望 ${expected}`);
  }

  // 反滤波（Sub/Up/Average/Paeth）。
  const pixels = Buffer.alloc(height * stride);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const srcRow = y * (stride + 1) + 1;
    const dstRow = y * stride;
    for (let x = 0; x < stride; x++) {
      const rawV = raw[srcRow + x];
      const a = x >= bpp ? pixels[dstRow + x - bpp] : 0;
      const b = y > 0 ? pixels[dstRow - stride + x] : 0;
      const c = x >= bpp && y > 0 ? pixels[dstRow - stride + x - bpp] : 0;
      let value;
      if (filter === 0) value = rawV;
      else if (filter === 1) value = rawV + a;
      else if (filter === 2) value = rawV + b;
      else if (filter === 3) value = rawV + Math.floor((a + b) / 2);
      else if (filter === 4) value = rawV + paethPredictor(a, b, c);
      else throw new Error(`未知滤波器类型 ${filter}（行 ${y}）`);
      pixels[dstRow + x] = value & 0xff;
    }
  }

  if (colorType === 6) {
    return { width, height, rgba: pixels };
  }
  // RGB → RGBA 补全 alpha=255。
  const rgba = Buffer.alloc(width * height * 4);
  for (let i = 0; i < width * height; i++) {
    pixels.copy(rgba, i * 4, i * 3, i * 3 + 3);
    rgba[i * 4 + 3] = 255;
  }
  return { width, height, rgba };
}

// ---------------------------------------------------------------------------
// 参数解析
// ---------------------------------------------------------------------------

/** 解析命令行参数。 */
function parseArgs(argv) {
  const args = {
    src: join(repoRoot, 'assets', 'sprites'),
    out: null,
    threshold: DEFAULT_THRESHOLD,
    pack: false,
    perPack: DEFAULT_ACTIONS_PER_PACK,
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--src') {
      args.src = resolve(argv[++i]);
    } else if (arg === '--out') {
      args.out = resolve(argv[++i]);
    } else if (arg === '--threshold') {
      const value = Number(argv[++i]);
      if (!Number.isInteger(value) || value < 0 || value > 255) {
        throw new Error(`--threshold 必须为 0~255 整数，实际 ${argv[i]}`);
      }
      args.threshold = value;
    } else if (arg === '--pack') {
      args.pack = true;
    } else if (arg === '--per-pack') {
      const value = Number(argv[++i]);
      if (!Number.isInteger(value) || value <= 0) {
        throw new Error(`--per-pack 必须为正整数，实际 ${argv[i]}`);
      }
      args.perPack = value;
    } else {
      throw new Error(`未知参数：${arg}（支持 --src/--out/--threshold/--pack/--per-pack）`);
    }
  }
  return args;
}

// ---------------------------------------------------------------------------
// 扫描与分组
// ---------------------------------------------------------------------------

/**
 * 解析帧文件名 `action_state_direction_frame.png`。
 * 从右取 frame / direction / state 三段，其余合为 action（ACTION-ID 含连字符）。
 */
function parseFrameName(fileName) {
  if (!fileName.toLowerCase().endsWith('.png')) return null;
  const stem = fileName.slice(0, -4);
  const parts = stem.split('_');
  if (parts.length < 4) return null;
  const frame = Number(parts[parts.length - 1]);
  const direction = parts[parts.length - 2];
  const state = parts[parts.length - 3];
  const action = parts.slice(0, parts.length - 3).join('_');
  if (!Number.isInteger(frame) || frame < 0) return null;
  if (direction !== 'l' && direction !== 'r') return null;
  if (!action || !state) return null;
  return { action, state, direction, frame };
}

/** 递归收集目录下的 .png 文件。 */
function walkPngFiles(dir, out, acc) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      walkPngFiles(full, out, acc);
    } else if (entry.isFile() && entry.name.toLowerCase().endsWith('.png')) {
      acc.push(full);
    } else {
      out.push(`跳过非 PNG 文件：${full}`);
    }
  }
}

/** 收集帧：返回 Map「组键 → { dir, action, state, frames: Map<序号, 文件路径> }」。 */
function collectFrames(srcDir, logs) {
  const files = [];
  walkPngFiles(srcDir, logs, files);

  const groups = new Map();
  for (const file of files) {
    const parsed = parseFrameName(file.slice(0, file.length - 4).split(/[\\/]/).pop() + '.png');
    if (!parsed) {
      logs.push(`[warn] 文件名不符合 action_state_direction_frame.png 规范，已忽略：${file}`);
      continue;
    }
    if (parsed.direction === 'r') {
      // 仅交付 l（运行时镜像），r 帧忽略。
      logs.push(`[warn] 检测到 r 向帧（运行时自动镜像，无需交付），已忽略：${file}`);
      continue;
    }
    const dir = dirname(file);
    const key = `${dir}::${parsed.action}::${parsed.state}`;
    if (!groups.has(key)) {
      groups.set(key, { dir, action: parsed.action, state: parsed.state, frames: new Map() });
    }
    const group = groups.get(key);
    if (group.frames.has(parsed.frame)) {
      throw new Error(`帧序号重复：${parsed.action}/${parsed.state}#${parsed.frame}\n  ${group.frames.get(parsed.frame)}\n  ${file}`);
    }
    group.frames.set(parsed.frame, file);
  }
  return groups;
}

/** 校验帧尺寸并合成横向图集 RGBA。返回 { width, height, rgba, frameCount }。 */
function composeAtlas(group, logs) {
  const frameNumbers = [...group.frames.keys()].sort((a, b) => a - b);
  const frameCount = frameNumbers[frameNumbers.length - 1] + 1;
  const blank = Buffer.alloc(FRAME_SIZE * FRAME_SIZE * 4);
  const frameBuffers = new Array(frameCount).fill(null);

  for (const num of frameNumbers) {
    const file = group.frames.get(num);
    const decoded = decodePng(readFileSync(file));
    if (decoded.width !== FRAME_SIZE || decoded.height !== FRAME_SIZE) {
      throw new Error(`帧尺寸 ${decoded.width}×${decoded.height} 不符（要求 ${FRAME_SIZE}×${FRAME_SIZE}）：${file}`);
    }
    frameBuffers[num] = decoded.rgba;
  }
  for (let i = 0; i < frameCount; i++) {
    if (!frameBuffers[i]) {
      logs.push(`[warn] ${group.action}/${group.state} 缺少帧 #${i}，以透明帧补位`);
      frameBuffers[i] = blank;
    }
  }

  const width = FRAME_SIZE * frameCount;
  const height = FRAME_SIZE;
  const atlasRgba = Buffer.alloc(width * height * 4);
  for (let f = 0; f < frameCount; f++) {
    for (let y = 0; y < FRAME_SIZE; y++) {
      frameBuffers[f].copy(
        atlasRgba,
        (y * width + f * FRAME_SIZE) * 4,
        y * FRAME_SIZE * 4,
        (y + 1) * FRAME_SIZE * 4,
      );
    }
  }
  return { width, height, rgba: atlasRgba, frameCount };
}

// ---------------------------------------------------------------------------
// S10 分包合成（方案 A：多动作网格包）
// ---------------------------------------------------------------------------

/**
 * 把已合成的逐动作横排图集均衡切包（纯函数；与前端 `atlasPacks.planAtlasPacks`
 * 同口径：去重保序、每包 ≤ perPack）。
 *
 * @param {Array<{ entry: object, composed: object }>} items 逐动作图集（输入序即包内行序）
 * @param {number} perPack 每包动作数上限
 * @returns {Array<Array<{ entry: object, composed: object }>>} 包数组（每包 ≤ perPack）
 */
function planPacks(items, perPack) {
  const per = Number.isInteger(perPack) && perPack > 0 ? perPack : DEFAULT_ACTIONS_PER_PACK;
  const packs = [];
  for (let i = 0; i < items.length; i += per) {
    packs.push(items.slice(i, i + per));
  }
  return packs;
}

/**
 * 把一个包内的逐动作横排图集合成为**网格包 PNG**（每动作占一行，行主序）。
 *
 * 网格几何：`packColumns = max(各动作帧数)`（行宽 stride，物理列数），
 * `packRows = 包内动作数`；动作 `row` 的帧 `i` 落在包内
 * `(col = i, row = row)` 单元（行内尾部补透明列）。由此：
 *   包内线性帧号 = `row × packColumns + i`，
 *   前端/内核按 `col = 线性号 % packColumns`、`row = 线性号 / packColumns`
 *   即可切出正确子矩形（`computeFrameRect` 零改动）。
 *
 * @param {Array<{ entry: object, composed: object }>} packItems 包内动作（输入序即行序）
 * @returns {{ width: number, height: number, rgba: Buffer, packColumns: number, packRows: number }}
 * @throws {Error} 包边长超过 [`ATLAS_MAX_PX`]（2048×2048 硬上限）时抛出
 */
function composePack(packItems) {
  let packColumns = 1;
  for (const { entry } of packItems) {
    packColumns = Math.max(packColumns, entry.columns);
  }
  const packRows = packItems.length;
  const width = FRAME_SIZE * packColumns;
  const height = FRAME_SIZE * packRows;

  // 几何硬守卫（`02 §7.7` 帧回退图集单图 ≤2048×2048）：把「必须 ≤2048²」从文档
  // 纪律变成代码硬约束，防止以后调 `--per-pack` 或帧尺寸又悄悄超限。
  if (width > ATLAS_MAX_PX || height > ATLAS_MAX_PX) {
    throw new Error(
      `包尺寸 ${width}×${height} 超过硬上限 ${ATLAS_MAX_PX}×${ATLAS_MAX_PX}：` +
        `packColumns=${packColumns}、packRows=${packRows}（帧 ${FRAME_SIZE}）；` +
        `请减小 --per-pack 或帧尺寸`,
    );
  }
  const packRgba = Buffer.alloc(width * height * 4);

  for (let row = 0; row < packItems.length; row++) {
    const { composed } = packItems[row];
    // composed.rgba 为该动作横排图集（宽 = frameCount × FRAME_SIZE，高 = FRAME_SIZE）；
    // 逐扫描行搬入 (row, col 0..frameCount-1) 区段（行内其余列保持透明）。
    const actionWidth = composed.width;
    for (let y = 0; y < FRAME_SIZE; y++) {
      const dstRowStart = ((row * FRAME_SIZE + y) * width) * 4;
      const srcRowStart = (y * actionWidth) * 4;
      composed.rgba.copy(packRgba, dstRowStart, srcRowStart, srcRowStart + actionWidth * 4);
    }
  }
  return { width, height, rgba: packRgba, packColumns, packRows };
}

/**
 * 由包几何生成某动作在 `atlas.json` 中的可选 `pack` 子对象。
 *
 * @param {string} packPng   包 PNG 文件名（如 `atlas-pack-0.png`）
 * @param {number} packColumns 包行宽（物理列数，stride）
 * @param {number} packRows    包总行数
 * @param {number} row         动作所在行（0 起）
 * @returns {{png: string, columns: number, rows: number, row: number}}
 */
function packRef(packPng, packColumns, packRows, row) {
  return { png: packPng, columns: packColumns, rows: packRows, row };
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

function main() {
  const logs = [];
  let args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (err) {
    console.error(`[gen-atlas] 参数错误：${err.message}`);
    return EXIT_FAIL;
  }

  if (!existsSync(args.src)) {
    console.log('[gen-atlas] 未找到序列帧目录，尚未生成图集。');
    console.log(`[gen-atlas] 期望目录：${relative(repoRoot, args.src) || '.'}`);
    console.log('[gen-atlas] 指引：按美术规范交付 action_state_direction_frame.png（direction 仅 l，');
    console.log(`[gen-atlas] 单帧 ${FRAME_SIZE}×${FRAME_SIZE} 透明底 PNG-32）后重新运行本脚本。`);
    return EXIT_OK;
  }

  const groups = collectFrames(args.src, logs);
  for (const log of logs) console.log(log);

  if (groups.size === 0) {
    console.log('[gen-atlas] 输入目录存在但未发现任何规范命名帧，尚未生成图集。');
    console.log('[gen-atlas] 指引：帧文件命名 action_state_direction_frame.png（direction ∈ {l,r}，仅交付 l）。');
    return EXIT_OK;
  }

  // 打包每组；显式 --out 时全部输出到同一目录，否则输出到各帧所在目录的 atlas/。
  const atlasByDir = new Map(); // 输出目录 → entries
  for (const group of groups.values()) {
    try {
      const composed = composeAtlas(group, logs);
      const outDir = args.out ?? join(group.dir, 'atlas');
      const pngName = `${group.action}_${group.state}.png`;
      const entry = {
        actionId: group.action,
        png: pngName,
        frameW: FRAME_SIZE,
        frameH: FRAME_SIZE,
        columns: composed.frameCount,
        rows: 1,
        frameCount: composed.frameCount,
        anchor: { ...ANCHOR },
        hit: { threshold: args.threshold },
        secondary: [],
      };
      if (!atlasByDir.has(outDir)) atlasByDir.set(outDir, []);
      atlasByDir.get(outDir).push({ entry, composed });
    } catch (err) {
      console.error(`[gen-atlas] 打包失败（${group.action}/${group.state}）：${err.message}`);
      return EXIT_FAIL;
    }
  }
  for (const log of logs) console.log(log);

  for (const [outDir, items] of atlasByDir) {
    const actionIds = items.map((it) => it.entry.actionId);
    const dup = actionIds.filter((id, i) => actionIds.indexOf(id) !== i);
    if (dup.length > 0) {
      console.error(`[gen-atlas] 输出目录冲突：动作 ID 重复 ${[...new Set(dup)].join(', ')}（请改用 --out 分目录输出）`);
      return EXIT_FAIL;
    }
    try {
      mkdirSync(outDir, { recursive: true });
      if (args.pack) {
        writePackedAtlases(outDir, items, args.perPack);
      } else {
        for (const { entry, composed } of items) {
          const pngPath = join(outDir, entry.png);
          writeFileSync(pngPath, encodePng(composed.width, composed.height, composed.rgba));
          console.log(`[gen-atlas] 已写出 ${pngPath}（${entry.frameCount} 帧，${composed.width}×${composed.height}）`);
        }
      }
      const atlasJson = {
        version: 1,
        actions: items.map((it) => it.entry),
      };
      writeFileSync(join(outDir, 'atlas.json'), `${JSON.stringify(atlasJson, null, 2)}\n`);
      console.log(`[gen-atlas] 已写出 ${join(outDir, 'atlas.json')}（${items.length} 个动作${args.pack ? `，${items.length > 0 ? planPacks(items, args.perPack).length : 0} 包` : ''}）`);
    } catch (err) {
      console.error(`[gen-atlas] 写出失败：${err.message}`);
      return EXIT_FAIL;
    }
  }

  return EXIT_OK;
}

/**
 * 分包写出（方案 A）：`items` 按 ≤ perPack 均衡切包 → 每包合成网格 PNG
 * `atlas-pack-<i>.png`；并**就地为每个 entry 挂上 `pack` 子对象**（含 png/columns/
 * rows/row），使随后统一的 atlas.json 写出携带包坐标；同时把 entry.png 也改写为
 * 包文件名（内核/前端 `meta.png` 取包，切片用 pack 坐标）。
 *
 * 副作用：入参 items 的 entry 被就地更新（pack、png 字段）。
 *
 * @param {string} outDir 输出目录
 * @param {Array<{ entry: object, composed: object }>} items 逐动作图集
 * @param {number} perPack 每包动作数上限
 */
function writePackedAtlases(outDir, items, perPack) {
  const packs = planPacks(items, perPack);
  packs.forEach((packItems, packIndex) => {
    const packPng = `atlas-pack-${packIndex}.png`;
    const composed = composePack(packItems);
    const pngPath = join(outDir, packPng);
    writeFileSync(pngPath, encodePng(composed.width, composed.height, composed.rgba));
    console.log(
      `[gen-atlas] 已写出 ${pngPath}（${packItems.length} 动作，${composed.width}×${composed.height}，` +
        `${composed.packColumns} 列 × ${composed.packRows} 行）`,
    );
    packItems.forEach(({ entry }, row) => {
      const ref = packRef(packPng, composed.packColumns, composed.packRows, row);
      // 就地更新：pack 子对象 + png 指向包文件（内核/前端按包取图，按 pack 坐标切片）。
      entry.pack = ref;
      entry.png = packPng;
    });
  });
}

process.exit(main());
