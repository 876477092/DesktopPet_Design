//! 明信片挂件内容与「旅行日记」汇总（S8-M3，T-21 段 · 3/4；`02 §5.13` / D-2）。
//!
//! 职责：
//!   - [`postcard_text`]：按目的地生成明信片文案（D-2：画在宠物窗口内，只驻留 1 张；
//!     未读提示进托盘，可拖可关）；
//!   - [`travel_diary`]：把离线期间到期的多张明信片合并为「旅行日记」摘要
//!     （PRD §6.13.5：回归时汇总为一张日记卡，代替逐张推送）。
//!
//! 挂件资源：图片素材 `assets/ui/postcard/*.png` 属 S8-M6 美术交付；本模块输出
//! **确定性文本**，前端据此渲染（图片缺失时文字兜底，不阻塞功能）。

use crate::model::ActivityInstance;

/// 明信片文案（确定性；`01 §6.13.5`：明信片 = 目的地照片 + 台词）。
///
/// 模板：`「{目的地}」的{时刻}，心心很想你～ 带回了{纪念品}`（无照片资源时前端
/// 用文字卡兜底）。`at_ms` 仅用于去重展示，不参与文案差异。
#[must_use]
pub fn postcard_text(inst: &ActivityInstance, at_ms: i64) -> String {
    let _ = at_ms;
    let name = if inst.def_id.is_empty() { "远方".to_string() } else { inst.def_id.clone() };
    let line = match inst.kind {
        crate::model::ActivityKind::Work => "打工中的小憩，心心也想你～".to_string(),
        crate::model::ActivityKind::Study => "学累了，给你写张卡片～".to_string(),
        crate::model::ActivityKind::Travel => {
            format!("心心在{name}很想你～ 回来给你带礼物！")
        }
    };
    line
}

/// 旅行日记摘要（回归时汇总：N 张明信片 → 1 张日记卡）。
#[must_use]
pub fn travel_diary(inst: &ActivityInstance, postcards: &[i64]) -> String {
    if postcards.is_empty() {
        return format!("「{}」旅程结束，心心安全回来了！", display_name(inst));
    }
    format!(
        "「{}」旅程集齐 {} 张明信片，都是想你的证据～ 回来给你带礼物！",
        display_name(inst),
        postcards.len()
    )
}

/// 明信片挂件尺寸（`activities.json.postcard.sizePx` 的默认镜像；前端以此为初始）。
#[must_use]
pub const fn postcard_size_px() -> (u32, u32) {
    (160, 220)
}

/// 明信片默认位置（`activities.json.postcard.position`；`bottom-right`）。
#[must_use]
pub const fn postcard_default_position() -> &'static str {
    "bottom-right"
}

/// 目的地展示名（配置名；未知 def 回退）。
fn display_name(inst: &ActivityInstance) -> &str {
    if inst.def_id.is_empty() { "远方" } else { inst.def_id.as_str() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ActivityInstance, ActivityKind};

    fn inst(kind: ActivityKind, def_id: &str) -> ActivityInstance {
        ActivityInstance {
            kind,
            def_id: def_id.to_string(),
            ..ActivityInstance::default()
        }
    }

    #[test]
    fn travel_postcard_mentions_destination() {
        let i = inst(ActivityKind::Travel, "TR-01");
        let text = postcard_text(&i, 1_000);
        assert!(text.contains("TR-01"), "明信片应含目的地：{text}");
        assert!(text.contains("想"));
    }

    #[test]
    fn diary_aggregates_multiple_postcards() {
        let i = inst(ActivityKind::Travel, "TR-01");
        let diary = travel_diary(&i, &[1_000, 2_000, 3_000]);
        assert!(diary.contains("3 张明信片"), "应汇总张数：{diary}");
    }

    #[test]
    fn diary_empty_returns_safe_return() {
        let i = inst(ActivityKind::Travel, "TR-01");
        let diary = travel_diary(&i, &[]);
        assert!(diary.contains("安全回来"));
    }

    #[test]
    fn work_postcard_is_countdown_flavor() {
        let i = inst(ActivityKind::Work, "W-01");
        let text = postcard_text(&i, 0);
        assert!(text.contains("小憩"), "打工挂件为倒计时风味文案：{text}");
    }

    #[test]
    fn defaults_match_postcard_cfg() {
        assert_eq!(postcard_size_px(), (160, 220));
        assert_eq!(postcard_default_position(), "bottom-right");
    }
}
