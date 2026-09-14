//! 图集 LRU 缓存：按**字节上限**驱逐（`02 §4.4` / `03 §4.4` 行 1711）。
//!
//! 契约：帧回退图集 LRU 常驻 ≤ **64MB** 硬上限（C10，2026-09-13 现场修复：原 48MB → 64MB）。
//! 实现基于 `lru` crate 的
//! 条目级 LRU（`LruCache::unbounded()`），插入后循环 `pop_lru` 直到
//! `total_bytes <= max_bytes`；单条超过上限的帧直接拒绝插入并 `warn`
//! （此类帧应走占位帧降级路径，`02 §7.4`）。

use std::num::NonZeroUsize;
use std::sync::Arc;

use lru::LruCache;
use tracing::warn;

/// 图集帧缓存字节硬上限：**64 × 1024 × 1024**（C10 重估：A 批 29 图集解码后实测
/// ≈53.5MB，48MB 预算按占位图文件大小估算偏低，2026-09-13 现场修复，与前端
/// AtlasCache 同口径；`02 §4.4` / `03 §4.4` 已同步回写）。
pub const ATLAS_LRU_MAX_BYTES: usize = 64 * 1024 * 1024;

/// 缓存条目：解码后 RGBA 帧缓冲 + 字节数（`bytes = w * h * 4`）。
#[derive(Debug, Clone)]
struct CachedFrame {
    /// 帧数据（RGBA8；`Arc` 支持渲染侧零拷贝共享）。
    data: Arc<[u8]>,
    /// 占用字节数。
    bytes: usize,
}

/// 图集帧 LRU 缓存（字节上限驱逐）。
#[derive(Debug)]
pub struct LruAtlasCache {
    /// 底层 LRU（容量由字节驱逐逻辑管理，条目数不设限）。
    inner: LruCache<String, CachedFrame>,
    /// 当前总字节数。
    total_bytes: usize,
    /// 字节上限。
    max_bytes: usize,
}

impl LruAtlasCache {
    /// 创建按字节上限驱逐的缓存。
    ///
    /// `max_bytes == 0` 退化为不可插入的空缓存（任何插入都被拒绝）。
    pub fn new(max_bytes: usize) -> Self {
        let capacity = NonZeroUsize::new(max_bytes).unwrap_or(NonZeroUsize::MIN);
        Self { inner: LruCache::new(capacity), total_bytes: 0, max_bytes }
    }

    /// 以契约硬上限创建（64MB，2026-09-13 现场修复：原 48MB → 64MB）。
    pub fn with_contract_limit() -> Self {
        Self::new(ATLAS_LRU_MAX_BYTES)
    }

    /// 插入一帧。
    ///
    /// 返回 `true` = 插入成功；`false` = 被拒绝（单条超上限或上限为 0）。
    /// 插入后若超限，从队首（最旧）驱逐至 `total_bytes <= max_bytes`。
    pub fn insert(&mut self, key: impl Into<String>, data: Arc<[u8]>, bytes: usize) -> bool {
        if self.max_bytes == 0 || bytes > self.max_bytes {
            warn!(
                "图集帧 {} 拒绝插入：占用 {bytes} 字节超过上限 {}",
                key.into(),
                self.max_bytes
            );
            return false;
        }

        let key = key.into();
        if let Some(old) = self.inner.peek(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(old.bytes);
        }
        self.inner.put(key, CachedFrame { data, bytes });
        self.total_bytes += bytes;

        while self.total_bytes > self.max_bytes {
            let Some((evicted, frame)) = self.inner.pop_lru() else {
                break;
            };
            self.total_bytes = self.total_bytes.saturating_sub(frame.bytes);
            warn!(
                "图集 LRU 驱逐：{evicted}（{} 字节，剩 {} 字节）",
                frame.bytes, self.total_bytes
            );
        }
        true
    }

    /// 取一帧（命中即提升至队首；`Arc` 克隆零拷贝）。
    pub fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        self.inner.get(key).map(|frame| Arc::clone(&frame.data))
    }

    /// 当前是否缓存了某键。
    pub fn contains(&self, key: &str) -> bool {
        self.inner.contains(key)
    }

    /// 当前总字节数（驱逐后始终 `<= max_bytes`）。
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// 当前条目数。
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// 字节上限。
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2048×1024 RGBA 帧 = 8MB（模拟大图集帧）。
    const FRAME_BYTES: usize = 2048 * 1024 * 4;

    fn frame(data_byte: u8) -> Arc<[u8]> {
        vec![data_byte; FRAME_BYTES].into()
    }

    /// 满负载：插入 9 张 8MB（总量 72MB > 64MB 上限）→ 驱逐最旧、total ≤ 64MB、命中正确。
    #[test]
    fn eviction_keeps_total_under_contract_limit() {
        let mut cache = LruAtlasCache::with_contract_limit();
        assert_eq!(cache.max_bytes(), ATLAS_LRU_MAX_BYTES);

        for i in 0..9u32 {
            assert!(
                cache.insert(format!("action_{i:02}"), frame(i as u8), FRAME_BYTES),
                "单条 8MB 不应被拒绝"
            );
            assert!(
                cache.total_bytes() <= ATLAS_LRU_MAX_BYTES,
                "任何时刻 total 不得超过 64MB：{}",
                cache.total_bytes()
            );
        }

        assert_eq!(cache.len(), 8, "64MB / 8MB = 8 条驻留");
        assert_eq!(cache.total_bytes(), FRAME_BYTES * 8);
        // 最旧一张被驱逐。
        assert!(!cache.contains("action_00"));
        // 最新一张仍在。
        assert!(cache.contains("action_08"));
        assert!(cache.get("action_08").is_some());
        assert!(cache.get("action_00").is_none());
    }

    /// get 命中提升至队首：中间条目被访问后，驱逐时优先淘汰真正最旧的条目。
    #[test]
    fn get_promotes_to_front() {
        let mut cache = LruAtlasCache::new(FRAME_BYTES * 2); // 恰容 2 条
        cache.insert("a", frame(0), FRAME_BYTES);
        cache.insert("b", frame(1), FRAME_BYTES);
        // 访问 a → a 变最新，b 最旧。
        assert!(cache.get("a").is_some());
        cache.insert("c", frame(2), FRAME_BYTES);
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains("b"), "b 应被驱逐（插入 c 后最旧）");
        assert!(cache.contains("a"), "a 因命中提升而保留");
        assert!(cache.contains("c"));
    }

    /// 单条超过上限 → 拒绝插入并保持缓存不变。
    #[test]
    fn oversized_frame_is_rejected() {
        let mut cache = LruAtlasCache::new(FRAME_BYTES * 2);
        cache.insert("small", frame(0), FRAME_BYTES);
        let before = (cache.len(), cache.total_bytes());

        assert!(!cache.insert("huge", frame(1), FRAME_BYTES * 3), "超上限条目应被拒绝");
        assert_eq!(cache.len(), before.0);
        assert_eq!(cache.total_bytes(), before.1);
        assert!(cache.contains("small"));
    }

    /// 重复插入同键：旧条目字节数先扣除（不重复计入 total）。
    #[test]
    fn reinsert_same_key_replaces_bytes() {
        let mut cache = LruAtlasCache::new(FRAME_BYTES * 4);
        cache.insert("k", frame(0), FRAME_BYTES);
        cache.insert("k", frame(1), FRAME_BYTES * 2);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.total_bytes(), FRAME_BYTES * 2);
    }

    /// 空缓存与零上限。
    #[test]
    fn zero_limit_rejects_everything() {
        let mut cache = LruAtlasCache::new(0);
        assert!(cache.is_empty());
        assert!(!cache.insert("x", frame(0), FRAME_BYTES));
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.total_bytes(), 0);
    }
}
