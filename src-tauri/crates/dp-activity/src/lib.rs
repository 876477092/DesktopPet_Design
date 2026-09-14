//! `dp-activity`：外出活动（T-01 占位 crate）。
//!
//! 承载内容（`02 §3`）：`model/machine/clock/settle/events/postcard/anomaly`。
//!
//! 契约要点（`03 §4.3` / `02 §5 K-12`）：
//!   - `end_ms` 为 UTC 毫秒，wall-clock 结算；
//!   - 异常分支必须覆盖 DST 与小幅前拨；
//!   - 时间读取一律经 `dp-core::perception::time` 暴露的 `WallClock` 端口（C3）。
