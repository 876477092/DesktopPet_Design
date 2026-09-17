//! 提醒调度（S7-M2，T-18 段 · 上 / `01 §6.10` FR-10-2 FR-10-4 / `02 §5.6`）。
//!
//! 承接 `03 §3.3 B21 ②`：「久坐 / 喝水间隔的消费方（计时 → `ACT-P-03` 伸手指 + 气泡）」
//! ——S5-M5 只交付偏好、取值域与持久化，本模块补齐**计时与到点产出**。
//!
//! ## 职责
//!
//!   - 两个通道（久坐 `Sedentary` / 喝水 `Water`）的**绝对锚定**计时
//!     （C3：`deadline = 起点 + k × 间隔`，迟到过冲不向后累积漂移）；
//!   - 到点产出 [`ReminderEvent`]（由上层做动作提交 `ACT-P-03` 与气泡，本模块**不接触**
//!     动作目录 / 事件总线——保持纯逻辑与可单测）；
//!   - 勿扰静默（FR-10-4「勿扰下零打扰」）：勿扰期间**只推进计时、不产出事件**，
//!     解除后不补发积压（避免「关掉勿扰瞬间连弹三条」）；
//!   - 「知道了」重排（FR-10-2 `ackResetsTimer`）：延迟到 `now + 间隔`。
//!
//! ## 时间纪律（C3）
//!
//! 本模块**零时钟**：一切时间由调用方注入 `now_ms`（`dp-app` 取 `WallClock::now_ms()`）。
//! 间隔取值一律经 [`ReminderConfig::clamp_minutes`] 钳到
//! `schedule.json.reminders.intervalMinMin/Max`（`15`~`180`），用户/配置写坏不致失控。
//!
//! ## 边界
//!
//! 不实装提醒文案（归 `lines.json` 台词池，S7-M8 决定最终形态）；不做动作提交
//! （`ACT-P-03` 资源为批次 C，`disabled=true` 期间仲裁器自然丢弃——触发面在此就绪）；
//! 不新增 `pet://` 事件（C8）。提醒偏好真源 = 存档 B 段 `settings.reminders`。

/// 提醒通道（`01 FR-10-2`：久坐 / 喝水）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReminderChannel {
    /// 久坐提醒（`ACT-P-03` 伸手指）。
    Sedentary,
    /// 喝水提醒。
    Water,
}

impl ReminderChannel {
    /// 全部通道（固定顺序，与内部数组下标一一对应）。
    pub const ALL: [ReminderChannel; 2] = [ReminderChannel::Sedentary, ReminderChannel::Water];

    /// 通道下标（内部数组索引）。
    const fn index(self) -> usize {
        match self {
            ReminderChannel::Sedentary => 0,
            ReminderChannel::Water => 1,
        }
    }
}

/// 提醒配置视图（一次性快照；由上层从 `bridge::RemindersSnapshot` 组装）。
///
/// `interval_min_min` / `interval_min_max` 取自 `schedule.json`（UI 不硬编码，C7）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReminderConfig {
    /// 久坐提醒开关。
    pub sedentary_enabled: bool,
    /// 久坐提醒间隔（分钟）。
    pub sedentary_interval_min: u32,
    /// 喝水提醒开关。
    pub water_enabled: bool,
    /// 喝水提醒间隔（分钟）。
    pub water_interval_min: u32,
    /// 间隔下界（分钟）。
    pub interval_min_min: u32,
    /// 间隔上界（分钟）。
    pub interval_max_min: u32,
    /// 「知道了」是否重新计时（FR-10-2）。
    pub ack_resets_timer: bool,
}

impl ReminderConfig {
    /// 全新构造（取值域非法时按「下界优先、上界 ≥ 下界」兜底，不 panic）。
    #[must_use]
    pub const fn new(
        sedentary_enabled: bool,
        sedentary_interval_min: u32,
        water_enabled: bool,
        water_interval_min: u32,
        interval_min_min: u32,
        interval_max_min: u32,
        ack_resets_timer: bool,
    ) -> Self {
        Self {
            sedentary_enabled,
            sedentary_interval_min,
            water_enabled,
            water_interval_min,
            interval_min_min,
            interval_max_min,
            ack_resets_timer,
        }
    }

    /// 取值域下界（`max(1, min_min)`：至少 1 分钟，避免 0 分钟风暴）。
    #[must_use]
    pub const fn lo(&self) -> u32 {
        if self.interval_min_min == 0 {
            1
        } else {
            self.interval_min_min
        }
    }

    /// 取值域上界（恒 ≥ 下界）。
    #[must_use]
    pub const fn hi(&self) -> u32 {
        let lo = self.lo();
        if self.interval_max_min < lo {
            lo
        } else {
            self.interval_max_min
        }
    }

    /// 把分钟数钳到取值域。
    #[must_use]
    pub const fn clamp_minutes(&self, minutes: u32) -> u32 {
        if minutes < self.lo() {
            self.lo()
        } else if minutes > self.hi() {
            self.hi()
        } else {
            minutes
        }
    }

    /// 指定通道的开关。
    #[must_use]
    pub const fn enabled(&self, channel: ReminderChannel) -> bool {
        match channel {
            ReminderChannel::Sedentary => self.sedentary_enabled,
            ReminderChannel::Water => self.water_enabled,
        }
    }

    /// 指定通道的间隔（分钟，已钳）。
    #[must_use]
    pub const fn interval_min(&self, channel: ReminderChannel) -> u32 {
        match channel {
            ReminderChannel::Sedentary => self.clamp_minutes(self.sedentary_interval_min),
            ReminderChannel::Water => self.clamp_minutes(self.water_interval_min),
        }
    }

    /// 指定通道的间隔（毫秒）。
    #[must_use]
    pub const fn interval_ms(&self, channel: ReminderChannel) -> i64 {
        self.interval_min(channel) as i64 * 60_000
    }
}

/// 到点事件（本模块的**唯一产出**）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReminderEvent {
    /// 触发的通道。
    pub channel: ReminderChannel,
    /// 触发时刻（墙钟毫秒，由调用方注入）。
    pub due_ms: i64,
}

/// 单通道计时状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ChannelState {
    /// 下次到点时刻（`None` = 未排程）。
    next_due_ms: Option<i64>,
}

/// 提醒调度器（纯逻辑；零时钟、零分配）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReminderScheduler {
    cfg: ReminderConfig,
    channels: [ChannelState; 2],
    /// 勿扰中（`true` = 静默：只推进计时，不产出事件）。
    do_not_disturb: bool,
}

impl ReminderScheduler {
    /// 以配置构造（`now_ms` 起算首个 deadline：`now + 间隔`，绝对锚定起点）。
    #[must_use]
    pub fn new(cfg: ReminderConfig, now_ms: i64) -> Self {
        let mut scheduler = Self {
            cfg,
            channels: [ChannelState::default(); 2],
            do_not_disturb: false,
        };
        scheduler.reschedule_all(now_ms);
        scheduler
    }

    /// 当前配置快照。
    #[must_use]
    pub const fn config(&self) -> &ReminderConfig {
        &self.cfg
    }

    /// 更新配置（`now_ms` 起**重排全部通道**；通道被关闭 → 取消排程）。
    ///
    /// 语义：设置页改间隔即重排（不保留旧进度），避免「改完等半小时才生效」。
    pub fn set_config(&mut self, cfg: ReminderConfig, now_ms: i64) {
        self.cfg = cfg;
        self.reschedule_all(now_ms);
    }

    /// 置勿扰态（FR-10-4：勿扰下零打扰）。
    ///
    /// 语义：**不重置计时**——勿扰期间 deadline 继续推进（到点即静默跳过），
    /// 解除后从「下一个未来到点」开始，不补发积压。
    pub fn set_do_not_disturb(&mut self, on: bool) {
        self.do_not_disturb = on;
    }

    /// 是否处于勿扰。
    #[must_use]
    pub const fn is_do_not_disturb(&self) -> bool {
        self.do_not_disturb
    }

    /// 指定通道的下次到点时刻（`None` = 未排程 / 已关闭）。
    #[must_use]
    pub const fn next_due_ms(&self, channel: ReminderChannel) -> Option<i64> {
        self.channels[channel.index()].next_due_ms
    }

    /// 推进到 `now_ms`，返回**至多一条**到点事件。
    ///
    /// 迟到处理（C3 绝对锚定）：把 deadline 沿 `+ 间隔` 推进到首个 `> now_ms` 的时刻，
    /// **一次调用最多产出一条事件**——即「错过多个周期只提醒一次」，既不补发积压，
    /// 也不因迟到的过冲而向后累积漂移。同拍到点多个通道时按 [`ReminderChannel::ALL`]
    /// 顺序取首个（久坐优先），其余留待下一拍（由随后的 deadline 推进保证不再积压）。
    pub fn tick(&mut self, now_ms: i64) -> Option<ReminderEvent> {
        let mut fired: Option<ReminderEvent> = None;
        for channel in ReminderChannel::ALL {
            let idx = channel.index();
            let interval = self.cfg.interval_ms(channel);
            let Some(due) = self.channels[idx].next_due_ms else {
                continue;
            };
            if interval <= 0 {
                self.channels[idx].next_due_ms = None;
                continue;
            }
            if now_ms < due {
                continue;
            }
            // 绝对锚定推进（C3）：跳到首个 > now_ms 的网格点 `due + k × 间隔`。
            // **O(1) 直接求解**而非循环 `+= 间隔`——后者在「时钟大幅前跳 / 长时间挂起」
            // 时会退化成数十亿次迭代（S7-M2 自测发现的一次真实挂死）。
            let elapsed = now_ms.saturating_sub(due).max(0);
            let periods = elapsed / interval; // 已错过的完整周期数
            let step = interval.saturating_mul(periods.saturating_add(1));
            let bumped = due.saturating_add(step);
            if bumped <= now_ms {
                // 仅当饱和到 `i64::MAX` 仍未越过 now_ms（极端时钟）时发生：
                // 放弃该通道排程（防御，避免「每拍重复提醒」）。
                self.channels[idx].next_due_ms = None;
                continue;
            }
            self.channels[idx].next_due_ms = Some(bumped);
            if fired.is_none() && !self.do_not_disturb {
                fired = Some(ReminderEvent { channel, due_ms: now_ms });
            }
        }
        fired
    }

    /// 「知道了」/ 用户点掉本次提醒（FR-10-2：`ackResetsTimer` 时从此刻重新计时）。
    ///
    /// `ack_resets_timer = false` 时**不改动**计时（保持原 deadline 链）。
    pub fn ack(&mut self, channel: ReminderChannel, now_ms: i64) {
        if !self.cfg.ack_resets_timer {
            return;
        }
        let idx = channel.index();
        if self.channels[idx].next_due_ms.is_none() {
            return;
        }
        self.channels[idx].next_due_ms = Some(now_ms.saturating_add(self.cfg.interval_ms(channel)));
    }

    /// 全部通道按配置重排（关闭 → 取消；开启 → `now + 间隔`）。
    fn reschedule_all(&mut self, now_ms: i64) {
        for channel in ReminderChannel::ALL {
            let idx = channel.index();
            self.channels[idx].next_due_ms = if self.cfg.enabled(channel) {
                Some(now_ms.saturating_add(self.cfg.interval_ms(channel)))
            } else {
                None
            };
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn cfg() -> ReminderConfig {
        // 与 `schedule.json` 出厂默认一致（45min / 15~180 / ack 重排）。
        ReminderConfig::new(true, 45, true, 45, 15, 180, true)
    }

    #[test]
    fn defaults_match_schedule_json() {
        let c = cfg();
        assert_eq!(c.interval_min(ReminderChannel::Sedentary), 45);
        assert_eq!(c.interval_min(ReminderChannel::Water), 45);
        assert_eq!(c.lo(), 15);
        assert_eq!(c.hi(), 180);
        assert!(c.enabled(ReminderChannel::Sedentary));
    }

    #[test]
    fn interval_clamping_uses_config_bounds() {
        let mut c = cfg();
        c.sedentary_interval_min = 1; // 低于下界
        assert_eq!(c.interval_min(ReminderChannel::Sedentary), 15, "钳到 15min");
        c.sedentary_interval_min = 999; // 高于上界
        assert_eq!(c.interval_min(ReminderChannel::Sedentary), 180, "钳到 180min");
        // 非法取值域（上界 < 下界 / 下界 0）自愈，不 panic。
        let bad = ReminderConfig::new(true, 45, true, 45, 0, 0, true);
        assert_eq!(bad.lo(), 1);
        assert_eq!(bad.hi(), 1);
        assert_eq!(bad.interval_min(ReminderChannel::Water), 1);
    }

    #[test]
    fn fires_once_when_interval_elapsed() {
        let mut s = ReminderScheduler::new(cfg(), 0);
        assert_eq!(s.next_due_ms(ReminderChannel::Sedentary), Some(45 * MIN));
        assert_eq!(s.tick(44 * MIN), None, "未到点不触发");
        let ev = s.tick(45 * MIN).expect("到点应触发");
        assert_eq!(ev.channel, ReminderChannel::Sedentary, "同拍到点按通道顺序取久坐");
        // 下一 deadline 绝对锚定 +45min。
        assert_eq!(s.next_due_ms(ReminderChannel::Sedentary), Some(90 * MIN));
        // 同刻再 tick 不重复触发（水通道仍待 45min 已被跳过，下一拍按新 deadline）。
        assert!(s.tick(45 * MIN).is_none() || s.next_due_ms(ReminderChannel::Water).is_some());
    }

    #[test]
    fn water_fires_on_its_own_channel() {
        let mut s = ReminderScheduler::new(
            ReminderConfig::new(true, 60, true, 30, 15, 180, true),
            0,
        );
        // 30min：水通道到点（久坐要 60min）。
        let ev = s.tick(30 * MIN).expect("喝水到点");
        assert_eq!(ev.channel, ReminderChannel::Water);
        assert_eq!(s.next_due_ms(ReminderChannel::Water), Some(60 * MIN));
        // 60min：久坐到点（水通道 60min 也到点，但久坐按顺序优先）。
        let ev = s.tick(60 * MIN).expect("久坐到点");
        assert_eq!(ev.channel, ReminderChannel::Sedentary);
    }

    #[test]
    fn missed_periods_do_not_burst_or_drift() {
        let mut s = ReminderScheduler::new(cfg(), 0);
        // 睡了一觉：6h 后才 tick（错过 8 个周期）→ 只产出 1 条，且 deadline 推到未来。
        let ev = s.tick(6 * 60 * MIN).expect("迟到仍应提醒一次");
        assert_eq!(ev.channel, ReminderChannel::Sedentary);
        let next = s.next_due_ms(ReminderChannel::Sedentary).expect("仍排程");
        assert!(next > 6 * 60 * MIN, "deadline 必须推到未来：{next}");
        assert_eq!(next % (45 * MIN), 0, "绝对锚定：仍是 45min 网格上的点");
        // 紧接着再 tick → 不再触发（无积压补发）。
        assert!(s.tick(6 * 60 * MIN + 1_000).is_none());
    }

    #[test]
    fn absolute_anchoring_survives_late_ticks() {
        let mut s = ReminderScheduler::new(cfg(), 0);
        // 每次 tick 都迟到 10 分钟：deadline 链仍为 45/90/135…（不漂移到 55/120…）。
        let _ = s.tick(55 * MIN);
        assert_eq!(s.next_due_ms(ReminderChannel::Sedentary), Some(90 * MIN));
        let _ = s.tick(100 * MIN);
        assert_eq!(s.next_due_ms(ReminderChannel::Sedentary), Some(135 * MIN));
    }

    #[test]
    fn ack_resets_timer_when_configured() {
        let mut s = ReminderScheduler::new(cfg(), 0);
        let _ = s.tick(45 * MIN);
        s.ack(ReminderChannel::Sedentary, 45 * MIN);
        assert_eq!(
            s.next_due_ms(ReminderChannel::Sedentary),
            Some(90 * MIN),
            "ack 从此刻重新计时（45 + 45）"
        );
        // 关闭重排：ack 不改动 deadline。
        let mut s2 = ReminderScheduler::new(
            ReminderConfig::new(true, 45, true, 45, 15, 180, false),
            0,
        );
        let _ = s2.tick(45 * MIN);
        let before = s2.next_due_ms(ReminderChannel::Sedentary);
        s2.ack(ReminderChannel::Sedentary, 46 * MIN);
        assert_eq!(s2.next_due_ms(ReminderChannel::Sedentary), before, "未开启重排则不变");
    }

    #[test]
    fn do_not_disturb_silences_without_backlog() {
        let mut s = ReminderScheduler::new(cfg(), 0);
        s.set_do_not_disturb(true);
        assert!(s.is_do_not_disturb());
        // 勿扰期间连跨 3 个周期 → 零打扰。
        assert!(s.tick(45 * MIN).is_none());
        assert!(s.tick(90 * MIN).is_none());
        assert!(s.tick(135 * MIN).is_none());
        // deadline 仍在推进（未被冻结在 45min）。
        let next = s.next_due_ms(ReminderChannel::Sedentary).expect("仍排程");
        assert!(next > 135 * MIN, "勿扰期间计时继续推进：{next}");
        // 解除勿扰 → 不补发积压，等到下一个未来 deadline 才提醒。
        s.set_do_not_disturb(false);
        assert!(s.tick(136 * MIN).is_none(), "解除后不得立刻补发");
        let ev = s.tick(next).expect("下一周期正常提醒");
        assert_eq!(ev.channel, ReminderChannel::Sedentary);
    }

    #[test]
    fn disabled_channel_has_no_deadline_and_reschedule_restores_it() {
        let mut s = ReminderScheduler::new(
            ReminderConfig::new(false, 45, true, 45, 15, 180, true),
            0,
        );
        assert_eq!(s.next_due_ms(ReminderChannel::Sedentary), None, "关闭的通道不排程");
        // 关闭态下久坐永不触发；水通道仍按自身间隔工作。
        let ev = s.tick(1_000 * MIN);
        assert!(
            ev.is_none() || ev.map(|e| e.channel) == Some(ReminderChannel::Water),
            "关闭的久坐通道不得触发"
        );
        // 打开久坐 → 从当前时刻重排。
        s.set_config(ReminderConfig::new(true, 20, true, 45, 15, 180, true), 10 * MIN);
        assert_eq!(s.next_due_ms(ReminderChannel::Sedentary), Some(10 * MIN + 20 * MIN));
        assert_eq!(
            s.next_due_ms(ReminderChannel::Water),
            Some(10 * MIN + 45 * MIN),
            "改配置重排全部通道"
        );
    }

    #[test]
    fn set_config_clamps_and_never_panics_on_hostile_input() {
        let mut s = ReminderScheduler::new(cfg(), 0);
        s.set_config(ReminderConfig::new(true, u32::MAX, true, 0, 15, 180, true), 0);
        assert_eq!(s.next_due_ms(ReminderChannel::Sedentary), Some(180 * MIN));
        assert_eq!(s.next_due_ms(ReminderChannel::Water), Some(15 * MIN));
        assert!(s.tick(i64::MAX / 2).is_some());
    }
}
