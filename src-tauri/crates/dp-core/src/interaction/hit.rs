//! 像素命中判定源（S3-M2 / T-08 段 · 中）。
//!
//! 依据：`02 §5 K-2`（像素级点击热区 / 双命中源抽象）、
//! `gate/arch-audit/2026-09-13-S3M2M3-实现设计.md` 裁定1（trait 落 dp-core，
//! `MaskHitSource` 为 dp-app 自有类型组合 dp-assets `HitMask`，孤儿规则合规）
//! 与裁定5（三态数据流）。
//!
//! ## 三态语义（K-2 / 裁定5）
//! - [`HitResult::Hit`]：主热区不透明像素——按钮/滚轮吞掉并投递、Move 投递；
//! - [`HitResult::Hover`]：仅次级热区（狐耳等）命中——悬停有效、点击落主判定（放行）；
//! - [`HitResult::Miss`]：窗口内透明区——放行穿透到桌面（AC-10）。
//!
//! ## 回退语义（=S3-M1 矩形判定行为）
//! [`BboxHitSource`] 过粗筛即 Hit（粗筛在句柄层完成，见 `dp-app::hit_latest`）；
//! 掩码库未就绪 / 当前帧无效 / 掩码为空（解码失败）时，`HitSource` 实现同样
//! 回退 Hit——保证「切换 HitSource 实现，事件路由行为一致」（卡片验收）。

/// 命中三态结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitResult {
    /// 未命中（窗口内透明区 → 放行穿透）。
    Miss,
    /// 仅次级热区命中（悬停有效、点击落主判定）。
    Hover,
    /// 主热区命中（不透明像素）。
    Hit,
}

/// 半开区间矩形（帧像素域；次级热区载体，与 atlas.json `secondary` 同构换算）。
///
/// 包含判定 `px ∈ [x, x+w) && py ∈ [y, y+h)`；零宽 / 零高 / 反向（w 或 h ≤ 0）
/// 恒不含（退化安全，与 `dp-platform::win::hook::rect_contains` 同式不同域——
/// 那里是屏幕物理域 bbox 粗筛，这里是帧像素域次级热区，非重复实现）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct HitRect {
    /// 左上 X。
    pub x: i32,
    /// 左上 Y。
    pub y: i32,
    /// 宽。
    pub w: i32,
    /// 高。
    pub h: i32,
}

impl HitRect {
    /// 点是否落在半开区间内（退化矩形恒 `false`，不 panic）。
    #[must_use]
    pub const fn contains(&self, px: i32, py: i32) -> bool {
        self.w > 0
            && self.h > 0
            && px >= self.x
            && px < self.x + self.w
            && py >= self.y
            && py < self.y + self.h
    }
}

/// 命中判定源（像素级；`Send + Sync` 供钩子回调线程跨线程只读）。
pub trait HitSource: Send + Sync {
    /// 判定**帧像素坐标** `(frame_x, frame_y)` 的命中三态；`mirror` 为当前帧
    /// 水平镜像标记（K-4，掩码侧按 `frameW - 1 - x` 映射查询）。
    ///
    /// # 设计注记（登记偏差）
    /// 设计草图参数记为「窗口局部物理像素」；实现定为帧像素坐标——比例映射
    /// `frame_px = local_px × frame_w / bbox_w` 需要 bbox 宽（仅 `HitLatestHandle`
    /// 粗筛层可得，对应设计 §4 流程「粗筛 → 局部换算 → 比例映射 → source.test」
    /// 的最后一步产物），故换算在句柄层完成后以帧域坐标传入。
    fn test(&self, frame_x: i32, frame_y: i32, mirror: bool) -> HitResult;
}

/// 回退实现：过粗筛即 Hit（=S3-M1 矩形判定行为）。
///
/// 仅在 bbox 粗筛通过后被调用（粗筛逻辑在 `dp-app::hit_latest::HitLatestHandle`），
/// 故 `test` 恒 [`HitResult::Hit`]；掩码库不可用时句柄层切回本语义（裁定2）。
#[derive(Debug, Clone, Copy, Default)]
pub struct BboxHitSource;

impl HitSource for BboxHitSource {
    fn test(&self, _frame_x: i32, _frame_y: i32, _mirror: bool) -> HitResult {
        HitResult::Hit
    }
}

// ---------------------------------------------------------------------------
// 单元测试（半开区间边界 / 退化矩形 / 负坐标域 / 回退语义）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_rect_contains_is_half_open() {
        let r = HitRect { x: 10, y: 20, w: 8, h: 6 };
        assert!(r.contains(10, 20), "左上闭");
        assert!(r.contains(17, 25), "右下减一闭");
        assert!(r.contains(13, 23), "内点");
        assert!(!r.contains(18, 23), "右开");
        assert!(!r.contains(13, 26), "下开");
        assert!(!r.contains(9, 23), "左外");
        assert!(!r.contains(13, 19), "上外");
    }

    #[test]
    fn hit_rect_degenerate_and_negative_domain() {
        // 零尺寸 / 反向 → 恒 false，不 panic。
        assert!(!HitRect { x: 0, y: 0, w: 0, h: 5 }.contains(0, 2), "零宽");
        assert!(!HitRect { x: 0, y: 0, w: 5, h: 0 }.contains(2, 0), "零高");
        assert!(!HitRect { x: 0, y: 0, w: -3, h: 5 }.contains(0, 2), "反向宽");
        // 多屏负坐标域（帧坐标不会为负，但判定式应通用安全）。
        let neg = HitRect { x: -32, y: -16, w: 16, h: 8 };
        assert!(neg.contains(-32, -16), "负域左上闭");
        assert!(!neg.contains(-16, -16), "负域右开");
        assert!(!neg.contains(-33, -16), "负域左外");
    }

    #[test]
    fn bbox_hit_source_is_fallback_hit_verbatim() {
        // 回退语义：过粗筛即 Hit——任意帧坐标 / 镜像组合恒 Hit（=S3-M1 行为）。
        let s = BboxHitSource;
        assert_eq!(s.test(0, 0, false), HitResult::Hit);
        assert_eq!(s.test(255, 255, false), HitResult::Hit);
        assert_eq!(s.test(-5, -5, true), HitResult::Hit);
        assert_eq!(s.test(0, 0, true), HitResult::Hit);
    }

    #[test]
    fn hit_result_is_copy_pod() {
        fn assert_copy<T: Copy + Eq + std::fmt::Debug>() {}
        assert_copy::<HitResult>();
        assert_copy::<HitRect>();
        assert_eq!(HitResult::Miss, HitResult::Miss);
        assert_ne!(HitResult::Hover, HitResult::Hit);
    }
}
