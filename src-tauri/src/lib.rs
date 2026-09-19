//! 根包占位库。
//!
//! Tauri CLI 要求 `src-tauri/Cargo.toml` 带 `[package]` 才能读取版本/包名。
//! 本根包不承载任何应用逻辑；真正的桌面宠物可执行产物在
//! `crates/dp-app`（`[[bin]] name = "DesktopPet"`，与
//! `tauri.conf.json > build > mainBinaryName` 一致），由 tauri 以
//! `--bin DesktopPet` 构建。
