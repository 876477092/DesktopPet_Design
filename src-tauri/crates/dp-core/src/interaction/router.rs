//! 交互意图路由（S3-M3 / T-08 段 · 下）。
//!
//! 依据：`02 §5 K-6`（手势 → ACT 码映射表 + FR-11-12 扩展位）、
//! `gate/arch-audit/2026-09-13-S3M2M3-实现设计.md` 裁定6——`InteractionRouter`
//! 产出 [`InteractionKind`]（含 S7-M6 占位变体）+ [`act_of`] 纯映射 ACT 码
//! （**只映射不提交仲裁器**——卡片边界「只做手势→意图」，仲裁/情绪结算归 S4）。
//!
//! 本模块无状态、无时钟（C3）；消费端（dp-app）以 [`as_index`] + [`KIND_COUNT`]
//! 做 `AtomicU64` 分类计数（C8：意图只落日志与计数，零新增 `pet://` 事件）。

use crate::interaction::gesture::GestureOutput;

/// 交互意图（`02 §5 K-6` + FR-11-12；`#[non_exhaustive]` 供后续里程碑扩展）。
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InteractionKind {
    /// 悬停（光标持续在场 ≥600ms；行为侧「看向光标」归情绪域，无 ACT 码）。
    Hover,
    /// 耳朵抖动（停留累计 >2s → ACT-S-01）。
    EarTwitch,
    /// 单击（300ms 窗内无第二次按下 → ACT-T-04）。
    Click,
    /// 双击（300ms 窗内第二次按下 → ACT-T-02；消费同次 Click）。
    DoubleClick,
    /// 抚摸（长按 ≥500ms 慢移累计 → ACT-T-03）。
    Stroke,
    /// 戳痒（2s 内 ≥5 击 → ACT-T-05；覆盖 Click）。
    Tickle,
    /// 拖拽起手（位移 >8px 且快速 → ACT-T-06 挂光标；跟手实播归 S3-M4）。
    DragStart,
    /// 甩出（松手瞬时速度 >1200px/s；ACT-T-06→07→08 链首；终止 Stroke/Drag）。
    Throw,
    /// 轨迹彩蛋：画圈（FR-4-9，净转角 >270°）。
    Circle,
    /// 轨迹彩蛋：直线（FR-4-9，拟合残差 <12px）。
    Line,
    /// 轨迹彩蛋：Z 字折返（FR-4-9，≥2 次反转且夹角 >60°）。
    Zigzag,
    /// 喂食（S7-M6 托盘替代入口占位，本阶段不产出）。
    Feed,
    /// 洗澡（S7-M6 占位，本阶段不产出）。
    Bath,
    /// 派件召回（S7-M6 占位，本阶段不产出）。
    DispatchRecall,
    /// 托盘安抚（S7-M6 占位，本阶段不产出）。
    TrayCoax,
}

/// 意图 → ACT 动作码（K-6 表纯映射；**只映射不提交仲裁器**）。
///
/// 返回空串 = 该意图尚无登记动作：悬停看向光标属情绪域行为；轨迹彩蛋
/// （Circle/Line/Zigzag）与 S7-M6 占位变体的 ACT 码待归口里程碑登记。
#[must_use]
pub fn act_of(kind: InteractionKind) -> &'static str {
    match kind {
        InteractionKind::EarTwitch => "ACT-S-01",
        InteractionKind::Click => "ACT-T-04",
        InteractionKind::DoubleClick => "ACT-T-02",
        InteractionKind::Stroke => "ACT-T-03",
        InteractionKind::Tickle => "ACT-T-05",
        InteractionKind::DragStart => "ACT-T-06",
        // 甩出动作链 ACT-T-06→ACT-T-07→ACT-T-08（K-6 表）；意图只取链首，
        // 链式实播（空中翻滚 / 落地 / 抗议）归 S3-M4 物理实播。
        InteractionKind::Throw => "ACT-T-06",
        InteractionKind::Hover
        | InteractionKind::Circle
        | InteractionKind::Line
        | InteractionKind::Zigzag
        | InteractionKind::Feed
        | InteractionKind::Bath
        | InteractionKind::DispatchRecall
        | InteractionKind::TrayCoax => "",
    }
}

/// 分类计数槽总数（与 [`as_index`] 一一对应；dp-app 消费端 `[AtomicU64; KIND_COUNT]`）。
pub const KIND_COUNT: usize = 15;

/// 甩出落地动作链（K-6 甩出链 `ACT-T-06→07→08` 的落地段；空中段 ACT-T-06 由
/// [`act_of`](InteractionKind::Throw) 链首承接）。
///
/// **router 是拖拽/甩出 ACT 映射唯一真源**（S3-M4 卡片交付物「router.rs（拖拽
/// 部分）」落点）：空中翻滚 = `act_of(Throw)` = `"ACT-T-06"`（链首）；落地二连
/// 「摔倒打滚 → 抗议跺脚」由本链按序承接——dp-app 物理落地结算经此常量取码
/// （FR-4-6：落地打滚后跺脚抗议，Mood−2 结算归 S4，本卡只计数+日志）。
pub const THROW_LANDING_CHAIN: [&str; 2] = ["ACT-T-07", "ACT-T-08"];

/// 意图 → 分类计数槽下标（消费端 `AtomicU64` 分类计数用；与 [`KIND_COUNT`] 对齐）。
#[must_use]
pub const fn as_index(kind: InteractionKind) -> usize {
    match kind {
        InteractionKind::Hover => 0,
        InteractionKind::EarTwitch => 1,
        InteractionKind::Click => 2,
        InteractionKind::DoubleClick => 3,
        InteractionKind::Stroke => 4,
        InteractionKind::Tickle => 5,
        InteractionKind::DragStart => 6,
        InteractionKind::Throw => 7,
        InteractionKind::Circle => 8,
        InteractionKind::Line => 9,
        InteractionKind::Zigzag => 10,
        InteractionKind::Feed => 11,
        InteractionKind::Bath => 12,
        InteractionKind::DispatchRecall => 13,
        InteractionKind::TrayCoax => 14,
    }
}

/// 交互路由器：手势机输出 → (意图, ACT 码) 的唯一映射收口。
///
/// 无状态（K-6 映射为纯函数）；预留 S7-M6 的 Feed/Bath/DispatchRecall/TrayCoax
/// 业务来源扩展位——届时由托盘/需求域产出口径补充 `route` 之外的新入口。
#[derive(Debug, Clone, Copy, Default)]
pub struct InteractionRouter;

impl InteractionRouter {
    /// 构造（无状态）。
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// 路由一次手势输出：意图 + ACT 码（纯映射，不提交仲裁器）。
    #[must_use]
    pub fn route(&self, out: GestureOutput) -> (InteractionKind, &'static str) {
        (out.kind, act_of(out.kind))
    }
}

// ---------------------------------------------------------------------------
// 单元测试（K-6 映射表逐项 / 计数槽双射 / 路由收口）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::interaction::router::InteractionKind as Kind;

    #[test]
    fn act_of_maps_k6_table_verbatim() {
        // K-6 表逐项（悬停/轨迹/S7 占位无登记动作 → 空串）。
        assert_eq!(act_of(Kind::EarTwitch), "ACT-S-01");
        assert_eq!(act_of(Kind::Click), "ACT-T-04");
        assert_eq!(act_of(Kind::DoubleClick), "ACT-T-02");
        assert_eq!(act_of(Kind::Stroke), "ACT-T-03");
        assert_eq!(act_of(Kind::Tickle), "ACT-T-05");
        assert_eq!(act_of(Kind::DragStart), "ACT-T-06");
        assert_eq!(act_of(Kind::Throw), "ACT-T-06", "甩出取链首（07/08 链归 S3-M4）");
        assert_eq!(act_of(Kind::Hover), "", "悬停看向光标属情绪域");
        for kind in [
            Kind::Circle,
            Kind::Line,
            Kind::Zigzag,
            Kind::Feed,
            Kind::Bath,
            Kind::DispatchRecall,
            Kind::TrayCoax,
        ] {
            assert_eq!(act_of(kind), "", "{kind:?} 尚无登记动作 → 空串");
        }
    }

    #[test]
    fn as_index_is_bijective_over_kind_count() {
        let all = [
            Kind::Hover,
            Kind::EarTwitch,
            Kind::Click,
            Kind::DoubleClick,
            Kind::Stroke,
            Kind::Tickle,
            Kind::DragStart,
            Kind::Throw,
            Kind::Circle,
            Kind::Line,
            Kind::Zigzag,
            Kind::Feed,
            Kind::Bath,
            Kind::DispatchRecall,
            Kind::TrayCoax,
        ];
        assert_eq!(all.len(), KIND_COUNT, "变体数与 KIND_COUNT 一致");
        let mut seen = [false; KIND_COUNT];
        for kind in all {
            let idx = as_index(kind);
            assert!(idx < KIND_COUNT, "{kind:?} 下标越界");
            assert!(!seen[idx], "{kind:?} 下标重复：{idx}");
            seen[idx] = true;
        }
        assert!(seen.iter().all(|&s| s), "双射：全部槽位恰被覆盖一次");
    }

    #[test]
    fn router_routes_output_to_kind_and_act() {
        let router = InteractionRouter::new();
        let out = GestureOutput { kind: Kind::Click, at_ms: 1_310, vel_px_per_sec: (0, 0) };
        let (kind, act) = router.route(out);
        assert_eq!(kind, Kind::Click);
        assert_eq!(act, "ACT-T-04");
        // 纯映射不改变输入语义（互斥裁决在状态机，router 不二次决策）。
        let out = GestureOutput { kind: Kind::Tickle, at_ms: 400, vel_px_per_sec: (0, 0) };
        assert_eq!(router.route(out).0, Kind::Tickle);
    }

    #[test]
    fn interaction_kind_is_hashable_copy_for_stats() {
        fn assert_props<T: Copy + Eq + std::hash::Hash + std::fmt::Debug>() {}
        assert_props::<InteractionKind>();
    }

    #[test]
    fn throw_landing_chain_matches_actions_json_order() {
        // 链常量与 `resources/config/actions.json` 口径一致：07（摔倒打滚）
        // → 08（抗议跺脚）顺序不可倒置；两者在目录中且 07 先于 08（json 序），
        // 元数据与甩出链语义吻合（07 优先级更高且不可打断，08 收尾）。
        assert_eq!(THROW_LANDING_CHAIN, ["ACT-T-07", "ACT-T-08"], "落地二连：打滚 → 抗议");
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../");
        let catalog = crate::anim::ActionCatalog::load(&root.join("resources").join("config"))
            .expect("默认配置应可加载");
        let a07 = catalog.find(THROW_LANDING_CHAIN[0]).expect("ACT-T-07 应在目录中");
        let a08 = catalog.find(THROW_LANDING_CHAIN[1]).expect("ACT-T-08 应在目录中");
        assert!(!a07.disabled && !a08.disabled, "链上动作应启用");
        assert!(!a07.looping && !a08.looping, "落地二连为单次动作");
        assert_eq!(a07.priority, 7, "07 打滚优先级（actions.json）");
        assert!(!a07.interruptible, "07 打滚不可打断（链中段）");
        assert_eq!(a08.priority, 6, "08 抗议优先级低于 07（链尾收尾）");
        let all = catalog.all();
        let p07 = all.iter().position(|a| a.id == THROW_LANDING_CHAIN[0]).expect("07 位置");
        let p08 = all.iter().position(|a| a.id == THROW_LANDING_CHAIN[1]).expect("08 位置");
        assert!(p07 < p08, "actions.json 中 07 先于 08（链序一致）");
    }
}
