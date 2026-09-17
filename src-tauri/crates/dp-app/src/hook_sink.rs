//! `dp-app/src/hook_sink.rs` —— S3-M1 钩子事件投递口的具体实装（装配层）。
//!
//! 职责（**仅装配层，零算法**）：把 `dp-platform` 钩子回调投递的 [`HookEvent`] 经
//! **std 有界队列**（`mpsc::sync_channel`）**非阻塞**转发给消费端，满则丢并计数。
//!
//! ## 设计依据（`gate/arch-audit/2026-09-13-S3M1-实现设计补充.md` §3）
//! - **零新增依赖**：std `sync_channel` 满足「有界 / 非阻塞 / 每 send 零分配」，优于引入 `crossbeam`；
//!   `dp-platform` 保持零依赖（只定义 `HookSink` 端口，队列落在本 crate）。
//! - **回调热路径**：`SyncSender::try_send` 非阻塞、不分配；`Full` → 丢 + `AtomicU64` 计数（不阻塞、不分配）；
//!   `Disconnected` → 静默（消费端已退出）。
//! - **消费端**：S3-M1 阶段只做**计数 / 降级日志**（[`ChannelSink::drain`]）；S3-M3 手势状态机接管消费。
//!   消费**不在回调内**。
//!
//! ## S7-M1 扩展：鼠标输入强度累加（`02 §5.6`「点击/移动强度 = 复用累加器，单次成本 0」）
//!
//! 钩子回调内追加两个**无锁原子累加**（点击次数 / 光标累计位移）——与既有
//! `try_send` 同级成本（`Relaxed` 原子加），不引入锁、不引入分配，满足
//! `02 §5.6` 的单次成本 0 约束。键盘强度**不在此处**（归 `dp-platform::win::keyhook`
//! 的 `WH_KEYBOARD_LL` 只计数钩子）。
//!
//! ## 红线
//! C9：零网络；不改 `Cargo.toml`（零新增依赖）；不新增 `pet://` 事件（C8）。

#![cfg(windows)]

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Mutex;

use dp_platform::win::hook::{HookEvent, HookSink};

/// 钩子事件队列默认容量（有界；`02 §5.24 L-04`：丢弃计数为 0 或仅在队列满时 ≤1%）。
pub const DEFAULT_HOOK_QUEUE_CAPACITY: usize = 512;

/// 「无上一次采样点」哨兵值（S7-M1：`last_pt` 初始态）。
const NO_POINT: i64 = i64::MIN;

/// 物理像素点打包为 `i64`（S7-M1：单原子保存二维采样点，避免两原子撕裂）。
#[inline]
fn pack_pt(x: i32, y: i32) -> i64 {
    (i64::from(x) << 32) | i64::from(y as u32)
}

/// 解包 `pack_pt` 的结果。
#[inline]
fn unpack_pt(packed: i64) -> (i32, i32) {
    ((packed >> 32) as i32, packed as u32 as i32)
}

/// 基于 std 有界队列的钩子事件投递口（`HookSink` 实装）。
///
/// - `send` 侧（[`SyncSender`]）：回调线程调用，**非阻塞 / 零分配**；
/// - `recv` 侧（[`Receiver`]）：消费端调用，经 [`Mutex`] 收口以保持 `Sync`
///   （`HookSink: Send + Sync` 契约要求；`Receiver` 本身非 `Sync`）；
/// - [`AtomicU64`] 丢弃计数：队列满（`Full`）时自增（`02 §5.24 L-04` ④）。
pub struct ChannelSink {
    tx: SyncSender<HookEvent>,
    rx: Mutex<Receiver<HookEvent>>,
    dropped: AtomicU64,
    /// 鼠标按键「按下」累计（S7-M1：点击强度来源；回调内 `Relaxed` 自增）。
    clicks: AtomicU64,
    /// 光标累计位移（S7-M1：移动强度来源；单位 = 物理像素，与钩子载荷同坐标系）。
    move_px: AtomicU64,
    /// 上一次移动采样点（`i32` 位模式打包为 `i64`；`None` 态用 `i64::MIN` 表示）。
    last_pt: AtomicI64,
}

impl ChannelSink {
    /// 构造：容量 `capacity`（`0` 会被钳为 `1`，避免 `sync_channel(0)` 语义意外）。
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, rx) = sync_channel::<HookEvent>(capacity.max(1));
        Self {
            tx,
            rx: Mutex::new(rx),
            dropped: AtomicU64::new(0),
            clicks: AtomicU64::new(0),
            move_px: AtomicU64::new(0),
            last_pt: AtomicI64::new(NO_POINT),
        }
    }

    /// 鼠标按键「按下」累计次数（S7-M1 点击强度来源）。
    #[must_use]
    pub fn clicks(&self) -> u64 {
        self.clicks.load(Ordering::Relaxed)
    }

    /// 光标累计位移（物理像素；S7-M1 移动强度来源）。
    #[must_use]
    pub fn move_px(&self) -> u64 {
        self.move_px.load(Ordering::Relaxed)
    }

    /// 累计丢弃事件数（队列满导致）。
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 非阻塞取一个事件（消费端用；空队列返回 `None`）。
    #[must_use]
    pub fn try_recv(&self) -> Option<HookEvent> {
        self.rx.lock().unwrap_or_else(|e| e.into_inner()).try_recv().ok()
    }

    /// 排空当前队列并返回本次排空的事件数（S3-M1 仅计数 / 降级日志；S3-M3 接管消费）。
    pub fn drain(&self) -> usize {
        let rx = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        let mut n = 0usize;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        n
    }
}

impl HookSink for ChannelSink {
    fn on_event(&self, ev: HookEvent) {
        // S7-M1：输入强度累加（无锁、零分配；见模块文档「S7-M1 扩展」）。
        match ev {
            HookEvent::Move { x, y } => {
                let packed = pack_pt(x, y);
                let prev = self.last_pt.swap(packed, Ordering::Relaxed);
                if prev != NO_POINT {
                    let (px, py) = unpack_pt(prev);
                    let dx = i64::from(x) - i64::from(px);
                    let dy = i64::from(y) - i64::from(py);
                    // 曼哈顿距离即可（速率量级判定，`emotion.json.busyness` 阈值为千像素级）；
                    // 无开方 → 回调内保持整数运算与 O(1)。
                    let step = dx.unsigned_abs().saturating_add(dy.unsigned_abs());
                    let _ = self.move_px.fetch_add(step, Ordering::Relaxed);
                }
            }
            HookEvent::Down { .. } => {
                let _ = self.clicks.fetch_add(1, Ordering::Relaxed);
            }
            HookEvent::Up { .. } | HookEvent::Wheel { .. } => {}
        }
        match self.tx.try_send(ev) {
            Ok(()) => {}
            // 队列满 → 丢 + 计数（不阻塞、不分配）。
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            // 消费端已退出 → 静默（不 panic）。
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试（有界队列满丢计数 / 排空 / 断开静默）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dp_platform::win::hook::MouseButton;

    fn ev(i: i32) -> HookEvent {
        HookEvent::Move { x: i, y: i }
    }

    #[test]
    fn delivers_when_capacity_available() {
        let sink = ChannelSink::new(4);
        for i in 0..4 {
            sink.on_event(ev(i));
        }
        assert_eq!(sink.dropped(), 0, "容量内不应丢弃");
        assert_eq!(sink.drain(), 4, "应排空 4 个");
        assert_eq!(sink.try_recv(), None, "排空后为空");
    }

    #[test]
    fn counts_drops_when_full_and_never_blocks() {
        let sink = ChannelSink::new(2);
        for i in 0..10 {
            sink.on_event(ev(i)); // 满后全部丢弃、不阻塞、不 panic
        }
        assert_eq!(sink.dropped(), 8, "容量 2、投 10 → 丢 8");
        assert_eq!(sink.drain(), 2, "仅保留容量上限 2 个");
    }

    #[test]
    fn capacity_zero_is_clamped_to_one() {
        let sink = ChannelSink::new(0);
        sink.on_event(ev(1));
        sink.on_event(ev(2)); // 满 → 丢
        assert_eq!(sink.dropped(), 1);
        assert_eq!(sink.drain(), 1);
    }

    #[test]
    fn disconnected_is_silent_and_not_counted() {
        let sink = ChannelSink::new(4);
        // 取走并丢弃与生产端（tx）配对的接收端 → 后续 try_send 走 `Disconnected` 分支。
        // 顶部用一个新的「已断开」接收端顶替私有字段（测试模块内可访问 `sink.rx`）。
        let original = {
            let mut guard = sink.rx.lock().unwrap();
            let (dead_tx, dead_rx) = sync_channel::<HookEvent>(1);
            drop(dead_tx);
            std::mem::replace(&mut *guard, dead_rx)
        };
        drop(original);
        sink.on_event(ev(1)); // 消费端已退出 → 静默，不 panic、不计入 dropped
        assert_eq!(sink.dropped(), 0, "Disconnected 不应计入丢弃");
    }

    // -- S7-M1：输入强度累加 ------------------------------------------------

    #[test]
    fn counts_clicks_and_move_distance() {
        let sink = ChannelSink::new(64);
        sink.on_event(HookEvent::Move { x: 0, y: 0 });
        assert_eq!(sink.move_px(), 0, "首个移动样本只建基线（无位移可言）");
        sink.on_event(HookEvent::Move { x: 3, y: 4 });
        assert_eq!(sink.move_px(), 7, "曼哈顿距离 3+4");
        sink.on_event(HookEvent::Move { x: 3, y: 0 });
        assert_eq!(sink.move_px(), 11, "回退 4 像素同样计入（7 + 4）");
        sink.on_event(HookEvent::Down { button: MouseButton::Left, x: 3, y: 0 });
        sink.on_event(HookEvent::Down { button: MouseButton::Right, x: 3, y: 0 });
        sink.on_event(HookEvent::Up { button: MouseButton::Left, x: 3, y: 0 });
        sink.on_event(HookEvent::Wheel { delta: 120, x: 3, y: 0 });
        assert_eq!(sink.clicks(), 2, "只计「按下」，不计抬起 / 滚轮");
    }

    #[test]
    fn move_distance_is_overflow_safe_and_never_panics() {
        let sink = ChannelSink::new(64);
        sink.on_event(HookEvent::Move { x: i32::MIN, y: i32::MIN });
        sink.on_event(HookEvent::Move { x: i32::MAX, y: i32::MAX });
        // 极端坐标差以饱和加法累计（不得 panic / 回绕为负）。
        assert!(sink.move_px() > 0);
    }

    #[test]
    fn intensity_counters_do_not_affect_queue_semantics() {
        let sink = ChannelSink::new(2);
        for i in 0..10 {
            sink.on_event(HookEvent::Down { button: MouseButton::Left, x: i, y: i });
        }
        assert_eq!(sink.clicks(), 10, "强度计数与队列丢弃无关（全量累加）");
        assert_eq!(sink.dropped(), 8, "队列仍按容量丢弃");
        assert_eq!(sink.drain(), 2);
    }

    #[test]
    fn pack_unpack_round_trip_handles_negative_coordinates() {
        for (x, y) in [(0, 0), (-1, -1), (i32::MIN, i32::MAX), (1920, -1080)] {
            assert_eq!(unpack_pt(pack_pt(x, y)), (x, y), "打包必须无损");
        }
    }

    #[test]
    fn hook_event_button_variants_are_forwarded_verbatim() {
        let sink = ChannelSink::new(4);
        sink.on_event(HookEvent::Down { button: MouseButton::X2, x: 7, y: 8 });
        match sink.try_recv() {
            Some(HookEvent::Down { button, x, y }) => {
                assert_eq!((button, x, y), (MouseButton::X2, 7, 8));
            }
            other => panic!("应投递 Down{{X2}}，实得 {other:?}"),
        }
    }
}
