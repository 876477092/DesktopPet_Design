//! 光标热区与绕行方向（S2-M4 · `02 §5` K-3「避让光标」）。
//!
//! K-3 语义：宠物在光标 150px 热区内不落点；若当前目标在热区内或路径穿过
//! 热区，提供 tangent 方向修正（纯函数，不做完整势场）。半径由调用方从
//! `RoamCfg::cursor_avoid_radius_px`（默认 150）注入；内核按 VDC 逻辑像素
//! 口径使用该值（C6），物理像素口径的换算由调用方决定注入口径。
//!
//! 时间纪律（C3）：本模块为纯几何计算，无时钟、无状态副作用。

use super::Vec2;

/// 绕行路标外扩裕量（逻辑像素；落在热区边界外再留 8px 间隙，避免贴边抖动）。
pub const DETOUR_MARGIN_PX: f32 = 8.0;

/// 光标热区（`02 §5` K-3：`CursorHeat { pos, radiusPx }`）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorHeat {
    /// 光标位置（VDC）。
    pub pos: Vec2,
    /// 热区半径（VDC 逻辑像素；K-3 默认 150）。
    pub radius_px: f32,
}

impl CursorHeat {
    /// 构造热区。
    #[must_use]
    pub const fn new(pos: Vec2, radius_px: f32) -> Self {
        Self { pos, radius_px }
    }

    /// 防御半径（非有限 / ≤ 0 → 0，即热区退化为空集，不避让）。
    #[inline]
    fn safe_radius(&self) -> f32 {
        if self.radius_px.is_finite() && self.radius_px > 0.0 {
            self.radius_px
        } else {
            0.0
        }
    }

    /// 包含判定：欧氏距离 **严格小于** 半径（`02 §5` K-3：距离 < 150px）。
    ///
    /// 边界点（距离 == 半径）**不含**——半开区间口径，与 `RectI::contains`
    /// 的半开语义一致，避免边界点在热区边缘来回横跳。
    #[must_use]
    pub fn contains(&self, p: Vec2) -> bool {
        self.pos.distance(p) < self.safe_radius()
    }

    /// 线段 `a → b` 是否穿过热区（最近点距离严格小于半径）。
    #[must_use]
    pub fn segment_intersects(&self, a: Vec2, b: Vec2) -> bool {
        self.pos.distance(closest_point_on_segment(a, b, self.pos)) < self.safe_radius()
    }
}

/// 绕行方向（纯函数）：若 `from → to` 路径穿过热区，返回绕行偏移方向
/// （单位向量 = 路径方向的垂直向量，符号取「保持宠物在原侧」——即
/// `dot(from − heat.pos, 法向) ≥ 0` 取法向、否则取其反向）；不穿过或
/// 路径退化为零长时返回 `None`。
///
/// `from` 恰在过热区中心、垂直于路径的直线上（side == 0）时取法向正向
/// （任意一侧等价，取正向保持确定性）。
#[must_use = "绕行方向需由调用方消费（构造绕行路标）"]
pub fn detour_direction(from: Vec2, to: Vec2, heat: &CursorHeat) -> Option<Vec2> {
    let path = to - from;
    let len = path.length();
    if len < 1e-6 {
        return None;
    }
    let dir = path / len;
    let closest = closest_point_on_segment(from, to, heat.pos);
    if heat.pos.distance(closest) >= heat.safe_radius() {
        return None;
    }
    // 路径方向的垂直向量（左法向）；side 的符号决定绕行侧。
    let normal = Vec2::new(-dir.y, dir.x);
    let rel = from - heat.pos;
    let side = rel.x * normal.x + rel.y * normal.y;
    Some(if side >= 0.0 { normal } else { normal.scale(-1.0) })
}

/// 绕行路标（纯函数）：路径穿过热区时，返回「路径上距热区中心最近点 +
/// 切向 ×（半径 + [`DETOUR_MARGIN_PX`]）」的建议路标；不穿过返回 `None`。
///
/// 调用方（上层驱动循环）可据此插入中间路标实现绕行；是否采用由上层决定
/// （如需保持站立面约束可再经 `StandSurface::clamp_to_stand` 校验）。
#[must_use = "绕行路标需由调用方消费（插入路径）"]
pub fn detour_waypoint(from: Vec2, to: Vec2, heat: &CursorHeat) -> Option<Vec2> {
    let dir = detour_direction(from, to, heat)?;
    let closest = closest_point_on_segment(from, to, heat.pos);
    Some(closest + dir.scale(heat.safe_radius() + DETOUR_MARGIN_PX))
}

/// 点 `p` 在线段 `a → b` 上的最近点（参数 t 钳制到 [0, 1]）。
fn closest_point_on_segment(a: Vec2, b: Vec2, p: Vec2) -> Vec2 {
    let ab = b - a;
    let len_sq = ab.x * ab.x + ab.y * ab.y;
    if len_sq < 1e-12 {
        return a;
    }
    let t = (((p.x - a.x) * ab.x + (p.y - a.y) * ab.y) / len_sq).clamp(0.0, 1.0);
    a + ab.scale(t)
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认 150px 热区（K-3 口径）。
    fn heat(pos: Vec2) -> CursorHeat {
        CursorHeat::new(pos, 150.0)
    }

    // -- contains 边界（严格小于） -------------------------------------------

    #[test]
    fn heat_contains_boundary_is_strict() {
        let h = heat(Vec2::ZERO);
        // 距离 149.99 < 150 → 含。
        assert!(h.contains(Vec2::new(149.99, 0.0)));
        assert!(h.contains(Vec2::new(0.0, -149.99)));
        // 距离 == 半径 → 不含（半开区间口径）。
        assert!(!h.contains(Vec2::new(150.0, 0.0)));
        assert!(!h.contains(Vec2::new(0.0, 150.0)));
        // 远点不含。
        assert!(!h.contains(Vec2::new(1000.0, 0.0)));
    }

    #[test]
    fn heat_non_positive_radius_degenerates_to_empty() {
        let h = CursorHeat::new(Vec2::ZERO, 0.0);
        assert!(!h.contains(Vec2::ZERO), "半径 0 → 空集（防御）");
        let nan = CursorHeat::new(Vec2::ZERO, f32::NAN);
        assert!(!nan.contains(Vec2::ZERO), "非有限半径 → 空集（防御）");
    }

    // -- segment_intersects ---------------------------------------------------

    #[test]
    fn segment_intersects_strict_boundary() {
        let h = heat(Vec2::new(0.0, 0.0));
        // 横穿圆心 → 相交。
        assert!(h.segment_intersects(Vec2::new(-200.0, 0.0), Vec2::new(200.0, 0.0)));
        // 路径上方 300px → 不相交。
        assert!(!h.segment_intersects(Vec2::new(-200.0, 300.0), Vec2::new(200.0, 300.0)));
        // 与热区外切（最近点距离 == 半径）→ 不相交（严格口径，与 contains 一致）。
        let tangent = CursorHeat::new(Vec2::ZERO, 100.0);
        assert!(!tangent.segment_intersects(Vec2::new(-200.0, 100.0), Vec2::new(200.0, 100.0)));
        // 内切一点（距离 < 半径）→ 相交。
        assert!(tangent.segment_intersects(Vec2::new(-200.0, 99.0), Vec2::new(200.0, 99.0)));
        // 线段端点在热区内 → 相交（t 钳制到 [0,1] 生效）。
        assert!(h.segment_intersects(Vec2::new(-50.0, 0.0), Vec2::new(-1000.0, 0.0)));
    }

    // -- detour_direction -------------------------------------------------------

    #[test]
    fn detour_direction_keeps_original_side() {
        // 路径 y=50 横穿圆心在原点的 150px 热区；from 在上方（y>0）→ 绕行方向朝上。
        let h = heat(Vec2::ZERO);
        let dir = detour_direction(Vec2::new(-200.0, 50.0), Vec2::new(200.0, 50.0), &h)
            .expect("路径穿过热区应有绕行方向");
        assert!((dir.length() - 1.0).abs() < 1e-5, "绕行方向为单位向量");
        assert!(dir.y > 0.0, "保持原侧：from 在上方 → 切向朝上");
        // from 在下方 → 切向朝下（符号翻转）。
        let dir_down = detour_direction(Vec2::new(-200.0, -50.0), Vec2::new(200.0, -50.0), &h)
            .expect("路径穿过热区应有绕行方向");
        assert!(dir_down.y < 0.0, "保持原侧：from 在下方 → 切向朝下");
    }

    #[test]
    fn detour_direction_none_when_path_clear_or_degenerate() {
        let h = heat(Vec2::new(0.0, 500.0));
        // 路径不穿过热区 → None。
        assert!(detour_direction(Vec2::new(-200.0, 0.0), Vec2::new(200.0, 0.0), &h).is_none());
        // 零长路径 → None。
        assert!(detour_direction(Vec2::new(10.0, 10.0), Vec2::new(10.0, 10.0), &heat(Vec2::ZERO))
            .is_none());
    }

    // -- detour_waypoint ----------------------------------------------------------

    #[test]
    fn detour_waypoint_offsets_by_radius_plus_margin() {
        // 路径 (-200,50)→(200,50)，热区圆心 (0,0) r=150：最近点 (0,50)，
        // 切向 (0,1) → 路标 = (0, 50 + 150 + 8) = (0, 208)。
        let h = heat(Vec2::ZERO);
        let wp = detour_waypoint(Vec2::new(-200.0, 50.0), Vec2::new(200.0, 50.0), &h)
            .expect("路径穿过热区应有绕行路标");
        assert!((wp.x - 0.0).abs() < 1e-4, "wp.x={}", wp.x);
        assert!((wp.y - (50.0 + 150.0 + DETOUR_MARGIN_PX)).abs() < 1e-4, "wp.y={}", wp.y);
        // 不穿过 → None。
        assert!(
            detour_waypoint(Vec2::new(-200.0, 400.0), Vec2::new(200.0, 400.0), &h).is_none(),
            "路径不穿过热区无绕行路标"
        );
    }
}
