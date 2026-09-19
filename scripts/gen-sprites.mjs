#!/usr/bin/env node
/**
 * gen-sprites.mjs —— 程序化生成 Q 版狐形宠物「心月狐」序列帧（零依赖，纯 Node）。
 *
 * 背景：真实美术资产（序列帧 / Spine 骨架）从未交付，`resources/atlas/` 下只有
 * 程序生成的纯色占位块（每张仅 3~4 色），导致真机显示为「色块」。本脚本以
 * **参数化绘制基元**（椭圆/三角/圆角矩形 + alpha 混合光栅器）程序化产出可辨识
 * 的角色序列帧，交付 `gen-atlas.mjs` 变成正式图集。
 *
 * 输入：`resources/config/actions.json`（53 个动作的 fps/looping/loopRange/category/name）
 * 输出：`assets/sprites/xinyuehu/act/<ACTION-ID>_act_l_<n>.png`（256×256 透明底 PNG-32）
 *
 * 用法：
 *   node scripts/gen-sprites.mjs                 # 写出全部帧
 *   node scripts/gen-sprites.mjs --only ACT-M-01 # 只写一个动作（预览调试）
 *
 * 约束：
 *   - C1：脚本内禁止盘符字面量，路径一律相对工程根解析；
 *   - C9：零网络；C4：零依赖（PNG 用 `node:zlib` + 手写 CRC32 编码）。
 *
 * @module scripts/gen-sprites.mjs
 */

import { Buffer } from 'node:buffer';
import process from 'node:process';
import { deflateSync } from 'node:zlib';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/** 单帧物理尺寸（`gen-atlas.mjs` 强校验 256×256）。 */
const FRAME_SIZE = 256;
/** 工程根（repo/）。 */
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
/** 帧输出根目录（符合 `gen-atlas.mjs` 默认 `--src assets/sprites` 约定）。 */
const spriteRoot = join(repoRoot, 'assets', 'sprites', 'xinyuehu', 'act');
/** 动作定义文件。 */
const actionsPath = join(repoRoot, 'resources', 'config', 'actions.json');

// ---------------------------------------------------------------------------
// PNG 编码（零依赖；每扫描行滤波器字节恒 0，与 gen-atlas.mjs 一致）
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

/** 编码 RGBA8 位图为 PNG。 */
function encodePng(width, height, rgba) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // color type: RGBA
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
    pngChunk('IDAT', deflateSync(raw, { level: 9 })),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

// ---------------------------------------------------------------------------
// 光栅器（source-over alpha 混合 + 椭圆/三角/圆角矩形/直线）
// ---------------------------------------------------------------------------

/** RGBA 画布。 */
class Canvas {
  /** @param {number} w @param {number} h */
  constructor(w, h) {
    this.w = w;
    this.h = h;
    this.px = Buffer.alloc(w * h * 4);
  }

  /**
   * source-over alpha 混合单像素。
   * @param {number} x @param {number} y @param {number[]} rgba [r,g,b,a]
   */
  blend(x, y, rgba) {
    if (x < 0 || y < 0 || x >= this.w || y >= this.h) return;
    const a = rgba[3];
    if (a <= 0) return;
    const i = (y * this.w + x) * 4;
    const sa = a / 255;
    const da = this.px[i + 3] / 255;
    const oa = sa + da * (1 - sa);
    if (oa <= 0) return;
    for (let k = 0; k < 3; k++) {
      const dv = this.px[i + k];
      this.px[i + k] = Math.round((rgba[k] * sa + dv * da * (1 - sa)) / oa);
    }
    this.px[i + 3] = Math.round(oa * 255);
  }

  /**
   * 填充椭圆（像素中心采样 + 2×2 超采样抗锯齿）。
   * @param {number} cx @param {number} cy @param {number} rx @param {number} ry
   * @param {number[]|((x:number,y:number)=>number[]|null)} color
   */
  ellipse(cx, cy, rx, ry, color) {
    const srx = Math.max(rx, 0.5);
    const sry = Math.max(ry, 0.5);
    const x0 = Math.floor(cx - srx) - 1;
    const x1 = Math.ceil(cx + srx) + 1;
    const y0 = Math.floor(cy - sry) - 1;
    const y1 = Math.ceil(cy + sry) + 1;
    for (let y = y0; y <= y1; y++) {
      for (let x = x0; x <= x1; x++) {
        let hits = 0;
        for (const oy of [-0.25, 0.25]) {
          for (const ox of [-0.25, 0.25]) {
            const dx = (x + ox - cx) / srx;
            const dy = (y + oy - cy) / sry;
            if (dx * dx + dy * dy <= 1) hits++;
          }
        }
        if (hits === 0) continue;
        const base = typeof color === 'function' ? color(x, y) : color;
        if (base === null || base === undefined) continue;
        this.blend(x, y, [base[0], base[1], base[2], Math.round(base[3] * (hits / 4))]);
      }
    }
  }

  /**
   * 填充任意三角形。
   * @param {number[]} p1 @param {number[]} p2 @param {number[]} p3 @param {number[]} color
   */
  tri(p1, p2, p3, color) {
    const sign = (ax, ay, bx, by, cx, cy) =>
      (ax - cx) * (by - cy) - (bx - cx) * (ay - cy);
    const minX = Math.floor(Math.min(p1[0], p2[0], p3[0]));
    const maxX = Math.ceil(Math.max(p1[0], p2[0], p3[0]));
    const minY = Math.floor(Math.min(p1[1], p2[1], p3[1]));
    const maxY = Math.ceil(Math.max(p1[1], p2[1], p3[1]));
    for (let y = minY; y <= maxY; y++) {
      for (let x = minX; x <= maxX; x++) {
        const d1 = sign(x, y, p1[0], p1[1], p2[0], p2[1]);
        const d2 = sign(x, y, p2[0], p2[1], p3[0], p3[1]);
        const d3 = sign(x, y, p3[0], p3[1], p1[0], p1[1]);
        const hasNeg = d1 < 0 || d2 < 0 || d3 < 0;
        const hasPos = d1 > 0 || d2 > 0 || d3 > 0;
        if (hasNeg && hasPos) continue;
        this.blend(x, y, color);
      }
    }
  }

  /**
   * 填充圆角矩形。
   * @param {number} x @param {number} y @param {number} w @param {number} h
   * @param {number} r @param {number[]} color
   */
  roundRect(x, y, w, h, r, color) {
    const rad = Math.min(r, w / 2, h / 2);
    for (let py = 0; py < h; py++) {
      for (let px = 0; px < w; px++) {
        const dx = Math.max(0, Math.abs(px - w / 2) - (w / 2 - rad));
        const dy = Math.max(0, Math.abs(py - h / 2) - (h / 2 - rad));
        if (Math.hypot(dx, dy) > rad) continue;
        this.blend(x + px, y + py, color);
      }
    }
  }
}

// ---------------------------------------------------------------------------
// 调色板（与 fox_poc.py 概念验证一致：橙毛白腹、粉内耳、大眼高光、腮红）
// ---------------------------------------------------------------------------

/** @type {Record<string, number[]>} */
const C = {
  FUR: [247, 168, 62, 255],
  FUR_D: [216, 124, 32, 255],
  FUR_L: [255, 202, 122, 255],
  BELLY: [255, 246, 228, 255],
  EAR_IN: [255, 158, 168, 255],
  DARK: [58, 40, 30, 255],
  EYE: [40, 30, 24, 255],
  GLINT: [255, 255, 255, 235],
  CHEEK: [255, 132, 122, 150],
  BLUSH_STRONG: [255, 112, 122, 205],
  TEAR: [150, 210, 250, 225],
  SWEAT: [140, 200, 245, 235],
  INK: [70, 60, 70, 255],
  WHITE: [255, 255, 255, 255],
  BUBBLE: [205, 232, 255, 170],
  HEART: [255, 96, 128, 255],
  HEART_D: [226, 58, 96, 255],
  STEAM: [210, 210, 214, 200],
  PINK: [255, 174, 190, 255],
  PINK_D: [230, 120, 150, 255],
  GOLD: [255, 212, 92, 255],
  GOLD_D: [216, 160, 40, 255],
  CLEAN: [190, 230, 255, 150],
  BADGE: [255, 255, 255, 230],
  STAR: [255, 234, 140, 255],
  CONF: [255, 120, 160, 255],
  CONF2: [120, 200, 255, 255],
  CONF3: [255, 220, 90, 255],
  SPARK: [255, 245, 200, 255],
};

// ---------------------------------------------------------------------------
// 角色绘制（参数化基元复用；全部动作共用同一只角色）
// ---------------------------------------------------------------------------

/** 确定性伪随机（保证每次生成逐帧同构，可复现）。 */
function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/**
 * 绘制一只 Q 版狐形宠物「心月狐」。
 *
 * 所有姿态均由参数驱动，供 53 个动作复用同一套绘制基元。
 *
 * @param {Canvas} c 画布
 * @param {object} p 姿态参数
 * @param {number} [p.cx=128] 角色纵向轴线
 * @param {number} [p.cy=150] 身体基准中心
 * @param {number} [p.bob=0] 整体上下浮动（像素）
 * @param {number} [p.squash=0] 纵向挤压：正=压扁（落地），负=拉长（起跳）
 * @param {number} [p.lean=0] 整体横向倾斜（像素/顶部偏移）
 * @param {number} [p.armL=0] @param {number} [p.armR=0] 前爪摆动
 * @param {number} [p.legL=0] @param {number} [p.legR=0] 后腿伸展
 * @param {number} [p.legSpread=1] 后腿间距系数
 * @param {number} [p.earAngle=0] 耳朵外撇角度（± 像素）
 * @param {number} [p.earDroop=0] 耳朵下垂量（像素，正=垂）
 * @param {number} [p.tailSwing=0] 尾巴摆动（-1..1）
 * @param {number} [p.tailLift=0] 尾巴抬高（像素，正=上翘）
 * @param {number} [p.tailTuck=0] 尾巴内收（像素，正=夹尾）
 * @param {number} [p.eye=0] 眼睛张开度 0=闭 1=正常 1.3=瞪大
 * @param {boolean} [p.blink=false] 眨眼
 * @param {string} [p.mouth='smile'] 嘴型：smile|open|flat|frown|o|grin|wavy|eat|munch|tongue
 * @param {number} [p.blush=0] 腮红强度 0..1
 * @param {number} [p.tears=0] 泪水滴数 0..2
 * @param {number} [p.sweat=0] 汗滴数 0..2
 * @param {boolean} [p.aura=false] 怒气球泡
 * @param {boolean} [p.back=false] 背对观众（不画脸）
 * @param {boolean} [p.sleep=false] 闭眼 + zZ
 * @param {boolean} [p.wink=false] 单眼眨
 */
function drawFox(c, p = {}) {
  const cx = p.cx ?? 128;
  const cy = (p.cy ?? 150) + (p.bob ?? 0);
  const squash = p.squash ?? 0;
  const lean = p.lean ?? 0;
  const armL = p.armL ?? 0;
  const armR = p.armR ?? 0;
  const legL = p.legL ?? 0;
  const legR = p.legR ?? 0;
  const legSpread = p.legSpread ?? 1;
  const earAngle = p.earAngle ?? 0;
  const earDroop = p.earDroop ?? 0;
  const tailSwing = p.tailSwing ?? 0;
  const tailLift = p.tailLift ?? 0;
  const tailTuck = p.tailTuck ?? 0;
  // 纵向挤压：身体/头部幅度的统一缩放。
  const sy = 1 + squash;
  const sx = 1 - squash * 0.45;

  // 倾斜：整体绕臀部旋转的小角度近似（按高度线性平移）。
  const tilt = (y) => lean * ((cy + 60 - y) / 160);

  // ---- 尾巴（大而蓬松；从臀部向上后方扫出的三段同心椭圆 + 白尖） ----
  // 尾根贴臀（cx-34），逐段向左上偏移，形成清晰的狐尾上扬弧线。
  const tx = cx - 34 + tailSwing * 14 + tailTuck * 16;
  const ty = cy + 20 - tailLift;
  for (let i = 0; i < 3; i++) {
    const r = [29, 25, 17][i];
    const col = [C.FUR_D, C.FUR, C.BELLY][i];
    c.ellipse(
      tx - i * 6 + tilt(ty - i * 7),
      ty - i * 7,
      r * sx,
      r * 0.96 * sy,
      col,
    );
  }
  // 白色尾尖
  c.ellipse(tx - 20 + tilt(ty - 15), ty - 16, 12 * sx, 14 * sy, C.BELLY);

  // ---- 后腿（腿 + 脚掌同组偏移，保证不脱节） ----
  const lx = cx - 20 * legSpread;
  const rx = cx + 20 * legSpread;
  const legMidY = cy + 42 * sy;
  const footY = cy + 54 * sy;
  c.ellipse(lx + tilt(cy + 42 + legL), legMidY + legL, 15 * sx, 17 * sy, C.FUR_D);
  c.ellipse(rx + tilt(cy + 42 + legR), legMidY + legR, 15 * sx, 17 * sy, C.FUR_D);
  // 脚掌（随腿同步平移，重叠保证连续）
  c.ellipse(lx + tilt(cy + 42 + legL), footY + legL - 3, 14 * sx, 9 * sy, C.FUR);
  c.ellipse(rx + tilt(cy + 42 + legR), footY + legR - 3, 14 * sx, 9 * sy, C.FUR);

  // ---- 身体（含白腹） ----
  c.ellipse(cx + tilt(cy + 12), cy + 12 * sy, 46 * sx, 40 * sy, C.FUR);
  c.ellipse(cx + tilt(cy + 22), cy + 22 * sy, 30 * sx, 26 * sy, C.BELLY);

  // ---- 前爪（摆动 / 抬起） ----
  c.ellipse(
    cx - 24 + armL * 3 + tilt(cy + 50 + armL),
    cy + 50 * sy + armL * 5,
    10 * sx,
    8 * sy,
    C.FUR_D,
  );
  c.ellipse(
    cx + 24 + armR * 3 + tilt(cy + 50 + armR),
    cy + 50 * sy + armR * 5,
    10 * sx,
    8 * sy,
    C.FUR_D,
  );

  // ---- 耳朵（大三角 + 粉内耳；earDroop 控制垂下，earAngle 控制外撇） ----
  // 耳尖明显高于头顶（cy-70），且**不随 squash 缩放**，保证任何姿态都像狐狸。
  const droopL = earDroop;
  const droopR = earDroop;
  // 左耳
  c.tri(
    [cx - 42 + tilt(cy - 40), cy - 40 + droopL * 0.4],
    [cx - 10 - earAngle + tilt(cy - 72 + droopL), cy - 72 + droopL],
    [cx - 8 + tilt(cy - 12), cy - 12 + droopL * 0.15],
    C.FUR,
  );
  // 右耳
  c.tri(
    [cx + 42 + tilt(cy - 40), cy - 40 + droopR * 0.4],
    [cx + 10 + earAngle + tilt(cy - 72 + droopR), cy - 72 + droopR],
    [cx + 8 + tilt(cy - 12), cy - 12 + droopR * 0.15],
    C.FUR,
  );
  // 左耳内耳（粉色小三角）
  c.tri(
    [cx - 34 + tilt(cy - 42), cy - 42 + droopL * 0.4],
    [cx - 16 - earAngle * 0.6 + tilt(cy - 62 + droopL * 0.85), cy - 62 + droopL * 0.85],
    [cx - 15 + tilt(cy - 24), cy - 24 + droopL * 0.15],
    C.EAR_IN,
  );
  // 右耳内耳
  c.tri(
    [cx + 34 + tilt(cy - 42), cy - 42 + droopR * 0.4],
    [cx + 16 + earAngle * 0.6 + tilt(cy - 62 + droopR * 0.85), cy - 62 + droopR * 0.85],
    [cx + 15 + tilt(cy - 24), cy - 24 + droopR * 0.15],
    C.EAR_IN,
  );

  // ---- 头 ----
  const headY = cy - 30;
  c.ellipse(cx + tilt(headY), headY, 42 * sx, 38 * sy, C.FUR);
  // 脸颊白斑
  c.ellipse(cx - 20 + tilt(headY + 14), headY + 14, 17 * sx, 14 * sy, C.BELLY);
  c.ellipse(cx + 20 + tilt(headY + 14), headY + 14, 17 * sx, 14 * sy, C.BELLY);

  if (p.back) {
    // 背对：画后脑勺 + 两耳（上面已画）+ 尾巴（上面已画）。
    c.ellipse(cx + tilt(headY), headY + 6, 30 * sx, 26 * sy, C.FUR_L);
    return;
  }

  // ---- 眼睛 ----
  const eyeOpen = p.sleep ? 0 : (p.eye ?? 1);
  const ey = headY - 6;
  const eyeXs = [cx - 16, cx + 16];
  if (eyeOpen <= 0.05 || p.blink) {
    for (let k = 0; k < eyeXs.length; k++) {
      const ex = eyeXs[k] + tilt(headY);
      if (p.wink && k === 1) continue;
      for (let d = -8; d <= 8; d++) {
        c.blend(ex + d, ey, C.DARK);
        c.blend(ex + d, ey + 1, C.DARK);
      }
    }
  } else {
    const rx = 8.5;
    const ry = 9.5 * eyeOpen;
    for (let k = 0; k < eyeXs.length; k++) {
      const ex = eyeXs[k] + tilt(headY);
      if (p.wink && k === 1) {
        for (let d = -8; d <= 8; d++) {
          c.blend(ex + d, ey, C.DARK);
          c.blend(ex + d, ey + 1, C.DARK);
        }
        continue;
      }
      c.ellipse(ex, ey, rx, ry, C.EYE);
      c.ellipse(ex + 3, ey - 3, 3.2, 3.4 * Math.max(0.4, eyeOpen), C.GLINT);
      c.ellipse(ex - 3, ey + 3, 1.6, 1.6 * Math.max(0.4, eyeOpen), C.GLINT);
    }
  }

  // ---- 鼻 + 嘴 ----
  const my = headY + 14;
  c.ellipse(cx + tilt(my), my, 6, 4.5, C.DARK);
  drawMouth(c, cx + tilt(my + 4), my + 4, p.mouth ?? 'smile', p.tears > 0);

  // ---- 腮红 ----
  const blush = p.blush ?? 0;
  if (blush > 0) {
    const col = blush >= 0.8 ? C.BLUSH_STRONG : C.CHEEK;
    const scaled = [col[0], col[1], col[2], Math.round(col[3] * Math.min(1, blush))];
    c.ellipse(cx - 27 + tilt(my - 4), my - 4, 8, 5, scaled);
    c.ellipse(cx + 27 + tilt(my - 4), my - 4, 8, 5, scaled);
  }

  // ---- 泪水 / 汗滴 ----
  for (let i = 0; i < (p.tears ?? 0); i++) {
    const ex = (i === 0 ? cx - 16 : cx + 16) + tilt(headY);
    c.ellipse(ex, ey + 12 + i * 2, 3.2, 5, C.TEAR);
    c.ellipse(ex, ey + 18 + i * 2, 2.2, 3.4, C.TEAR);
  }
  for (let i = 0; i < (p.sweat ?? 0); i++) {
    const ex = (i === 0 ? cx + 34 : cx - 34) + tilt(headY);
    c.ellipse(ex, headY - 24 - i * 6, 3, 4.6, C.SWEAT);
  }

  // ---- 怒气球泡（暴走） ----
  if (p.aura) {
    for (const [dx, dy, r] of [[-40, headY - 30, 9], [42, headY - 34, 11], [30, headY - 50, 7]]) {
      c.ellipse(cx + dx + tilt(headY), dy, r, r * 0.86, C.STEAM);
    }
  }
}

/**
 * 绘制嘴型。
 * @param {Canvas} c @param {number} mx @param {number} my @param {string} mouth
 * @param {boolean} sad 是否委屈（影响 wavy 幅度）
 */
function drawMouth(c, mx, my, mouth, sad) {
  const dot = (x, y) => c.blend(x, y, C.DARK);
  switch (mouth) {
    case 'flat':
      for (let d = -4; d <= 4; d++) dot(mx + d, my);
      break;
    case 'frown':
      for (let d = -5; d <= 5; d++) dot(mx + d, my + Math.round(Math.abs(d) * 0.4));
      break;
    case 'open':
      c.ellipse(mx, my + 2, 6.5, 7, C.DARK);
      c.ellipse(mx, my + 4, 4, 4, [190, 90, 100, 255]);
      break;
    case 'grin':
      for (let d = -7; d <= 7; d++) dot(mx + d, my + Math.round(Math.abs(d) * 0.25));
      c.ellipse(mx, my + 6, 6, 4, [190, 90, 100, 200]);
      break;
    case 'o':
      c.ellipse(mx, my + 2, 4.2, 4.6, C.DARK);
      break;
    case 'tongue':
      for (let d = -5; d <= 5; d++) dot(mx + d, my + Math.round(Math.abs(d) * 0.3));
      c.ellipse(mx + 3, my + 6, 4, 6, C.PINK);
      break;
    case 'eat':
      c.ellipse(mx, my + 1, 5, 5.5, C.DARK);
      break;
    case 'munch':
      for (let d = -4; d <= 4; d++) dot(mx + d, my);
      c.ellipse(mx, my + 4, 3.4, 3.4, C.DARK);
      break;
    case 'wavy': {
      const amp = sad ? 2.4 : 1.6;
      for (let d = -6; d <= 6; d++) {
        dot(mx + d, my + Math.round(Math.sin(d * 0.9) * amp));
      }
      break;
    }
    case 'smile':
    default:
      for (let d = -6; d <= 6; d++) dot(mx + d, my + Math.round(Math.abs(d) * 0.28));
      break;
  }
}

// ---------------------------------------------------------------------------
// 装饰基元（Z 符号 / 爱心 / 星星 / 金币 / 彩纸 / 气泡表情）
// ---------------------------------------------------------------------------

/** 画一个「Z」字母（睡觉）。 */
function drawZ(c, x, y, size, alpha) {
  const col = [90, 90, 120, alpha];
  const t = Math.max(1, Math.round(size * 0.2));
  for (let d = 0; d < size; d++) {
    c.blend(x + d, y, col);
    c.blend(x - (d - size), y + size - 1, col);
  }
  for (let dy = 0; dy < size; dy++) {
    const d = Math.round((dy / size) * size);
    for (let k = 0; k < t; k++) c.blend(x + size - d + k, y + dy, col);
  }
}

/** 爱心。 */
function drawHeart(c, x, y, r, col) {
  c.ellipse(x - r * 0.5, y - r * 0.35, r * 0.55, r * 0.5, col);
  c.ellipse(x + r * 0.5, y - r * 0.35, r * 0.55, r * 0.5, col);
  c.tri([x - r, y - r * 0.15], [x + r, y - r * 0.15], [x, y + r], col);
}

/** 四角星。 */
function drawStar(c, x, y, r, col) {
  c.tri([x, y - r], [x - r * 0.35, y], [x + r * 0.35, y], col);
  c.tri([x, y + r], [x - r * 0.35, y], [x + r * 0.35, y], col);
  c.tri([x - r, y], [x, y - r * 0.35], [x, y + r * 0.35], col);
  c.tri([x + r, y], [x, y - r * 0.35], [x, y + r * 0.35], col);
}

/** 金币。 */
function drawCoin(c, x, y, r) {
  c.ellipse(x, y, r, r, C.GOLD_D);
  c.ellipse(x, y, r * 0.78, r * 0.78, C.GOLD);
  c.ellipse(x - r * 0.2, y - r * 0.2, r * 0.28, r * 0.28, [255, 245, 200, 220]);
}

/** 彩纸屑（确定性随机）。 */
function drawConfetti(c, seed, count, t) {
  const rnd = mulberry32(seed);
  const cols = [C.CONF, C.CONF2, C.CONF3, C.GOLD];
  for (let i = 0; i < count; i++) {
    const bx = rnd() * 256;
    const by = rnd() * 200;
    const vx = (rnd() - 0.5) * 18;
    const vy = 30 + rnd() * 40;
    const col = cols[i % cols.length];
    const x = bx + vx * t;
    const y = ((by + vy * t) % 260) - 6;
    const w = 4 + Math.round(rnd() * 3);
    const h = 3 + Math.round(rnd() * 3);
    c.roundRect(Math.round(x), Math.round(y), w, h, 1, col);
  }
}

/** 肥皂泡沫（洗澡）。 */
function drawBubbles(c, seed, count, t) {
  const rnd = mulberry32(seed);
  for (let i = 0; i < count; i++) {
    const x = rnd() * 220;
    const y = rnd() * 180 + 30;
    const r = 6 + rnd() * 10;
    const yy = y - ((t * 26 + i * 17) % 200);
    c.ellipse(x, yy, r, r, C.BUBBLE);
    c.ellipse(x - r * 0.3, yy - r * 0.3, r * 0.28, r * 0.28, [255, 255, 255, 200]);
  }
}

/** 思考/无语气泡（含三点）。 */
function drawThinkBubble(c, x, y, r) {
  c.ellipse(x, y, r, r * 0.78, [255, 255, 255, 225]);
  c.ellipse(x + r * 0.5, y + r * 1.1, r * 0.28, r * 0.22, [255, 255, 255, 225]);
  c.ellipse(x + r * 0.75, y + r * 1.5, r * 0.16, r * 0.13, [255, 255, 255, 225]);
  for (let i = -1; i <= 1; i++) {
    c.ellipse(x + i * r * 0.4, y, r * 0.11, r * 0.11, C.INK);
  }
}

/** 音符。 */
function drawNote(c, x, y, r, col) {
  c.ellipse(x - r * 0.4, y + r * 0.7, r * 0.34, r * 0.26, col);
  for (let d = 0; d < r * 1.4; d++) c.blend(Math.round(x + r * 0.05), Math.round(y + r * 0.7 - d), col);
  c.ellipse(x + r * 0.6, y + r * 0.3, r * 0.34, r * 0.26, col);
}

// ---------------------------------------------------------------------------
// 动作语义表（从 actions.json 读到的 id → 姿态生成器）
// ---------------------------------------------------------------------------

/**
 * 通用姿态生成器构造器。
 * @param {(f:number, n:number, rand:()=>number)=>{pose:object, deco?:Function}} build
 */
function frames(n, build) {
  return (i) => build(i, n, mulberry32(0x51a7 + i * 131));
}

/** 相位：0..1 循环。 */
const phase = (i, n) => (n <= 1 ? 0 : (i % n) / n);

/** 动作 → 帧数 + 逐帧姿态生成。 */
const ACTIONS = {
  // ---- move ----
  'ACT-M-01': { frames: 4, gen: frames(4, (i, n) => {
    const t = phase(i, n);
    return { pose: { bob: Math.sin(t * Math.PI * 2) * 3, earAngle: Math.sin(t * Math.PI * 2) * 2,
      tailSwing: Math.sin(t * Math.PI * 2) * 0.5, blink: i === 3, eye: i === 3 ? 0 : 1 } };
  }) },
  'ACT-M-02': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: Math.abs(s) * 4 - 2, legL: s * 8, legR: -s * 8, armL: -s * 5, armR: s * 5,
      tailSwing: s * 0.7, earAngle: s * 3, legSpread: 0.9 } };
  }) },
  'ACT-M-03': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: Math.abs(s) * 6 - 3, legL: s * 14, legR: -s * 14, armL: -s * 10, armR: s * 10,
      tailSwing: s * 0.9 + 0.3, tailLift: 6, lean: 6, earAngle: s * 5, legSpread: 0.85, eye: 1.05 } };
  }) },
  'ACT-M-04': { frames: 8, gen: frames(8, (i, n) => {
    // 非循环：蓄力 → 起跳 → 滞空 → 下落。
    const k = [0, 1, 2, 3, 4, 5, 6, 7][i];
    const map = [
      { squash: 0.24, bob: 10, armL: 6, armR: 6, eye: 0.6, earDroop: 6, mouth: 'flat' },
      { squash: 0.12, bob: 4, armL: 2, armR: 2, eye: 0.9, earDroop: 3 },
      { squash: -0.26, bob: -18, armL: -10, armR: -10, eye: 1.2, earAngle: 8, mouth: 'open', tailLift: 14 },
      { squash: -0.34, bob: -30, armL: -14, armR: -14, eye: 1.2, earAngle: 10, mouth: 'open', tailLift: 20 },
      { squash: -0.3, bob: -34, armL: -13, armR: -13, eye: 1.1, earAngle: 9, mouth: 'open', tailLift: 22, legL: -6, legR: 6 },
      { squash: -0.14, bob: -20, armL: -6, armR: -6, eye: 1.0, earAngle: 6, legL: 8, legR: -8 },
      { squash: 0.1, bob: -4, armL: 4, armR: 4, eye: 0.9, legL: 10, legR: -10 },
      { squash: 0.3, bob: 12, armL: 8, armR: 8, eye: 0.5, mouth: 'flat', legSpread: 1.15, earDroop: 5 },
    ];
    void k;
    return { pose: map[i] };
  }) },
  'ACT-M-05': { frames: 6, gen: frames(6, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: -10 + s * 3, legL: -6 + s * 3, legR: -6 - s * 3, armL: -8, armR: -8,
      earAngle: 8, eye: 1.15, mouth: 'open', tailLift: 16, lean: s * 4 } };
  }) },
  'ACT-M-06': { frames: 6, gen: frames(6, (i) => {
    const seq = [
      { squash: 0.34, bob: 14, legSpread: 1.25, armL: 8, armR: 8, eye: 0.5, mouth: 'flat', earDroop: 6 },
      { squash: 0.22, bob: 9, legSpread: 1.15, armL: 5, armR: 5, eye: 0.7, earDroop: 4 },
      { squash: 0.06, bob: 2, legSpread: 1.05, armL: 2, armR: 2, eye: 0.9, earDroop: 2 },
      { squash: -0.06, bob: -4, legSpread: 1.0, eye: 1.05 },
      { squash: 0.03, bob: 1, legSpread: 1.0, eye: 1.0 },
      { squash: 0, bob: 0, legSpread: 1.0, eye: 1.0, mouth: 'smile' },
    ];
    return { pose: seq[i] };
  }) },
  'ACT-M-07': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: Math.abs(s) * 3 - 1.5, legL: s * 6, legR: -s * 6, armL: -s * 4, armR: s * 4,
      tailSwing: s * 0.5, lean: -4, earAngle: s * 2, legSpread: 0.92 } };
  }) },

  // ---- idle ----
  'ACT-I-01': { frames: 8, gen: frames(8, (i) => {
    const look = [0, 1, 2, 3, 2, 1, 0, -1][i];
    return { pose: { bob: (i % 2) * 1.5, lean: look * 3, earAngle: look * 2,
      tailSwing: look * 0.3, eye: 1, blink: i === 4 } };
  }) },
  'ACT-I-02': { frames: 8, gen: frames(8, (i) => {
    const open = [0.2, 0.4, 0.7, 1.0, 1.15, 1.0, 0.6, 0.3][i];
    return { pose: { bob: [0, -1, -2, -3, -3, -2, -1, 0][i], eye: open, mouth: i >= 3 && i <= 5 ? 'open' : 'flat',
      earDroop: [0, 1, 2, 3, 3, 2, 1, 0][i] } };
  }) },
  'ACT-I-03': { frames: 6, gen: frames(6, (i, n) => {
    const t = phase(i, n);
    return { pose: { bob: Math.sin(t * Math.PI * 2) * 2, eye: 0.55, mouth: 'flat',
      armL: 10, armR: 10, legSpread: 0.85, tailTuck: 6, earDroop: 4 },
      deco: (c) => { if (i === 2) drawThinkBubble(c, 196, 52, 26); } };
  }) },
  'ACT-I-04': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    const c2 = Math.cos(t * Math.PI * 2);
    return { pose: { bob: Math.abs(s) * 3 - 1.5, lean: s * 5, legL: s * 10, legR: -s * 10,
      tailSwing: 0.7 + c2 * 0.25, tailLift: 10, eye: 1.1, mouth: 'grin', earAngle: -s * 4 } };
  }) },
  'ACT-I-05': { frames: 4, gen: frames(4, (i, n) => {
    const t = phase(i, n);
    return { pose: { bob: Math.sin(t * Math.PI * 2) * 2.5, eye: 0, sleep: true, mouth: 'flat',
      tailTuck: 10, earDroop: 8, squash: 0.12 },
      deco: (c) => { drawZ(c, 178, 44 - i * 6, 16 + i * 2, 200 - i * 20); drawZ(c, 200, 26, 11, 150); } };
  }) },
  'ACT-I-06': { frames: 8, gen: frames(8, (i) => {
    const stretch = [0, 0, 1, 2, 3, 3, 1, 0][i];
    return { pose: { squash: -0.12 * stretch, bob: -3 * stretch, armL: -12 * stretch, armR: -12 * stretch,
      eye: i >= 2 && i <= 5 ? 0 : 1, mouth: i >= 2 && i <= 5 ? 'open' : 'smile',
      tailLift: 12 * stretch, earAngle: 4 * stretch, legSpread: 1 + 0.12 * stretch } };
  }) },

  // ---- interact ----
  'ACT-T-01': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { armR: -8 - s * 6, armL: 2, eye: 1.05, mouth: 'grin', earAngle: s * 4,
      tailSwing: s * 0.8, bob: Math.abs(s) * 2 },
      deco: (c) => { if (i < 4) drawSparkles(c, i); } };
  }) },
  'ACT-T-02': { frames: 6, gen: frames(6, (i) => {
    const up = [0, 1, 2, 2, 1, 0][i];
    return { pose: { armL: -10 * up, armR: -10 * up, eye: 1.0, mouth: 'grin', blush: 0.5 + up * 0.3,
      earAngle: up * 3, tailSwing: up * 0.5 },
      deco: (c) => { if (up > 0) { drawHeart(c, 104, 58, 12, C.HEART); drawHeart(c, 154, 46, 9, C.HEART_D); } } };
  }) },
  'ACT-T-03': { frames: 6, gen: frames(6, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: s * 2, blush: 0.95, eye: 0.7, mouth: 'wavy', earDroop: 5, lean: s * 2,
      armL: 6, armR: 6 },
      deco: (c) => { drawHeart(c, 176, 62, 8, [255, 150, 170, 180]); } };
  }) },
  'ACT-T-04': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { armR: -12, armL: 2, bob: Math.abs(s) * 2, eye: 0.85, mouth: 'grin',
      blush: 0.4, earAngle: s * 3, lean: s * 2 } };
  }) },
  'ACT-T-05': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 4);
    return { pose: { bob: Math.abs(s) * 4 - 2, lean: s * 6, armL: -s * 8, armR: s * 8,
      legL: s * 6, legR: -s * 6, eye: 0, mouth: 'open', earAngle: s * 6, tailSwing: s * 1.0 } };
  }) },
  'ACT-T-06': { frames: 8, gen: frames(8, (i, n) => {
    // 抛物线翻滚：整体压缩 + 倾斜翻转。
    const t = phase(i, n);
    const h = Math.sin(Math.PI * Math.min(1, (i + 0.5) / n));
    return { pose: { bob: -30 * h, squash: -0.1 + 0.3 * (1 - h), armL: -10 * h, armR: -10 * h,
      eye: 0, mouth: 'o', tailLift: 20 * h, earAngle: 8, lean: Math.cos(t * Math.PI * 2) * 8 } };
  }) },
  'ACT-T-07': { frames: 8, gen: frames(8, (i) => {
    const roll = [0, 1, 2, 3, 3, 2, 1, 0][i];
    return { pose: { squash: 0.2 + roll * 0.05, bob: 8 + roll * 2, eye: 0, mouth: 'o',
      armL: -8, armR: -8, legSpread: 1 + roll * 0.1, tailTuck: 4, lean: roll * 3 },
      deco: (c) => { if (roll >= 2) { drawStar(c, 186, 66, 10, C.STAR); drawStar(c, 206, 44, 6, C.STAR); } } };
  }) },
  'ACT-T-08': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: -Math.abs(s) * 6, legL: s * 10, legR: -s * 10, armL: -s * 4, armR: s * 4,
      eye: 0.9, mouth: 'frown', earAngle: -s * 4, tailSwing: s * 0.4 },
      deco: (c) => { if (Math.abs(s) > 0.6) drawPuff(c, 120, 236, 20); } };
  }) },
  'ACT-T-09': { frames: 6, gen: frames(6, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { armL: -8, armR: -8 + s * 10, eye: 0.7, mouth: 'flat', lean: -s * 3,
      earAngle: -3, headShake: s } };
  }) },
  'ACT-T-10': { frames: 6, gen: frames(6, (i) => {
    const near = [0, 1, 2, 3, 3, 2][i];
    return { pose: { squash: -0.2 * near * 0.3, bob: -2 * near, eye: 1.3, mouth: 'o',
      lean: 3, earAngle: 6, legSpread: 0.85, tailLift: 6 } };
  }) },

  // ---- emotion ----
  'ACT-E-01': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    const cc = Math.cos(t * Math.PI * 2);
    return { pose: { bob: -Math.abs(s) * 6, lean: cc * 6, armL: -10 - s * 4, armR: -10 + s * 4,
      eye: 1.1, mouth: 'grin', blush: 0.6, tailSwing: s * 1.0, tailLift: 8, earAngle: s * 5 },
      deco: (c) => { drawHeart(c, 70 - cc * 10, 60 + s * 14, 8, [255, 120, 150, 200]);
        drawHeart(c, 190 - cc * 8, 52 - s * 12, 7, [255, 150, 175, 190]); } };
  }) },
  'ACT-E-02': { frames: 6, gen: frames(6, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: s * 2, eye: 0.75, mouth: 'wavy', tears: 1 + (i % 2), earDroop: 8,
      tailTuck: 8, blush: 0.3, lean: s * 2 },
      deco: (c) => { drawThinkBubble(c, 202, 60, 22); } };
  }) },
  'ACT-E-03': { frames: 4, gen: frames(4, (i, n) => {
    const t = phase(i, n);
    return { pose: { back: true, bob: Math.sin(t * Math.PI * 2) * 2, tailSwing: 0.4 + Math.sin(t * Math.PI * 4) * 0.3,
      earDroop: 6, squash: 0.08 },
      deco: (c) => { if (i % 2 === 1) drawPuff(c, 168, 58, 22); } };
  }) },
  'ACT-E-04': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 4);
    return { pose: { bob: Math.abs(s) * 4 - 2, lean: s * 5, armL: -s * 6, armR: s * 6,
      eye: 1.2, mouth: 'open', earAngle: -s * 5, aura: true, tailSwing: s * 0.5, blush: 0.9 },
      deco: (c) => { drawStar(c, 190 + s * 6, 44, 9, [255, 150, 130, 220]); } };
  }) },
  'ACT-E-05': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: Math.abs(s) * 3, legL: s * 5, legR: -s * 5, eye: 0.8, mouth: 'flat',
      earDroop: 7, tailTuck: 10, lean: -4 },
      deco: (c) => { drawBundle(c, 52 - i, 150); } };
  }) },
  'ACT-E-06': { frames: 8, gen: frames(8, (i) => {
    const cry = [1, 1, 0, 0, 0, 0, 0, 0][i];
    const happy = [0, 0, 0.3, 0.6, 0.9, 1, 0.8, 0.5][i];
    return { pose: { bob: -happy * 3, eye: cry ? 0.5 : 0.9 + happy * 0.2,
      mouth: cry ? 'wavy' : 'grin', tears: cry ? 1 : 0, blush: 0.3 + happy * 0.4,
      earAngle: happy * 4, tailSwing: happy * 0.8, armL: -happy * 6, armR: -happy * 6 },
      deco: (c) => { if (happy > 0.6) drawHeart(c, 178, 54, 9, C.HEART); } };
  }) },

  // ---- need ----
  'ACT-N-01': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 4);
    return { pose: { bob: Math.abs(s) * 3, eye: 1.15, mouth: 'open', armL: -12, armR: -12,
      earDroop: 4, tailSwing: s * 0.6, blush: 0.4 },
      deco: (c) => { if (i % 2 === 0) drawThinkBubble(c, 200, 54, 20); } };
  }) },
  'ACT-N-02': { frames: 8, gen: frames(8, (i) => {
    const chew = i % 2 === 0;
    return { pose: { bob: chew ? -2 : 0, eye: 0.9, mouth: chew ? 'eat' : 'munch',
      armL: -4, armR: -4, earAngle: chew ? 3 : 1 },
      deco: (c) => { drawBowl(c, 128, 224); } };
  }) },
  'ACT-N-03': { frames: 6, gen: frames(6, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: s * 3, eye: 0, mouth: 'smile', blush: 0.7, armL: 8, armR: 8,
      legSpread: 1.1, tailSwing: s * 0.3 },
      deco: (c) => { drawSparkles(c, i); } };
  }) },
  'ACT-N-04': { frames: 6, gen: frames(6, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: s * 2, eye: 0.8, mouth: 'frown', earDroop: 6,
      dirt: true, tailSwing: s * 0.3 },
      deco: (c) => { drawFlies(c, i); } };
  }) },
  'ACT-N-05': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 4);
    return { pose: { bob: Math.abs(s) * 3 - 1, armL: -s * 8, armR: s * 8, eye: 0.7,
      mouth: 'flat', earAngle: s * 3, squash: 0.06 } };
  }) },
  'ACT-N-06': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: Math.abs(s) * 3, armL: -10, armR: -10, eye: 1.1, mouth: 'open',
      earDroop: 5, tailSwing: Math.sin(t * Math.PI * 4) * 0.4 },
      deco: (c) => { if (i % 2 === 0) c.ellipse(196, 66, 7, 7, C.BUBBLE); } };
  }) },
  'ACT-N-07': { frames: 8, gen: frames(8, (i) => {
    return { pose: { bob: 0, eye: 0, mouth: 'smile', tailSwing: 0.3, earDroop: 3 },
      deco: (c) => { drawBubbles(c, 0x88, 9, i / 8); } };
  }) },
  'ACT-N-08': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: s * 2, eye: 1.1, mouth: 'grin', tailSwing: s * 0.6, tailLift: 8,
      blush: 0.35, armL: -4, armR: -4 },
      deco: (c) => { drawSparkles(c, i); } };
  }) },

  // ---- special ----
  'ACT-S-01': { frames: 6, gen: frames(6, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { earAngle: s * 10, earDroop: -Math.abs(s) * 4, eye: 0.9,
      mouth: 'flat', tailSwing: s * 0.4, bob: Math.abs(s) * 2 } };
  }) },
  'ACT-S-02': { frames: 8, gen: frames(8, (i) => {
    // 原地消失闪现：淡出→淡入（用整体缩放近似）。
    const sc = [1, 0.75, 0.5, 0.25, 0.2, 0.5, 0.8, 1][i];
    return { pose: { squash: 0.5 * (1 - sc) * 0.6, bob: 0, eye: sc < 0.5 ? 0 : 1,
      mouth: sc < 0.5 ? 'flat' : 'grin', earAngle: (1 - sc) * 8, tailSwing: 0.3 },
      deco: (c) => { if (sc > 0.4 && sc < 1) drawSparkles(c, i); } };
  }) },
  'ACT-S-03': { frames: 6, gen: frames(6, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { eye: i === 2 ? 0 : 1.1, mouth: i === 2 ? 'flat' : 'smile',
      lean: 4, eyeShift: s * 4, earAngle: s * 3, bob: Math.abs(s) * 1.5 } };
  }) },
  'ACT-S-04': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { armL: -12, armR: -12, bob: -Math.abs(s) * 5, eye: 1.1, mouth: 'grin',
      blush: 0.5, tailSwing: s * 0.9, earAngle: s * 4 },
      deco: (c) => { drawConfetti(c, 0xc0de, 22, i / n); } };
  }) },

  // ---- perception ----
  'ACT-P-01': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 4);
    return { pose: { armL: -s * 12, armR: -s * 12, eye: 1.25, mouth: 'o', bob: Math.abs(s) * 3,
      squash: 0.05, earAngle: s * 5, tailSwing: s * 0.7 },
      deco: (c) => { c.ellipse(128, 228, 16, 7, [120, 180, 240, 150]); } };
  }) },
  'ACT-P-02': { frames: 4, gen: frames(4, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 6);
    return { pose: { bob: s * 2, lean: s * 2, eye: 1.2, mouth: 'wavy', earAngle: -s * 3,
      sweat: 1, armL: 4, armR: 4, tailSwing: s * 0.3 } };
  }) },
  'ACT-P-03': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { armR: -14, armL: 2, eye: 0.95, mouth: 'flat', earAngle: s * 3,
      bob: Math.abs(s) * 1.5, legSpread: 0.95 } };
  }) },
  'ACT-P-04': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { lean: 5 + s * 4, squash: -0.12, bob: -3 + s * 2, eye: 1.25,
      mouth: 'o', earAngle: 6, armL: -6, armR: -6, tailLift: 10 } };
  }) },

  // ---- activity ----
  'ACT-N-09': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { eye: 1.0, mouth: 'smile', armL: -4, armR: -4, earAngle: s * 3,
      bob: Math.abs(s) * 2, tailSwing: s * 0.4 } };
  }) },
  'ACT-N-10': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: Math.abs(s) * 3, legL: s * 5, legR: -s * 5, eye: 1.05,
      mouth: 'grin', armL: -s * 5, armR: s * 5, tailSwing: s * 0.8 },
      deco: (c) => { drawCoin(c, 190, 70 - i * 3, 9); } };
  }) },
  'ACT-N-11': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { eye: 0.95, mouth: i === 6 ? 'o' : 'flat', armL: 6, armR: 6,
      bob: Math.abs(s) * 1.5, earAngle: s * 2, blush: 0.2 },
      deco: (c) => { drawBook(c, 128, 226); } };
  }) },
  'ACT-N-12': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: Math.abs(s) * 4, eye: 1.1, mouth: 'grin', armL: -s * 6,
      armR: s * 6, legL: s * 6, legR: -s * 6, tailSwing: s * 0.7, squash: -0.05 },
      deco: (c) => { drawBundle(c, 54, 152); } };
  }) },
  'ACT-N-13': { frames: 8, gen: frames(8, (i) => {
    const up = [0, 1, 2, 3, 3, 2, 1, 0][i];
    return { pose: { armR: -8 - up * 4, armL: 2, eye: 1.0, mouth: 'smile', bob: up * 2,
      earAngle: up * 3, tailSwing: up * 0.4 },
      deco: (c) => { c.roundRect(168, 48 - up * 4, 34, 24, 3, [255, 252, 240, 245]); } };
  }) },
  'ACT-N-14': { frames: 8, gen: frames(8, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: Math.abs(s) * 3, legL: s * 6, legR: -s * 6, eye: 1.0,
      mouth: 'grin', armL: -s * 5, armR: s * 5, tailSwing: s * 0.8 } };
  }) },
  'ACT-N-15': { frames: 8, gen: frames(8, (i) => {
    const n = [1, 2, 3, 4, 5, 6, 7, 8][i];
    return { pose: { eye: 0.9, mouth: 'smile', armL: -6, armR: -6, bob: Math.abs(Math.sin(i / 2)) * 2,
      earAngle: ((i % 3) - 1) * 3, tailSwing: 0.3 },
      deco: (c) => { drawCoinStack(c, 128, 228, n); } };
  }) },
  'ACT-N-16': { frames: 6, gen: frames(6, (i, n) => {
    const t = phase(i, n);
    const s = Math.sin(t * Math.PI * 2);
    return { pose: { bob: Math.abs(s) * 4, eye: 0.5, mouth: 'open', squash: 0.05,
      armL: -6, armR: -6, earDroop: 6, sweat: 1, tailSwing: s * 0.3 } };
  }) },
};

/** 星芒点缀。 */
function drawSparkles(c, i) {
  const pts = [[70, 62], [186, 52], [58, 150], [200, 130]];
  const [x, y] = pts[i % pts.length];
  drawStar(c, x, y, 7 + (i % 3), C.SPARK);
}

/** 跺脚灰尘。 */
function drawPuff(c, x, y, r) {
  c.ellipse(x - r * 0.5, y, r * 0.5, r * 0.35, [210, 200, 190, 170]);
  c.ellipse(x + r * 0.5, y - 2, r * 0.42, r * 0.3, [210, 200, 190, 150]);
}

/** 小包袱（离家/旅游）。 */
function drawBundle(c, x, y) {
  c.roundRect(x - 14, y, 28, 24, 5, [180, 140, 100, 255]);
  for (let d = 0; d < 16; d++) c.blend(x - 6 + d, y - 3, [150, 110, 78, 255]);
}

/** 饭碗。 */
function drawBowl(c, x, y) {
  c.ellipse(x, y, 30, 10, [235, 235, 240, 255]);
  c.ellipse(x, y + 2, 24, 8, [225, 225, 232, 255]);
  c.ellipse(x, y - 2, 22, 6, C.FUR_L);
}

/** 书。 */
function drawBook(c, x, y) {
  c.roundRect(x - 26, y - 12, 52, 22, 3, [90, 140, 200, 255]);
  c.roundRect(x - 26, y - 12, 26, 22, 3, [255, 250, 235, 255]);
  for (let i = 0; i < 4; i++) {
    for (let d = -8; d < 8; d++) c.blend(x - 22 + d, y - 6 + i * 4, [170, 170, 180, 200]);
  }
}

/** 金币堆。 */
function drawCoinStack(c, x, y, n) {
  const cols = Math.ceil(n / 3);
  let k = 0;
  for (let col = 0; col < cols && k < n; col++) {
    const rows = Math.min(3, n - col * 3);
    for (let r = 0; r < rows; r++) {
      drawCoin(c, x + (col - (cols - 1) / 2) * 22, y - r * 9, 10);
      k++;
    }
  }
}

/** 苍蝇（脏污）。 */
function drawFlies(c, i) {
  const a = (i / 6) * Math.PI * 2;
  for (const [bx, by] of [[62, 60], [196, 74]]) {
    const x = bx + Math.cos(a) * 8;
    const y = by + Math.sin(a * 1.3) * 6;
    c.ellipse(x, y, 3, 2.4, [60, 60, 70, 235]);
    c.ellipse(x - 3, y - 2, 3, 2, [140, 140, 150, 150]);
    c.ellipse(x + 3, y - 2, 3, 2, [140, 140, 150, 150]);
  }
}

/** 落笔：单帧生成入口。 */
function renderFrame(spec, i) {
  const c = new Canvas(FRAME_SIZE, FRAME_SIZE);
  const { pose, deco } = spec.gen(i);
  applyExtras(c, pose);
  drawFox(c, pose);
  if (deco) deco(c);
  return c.px;
}

/** 处理超出 drawFox 基础参数的附加效果（脏污等）。 */
function applyExtras(c, pose) {
  if (pose.dirt) {
    const rnd = mulberry32(0xd17);
    for (let k = 0; k < 16; k++) {
      const x = 128 + (rnd() - 0.5) * 90;
      const y = 150 + (rnd() - 0.5) * 70;
      c.ellipse(x, y, 3 + rnd() * 3, 2 + rnd() * 2, [120, 100, 80, 180]);
    }
  }
  if (pose.eyeShift) {
    void pose.eyeShift; // 眼球位移在 drawFox 内以 lean 近似，无需额外处理。
  }
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const args = { only: null, clean: true };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--only') args.only = argv[++i];
    else if (argv[i] === '--no-clean') args.clean = false;
  }
  return args;
}

function main() {
  let actionsCfg;
  try {
    actionsCfg = JSON.parse(readFileSync(actionsPath, 'utf8'));
  } catch (err) {
    console.error(`[gen-sprites] 无法读取 actions.json：${err.message}`);
    return 1;
  }
  const actions = Array.isArray(actionsCfg.actions) ? actionsCfg.actions : [];
  if (actions.length === 0) {
    console.error('[gen-sprites] actions.json 无动作定义');
    return 1;
  }

  const args = parseArgs(process.argv.slice(2));
  if (args.clean && !args.only) {
    rmSync(spriteRoot, { recursive: true, force: true });
  }
  mkdirSync(spriteRoot, { recursive: true });

  const missing = [];
  let totalFrames = 0;
  for (const action of actions) {
    const id = action.id;
    if (args.only && id !== args.only) continue;
    const table = ACTIONS[id];
    if (!table) {
      missing.push(id);
      continue;
    }
    const n = table.frames;
    for (let i = 0; i < n; i++) {
      const rgba = renderFrame(table, i);
      const name = `${id}_act_l_${i}.png`;
      writeFileSync(join(spriteRoot, name), encodePng(FRAME_SIZE, FRAME_SIZE, rgba));
      totalFrames++;
    }
    console.log(`[gen-sprites] ${id}（${action.name}）：${n} 帧`);
  }

  if (missing.length > 0) {
    console.error(`[gen-sprites] 缺少姿态定义的动作：${missing.join(', ')}`);
    return 1;
  }
  console.log(`[gen-sprites] 共写出 ${totalFrames} 帧 → ${spriteRoot}`);
  return 0;
}

process.exit(main());
