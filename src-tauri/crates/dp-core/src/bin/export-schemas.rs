//! `export-schemas` —— 由 Rust model 生成 `resources/schema/*.schema.json`。
//!
//! dev-only 工具（`02 §4.2` / §7.7-5）：仅在 `--features schema` 下参与编译，
//! 由 `scripts/gen-schema.mjs` 调用；默认 `cargo check/test --workspace` 不编译本 bin。
//!
//! 用法：
//!   ```text
//!   cargo run -p dp-core --features schema --bin export-schemas [-- --out <dir>]
//!   ```
//!   - 默认输出目录：`<工程根>/resources/schema/`（经 `CARGO_MANIFEST_DIR` 上溯三级
//!     推算，无盘符字面量，C1）；
//!   - `--out <dir>`：显式输出目录（供 CI 漂移检查导出到临时目录后 diff）。
//!
//! 产物：`settings.schema.json` / `character.schema.json` / `actions.schema.json` /
//! `emotion.schema.json` / `needs.schema.json` / `animation.schema.json` /
//! `lines.schema.json`（**S4-M5 新增第 7 份**）/ `schedule.schema.json`
//! （**S5-M5 新增第 8 份**，`02 §7.7-5`），与 `resources/config/*.json` 一一对应。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dp_core::config::model::{
    ActionsConfig, AnimationConfig, CharacterConfig, EmotionConfig, NeedsConfig, ScheduleConfig,
    SettingsConfig,
};
use dp_core::emotion::lines::LinesConfig;
use schemars::schema::RootSchema;

/// 配置文件基名 → schema 文件名（与 resources/config 一一对应）。
///
/// 注：`lines.json` 的模型住在 `dp-core::emotion::lines`（台词库是**内容资产**，
/// 由 `LinesLibrary` 自行加载与交叉校验），不在 `config::model` 的数值配置之列，
/// 故此处单独取用；schema 产物仍落在同一目录，`gen-schema` 一并校验（`02 §7.7-5`）。
const SCHEMA_TARGETS: [(&str, fn() -> RootSchema); 8] = [
    ("settings", || schemars::schema_for!(SettingsConfig)),
    ("character", || schemars::schema_for!(CharacterConfig)),
    ("actions", || schemars::schema_for!(ActionsConfig)),
    ("emotion", || schemars::schema_for!(EmotionConfig)),
    ("needs", || schemars::schema_for!(NeedsConfig)),
    ("animation", || schemars::schema_for!(AnimationConfig)),
    ("lines", || schemars::schema_for!(LinesConfig)),
    // S5-M5：`schedule.json`（提醒默认间隔 + 勿扰默认行为）纳入 schema 校验。
    ("schedule", || schemars::schema_for!(ScheduleConfig)),
];

/// 解析 `--out <dir>` 参数；缺省时定位工程根下的 `resources/schema`。
fn resolve_out_dir(args: &[String]) -> Result<PathBuf, String> {
    let default_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/schema");
    let mut out: Option<PathBuf> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--out" {
            let Some(value) = iter.next() else {
                return Err("--out 缺少目录参数".to_string());
            };
            out = Some(PathBuf::from(value));
        } else {
            return Err(format!("未知参数：{arg}（支持：--out <dir>）"));
        }
    }
    Ok(out.unwrap_or(default_dir))
}

/// 写出单份 schema（缺失目录自动创建，UTF-8 尾随换行）。
fn write_schema(out_dir: &Path, base: &str, schema: &RootSchema) -> Result<(), String> {
    let json = serde_json::to_string_pretty(schema)
        .map_err(|err| format!("schema {base} 序列化失败：{err}"))?;
    let path = out_dir.join(format!("{base}.schema.json"));
    std::fs::create_dir_all(out_dir)
        .map_err(|err| format!("创建输出目录 {} 失败：{err}", out_dir.display()))?;
    std::fs::write(&path, format!("{json}\n"))
        .map_err(|err| format!("写入 {} 失败：{err}", path.display()))?;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out_dir = match resolve_out_dir(&args) {
        Ok(dir) => dir,
        Err(message) => {
            eprintln!("[export-schemas] {message}");
            return ExitCode::FAILURE;
        }
    };

    let mut failed = false;
    for (base, factory) in SCHEMA_TARGETS {
        if let Err(message) = write_schema(&out_dir, base, &factory()) {
            eprintln!("[export-schemas] {message}");
            failed = true;
        } else {
            println!("[export-schemas] 已生成 {}.schema.json", base);
        }
    }

    if failed {
        ExitCode::FAILURE
    } else {
        println!("[export-schemas] 完成：{} 份 schema → {}", SCHEMA_TARGETS.len(), out_dir.display());
        ExitCode::SUCCESS
    }
}
