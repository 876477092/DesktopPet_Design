#!/usr/bin/env node
/**
 * gen-types.mjs —— Rust serde 结构 → TypeScript 类型（防漂移）。
 *
 * 【T-01 状态：占位实现】本脚本当前只校验前置条件并打印指引，
 * 由 **T-03 配置中心（S1-M3 / dp-core::config）** 填充真实实现。
 *
 * @module scripts/gen-types.mjs
 *
 * 预期行为（填充后）：
 *   1. 读取 `src-tauri/crates/dp-core/src/config/model.rs` 等 `#[derive(Serialize, Deserialize)]`
 *      结构体（后续改为读取 `resources/schema/*.schema.json`，由 `schemars` 生成）；
 *   2. 按 `02 §7.3` 命名规范（camelCase + 单位后缀 `Ms`/`Sec`/`Min`）转换为 TS 接口；
 *   3. 输出到 `src/shared/types.ts` 的「自动生成区」（手写区以 `// ---8<---` 标记分隔，不被覆盖）；
 *   4. 与既有文件不一致时以非零码退出，供 CI 拦截漂移。
 *
 * 计划参数（填充后）：
 *   --check    只校验不写入，差异时 exit 1（CI 用）
 *   --out <p>  指定输出文件（默认 `src/shared/types.ts`）
 *
 * 约束：脚本内禁止出现盘符字面量（C1）；路径一律相对工程根解析。
 */

import process from 'node:process';

/** 退出码：0 = 成功（占位阶段恒为 0）。 */
const EXIT_OK = 0;

function main() {
  // 占位阶段不接受任何参数，仅给出明确指引，避免误以为已产出类型。
  const args = process.argv.slice(2);
  if (args.length > 0) {
    console.warn(`[gen-types] 警告：当前为占位实现，已忽略参数：${args.join(' ')}`);
  }

  console.log('[gen-types] T-01 占位脚本：尚未生成类型。');
  console.log('[gen-types] TODO(T-03 / S1-M3)：由 dp-core::config 提供 schemars schema 后实现真实转换。');
  console.log('[gen-types] 目标输出：src/shared/types.ts（自动生成区）');
  process.exit(EXIT_OK);
}

main();
