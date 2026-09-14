//! `dp-audio`：音频播放（**S4-M6**，T-13 段 · 下；`02 §1.3 dp-audio` / §1.4 / §7.7-4）。
//!
//! 承载内容（`02 §3`）：[`player`]（解码 + 播放）、[`bus`]（事件总线接入与门控）。
//!
//! ## 契约要点
//!
//!   - 运行于独立 `std::thread`（线程名 `dp-audio`），**队列上限 8，溢出丢弃**
//!     （`02 §1.4`；`try_send` 非阻塞 ⇒ 绝不阻塞 core-loop）；
//!   - 音效 OGG ≤200KB/条，采样率 44.1kHz，统一归一化 −16 LUFS（`02 §7.7-4`）；
//!   - 资源来自只读资源目录，**禁止任何网络拉取**（C9；rodio/cpal/symphonia 全为
//!     本地解码与设备访问，`cargo tree` 无网络 crate）；
//!   - 主音量 / 静音统一控制（`01 FR-7-3`）；勿扰模式静默（`01 FR-7-4`）；
//!     穿透模式下不播放（`02 §5 K-6`）。
//!
//! ## 使用
//!
//! ```no_run
//! use dp_audio::{AudioBus, AudioCue, AudioSettings};
//! let bus = AudioBus::spawn(AudioSettings::default(), std::path::PathBuf::from("assets/audio"));
//! let _ = bus.request(AudioCue::InteractHeart);        // 非阻塞
//! bus.set_settings(AudioSettings { muted: true, ..AudioSettings::default() });
//! ```
//!
//! ## 边界（禁止顺手改动）
//!
//! 本 crate **不做情绪结算 / 不做台词**（那属 `dp-core`）；Cue 的**触发来源**由
//! 调用方（`dp-app`）按 `01 §6.5.5` 映射表接线。本 crate 也不读写配置文件：
//! 设置快照经 [`AudioBus::set_settings`] 由上层注入（配置真源在 `dp-core::config`）。

pub mod bus;
pub mod player;

pub use bus::{
    AUDIO_QUEUE_CAP, AudioBus, AudioCategory, AudioCue, AudioSettings, RequestOutcome,
    SETTINGS_POLL_MS, SuppressReason, apply_settings, handle_cue, resolve_play,
};
pub use player::{AudioError, AudioSink, NullSink, RodioSink};
