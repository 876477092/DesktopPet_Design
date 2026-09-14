//! `dp-app/src/interaction_consumer.rs` —— S3-M3 手势消费端（钩子事件 → 手势意图）。
//!
//! 职责（**仅消费编排，零手势算法**——状态机与路由全在 `dp-core::interaction`）：
//!   1. [`to_input`]：`HookEvent → InputEvent` 纯映射（左键 = 手势键；右/中/侧键不
//!      入手势机，右键另落计数——裁定②「右键吞掉仅计数」；S3-M6 起右键 Down/Up
//!      另做**单击配对**（[`MenuState`]），命中点经 [`InteractionConsumer::take_menu_clicks`]
//!      交 core-loop 发 `pet://menu`）；
//!   2. [`InteractionConsumer`]：20Hz logic 档 drain 钩子队列 → 手势状态机
//!      `feed` → 意图日志 + 分类计数（C8：意图只落 `eprintln` + `AtomicU64`，
//!      零新增 `pet://` 事件）；批尾 `tick` 驱动悬停 / 双击窗 / 长按等超时类迁移。
//!      S3-M4 起 [`InteractionConsumer::drain_collect`] 额外**收集**本轮意图供
//!      coreloop 接线（DragStart/Throw → 拖拽相/甩出物理；本模块自身仍零仲裁）。
//!
//! ## 批内时戳口径（设计 §5 裁定6）
//! 一批 drain 内多个事件的时戳以批首 `now_ms` 起 **+1ms 递增**近似（单调可分、
//! 不回退），保证 8px 分流 / 速度阈值 / 双击窗等时差判据在批内语义成立；
//! 批尾 `tick` 以累计后的批尾时戳结算（≥ 全部事件时戳，超时类迁移不提前）。
//!
//! ## 线程与锁
//! 本类型由 `coreloop::spawn` 在 `dp-core-loop` 线程内构造与驱动（单线程 Actor）；
//! `GestureMachine` 以 `Mutex` 收口满足 `Sync`（实锁仅本线程获取，临界区微秒级）。
//! 钩子回调线程只向 [`ChannelSink`] 投递（`try_send` 非阻塞），与本类型零共享。
//!
//! ## 红线
//! C3 本文件零时钟（`now_ms` 全由 core-loop 注入）；C8 意图零 `pet://` 事件；
//! 手势意图**不提交仲裁器**（卡片边界「只做命中判定/手势→意图」，实播归 S3-M4 /
//! 仲裁归 S4，见 `dp_core::interaction::router` 文档）。

#![cfg(windows)]

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use dp_core::config::model::GestureCfg;
use dp_core::interaction::gesture::{GestureMachine, GestureOutput, InputEvent};
use dp_core::interaction::router::{InteractionKind, InteractionRouter, KIND_COUNT, as_index};
use dp_platform::win::hook::{HookEvent, MouseButton};

use crate::hook_sink::ChannelSink;

// ---------------------------------------------------------------------------
// 右键单击配对（S3-M6：钩子右键 Down/Up 吞掉后 → 消费端识别「右键单击命中」）
// ---------------------------------------------------------------------------

/// 右键 Down→Up 判定为「单击」的最大间隔（毫秒；含端点）。
pub const RIGHT_CLICK_MAX_INTERVAL_MS: u64 = 500;

/// 右键单击判定的最大位移（物理像素；Down→Up 间移动超过此值视为拖选，不弹菜单）。
pub const RIGHT_CLICK_MAX_MOVE_PX: i32 = 8;

/// 右键单击命中点（屏幕物理像素；Up 时刻坐标为锚点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RightClickHit {
    /// 屏幕物理 X。
    pub x: i32,
    /// 屏幕物理 Y。
    pub y: i32,
}

/// 右键配对状态（纯逻辑；仅 core-loop 线程驱动，`Mutex` 收口 `Sync` 同手势机口径）。
#[derive(Default)]
struct MenuState {
    /// 已按下未抬起的右键（重复 Down 覆盖旧值：最后一次按下为准）。
    pending: Option<(u64, i32, i32)>,
    /// 已配对成功的单击命中（消费端 `take_menu_clicks` 取走）。
    clicks: Vec<RightClickHit>,
}

impl MenuState {
    /// 右键按下：记录时戳与位置（覆盖未抬起的旧按下——异常序保守取新）。
    fn on_down(&mut self, x: i32, y: i32, now_ms: u64) {
        self.pending = Some((now_ms, x, y));
    }

    /// 右键抬起：与未决按下配对——间隔 ≤ [`RIGHT_CLICK_MAX_INTERVAL_MS`] 且
    /// 位移 ≤ [`RIGHT_CLICK_MAX_MOVE_PX`] → 产出单击命中（Up 坐标锚点）。
    /// 无未决按下 / 判定失败 → 清除状态不产出（幂等防御）。
    fn on_up(&mut self, x: i32, y: i32, now_ms: u64) -> bool {
        let Some((down_ms, dx, dy)) = self.pending.take() else {
            return false;
        };
        let dt = now_ms.saturating_sub(down_ms);
        let moved = (x - dx).abs().max((y - dy).abs());
        if dt <= RIGHT_CLICK_MAX_INTERVAL_MS && moved <= RIGHT_CLICK_MAX_MOVE_PX {
            self.clicks.push(RightClickHit { x, y });
            return true;
        }
        false
    }

    /// 取走全部已配对单击（取后清空，幂等）。
    fn take_clicks(&mut self) -> Vec<RightClickHit> {
        std::mem::take(&mut self.clicks)
    }
}

// ---------------------------------------------------------------------------
// HookEvent → InputEvent 纯映射
// ---------------------------------------------------------------------------

/// 钩子事件 → 手势机输入（纯映射）。
///
/// - 左键 = 手势键（K-6 全部手势以左键驱动）：`Down/Up → Press/Release`；
/// - 移动 / 滚轮照映射（滚轮状态机内忽略，为 Wheel 语义完整性保留映射）；
/// - 右键（裁定②）/ 中键 / 侧键：`None`（不入手势机；右键由消费端计数）。
#[must_use]
pub fn to_input(ev: HookEvent) -> Option<InputEvent> {
    match ev {
        HookEvent::Move { x, y } => Some(InputEvent::Move { x, y }),
        HookEvent::Down { button: MouseButton::Left, x, y } => Some(InputEvent::Press { x, y }),
        HookEvent::Up { button: MouseButton::Left, x, y } => Some(InputEvent::Release { x, y }),
        HookEvent::Wheel { delta, .. } => Some(InputEvent::Wheel { delta }),
        HookEvent::Down { button: MouseButton::Right, .. }
        | HookEvent::Up { button: MouseButton::Right, .. }
        | HookEvent::Down { button: MouseButton::Middle, .. }
        | HookEvent::Up { button: MouseButton::Middle, .. }
        | HookEvent::Down { button: MouseButton::X1, .. }
        | HookEvent::Up { button: MouseButton::X1, .. }
        | HookEvent::Down { button: MouseButton::X2, .. }
        | HookEvent::Up { button: MouseButton::X2, .. } => None,
    }
}

// ---------------------------------------------------------------------------
// InteractionConsumer：队列消费 → 手势机 → 意图日志 + 计数
// ---------------------------------------------------------------------------

/// 手势消费端（core-loop logic 档每批驱动一次；见模块文档的线程与锁口径）。
pub struct InteractionConsumer {
    /// 钩子事件队列（与 `HookService` 共享；本侧为 drain 消费端）。
    sink: Arc<ChannelSink>,
    /// 手势状态机（`Mutex` 收口 `Sync`；实锁仅 core-loop 线程获取）。
    machine: Mutex<GestureMachine>,
    /// 意图 → ACT 码路由（无状态纯映射）。
    router: InteractionRouter,
    /// 分类计数槽（下标 = [`as_index`]；C8 诊断口径：意图只落日志 + 计数）。
    stats: [AtomicU64; KIND_COUNT],
    /// 右键按下累计（裁定②：钩子吞掉，本阶段计数 + S3-M6 单击配对）。
    right_down: AtomicU64,
    /// 右键抬起累计。
    right_up: AtomicU64,
    /// 右键单击配对状态（S3-M6：Down/Up 配对 → 右键单击命中 → core-loop 发
    /// `pet://menu`；同手势机 `Mutex` 收口口径，实锁仅 core-loop 线程获取）。
    menu: Mutex<MenuState>,
}

impl InteractionConsumer {
    /// 构造（队列消费端 + 手势参数快照；手势机初始 Idle）。
    #[must_use]
    pub fn new(sink: Arc<ChannelSink>, gesture: GestureCfg) -> Self {
        Self {
            sink,
            machine: Mutex::new(GestureMachine::new(&gesture)),
            router: InteractionRouter::new(),
            stats: std::array::from_fn(|_| AtomicU64::new(0)),
            right_down: AtomicU64::new(0),
            right_up: AtomicU64::new(0),
            menu: Mutex::new(MenuState::default()),
        }
    }

    /// 排空钩子队列并喂手势机（core-loop logic 档每批一次）。
    ///
    /// 返回本次喂入手势机的**手势事件数**（右/中/侧键不计入；右键另落
    /// [`InteractionConsumer::right_down_count`] / [`InteractionConsumer::right_up_count`]）。
    /// 批内时戳 +1ms 递增、批尾 `tick` 结算（见模块文档「批内时戳口径」）。
    /// S3-M4 起为 [`Self::drain_collect`] 的薄委托（仅取 fed 数）。
    pub fn drain_and_feed(&self, now_ms: u64) -> usize {
        self.drain_collect(now_ms).0
    }

    /// 排空钩子队列并喂手势机，**同时收集**本轮产出的全部意图（S3-M4 接线用）。
    ///
    /// 返回 `(喂入事件数, 意图列表)`；日志 + 分类计数照常（C8 口径不变）。
    /// 收集以**返回值**承载而非内部缓冲：单线程 Actor 口径下 `GestureMachine`
    /// 经 `Mutex` 独占访问，无需 `&mut self` / 内部 pop 缓冲——既有
    /// `drain_and_feed` 调用点（含全部测试）零改动。
    pub fn drain_collect(&self, now_ms: u64) -> (usize, Vec<GestureOutput>) {
        let mut machine = self.machine.lock().unwrap_or_else(|e| e.into_inner());
        let mut tick_ms = now_ms;
        let mut fed = 0usize;
        let mut intents = Vec::new();
        while let Some(hook_ev) = self.sink.try_recv() {
            // S3-M6：右键 Down/Up 先走单击配对（不入手势机；计数口径不变）。
            match hook_ev {
                HookEvent::Down { button: MouseButton::Right, x, y } => {
                    self.right_down.fetch_add(1, Ordering::Relaxed);
                    self.menu.lock().unwrap_or_else(|e| e.into_inner()).on_down(x, y, tick_ms);
                    continue;
                }
                HookEvent::Up { button: MouseButton::Right, x, y } => {
                    self.right_up.fetch_add(1, Ordering::Relaxed);
                    self.menu.lock().unwrap_or_else(|e| e.into_inner()).on_up(x, y, tick_ms);
                    continue;
                }
                _ => {}
            }
            let Some(input) = to_input(hook_ev) else {
                self.count_non_gesture(hook_ev);
                continue;
            };
            tick_ms = tick_ms.wrapping_add(1);
            for out in machine.feed(input, tick_ms) {
                self.emit(out);
                intents.push(out);
            }
            fed += 1;
        }
        // 批尾 tick：悬停进入 / 双击窗到期 / 长按成立等超时类迁移（≥ 全部事件时戳）。
        for out in machine.tick(tick_ms) {
            self.emit(out);
            intents.push(out);
        }
        (fed, intents)
    }

    /// 取走全部已配对的右键单击命中（S3-M6；core-loop logic 档每批调用一次，
    /// 取后清空）。返回屏幕物理坐标命中点列表（Up 时刻锚点）。
    #[must_use]
    pub fn take_menu_clicks(&self) -> Vec<RightClickHit> {
        self.menu.lock().unwrap_or_else(|e| e.into_inner()).take_clicks()
    }

    /// 某意图的累计产出次数（诊断视图；C8 口径）。
    #[must_use]
    pub fn kind_count(&self, kind: InteractionKind) -> u64 {
        self.stats[as_index(kind)].load(Ordering::Relaxed)
    }

    /// 右键按下累计（裁定②诊断视图）。
    #[must_use]
    pub fn right_down_count(&self) -> u64 {
        self.right_down.load(Ordering::Relaxed)
    }

    /// 右键抬起累计（裁定②诊断视图）。
    #[must_use]
    pub fn right_up_count(&self) -> u64 {
        self.right_up.load(Ordering::Relaxed)
    }

    /// 不入手势机的事件分类计数（右键 Down/Up；中/侧键无计数口径，静默丢弃）。
    fn count_non_gesture(&self, ev: HookEvent) {
        match ev {
            HookEvent::Down { button: MouseButton::Right, .. } => {
                self.right_down.fetch_add(1, Ordering::Relaxed);
            }
            HookEvent::Up { button: MouseButton::Right, .. } => {
                self.right_up.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// 意图出口：路由 → 计数 + 降级日志（core-loop 线程，非回调热路径；eprintln 允许）。
    fn emit(&self, out: GestureOutput) {
        let (kind, act) = self.router.route(out);
        self.stats[as_index(kind)].fetch_add(1, Ordering::Relaxed);
        if act.is_empty() {
            eprintln!("[dp-app] interaction 意图 {kind:?} at={}ms（无登记 ACT）", out.at_ms);
        } else {
            eprintln!("[dp-app] interaction 意图 {kind:?} → {act} at={}ms", out.at_ms);
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试（映射 / 单击跨批结算 / 双击 / Tickle 覆盖 / 右键计数 / 悬停批尾）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dp_platform::win::hook::HookSink;

    fn sink(cap: usize) -> Arc<ChannelSink> {
        Arc::new(ChannelSink::new(cap))
    }

    fn left_down(x: i32, y: i32) -> HookEvent {
        HookEvent::Down { button: MouseButton::Left, x, y }
    }

    fn left_up(x: i32, y: i32) -> HookEvent {
        HookEvent::Up { button: MouseButton::Left, x, y }
    }

    // -- to_input 纯映射 ---------------------------------------------------------

    #[test]
    fn to_input_maps_left_move_wheel_and_drops_other_buttons() {
        assert_eq!(to_input(HookEvent::Move { x: 1, y: 2 }), Some(InputEvent::Move { x: 1, y: 2 }));
        assert_eq!(
            to_input(left_down(3, 4)),
            Some(InputEvent::Press { x: 3, y: 4 }),
            "左键 Down = 手势 Press"
        );
        assert_eq!(
            to_input(left_up(3, 4)),
            Some(InputEvent::Release { x: 3, y: 4 }),
            "左键 Up = 手势 Release"
        );
        assert_eq!(
            to_input(HookEvent::Wheel { delta: 120, x: 0, y: 0 }),
            Some(InputEvent::Wheel { delta: 120 })
        );
        // 右键（裁定②）/ 中键 / 侧键不入手势机。
        let others = [
            (MouseButton::Right, true),
            (MouseButton::Right, false),
            (MouseButton::Middle, true),
            (MouseButton::Middle, false),
            (MouseButton::X1, true),
            (MouseButton::X1, false),
            (MouseButton::X2, true),
            (MouseButton::X2, false),
        ];
        for (button, down) in others {
            let ev = if down {
                HookEvent::Down { button, x: 9, y: 9 }
            } else {
                HookEvent::Up { button, x: 9, y: 9 }
            };
            let phase = if down { "Down" } else { "Up" };
            assert_eq!(to_input(ev), None, "{button:?} {phase} 不入手势机");
        }
    }

    // -- 单击：跨批窗到期结算（批内 +1ms / 批尾 tick） ----------------------------

    #[test]
    fn drain_feeds_click_after_window_settles_in_next_batch() {
        let s = sink(64);
        // 缩双击窗便于批间推进（仅测试口径；结构体更新语法满足 field_reassign lint）。
        let cfg = GestureCfg { double_click_window_ms: 100, ..GestureCfg::default() };
        let consumer = InteractionConsumer::new(Arc::clone(&s), cfg);

        s.on_event(HookEvent::Move { x: 5, y: 5 });
        s.on_event(left_down(5, 5));
        s.on_event(left_up(5, 5));
        assert_eq!(consumer.drain_and_feed(1_000), 3, "三事件入批");
        assert_eq!(consumer.kind_count(InteractionKind::Click), 0, "双击窗内不结算");

        // 下一批：批尾 tick（now=1200 > 首击按下 1002 + 100ms）结算 Click。
        assert_eq!(consumer.drain_and_feed(1_200), 0, "空批仅批尾 tick");
        assert_eq!(consumer.kind_count(InteractionKind::Click), 1);
    }

    // -- 双击（窗内第二击消费 Click） ---------------------------------------------

    #[test]
    fn drain_routes_double_click_within_window() {
        let s = sink(64);
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        s.on_event(left_down(0, 0));
        s.on_event(left_up(0, 0));
        s.on_event(left_down(1, 0));
        assert_eq!(consumer.drain_and_feed(1_000), 3);
        assert_eq!(consumer.kind_count(InteractionKind::DoubleClick), 1);
        assert_eq!(consumer.kind_count(InteractionKind::Click), 0, "双击消费同次 Click");
    }

    // -- Tickle（2s 内 ≥5 击覆盖未决 Click） ---------------------------------------

    #[test]
    fn drain_routes_tickle_overriding_pending_click() {
        let s = sink(128);
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        for _ in 0..5 {
            s.on_event(left_down(0, 0));
            s.on_event(left_up(0, 0));
        }
        assert_eq!(consumer.drain_and_feed(5_000), 10);
        assert_eq!(consumer.kind_count(InteractionKind::Tickle), 1, "第 5 击即时触发");
        // K-6 互斥语义：300ms 双击窗先裁前两对击（击1/2、击3/4 → DoubleClick 消费
        // 同次 Click）；Tickle 滚动窗含全部 5 击，第 5 击覆盖击 4 的未决 Click。
        assert_eq!(consumer.kind_count(InteractionKind::DoubleClick), 2);
        assert_eq!(consumer.kind_count(InteractionKind::Click), 0, "覆盖未决 Click");
    }

    // -- 右键：仅计数，绝不产意图（裁定②） ----------------------------------------

    #[test]
    fn right_button_counts_and_never_feeds_machine() {
        let s = sink(64);
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        s.on_event(HookEvent::Down { button: MouseButton::Right, x: 3, y: 3 });
        s.on_event(HookEvent::Up { button: MouseButton::Right, x: 3, y: 3 });
        s.on_event(HookEvent::Down { button: MouseButton::Middle, x: 3, y: 3 });
        s.on_event(HookEvent::Down { button: MouseButton::X1, x: 3, y: 3 });
        assert_eq!(consumer.drain_and_feed(0), 0, "右/中/侧键不入手势机");
        assert_eq!(consumer.right_down_count(), 1, "右键 Down 计数");
        assert_eq!(consumer.right_up_count(), 1, "右键 Up 计数");
        for kind in [
            InteractionKind::Click,
            InteractionKind::DoubleClick,
            InteractionKind::Hover,
            InteractionKind::Stroke,
        ] {
            assert_eq!(consumer.kind_count(kind), 0, "{kind:?} 不得因右/中/侧键产出");
        }
    }

    // -- 悬停：批尾 tick 结算（600ms 进入） -----------------------------------------

    #[test]
    fn hover_fires_via_batch_tail_tick_across_batches() {
        let s = sink(64);
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        s.on_event(HookEvent::Move { x: 7, y: 7 });
        assert_eq!(consumer.drain_and_feed(100), 1);
        assert_eq!(consumer.kind_count(InteractionKind::Hover), 0, "未满 600ms");

        // 光标持续在场：空批推进批尾 tick（Move 批内时戳 = 101 → 701 - 101 = 600 ≥ 600ms）。
        assert_eq!(consumer.drain_and_feed(701), 0);
        assert_eq!(consumer.kind_count(InteractionKind::Hover), 1, "批尾 tick 结算悬停");
        // 停留不重复产出（Hovering 态 tick 不再发 Hover；EarTwitch 之前 2000ms 未满）。
        assert_eq!(consumer.drain_and_feed(1_000), 0);
        assert_eq!(consumer.kind_count(InteractionKind::Hover), 1);
    }

    // ---- QA 探针（清单⑥：消费端语义 —— 队列满丢弃不破坏消费 / 批内时戳双击边界 / 右键交织）----

    #[test]
    fn qa_probe_sink_full_drops_do_not_break_consumer() {
        let s = sink(2);
        // 投 5 事件：前 2（左键按下/抬起对）占满，后 3 个 Move 溢出丢弃（最新丢弃口径）。
        s.on_event(left_down(0, 0));
        s.on_event(left_up(0, 0));
        s.on_event(HookEvent::Move { x: 1, y: 0 });
        s.on_event(HookEvent::Move { x: 2, y: 0 });
        s.on_event(HookEvent::Move { x: 3, y: 0 });
        assert_eq!(s.dropped(), 3, "容量 2 投 5 → 丢 3（队列满丢弃+计数，清单⑥）");
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        // 存活事件照常喂机（丢弃不影响消费端状态机），fed = 容量上限 2。
        assert_eq!(consumer.drain_and_feed(1_000), 2, "槽内存活事件入批");
        assert_eq!(s.dropped(), 3, "丢弃计数与消费计数独立口径");
        // 腾空后可继续投递且不新增丢弃。
        s.on_event(HookEvent::Move { x: 9, y: 9 });
        assert_eq!(s.dropped(), 3, "有空位后投递成功不丢");
        assert_eq!(consumer.drain_and_feed(1_100), 1, "后续事件继续正常消费");
    }

    #[test]
    fn qa_probe_batch_timestamps_double_click_window_boundary_300_vs_301() {
        // 批内时戳口径（裁定6）：批首 now+1 起每事件 +1ms。
        // 恰 300ms：press2 - press1 = 300（按下侧 <= 含端点）→ DoubleClick。
        let s = sink(16);
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        s.on_event(left_down(0, 0));
        s.on_event(left_up(0, 0));
        assert_eq!(consumer.drain_and_feed(1_000), 2, "第一击：按下=1001");
        s.on_event(left_down(0, 0));
        s.on_event(left_up(0, 0));
        assert_eq!(consumer.drain_and_feed(1_300), 2, "第二击按下 = 1301 = 1001+300");
        assert_eq!(consumer.kind_count(InteractionKind::DoubleClick), 1, "恰 300ms 判双击");
        assert_eq!(consumer.kind_count(InteractionKind::Click), 0);

        // 301ms：press2 - press1 = 301 > 300 → 前击结算 Click，不判双击。
        let s2 = sink(16);
        let consumer2 = InteractionConsumer::new(Arc::clone(&s2), GestureCfg::default());
        s2.on_event(left_down(0, 0));
        s2.on_event(left_up(0, 0));
        assert_eq!(consumer2.drain_and_feed(1_000), 2);
        s2.on_event(left_down(0, 0));
        s2.on_event(left_up(0, 0));
        assert_eq!(consumer2.drain_and_feed(1_301), 2, "第二击按下 = 1302 = 1001+301");
        assert_eq!(consumer2.kind_count(InteractionKind::DoubleClick), 0, "出窗不判双击");
        // 设计口径（K-6/模块文档）：Click 仅经「窗到期」的 tick 侧结算；超窗第二击
        // 视为新按压轮（gesture.rs on_press 注释「超窗的第二击 = 新一轮按压」）——
        // 常规 20Hz tick 下前击早已由批尾 tick 结算，此分支仅 tick 饥饿时可达。
        assert_eq!(consumer2.kind_count(InteractionKind::Click), 0, "前击不经 press 侧结算");
        // 第二击的 Click 经批尾 tick 结算（tick 侧 >= 含端点：1302+300=1602 恰好）。
        assert_eq!(consumer2.drain_and_feed(1_602), 0);
        assert_eq!(consumer2.kind_count(InteractionKind::Click), 1, "第二击经 tick 恰 300ms 结算");
    }

    #[test]
    fn qa_probe_right_click_interleaves_without_disturbing_left_click() {
        let s = sink(16);
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        s.on_event(HookEvent::Down { button: MouseButton::Right, x: 0, y: 0 });
        s.on_event(left_down(0, 0));
        s.on_event(HookEvent::Up { button: MouseButton::Right, x: 0, y: 0 });
        s.on_event(left_up(0, 0));
        s.on_event(HookEvent::Move { x: 1, y: 0 });
        // 右键 Down/Up 不入批：fed = 3（左 down/up + move），右键不占 +1ms 时戳槽。
        assert_eq!(consumer.drain_and_feed(1_000), 3);
        assert_eq!(consumer.right_down_count(), 1, "右键 Down 计数");
        assert_eq!(consumer.right_up_count(), 1, "右键 Up 计数");
        assert_eq!(consumer.kind_count(InteractionKind::Click), 0, "批内双击窗未到期");
        // 空批推进批尾 tick：左键单击正常结算（右键交织零干扰，裁定②）。
        assert_eq!(consumer.drain_and_feed(1_400), 0);
        assert_eq!(consumer.kind_count(InteractionKind::Click), 1);
        assert_eq!(consumer.kind_count(InteractionKind::DoubleClick), 0);
    }

    // ---- S3-M4：drain_collect 意图收集（coreloop 接线通道） ----------------------

    #[test]
    fn drain_collect_returns_intents_and_keeps_counts() {
        let s = sink(64);
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        // 快速拖拽 + 甩出：Press → 快速 Move（DragStart）→ 再 Move 猛甩 → 抬起（Throw）。
        s.on_event(left_down(0, 0));
        s.on_event(HookEvent::Move { x: 25, y: 0 });
        s.on_event(HookEvent::Move { x: 225, y: 0 });
        s.on_event(left_up(225, 0));
        let (fed, intents) = consumer.drain_collect(1_000);
        assert_eq!(fed, 4, "四事件入批");
        let ks: Vec<InteractionKind> = intents.iter().map(|o| o.kind).collect();
        assert_eq!(
            ks,
            vec![InteractionKind::DragStart, InteractionKind::Throw],
            "收集序 = 产出序（DragStart 即时 → Throw 松手结算）"
        );
        // 日志 + 计数照常（C8 口径不变）。
        assert_eq!(consumer.kind_count(InteractionKind::DragStart), 1);
        assert_eq!(consumer.kind_count(InteractionKind::Throw), 1);
        // Throw 意图携带非零速度矢量（物理像素/秒；批内 +1ms 时戳 → 200px/1ms）。
        let throw = intents.iter().find(|o| o.kind == InteractionKind::Throw).expect("有 Throw");
        assert!(
            throw.vel_px_per_sec.0 > 1_200,
            "甩出矢量模 > 阈值：{:?}",
            throw.vel_px_per_sec
        );
        // drain_and_feed 委托口径不变（仅 fed 数，空批为 0）。
        assert_eq!(consumer.drain_and_feed(1_100), 0);
    }

    // ---- S3-M4 QA 探针：drain_collect 空队列边界 ---------------------------------

    /// 空队列 drain_collect：返回 (0, 空) 且不 panic；连续空排空幂等（批尾 tick
    /// 在 Idle 态零输出）；随后投递事件恢复常规消费（零残留状态污染）。
    #[test]
    fn qa_probe_drain_collect_empty_queue_returns_zero_and_no_panic() {
        let s = sink(16);
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        // 空队列直接排空（批尾 tick 仍执行：Idle 态无悬停锚点 → 零意图）。
        let (fed, intents) = consumer.drain_collect(1_000);
        assert_eq!((fed, intents.len()), (0, 0), "空队列 → (0, 空)");
        // 连续空排空幂等。
        let (fed2, intents2) = consumer.drain_collect(1_050);
        assert_eq!((fed2, intents2.len()), (0, 0), "连续空排空结果一致");
        // 对照：投递事件后恢复常规消费。
        s.on_event(HookEvent::Move { x: 1, y: 1 });
        let (fed3, intents3) = consumer.drain_collect(1_100);
        assert_eq!(fed3, 1, "事件投递后排空恢复正常计数");
        assert!(intents3.is_empty(), "单 Move 不产意图");
    }

    // ---- S3-M6：右键单击配对（Down/Up → 右键单击命中 → take_menu_clicks）--------

    fn right_down(x: i32, y: i32) -> HookEvent {
        HookEvent::Down { button: MouseButton::Right, x, y }
    }

    fn right_up(x: i32, y: i32) -> HookEvent {
        HookEvent::Up { button: MouseButton::Right, x, y }
    }

    #[test]
    fn right_click_pairing_produces_single_hit_at_up_position() {
        let s = sink(16);
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        s.on_event(right_down(1_000, 500));
        s.on_event(right_up(1_002, 498));
        assert_eq!(consumer.drain_collect(1_000).0, 0, "右键不入手势机");
        assert_eq!(consumer.right_down_count(), 1, "计数口径不变");
        assert_eq!(consumer.right_up_count(), 1);
        // 命中点 = Up 时刻坐标；取后清空（幂等）。
        assert_eq!(
            consumer.take_menu_clicks(),
            vec![RightClickHit { x: 1_002, y: 498 }]
        );
        assert!(consumer.take_menu_clicks().is_empty(), "取后清空");
    }

    #[test]
    fn right_click_window_and_move_boundaries() {
        // 恰 500ms 间隔（含端点）+ 恰 8px 位移（含端点）→ 单击成立。
        let s = sink(16);
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        s.on_event(right_down(0, 0));
        consumer.drain_collect(1_000); // 按下批：pending 时戳 = 1000
        s.on_event(right_up(8, 8));
        consumer.drain_collect(1_500); // 抬起批：dt = 500（恰边界）、位移 = 8（恰边界）
        assert_eq!(
            consumer.take_menu_clicks(),
            vec![RightClickHit { x: 8, y: 8 }],
            "边界（500ms / 8px）均含端点"
        );

        // 超窗（Down 与 Up 跨批间隔 > 500ms）→ 不命中。
        let s2 = sink(16);
        let consumer2 = InteractionConsumer::new(Arc::clone(&s2), GestureCfg::default());
        s2.on_event(right_down(0, 0));
        consumer2.drain_collect(1_000); // 按下批：pending 时戳 = 1000
        consumer2.drain_collect(1_600); // 空批（pending 保持）
        s2.on_event(right_up(0, 0));
        consumer2.drain_collect(1_600); // 抬起批：dt = 600 > 500
        assert!(consumer2.take_menu_clicks().is_empty(), "超窗不命中");

        // 位移超 8px（拖选语义）→ 不命中。
        let s3 = sink(16);
        let consumer3 = InteractionConsumer::new(Arc::clone(&s3), GestureCfg::default());
        s3.on_event(right_down(0, 0));
        s3.on_event(right_up(9, 0));
        consumer3.drain_collect(1_000);
        assert!(consumer3.take_menu_clicks().is_empty(), "位移超限不命中");
    }

    #[test]
    fn right_up_without_down_and_repeated_down_are_idempotent() {
        let s = sink(16);
        let consumer = InteractionConsumer::new(Arc::clone(&s), GestureCfg::default());
        // 无 Down 的 Up：忽略不产出。
        s.on_event(right_up(5, 5));
        consumer.drain_collect(1_000);
        assert!(consumer.take_menu_clicks().is_empty());
        // 重复 Down：最后一次按下为准（前一个 Down 被覆盖）。
        s.on_event(right_down(0, 0));
        s.on_event(right_down(3, 3));
        s.on_event(right_up(3, 3));
        consumer.drain_collect(1_100);
        assert_eq!(
            consumer.take_menu_clicks(),
            vec![RightClickHit { x: 3, y: 3 }],
            "重复按下取最后一次"
        );
        // 跨批右键交织左键：左键手势与右键配对互不干扰。
        s.on_event(right_down(0, 0));
        s.on_event(left_down(0, 0));
        s.on_event(right_up(0, 0));
        s.on_event(left_up(0, 0));
        assert_eq!(consumer.drain_collect(2_000).0, 2, "仅左键事件入批");
        assert_eq!(consumer.take_menu_clicks().len(), 1, "右键配对照常产出");
        // 左键单击经批尾 tick 结算（右键零干扰回归）。
        consumer.drain_collect(2_400);
        assert_eq!(consumer.kind_count(InteractionKind::Click), 1);
    }
}
