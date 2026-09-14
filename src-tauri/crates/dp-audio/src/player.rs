//! `dp-audio::player`：解码 + 播放后端（**S4-M6**，`02 §1.3 dp-audio` / §7.7-4）。
//!
//! ## 职责
//!
//! 把「一个 OGG 文件 + 音量」播放出来；**不做**门控与队列（那是 [`crate::bus`] 的事）。
//! 后端以 trait 抽象（[`AudioSink`]），便于：
//!   - 无音频设备时降级 [`NullSink`]（`02 §7.4.2`：平台调用失败一律降级不崩溃）；
//!   - 单测用记录型后端断言「播了什么、音量多少、是否被停」。
//!
//! ## 线程口径（`02 §1.4`）
//!
//! [`RodioSink`] 持有 cpal 输出流（Windows = WASAPI），**只在 audio 线程内构造与使用**：
//! `OutputStream` 非 `Send`，跨线程共享会编译失败（这是刻意的防线，不是缺陷）。
//!
//! ## 音量口径
//!
//! `01 FR-7-3` 的「主音量 0~100」为**线性百分比**，映射为 rodio `Sink::set_volume`
//! 的线性振幅系数 `percent / 100`。不做等响度曲线（产品未要求，且曲线属音频设计决策）。

use std::path::Path;

use thiserror::Error;

/// 音频错误（`#[non_exhaustive]`，`02 §7.4`）。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AudioError {
    /// 音频输出设备初始化失败（无声卡 / 独占 / 驱动异常）。
    #[error("初始化音频输出设备失败：{0}")]
    Device(String),
    /// 解码或播放失败。
    #[error("播放失败（{path}）：{reason}")]
    Playback {
        /// 资源路径。
        path: String,
        /// 底层原因（**刻意不叫 `source`**：thiserror 会把名为 `source` 的字段当作
        /// `Error::source()`，而这里是文本原因而非嵌套错误）。
        reason: String,
    },
}

/// 播放后端抽象（唯一把「解码 + 出声」抽出来的缝，供降级与单测替换）。
pub trait AudioSink {
    /// 播放一个音频文件到指定音量（线性 0.0~1.0）。
    ///
    /// # Errors
    /// 文件缺失 / 解码失败 / 设备拒绝 → [`AudioError`]。
    fn play_file(&mut self, path: &Path, volume: f32) -> Result<(), AudioError>;

    /// 调整当前所有在播音效的音量（`01 FR-7-3`「调节音量立即生效」）。
    fn set_volume(&mut self, volume: f32);

    /// 停止全部在播音效（静音 / 勿扰开关切到「静默」时调用）。
    fn stop(&mut self);

    /// 后端是否真的能出声（`false` = 已降级为「静默但可用」）。
    fn is_available(&self) -> bool;
}

/// 空后端：全部 no-op，`is_available()` 为 `false`。
///
/// 用途：无音频设备时的降级实现 —— **不崩、不阻塞、不报错**，只是没声音
/// （`02 §7.4.2`）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl NullSink {
    /// 构造。
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl AudioSink for NullSink {
    fn play_file(&mut self, _path: &Path, _volume: f32) -> Result<(), AudioError> {
        Ok(())
    }

    fn set_volume(&mut self, _volume: f32) {}

    fn stop(&mut self) {}

    fn is_available(&self) -> bool {
        false
    }
}

/// rodio 后端（Windows：cpal → WASAPI）。
///
/// 在播音效以 `Sink` 列表持有（**不 detach**：需要保留句柄以便随时改音量 / 停播）；
/// 每次播放前先剪掉已播完的 `Sink`，避免列表无界增长。
pub struct RodioSink {
    /// cpal 输出流：必须存活，drop 会立刻停声。
    _stream: rodio::OutputStream,
    /// 播放句柄（创建 `Sink` 用）。
    handle: rodio::OutputStreamHandle,
    /// 在播 sink（改音量 / 停播的唯一入口）。
    sinks: Vec<rodio::Sink>,
}

impl RodioSink {
    /// 打开默认输出设备。
    ///
    /// # Errors
    /// 无可用设备 / 设备初始化失败 → [`AudioError::Device`]（调用方应降级 [`NullSink`]）。
    pub fn try_new() -> Result<Self, AudioError> {
        let (stream, handle) =
            rodio::OutputStream::try_default().map_err(|err| AudioError::Device(err.to_string()))?;
        Ok(Self { _stream: stream, handle, sinks: Vec::new() })
    }

    /// 当前在播 sink 数（诊断 / 单测）。
    #[must_use]
    pub fn active_sinks(&self) -> usize {
        self.sinks.len()
    }
}

impl AudioSink for RodioSink {
    fn play_file(&mut self, path: &Path, volume: f32) -> Result<(), AudioError> {
        let err = |reason: String| AudioError::Playback {
            path: path.display().to_string(),
            reason,
        };
        // 先剪掉已播完的 sink，保持列表有界。
        self.sinks.retain(|s| !s.empty());

        let file = std::fs::File::open(path).map_err(|e| err(e.to_string()))?;
        let decoder = rodio::Decoder::new(std::io::BufReader::new(file)).map_err(|e| err(e.to_string()))?;
        let sink = rodio::Sink::try_new(&self.handle).map_err(|e| err(e.to_string()))?;
        sink.set_volume(volume.clamp(0.0, 1.0));
        sink.append(decoder);
        self.sinks.push(sink);
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) {
        let v = volume.clamp(0.0, 1.0);
        for sink in &self.sinks {
            sink.set_volume(v);
        }
    }

    fn stop(&mut self) {
        for sink in &self.sinks {
            sink.stop();
        }
        self.sinks.clear();
    }

    fn is_available(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 记录型后端（单测用）：记录播放调用、音量与停播次数。
    #[derive(Debug, Default)]
    struct RecordingSink {
        played: Vec<(PathBuf, f32)>,
        volume: f32,
        stops: u32,
        fail_next: bool,
    }

    impl AudioSink for RecordingSink {
        fn play_file(&mut self, path: &Path, volume: f32) -> Result<(), AudioError> {
            if self.fail_next {
                return Err(AudioError::Playback {
                    path: path.display().to_string(),
                    reason: "测试注入失败".to_string(),
                });
            }
            self.played.push((path.to_path_buf(), volume));
            Ok(())
        }

        fn set_volume(&mut self, volume: f32) {
            self.volume = volume;
        }

        fn stop(&mut self) {
            self.stops += 1;
        }

        fn is_available(&self) -> bool {
            true
        }
    }

    #[test]
    fn null_sink_is_silent_and_available_false() {
        let mut sink = NullSink::new();
        assert!(!sink.is_available());
        assert!(sink.play_file(Path::new("nope.ogg"), 1.0).is_ok(), "空后端不得报错");
        sink.set_volume(0.5);
        sink.stop();
    }

    #[test]
    fn recording_sink_contract_is_sound() {
        let mut sink = RecordingSink::default();
        sink.play_file(Path::new("a.ogg"), 0.4).expect("应成功");
        assert_eq!(sink.played, vec![(PathBuf::from("a.ogg"), 0.4)]);
        sink.set_volume(0.8);
        assert_eq!(sink.volume, 0.8);
        sink.stop();
        assert_eq!(sink.stops, 1);
        assert!(sink.is_available());
    }

    #[test]
    fn playback_error_carries_path_and_reason() {
        let mut sink = RecordingSink { fail_next: true, ..RecordingSink::default() };
        let err = sink.play_file(Path::new("missing.ogg"), 1.0).expect_err("应报错");
        let text = err.to_string();
        assert!(text.contains("missing.ogg"), "错误须带路径：{text}");
        assert!(text.contains("测试注入失败"), "错误须带原因：{text}");
    }
}
