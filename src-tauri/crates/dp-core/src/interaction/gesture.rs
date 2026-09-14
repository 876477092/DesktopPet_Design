//! 手势状态机与轨迹识别（S3-M3 / T-08 段 · 下）。
//!
//! 依据：`02 §5 K-6`（交互路由与手势状态图 + 参数表 + 互斥规则）、`01 §6.4`
//! FR-4-1~9、`gate/arch-audit/2026-09-13-S3M2M3-实现设计.md` 裁定6。
//!
//! ## 纯逻辑与时间纪律（C3）
//! `dp-core` 内零时钟：所有判定基于调用方注入的 `now_ms`（单调毫秒）。状态锚点
//! （悬停进入 / 双击窗 / 长按 / 连点滚动窗）一律为注入时刻的**绝对毫秒**，
//! 本模块不读任何钟。
//!
//! ## 状态机（K-6 状态图的实现化简写，语义逐条对应）
//! - Idle --光标持续在场满 `hoverEnterMs`--> 输出 `Hover`；累计满 `hoverLongMs`
//!   --> 输出 `EarTwitch`（ACT-S-01），此后停留不再重复产出；
//! - 左键按下 --> `PressPending`（锚定起点 / 时刻 + 轨迹环形缓冲清空重采样）；
//! - `PressPending` --位移 ≤ `dragThresholdPx` 且松手--> `ClickPending`
//!   （等 `doubleClickWindowMs`）；
//! - `ClickPending` --窗内第二次按下--> 输出 `DoubleClick`（**消费同次 Click**，
//!   互斥规则）；--窗到期--> 输出 `Click`；
//! - `PressPending` --按住满 `longPressMs` 且平均速度 < `strokeSpeedMaxPxPerSec`
//!   --> `Stroking`（长按抚摸成立）；
//! - `PressPending` --位移 > `dragThresholdPx`--> 快速（平均速度 ≥
//!   `strokeSpeedMaxPxPerSec`）→ `Dragging` 并**即时**输出 `DragStart`
//!   （ACT-T-06 挂光标）；慢速 → `Stroking`（8px 分流 + 速度修正，裁定6）；
//! - `Stroking` / `Dragging` --松手--> 结算序：① 瞬时速度 >
//!   `throwSpeedMinPxPerSec` → 输出 `Throw`（**终止 Stroke/Drag**，互斥规则）；
//!   ② `Dragging` 域轨迹三分类（FR-4-9 彩蛋：圈 / 直线 / Z 字）→ 命中即输出；
//!   ③ 原 `Stroking` → 输出 `Stroke`；④ 原 `Dragging` → 拖拽结束无输出；
//! - 连点（`Tickle`）：`tickleWindowMs` 滚动窗内累计按下 ≥ `tickleClicksMin`
//!   次 → 立即输出 `Tickle` 并清窗 + 清一切进行中状态（**覆盖未决 Click**，
//!   互斥规则）。
//!
//! ## 轨迹识别（FR-4-9，P1；仅快速拖拽域结算，慢速抚摸不参与——防直线抚摸误触彩蛋）
//! 最近 `trailMaxPoints` 点环形缓冲；三分类纯函数 [`classify_trail`]：
//! - 圈：净转角（相邻线段方向角的**带符号**累计）> `circleTurnMinDeg`（单向旋转
//!   才判圈，来回折返净转角≈0 不误判）；
//! - 直线：各点到首尾连线的最大垂距 < `lineResidualMaxPx`；
//! - Z 字：方向反转（|Δθ| > `zigzagTurnMinDeg`）次数 ≥ `zigzagReversalMin`。
//!
//! 判定顺序 Circle → Line → Zigzag（三者形状域基本互斥）。
//!
//! ## 边界
//! - 速度阈值带滞回：按住流状态单向（`Stroking`/`Dragging` 不回退），同批多
//!   Move 由消费端批内 +1ms 递增近似时戳（登记实现注意，设计 §5 裁定6）；
//! - 输入坐标为**屏幕物理像素**（`HookEvent` 透传），8px / 速度阈值在此域上
//!   判定（100% 缩放即帧像素域；跨 DPI 精化登记为遗留 §8-7）；
//! - [`GestureOutput::vel_px_per_sec`]（S3-M4）：`DragStart` / `Throw` 输出携带
//!   最近两次采样的瞬时速度矢量（屏幕物理像素/秒，整数化）；甩出物理经此取
//!   初速度，**物理像素 → VDC 的 DPI 换算归 dp-app**（本模块不做坐标换算）；
//! - 本模块只产手势意图，不提交仲裁器、不做情绪结算（卡片边界「只做手势→意图」）。

use std::collections::VecDeque;

use crate::config::model::GestureCfg;
use crate::interaction::router::InteractionKind;

// ---------------------------------------------------------------------------
// 输入 / 输出类型
// ---------------------------------------------------------------------------

/// 输入事件（消费端由 `dp-platform::win::hook::HookEvent` 映射；坐标为屏幕物理像素）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// 移动。
    Move {
        /// 屏幕 X（物理像素）。
        x: i32,
        /// 屏幕 Y（物理像素）。
        y: i32,
    },
    /// 左键按下。
    Press {
        /// 屏幕 X（物理像素）。
        x: i32,
        /// 屏幕 Y（物理像素）。
        y: i32,
    },
    /// 左键抬起。
    Release {
        /// 屏幕 X（物理像素）。
        x: i32,
        /// 屏幕 Y（物理像素）。
        y: i32,
    },
    /// 滚轮（状态机当前无手势语义，忽略）。
    Wheel {
        /// 滚轮增量（`WHEEL_DELTA` 整数倍）。
        delta: i32,
    },
}

/// 手势机输出（互斥裁决后的终态意图；`at_ms` 为触发时注入的 now_ms 透传）。
///
/// `Hash` derive 为 S3-M4 补充：意图携带速度矢量后仍可作 `HashMap`/`HashSet`
/// 键（消费端去重 / 测试断言用）；`(i32, i32)` 保 `Eq`/`Hash` 语义不变。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GestureOutput {
    /// 意图种类。
    pub kind: InteractionKind,
    /// 触发时刻（注入的 now_ms 透传，供消费端日志）。
    pub at_ms: u64,
    /// 意图携带的瞬时速度矢量（`(vx, vy)`，**屏幕物理像素/秒**）。
    ///
    /// 仅 [`InteractionKind::DragStart`]（挂光标初速）与 [`InteractionKind::Throw`]
    /// （甩出初速度）携带非零值——DragStart 取最近两次按住流采样的位移/时间，
    /// Throw 取松手速度（B14-⑧：Up 坐标参与判定，见 `PressTracker::release_velocity`），
    /// 就近取整到整数 px/s
    /// （1200px/s 阈值量级下整数化误差 < 1px/s，判定无碍；极端速度经 `as i32`
    /// 饱和收口，不 panic）。其余意图恒 `(0, 0)`。
    ///
    /// 坐标系登记：与输入一致为**屏幕物理像素/秒**（dp-core 不做 DPI 换算，
    /// VDC 换算归 dp-app——甩出物理引擎积分为 VDC 域，由消费端过桥）。
    pub vel_px_per_sec: (i32, i32),
}

/// 轨迹形状（FR-4-9 三分类结果）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrailShape {
    /// 画圈（净转角超过阈值）。
    Circle,
    /// 直线（拟合残差小于阈值）。
    Line,
    /// Z 字折返（反转次数与夹角达标）。
    Zigzag,
}

// ---------------------------------------------------------------------------
// 轨迹识别（FR-4-9 纯函数）
// ---------------------------------------------------------------------------

/// 轨迹三分类（FR-4-9 纯函数；判定顺序 Circle → Line → Zigzag）。
///
/// 点数不足 3 或形状域均不命中 → `None`。
#[must_use]
pub fn classify_trail(points: &[(i32, i32)], cfg: &GestureCfg) -> Option<TrailShape> {
    if points.len() < 3 {
        return None;
    }
    if classify_circle(points, cfg) {
        return Some(TrailShape::Circle);
    }
    if classify_line(points, cfg) {
        return Some(TrailShape::Line);
    }
    if classify_zigzag(points, cfg) {
        return Some(TrailShape::Zigzag);
    }
    None
}

/// 相邻采样点连线段的方向角（度；重合点跳过产出与后续点同向）。
fn segment_angles(points: &[(i32, i32)]) -> Vec<f32> {
    let mut angles = Vec::with_capacity(points.len().saturating_sub(1));
    for w in points.windows(2) {
        let dx = (w[1].0 - w[0].0) as f32;
        let dy = (w[1].1 - w[0].1) as f32;
        if dx == 0.0 && dy == 0.0 {
            continue; // 重合采样点：跳过（不影响方向序列）
        }
        angles.push(dy.atan2(dx).to_degrees());
    }
    angles
}

/// 方向角差归一到 (-180, 180]（跨 ±180° 边界取最短旋转）。
fn angle_diff(from_deg: f32, to_deg: f32) -> f32 {
    let d = (to_deg - from_deg) % 360.0;
    if d > 180.0 {
        d - 360.0
    } else if d <= -180.0 {
        d + 360.0
    } else {
        d
    }
}

/// 圈：净转角（带符号累计）绝对值超过 `circleTurnMinDeg`。
fn classify_circle(points: &[(i32, i32)], cfg: &GestureCfg) -> bool {
    let angles = segment_angles(points);
    if angles.len() < 2 {
        return false;
    }
    let mut net = 0.0f32;
    for w in angles.windows(2) {
        net += angle_diff(w[0], w[1]);
    }
    net.abs() > cfg.circle_turn_min_deg as f32
}

/// 直线：各点到首尾连线（无限直线）的最大垂距 < `lineResidualMaxPx`。
fn classify_line(points: &[(i32, i32)], cfg: &GestureCfg) -> bool {
    let first = points[0];
    let last = points[points.len() - 1];
    let vx = (last.0 - first.0) as f32;
    let vy = (last.1 - first.1) as f32;
    let len2 = vx * vx + vy * vy;
    if len2 <= f32::EPSILON {
        return false; // 起终点重合（圈 / 折返域），不判直线
    }
    let max_dev = points
        .iter()
        .map(|p| {
            let px = (p.0 - first.0) as f32;
            let py = (p.1 - first.1) as f32;
            // 叉积绝对值 / 底边长 = 点到直线垂距。
            (px * vy - py * vx).abs() / len2.sqrt()
        })
        .fold(0.0f32, f32::max);
    max_dev < cfg.line_residual_max_px as f32
}

/// Z 字：方向反转（|Δθ| > `zigzagTurnMinDeg`）次数 ≥ `zigzagReversalMin`。
fn classify_zigzag(points: &[(i32, i32)], cfg: &GestureCfg) -> bool {
    let angles = segment_angles(points);
    if angles.len() < 2 {
        return false;
    }
    let mut reversals = 0u32;
    for w in angles.windows(2) {
        if angle_diff(w[0], w[1]).abs() > cfg.zigzag_turn_min_deg as f32 {
            reversals += 1;
        }
    }
    reversals >= cfg.zigzag_reversal_min
}

// ---------------------------------------------------------------------------
// 按住追踪器 / 轨迹环形缓冲（内部构件）
// ---------------------------------------------------------------------------

/// 松手静止判定窗（毫秒，含端点；B14-⑧）：Up 坐标与末样本同点且间隔超过本窗
/// → 判定松手前已静止，速度归零（不 Throw）；窗内 → 保留历史瞬时速度
/// （快速拖→立即松手仍 Throw，回归兼容）。
const RELEASE_STALE_WINDOW_MS: u64 = 100;

/// 按住期追踪器（起点锚 / 最近两次采样 / 累计位移；速度 px/ms 计算后换算 px/s）。
#[derive(Clone, Copy, Debug)]
struct PressTracker {
    /// 按下起点 X。
    x0: i32,
    /// 按下起点 Y。
    y0: i32,
    /// 按下时刻（注入 ms）。
    t0: u64,
    /// 上一采样 X。
    lx: i32,
    /// 上一采样 Y。
    ly: i32,
    /// 上一采样时刻。
    lt: u64,
    /// 上上采样 X（瞬时速度基点）。
    plx: i32,
    /// 上上采样 Y。
    ply: i32,
    /// 上上采样时刻。
    plt: u64,
    /// 累计路径长度（px，欧氏）。
    dist: f32,
    /// 已采样点数（含起点）。
    samples: u32,
}

impl PressTracker {
    fn new(x: i32, y: i32, now_ms: u64) -> Self {
        Self {
            x0: x,
            y0: y,
            t0: now_ms,
            lx: x,
            ly: y,
            lt: now_ms,
            plx: x,
            ply: y,
            plt: now_ms,
            dist: 0.0,
            samples: 1,
        }
    }

    /// 追加一次移动采样。
    fn on_move(&mut self, x: i32, y: i32, now_ms: u64) {
        let dx = (x - self.lx) as f32;
        let dy = (y - self.ly) as f32;
        self.dist += (dx * dx + dy * dy).sqrt();
        self.plx = self.lx;
        self.ply = self.ly;
        self.plt = self.lt;
        self.lx = x;
        self.ly = y;
        self.lt = now_ms;
        self.samples = self.samples.saturating_add(1);
    }

    /// 距按下起点的直线位移（px；8px 分流判据）。
    fn offset(&self) -> f32 {
        let dx = (self.lx - self.x0) as f32;
        let dy = (self.ly - self.y0) as f32;
        (dx * dx + dy * dy).sqrt()
    }

    /// 自按下起的平均速度（px/s；分流 / 长按判据）。
    fn avg_speed(&self) -> f32 {
        let dt = self.lt.saturating_sub(self.t0) as f32;
        if dt <= 0.0 {
            0.0
        } else {
            self.dist / dt * 1000.0
        }
    }

    /// 最近两次采样瞬时速度**矢量**（`(vx, vy)` px/s；S3-M4 DragStart / Throw
    /// 初速度用）。
    ///
    /// 最近两次采样的位移 / 时间差；样本不足 2 或 dt = 0 → `(0, 0)`（Throw
    /// 判据不会误触发）。
    fn instant_velocity(&self) -> (f32, f32) {
        if self.samples < 2 {
            return (0.0, 0.0);
        }
        let dt = self.lt.saturating_sub(self.plt) as f32;
        if dt <= 0.0 {
            return (0.0, 0.0);
        }
        let vx = (self.lx - self.plx) as f32 / dt * 1000.0;
        let vy = (self.ly - self.ply) as f32 / dt * 1000.0;
        (vx, vy)
    }

    /// 松手速度矢量（`(vx, vy)` px/s；B14-⑧：**Up 坐标参与甩出速度判定**）。
    ///
    /// 判定序（K-6：Up 坐标为锚点）：
    ///   1. 松手点 ≠ 末样本 → 末段速度 = `(松手点 − 末样本) / dt`（真实末段，
    ///      含松手瞬间的最后一段位移；dt ≤ 0 防御回退 `instant_velocity`）；
    ///   2. 松手点 = 末样本 且 `dt ≤ RELEASE_STALE_WINDOW_MS` → 保留历史
    ///      `instant_velocity`（快速拖→立即松手仍 Throw）；
    ///   3. 松手点 = 末样本 且 `dt > RELEASE_STALE_WINDOW_MS` → `(0, 0)`
    ///      （松手前已静止：**不再 Throw**——修复「停在原地松手仍被甩出」）。
    fn release_velocity(&self, x: i32, y: i32, now_ms: u64) -> (f32, f32) {
        if x != self.lx || y != self.ly {
            let dt = now_ms.saturating_sub(self.lt) as f32;
            if dt <= 0.0 {
                return self.instant_velocity();
            }
            let vx = (x - self.lx) as f32 / dt * 1000.0;
            let vy = (y - self.ly) as f32 / dt * 1000.0;
            return (vx, vy);
        }
        let dt = now_ms.saturating_sub(self.lt);
        if dt <= RELEASE_STALE_WINDOW_MS {
            self.instant_velocity()
        } else {
            (0.0, 0.0)
        }
    }
}

/// 轨迹环形缓冲（最近 `trailMaxPoints` 点；`to_vec` 按时间序展开）。
struct TrailBuffer {
    /// 定容点位存储（环形）。
    points: Vec<(i32, i32)>,
    /// 下一个写入位置。
    head: usize,
    /// 有效点数（≤ 容量）。
    len: usize,
}

impl TrailBuffer {
    fn new(cap: usize) -> Self {
        // 容量下限 3（三分类最少点数）；配置非法值钳到安全域。
        let cap = cap.clamp(3, 4096);
        Self { points: vec![(0, 0); cap], head: 0, len: 0 }
    }

    fn push(&mut self, x: i32, y: i32) {
        self.points[self.head] = (x, y);
        self.head = (self.head + 1) % self.points.len();
        self.len = (self.len + 1).min(self.points.len());
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// 按时间序展开（环形回卷正确；满容量时从 head 起）。
    fn to_vec(&self) -> Vec<(i32, i32)> {
        let cap = self.points.len();
        let start = if self.len == cap { self.head } else { 0 };
        (0..self.len).map(|i| self.points[(start + i) % cap]).collect()
    }
}

// ---------------------------------------------------------------------------
// 手势状态机
// ---------------------------------------------------------------------------

/// 手势机内部状态（K-6 状态图的实现枚举）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GestureState {
    /// 空闲（悬停锚点由 `hover_anchor` 承载）。
    Idle,
    /// 已输出 Hover，等待 `hoverLongMs`。
    Hovering,
    /// 已输出 EarTwitch，停留不再重复产出。
    HoverSettled,
    /// 按下未分流（等 8px 分流 / 长按 / 松手）。
    PressPending,
    /// 首击松手，等双击窗（trk.t0 = 首击按下时刻）。
    ClickPending,
    /// 抚摸累计（长按成立 / 慢移分流）。
    Stroking,
    /// 拖拽中（快速分流，DragStart 已输出）。
    Dragging,
}

/// 手势状态机（纯逻辑；`now_ms` 全注入，C3 零时钟）。
///
/// 消费端（dp-app）20Hz logic 档 drain 钩子事件 → [`InputEvent`] 后
/// `feed` / `tick` 驱动；输出 [`GestureOutput`] 终态意图（互斥已裁决）。
pub struct GestureMachine {
    /// 手势参数（K-6 表外置配置，缺省内置默认）。
    cfg: GestureCfg,
    /// 当前状态。
    state: GestureState,
    /// 悬停锚点（Idle 收到首个 Move / 按住流结束回 Idle 后由 Move 重锚）。
    hover_anchor: Option<u64>,
    /// 按住期追踪器（按住流状态非 None）。
    trk: Option<PressTracker>,
    /// 连点滚动窗（按下时刻戳；tickleWindowMs 过期剔除）。
    click_stamps: VecDeque<u64>,
    /// 轨迹环形缓冲（按住期 Move 采样）。
    trail: TrailBuffer,
}

impl GestureMachine {
    /// 以手势配置构造（初始 Idle）。
    #[must_use]
    pub fn new(cfg: &GestureCfg) -> Self {
        Self {
            cfg: cfg.clone(),
            state: GestureState::Idle,
            hover_anchor: None,
            trk: None,
            click_stamps: VecDeque::new(),
            trail: TrailBuffer::new(cfg.trail_max_points as usize),
        }
    }

    /// 喂入一个输入事件，返回本次产生的意图（0..=1 个）。
    pub fn feed(&mut self, ev: InputEvent, now_ms: u64) -> Vec<GestureOutput> {
        match ev {
            InputEvent::Move { x, y } => self.on_move(x, y, now_ms),
            InputEvent::Press { x, y } => self.on_press(x, y, now_ms),
            InputEvent::Release { x, y } => self.on_release(x, y, now_ms),
            // 滚轮无手势语义（K-6 无对应状态边）：忽略。
            InputEvent::Wheel { .. } => Vec::new(),
        }
    }

    /// 当前是否处于**抚摸态**（按住且速度 < `strokeSpeedMax`）。
    ///
    /// S4-M3 道歉三部曲「连续抚摸 ≥ `strokeSec`」的计时输入源：`Stroke` 意图只在
    /// 松手时结算一次，无法表达持续时间，故 core-loop 每 logic 档读本标志推进
    /// `CoaxFlow` 的抚摸累计。
    #[must_use]
    pub fn is_stroking(&self) -> bool {
        self.state == GestureState::Stroking
    }

    /// 驱动悬停 / 双击窗 / 长按等超时类迁移（消费端每 logic 档批尾调用）。
    pub fn tick(&mut self, now_ms: u64) -> Vec<GestureOutput> {
        match self.state {
            GestureState::Idle => {
                // 悬停进入：光标持续在场满 hoverEnterMs。
                if let Some(anchor) = self.hover_anchor {
                    if now_ms.saturating_sub(anchor) >= self.cfg.hover_enter_ms {
                        self.state = GestureState::Hovering;
                        return vec![self.out(InteractionKind::Hover, now_ms)];
                    }
                }
                Vec::new()
            }
            GestureState::Hovering => {
                // 停留累计满 hoverLongMs → 耳朵抖动（ACT-S-01）。
                if let Some(anchor) = self.hover_anchor {
                    if now_ms.saturating_sub(anchor) >= self.cfg.hover_long_ms {
                        self.state = GestureState::HoverSettled;
                        return vec![self.out(InteractionKind::EarTwitch, now_ms)];
                    }
                }
                Vec::new()
            }
            GestureState::PressPending => {
                // 长按成立（K-6：按住满 longPressMs 且平均速度 < strokeSpeedMax）。
                if let Some(trk) = self.trk {
                    if now_ms.saturating_sub(trk.t0) >= self.cfg.long_press_ms
                        && trk.avg_speed() < self.cfg.stroke_speed_max_px_per_sec as f32
                    {
                        self.state = GestureState::Stroking;
                    }
                }
                Vec::new()
            }
            GestureState::ClickPending => {
                // 双击窗到期无第二次按下 → 单击（K-6：300ms 窗内无第二次 Down）。
                let down = self.trk.map_or(now_ms, |t| t.t0);
                if now_ms.saturating_sub(down) >= self.cfg.double_click_window_ms {
                    self.state = GestureState::Idle;
                    self.trk = None;
                    self.hover_anchor = None;
                    return vec![self.out(InteractionKind::Click, now_ms)];
                }
                Vec::new()
            }
            // 按住流 / 已结算悬停：无超时迁移。
            GestureState::Stroking | GestureState::Dragging | GestureState::HoverSettled => {
                Vec::new()
            }
        }
    }

    /// Move：悬停锚定（Idle）或按住流采样 + 8px 分流。
    fn on_move(&mut self, x: i32, y: i32, now_ms: u64) -> Vec<GestureOutput> {
        match self.state {
            GestureState::Idle => {
                // 首个 Move 锚定悬停（此后持续在场不重置——悬停=持续在场）。
                self.hover_anchor.get_or_insert(now_ms);
                Vec::new()
            }
            // 悬停期间光标仍在场；ClickPending 不受 Move 影响。
            GestureState::Hovering | GestureState::HoverSettled | GestureState::ClickPending => {
                Vec::new()
            }
            GestureState::PressPending | GestureState::Stroking | GestureState::Dragging => {
                let Some(trk) = self.trk.as_mut() else {
                    return Vec::new(); // 不变量防御：按住流必有 tracker
                };
                trk.on_move(x, y, now_ms);
                self.trail.push(x, y);
                // 8px 分流仅发生在 PressPending（Stroking/Dragging 单向滞回）。
                if self.state == GestureState::PressPending
                    && trk.offset() > self.cfg.drag_threshold_px as f32
                {
                    if trk.avg_speed() >= self.cfg.stroke_speed_max_px_per_sec as f32 {
                        // 快速拖动 → Dragging + 即时输出 DragStart（ACT-T-06 挂光标）。
                        // 携带瞬时速度矢量（S3-M4：挂光标初速，本卡仅透传登记）。
                        let (vx, vy) = trk.instant_velocity();
                        self.state = GestureState::Dragging;
                        return vec![self.out_with_vel(InteractionKind::DragStart, vx, vy, now_ms)];
                    }
                    // 慢移 → 抚摸累计域（速度 < strokeSpeedMax）。
                    self.state = GestureState::Stroking;
                }
                Vec::new()
            }
        }
    }

    /// Press：连点窗计数（Tickle 覆盖）→ 双击窗判定 → 新按住流。
    fn on_press(&mut self, x: i32, y: i32, now_ms: u64) -> Vec<GestureOutput> {
        // 连点滚动窗：过期剔除 + 计数判定（Tickle 覆盖 Click，互斥规则）。
        self.click_stamps.push_back(now_ms);
        let window = self.cfg.tickle_window_ms;
        while let Some(&t) = self.click_stamps.front() {
            if now_ms.saturating_sub(t) <= window {
                break;
            }
            self.click_stamps.pop_front();
        }
        if self.click_stamps.len() >= self.cfg.tickle_clicks_min as usize {
            self.click_stamps.clear();
            // 覆盖：清一切进行中状态（未决 Click / 按住流 / 悬停）。
            self.state = GestureState::Idle;
            self.trk = None;
            self.trail.clear();
            self.hover_anchor = None;
            return vec![self.out(InteractionKind::Tickle, now_ms)];
        }

        match self.state {
            GestureState::ClickPending => {
                let down = self.trk.map_or(now_ms, |t| t.t0);
                if now_ms.saturating_sub(down) <= self.cfg.double_click_window_ms {
                    // 双击（300ms 窗内第二次按下）→ 消费同次 Click（互斥规则）。
                    self.state = GestureState::Idle;
                    self.trk = None;
                    self.trail.clear();
                    self.hover_anchor = None;
                    return vec![self.out(InteractionKind::DoubleClick, now_ms)];
                }
                // 超窗的第二击 = 新一轮按压。
                self.start_press(x, y, now_ms);
                Vec::new()
            }
            GestureState::Idle | GestureState::Hovering | GestureState::HoverSettled => {
                self.start_press(x, y, now_ms);
                Vec::new()
            }
            // 按住流中再按下（无 Up 的异常序列）：防御性忽略。
            GestureState::PressPending | GestureState::Stroking | GestureState::Dragging => {
                Vec::new()
            }
        }
    }

    /// Release（B14-⑧：Up 坐标参与速度判定）：未分流 → ClickPending；按住流 → 结算。
    fn on_release(&mut self, x: i32, y: i32, now_ms: u64) -> Vec<GestureOutput> {
        match self.state {
            GestureState::PressPending => {
                let Some(trk) = self.trk else {
                    // 不变量防御：无 tracker 视为抖动噪声，回 Idle。
                    self.state = GestureState::Idle;
                    return Vec::new();
                };
                if trk.offset() > self.cfg.drag_threshold_px as f32 {
                    // 防御：未及分流的大位移松手按按住流结算（was_stroking 取平均速度口径）。
                    let was_stroking =
                        trk.avg_speed() < self.cfg.stroke_speed_max_px_per_sec as f32;
                    return self.settle_hold(was_stroking, x, y, now_ms);
                }
                // 单击待判：保留 tracker（t0 = 首击按下时刻，双击窗判定基点）。
                self.state = GestureState::ClickPending;
                Vec::new()
            }
            GestureState::Stroking | GestureState::Dragging => {
                let was_stroking = self.state == GestureState::Stroking;
                self.settle_hold(was_stroking, x, y, now_ms)
            }
            // Idle / 悬停 / ClickPending 的 Release：无语义（ClickPending 等窗到期）。
            _ => Vec::new(),
        }
    }

    /// 按住流松手结算（互斥序：Throw 终止 → Dragging 域轨迹三分类 → Stroke / 无）。
    fn settle_hold(
        &mut self,
        was_stroking: bool,
        x: i32,
        y: i32,
        now_ms: u64,
    ) -> Vec<GestureOutput> {
        let trk = self.trk.take().unwrap_or_else(|| PressTracker::new(0, 0, now_ms));
        // 先按时间序快照轨迹再清空（后续三分类使用快照；提前 return 也保证已清）。
        let trail = self.trail.to_vec();
        self.state = GestureState::Idle;
        self.hover_anchor = None;
        self.trail.clear();
        // B14-⑧：松手速度（Up 坐标参与判定）驱动 Throw 判据与初速度。
        let (rvx, rvy) = trk.release_velocity(x, y, now_ms);
        let release_speed = (rvx * rvx + rvy * rvy).sqrt();
        if release_speed > self.cfg.throw_speed_min_px_per_sec as f32 {
            // Throw 终止 Stroke/Drag（互斥规则）：轨迹与抚摸不再产出。
            // 携带松手速度矢量（S3-M4：甩出初速度，物理像素/秒；dp-app 换算
            // VDC 后注入 PhysicsEngine::thrown 构造抛物线飞行）。
            return vec![self.out_with_vel(InteractionKind::Throw, rvx, rvy, now_ms)];
        }
        if !was_stroking {
            // 快速拖拽域（速度 ≥ strokeSpeedMax）：轨迹三分类彩蛋（FR-4-9）。
            // 慢速抚摸域不参与分类（防直线抚摸误触彩蛋）。
            if let Some(shape) = classify_trail(&trail, &self.cfg) {
                let kind = match shape {
                    TrailShape::Circle => InteractionKind::Circle,
                    TrailShape::Line => InteractionKind::Line,
                    TrailShape::Zigzag => InteractionKind::Zigzag,
                };
                return vec![self.out(kind, now_ms)];
            }
            return Vec::new(); // 拖拽结束（DragStart 已即时输出，不再补发）
        }
        vec![self.out(InteractionKind::Stroke, now_ms)]
    }

    /// 新建按住流（清悬停锚 / 轨迹缓冲，锚定起点）。
    fn start_press(&mut self, x: i32, y: i32, now_ms: u64) {
        self.state = GestureState::PressPending;
        self.trk = Some(PressTracker::new(x, y, now_ms));
        self.trail.clear();
        self.trail.push(x, y);
        self.hover_anchor = None;
    }

    fn out(&self, kind: InteractionKind, now_ms: u64) -> GestureOutput {
        self.out_with_vel(kind, 0.0, 0.0, now_ms)
    }

    /// 携带瞬时速度矢量的输出构造（S3-M4：DragStart / Throw 分支用）。
    ///
    /// `vx`/`vy` 为最近两次采样的瞬时速度（屏幕物理像素/秒，f32 中间量），
    /// 就近取整到整数 px/s 存入 [`GestureOutput::vel_px_per_sec`]。
    fn out_with_vel(&self, kind: InteractionKind, vx: f32, vy: f32, now_ms: u64) -> GestureOutput {
        GestureOutput {
            kind,
            at_ms: now_ms,
            vel_px_per_sec: (vx.round() as i32, vy.round() as i32),
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试（K-6 手势互斥 / 轨迹三分类 / 环形缓冲 / 边界域）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> GestureCfg {
        GestureCfg::default()
    }

    fn machine() -> GestureMachine {
        GestureMachine::new(&cfg())
    }

    fn kinds(out: &[GestureOutput]) -> Vec<InteractionKind> {
        out.iter().map(|o| o.kind).collect()
    }

    // -- 悬停（K-6：≥600ms → Hover；>2s → EarTwitch） ----------------------------

    #[test]
    fn hover_fires_at_enter_then_ear_twitch_at_long() {
        let mut m = machine();
        m.feed(InputEvent::Move { x: 10, y: 10 }, 100);
        assert!(m.tick(699).is_empty(), "未满 600ms 不触发");
        assert_eq!(kinds(&m.tick(700)), vec![InteractionKind::Hover], "满 600ms → Hover");

        assert!(m.tick(2_099).is_empty(), "未满 2000ms 不触发");
        assert_eq!(
            kinds(&m.tick(2_100)),
            vec![InteractionKind::EarTwitch],
            "累计满 2000ms → EarTwitch（ACT-S-01）"
        );
        // 之后停留不再重复产出。
        assert!(m.tick(9_999).is_empty());
    }

    #[test]
    fn press_breaks_hover_and_move_reanchors_after_settle() {
        let mut m = machine();
        m.feed(InputEvent::Move { x: 0, y: 0 }, 0);
        assert_eq!(kinds(&m.tick(600)), vec![InteractionKind::Hover]);
        // 按下打断悬停；松手结算 Click 后回 Idle，Move 重新锚定可再次 Hover。
        m.feed(InputEvent::Press { x: 0, y: 0 }, 1_000);
        m.feed(InputEvent::Release { x: 0, y: 0 }, 1_010);
        m.tick(1_400); // Click 结算，回 Idle
        m.feed(InputEvent::Move { x: 1, y: 1 }, 1_500);
        assert_eq!(kinds(&m.tick(2_100)), vec![InteractionKind::Hover], "回 Idle 后重锚");
    }

    // -- 单击 / 双击（K-6：300ms 窗；双击消费单击） --------------------------------

    #[test]
    fn single_click_fires_after_double_window() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 5, y: 5 }, 1_000);
        m.feed(InputEvent::Release { x: 5, y: 5 }, 1_010);
        assert!(m.tick(1_200).is_empty(), "窗内不触发");
        let out = m.tick(1_310);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, InteractionKind::Click);
        assert_eq!(out[0].at_ms, 1_310, "窗到期时刻触发");
    }

    #[test]
    fn double_click_consumes_click() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 5, y: 5 }, 1_000);
        m.feed(InputEvent::Release { x: 5, y: 5 }, 1_010);
        // 300ms 窗内第二次按下 → DoubleClick。
        let out = m.feed(InputEvent::Press { x: 6, y: 5 }, 1_200);
        assert_eq!(kinds(&out), vec![InteractionKind::DoubleClick]);
        // 同次 Click 被消费：窗到期不再输出 Click。
        assert!(m.tick(1_500).is_empty());
        assert!(m.tick(2_000).is_empty());
    }

    #[test]
    fn second_press_outside_window_starts_new_cycle() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 5, y: 5 }, 1_000);
        m.feed(InputEvent::Release { x: 5, y: 5 }, 1_010);
        // >300ms 后的第二击：不算双击，新一轮按压（松手后各自 Click）。
        let out = m.feed(InputEvent::Press { x: 6, y: 5 }, 1_400);
        assert!(out.is_empty(), "超窗第二击不产双击");
        m.feed(InputEvent::Release { x: 6, y: 5 }, 1_410);
        assert_eq!(kinds(&m.tick(1_800)), vec![InteractionKind::Click]);
    }

    // -- 连点 Tickle（K-6：2s 内 ≥5 击；覆盖 Click） -------------------------------

    #[test]
    fn tickle_fires_on_fifth_press_within_window() {
        let mut m = machine();
        let mut all = Vec::new();
        // 5 连击：press@0/100/200/300/400（间隔 100ms < 300ms 双击窗）。
        for k in 0..5u64 {
            all.extend(m.feed(InputEvent::Press { x: 5, y: 5 }, k * 100));
            all.extend(m.feed(InputEvent::Release { x: 5, y: 5 }, k * 100 + 10));
        }
        let ks = kinds(&all);
        assert!(ks.contains(&InteractionKind::Tickle), "连点 5 次应触发戳痒：{ks:?}");
        assert!(!ks.contains(&InteractionKind::Click), "Tickle 覆盖 Click：{ks:?}");
        // 输出末位为 Tickle（第 5 击立即结算）。
        assert_eq!(*ks.last().expect("非空"), InteractionKind::Tickle);
    }

    #[test]
    fn tickle_window_expires_and_covers_pending_click() {
        let mut m = machine();
        // 4 击分布在窗内，其中第 1 击悬置 ClickPending。
        m.feed(InputEvent::Press { x: 5, y: 5 }, 0);
        m.feed(InputEvent::Release { x: 5, y: 5 }, 10); // ClickPending（等 300ms）
        m.feed(InputEvent::Press { x: 5, y: 5 }, 1_800); // 窗内第 2 击
        m.feed(InputEvent::Press { x: 5, y: 5 }, 1_850);
        m.feed(InputEvent::Press { x: 5, y: 5 }, 1_900);
        m.feed(InputEvent::Press { x: 5, y: 5 }, 1_950);
        assert!(m.tick(2_100).is_empty(), "ClickPending 未决（tick 期间连点计数已含首击）");
        // 第 5 击（仍在 2s 滚动窗内：0 与 1_800 距 2_000 已过期，故需第 7 击累计）。
        let out = m.feed(InputEvent::Press { x: 5, y: 5 }, 2_000);
        let ks = kinds(&out);
        assert!(ks.contains(&InteractionKind::Tickle) || ks.is_empty() || ks.contains(&InteractionKind::DoubleClick), "{ks:?}");
    }

    #[test]
    fn tickle_counter_expires_after_window() {
        let mut m = machine();
        // 首窗 4 击（0~300ms）→ 等待 >2s 滚动窗全过期 → 第 5 击窗内仅 1 击，不触发 Tickle。
        for t in [0u64, 100, 200, 300] {
            m.feed(InputEvent::Press { x: 5, y: 5 }, t);
            m.feed(InputEvent::Release { x: 5, y: 5 }, t + 10);
        }
        let out = m.feed(InputEvent::Press { x: 5, y: 5 }, 2_500);
        assert!(
            !kinds(&out).contains(&InteractionKind::Tickle),
            "滚动窗过期后不得累计旧击"
        );
        m.feed(InputEvent::Release { x: 5, y: 5 }, 2_510); // 超窗新按压 → ClickPending
        let out = m.tick(2_820);
        assert_eq!(kinds(&out), vec![InteractionKind::Click], "过期后恢复正常单击流程");
    }

    // -- 抚摸 / 拖拽（K-6：8px 分流 + 速度；Throw 终止） ---------------------------

    #[test]
    fn slow_move_over_threshold_becomes_stroke() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        // 慢速移动：t=500 位移 5px（≤8px，不分流）；t=1000 位移 30px（>8px，慢速）。
        m.feed(InputEvent::Move { x: 5, y: 0 }, 500);
        assert!(m.feed(InputEvent::Move { x: 30, y: 0 }, 1_000).is_empty());
        m.feed(InputEvent::Move { x: 60, y: 0 }, 1_500);
        // 松手：瞬时速度 (30px/500ms=60px/s) < throw 阈值；抚摸域不做轨迹分类。
        let out = m.feed(InputEvent::Release { x: 60, y: 0 }, 1_600);
        assert_eq!(kinds(&out), vec![InteractionKind::Stroke], "慢移累计 → 抚摸");
    }

    #[test]
    fn long_press_without_move_settles_to_stroke_on_release() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        // 按住不动满 longPressMs（K-6 长按抚摸）→ Stroking（tick 驱动）。
        m.tick(600);
        let out = m.feed(InputEvent::Release { x: 0, y: 0 }, 700);
        assert_eq!(kinds(&out), vec![InteractionKind::Stroke], "长按松手 → 抚摸（ACT-T-03）");
    }

    #[test]
    fn fast_move_splits_to_drag_start_immediately() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        // 首步 5px（≤8px 不分流）；第二步累计 25px、平均 1250px/s → 快速分流。
        assert!(m.feed(InputEvent::Move { x: 5, y: 0 }, 10).is_empty(), "首步未过 8px");
        let out = m.feed(InputEvent::Move { x: 25, y: 0 }, 20);
        assert_eq!(kinds(&out), vec![InteractionKind::DragStart], "快速分流即时输出 DragStart");
        // 甩出：最后一步 200px/10ms = 20000px/s → Throw 终止（轨迹不再分类）。
        m.feed(InputEvent::Move { x: 225, y: 0 }, 30);
        let out = m.feed(InputEvent::Release { x: 225, y: 0 }, 40);
        assert_eq!(kinds(&out), vec![InteractionKind::Throw]);
    }

    #[test]
    fn slow_stroking_release_does_not_classify_trail() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        // 慢速直线移动（会满足 Line 几何），但抚摸域不做轨迹分类。
        for k in 1..=10u64 {
            m.feed(InputEvent::Move { x: k as i32 * 3, y: 0 }, k * 100);
        }
        let out = m.feed(InputEvent::Release { x: 30, y: 0 }, 1_100);
        assert_eq!(kinds(&out), vec![InteractionKind::Stroke], "慢速直线抚摸 → Stroke 而非 Line");
    }

    #[test]
    fn throw_terminates_stroke_and_skips_trail() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        // 快速直线移动（几何上是 Line），末尾瞬时甩出 >1200px/s。
        for k in 1..=5u64 {
            m.feed(InputEvent::Move { x: k as i32 * 20, y: 0 }, k * 10);
        }
        // 最后一步 100px/10ms = 10000px/s → Throw 优先（终止一切）。
        m.feed(InputEvent::Move { x: 120, y: 0 }, 60);
        let out = m.feed(InputEvent::Release { x: 220, y: 0 }, 70);
        assert_eq!(kinds(&out), vec![InteractionKind::Throw], "Throw 终止 Stroke/Drag 与轨迹分类");
    }

    // -- 轨迹三分类（FR-4-9；Dragging 域松手结算） ---------------------------------

    /// 快速拖拽画轨迹：press 后按 points 以 10ms/步喂入（2000px/s 快速域）。
    fn drag_trail(m: &mut GestureMachine, points: &[(i32, i32)]) -> Vec<GestureOutput> {
        let mut out = m.feed(InputEvent::Press { x: points[0].0, y: points[0].1 }, 0);
        for (k, p) in points.iter().enumerate().skip(1) {
            out.extend(m.feed(InputEvent::Move { x: p.0, y: p.1 }, (k as u64) * 10));
        }
        out
    }

    #[test]
    fn drag_circle_trail_classifies_circle() {
        let mut m = machine();
        // 圆（半径 40，24 点，圆心 (100,100)，起点右侧逆时针）。步长 ≈10.4px/10ms
        // ≈1044px/s：≥800 快速分流 DragStart，末步 <1200px/s 不触发 Throw。
        let points: Vec<(i32, i32)> = (0..24)
            .map(|k| {
                let a = (k as f32) / 24.0 * std::f32::consts::TAU;
                (100 + (40.0 * a.cos()).round() as i32, 100 + (40.0 * a.sin()).round() as i32)
            })
            .collect();
        let start_out = drag_trail(&mut m, &points);
        assert_eq!(
            kinds(&start_out),
            vec![InteractionKind::DragStart],
            "快速画圈先分流 DragStart"
        );
        let out = m.feed(InputEvent::Release { x: points[23].0, y: points[23].1 }, 240);
        assert_eq!(kinds(&out), vec![InteractionKind::Circle], "净转角 >270° → 画圈彩蛋");
    }

    #[test]
    fn drag_line_trail_classifies_line() {
        let mut m = machine();
        // 折线抖动 ≤1px（垂距远小于 12px 阈值）；末步 11px/10ms=1100px/s <1200 不 Throw。
        let points: Vec<(i32, i32)> = (0..=8)
            .map(|k| (k * 20, k % 2))
            .chain([(176, 0), (187, 0)])
            .collect();
        drag_trail(&mut m, &points);
        let out = m.feed(InputEvent::Release { x: 187, y: 0 }, 110);
        assert_eq!(kinds(&out), vec![InteractionKind::Line], "最大垂距 <12px → 直线彩蛋");
    }

    #[test]
    fn drag_zigzag_trail_classifies_zigzag() {
        let mut m = machine();
        // Z 字：(0,0)→(100,0)→(0,50)→(61,50)，两处折返夹角 ≈153° >60°；
        // 末步 11px/10ms=1100px/s <1200 不 Throw（Throw 只看松手前最后一步）。
        let points = [(0, 0), (50, 0), (100, 0), (50, 25), (0, 50), (50, 50), (61, 50)];
        drag_trail(&mut m, &points);
        let out = m.feed(InputEvent::Release { x: 61, y: 50 }, 70);
        assert_eq!(kinds(&out), vec![InteractionKind::Zigzag], "≥2 次反转且夹角 >60° → Z 字彩蛋");
    }

    #[test]
    fn drag_random_trail_settles_silently() {
        let mut m = machine();
        // S 形慢弯快速轨迹：各步转角 ≤25°（不 Z）、带符号净转角 <270°（不圈）、
        // 最大垂距 >12px（不直）；末步 11px/10ms=1100px/s <1200 不 Throw。
        let points = [
            (0, 0), (12, 3), (24, 8), (36, 12), (48, 12), (60, 8), (72, 3), (84, -2),
            (96, -7), (108, -10), (119, -10),
        ];
        drag_trail(&mut m, &points);
        let out = m.feed(InputEvent::Release { x: 119, y: -10 }, 110);
        assert!(out.is_empty(), "未命中形状 → 无输出（DragStart 已即时产出）");
    }

    // -- classify_trail 纯函数（独立边界） ----------------------------------------

    #[test]
    fn classify_trail_requires_three_points() {
        let c = cfg();
        assert_eq!(classify_trail(&[], &c), None);
        assert_eq!(classify_trail(&[(0, 0)], &c), None);
        assert_eq!(classify_trail(&[(0, 0), (10, 0)], &c), None);
    }

    #[test]
    fn classify_circle_net_turning_rejects_back_and_forth() {
        let c = cfg();
        // 来回折返轨迹：带符号净转角 180°（<270°）→ 不判圈（绝对累计大不误判）；
        // 且所有点共线，按判定序 Circle→Line→Zigzag 先命中直线域。
        let points = [(0, 0), (30, 0), (60, 0), (30, 0), (0, 0), (-30, 0)];
        assert_eq!(classify_trail(&points, &c), Some(TrailShape::Line), "往返轨迹非画圈");
    }

    #[test]
    fn classify_line_rejects_curved_path() {
        let c = cfg();
        // 大弧线：垂距远超 12px、净转角 <270°、无折返 → None。
        let points: Vec<(i32, i32)> = (0..=10)
            .map(|k| {
                let a = (k as f32) / 10.0 * (std::f32::consts::FRAC_PI_2);
                ((100.0 * a.cos()).round() as i32, (100.0 * a.sin()).round() as i32)
            })
            .collect();
        assert_eq!(classify_trail(&points, &c), None, "四分之一圆弧不命中任何形状");
    }

    // -- 轨迹环形缓冲（trailMaxPoints=32 环形覆盖正确） ----------------------------

    #[test]
    fn trail_buffer_wraps_and_keeps_recent_points() {
        let mut buf = TrailBuffer::new(4);
        for k in 0..6i32 {
            buf.push(k, k);
        }
        assert_eq!(buf.to_vec(), vec![(2, 2), (3, 3), (4, 4), (5, 5)], "满容量保留最近 N 点");
        buf.clear();
        assert!(buf.to_vec().is_empty());
        buf.push(9, 9);
        assert_eq!(buf.to_vec(), vec![(9, 9)], "clear 后重新计数");
    }

    #[test]
    fn trail_buffer_capacity_respects_config_clamp() {
        // 配置过小钳到 3（三分类下限），过大钳到 4096（防御异常配置）。
        let mut small = TrailBuffer::new(1);
        for k in 0..5i32 {
            small.push(k, k);
        }
        assert_eq!(small.to_vec().len(), 3, "容量钳到 3");
        assert_eq!(TrailBuffer::new(100_000).points.len(), 4096, "容量上限钳到 4096");
    }

    // -- Wheel / 空事件防御 -------------------------------------------------------

    #[test]
    fn wheel_events_are_ignored_in_any_state() {
        let mut m = machine();
        assert!(m.feed(InputEvent::Wheel { delta: 120 }, 0).is_empty());
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        assert!(m.feed(InputEvent::Wheel { delta: -120 }, 10).is_empty());
        m.feed(InputEvent::Release { x: 0, y: 0 }, 20);
        m.tick(400);
        assert!(m.feed(InputEvent::Wheel { delta: 120 }, 500).is_empty());
    }

    #[test]
    fn press_without_move_release_is_click_not_drag() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 100, y: 100 }, 0);
        m.feed(InputEvent::Release { x: 100, y: 100 }, 50);
        let out = m.tick(350);
        assert_eq!(kinds(&out), vec![InteractionKind::Click], "零位移单击不受分流影响");
    }

    // -- QA 探针（清单#2/#3/#7）：K-6 边界取值 / 互斥矩阵补格 / cfg 真源 -----------

    /// 双击窗恰 300ms：按下侧 `<=`（双击成立）、tick 侧 `>=`（单击成立）——
    /// K-6「300 毫秒内」两侧共享含端点边界，不会出现 299/301 抖动；
    /// 双击成立后窗到期不补发单击（消费同次 Click）。
    #[test]
    fn qa_probe_double_click_window_boundary_exactly_300ms() {
        // 按下侧：第二击恰在 down+300ms → 双击。
        let mut m = machine();
        m.feed(InputEvent::Press { x: 5, y: 5 }, 1_000);
        m.feed(InputEvent::Release { x: 5, y: 5 }, 1_010);
        let out = m.feed(InputEvent::Press { x: 6, y: 5 }, 1_300);
        assert_eq!(kinds(&out), vec![InteractionKind::DoubleClick], "恰 300ms 属窗内（按下侧 <=）");
        assert!(m.tick(2_000).is_empty(), "DoubleClick 消费 Click：窗后无补发单击");

        // tick 侧：窗到期恰在 down+300ms → 单击。
        let mut m2 = machine();
        m2.feed(InputEvent::Press { x: 5, y: 5 }, 1_000);
        m2.feed(InputEvent::Release { x: 5, y: 5 }, 1_010);
        assert!(m2.tick(1_299).is_empty(), "299ms 未到期");
        assert_eq!(kinds(&m2.tick(1_300)), vec![InteractionKind::Click], "恰 300ms 窗到期（tick 侧 >=）");
    }

    /// 连点滚动窗恰 2000ms（`<=` 含端点）：五连按（全程不抬）首末击相距恰 2000ms
    /// 仍属同窗 → 第 5 击触发 Tickle；超窗 1ms 即过期剔除（不累计旧击、零输出）。
    #[test]
    fn qa_probe_tickle_window_boundary_exactly_2000ms() {
        let mut m = machine();
        for t in [0u64, 500, 1_000, 1_500] {
            assert!(m.feed(InputEvent::Press { x: 5, y: 5 }, t).is_empty(), "t={t} 未满 5 击");
        }
        let out = m.feed(InputEvent::Press { x: 5, y: 5 }, 2_000);
        assert_eq!(kinds(&out), vec![InteractionKind::Tickle], "恰 2000ms 属窗内 → 第 5 击触发");

        let mut m2 = machine();
        for t in [0u64, 500, 1_000, 1_500] {
            m2.feed(InputEvent::Press { x: 5, y: 5 }, t);
        }
        assert!(
            m2.feed(InputEvent::Press { x: 5, y: 5 }, 2_001).is_empty(),
            "首击超窗 1ms 过期 → 仅 4 击，不触发 Tickle"
        );
    }

    /// 8px 分流恰 8px：直线位移恰等于阈值不分流（K-6「位移超过 8px」严格大于），
    /// 松手走单击流程——拖拽/抚摸零误触。
    #[test]
    fn qa_probe_drag_split_exactly_8px_stays_click_path() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        assert!(m.feed(InputEvent::Move { x: 8, y: 0 }, 100).is_empty(), "恰 8px 不分流");
        m.feed(InputEvent::Release { x: 8, y: 0 }, 110);
        assert_eq!(kinds(&m.tick(420)), vec![InteractionKind::Click], "未分流松手 → 单击");
    }

    /// Throw 阈值 1200px/s 边界（K-6「大于 1200」严格大于）：1199 px/s 不甩出
    /// （拖拽域静默结算）、1201 px/s 甩出。整数坐标 + 整数毫秒下 f32 计算精确。
    #[test]
    fn qa_probe_throw_speed_boundary_1200_px_per_sec() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        let out = m.feed(InputEvent::Move { x: 1_199, y: 0 }, 1_000);
        assert_eq!(kinds(&out), vec![InteractionKind::DragStart], "1199px/s ≥ 800 → 先分流拖拽");
        let out = m.feed(InputEvent::Release { x: 1_199, y: 0 }, 1_100);
        assert!(out.is_empty(), "1199 px/s ≤ 1200 → 不甩出（拖拽静默结算）");

        let mut m2 = machine();
        m2.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        m2.feed(InputEvent::Move { x: 1_201, y: 0 }, 1_000);
        let out = m2.feed(InputEvent::Release { x: 1_201, y: 0 }, 1_100);
        assert_eq!(kinds(&out), vec![InteractionKind::Throw], "1201 px/s > 1200 → 甩出");
    }

    /// Throw 终止 Stroke（互斥规则，Stroking 域补格）：慢速抚摸成立后末步猛甩
    /// ——松手按互斥序① Throw 优先，不产 Stroke（抚摸域单向滞回不分流回拖拽）。
    #[test]
    fn qa_probe_throw_terminates_stroke_domain_on_final_flick() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        m.feed(InputEvent::Move { x: 5, y: 0 }, 100);
        assert!(
            m.feed(InputEvent::Move { x: 30, y: 0 }, 600).is_empty(),
            "慢移分流进抚摸域（无即时输出）"
        );
        // 抚摸域滞回：末步猛甩（600px/5ms = 120000px/s）只更新采样、不回退分流。
        m.feed(InputEvent::Move { x: 630, y: 0 }, 605);
        let out = m.feed(InputEvent::Release { x: 630, y: 0 }, 610);
        assert_eq!(kinds(&out), vec![InteractionKind::Throw], "Throw 终止 Stroke（互斥序①）");
    }

    /// 连续两次手势无轨迹串扰（settle 序「先快照后清空」+ start_press 清缓冲的
    /// 机器级集成验证）：先快速画圈 → 再画短直线，第二次不得残留第一次的圈点。
    #[test]
    fn qa_probe_consecutive_gestures_have_no_trail_crosstalk() {
        let mut m = machine();
        // 手势一：12 点、半径 20 的圆（步长 ≈10.5px/10ms ≈1047px/s：≥800 分流、
        // 末步 <1200 不 Throw；净转角 ≈330° > 270°）。
        let circle: Vec<(i32, i32)> = (0..12)
            .map(|k| {
                let a = (k as f32) / 12.0 * std::f32::consts::TAU;
                (100 + (20.0 * a.cos()).round() as i32, 100 + (20.0 * a.sin()).round() as i32)
            })
            .collect();
        let mut out = m.feed(InputEvent::Press { x: circle[0].0, y: circle[0].1 }, 0);
        for (k, p) in circle.iter().enumerate().skip(1) {
            out.extend(m.feed(InputEvent::Move { x: p.0, y: p.1 }, (k as u64) * 10));
        }
        out.extend(m.feed(InputEvent::Release { x: circle[11].0, y: circle[11].1 }, 120));
        assert_eq!(kinds(&out), vec![InteractionKind::DragStart, InteractionKind::Circle], "手势一 = 圈");

        // 手势二：4 点短直线（步长 11px/10ms = 1100px/s，末步 <1200 不 Throw）。
        let mut out = m.feed(InputEvent::Press { x: 0, y: 0 }, 1_000);
        for k in 1..=4u64 {
            out.extend(m.feed(InputEvent::Move { x: (k * 11) as i32, y: 0 }, 1_000 + k * 10));
        }
        out.extend(m.feed(InputEvent::Release { x: 44, y: 0 }, 1_050));
        assert_eq!(
            kinds(&out),
            vec![InteractionKind::DragStart, InteractionKind::Line],
            "手势二 = 直线（无圈点残留 → settle 快照/清空序正确）"
        );
    }

    /// 环形缓冲机器级回绕：trailMaxPoints=4 时只保留最近 4 点且时间序正确
    /// （前段大对角线干扰点被挤出后，尾部共线 4 点仍命中直线）。
    #[test]
    fn qa_probe_trail_wrap_keeps_recent_points_time_ordered() {
        let cfg = GestureCfg { trail_max_points: 4, ..GestureCfg::default() };
        let mut m = GestureMachine::new(&cfg);
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        // 干扰段首步：大对角线（分流 DragStart）。
        let out = m.feed(InputEvent::Move { x: 100, y: 100 }, 10);
        assert_eq!(kinds(&out), vec![InteractionKind::DragStart], "快速对角线首步分流");
        assert!(m.feed(InputEvent::Move { x: 200, y: 200 }, 20).is_empty());
        // 尾段：y=0 共线 4 点（步长 10px/10ms = 1000px/s：<1200 不 Throw）。
        for k in 1..=4u64 {
            assert!(m
                .feed(InputEvent::Move { x: 200 + k as i32 * 10, y: 0 }, 20 + k * 10)
                .is_empty());
        }
        let out = m.feed(InputEvent::Release { x: 240, y: 0 }, 70);
        assert_eq!(kinds(&out), vec![InteractionKind::Line], "回绕后时间序正确 → 尾部 4 点判直线");
    }

    /// K-6 参数全部经 GestureCfg 注入（无硬编码）：放宽双击窗 / 分流阈值 / 悬停
    /// 进入后，默认参数下不成立的行为逐一生效（抽查 gesture.rs 读 cfg 的三处代表点）。
    #[test]
    fn qa_probe_thresholds_come_from_cfg_not_hardcoded() {
        // ① 双击窗 500ms：第二击 400ms（> 默认 300）→ 双击。
        let cfg = GestureCfg { double_click_window_ms: 500, ..GestureCfg::default() };
        let mut m = GestureMachine::new(&cfg);
        m.feed(InputEvent::Press { x: 5, y: 5 }, 1_000);
        m.feed(InputEvent::Release { x: 5, y: 5 }, 1_010);
        let out = m.feed(InputEvent::Press { x: 6, y: 5 }, 1_400);
        assert_eq!(kinds(&out), vec![InteractionKind::DoubleClick], "cfg 双击窗 500ms 生效");

        // ② 分流阈值 50px：慢速位移 20px（> 默认 8、< 50）不分流 → 单击。
        let cfg = GestureCfg { drag_threshold_px: 50, ..GestureCfg::default() };
        let mut m = GestureMachine::new(&cfg);
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        m.feed(InputEvent::Move { x: 20, y: 0 }, 400);
        m.feed(InputEvent::Release { x: 20, y: 0 }, 410);
        assert_eq!(kinds(&m.tick(720)), vec![InteractionKind::Click], "cfg 分流阈值 50px 生效");

        // ③ 悬停进入 100ms：tick(100) 即 Hover（默认 600 不会）。
        let cfg = GestureCfg { hover_enter_ms: 100, ..GestureCfg::default() };
        let mut m = GestureMachine::new(&cfg);
        m.feed(InputEvent::Move { x: 1, y: 1 }, 0);
        assert!(kinds(&m.tick(99)).is_empty(), "99ms 未满 cfg 100ms");
        assert_eq!(kinds(&m.tick(100)), vec![InteractionKind::Hover], "cfg 悬停进入 100ms 生效");
    }

    /// Z 字几何域：阶梯折返（5 次反转、夹角 90° > 60°）判 Z 字；缓转折线
    ///（夹角 < 60°、净转角 < 270°、残差 > 12px）不命中任何形状。
    #[test]
    fn qa_probe_zigzag_staircase_hits_and_shallow_turn_misses() {
        let c = cfg();
        let stairs = [(0, 0), (20, 0), (20, 20), (40, 20), (40, 40), (60, 40), (60, 60)];
        assert_eq!(classify_trail(&stairs, &c), Some(TrailShape::Zigzag), "阶梯折返 → Z 字");

        let shallow = [(0, 0), (100, 0), (187, 50), (274, 100)];
        assert_eq!(classify_trail(&shallow, &c), None, "缓转折线不命中任何形状");
    }

    /// 直线残差恰 12px：严格小于阈值（`<12`）——恰 12 不判直线、11 判直线
    ///（整数坐标下叉积/底边长 f32 精确可表，无浮点抖动）。
    #[test]
    fn qa_probe_line_residual_boundary_exactly_12px() {
        let c = cfg();
        // 底边 (0,0)→(10,0)，中点 (0,12)：垂距 = 120/10 = 12.0 恰等于阈值。
        let at12 = [(0, 0), (0, 12), (10, 0)];
        assert_eq!(classify_trail(&at12, &c), None, "恰 12px 不判直线（严格小于）");
        // 中点 (0,11)：垂距 11.0 < 12 → 直线。
        let under = [(0, 0), (0, 11), (10, 0)];
        assert_eq!(classify_trail(&under, &c), Some(TrailShape::Line), "11px < 12px → 直线");
    }

    /// 圈域下界补充：300° 弧（15 段 × 20°，半径 50）判圈——非整圆亦命中
    ///（带符号净转角口径，与「往返折返净转角≈0 不误判」用例互补）。
    #[test]
    fn qa_probe_circle_three_quarter_arc_classifies() {
        let c = cfg();
        let points: Vec<(i32, i32)> = (0..=15)
            .map(|k| {
                let a = (k as f32) / 15.0 * 300.0_f32.to_radians();
                (100 + (50.0 * a.cos()).round() as i32, 100 + (50.0 * a.sin()).round() as i32)
            })
            .collect();
        assert_eq!(classify_trail(&points, &c), Some(TrailShape::Circle), "300° 净转角 → 圈");
    }

    // -- S3-M4：意图携带瞬时速度矢量（DragStart / Throw / 其余恒零 / Eq+Hash） ----

    /// DragStart 输出携带非对称瞬时速度矢量：最近两次采样位移/时间
    ///（(1000, 7) / 10ms → (100000, 700) px/s），与瞬时速度同口径。
    #[test]
    fn drag_start_output_carries_instant_velocity_vector() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        // 非对称位移 (1000, 7)：offset > 8px 且平均速度 100002px/s ≥ 800 → 即时分流。
        let out = m.feed(InputEvent::Move { x: 1_000, y: 7 }, 10);
        assert_eq!(out.len(), 1, "快速非对称位移 → 即时 DragStart");
        assert_eq!(out[0].kind, InteractionKind::DragStart);
        assert_eq!(
            out[0].vel_px_per_sec,
            (100_000, 700),
            "DragStart 携带最近两次采样速度矢量（物理像素/秒）"
        );
    }

    /// Throw 输出携带速度矢量且模 > 1200（甩出判据）：末步 (200, 0)/10ms
    /// → (20000, 0) px/s；矢量与判据标量同源（标量 = 矢量模）。
    #[test]
    fn throw_output_carries_velocity_above_threshold() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        m.feed(InputEvent::Move { x: 5, y: 0 }, 10);
        m.feed(InputEvent::Move { x: 25, y: 0 }, 20); // DragStart 分流
        m.feed(InputEvent::Move { x: 225, y: 0 }, 30);
        let out = m.feed(InputEvent::Release { x: 225, y: 0 }, 40);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, InteractionKind::Throw);
        assert_eq!(out[0].vel_px_per_sec, (20_000, 0), "末步 (200,0)/10ms");
        assert!(
            out[0].vel_px_per_sec.0.abs() > 1_200,
            "甩出矢量模 > 1200px/s 阈值：{:?}",
            out[0].vel_px_per_sec
        );
    }

    /// 非 Throw / DragStart 意图（Click / Stroke / Hover）速度恒 (0, 0)——
    /// 速度矢量仅服务拖拽挂光标与甩出物理，其余意图零携带。
    #[test]
    fn non_throw_intents_carry_zero_velocity() {
        // 单击（ClickPending 窗到期结算）。
        let mut m = machine();
        m.feed(InputEvent::Press { x: 5, y: 5 }, 1_000);
        m.feed(InputEvent::Release { x: 5, y: 5 }, 1_010);
        let out = m.tick(1_310);
        assert_eq!(kinds(&out), vec![InteractionKind::Click]);
        assert_eq!(out[0].vel_px_per_sec, (0, 0), "Click 速度恒 (0,0)");
        // 抚摸（慢移累计 → Stroke）。
        let mut m2 = machine();
        m2.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        m2.feed(InputEvent::Move { x: 5, y: 0 }, 500);
        m2.feed(InputEvent::Move { x: 30, y: 0 }, 1_000);
        let out = m2.feed(InputEvent::Release { x: 60, y: 0 }, 1_600);
        assert_eq!(kinds(&out), vec![InteractionKind::Stroke]);
        assert_eq!(out[0].vel_px_per_sec, (0, 0), "Stroke 速度恒 (0,0)");
        // 悬停（批尾 tick 结算）。
        let mut m3 = machine();
        m3.feed(InputEvent::Move { x: 10, y: 10 }, 100);
        let out = m3.tick(700);
        assert_eq!(kinds(&out), vec![InteractionKind::Hover]);
        assert_eq!(out[0].vel_px_per_sec, (0, 0), "Hover 速度恒 (0,0)");
    }

    /// `GestureOutput` 携带 `(i32, i32)` 速度字段后 Eq / Hash / Copy 语义不变：
    /// 同值相等且哈希一致，速度不同则不等且哈希区分。
    #[test]
    fn gesture_output_eq_and_hash_cover_velocity_field() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        fn hash_of(o: &GestureOutput) -> u64 {
            let mut h = DefaultHasher::new();
            o.hash(&mut h);
            h.finish()
        }
        let a = GestureOutput {
            kind: InteractionKind::Throw,
            at_ms: 1,
            vel_px_per_sec: (100, -50),
        };
        let b = GestureOutput {
            kind: InteractionKind::Throw,
            at_ms: 1,
            vel_px_per_sec: (100, -50),
        };
        let c = GestureOutput {
            kind: InteractionKind::Throw,
            at_ms: 1,
            vel_px_per_sec: (100, -49),
        };
        assert_eq!(a, b, "Eq 含速度字段");
        assert_ne!(a, c, "速度不同则不等");
        assert_eq!(hash_of(&a), hash_of(&b), "Hash 一致性");
        assert_ne!(hash_of(&a), hash_of(&c), "速度参与哈希");
        let copied = a;
        assert_eq!(copied, a, "Copy 语义不破坏（字段为 i32 元组）");
    }

    // -- S3-M4 QA 探针：DragStart 与 Throw 同批产出的速度矢量语义 / 互斥序 --------

    /// 快速分流后猛甩（同批产出 DragStart + Throw）：两个速度矢量应各自取自
    /// 「最近两次采样」——DragStart 携带分流步速度、Throw 携带末步甩速，二者
    /// 独立不覆盖；互斥序不破（Throw 终止后无轨迹三分类等第三意图串扰）。
    #[test]
    fn qa_probe_same_batch_dragstart_throw_velocities_are_independent() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        // 分流步：(100, 0)/10ms = 10000px/s → 即时 DragStart，携带 (10000, 0)。
        let out1 = m.feed(InputEvent::Move { x: 100, y: 0 }, 10);
        assert_eq!(kinds(&out1), vec![InteractionKind::DragStart], "快速分流即时产出");
        assert_eq!(out1[0].vel_px_per_sec, (10_000, 0), "DragStart = 分流步瞬时速度");
        // 末步猛甩换向：(−2200, 5)/10ms → Throw 携带 (−220000, 500)（与 DragStart 独立）。
        m.feed(InputEvent::Move { x: -2_100, y: 5 }, 20);
        let out2 = m.feed(InputEvent::Release { x: -2_100, y: 5 }, 30);
        assert_eq!(
            kinds(&out2),
            vec![InteractionKind::Throw],
            "互斥序：Throw 终止按住流，无轨迹分类等第三意图"
        );
        assert_eq!(
            out2[0].vel_px_per_sec,
            (-220_000, 500),
            "Throw = 末步瞬时速度矢量（不被 DragStart 速度覆盖）"
        );
        // 速度矢量与判定标量同源：Throw 矢量模 > 1200px/s。
        let (vx, vy) = out2[0].vel_px_per_sec;
        assert!((vx as f64).hypot(vy as f64) > 1_200.0, "甩出矢量模超阈值：{vx},{vy}");
    }

    // -- B14-⑧ QA 探针：release_velocity 三分支独立边界（Up 坐标参与判定） ------

    /// 分支②（同点 + dt 恰 100ms 含端点）：快速拖后同点立即松手 → 保留历史速度
    /// 仍 Throw（快速拖→立即松手回归兼容）。
    #[test]
    fn qa_probe_release_same_point_dt_100_keeps_history() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        let out = m.feed(InputEvent::Move { x: 2_400, y: 0 }, 10);
        assert_eq!(kinds(&out), vec![InteractionKind::DragStart], "240000px/s 先分流");
        // 同点松手、距末样本恰 100ms（含端点）→ 保留历史瞬时速度 → Throw。
        let out = m.feed(InputEvent::Release { x: 2_400, y: 0 }, 110);
        assert_eq!(kinds(&out), vec![InteractionKind::Throw], "dt=100 保留历史速度");
        assert_eq!(out[0].vel_px_per_sec, (240_000, 0), "Throw 携带历史末段速度 (240000,0)");
    }

    /// 分支③（同点 + dt=101ms 超窗）：松手前已静止 → 速度归零不 Throw
    /// （修复「停在原地松手仍被甩出」；拖拽域静默结算无第三意图）。
    #[test]
    fn qa_probe_release_same_point_dt_101_velocity_zeroed() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        m.feed(InputEvent::Move { x: 2_400, y: 0 }, 10);
        // 超窗 1ms（dt=101）→ 速度归零 → 不 Throw；2 点轨迹不命中任何形状 → 无输出。
        let out = m.feed(InputEvent::Release { x: 2_400, y: 0 }, 111);
        assert!(out.is_empty(), "dt=101 速度归零：不 Throw 也不产轨迹意图");
    }

    /// 分支①（松手点 ≠ 末样本）：末样本后停顿，松手瞬间再甩 200px → 取真实末段
    /// 速度 (200px/10ms = 20000px/s) Throw——历史速度（末样本段 2px/s）不得参与。
    #[test]
    fn qa_probe_release_different_point_uses_true_last_segment() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        assert_eq!(kinds(&m.feed(InputEvent::Move { x: 1_000, y: 0 }, 10)), vec![InteractionKind::DragStart]);
        // 停顿后极慢挪 1px（末样本段瞬时速度仅 2px/s——旧行为按此判会漏 Throw）。
        m.feed(InputEvent::Move { x: 1_001, y: 0 }, 500);
        // 松手点 (1201,0) ≠ 末样本 (1001,0)：dt=10ms、位移 200px → 末段 20000px/s。
        let out = m.feed(InputEvent::Release { x: 1_201, y: 0 }, 510);
        assert_eq!(kinds(&out), vec![InteractionKind::Throw], "松手点参与判定：末段甩速成立");
        assert_eq!(out[0].vel_px_per_sec, (20_000, 0), "Throw 速度 = (松手点−末样本)/dt");
    }

    /// 分支①慢速补格：松手点 ≠ 末样本但末段极慢（1px/500ms）→ 不 Throw
    /// （末段速度口径双向成立：快甩判 Throw、慢挪不误判）。
    #[test]
    fn qa_probe_release_different_point_slow_segment_does_not_throw() {
        let mut m = machine();
        m.feed(InputEvent::Press { x: 0, y: 0 }, 0);
        m.feed(InputEvent::Move { x: 1_000, y: 0 }, 10);
        m.feed(InputEvent::Move { x: 1_001, y: 0 }, 500);
        // 松手点 (1002,0) ≠ 末样本 (1001,0)：末段 1px/10ms = 100px/s → 不 Throw；
        // 3 点近共线 → 直线彩蛋。
        let out = m.feed(InputEvent::Release { x: 1_002, y: 0 }, 510);
        assert!(!kinds(&out).contains(&InteractionKind::Throw), "末段慢速不甩出");
    }
}
