// gen-skeleton.mjs — S9-M2 骨骼覆盖率表生成器（纯 Node 零依赖）
//
// 由 SG-M2 门禁样例泛化而来：对 skeleton.json **每个 clip** 均匀取 8 个相位
// t = k/8 * duration，把各骨骼上 region attachment 投影到 32x32 覆盖率网格，
// 输出每 clip 一张覆盖率表（53 clip × 4KB ≈ 212KB，K-14 / `02 §4.4`）。
//
// 用法：
//   node scripts/gen-skeleton.mjs [--input <skeleton.json>] [--out <coverage.json>]
//
// 输出 coverage.json：
//   { gridSize, worldRange, clips: { "<clip>": { duration, phases:[{t,angle,cells,cellCount,occupancyRatio}] } } }
//
// 注意：最小正运动学仅覆盖 rotate/translate 继承链 + 线性插值（门禁边界）；
// 完整曲线/IK/物理以 Spine 运行时实测为准（见 SG-M2 README「已知限制」）。

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..");

function parseArgs(argv) {
  const args = { input: null, out: null };
  for (let i = 2; i < argv.length; i++) {
    if (argv[i] === "--input") args.input = argv[++i];
    else if (argv[i] === "--out") args.out = argv[++i];
  }
  return args;
}

const args = parseArgs(process.argv);
const skelPath = args.input ?? path.join(REPO, "resources", "skeleton", "skeleton.json");
const outPath = args.out ?? path.join(REPO, "resources", "skeleton", "coverage.json");

if (!fs.existsSync(skelPath)) {
  console.error(`未找到骨骼资产：${skelPath}（正式美术资产绑定为 SG-M2 人工步骤）`);
  process.exit(2);
}

const skel = JSON.parse(fs.readFileSync(skelPath, "utf8"));

// ---- Spine 4.2 结构 sanity check ---------------------------------------------
function sanityCheck(s) {
  const errors = [];
  if (!s.skeleton || !s.skeleton.spine) errors.push("missing skeleton.spine");
  else if (!String(s.skeleton.spine).startsWith("4.2")) errors.push(`spine must be 4.2.x, got ${s.skeleton.spine}`);
  if (!Array.isArray(s.bones) || s.bones.length === 0) errors.push("missing bones[]");
  if (!Array.isArray(s.slots) || s.slots.length === 0) errors.push("missing slots[]");
  if (!s.animations || typeof s.animations !== "object" || Object.keys(s.animations).length === 0) {
    errors.push("missing animations{}");
  }
  if (errors.length) throw new Error("skeleton.json sanity check FAILED: " + errors.join("; "));
  const names = new Set(s.bones.map((b) => b.name));
  for (const b of s.bones) {
    if (b.parent && !names.has(b.parent)) throw new Error(`bone ${b.name} unknown parent ${b.parent}`);
  }
}
sanityCheck(skel);

// ---- 最小正运动学 ------------------------------------------------------------
const bonesByName = new Map(skel.bones.map((b) => [b.name, b]));

function boneChain(name) {
  const chain = [];
  let cur = bonesByName.get(name);
  while (cur) {
    chain.unshift(cur);
    cur = cur.parent ? bonesByName.get(cur.parent) : null;
  }
  return chain;
}

function boneWorldTransform(name) {
  let px = 0, py = 0, prot = 0;
  for (const b of boneChain(name)) {
    const localRot = (b.rotation || 0) * Math.PI / 180;
    const lx = b.x || 0, ly = b.y || 0;
    px += lx * Math.cos(prot) - ly * Math.sin(prot);
    py += lx * Math.sin(prot) + ly * Math.cos(prot);
    prot += localRot;
    if (b.name === name) return { x: px, y: py, rotation: prot };
  }
  throw new Error("bone not found: " + name);
}

function interpolate(keys, t) {
  if (!keys || keys.length === 0) return 0;
  const val = (k) => (k.value !== undefined ? k.value : (k.angle ?? 0));
  if (t <= keys[0].time) return val(keys[0]);
  for (let i = 1; i < keys.length; i++) {
    const k1 = keys[i], k0 = keys[i - 1];
    if (t <= k1.time) {
      const span = k1.time - k0.time;
      const f = span > 0 ? (t - k0.time) / span : 0;
      return val(k0) + (val(k1) - val(k0)) * f;
    }
  }
  return val(keys[keys.length - 1]);
}

// ---- 每 clip 8 相位 → 32x32 覆盖率 -------------------------------------------
const PHASES = 8;
const GRID = 32;
const WORLD_HALF = 128;

// attachment 尺寸：取 default skin 第一个 slot 的第一个 region attachment。
const defSkin = skel.skins.find((k) => k.name === "default") ?? skel.skins[0];
const firstSlot = skel.slots[0].name;
const firstAttName = Object.keys(defSkin.attachments[firstSlot] ?? {})[0];
const attachment = defSkin.attachments[firstSlot]?.[firstAttName] ?? { width: 100, height: 100 };
const ATT_W = attachment.width ?? 100;
const ATT_H = attachment.height ?? 100;

function samplePhase(clip, clipName, k) {
  // 找驱动骨骼：优先 body，否则取第一个有 rotate 轨道的骨骼。
  let driver = "body";
  if (!(clip.bones && clip.bones[driver] && clip.bones[driver].rotate)) {
    driver = Object.keys(clip.bones ?? {}).find((bn) => clip.bones[bn]?.rotate?.length > 0);
  }
  const rotateKeys = driver ? clip.bones[driver].rotate : [];
  const duration = rotateKeys.length ? rotateKeys[rotateKeys.length - 1].time : 0;
  if (duration <= 0) return { duration: 0, phases: [] };

  const t = (k / PHASES) * duration;
  const angle = interpolate(rotateKeys, t);
  const base = boneWorldTransform(driver);
  const theta = base.rotation + angle * Math.PI / 180;
  const cosT = Math.cos(theta), sinT = Math.sin(theta);

  const occ = new Set();
  const halfW = ATT_W / 2, halfH = ATT_H / 2;
  for (let row = 0; row < GRID; row++) {
    for (let col = 0; col < GRID; col++) {
      const wx = -WORLD_HALF + (col + 0.5) * (2 * WORLD_HALF / GRID);
      const wy = WORLD_HALF - (row + 0.5) * (2 * WORLD_HALF / GRID);
      const dx = wx - base.x, dy = wy - base.y;
      const lx = dx * cosT + dy * sinT;
      const ly = -dx * sinT + dy * cosT;
      if (Math.abs(lx) <= halfW && Math.abs(ly) <= halfH) occ.add(row * GRID + col);
    }
  }
  return {
    t: Number(t.toFixed(4)),
    angle: Number(angle.toFixed(2)),
    cells: [...occ],
    cellCount: occ.size,
    occupancyRatio: Number((occ.size / (GRID * GRID)).toFixed(5)),
  };
}

const clips = {};
for (const [clipName, clip] of Object.entries(skel.animations)) {
  const rotateKeys = clip?.bones?.body?.rotate ?? [];
  const duration = rotateKeys.length ? rotateKeys[rotateKeys.length - 1].time : 0;
  const phases = [];
  for (let k = 0; k < PHASES; k++) phases.push(samplePhase(clip, clipName, k));
  clips[clipName] = { duration, phases };
}

const out = {
  gridSize: GRID,
  worldRange: [-WORLD_HALF, WORLD_HALF],
  attachment: { slot: firstSlot, name: firstAttName, width: ATT_W, height: ATT_H },
  clips,
};

fs.mkdirSync(path.dirname(outPath), { recursive: true });
fs.writeFileSync(outPath, JSON.stringify(out, null, 2), "utf8");

const clipCount = Object.keys(clips).length;
console.log(`gen-skeleton OK: ${clipCount} clips × ${PHASES} phases × ${GRID}x${GRID}`);
console.log("  " + outPath);
