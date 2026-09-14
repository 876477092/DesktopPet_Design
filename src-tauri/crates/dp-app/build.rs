// T-01 构建脚本入口：复用工作区级 `src-tauri/build.rs`（单一定义，避免重复实现）。
//
// 路径说明：本文件位于 `src-tauri/crates/dp-app/build.rs`，`../..` 即 `src-tauri/`。
include!("../../build.rs");
