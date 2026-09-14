// T-01 构建脚本（工作区级，`src-tauri/build.rs`）。
//
// 职责（Tauri 2 标准管线）：
//   1. 解析并校验 `tauri.conf.json`；
//   2. 生成 Windows 资源（应用图标、应用清单）与能力集代码；
//   3. 为 `tauri::generate_context!` 准备编译期产出物。
//
// ⚠️ 工作目录修正（关键）：
// `tauri-build` 以**构建脚本的当前工作目录**为基准定位 `tauri.conf.json`，
// 并据此解析配置中的 `bundle.icon`、`build.frontendDist` 等相对路径。
// 本工作区的二进制 crate 位于 `src-tauri/crates/dp-app`，而配置在 `src-tauri/` 根，
// 若不切换工作目录会报 “unable to read Tauri config file at .../crates/dp-app/tauri.conf.json”。
// 这里上溯查找配置文件所在目录并切换，避免写死层级深度。
//
// 本文件由承载 Tauri 运行时的二进制 crate（`crates/dp-app/build.rs`）通过 `include!` 复用，
// 以保证全工作区只有一处构建脚本定义。
//
// 扩展点（S1-M2 起）：若后续需要额外的代码生成（如配置 schema），在此处追加，
// 不要在子 crate 内另建构建脚本。
fn main() {
    let mut dir =
        std::env::current_dir().expect("读取构建脚本当前工作目录失败");

    while !dir.join("tauri.conf.json").is_file() {
        if !dir.pop() {
            panic!("上溯到文件系统根仍未找到 tauri.conf.json");
        }
    }

    std::env::set_current_dir(&dir)
        .unwrap_or_else(|e| panic!("切换构建工作目录到 {} 失败：{e}", dir.display()));

    tauri_build::build()
}
