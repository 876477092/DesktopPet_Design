//! 感知事件总线（S2-M7，T-07）：perception → core 有界队列（`02 §1.4`）。
//!
//! 职责：perception 采样线程（生产者）与 core 主循环（消费者）之间的
//! **单生产者单消费者**通道，容量 [`PERCEPTION_CHANNEL_CAP`] = 64。
//! 投递策略为「最新值优先」：队列满时**丢弃最旧一条**再入队（`02 §1.4`），
//! 保证 core 侧始终优先消费最新感知状态；消费侧以 [`PerceptionBus::drain`]
//! 逐 tick 取空积压。
//!
//! 边界：通道断开（消费端已销毁）时 `offer` 静默丢弃、**永不阻塞 / panic**
//! （采样线程的健康高于任何单条事件）。

use crossbeam_channel::{Receiver, Sender, TrySendError};

/// 感知事件通道容量（`02 §1.4`：`bounded(64)`，满则丢最旧）。
pub const PERCEPTION_CHANNEL_CAP: usize = 64;

/// perception → core 单生产者单消费者总线：最新值优先，满则丢弃最旧。
///
/// 内部为 `crossbeam_channel::bounded(PERCEPTION_CHANNEL_CAP)` 的一对
/// 发送 / 接收端（同构持有：core 主循环创建后分发给采样线程使用）。
#[derive(Debug)]
pub struct PerceptionBus<E> {
    /// 发送端（perception 采样线程侧）。
    tx: Sender<E>,
    /// 接收端（core 主循环侧）。
    rx: Receiver<E>,
}

impl<E> PerceptionBus<E> {
    /// 以固定容量 [`PERCEPTION_CHANNEL_CAP`] 构造总线。
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::bounded(PERCEPTION_CHANNEL_CAP);
        Self { tx, rx }
    }

    /// 生产者侧投递：满则先丢弃最旧一条再入队（`02 §1.4` 最新值优先）；
    /// 通道已断开则静默丢弃。
    ///
    /// 重试上限 `cap + 1` 仅防御极端并发下的活锁（单生产者语义下丢一条
    /// 最旧即可腾位，实际一次重试内成功）；超限则放弃本次事件。
    pub fn offer(&self, ev: E) {
        let mut ev = ev;
        for _ in 0..=PERCEPTION_CHANNEL_CAP {
            match self.tx.try_send(ev) {
                Ok(()) => return,
                Err(TrySendError::Full(returned)) => {
                    // 满：丢弃最旧一条腾出容量后重试（try_recv 返回值即被丢弃的最旧事件）。
                    let _ = self.rx.try_recv();
                    ev = returned;
                }
                Err(TrySendError::Disconnected(_)) => {
                    // 消费端已断开：静默丢弃（不阻塞 / panic 采样线程）。
                    return;
                }
            }
        }
        // 循环上限兜底：仍满则放弃本次事件（防御路径，单生产者下不应到达）。
    }

    /// 消费者侧取空当前积压（core 主循环每 tick 调用；事件按投递顺序追加至 `out`）。
    pub fn drain(&self, out: &mut Vec<E>) {
        // Empty：积压取空；Disconnected：缓冲亦已取尽（crossbeam 先排空
        // 缓冲再报断连）。两种情况都结束本轮 drain。
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
    }

    /// 当前积压是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rx.is_empty()
    }

    /// 当前积压条数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.rx.len()
    }

    /// 测试辅助（仅测试编译）：把真实接收端移出总线（以已断连的占位接收端顶替），
    /// 使原通道可被外部 drop 进入 `Disconnected` 态，用于覆盖 `offer` 的静默丢弃路径。
    #[cfg(test)]
    fn take_receiver(&mut self) -> Receiver<E> {
        let (dead_tx, placeholder) = crossbeam_channel::bounded::<E>(1);
        drop(dead_tx);
        std::mem::replace(&mut self.rx, placeholder)
    }
}

impl<E> Default for PerceptionBus<E> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 单元测试（`02 §1.4` 容量 / 丢弃 / 顺序语义）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_keeps_fifo_order_when_not_full() {
        let bus: PerceptionBus<u32> = PerceptionBus::new();
        for i in 0..8u32 {
            bus.offer(i);
        }
        assert_eq!(bus.len(), 8);
        let mut out = Vec::new();
        bus.drain(&mut out);
        assert_eq!(out, vec![0, 1, 2, 3, 4, 5, 6, 7], "未满时严格 FIFO");
    }

    #[test]
    fn offer_overflow_drops_oldest_and_keeps_newest() {
        let bus: PerceptionBus<u32> = PerceptionBus::new();
        let total = (PERCEPTION_CHANNEL_CAP + 10) as u32;
        for i in 0..total {
            bus.offer(i);
        }
        // 容量恒为 cap：超出的 10 条中最旧的 0..10 被丢弃，最新一条保留。
        assert_eq!(bus.len(), PERCEPTION_CHANNEL_CAP);
        let mut out = Vec::new();
        bus.drain(&mut out);
        assert_eq!(out.len(), PERCEPTION_CHANNEL_CAP);
        assert_eq!(out.first(), Some(&10), "最旧的 0..10 应被丢弃");
        assert_eq!(out.last(), Some(&(total - 1)), "最新一条必须保留");
    }

    #[test]
    fn drain_clears_backlog_and_is_empty() {
        let bus: PerceptionBus<u32> = PerceptionBus::new();
        bus.offer(1);
        bus.offer(2);
        assert!(!bus.is_empty());
        let mut out = Vec::new();
        bus.drain(&mut out);
        assert_eq!(out.len(), 2);
        assert!(bus.is_empty(), "drain 后应清空");
        assert_eq!(bus.len(), 0);
        // 再次 drain 无副作用、返回空。
        let mut again = Vec::new();
        bus.drain(&mut again);
        assert!(again.is_empty());
    }

    #[test]
    fn offer_after_receiver_dropped_is_silent_without_panic() {
        let mut bus: PerceptionBus<u32> = PerceptionBus::new();
        let real_rx = bus.take_receiver();
        drop(real_rx); // 原通道所有接收端已销毁 → Disconnected
        bus.offer(42); // 静默丢弃，不得 panic
        assert!(bus.is_empty());
        assert_eq!(bus.len(), 0);
    }

    #[test]
    fn default_constructs_usable_bus() {
        let bus: PerceptionBus<u32> = PerceptionBus::default();
        assert!(bus.is_empty());
        bus.offer(9);
        assert_eq!(bus.len(), 1);
    }
}
