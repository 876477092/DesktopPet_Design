#!/usr/bin/env node
/**
 * gen-schema.mjs —— 配置 JSON Schema 生成与校验（`02 §7.7-5`）。
 *
 * 产物：`resources/schema/*.schema.json` 由 `schemars` 从 Rust model 生成，
 * **schemars 保持 dev-only**（dp-core 的 `schema` feature 默认关，由本脚本以
 * `cargo run -p dp-core --features schema --bin export-schemas` 触发导出）。
 *
 * 覆盖范围（S4-M5 起 7 份）：settings / character / actions / emotion / needs /
 * animation / **lines**（台词库 `lines.json` 由 `dp-core::emotion::lines` 持有模型）。
 *
 * 模式：
 *   - 默认（无参）：触发导出 → 校验 7 份 schema 存在且为合法 JSON；
 *   - `--check`：**只比对磁盘内容**（不触发导出、不写文件）——校验 7 份配置与
 *     7 份 schema 一一对应存在、schema 均为合法 JSON；任一缺失/非法 → exit 1
 *     （CI 口径；漂移检测可由 CI 编排：导出到临时目录后 diff）。
 *
 * 环境（Windows / Git Bash）：cargo 需 MSVC x64 环境，先执行
 *   `source gate/s1-m2/msvc-env.sh`（构建环境注入脚本，见 03 §0.4），
 * 再运行本脚本；本脚本自身只调 cargo 并校验产物，不注入任何环境。
 *
 * 约束：脚本内禁止盘符字面量（C1）；路径相对工程根解析；零 npm 依赖。
 *
 * @module scripts/gen-schema.mjs
 */

import process from 'node:process';
import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** 退出码：0 = 成功，1 = 失败。 */
const EXIT_OK = 0;
const EXIT_FAIL = 1;

const scriptDir = dirname(fileURLToPath(import.meta.url));
/** 工程根（repo/）。 */
const repoRoot = join(scriptDir, '..');

/** 配置与 schema 的基名（一一对应；S4-M5 起为 7 份，新增 `lines`）。 */
const BASE_NAMES = ['settings', 'character', 'actions', 'emotion', 'needs', 'animation', 'lines'];

const configDir = join(repoRoot, 'resources', 'config');
const schemaDir = join(repoRoot, 'resources', 'schema');

/** 校验各份配置存在且为合法 JSON。返回 null（通过）或错误信息列表。 */
function checkConfigs() {
  const errors = [];
  for (const base of BASE_NAMES) {
    const path = join(configDir, `${base}.json`);
    if (!existsSync(path) || !statSync(path).isFile()) {
      errors.push(`缺少配置文件：${relative(path)}`);
      continue;
    }
    try {
      JSON.parse(readFileSync(path, 'utf8'));
    } catch (err) {
      errors.push(`配置文件非合法 JSON：${relative(path)}（${err.message}）`);
    }
  }
  return errors;
}

/** 校验各份 schema 存在且为合法 JSON。返回 null（通过）或错误信息列表。 */
function checkSchemas() {
  const errors = [];
  for (const base of BASE_NAMES) {
    const path = join(schemaDir, `${base}.schema.json`);
    if (!existsSync(path) || !statSync(path).isFile()) {
      errors.push(`缺少 schema：${relative(path)}`);
      continue;
    }
    let value;
    try {
      value = JSON.parse(readFileSync(path, 'utf8'));
    } catch (err) {
      errors.push(`schema 非合法 JSON：${relative(path)}（${err.message}）`);
      continue;
    }
    if (typeof value !== 'object' || value === null) {
      errors.push(`schema 应为对象：${relative(path)}`);
    }
  }
  return errors;
}

/** 以工程根为基准打印相对路径。 */
function relative(path) {
  return path.slice(repoRoot.length + 1) || path;
}

/** 默认模式：触发 cargo 导出并校验产物。 */
function runExport() {
  const srcTauri = join(repoRoot, 'src-tauri');
  console.log(`[gen-schema] 触发导出：cargo run -p dp-core --features schema --bin export-schemas`);
  console.log(`[gen-schema] 工作目录：${srcTauri}`);

  const result = spawnSync(
    'cargo',
    [
      'run',
      '--quiet',
      '-p',
      'dp-core',
      '--features',
      'schema',
      '--bin',
      'export-schemas',
      '--',
      '--out',
      schemaDir,
    ],
    { cwd: srcTauri, stdio: 'inherit', shell: false },
  );

  if (result.error && result.error.code === 'ENOENT') {
    console.error('[gen-schema] 未找到 cargo。Windows 下请先在 Git Bash 中执行：');
    console.error('[gen-schema]   source gate/s1-m2/msvc-env.sh   （注入 MSVC x64 环境）');
    console.error('[gen-schema] 再运行本脚本。');
    return EXIT_FAIL;
  }
  if (result.error) {
    console.error(`[gen-schema] cargo 启动失败：${result.error.message}`);
    return EXIT_FAIL;
  }
  if (result.status !== 0) {
    console.error(`[gen-schema] export-schemas 退出码 ${result.status}`);
    return EXIT_FAIL;
  }
  return EXIT_OK;
}

function main() {
  const args = process.argv.slice(2);
  const checkOnly = args.includes('--check');
  if (args.some((arg) => arg !== '--check')) {
    console.error(`[gen-schema] 未知参数：${args.filter((arg) => arg !== '--check').join(' ')}`);
    return EXIT_FAIL;
  }

  if (checkOnly) {
    // CI 口径：只比对磁盘内容（不触发导出、不写文件）。
    const errors = [...checkConfigs(), ...checkSchemas()];
    if (errors.length > 0) {
      for (const message of errors) console.error(`[gen-schema] ${message}`);
      console.error('[gen-schema] --check 未通过：请运行 `node scripts/gen-schema.mjs` 重新导出。');
      return EXIT_FAIL;
    }
    console.log(`[gen-schema] --check 通过：${BASE_NAMES.length} 份 schema 与配置一一对应且为合法 JSON。`);
    return EXIT_OK;
  }

  if (runExport() !== EXIT_OK) return EXIT_FAIL;

  const errors = [...checkConfigs(), ...checkSchemas()];
  if (errors.length > 0) {
    for (const message of errors) console.error(`[gen-schema] ${message}`);
    return EXIT_FAIL;
  }
  console.log(`[gen-schema] 已生成并校验 ${BASE_NAMES.length} 份 schema（${relative(schemaDir)}）。`);
  return EXIT_OK;
}

process.exit(main());
