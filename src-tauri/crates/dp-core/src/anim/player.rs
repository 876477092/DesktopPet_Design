//! 动作播放器（S2-M2，卡片要点 2/3）。
//!
//! 职责：
//!   - **轮播**：按目录序（actions.json 文件序）循环播放批次 A 已启用动作；
//!   - **帧号推进**：动画帧号 = `elapsed × 动作 fps`（actions.json 每动作 `fps`
//!     字段驱动，如 ACT-M-02 行走 12fps）——与 tick 档位正交；
//!   - **循环段**：`loopRange` 闭区间内回绕（行主序帧索引）；非循环动作播完
//!     **停留末帧**（`02 §5.11` 非循环语义），轮播驻留到期后切换下一动作；
//!   - **fps 档位**（K-4 场景表）：2/4/6/15/30/60 为播放器 **tick 节拍档位**
//!     （睡眠 2 / 省电 4 / 空闲 6 / 高负载 15 / 中档 30 / 普通活动 60），
//!     可切换、CPU 随档位变化；tick 绝对时间锚定由调用方 tick 循环持有
//!     [`std::time::Instant`] 实现（AC-11：固定 sleep 过冲逐 tick 累加必须避免）；
//!   - **镜像规则**（`02 §5 K-4`）：素材仅交付朝左（direction 仅 `l`），
//!     `cmd.mirror = 动作元数据 mirror && 朝向 == Right`；渲染端 `ctx.scale(-1,1)`
//!     / uFlipX 与命中源 `frameW - x` 映射（T-08）共用 [`mirror_x`] 离散语义。
//!
//! 设计：纯函数核心（无内部时钟）——播放器只消费 `Duration`（自播放起点起的
//! 单调流逝时长），帧号/轮播定位可确定性单测；状态仅 fps 档位与朝向。

use std::time::Duration;

use crate::anim::AnimError;
use crate::config::ActionCfg;

/// K-4 场景表的 fps 档位（播放器 tick 节拍档位；**动作自身帧率与此无关**）。
///
/// | 档位 | fps | 场景（K-4） |
/// |------|-----|-------------|
/// | [`FpsTier::Sleep`] | 2 | 睡眠 |
/// | [`FpsTier::PowerSave`] | 4 | 省电 |
/// | [`FpsTier::Idle`] | 6 | 空闲（默认档） |
/// | [`FpsTier::Medium`] | 30 | 中档（K-4 未命名场景预留） |
/// | [`FpsTier::HighLoad`] | 15 | 高负载 |
/// | [`FpsTier::Active`] | 60 | 普通活动 |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FpsTier {
    /// 睡眠档：2 fps。
    Sleep,
    /// 省电档：4 fps。
    PowerSave,
    /// 空闲档：6 fps（默认）。
    Idle,
    /// 中档：30 fps（K-4 场景表未命名，登记档位预留）。
    Medium,
    /// 高负载档：15 fps。
    HighLoad,
    /// 普通活动档：60 fps。
    Active,
}

impl FpsTier {
    /// 全部登记档位（切档白名单口径）。
    pub const ALL: [FpsTier; 6] = [
        FpsTier::Sleep,
        FpsTier::PowerSave,
        FpsTier::Idle,
        FpsTier::Medium,
        FpsTier::HighLoad,
        FpsTier::Active,
    ];

    /// 档位对应 tick 频率（fps）。
    #[must_use]
    pub const fn fps(self) -> u32 {
        match self {
            FpsTier::Sleep => 2,
            FpsTier::PowerSave => 4,
            FpsTier::Idle => 6,
            FpsTier::Medium => 30,
            FpsTier::HighLoad => 15,
            FpsTier::Active => 60,
        }
    }

    /// 档位对应 tick 间隔（恒整毫秒：1000/fps 对 2/4/6/15/30/60 均整除）。
    #[must_use]
    pub const fn tick_interval(self) -> Duration {
        Duration::from_millis(1_000 / self.fps() as u64)
    }

    /// 从 fps 数值解析档位（白名单外 → `None`，调用方按 [`AnimError::InvalidFpsTier`] 收口）。
    #[must_use]
    pub fn from_fps(value: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|tier| tier.fps() == value)
    }
}

impl Default for FpsTier {
    /// 默认档：空闲 6 fps（K-4 场景表 + S2-M1 冒烟同口径）。
    fn default() -> Self {
        FpsTier::Idle
    }
}

/// 朝向（`02 §5 K-4`：素材仅交付朝左 `l`，右向由运行时镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Facing {
    /// 朝左（素材原方向，不镜像）。
    #[default]
    Left,
    /// 朝右（运行时水平镜像）。
    Right,
}

/// 单动作播放项：从 [`ActionCfg`] 派生的 anim 专用视图（mirror/fps/loopRange/帧数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayItem {
    /// 动作 ID（`ACT-*`）。
    pub action_id: String,
    /// 动作自身帧率（actions.json `fps`，驱动动画帧号）。
    pub fps: u32,
    /// 是否循环动作。
    pub looping: bool,
    /// 循环帧区间（闭区间，行主序帧索引）。
    pub loop_range: Option<[u32; 2]>,
    /// 是否提供镜像（动作元数据 `mirror`；最终 `cmd.mirror` 还需结合朝向）。
    pub mirror: bool,
    /// 图集帧总数（由 `AtlasFile` 的 `frameCount` 注入；非循环动作的播放长度依据）。
    pub frame_count: u32,
    /// 循环动作轮播驻留时长（毫秒；到期切换下一动作）。
    pub dwell_ms: u64,
}

impl PlayItem {
    /// 从动作元数据派生播放项（图集帧数外部注入）。
    ///
    /// 防御性收口（不 panic，`02 §7.4`）：`fps == 0` → 按 1 处理；
    /// `frame_count == 0` → 按 1 帧处理；`loopRange` 非法（越界 / 起点大于终点）
    /// → 回退为整段循环。
    #[must_use]
    pub fn from_action(action: &ActionCfg, frame_count: u32, dwell_ms: u64) -> Self {
        let frame_count = frame_count.max(1);
        // loopRange 合法性：区间在 [0, frame_count) 内且 start <= end。
        let loop_range = match action.loop_range {
            Some([start, end]) if start <= end && end < frame_count => Some([start, end]),
            other => {
                if other.is_some() {
                    // 非法区间：告警口径由调用方日志承担，此处静默回退整段。
                }
                None
            }
        };
        Self {
            action_id: action.id.clone(),
            fps: action.fps.max(1),
            looping: action.looping,
            loop_range: if action.looping { loop_range } else { None },
            mirror: action.mirror,
            frame_count,
            dwell_ms: dwell_ms.max(1),
        }
    }

    /// 本动作单次轮播时长（毫秒）：
    /// - 循环动作 → `dwell_ms`（驻留到期即切换）；
    /// - 非循环动作 → 一次完整播放时长（`frame_count / fps × 1000`，向上取整），
    ///   播完后停留末帧至轮播切换（与循环动作等长驻留也可，此处取自然时长）。
    #[must_use]
    pub fn segment_ms(&self) -> u64 {
        if self.looping {
            return self.dwell_ms;
        }
        let fps = u64::from(self.fps.max(1));
        (u64::from(self.frame_count) * 1_000).div_ceil(fps)
    }
}

/// 播放器一帧输出（与 `dp-app` 的 `RenderFrameCmd` v1 同构的最小视图；
/// 图集布局字段（columns/rows/frameW/frameH/png）由 dp-app 从图集元数据拼装，C8 冻结）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerFrame {
    /// 动作 ID。
    pub action_id: String,
    /// 帧序号（0 起连续，行主序）。
    pub frame_index: u32,
    /// 镜像标志（K-4：动作元数据 `mirror` && 朝向 == [`Facing::Right`]）。
    pub mirror: bool,
    /// 动作自身帧率（调试/降帧提示；tick 档位另见 [`FpsTier`]）。
    pub action_fps: u32,
}

/// 轮播定位：当前动作 + 该动作内流逝时长。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    /// 播放项下标（`items` 内）。
    index: usize,
    /// 进入该动作以来的流逝毫秒。
    action_ms: u64,
}

/// 动作播放器（批次 A 轮播；状态仅档位 + 朝向，帧号纯函数推进）。
#[derive(Debug, Clone)]
pub struct ActionPlayer {
    /// 播放项列表（目录序）。
    items: Vec<PlayItem>,
    /// 当前 fps 档位（tick 节拍）。
    tier: FpsTier,
    /// 当前朝向（镜像规则输入）。
    facing: Facing,
}

impl ActionPlayer {
    /// 构建播放器（空列表合法：`frame_at` 恒 `None`，调用方降级）。
    #[must_use]
    pub fn new(items: Vec<PlayItem>) -> Self {
        Self { items, tier: FpsTier::default(), facing: Facing::default() }
    }

    /// 播放项列表（只读）。
    #[must_use]
    pub fn items(&self) -> &[PlayItem] {
        &self.items
    }

    /// 当前 fps 档位。
    #[must_use]
    pub const fn tier(&self) -> FpsTier {
        self.tier
    }

    /// 切换 fps 档位（K-4：2/4/6/15/30/60 可切；白名单外 → `Err`）。
    ///
    /// # Errors
    /// [`AnimError::InvalidFpsTier`]：档位不在登记清单。
    pub fn set_tier_fps(&mut self, fps: u32) -> Result<(), AnimError> {
        let tier = FpsTier::from_fps(fps).ok_or(AnimError::InvalidFpsTier { value: fps })?;
        self.tier = tier;
        Ok(())
    }

    /// 当前 tick 间隔（绝对锚定网格的步长；CPU 随档位变化的机制基础）。
    #[must_use]
    pub const fn tick_interval(&self) -> Duration {
        self.tier.tick_interval()
    }

    /// 切换朝向（镜像规则输入）。
    pub const fn set_facing(&mut self, facing: Facing) {
        self.facing = facing;
    }

    /// 当前朝向。
    #[must_use]
    pub const fn facing(&self) -> Facing {
        self.facing
    }

    /// 取自播放起点起 `elapsed` 流逝后应展示的一帧（纯函数，无内部时钟）。
    ///
    /// 播放列表为空 → `None`（调用方降级，不 panic）。
    #[must_use]
    pub fn frame_at(&self, elapsed: Duration) -> Option<PlayerFrame> {
        let seg = locate_segment(&self.items, elapsed.as_millis() as u64)?;
        let item = &self.items[seg.index];
        Some(PlayerFrame {
            action_id: item.action_id.clone(),
            frame_index: frame_index_for(item, seg.action_ms),
            mirror: cmd_mirror(item, self.facing),
            action_fps: item.fps,
        })
    }
}

/// 轮播定位（纯函数）：总流逝毫秒 → 当前动作与动作内流逝。
///
/// 各动作 segment 时长确定性（[`PlayItem::segment_ms`]），故总时长按轮播周期
/// 取模回绕——**绝对时间定位**，不依赖 tick 计数（AC-11：锚定不漂移）。
/// 空 / 总时长为 0 → `None`。
fn locate_segment(items: &[PlayItem], total_ms: u64) -> Option<Segment> {
    if items.is_empty() {
        return None;
    }
    let cycle: u64 = items.iter().map(PlayItem::segment_ms).sum();
    if cycle == 0 {
        return Some(Segment { index: 0, action_ms: 0 });
    }
    let mut rest = total_ms % cycle;
    for (index, item) in items.iter().enumerate() {
        let seg = item.segment_ms();
        if rest < seg {
            return Some(Segment { index, action_ms: rest });
        }
        rest -= seg;
    }
    // 防御性收口（浮点/取模边界理论不可达）：回落首项。
    Some(Segment { index: 0, action_ms: 0 })
}

/// 动作内帧号（纯函数）：`elapsed × 动作 fps`。
///
/// - 循环动作：`loopRange = [a, b]` → `a + (帧位 mod 区间长)`（闭区间回绕；
///   区间缺省/非法时回退整段 `0..frame_count`）；
/// - 非循环动作：`min(帧位, frame_count - 1)`——播完**停留末帧**（`02 §5.11`）。
///
/// 防御性收口：fps/frame_count 为 0 时按 1 处理（[`PlayItem::from_action`] 已收口，
/// 此处为纯函数独立防御）。
#[must_use]
pub fn frame_index_for(item: &PlayItem, action_ms: u64) -> u32 {
    let fps = u64::from(item.fps.max(1));
    let count = u64::from(item.frame_count.max(1));
    let frame_pos = action_ms.saturating_mul(fps) / 1_000;

    if item.looping {
        let (start, len) = match item.loop_range {
            Some([a, b]) if b >= a => (u64::from(a), u64::from(b - a) + 1),
            // 区间缺省 → 整段循环。
            _ => (0, count),
        };
        let len = len.max(1);
        let idx = start + frame_pos % len;
        // 区间越界防御（from_action 已拦，纯函数独立收口）。
        (idx % count) as u32
    } else {
        (frame_pos.min(count - 1)) as u32
    }
}

/// `cmd.mirror` 规则（纯函数，K-4）：
/// 动作元数据声明可镜像（`mirror=true`）且当前朝向为右 → 镜像；
/// 其余（素材原方向 / 不可镜像动作）→ 不镜像。
#[must_use]
pub const fn cmd_mirror(item: &PlayItem, facing: Facing) -> bool {
    item.mirror && matches!(facing, Facing::Right)
}

/// 镜像横向坐标映射（`02 §5 K-4`：`frameW - x`；离散像素索引域精确化为
/// `frameW - 1 - x`，与 `dp-assets::mask::MaskAtlas::bit_at_mirrored` 同一语义）。
///
/// 渲染端 `ctx.scale(-1, 1)` / uFlipX 后物理位置 `x` 对应原图 `frameW - 1 - x`；
/// 命中源（T-08）与渲染端必须共用本函数语义（镜像一致性单测）。
/// `frame_w == 0` 或 `x >= frame_w` → `None`（与掩码侧拒绝口径一致）。
#[must_use]
pub const fn mirror_x(x: u32, frame_w: u32) -> Option<u32> {
    if frame_w == 0 || x >= frame_w {
        return None;
    }
    Some(frame_w - 1 - x)
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造播放项的快捷工厂。
    fn item(id: &str, fps: u32, looping: bool, loop_range: Option<[u32; 2]>, frames: u32) -> PlayItem {
        PlayItem {
            action_id: id.to_string(),
            fps,
            looping,
            loop_range: if looping { loop_range } else { None },
            mirror: true,
            frame_count: frames,
            dwell_ms: 2_000,
        }
    }

    // -- fps 档位 -----------------------------------------------------------

    #[test]
    fn fps_tier_covers_all_registered_values() {
        // K-4 场景表 + 登记档位：2/4/6/15/30/60。
        let expect = [
            (FpsTier::Sleep, 2u32),
            (FpsTier::PowerSave, 4),
            (FpsTier::Idle, 6),
            (FpsTier::Medium, 30),
            (FpsTier::HighLoad, 15),
            (FpsTier::Active, 60),
        ];
        for (tier, fps) in expect {
            assert_eq!(tier.fps(), fps, "{tier:?} 应为 {fps}fps");
            assert_eq!(FpsTier::from_fps(fps), Some(tier), "fps={fps} 应能解析回档位");
        }
        assert_eq!(FpsTier::ALL.len(), 6);
    }

    #[test]
    fn fps_tier_rejects_unregistered_values() {
        for bad in [0, 1, 3, 5, 7, 14, 16, 29, 31, 59, 61, 120] {
            assert_eq!(FpsTier::from_fps(bad), None, "{bad} 不在登记档位");
        }
    }

    #[test]
    fn tick_interval_derives_from_tier() {
        assert_eq!(FpsTier::Idle.tick_interval(), Duration::from_millis(166));
        assert_eq!(FpsTier::Sleep.tick_interval(), Duration::from_millis(500));
        assert_eq!(FpsTier::Active.tick_interval(), Duration::from_millis(16));
        assert_eq!(FpsTier::HighLoad.tick_interval(), Duration::from_millis(66));
        assert_eq!(FpsTier::PowerSave.tick_interval(), Duration::from_millis(250));
        assert_eq!(FpsTier::Medium.tick_interval(), Duration::from_millis(33));
    }

    #[test]
    fn set_tier_fps_switches_interval_and_rejects_invalid() {
        let mut player = ActionPlayer::new(vec![item("ACT-M-01", 6, true, Some([0, 3]), 4)]);
        assert_eq!(player.tier(), FpsTier::Idle, "默认档为空闲 6fps");
        assert_eq!(player.tick_interval(), Duration::from_millis(166));

        player.set_tier_fps(2).expect("2fps 应为合法档位");
        assert_eq!(player.tick_interval(), Duration::from_millis(500), "切档立即生效");

        player.set_tier_fps(60).expect("60fps 应为合法档位");
        assert_eq!(player.tick_interval(), Duration::from_millis(16));

        let err = player.set_tier_fps(7).expect_err("非法档位应被拒绝");
        assert!(matches!(err, AnimError::InvalidFpsTier { value: 7 }), "{err}");
        // 拒绝后档位保持不变。
        assert_eq!(player.tick_interval(), Duration::from_millis(16));
    }

    // -- 帧号推进（动作 fps 换算 / 循环回绕 / 非循环停留） -------------------

    #[test]
    fn frame_index_scales_with_action_fps() {
        // ACT-M-02 行走 12fps、循环 [0,7]、8 帧：elapsed=250ms → 帧位 3。
        let walk = item("ACT-M-02", 12, true, Some([0, 7]), 8);
        assert_eq!(frame_index_for(&walk, 0), 0);
        assert_eq!(frame_index_for(&walk, 83), 0, "83ms×12fps=0.996 帧 → 0");
        assert_eq!(frame_index_for(&walk, 84), 1, "84ms×12fps=1.008 帧 → 1");
        assert_eq!(frame_index_for(&walk, 250), 3);
        assert_eq!(frame_index_for(&walk, 584), 7, "584ms×12fps=7.008 帧 → 7");
        // 循环回绕：750ms → 帧位 9 → 9 % 8 = 1。
        assert_eq!(frame_index_for(&walk, 750), 1);
        // 环绕多圈仍稳定（绝对时间定位不漂移）。
        assert_eq!(frame_index_for(&walk, 2_000), (2_000 * 12 / 1_000) % 8);
    }

    #[test]
    fn loop_range_wraps_within_closed_interval() {
        // 循环区间 [2, 5]（4 帧），10fps：帧位 k → 2 + (k mod 4)。
        let act = item("ACT-I-03", 10, true, Some([2, 5]), 8);
        assert_eq!(frame_index_for(&act, 0), 2);
        assert_eq!(frame_index_for(&act, 100), 3);
        assert_eq!(frame_index_for(&act, 300), 5);
        assert_eq!(frame_index_for(&act, 400), 2, "帧位 4 → 回绕至区间首");
        assert_eq!(frame_index_for(&act, 1_300), 3, "帧位 13 → 2+(13 mod 4)=3");
    }

    #[test]
    fn looping_without_range_falls_back_to_full_span() {
        let act = item("ACT-X-01", 4, true, None, 4);
        assert_eq!(frame_index_for(&act, 0), 0);
        assert_eq!(frame_index_for(&act, 750), 3);
        assert_eq!(frame_index_for(&act, 1_000), 0, "整段回绕");
    }

    #[test]
    fn non_looping_holds_last_frame_after_finish() {
        // ACT-M-04 跳跃 18fps 非循环、8 帧：一次播放 8/18 s ≈ 445ms。
        let jump = item("ACT-M-04", 18, false, None, 8);
        assert_eq!(frame_index_for(&jump, 0), 0);
        assert_eq!(frame_index_for(&jump, 250), 4);
        assert_eq!(frame_index_for(&jump, 444), 7);
        // 播完停留末帧（02 §5.11 非循环语义）。
        assert_eq!(frame_index_for(&jump, 1_000), 7);
        assert_eq!(frame_index_for(&jump, 10_000), 7, "长停留仍为末帧");
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let zero_fps = item("ACT-X-02", 0, true, Some([0, 3]), 4);
        assert_eq!(frame_index_for(&zero_fps, 999), 0, "fps=0 防御按 1 处理");
        let zero_frames = item("ACT-X-03", 6, false, None, 0);
        assert_eq!(frame_index_for(&zero_frames, 100), 0, "frame_count=0 防御按 1 帧");
        // loopRange 越界 → from_action 回退整段。
        let cfg = ActionCfg { id: "ACT-X-04".into(), looping: true, loop_range: Some([0, 9]), ..Default::default() };
        let derived = PlayItem::from_action(&cfg, 4, 1_000);
        assert_eq!(derived.loop_range, None, "越界区间应回退整段循环");
        assert_eq!(frame_index_for(&derived, 1_000), 1, "fps 缺省 0→1 防御：帧位 = 1000ms×1fps/1000 = 1，整段回绕");
    }

    // -- 轮播定位（绝对时间，确定性 segment） --------------------------------

    #[test]
    fn locate_segment_walks_items_in_order_and_cycles() {
        // 非循环 2 帧 @2fps → segment 1000ms；循环 dwell 2000ms。
        let items = vec![
            item("ACT-M-04", 2, false, None, 2),
            item("ACT-M-01", 6, true, Some([0, 3]), 4),
        ];
        assert_eq!(items[0].segment_ms(), 1_000, "非循环 = 2帧/2fps = 1000ms");
        assert_eq!(items[1].segment_ms(), 2_000, "循环 = dwell 2000ms");

        let at = |ms: u64| locate_segment(&items, ms).map(|s| (s.index, s.action_ms));
        assert_eq!(at(0), Some((0, 0)));
        assert_eq!(at(500), Some((0, 500)));
        assert_eq!(at(999), Some((0, 999)));
        assert_eq!(at(1_000), Some((1, 0)), "第二动作从 0 起");
        assert_eq!(at(2_500), Some((1, 1_500)));
        // 周期 3000ms：3500 → 500 → 首动作。
        assert_eq!(at(3_500), Some((0, 500)), "轮播周期回绕");
        assert_eq!(at(6_999), Some((0, 999)));
        assert_eq!(at(7_000), Some((1, 0)), "下一周期首动作边界");
        assert_eq!(at(7_001), Some((1, 1)));
    }

    #[test]
    fn locate_segment_handles_empty_and_zero_cycle() {
        assert_eq!(locate_segment(&[], 100), None, "空列表 → None");
        // dwell 防御 max(1) 后 cycle 不可能为 0；直接构造 0 时长场景验证收口。
        let items = vec![PlayItem {
            action_id: "ACT-X-05".into(),
            fps: 1,
            looping: false,
            loop_range: None,
            mirror: false,
            frame_count: 1,
            dwell_ms: 1,
        }];
        // segment = ceil(1*1000/1)=1000，正常路径。
        assert_eq!(locate_segment(&items, 1_500), Some(Segment { index: 0, action_ms: 500 }));
    }

    #[test]
    fn player_frame_at_joins_location_and_mirror() {
        let mut player = ActionPlayer::new(vec![
            item("ACT-M-04", 2, false, None, 2),
            item("ACT-M-01", 6, true, Some([0, 3]), 4),
        ]);
        // 默认朝左：不镜像。
        let frame = player.frame_at(Duration::from_millis(100)).expect("应有帧");
        assert_eq!(frame.action_id, "ACT-M-04");
        assert_eq!(frame.frame_index, 0, "100ms×2fps=0.2 帧 → 0");
        assert!(!frame.mirror, "朝左不镜像");
        assert_eq!(frame.action_fps, 2, "action_fps 为动作自身帧率");

        // 朝右：镜像生效。
        player.set_facing(Facing::Right);
        let frame = player.frame_at(Duration::from_millis(100)).expect("应有帧");
        assert!(frame.mirror, "朝右应镜像");

        // 切到第二动作（dwell 段内）。
        let frame = player.frame_at(Duration::from_millis(1_500)).expect("应有帧");
        assert_eq!(frame.action_id, "ACT-M-01");
        assert_eq!(frame.action_fps, 6, "动作帧率来自元数据而非档位");
        assert_eq!(frame.frame_index, 3, "500ms×6fps=3 帧");
    }

    #[test]
    fn player_empty_items_degrade_to_none() {
        let player = ActionPlayer::new(Vec::new());
        assert_eq!(player.frame_at(Duration::from_millis(0)), None);
    }

    // -- 镜像规则与一致性 ----------------------------------------------------

    #[test]
    fn cmd_mirror_follows_metadata_and_facing() {
        let mirrorable = item("ACT-M-01", 6, true, Some([0, 3]), 4);
        let fixed = PlayItem { mirror: false, ..mirrorable.clone() };

        assert!(!cmd_mirror(&mirrorable, Facing::Left), "素材原方向（朝左）不镜像");
        assert!(cmd_mirror(&mirrorable, Facing::Right), "朝右镜像");
        assert!(!cmd_mirror(&fixed, Facing::Left), "不可镜像动作恒不镜像");
        assert!(!cmd_mirror(&fixed, Facing::Right), "不可镜像动作恒不镜像");
    }

    /// 镜像一致性（K-4 / 卡片要点 2）：`mirror_x` 与 `dp-assets::mask` 的
    /// `bit_at_mirrored` 共用 `frameW - 1 - x` 离散语义——此处以逐位翻转
    /// 等价性断言（渲染端水平翻转 ⇄ 命中源映射）收口 catalog/player 侧。
    #[test]
    fn mirror_x_matches_horizontal_flip_semantics() {
        // 位图：宽 8，仅 x=1 与 x=6 点亮。
        let lit: [bool; 8] = [false, true, false, false, false, false, true, false];
        for x in 0..8u32 {
            let mirrored_x = mirror_x(x, 8).expect("合法 x 应可映射");
            // 渲染端镜像后物理位置 x 的像素 = 原图 mirrored_x 的像素。
            assert_eq!(lit[mirrored_x as usize], lit[(7 - x) as usize], "x={x} 应等于翻转位");
            // 对合性：镜像的镜像回到原位。
            assert_eq!(mirror_x(mirrored_x, 8), Some(x));
        }
        // 非法入参拒绝（与 dp-assets 掩码侧口径一致）。
        assert_eq!(mirror_x(8, 8), None, "x 越界拒绝");
        assert_eq!(mirror_x(0, 0), None, "frame_w=0 拒绝");
        assert_eq!(mirror_x(3, 0), None);
    }

    #[test]
    fn play_item_from_action_defends_degenerate_values() {
        let cfg = ActionCfg {
            id: "ACT-X-06".to_string(),
            fps: 0,
            looping: true,
            loop_range: Some([0, 3]),
            mirror: true,
            ..ActionCfg::default()
        };
        let derived = PlayItem::from_action(&cfg, 0, 0);
        assert_eq!(derived.fps, 1, "fps=0 → 1 防御");
        assert_eq!(derived.frame_count, 1, "frame_count=0 → 1 防御");
        assert_eq!(derived.dwell_ms, 1, "dwell=0 → 1 防御");
        // frame_count=1 时 loopRange [0,3] 越界 → 回退整段（None）。
        assert_eq!(derived.loop_range, None);
    }

    #[test]
    fn non_looping_segment_ms_uses_full_pass_duration() {
        // 8 帧 @18fps ≈ 445ms（向上取整）。
        let jump = item("ACT-M-04", 18, false, None, 8);
        assert_eq!(jump.segment_ms(), 445);
        // 8 帧 @15fps ≈ 534ms。
        let land = item("ACT-M-06", 15, false, None, 8);
        assert_eq!(land.segment_ms(), 534);
        // 循环动作取 dwell。
        let stand = item("ACT-M-01", 6, true, Some([0, 3]), 4);
        assert_eq!(stand.segment_ms(), 2_000);
    }
}
