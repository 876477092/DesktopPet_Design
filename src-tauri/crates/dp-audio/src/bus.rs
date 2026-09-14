//! `dp-audio::bus`：音效请求总线（**S4-M6**，`02 §1.4` / §7.7-4 / §5 K-6）。
//!
//! ## 数据流
//!
//! ```text
//!   core-loop / 主线程                     audio 线程
//!   AudioBus::request(cue)  ──try_send──▶  有界队列(8)  ──▶ 门控复核 ──▶ sink 播放
//!         │  ▲                                                       ▲
//!         │  └── 门控（音量/静音/勿扰/穿透）在**入队前**判一次          │
//!         └── set_settings() ──▶ Arc<RwLock<AudioSettings>> ──200ms 轮询┘
//! ```
//!
//! ## 三条硬口径
//!
//!   1. **有界队列 8，溢出丢弃**（`02 §1.4`）：`try_send` 非阻塞，满即丢并计数
//!      （音效是「表现」不是「状态」，丢一条不影响任何数值正确性）；
//!   2. **不阻塞 core-loop**（`02 §1.4`）：入队是 `try_send`，播放全在 audio 线程；
//!   3. **门控纯函数**（[`resolve_play`]）：静音 / 主音量 0 / 勿扰 / 穿透 四种静默
//!      在这里裁决，**入队前先判一次**（不把注定不播的请求塞进队列），
//!      audio 线程出队时**再判一次**（设置可能在排队期间变化）。
//!
//! ## 静默优先级（判定顺序即短路顺序）
//!
//!   1. `muted` → [`SuppressReason::Muted`]（`01 FR-7-3`：托盘一键静音）；
//!   2. `masterVolumePercent == 0` → [`SuppressReason::VolumeZero`]；
//!   3. `doNotDisturb` → [`SuppressReason::DoNotDisturb`]（`01 FR-7-4`；**全部**音效静默）；
//!   4. `clickThrough` → [`SuppressReason::ClickThrough`]（`02 §5 K-6`：穿透下不播放）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::player::{AudioError, AudioSink, NullSink, RodioSink};

/// 音效队列上限（`02 §1.4`：队列上限 8，溢出丢弃）。
pub const AUDIO_QUEUE_CAP: usize = 8;

/// 设置轮询间隔（毫秒）：决定「改音量 / 切静音」的生效上限（`01 FR-7-3`「立即生效」）。
///
/// 口径说明：音量变更**不需要**跨线程触碰 sink（`OutputStream` 非 `Send`），
/// 改由 audio 线程 200ms 轮询设置快照并就地应用 —— 200ms 对滑块操作即「立即生效」，
/// 且比「每条设置变更都建一条控制消息」少一类消息（更小的状态面）。
pub const SETTINGS_POLL_MS: u64 = 200;

/// 音效类别（`01 §9.2` 六类；决定资源命名前缀，`02 §7.7-3` `<类别>_<语义>.ogg`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioCategory {
    /// 移动（脚步 / 跳跃 / 落地 / 风声）。
    Move,
    /// 交互（问候 / 比心 / 心跳 / 跺脚 / 魔法）。
    Interact,
    /// 情绪（哼歌 / 抽泣 / 叹气 / 哈欠 / 鼾 / 怒气 / 欢呼 / 关门）。
    Emotion,
    /// 生存（肚子咕咕 / 吧唧嘴 / 打嗝 / 泡泡 / 甩水）。
    Need,
    /// 活动（换装 / 开门 / 金币 / 行李箱 / 邮戳 / 翻书 / 喘气）。
    Activity,
    /// 提醒（风铃 / 礼花）。
    Remind,
}

impl AudioCategory {
    /// 资源名前缀（`02 §7.7-3`）。
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Interact => "interact",
            Self::Emotion => "emotion",
            Self::Need => "need",
            Self::Activity => "activity",
            Self::Remind => "remind",
        }
    }
}

/// 音效 Cue（`01 §9.2` 音效清单的枚举化；`asset_name()` 即 `assets/audio/` 下文件名）。
///
/// **本卡只做播放服务**：Cue 的**触发来源**（哪种情绪 / 交互播哪条）按里程碑归口——
/// 情绪态 → 音效的映射表在 `01 §6.5.5`，其接线随各表现模块落地；本卡交付枚举、
/// 门控、队列与播放，并在 `dp-app` 接上「已落地事件 → Cue」的那部分。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioCue {
    /// 慢步。
    MoveStepSlow,
    /// 快步。
    MoveStepFast,
    /// 跳跃「咻」。
    MoveJump,
    /// 落地「噗 / 咚」。
    MoveLand,
    /// 下落风声。
    MoveWind,
    /// 交互问候「嗨~」。
    InteractHi,
    /// 比心「叮」。
    InteractHeart,
    /// 心跳。
    InteractHeartbeat,
    /// 「嘿嘿」。
    InteractHehe,
    /// 「呀啊啊」。
    InteractYaa,
    /// 「哼！」。
    InteractHmph,
    /// 跺脚。
    InteractStomp,
    /// 魔法音。
    InteractMagic,
    /// 哼歌（开心）。
    EmotionHum,
    /// 抽泣（委屈）。
    EmotionSob,
    /// 叹气（无聊）。
    EmotionSigh,
    /// 哈欠（困倦）。
    EmotionYawn,
    /// 轻鼾（睡眠）。
    EmotionSnore,
    /// 怒气音（生气）。
    EmotionAnger,
    /// 惊喜音。
    EmotionSurprise,
    /// 欢呼（兴奋）。
    EmotionCheer,
    /// 关门声。
    EmotionDoorClose,
    /// 脚步渐远。
    EmotionStepsFade,
    /// 肚子咕咕（讨食）。
    NeedGrowl,
    /// 吧唧嘴 / 碗勺。
    NeedBite,
    /// 可爱打嗝。
    NeedBurp,
    /// 水声 / 泡泡（求洗澡）。
    NeedBubble,
    /// 甩水「唰」。
    NeedShake,
    /// 换装「唰」。
    ActivityDress,
    /// 开门声。
    ActivityDoor,
    /// 金币叮当（含连击）。
    ActivityCoin,
    /// 拉杆箱轮声。
    ActivityLuggage,
    /// 邮戳「啪」。
    ActivityStamp,
    /// 翻书 / 笔尖沙沙。
    ActivityBook,
    /// 喘气。
    ActivityPant,
    /// 风铃（提醒）。
    RemindChime,
    /// 礼花（成就 / 节日）。
    RemindFireworks,
}

impl AudioCue {
    /// 全部 Cue（清单遍历 / 资源齐备性校验用）。
    pub const ALL: [AudioCue; 37] = [
        Self::MoveStepSlow,
        Self::MoveStepFast,
        Self::MoveJump,
        Self::MoveLand,
        Self::MoveWind,
        Self::InteractHi,
        Self::InteractHeart,
        Self::InteractHeartbeat,
        Self::InteractHehe,
        Self::InteractYaa,
        Self::InteractHmph,
        Self::InteractStomp,
        Self::InteractMagic,
        Self::EmotionHum,
        Self::EmotionSob,
        Self::EmotionSigh,
        Self::EmotionYawn,
        Self::EmotionSnore,
        Self::EmotionAnger,
        Self::EmotionSurprise,
        Self::EmotionCheer,
        Self::EmotionDoorClose,
        Self::EmotionStepsFade,
        Self::NeedGrowl,
        Self::NeedBite,
        Self::NeedBurp,
        Self::NeedBubble,
        Self::NeedShake,
        Self::ActivityDress,
        Self::ActivityDoor,
        Self::ActivityCoin,
        Self::ActivityLuggage,
        Self::ActivityStamp,
        Self::ActivityBook,
        Self::ActivityPant,
        Self::RemindChime,
        Self::RemindFireworks,
    ];

    /// 资源文件名（`02 §7.7-3`：`<类别>_<语义>.ogg`）。
    #[must_use]
    pub fn asset_name(self) -> &'static str {
        match self {
            Self::MoveStepSlow => "move_step_slow.ogg",
            Self::MoveStepFast => "move_step_fast.ogg",
            Self::MoveJump => "move_jump.ogg",
            Self::MoveLand => "move_land.ogg",
            Self::MoveWind => "move_wind.ogg",
            Self::InteractHi => "interact_hi.ogg",
            Self::InteractHeart => "interact_heart.ogg",
            Self::InteractHeartbeat => "interact_heartbeat.ogg",
            Self::InteractHehe => "interact_hehe.ogg",
            Self::InteractYaa => "interact_yaa.ogg",
            Self::InteractHmph => "interact_hmph.ogg",
            Self::InteractStomp => "interact_stomp.ogg",
            Self::InteractMagic => "interact_magic.ogg",
            Self::EmotionHum => "emotion_hum.ogg",
            Self::EmotionSob => "emotion_sob.ogg",
            Self::EmotionSigh => "emotion_sigh.ogg",
            Self::EmotionYawn => "emotion_yawn.ogg",
            Self::EmotionSnore => "emotion_snore.ogg",
            Self::EmotionAnger => "emotion_anger.ogg",
            Self::EmotionSurprise => "emotion_surprise.ogg",
            Self::EmotionCheer => "emotion_cheer.ogg",
            Self::EmotionDoorClose => "emotion_door_close.ogg",
            Self::EmotionStepsFade => "emotion_steps_fade.ogg",
            Self::NeedGrowl => "need_growl.ogg",
            Self::NeedBite => "need_bite.ogg",
            Self::NeedBurp => "need_burp.ogg",
            Self::NeedBubble => "need_bubble.ogg",
            Self::NeedShake => "need_shake.ogg",
            Self::ActivityDress => "activity_dress.ogg",
            Self::ActivityDoor => "activity_door.ogg",
            Self::ActivityCoin => "activity_coin.ogg",
            Self::ActivityLuggage => "activity_luggage.ogg",
            Self::ActivityStamp => "activity_stamp.ogg",
            Self::ActivityBook => "activity_book.ogg",
            Self::ActivityPant => "activity_pant.ogg",
            Self::RemindChime => "remind_chime.ogg",
            Self::RemindFireworks => "remind_fireworks.ogg",
        }
    }

    /// 所属类别。
    #[must_use]
    pub fn category(self) -> AudioCategory {
        match self {
            Self::MoveStepSlow | Self::MoveStepFast | Self::MoveJump | Self::MoveLand
            | Self::MoveWind => AudioCategory::Move,
            Self::InteractHi
            | Self::InteractHeart
            | Self::InteractHeartbeat
            | Self::InteractHehe
            | Self::InteractYaa
            | Self::InteractHmph
            | Self::InteractStomp
            | Self::InteractMagic => AudioCategory::Interact,
            Self::EmotionHum
            | Self::EmotionSob
            | Self::EmotionSigh
            | Self::EmotionYawn
            | Self::EmotionSnore
            | Self::EmotionAnger
            | Self::EmotionSurprise
            | Self::EmotionCheer
            | Self::EmotionDoorClose
            | Self::EmotionStepsFade => AudioCategory::Emotion,
            Self::NeedGrowl | Self::NeedBite | Self::NeedBurp | Self::NeedBubble
            | Self::NeedShake => AudioCategory::Need,
            Self::ActivityDress
            | Self::ActivityDoor
            | Self::ActivityCoin
            | Self::ActivityLuggage
            | Self::ActivityStamp
            | Self::ActivityBook
            | Self::ActivityPant => AudioCategory::Activity,
            Self::RemindChime | Self::RemindFireworks => AudioCategory::Remind,
        }
    }
}

/// 音频设置快照（`settings.json.audio` + `settings.json.behavior` 的静音相关位）。
///
/// 与 `dp-core::config::model::{AudioCfg, BehaviorCfg}` 同源字段，但本 crate
/// **不依赖 dp-core**（避免环），故此处只镜像需要的四个标量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSettings {
    /// 主音量百分比（0~100，`AudioCfg.master_volume_percent`）。
    pub master_volume_percent: u32,
    /// 是否静音（`AudioCfg.muted`）。
    pub muted: bool,
    /// 是否勿扰（`BehaviorCfg.do_not_disturb`）。
    pub do_not_disturb: bool,
    /// 是否穿透（`BehaviorCfg.click_through`；`02 §5 K-6` 穿透下不播放）。
    pub click_through: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            master_volume_percent: 80,
            muted: false,
            do_not_disturb: false,
            click_through: false,
        }
    }
}

impl AudioSettings {
    /// 主音量归一（0.0~1.0；越界钳制）。
    #[must_use]
    pub fn volume_ratio(&self) -> f32 {
        (self.master_volume_percent.min(100) as f32) / 100.0
    }

    /// 生效音量：`Some(v)` = 可播；`None` = 静默（原因见 [`Self::suppress_reason`]）。
    #[must_use]
    pub fn effective_volume(&self) -> Option<f32> {
        if self.suppress_reason().is_some() {
            None
        } else {
            Some(self.volume_ratio())
        }
    }

    /// 静默原因（`None` = 可播）；判定顺序即优先级。
    #[must_use]
    pub fn suppress_reason(&self) -> Option<SuppressReason> {
        if self.muted {
            Some(SuppressReason::Muted)
        } else if self.master_volume_percent == 0 {
            Some(SuppressReason::VolumeZero)
        } else if self.do_not_disturb {
            Some(SuppressReason::DoNotDisturb)
        } else if self.click_through {
            Some(SuppressReason::ClickThrough)
        } else {
            None
        }
    }
}

/// 静默原因（诊断 / 计数维度）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressReason {
    /// 被静音开关拦下。
    Muted,
    /// 主音量为 0。
    VolumeZero,
    /// 勿扰模式（全部音效静默）。
    DoNotDisturb,
    /// 穿透模式（`02 §5 K-6`）。
    ClickThrough,
}

/// 入队结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOutcome {
    /// 已入队（将在 audio 线程播放）。
    Enqueued,
    /// 被门控静默（未入队）。
    Suppressed(SuppressReason),
    /// 队列满（溢出丢弃，`02 §1.4`）。
    QueueFull,
    /// audio 线程已退出（总线已关闭；不阻塞调用方）。
    Closed,
}

/// 门控纯函数：该 cue 在当前设置下应播多大音量。
///
/// `cue` 目前不参与判定（静默是**全局**的：静音 / 主音量 0 / 勿扰 / 穿透一律静默所有类别），
/// 参数保留是为了将来「按类别细调」时不必改签名；此处**显式**说明以免被误读为遗漏。
///
/// # Errors
/// 应静默时返回原因。
#[allow(clippy::unnecessary_wraps)]
pub fn resolve_play(settings: &AudioSettings, _cue: AudioCue) -> Result<f32, SuppressReason> {
    match settings.effective_volume() {
        Some(v) => Ok(v),
        None => Err(settings.suppress_reason().unwrap_or(SuppressReason::Muted)),
    }
}

/// 处理一条已出队的 cue（线程循环与单测共用）。
///
/// 返回 `Ok(true)` = 真的交给了后端播放；`Ok(false)` = 出队时被门控静默（设置已变）。
///
/// # Errors
/// 后端播放失败（文件缺失 / 解码失败）。
pub fn handle_cue(
    sink: &mut dyn AudioSink,
    settings: &AudioSettings,
    assets_dir: &Path,
    cue: AudioCue,
) -> Result<bool, AudioError> {
    match resolve_play(settings, cue) {
        Ok(volume) => {
            sink.play_file(&assets_dir.join(cue.asset_name()), volume)?;
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

/// 把设置变更应用到后端（audio 线程内调用）。
///
/// 语义：由「可播」变「静默」（切静音 / 拉到 0 / 开勿扰 / 开穿透）→ **立即停掉在播音效**
/// （不是等它播完）；音量变化 → 就地改在播 sink 的音量。
pub fn apply_settings(sink: &mut dyn AudioSink, prev: &AudioSettings, next: &AudioSettings) {
    match next.effective_volume() {
        Some(volume) => {
            if prev.effective_volume().is_none() || prev.volume_ratio() != volume {
                sink.set_volume(volume);
            }
        }
        None => {
            if prev.effective_volume().is_some() {
                sink.stop();
            }
        }
    }
}

/// 读取设置快照（容忍锁中毒：中毒不影响标量读，取 `into_inner` 而非 panic）。
fn read_settings(settings: &Arc<RwLock<AudioSettings>>) -> AudioSettings {
    match settings.read() {
        Ok(guard) => *guard,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

/// 音效总线句柄（`Clone` 共享：`SyncSender` + `Arc` 皆可克隆）。
#[derive(Debug, Clone)]
pub struct AudioBus {
    tx: SyncSender<AudioCue>,
    settings: Arc<RwLock<AudioSettings>>,
    assets_dir: Arc<PathBuf>,
    dropped: Arc<AtomicU64>,
    suppressed: Arc<AtomicU64>,
}

impl AudioBus {
    /// 构造总线与接收端（**不启动线程**）：供单测与自定义宿主使用。
    ///
    /// `cap` 会被钳到 `[1, 64]`（`sync_channel(0)` 是「会合」语义，不适用于本场景）。
    #[must_use]
    pub fn channel(
        settings: AudioSettings,
        assets_dir: PathBuf,
        cap: usize,
    ) -> (Self, Receiver<AudioCue>) {
        let cap = cap.clamp(1, 64);
        let (tx, rx) = sync_channel(cap);
        let bus = Self {
            tx,
            settings: Arc::new(RwLock::new(settings)),
            assets_dir: Arc::new(assets_dir),
            dropped: Arc::new(AtomicU64::new(0)),
            suppressed: Arc::new(AtomicU64::new(0)),
        };
        (bus, rx)
    }

    /// 启动 audio 线程并返回总线句柄。
    ///
    /// 线程内构造 [`RodioSink`]；失败（无设备 / 驱动异常）降级 [`NullSink`] 并打印一条
    /// 告警 —— **不阻塞、不崩溃**（`02 §7.4.2`）。线程在总线全部句柄 drop
    /// （发送端断开）后自然退出（`RecvTimeoutError::Disconnected`）。
    #[must_use]
    pub fn spawn(settings: AudioSettings, assets_dir: PathBuf) -> Self {
        let (bus, rx) = Self::channel(settings, assets_dir, AUDIO_QUEUE_CAP);
        let settings_shared = Arc::clone(&bus.settings);
        let assets = Arc::clone(&bus.assets_dir);
        // 线程启动失败时的降级句柄（`bus` 需同时供成功/失败两个分支取用，故先克隆一份）。
        let fallback = bus.clone();
        std::thread::Builder::new()
            .name("dp-audio".to_string())
            .spawn(move || run_audio_loop(rx, settings_shared, assets))
            .map_or_else(
                |err| {
                    eprintln!("[dp-audio] audio 线程启动失败，音效静默：{err}");
                    fallback
                },
                |_handle| bus,
            )
    }

    /// 请求播放一条音效（**非阻塞**）。
    ///
    /// 入队前先做门控（[`resolve_play`]）：注定不播的请求不入队，直接计入 `suppressed`；
    /// 队列满 → 丢弃并计入 `dropped`（`02 §1.4` 溢出丢弃口径）。
    pub fn request(&self, cue: AudioCue) -> RequestOutcome {
        let settings = self.settings();
        if let Err(reason) = resolve_play(&settings, cue) {
            self.suppressed.fetch_add(1, Ordering::Relaxed);
            return RequestOutcome::Suppressed(reason);
        }
        match self.tx.try_send(cue) {
            Ok(()) => RequestOutcome::Enqueued,
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                RequestOutcome::QueueFull
            }
            Err(TrySendError::Disconnected(_)) => RequestOutcome::Closed,
        }
    }

    /// 当前设置快照。
    #[must_use]
    pub fn settings(&self) -> AudioSettings {
        read_settings(&self.settings)
    }

    /// 更新设置（`01 FR-7-3` / FR-7-4 热更新入口；S5-M4 接线）。
    ///
    /// audio 线程会在 ≤ [`SETTINGS_POLL_MS`] 内应用（音量变更就地生效；
    /// 转为静默则立即停掉在播音效）。
    pub fn set_settings(&self, next: AudioSettings) {
        match self.settings.write() {
            Ok(mut guard) => *guard = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    /// 资源目录。
    #[must_use]
    pub fn assets_dir(&self) -> &Path {
        self.assets_dir.as_path()
    }

    /// 某 cue 的资源绝对路径。
    #[must_use]
    pub fn asset_path(&self, cue: AudioCue) -> PathBuf {
        self.assets_dir.join(cue.asset_name())
    }

    /// 累计因队列满 / 总线关闭而丢弃的请求数（诊断）。
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 累计因门控被静默的请求数（诊断）。
    #[must_use]
    pub fn suppressed_count(&self) -> u64 {
        self.suppressed.load(Ordering::Relaxed)
    }
}

/// audio 线程主体（`02 §1.4`：事件驱动 + 200ms 设置轮询；发送端断开关闭）。
fn run_audio_loop(
    rx: Receiver<AudioCue>,
    settings: Arc<RwLock<AudioSettings>>,
    assets_dir: Arc<PathBuf>,
) {
    let mut sink: Box<dyn AudioSink> = match RodioSink::try_new() {
        Ok(s) => Box::new(s),
        Err(err) => {
            eprintln!("[dp-audio] 音频设备不可用，降级静默后端：{err}");
            Box::new(NullSink::new())
        }
    };
    let mut current = read_settings(&settings);
    loop {
        match rx.recv_timeout(Duration::from_millis(SETTINGS_POLL_MS)) {
            Ok(cue) => {
                let next = read_settings(&settings);
                if next != current {
                    apply_settings(sink.as_mut(), &current, &next);
                    current = next;
                }
                if let Err(err) = handle_cue(sink.as_mut(), &current, &assets_dir, cue) {
                    eprintln!("[dp-audio] 播放降级：{err}");
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let next = read_settings(&settings);
                if next != current {
                    apply_settings(sink.as_mut(), &current, &next);
                    current = next;
                }
            }
            // 全部发送端 drop → 线程退出（无需显式关闭协议）。
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::AudioError;
    use std::path::Path;

    /// 记录型后端（与 `player` 单测同范式；此处再实现一份以避免跨模块测试耦合）。
    #[derive(Debug, Default)]
    struct Rec {
        played: Vec<(PathBuf, f32)>,
        volume: f32,
        stops: u32,
    }

    impl AudioSink for Rec {
        fn play_file(&mut self, path: &Path, volume: f32) -> Result<(), AudioError> {
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

    fn dir() -> PathBuf {
        PathBuf::from("assets").join("audio")
    }

    fn settings() -> AudioSettings {
        AudioSettings::default()
    }

    fn drain(rx: &Receiver<AudioCue>) -> Vec<AudioCue> {
        let mut out = Vec::new();
        while let Ok(cue) = rx.try_recv() {
            out.push(cue);
        }
        out
    }

    #[test]
    fn default_settings_play_at_80_percent() {
        let s = settings();
        assert_eq!(s.master_volume_percent, 80);
        let v = resolve_play(&s, AudioCue::EmotionHum).expect("默认设置应可播");
        assert!((v - 0.8).abs() < 1e-6, "0.8 音量，实际 {v}");
    }

    #[test]
    fn mute_and_zero_volume_are_silent() {
        let muted = AudioSettings { muted: true, ..settings() };
        assert_eq!(resolve_play(&muted, AudioCue::MoveStepSlow), Err(SuppressReason::Muted));
        assert!(muted.effective_volume().is_none());

        let zero = AudioSettings { master_volume_percent: 0, ..settings() };
        assert_eq!(resolve_play(&zero, AudioCue::MoveStepSlow), Err(SuppressReason::VolumeZero));
    }

    #[test]
    fn dnd_silences_every_cue() {
        let dnd = AudioSettings { do_not_disturb: true, ..settings() };
        for cue in AudioCue::ALL {
            assert_eq!(
                resolve_play(&dnd, cue),
                Err(SuppressReason::DoNotDisturb),
                "勿扰下 {cue:?} 必须静默"
            );
        }
    }

    #[test]
    fn click_through_silences_every_cue() {
        let ct = AudioSettings { click_through: true, ..settings() };
        for cue in AudioCue::ALL {
            assert!(resolve_play(&ct, cue).is_err(), "穿透下 {cue:?} 不得播放（K-6）");
        }
    }

    #[test]
    fn suppress_priority_order_is_fixed() {
        let all = AudioSettings {
            master_volume_percent: 0,
            muted: true,
            do_not_disturb: true,
            click_through: true,
        };
        assert_eq!(all.suppress_reason(), Some(SuppressReason::Muted), "静音优先级最高");
        let no_mute = AudioSettings { muted: false, ..all };
        assert_eq!(no_mute.suppress_reason(), Some(SuppressReason::VolumeZero));
        let dnd = AudioSettings { master_volume_percent: 50, ..no_mute };
        assert_eq!(dnd.suppress_reason(), Some(SuppressReason::DoNotDisturb));
        let ct = AudioSettings { do_not_disturb: false, ..dnd };
        assert_eq!(ct.suppress_reason(), Some(SuppressReason::ClickThrough));
    }

    #[test]
    fn volume_percent_clamps_above_100() {
        let s = AudioSettings { master_volume_percent: 250, ..settings() };
        assert_eq!(s.volume_ratio(), 1.0);
    }

    #[test]
    fn request_enqueues_and_counts_suppressed_without_blocking() {
        let (bus, rx) = AudioBus::channel(settings(), dir(), AUDIO_QUEUE_CAP);
        assert_eq!(bus.request(AudioCue::EmotionHum), RequestOutcome::Enqueued);
        assert_eq!(bus.request(AudioCue::MoveLand), RequestOutcome::Enqueued);
        assert_eq!(drain(&rx), vec![AudioCue::EmotionHum, AudioCue::MoveLand]);
        assert_eq!(bus.suppressed_count(), 0);
        assert_eq!(bus.dropped_count(), 0);

        bus.set_settings(AudioSettings { muted: true, ..settings() });
        assert_eq!(
            bus.request(AudioCue::EmotionHum),
            RequestOutcome::Suppressed(SuppressReason::Muted)
        );
        assert!(drain(&rx).is_empty(), "被静默的请求不得入队");
        assert_eq!(bus.suppressed_count(), 1);
    }

    #[test]
    fn queue_overflow_drops_newest_and_counts() {
        let (bus, rx) = AudioBus::channel(settings(), dir(), 3);
        for _ in 0..3 {
            assert_eq!(bus.request(AudioCue::MoveStepSlow), RequestOutcome::Enqueued);
        }
        assert_eq!(bus.request(AudioCue::MoveStepSlow), RequestOutcome::QueueFull);
        assert_eq!(bus.dropped_count(), 1);
        assert_eq!(drain(&rx).len(), 3, "已在队列中的 3 条不受影响");
    }

    #[test]
    fn closed_bus_reports_closed_without_panic() {
        let (bus, rx) = AudioBus::channel(settings(), dir(), AUDIO_QUEUE_CAP);
        drop(rx);
        assert_eq!(bus.request(AudioCue::EmotionHum), RequestOutcome::Closed);
    }

    #[test]
    fn handle_cue_plays_asset_path_at_volume() {
        let mut sink = Rec::default();
        let played = handle_cue(&mut sink, &settings(), &dir(), AudioCue::InteractHeart)
            .expect("应成功");
        assert!(played);
        assert_eq!(
            sink.played,
            vec![(dir().join("interact_heart.ogg"), 0.8)],
            "播放路径 = assets_dir/<类别>_<语义>.ogg"
        );
    }

    #[test]
    fn handle_cue_respects_settings_at_dequeue_time() {
        let mut sink = Rec::default();
        let dnd = AudioSettings { do_not_disturb: true, ..settings() };
        let played = handle_cue(&mut sink, &dnd, &dir(), AudioCue::EmotionHum).expect("应成功");
        assert!(!played, "出队时设置已变 → 不播但不报错");
        assert!(sink.played.is_empty());
    }

    #[test]
    fn apply_settings_stops_playback_when_becoming_silent() {
        let mut sink = Rec::default();
        let prev = settings();
        // 变静音 → 立即停播。
        let muted = AudioSettings { muted: true, ..prev };
        apply_settings(&mut sink, &prev, &muted);
        assert_eq!(sink.stops, 1, "转静默须立即停掉在播音效");
        // 静默保持静默 → 不重复 stop。
        apply_settings(&mut sink, &muted, &muted);
        assert_eq!(sink.stops, 1, "静默→静默不应重复停播");
        // 恢复 → 只改音量，不停播。
        apply_settings(&mut sink, &muted, &prev);
        assert_eq!(sink.stops, 1);
        assert!((sink.volume - 0.8).abs() < 1e-6);
    }

    #[test]
    fn apply_settings_updates_volume_when_only_volume_changed() {
        let mut sink = Rec::default();
        let prev = settings();
        let louder = AudioSettings { master_volume_percent: 55, ..prev };
        apply_settings(&mut sink, &prev, &louder);
        assert!((sink.volume - 0.55).abs() < 1e-6, "音量变更须就地应用");
        assert_eq!(sink.stops, 0);
        // 无变化 → 不触碰后端。
        let mut untouched = Rec { volume: -1.0, ..Rec::default() };
        apply_settings(&mut untouched, &louder, &louder);
        assert!((untouched.volume + 1.0).abs() < 1e-6, "无变化不应写音量");
    }

    #[test]
    fn set_settings_is_visible_to_readers() {
        let (bus, _rx) = AudioBus::channel(settings(), dir(), AUDIO_QUEUE_CAP);
        let next = AudioSettings { master_volume_percent: 33, click_through: true, ..settings() };
        bus.set_settings(next);
        assert_eq!(bus.settings(), next);
        assert_eq!(bus.assets_dir(), dir());
        assert!(bus.asset_path(AudioCue::RemindChime).ends_with("remind_chime.ogg"));
    }

    #[test]
    fn cue_catalog_is_complete_and_uniquely_named() {
        use std::collections::BTreeSet;
        let mut names = BTreeSet::new();
        for cue in AudioCue::ALL {
            let name = cue.asset_name();
            assert!(name.ends_with(".ogg"), "{name} 应为 .ogg（02 §7.7-4）");
            let prefix = cue.category().prefix();
            assert!(name.starts_with(prefix), "{name} 前缀应为 {prefix}（02 §7.7-3）");
            assert!(names.insert(name), "资源名重复：{name}");
        }
        assert_eq!(names.len(), AudioCue::ALL.len());
        assert_eq!(AudioCue::ALL.len(), 37, "01 §9.2 音效清单规模 ≈37（约 40 条）");
    }

    #[test]
    fn cue_catalog_resources_exist_and_within_budget() {
        // 资源齐备性 + 体积预算（`02 §7.7-4`：OGG ≤200KB/条）——P3-4 前置检查的机械化留证。
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/audio");
        for cue in AudioCue::ALL {
            let path = dir.join(cue.asset_name());
            assert!(
                path.is_file(),
                "缺少音效资源：{}（请先运行 scripts/gen-audio.py）",
                path.display()
            );
            let meta = std::fs::metadata(&path).expect("资源元数据应可读");
            assert!(
                meta.len() <= 200 * 1024,
                "{} 体积 {}B 超过 200KB 上限（02 §7.7-4）",
                cue.asset_name(),
                meta.len()
            );
            let head = std::fs::read(&path).expect("资源应可读");
            assert_eq!(&head[..4], b"OggS", "{} 不是 OGG 容器", cue.asset_name());
        }
    }

    #[test]
    fn spawn_degrades_without_panicking_in_headless_session() {
        // 本沙箱不一定有音频设备：spawn 必须降级而非 panic。
        let bus = AudioBus::spawn(settings(), dir());
        let _ = bus.request(AudioCue::EmotionHum);
        let _ = bus.request(AudioCue::RemindChime);
        assert_eq!(bus.suppressed_count(), 0);
    }
}
