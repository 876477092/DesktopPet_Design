//! 动作优先级仲裁器（S2-M3，T-06）。
//!
//! 职责（03 卡片 S2-M3）：实现 `01 §6.3.1` 全部动作优先级仲裁规则——
//!   - **优先级 1~10**（10 最高）：差 ≥2 立即打断；差 1 等当前循环段结束
//!     （[`ActionArbiter::on_loop_segment_end`] 解除点）；低于/等于当前优先级
//!     进队列（上限 [`QUEUE_CAP`] = 3，溢出丢弃不崩溃）；
//!   - **同优先级**：「先到先播 + 打断冷却 500ms」排队（FCFS，按提交序）；
//!   - **打断冷却** [`INTERRUPT_COOLDOWN_MS`] = 500ms：打断型切换（含差 ≥2 立即
//!     打断与差 1 循环段解除）都记录冷却锚点，窗口内的合格请求进队列等待
//!     [`ActionArbiter::poll`] 活性提升；
//!   - **R-A 演出类不可打断**（`02 §5.11`）：`performance=true`（ACT-N-02/07/09/
//!     10/12/14，S8-M4 起 N-09/10/12/14 随批次 C 启用）→ 运行期
//!     `min_interrupt_priority = 255`；不可打断的 current 对任何请求返回
//!     [`Arbitration::Suppressed`]（不进队列）；
//!   - **R-B 情绪压制求助类**（`01 §6.3.1` v1.1）：current 优先级 ≥ 请求的
//!     `suppressed_by_emotion_priority`（N-01/05/06 = 7）→ 直接 [`Arbitration::Dropped`]
//!     （她生气时不会撒娇讨食）；
//!   - **R-C 求助退避**（`02 §5.11`）：[`HelpBackoff`] 纯计算——60s 无响应 →
//!     下次触发间隔 ×backoffMul，封顶 maxIntervalSec；响应后重置 baseIntervalSec。
//!     R-C 驱动循环（60s 无响应计时）归 S7-M3，本结构只做纯计算。
//!
//! 强制下发：情绪动作（生气/哭）由情绪状态机经 [`ActionArbiter::force_submit`]
//! 下发（优先级 ≥ 7 的 Emotion 请求）——**仅免打断冷却**，其余规则
//! （含不可打断 current → `Suppressed`）不变。
//!
//! 时间口径（C3 红线）：仲裁器**内部零时钟**——所有时间均为调用方传入的
//! 单调毫秒 `now_ms: u64`（调用方 tick 循环持 `std::time::Instant` 锚定），
//! 严禁 `SystemTime`/内部 `Instant`，测试完全确定性。
//!
//! 边界登记：
//!   - N 系列不占用彩蛋队列（彩蛋队列属 S8 另一套机制，此处不实现仅登记）；
//!   - 演出期间暂停 P 累积：[`ActionArbiter::is_performing`] 为 S7-M3
//!     `TickEnv.performing` 的接口预留，本阶段只暴露查询；
//!   - `fade_ms` 载荷冻结（v1 前端 FrameRenderer 固定 150ms 线性交叉淡入），
//!     本字段取 actions.json `fadeMs`，为 S9-M3 缓动升级预留；
//!   - `loop_range` 为预留字段，循环段计时由调用方（播放器）持有。

use std::collections::VecDeque;

use crate::config::ActionCfg;
use crate::config::model::HelpRequestCfg;

/// 动作 ID（RV-12：`ACT-<类>-<两位序号>`，如 `"ACT-T-04"`，`02 §4.3` 冻结词汇）。
pub type ActionId = String;

/// 请求来源（`02 §4.3` 冻结词汇：Emotion | Interaction | Motion | Activity | Ambient）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActionSource {
    /// 情绪状态机（生气/哭等，优先级 ≥ 7，`force_submit` 强制下发）。
    Emotion,
    /// 用户交互（抚摸 8 / 点击等，优先级 5~8）。
    Interaction,
    /// 漫游移动（行走/跑步/跳跃）。
    Motion,
    /// 活动系统（打工/旅游/学习等）。
    Activity,
    /// 环境氛围（感知类/彩蛋类）。
    Ambient,
}

/// 演出类动作的 `min_interrupt_priority` 归一化值（R-A：等效「不可打断」）。
const PERFORMANCE_MIN_INTERRUPT: u8 = 255;

/// 动作播放请求（`02 §4.3` 冻结字段 + `02 §5.11` R-A/R-B 元数据）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionRequest {
    /// 动作 ID（`ACT-*`）。
    pub id: ActionId,
    /// 优先级（1~10，10 最高；`01 §6.3.1`）。
    pub priority: u8,
    /// 是否可被更高优先级打断。
    pub interruptible: bool,
    /// 可打断本动作的最小优先级（0 = 不适用；演出类运行期归一化为 255，R-A）。
    pub min_interrupt_priority: u8,
    /// 演出类（R-A：ACT-N-02/07/09/10/12/14；期间暂停 P 累积，见
    /// [`ActionArbiter::is_performing`]）。
    pub performance: bool,
    /// 打断交叉淡入时长（毫秒；取 actions.json `fadeMs`。v1 前端固定 150ms，
    /// 载荷冻结，此字段为 S9-M3 缓动升级预留）。
    pub fade_ms: u32,
    /// 是否循环动作。
    pub looping: bool,
    /// 循环帧区间（预留字段：循环段计时由调用方（播放器）持有）。
    pub loop_range: Option<(u32, u32)>,
    /// 请求来源。
    pub source: ActionSource,
    /// 压制本求助类动作的情绪优先级下限（0 = 不适用；N-01/05/06 = 7，R-B）。
    pub suppressed_by_emotion_priority: u32,
}

impl ActionRequest {
    /// 从动作元数据构造请求（来源由调用方给定）。
    ///
    /// S2-M2 移交的「跳过 disabled」决策落点在此：`disabled=true` → `None`。
    /// 防御性收口（不 panic，`02 §7.4`）：
    ///   - `priority` 不在 1..=10 → `None`（`01 §6.3.1` 域外拒绝）；
    ///   - `performance=true` → `min_interrupt_priority` 归一化为 255（R-A，
    ///     `02 §5.11`，覆盖配置列任何取值）；
    ///   - cfg 的 `u32`/`u64` 字段收口到 `u8`/`u32`：`min_interrupt_priority`
    ///     上限 255、`fade_ms` 上限 `u32::MAX`（越界钳制而非丢弃）。
    #[must_use]
    pub fn from_cfg(cfg: &ActionCfg, source: ActionSource) -> Option<Self> {
        if cfg.disabled {
            return None;
        }
        if !(1..=10).contains(&cfg.priority) {
            return None;
        }
        let min_interrupt_priority = if cfg.performance {
            PERFORMANCE_MIN_INTERRUPT
        } else {
            u8::try_from(cfg.min_interrupt_priority).unwrap_or(PERFORMANCE_MIN_INTERRUPT)
        };
        Some(Self {
            id: cfg.id.clone(),
            priority: cfg.priority as u8,
            interruptible: cfg.interruptible,
            min_interrupt_priority,
            performance: cfg.performance,
            fade_ms: u32::try_from(cfg.fade_ms).unwrap_or(u32::MAX),
            looping: cfg.looping,
            loop_range: cfg.loop_range.map(|[start, end]| (start, end)),
            source,
            suppressed_by_emotion_priority: cfg.suppressed_by_emotion_priority,
        })
    }
}

/// 仲裁结果（`02 §4.3` 冻结词汇表）。
///
/// 不加 `#[non_exhaustive]`：结果集为 `02 §4.3` 冻结的封闭词汇表（与
/// [`crate::anim::player::FpsTier`] 等设计文档封闭枚举同口径），外部穷举匹配
/// 是合法用法；后续若扩充需走设计文档变更流程而非静默新增。
#[must_use = "仲裁结果必须被调用方消费（起播/入队/丢弃由调用方执行）"]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arbitration {
    /// 全新起播（无 current 时；fade 0，不设冷却）。
    Play,
    /// 立即打断（差 ≥2 或循环段解除），交叉淡入 `fade_ms` 毫秒。
    Interrupt {
        /// 交叉淡入时长（毫秒，取请求 `fade_ms`）。
        fade_ms: u32,
    },
    /// 进入等待队列，`pos` 为按「优先级降序 + 提交序升序」排列后的 1 基位置。
    Queued {
        /// 队列位置（1 基）。
        pos: u8,
    },
    /// 差 1：存入队列，等当前循环段结束（`on_loop_segment_end`）后切换。
    DeferToLoopEnd,
    /// 丢弃（R-B 情绪压制 / 队列溢出 / 域外优先级）。
    Dropped,
    /// 被当前动作压制（R-A：current 不可打断/演出类），请求不进队列。
    Suppressed {
        /// 压制者（current）的优先级。
        by_priority: u8,
    },
}

/// 当前播放中的动作（[`ActionArbiter::current`] 只读暴露）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveAction {
    /// 播放中的请求。
    pub request: ActionRequest,
    /// 起播时刻（调用方单调毫秒锚点）。
    pub started_ms: u64,
}

/// 仲裁器产出的起播指令（打断型切换携带交叉淡入时长）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Started {
    /// 起播的请求。
    pub request: ActionRequest,
    /// 交叉淡入时长：`0` = 全新起播；`> 0` = 交叉淡入打断。
    pub fade_ms: u32,
}

/// 队列容量（`01 §6.3.1`：上限 3，溢出丢弃）。
pub const QUEUE_CAP: usize = 3;

/// 打断冷却（`01 §6.3.1`：同优先级排队 + 打断冷却 500ms；对差 ≥2 打断同样生效）。
pub const INTERRUPT_COOLDOWN_MS: u64 = 500;

/// 队列中的等待项（`seq` 为提交序，FCFS 依据）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    request: ActionRequest,
    seq: u64,
}

/// 动作优先级仲裁器（状态机）。
///
/// 决策顺序（[`ActionArbiter::submit`]，文档即规格，`01 §6.3.1`）：
///   1. 请求优先级不在 1..=10 → `Dropped`（防御；正常路径 `from_cfg` 已拦）；
///   2. **R-A/不可打断**：current 存在且不可打断（`interruptible=false`，含
///      演出类 `performance=true`）→ `Suppressed { by_priority }`，不进队列；
///   3. **R-B 情绪压制求助类**：请求带压制下限且 current 优先级达标 → `Dropped`；
///   4. 无 current → 置为 current，`Play`（fade 0，不设冷却）；
///   5. 差 ≥2 且 current 可打断且过打断门槛且不在 500ms 冷却内 → 立即打断
///      （`Interrupt { fade_ms }`，记录冷却锚点）；
///   6. 差 1 → `DeferToLoopEnd`（存入队列，等当前循环段结束；队列满 → `Dropped`）；
///   7. 其余（低于/等于当前优先级、差 ≥2 但被冷却/门槛拦下）→ 进队列
///      （上限 3，溢出 `Dropped` 不崩溃），返回 `Queued { pos }`。
#[derive(Clone, Debug, Default)]
pub struct ActionArbiter {
    /// 当前播放中的动作。
    current: Option<ActiveAction>,
    /// 等待队列（保持「优先级降序 + 提交序升序」有序）。
    queue: VecDeque<Pending>,
    /// 下一个提交序号（FCFS 依据）。
    next_seq: u64,
    /// 差 1 等待循环段结束的提交序号（None = 无；随 current 消亡而失效）。
    deferred_seq: Option<u64>,
    /// 最近一次打断型切换的冷却锚点（单调毫秒）。
    last_interrupt_ms: Option<u64>,
}

impl ActionArbiter {
    /// 构建空仲裁器。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // -- 提交入口 -----------------------------------------------------------

    /// 提交动作请求（决策顺序见类型文档）。
    pub fn submit(&mut self, request: ActionRequest, now_ms: u64) -> Arbitration {
        self.submit_inner(request, now_ms, false)
    }

    /// 情绪状态机强制下发（优先级 ≥ 7 的 Emotion 请求）。
    ///
    /// **仅免打断冷却**，其余规则不变：面对不可打断/演出类 current 仍
    /// `Suppressed`，R-B 压制仍 `Dropped`，队列上限仍生效。
    pub fn force_submit(&mut self, request: ActionRequest, now_ms: u64) -> Arbitration {
        self.submit_inner(request, now_ms, true)
    }

    /// [`ActionArbiter::submit`] / [`ActionArbiter::force_submit`] 共用决策体。
    fn submit_inner(
        &mut self,
        request: ActionRequest,
        now_ms: u64,
        ignore_cooldown: bool,
    ) -> Arbitration {
        // 1. 优先级域防御（正常路径 from_cfg 已拦，不 panic）。
        if !(1..=10).contains(&request.priority) {
            return Arbitration::Dropped;
        }
        // 2. R-A/不可打断 current：任何请求被压制，不进队列。
        //    演出类即便配置异常（interruptible=true）也按 R-A 不可打断处理
        //    （`02 §5.11`：performance=true → 不可打断）。
        if let Some(cur) = self.current.as_ref() {
            if !cur.request.interruptible || cur.request.performance {
                return Arbitration::Suppressed { by_priority: cur.request.priority };
            }
        }
        // 3. R-B 情绪压制求助类：current 优先级 ≥ 请求的压制下限 → 丢弃
        //    （她生气时不会撒娇讨食，`01 §6.3.1` v1.1）。
        if let Some(cur) = self.current.as_ref() {
            if request.suppressed_by_emotion_priority > 0
                && u32::from(cur.request.priority) >= request.suppressed_by_emotion_priority
            {
                return Arbitration::Dropped;
            }
        }
        // 4. 无 current → 全新起播（fade 0，不设冷却）。
        let Some(cur) = self.current.as_ref() else {
            self.start_fresh(request, now_ms);
            return Arbitration::Play;
        };
        // 5. 差 ≥2 立即打断：可打断 + 过打断门槛 + 不在冷却内。
        if request.priority >= cur.request.priority.saturating_add(2)
            && cur.request.interruptible
            && request.priority >= cur.request.min_interrupt_priority
            && (ignore_cooldown || !self.in_cooldown(now_ms))
        {
            let fade_ms = request.fade_ms;
            self.promote_as_interrupt(request, now_ms);
            return Arbitration::Interrupt { fade_ms };
        }
        // 6. 差 1 → 等当前循环段结束（存入队列；队列满在 enqueue 内收口为 Dropped）。
        if request.priority == cur.request.priority.saturating_add(1) {
            return self.enqueue(request, true);
        }
        // 7. 其余（低于/等于当前优先级、差 ≥2 但被冷却/门槛拦下）→ 进队列。
        self.enqueue(request, false)
    }

    // -- 播放生命周期回调 -----------------------------------------------------

    /// 当前循环段结束（差 1 等待解除点，`01 §6.3.1`）。
    ///
    /// 从队列取最优合格者（优先级最高、同分先到）：满足「候选优先级 ≥
    /// current + 1 且 current 可打断且 ≥ current 打断门槛」→ 打断型切换
    /// （设冷却）；否则 `None`（调用方继续当前循环段）。
    #[must_use = "切换指令需由调用方起播"]
    pub fn on_loop_segment_end(&mut self, now_ms: u64) -> Option<Started> {
        let cur = self.current.as_ref()?;
        if !cur.request.interruptible {
            return None;
        }
        let min_priority = cur.request.min_interrupt_priority;
        let next_priority = cur.request.priority.saturating_add(1);
        // 队列已有序：取首个（优先级最高、同分先到）合格者。
        let idx = self.queue.iter().position(|p| {
            p.request.priority >= next_priority && p.request.priority >= min_priority
        })?;
        let pending = self.queue.remove(idx)?;
        self.sync_deferred();
        Some(self.promote_as_interrupt(pending.request, now_ms))
    }

    /// 非循环动作播完（`02 §5.11` 非循环语义的仲裁侧回调）。
    ///
    /// 清空 current，队列最优者直接起播（fade 0，不设冷却）；队列空返回
    /// `None`（调用方回落轮播，属 S3 集成）。
    #[must_use = "起播指令需由调用方执行"]
    pub fn on_action_finished(&mut self, now_ms: u64) -> Option<Started> {
        self.current.take()?;
        // defer 语义随 current 消亡（新 current 起播后重新按差 1 评估）。
        self.deferred_seq = None;
        let pending = self.queue.pop_front()?;
        let request = pending.request;
        self.start_fresh(request.clone(), now_ms);
        Some(Started { request, fade_ms: 0 })
    }

    /// 每逻辑 tick 活性保障（冷却过期后提升队列中的合格打断者）。
    ///
    /// 冷却已过且队列满足「差 ≥2 全部门槛」的最优者 → 打断提升（设冷却）；
    /// 否则 `None`。
    #[must_use = "提升指令需由调用方执行"]
    pub fn poll(&mut self, now_ms: u64) -> Option<Started> {
        if self.in_cooldown(now_ms) {
            return None;
        }
        let cur = self.current.as_ref()?;
        if !cur.request.interruptible {
            return None;
        }
        let min_priority = cur.request.min_interrupt_priority;
        let gap_two = cur.request.priority.saturating_add(2);
        // 队列已有序：差 ≥2 者必排在差 1 者之前，首个命中即最优。
        let idx = self.queue.iter().position(|p| {
            p.request.priority >= gap_two && p.request.priority >= min_priority
        })?;
        let pending = self.queue.remove(idx)?;
        self.sync_deferred();
        Some(self.promote_as_interrupt(pending.request, now_ms))
    }

    // -- 只读查询 -------------------------------------------------------------

    /// 当前播放中的动作（只读）。
    #[must_use]
    pub fn current(&self) -> Option<&ActiveAction> {
        self.current.as_ref()
    }

    /// 当前在播或队列中是否存在指定动作 ID（调用方「每拍循环演出」幂等守卫，
    /// S8-M4：学习桌面循环 `ACT-N-11` 防重复提交堆积）。
    #[must_use]
    pub fn has_action(&self, id: &str) -> bool {
        if self.current.as_ref().is_some_and(|a| a.request.id == id) {
            return true;
        }
        self.queue.iter().any(|p| p.request.id == id)
    }

    /// 是否正处于演出类动作播放中（R-A：演出期间暂停 P 累积）。
    ///
    /// 接口预留：S7-M3 接入 `TickEnv.performing`，本阶段只暴露查询。
    #[must_use]
    pub fn is_performing(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|a| a.request.performance)
    }

    /// 等待队列长度。
    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// 等待队列快照（调试视图；保持「优先级降序 + 提交序升序」队列序）。
    #[must_use]
    pub fn queue_snapshot(&self) -> Vec<&ActionRequest> {
        self.queue.iter().map(|p| &p.request).collect()
    }

    // -- 内部实现 -------------------------------------------------------------

    /// 是否处于打断冷却窗口内（C3：只用调用方传入的 `now_ms`）。
    fn in_cooldown(&self, now_ms: u64) -> bool {
        self.last_interrupt_ms
            .is_some_and(|t| now_ms < t.saturating_add(INTERRUPT_COOLDOWN_MS))
    }

    /// 全新起播（不设冷却锚点）。
    fn start_fresh(&mut self, request: ActionRequest, now_ms: u64) {
        self.current = Some(ActiveAction { request, started_ms: now_ms });
    }

    /// 打断型切换：换 current、记录冷却锚点，返回带交叉淡入的起播指令。
    fn promote_as_interrupt(&mut self, request: ActionRequest, now_ms: u64) -> Started {
        let fade_ms = request.fade_ms;
        self.current = Some(ActiveAction { request: request.clone(), started_ms: now_ms });
        self.last_interrupt_ms = Some(now_ms);
        Started { request, fade_ms }
    }

    /// 请求入队：保持「优先级降序 + 提交序升序」有序，超容溢出丢弃队尾
    /// （最低优先级/最新提交）不崩溃；返回 [`Arbitration::Queued`]（1 基位置）
    /// 或溢出/自弃时的 [`Arbitration::Dropped`]。
    ///
    /// `defer = true` 表示该请求以差 1 语义等待循环段结束
    /// （[`Arbitration::DeferToLoopEnd`]）。
    fn enqueue(&mut self, request: ActionRequest, defer: bool) -> Arbitration {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.push_back(Pending { request, seq });
        self.queue
            .make_contiguous()
            .sort_by(|a, b| b.request.priority.cmp(&a.request.priority).then(a.seq.cmp(&b.seq)));
        // 溢出丢弃（`01 §6.3.1`：上限 3，溢出丢弃；队尾 = 最低优先级/最新提交）。
        while self.queue.len() > QUEUE_CAP {
            let _ = self.queue.pop_back();
        }
        self.sync_deferred();
        match self.queue.iter().position(|p| p.seq == seq) {
            // 差 1 语义：以 DeferToLoopEnd 表达「等当前循环段结束」（`01 §6.3.1`）。
            Some(_) if defer => {
                self.deferred_seq = Some(seq);
                Arbitration::DeferToLoopEnd
            }
            // 队列上限 3 + 溢出前至多 4 项，1 基位置恒在 u8 域内。
            Some(idx) => Arbitration::Queued { pos: idx as u8 + 1 },
            // 自己被溢出丢弃。
            None => Arbitration::Dropped,
        }
    }

    /// defer 记账防御：被记账的等待项若已不在队列（理论不可达，防御收口），
    /// 清除记账。
    fn sync_deferred(&mut self) {
        if let Some(seq) = self.deferred_seq {
            if !self.queue.iter().any(|p| p.seq == seq) {
                self.deferred_seq = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// R-C 求助退避（预留实现，S7-M3 接入）
// ---------------------------------------------------------------------------

/// 求助退避计算器（R-C，`02 §5.11`：60s 无响应 → 下次触发间隔 ×backoffMul，
/// 封顶 maxIntervalSec；点「去喂食」立即重置 baseIntervalSec）。
///
/// **纯计算**，无内部时钟（C3）：R-C 驱动循环（60s 无响应计时、触发调度）
/// 归 S7-M3，本结构只提供 `record_*` 计数与 [`HelpBackoff::next_interval_sec`]
/// 换算。
#[derive(Clone, Debug, PartialEq)]
pub struct HelpBackoff {
    /// 基础求助间隔（秒）。
    base_interval_sec: u64,
    /// 退避乘子（f32；防御收口：< 1.0 或非有限值按 1.0）。
    backoff_mul: f32,
    /// 最大求助间隔（秒；防御收口：< base 按 base）。
    max_interval_sec: u64,
    /// 连续无响应次数（k：间隔 = base × mul^k）。
    no_response_count: u32,
}

impl HelpBackoff {
    /// 从求助请求参数构建（`actions.json` `helpRequest` 基本量）。
    #[must_use]
    pub fn new(cfg: &HelpRequestCfg) -> Self {
        let base = cfg.base_interval_sec.max(1);
        let mul = if cfg.backoff_mul.is_finite() && cfg.backoff_mul >= 1.0 {
            cfg.backoff_mul
        } else {
            1.0
        };
        Self {
            base_interval_sec: base,
            backoff_mul: mul,
            max_interval_sec: cfg.max_interval_sec.max(base),
            no_response_count: 0,
        }
    }

    /// 记录一次无响应：连续无响应计数 +1（下次间隔乘 backoff_mul）。
    pub fn record_no_response(&mut self) {
        self.no_response_count = self.no_response_count.saturating_add(1);
    }

    /// 记录用户响应（如点「去喂食」）：重置为 baseIntervalSec（计数归零）。
    pub fn record_responded(&mut self) {
        self.no_response_count = 0;
    }

    /// 下一次求助触发间隔（秒）：`base × backoff_mul^k` 封顶
    /// `max_interval_sec`，向上取整。
    ///
    /// f32 乘方用循环乘法（避免 `powi` 跨平台精度差异）；达到封顶后提前返回。
    #[must_use]
    pub fn next_interval_sec(&self) -> u64 {
        let base = self.base_interval_sec;
        let max = self.max_interval_sec;
        if base >= max {
            return max;
        }
        let max_f32 = max as f32;
        let mut acc = base as f32;
        for _ in 0..self.no_response_count {
            acc *= self.backoff_mul;
            if acc >= max_f32 {
                return max;
            }
        }
        (acc.ceil() as u64).min(max)
    }

    /// 当前连续无响应次数（调试/日志视图）。
    #[must_use]
    pub const fn no_response_count(&self) -> u32 {
        self.no_response_count
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 请求快捷工厂（默认：可打断、门槛 0（不适用）、非演出、fade 200ms、
    /// 循环 [0,7]、Interaction 来源、无 R-B 压制下限）。
    fn req(id: &str, priority: u8) -> ActionRequest {
        ActionRequest {
            id: id.to_string(),
            priority,
            interruptible: true,
            min_interrupt_priority: 0,
            performance: false,
            fade_ms: 200,
            looping: true,
            loop_range: Some((0, 7)),
            source: ActionSource::Interaction,
            suppressed_by_emotion_priority: 0,
        }
    }

    /// S8-M4：`has_action` 查询（在播 / 排队幂等守卫）。
    #[test]
    fn has_action_covers_current_and_queue() {
        let mut arb = ActionArbiter::new();
        assert!(!arb.has_action("ACT-N-11"), "空仲裁器不命中");
        // 无 current → 直接起播 → 在播命中。
        let verdict = arb.submit(req("ACT-N-11", 6), 0);
        assert!(matches!(verdict, Arbitration::Play));
        assert!(arb.has_action("ACT-N-11"));
        // 更高优先级打断（差 ≥2，可打断）→ 打断者起播、被替换者**丢弃**（仲裁语义：
        // 打断不保留被替换者；学习循环经 submit 重新提交 + 入队恢复，不依赖保留）。
        let intr = ActionRequest {
            id: "ACT-N-09".to_string(),
            priority: 9,
            interruptible: true,
            min_interrupt_priority: 0,
            performance: false,
            fade_ms: 200,
            looping: false,
            loop_range: None,
            source: ActionSource::Activity,
            suppressed_by_emotion_priority: 0,
        };
        let v2 = arb.submit(intr, 0);
        assert!(matches!(v2, Arbitration::Interrupt { .. }));
        assert!(arb.has_action("ACT-N-09"), "打断者在播应命中");
        assert!(!arb.has_action("ACT-N-11"), "被替换者按打断语义丢弃");
        // 低优先级请求（差 <2）→ 入队 → 排队项命中（学习循环幂等守卫覆盖队列）。
        let low = ActionRequest {
            id: "ACT-N-04".to_string(),
            priority: 4,
            interruptible: true,
            min_interrupt_priority: 0,
            performance: false,
            fade_ms: 200,
            looping: false,
            loop_range: None,
            source: ActionSource::Ambient,
            suppressed_by_emotion_priority: 0,
        };
        let v3 = arb.submit(low, 0);
        assert!(matches!(v3, Arbitration::Queued { .. } | Arbitration::Dropped));
        assert!(arb.has_action("ACT-N-04"), "排队项应命中");
        assert!(!arb.has_action("ACT-N-13"), "未出现动作不命中");
    }

    /// 工程根：dp-core 位于 crates/dp-core，上溯三级（无盘符字面量，C1）。
    fn repo_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../")
    }

    fn resources_config_dir() -> std::path::PathBuf {
        repo_root().join("resources").join("config")
    }

    // -- 规则 1：差 ≥2 立即打断（fade 回显 + 冷却锚点） ----------------------

    #[test]
    fn gap_two_interrupts_immediately_with_fade_and_cooldown() {
        let mut arb = ActionArbiter::new();
        // 空仲裁器首提交 → Play。
        assert_eq!(arb.submit(req("ACT-M-01", 1), 0), Arbitration::Play);
        assert_eq!(arb.current().map(|a| a.request.id.as_str()), Some("ACT-M-01"));

        // 差 ≥2（3 → 5）立即打断，fade_ms 回显请求的 fadeMs。
        let verdict = ArbProbe::submit(&mut arb, req("ACT-I-01", 5), 100);
        assert_eq!(
            verdict,
            Arbitration::Interrupt { fade_ms: 200 },
            "差 ≥2 应立即打断且回显 fade_ms"
        );
        assert_eq!(arb.current().map(|a| a.request.id.as_str()), Some("ACT-I-01"));

        // 冷却锚点被记录：500ms 窗口内的差 ≥2 合格请求（5 → 7）只能排队。
        let in_window = ArbProbe::submit(&mut arb, req("ACT-I-02", 7), 300);
        assert!(matches!(in_window, Arbitration::Queued { pos: 1 }), "{in_window:?}");
        assert_eq!(arb.queue_len(), 1, "冷却窗口内不打断，进队列");
    }

    // -- 规则 2：差 1 → DeferToLoopEnd，循环段结束后 fade 切换并设冷却 --------

    #[test]
    fn gap_one_defers_to_loop_segment_end_then_switches_with_fade() {
        let mut arb = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb, req("ACT-M-03", 4), 0);
        // 差 1（4 → 5）→ 等循环段结束。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-M-04", 5), 10), Arbitration::DeferToLoopEnd);
        assert_eq!(arb.queue_len(), 1, "差 1 请求入队等待");
        assert_eq!(
            arb.current().map(|a| a.request.id.as_str()),
            Some("ACT-M-03"),
            "循环段未结束不切换"
        );

        // 循环段结束 → 打断型切换，fade 回显。
        let started = arb.on_loop_segment_end(100).expect("差 1 等待应解除");
        assert_eq!(started.request.id, "ACT-M-04");
        assert_eq!(started.fade_ms, 200, "打断型切换携带交叉淡入");
        assert_eq!(arb.current().map(|a| a.request.id.as_str()), Some("ACT-M-04"));

        // 切换设冷却：窗口内差 ≥2 请求排队而非打断。
        let in_window = ArbProbe::submit(&mut arb, req("ACT-I-01", 7), 200);
        assert!(matches!(in_window, Arbitration::Queued { pos: 1 }), "{in_window:?}");
    }

    // -- 规则 3：同优先级排队 FCFS -------------------------------------------

    #[test]
    fn equal_priority_queues_and_drains_in_fcfs_order() {
        let mut arb = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb, req("ACT-M-02", 3), 0);
        // 同优先级 → 队列 1 基位置。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-I-01", 3), 1), Arbitration::Queued { pos: 1 });
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-I-02", 3), 2), Arbitration::Queued { pos: 2 });

        // 播完 → 队列最优者起播：同分按提交序（FCFS），fade 0。
        let first = arb.on_action_finished(1_000).expect("队列非空应起播");
        assert_eq!(first.request.id, "ACT-I-01", "同优先级先到先播");
        assert_eq!(first.fade_ms, 0, "播完起播为全新起播（fade 0）");
        let second = arb.on_action_finished(2_000).expect("队列非空应起播");
        assert_eq!(second.request.id, "ACT-I-02");
    }

    // -- 规则 4：低于当前排队 + 队列上限 3 溢出丢弃不崩溃 ----------------------

    #[test]
    fn lower_priority_queues_and_cap_three_overflows_dropped() {
        let mut arb = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb, req("ACT-M-04", 5), 0);
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-A-01", 4), 1), Arbitration::Queued { pos: 1 });
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-A-02", 3), 2), Arbitration::Queued { pos: 2 });
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-A-03", 2), 3), Arbitration::Queued { pos: 3 });
        assert_eq!(arb.queue_len(), QUEUE_CAP);

        // 第 4 个溢出 → Dropped，且状态不损坏。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-A-04", 1), 4), Arbitration::Dropped);
        assert_eq!(arb.queue_len(), 3, "溢出丢弃后队列容量不变");

        // 原 3 条按序保留，后续 on_action_finished 正常排空。
        let ids: Vec<&str> = arb.queue_snapshot().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["ACT-A-01", "ACT-A-02", "ACT-A-03"]);
        for expected in ["ACT-A-01", "ACT-A-02", "ACT-A-03"] {
            let started = arb.on_action_finished(10_000).expect("队列应逐条起播");
            assert_eq!(started.request.id, expected);
        }
        assert!(arb.on_action_finished(11_000).is_none(), "队列空应返回 None");
    }

    // -- 规则 5：500ms 冷却 → poll 活性提升 ----------------------------------

    #[test]
    fn cooldown_blocks_interrupt_then_poll_promotes_after_expiry() {
        let mut arb = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb, req("ACT-M-02", 3), 0);
        // 打断（3 → 5），锚点 t=10。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-M-04", 5), 10), Arbitration::Interrupt { fade_ms: 200 });

        // 冷却窗口内（10+500=510）差 ≥2 合格请求（5 → 7）排队。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-I-01", 7), 300), Arbitration::Queued { pos: 1 });
        // 窗口边界前 poll 不提升。
        assert!(arb.poll(500).is_none(), "冷却未过期 poll 不提升");
        // 窗口过期后 poll 提升打断。
        let started = arb.poll(520).expect("冷却过期应提升打断");
        assert_eq!(started.request.id, "ACT-I-01");
        assert_eq!(started.fade_ms, 200);
        assert_eq!(arb.current().map(|a| a.request.id.as_str()), Some("ACT-I-01"));
        assert_eq!(arb.queue_len(), 0);
        // poll 提升为打断型：新锚点 t=520 生效。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-S-01", 9), 530), Arbitration::Queued { pos: 1 });
    }

    // -- 规则 6：不可打断 / 演出类 current → Suppressed -----------------------

    #[test]
    fn non_interruptible_and_performance_current_suppress_all() {
        let mut arb = ActionArbiter::new();
        // interruptible=false 的 current（如 ACT-M-04 跳跃）。
        let mut jump = req("ACT-M-04", 5);
        jump.interruptible = false;
        let _ = ArbProbe::submit(&mut arb, jump, 0);
        let verdict = ArbProbe::submit(&mut arb, req("ACT-S-01", 10), 10);
        assert_eq!(verdict, Arbitration::Suppressed { by_priority: 5 }, "不可打断 current 压制任何请求");
        assert_eq!(arb.queue_len(), 0, "被压制请求不进队列");

        // 演出类 current（performance=true 即便 interruptible=true 也按 R-A 不可打断）。
        let mut arb2 = ActionArbiter::new();
        let mut bath = req("ACT-N-07", 8);
        bath.interruptible = true;
        bath.performance = true;
        let _ = ArbProbe::submit(&mut arb2, bath, 0);
        assert!(arb2.is_performing(), "演出期间 is_performing=true");
        assert_eq!(
            ArbProbe::submit(&mut arb2, req("ACT-I-01", 10), 10),
            Arbitration::Suppressed { by_priority: 8 },
            "演出类 current 压制任何请求（R-A）"
        );

        // from_cfg 对 performance 归一化 min_interrupt_priority=255（ACT-N-02 口径）。
        let cfg = crate::config::ActionCfg {
            id: "ACT-N-02".into(),
            priority: 6,
            interruptible: false,
            performance: true,
            disabled: false,
            ..crate::config::ActionCfg::default()
        };
        let request = ActionRequest::from_cfg(&cfg, ActionSource::Activity).expect("合法配置应可转换");
        assert_eq!(request.min_interrupt_priority, 255, "R-A 归一化");
        assert!(request.performance);
    }

    // -- 规则 7：R-B 情绪压制求助类 -------------------------------------------

    #[test]
    fn emotion_current_drops_help_requests_by_r_b() {
        let mut arb = ActionArbiter::new();
        // 她生气（优先级 7，Emotion 强制下发）→ N-01 讨食（suppressedBy=7）被丢弃。
        let anger = req("ACT-E-01", 7);
        let _ = ArbProbe::submit(&mut arb, anger, 0);
        let mut beg = req("ACT-N-01", 5);
        beg.suppressed_by_emotion_priority = 7;
        assert_eq!(ArbProbe::submit(&mut arb, beg, 10), Arbitration::Dropped, "R-B：生气时不会撒娇讨食");
        assert_eq!(arb.queue_len(), 0, "被压制求助请求不进队列");

        // current 优先级 6 < 7 → 不压制，走正常仲裁（5 < 6 → 排队）。
        let mut arb2 = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb2, req("ACT-M-05", 6), 0);
        let mut beg2 = req("ACT-N-01", 5);
        beg2.suppressed_by_emotion_priority = 7;
        assert!(
            matches!(ArbProbe::submit(&mut arb2, beg2, 10), Arbitration::Queued { pos: 1 }),
            "current=6 未达压制下限 7，正常排队"
        );

        // 压制下限为 0（不适用）→ 永不触发 R-B。
        let mut arb3 = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb3, req("ACT-E-01", 10), 0);
        let plain = req("ACT-I-01", 3);
        assert!(
            matches!(ArbProbe::submit(&mut arb3, plain, 10), Arbitration::Queued { pos: 1 }),
            "suppressed_by=0 不适用 R-B"
        );
    }

    // -- from_cfg 校验与真实 actions.json 冒烟 --------------------------------

    #[test]
    fn from_cfg_rejects_disabled_and_out_of_range_priority() {
        let mut cfg = crate::config::ActionCfg {
            id: "ACT-T-04".into(),
            priority: 5,
            disabled: false,
            ..crate::config::ActionCfg::default()
        };
        assert!(ActionRequest::from_cfg(&cfg, ActionSource::Ambient).is_some());
        // disabled=true → None（S2-M2 移交决策落点）。
        cfg.disabled = true;
        assert!(ActionRequest::from_cfg(&cfg, ActionSource::Ambient).is_none(), "disabled 应被跳过");
        // priority 越界（0 / 11）→ None。
        cfg.disabled = false;
        cfg.priority = 0;
        assert!(ActionRequest::from_cfg(&cfg, ActionSource::Ambient).is_none());
        cfg.priority = 11;
        assert!(ActionRequest::from_cfg(&cfg, ActionSource::Ambient).is_none());
    }

    #[test]
    fn from_cfg_maps_fields_and_u32_to_u8_clamps() {
        let cfg = crate::config::ActionCfg {
            id: "ACT-T-04".into(),
            priority: 5,
            interruptible: true,
            min_interrupt_priority: 6,
            looping: false,
            loop_range: None,
            fps: 10,
            fade_ms: 250,
            suppressed_by_emotion_priority: 7,
            performance: false,
            disabled: false,
            ..crate::config::ActionCfg::default()
        };
        let request = ActionRequest::from_cfg(&cfg, ActionSource::Interaction).expect("合法配置");
        assert_eq!(request.id, "ACT-T-04");
        assert_eq!(request.priority, 5);
        assert!(request.interruptible);
        assert_eq!(request.min_interrupt_priority, 6);
        assert!(!request.looping);
        assert_eq!(request.loop_range, None);
        assert_eq!(request.fade_ms, 250);
        assert_eq!(request.suppressed_by_emotion_priority, 7);
        assert_eq!(request.source, ActionSource::Interaction);
        assert!(!request.performance);

        // u32 → u8 收口：minInterruptPriority 超界钳制 255（非演出）。
        let clamped = crate::config::ActionCfg {
            min_interrupt_priority: 1_000,
            ..cfg.clone()
        };
        let request = ActionRequest::from_cfg(&clamped, ActionSource::Interaction).expect("合法配置");
        assert_eq!(request.min_interrupt_priority, 255, "越界钳制而非丢弃");

        // fade_ms 超界钳制 u32::MAX。
        let huge_fade = crate::config::ActionCfg { fade_ms: u64::from(u32::MAX) + 1, ..cfg };
        let request = ActionRequest::from_cfg(&huge_fade, ActionSource::Interaction).expect("合法配置");
        assert_eq!(request.fade_ms, u32::MAX, "fade_ms 钳制 u32::MAX");
    }

    /// 真实 `actions.json` 冒烟：经 `ActionCatalog::load` 验证字段映射
    /// （ACT-M-01 priority 1 / minInterrupt 2；ACT-N-01 suppressedBy 7；
    /// ACT-N-02 performance → 255）。
    #[test]
    fn catalog_smoke_maps_arbiter_relevant_fields() {
        let catalog = crate::anim::ActionCatalog::load(&resources_config_dir())
            .expect("默认配置应可加载");

        // ACT-M-01 站立：priority 1、minInterrupt 2（01 §6.3.2 / 02 §5.11）。
        let m01 = catalog.find("ACT-M-01").expect("ACT-M-01 应在目录中");
        let request = ActionRequest::from_cfg(m01, ActionSource::Motion).expect("启用动作应可转换");
        assert_eq!(request.priority, 1);
        assert_eq!(request.min_interrupt_priority, 2);
        assert_eq!(request.fade_ms, 200);
        assert!(request.looping);
        assert_eq!(request.loop_range, Some((0, 3)));
        assert!(!request.performance);

        // ACT-N-01 讨食：suppressedBy=7（R-B），当前资源批次 disabled=true
        // → from_cfg 返回 None；解除 disabled 后字段映射可见。
        let n01 = catalog.find("ACT-N-01").expect("ACT-N-01 应在目录中");
        assert_eq!(n01.suppressed_by_emotion_priority, 7, "R-B 元数据在目录侧");
        assert_eq!(
            n01.help_request.as_ref().map(|h| h.base_interval_sec),
            Some(180),
            "R-C 元数据在目录侧"
        );
        assert!(n01.disabled, "批次 B 资源未交付");
        assert!(ActionRequest::from_cfg(n01, ActionSource::Ambient).is_none(), "disabled → None");
        let mut n01_enabled = n01.clone();
        n01_enabled.disabled = false;
        let request =
            ActionRequest::from_cfg(&n01_enabled, ActionSource::Ambient).expect("解除 disabled");
        assert_eq!(request.suppressed_by_emotion_priority, 7);
        assert_eq!(request.priority, 5);

        // ACT-N-02 吃饭：performance=true → minInterrupt 归一化 255（R-A）。
        let n02 = catalog.find("ACT-N-02").expect("ACT-N-02 应在目录中");
        assert!(n02.performance, "ACT-N-02 为演出类");
        let mut n02_enabled = n02.clone();
        n02_enabled.disabled = false;
        let request =
            ActionRequest::from_cfg(&n02_enabled, ActionSource::Activity).expect("解除 disabled");
        assert_eq!(request.min_interrupt_priority, 255, "演出类归一化");
        assert!(request.performance);
    }

    // -- on_action_finished --------------------------------------------------

    #[test]
    fn on_action_finished_clears_current_and_drains_queue_best() {
        let mut arb = ActionArbiter::new();
        // 队列空时播完 → None（调用方回落轮播，S3 集成）。
        let _ = ArbProbe::submit(&mut arb, req("ACT-I-01", 5), 0);
        assert!(arb.on_action_finished(100).is_none(), "队列空返回 None");
        assert!(arb.current().is_none(), "播完清空 current");

        // 队列最优者直接起播（fade 0，不设冷却）。
        let _ = ArbProbe::submit(&mut arb, req("ACT-M-02", 3), 200);
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-A-01", 2), 210), Arbitration::Queued { pos: 1 });
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-A-02", 3), 220), Arbitration::Queued { pos: 1 });
        let started = arb.on_action_finished(1_000).expect("队列非空应起播");
        assert_eq!(started.request.id, "ACT-A-02", "优先级 3 > 2，最优者先出");
        assert_eq!(started.fade_ms, 0, "播完起播 fade 0");
        // 播完起播不设冷却：立即的差 ≥2 请求可打断。
        assert_eq!(
            ArbProbe::submit(&mut arb, req("ACT-I-01", 5), 1_010),
            Arbitration::Interrupt { fade_ms: 200 }
        );
    }

    // -- 冷却边界与 u64 邻界溢出安全 -------------------------------------------

    #[test]
    fn cooldown_boundary_exactly_500ms_is_unsealed() {
        let mut arb = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb, req("ACT-M-01", 1), 0);
        // 打断设锚点 t=100，窗口 [100, 600)。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-M-03", 4), 100), Arbitration::Interrupt { fade_ms: 200 });
        // 窗口内最后 1ms（t=599）仍拦截。
        assert!(
            matches!(ArbProbe::submit(&mut arb, req("ACT-I-01", 8), 599), Arbitration::Queued { pos: 1 }),
            "t = anchor+499 仍在冷却窗口内"
        );
        // 边界 t=600（== anchor+500）已解封 → 差 ≥2 立即打断（闭式解封口径）。
        assert_eq!(
            ArbProbe::submit(&mut arb, req("ACT-I-02", 9), 600),
            Arbitration::Interrupt { fade_ms: 200 },
            "t == anchor + 500 应解封"
        );
    }

    #[test]
    fn cooldown_near_u64_max_saturating_add_does_not_wrap() {
        let mut arb = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb, req("ACT-M-01", 1), u64::MAX - 400);
        // 打断设锚点 t = MAX-300；saturating_add 使窗口右端收口在 MAX。
        assert_eq!(
            ArbProbe::submit(&mut arb, req("ACT-M-03", 4), u64::MAX - 300),
            Arbitration::Interrupt { fade_ms: 200 },
            "u64 邻界打断正常设锚点"
        );
        // t = MAX-100 仍在窗口 [MAX-300, MAX) 内：若加法回绕（+500 溢出为
        // 199），此请求会被误判为已解封而立即打断——断言排队即证不回绕。
        assert!(
            matches!(
                ArbProbe::submit(&mut arb, req("ACT-I-01", 8), u64::MAX - 100),
                Arbitration::Queued { pos: 1 }
            ),
            "u64 邻界冷却窗口应仍生效（saturating_add 防回绕）"
        );
    }

    // -- 溢出公平性：差 1 等待项被更高优先级挤掉时记账一致 ----------------------

    #[test]
    fn deferred_item_evicted_by_higher_priority_overflow_keeps_accounting_consistent() {
        let mut arb = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb, req("ACT-M-01", 1), 0);
        // 打断（1→3）设冷却锚点 t=10，冷却内构造溢出场景。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-M-02", 3), 10), Arbitration::Interrupt { fade_ms: 200 });
        // 差 1（3→4）→ DeferToLoopEnd 入队。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-M-03", 4), 20), Arbitration::DeferToLoopEnd);
        // 冷却内连续 3 个更高优先级（差 ≥2 被冷却拦下）入队 → 队列 [8,4] →
        // [9,8,4] → [10,9,8]，队尾的差 1 等待项被溢出挤掉（最低优先级口径）。
        assert!(matches!(ArbProbe::submit(&mut arb, req("ACT-I-01", 8), 30), Arbitration::Queued { .. }));
        assert!(matches!(ArbProbe::submit(&mut arb, req("ACT-I-02", 9), 40), Arbitration::Queued { .. }));
        assert!(matches!(ArbProbe::submit(&mut arb, req("ACT-I-03", 10), 50), Arbitration::Queued { pos: 1 }));
        let ids: Vec<&str> = arb.queue_snapshot().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["ACT-I-03", "ACT-I-02", "ACT-I-01"], "差 1 等待项被挤掉，高优先级按序保留");
        // 记账一致：解除点仍按「最优合格者」切换，defer 记账失效不留悬引用、不 panic。
        let started = arb.on_loop_segment_end(600).expect("队列有合格者应切换");
        assert_eq!(started.request.id, "ACT-I-03");
        assert_eq!(arb.queue_len(), 2, "被挤掉后剩余队列不受影响");
    }

    // -- 不可打断 current 的循环段解除点防御 ------------------------------------

    #[test]
    fn loop_segment_end_with_non_interruptible_current_returns_none() {
        let mut arb = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb, req("ACT-M-01", 1), 0);
        // 先积压两条低优先级等待项（p1 同级排队；p2 差 1 等循环段，均不入 current）。
        assert!(matches!(ArbProbe::submit(&mut arb, req("ACT-A-01", 1), 5), Arbitration::Queued { .. }));
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-A-02", 2), 6), Arbitration::DeferToLoopEnd);
        // 打断（1→3）为不可打断动作（如单次跳跃口径）。
        let mut stiff = req("ACT-M-04", 3);
        stiff.interruptible = false;
        assert_eq!(ArbProbe::submit(&mut arb, stiff, 10), Arbitration::Interrupt { fade_ms: 200 });
        // 循环段结束：不可打断 current 不切换、队列不被误清。
        assert!(arb.on_loop_segment_end(1_000).is_none(), "不可打断 current 循环段结束不切换");
        assert_eq!(arb.queue_len(), 2, "解除点不动队列");
    }

    // -- 差 1 defer 与打断门槛的解除点复核 ---------------------------------------

    #[test]
    fn gap_one_defer_respects_min_interrupt_gate_at_loop_end() {
        let mut arb = ActionArbiter::new();
        // current 优先级 4 且打断门槛 6（如 ACT-N-01 口径 5→6 的镜像场景）。
        let mut gated = req("ACT-N-01", 4);
        gated.min_interrupt_priority = 6;
        let _ = ArbProbe::submit(&mut arb, gated, 0);
        // 差 1（4→5）→ DeferToLoopEnd；解除点复核门槛：5 < 6 → 不切换。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-M-04", 5), 10), Arbitration::DeferToLoopEnd);
        assert!(arb.on_loop_segment_end(100).is_none(), "低于打断门槛的差 1 等待项在解除点不切换");
        assert_eq!(arb.queue_len(), 1, "门槛拦截的等待项保留在队列（不悬死）");
        // current 消亡后队列正常排空。
        let started = arb.on_action_finished(200).expect("播完后队列最优者起播");
        assert_eq!(started.request.id, "ACT-M-04");
        assert_eq!(started.fade_ms, 0);
    }

    // -- force_submit ----------------------------------------------------------

    #[test]
    fn force_submit_bypasses_cooldown_but_not_suppression() {
        let mut arb = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb, req("ACT-M-02", 3), 0);
        // 常规打断建立冷却锚点 t=10。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-M-04", 5), 10), Arbitration::Interrupt { fade_ms: 200 });
        // 冷却窗口内（t=100）常规提交排队，force_submit 直接打断。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-I-01", 7), 100), Arbitration::Queued { pos: 1 });
        let mut rage = req("ACT-E-01", 8);
        rage.source = ActionSource::Emotion;
        assert_eq!(
            ArbProbe::force(&mut arb, rage, 110),
            Arbitration::Interrupt { fade_ms: 200 },
            "force_submit 仅免冷却"
        );
        assert_eq!(arb.current().map(|a| a.request.id.as_str()), Some("ACT-E-01"));

        // 面对演出类 current：force_submit 仍被 Suppressed（其余规则不变）。
        let mut arb2 = ActionArbiter::new();
        let mut eat = req("ACT-N-02", 6);
        eat.performance = true;
        eat.interruptible = false;
        let _ = ArbProbe::submit(&mut arb2, eat, 0);
        let mut cry = req("ACT-E-02", 9);
        cry.source = ActionSource::Emotion;
        assert_eq!(
            ArbProbe::force(&mut arb2, cry, 100),
            Arbitration::Suppressed { by_priority: 6 },
            "force_submit 不突破 R-A"
        );
    }

    // -- 队列排序：高优先级先出 ------------------------------------------------

    #[test]
    fn queue_promotes_highest_priority_first_on_loop_end() {
        let mut arb = ActionArbiter::new();
        let _ = ArbProbe::submit(&mut arb, req("ACT-M-01", 2), 0);
        // 打断建立冷却（2 → 4，锚点 t=10），冷却窗口内构造乱序队列。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-M-03", 4), 10), Arbitration::Interrupt { fade_ms: 200 });
        // 低优先级先入队（低于 current → 排队）、高优先级后入队（差 1 → defer）。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-A-01", 3), 20), Arbitration::Queued { pos: 1 });
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-A-02", 5), 30), Arbitration::DeferToLoopEnd);
        let ids: Vec<&str> = arb.queue_snapshot().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["ACT-A-02", "ACT-A-01"], "队列按优先级降序排列");

        // 循环段结束：高优先级（后入队但分高）先出。
        let started = arb.on_loop_segment_end(700).expect("差 1 等待应解除");
        assert_eq!(started.request.id, "ACT-A-02", "高优先级先出");
        assert_eq!(started.fade_ms, 200, "打断型切换设冷却");
        // 再来一轮：current=5，差 1 请求入队后循环段结束继续提升。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-A-03", 6), 710), Arbitration::DeferToLoopEnd);
        let started = arb.on_loop_segment_end(1_400).expect("差 1 等待应解除");
        assert_eq!(started.request.id, "ACT-A-03");
        // 低优先级等待项仍在队列（未被两次提升误清）。
        let ids: Vec<&str> = arb.queue_snapshot().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["ACT-A-01"], "低优先级等待项保留");
    }

    // -- HelpBackoff（R-C）-----------------------------------------------------

    #[test]
    fn help_backoff_doubles_on_no_response_and_resets_on_response() {
        let mut backoff = HelpBackoff::new(&HelpRequestCfg {
            base_interval_sec: 180,
            backoff_mul: 2.0,
            max_interval_sec: 900,
            no_response_sec: 60,
            with_bubble: true,
        });
        assert_eq!(backoff.next_interval_sec(), 180, "初始为 base");

        // 无响应 ×2 退避：360 → 720 → 封顶 900。
        backoff.record_no_response();
        assert_eq!(backoff.next_interval_sec(), 360);
        backoff.record_no_response();
        assert_eq!(backoff.next_interval_sec(), 720);
        backoff.record_no_response();
        assert_eq!(backoff.next_interval_sec(), 900, "封顶 max_interval_sec");
        backoff.record_no_response();
        assert_eq!(backoff.next_interval_sec(), 900, "继续无响应仍封顶");
        assert_eq!(backoff.no_response_count(), 4);

        // 响应 → 重置 base（180）。
        backoff.record_responded();
        assert_eq!(backoff.next_interval_sec(), 180);
        assert_eq!(backoff.no_response_count(), 0);
    }

    #[test]
    fn help_backoff_ceil_for_fractional_mul_and_defends_degenerate_cfg() {
        // 非整数退避：180 × 1.3 = 233.999…（f32）→ 向上取整 234。
        let mut backoff = HelpBackoff::new(&HelpRequestCfg {
            base_interval_sec: 180,
            backoff_mul: 1.3,
            max_interval_sec: 900,
            no_response_sec: 60,
            with_bubble: true,
        });
        backoff.record_no_response();
        assert_eq!(backoff.next_interval_sec(), 234, "循环乘法 + 向上取整");

        // 退化配置防御：mul < 1 → 按 1.0（间隔不缩）；base ≥ max → 恒 max。
        let flat = HelpBackoff::new(&HelpRequestCfg {
            base_interval_sec: 300,
            backoff_mul: 0.5,
            max_interval_sec: 900,
            no_response_sec: 60,
            with_bubble: false,
        });
        assert_eq!(flat.next_interval_sec(), 300, "mul<1 防御按 1.0");
        let capped = HelpBackoff::new(&HelpRequestCfg {
            base_interval_sec: 900,
            backoff_mul: 2.0,
            max_interval_sec: 600,
            no_response_sec: 60,
            with_bubble: false,
        });
        // base ≥ max 为矛盾配置：max 收口为 base，间隔恒 base（不缩、不 panic）。
        assert_eq!(capped.next_interval_sec(), 900);
    }

    /// S7-M9：needs→动作 触发映射接入仲裁器（`02 §5.11`；AC-21/22 触发面）。
    ///
    /// 真实 `needs.json` 分档 + 真实目录：档位候选 → [`crate::needs::NeedsActionTrigger`]
    /// → `ActionRequest::from_cfg` → `arbiter.submit`，验证：
    ///   ① AC-21 讨食（satiety<40 → ACT-N-01 + 求助气泡）；出厂 `disabled=true`
    ///     （资源批次 B 未交付）→ `from_cfg` 自动跳过（`02 §10.1` R3）；解除后起播；
    ///   ② AC-22 喂食完成且饱食 ≥ 满档 → ACT-N-03；
    ///   ③ AC-22 很脏（<15）→ ACT-N-06；洗澡结束 → ACT-N-08。
    #[test]
    fn needs_trigger_to_arbiter_integration_ac21_ac22() {
        use crate::needs::{BandEffects, NeedsActionTrigger};

        let catalog = crate::anim::ActionCatalog::load(&resources_config_dir()).expect("目录可加载");
        let needs: crate::config::model::NeedsConfig = {
            let text = std::fs::read_to_string(resources_config_dir().join("needs.json"))
                .expect("needs.json 可读");
            serde_json::from_str(&text).expect("needs.json 可解析")
        };
        let mut trigger = NeedsActionTrigger::new();
        let mut arb = ActionArbiter::new();

        // ① AC-21：Satiety=30（peckish）→ ACT-N-01 讨食 + 求助气泡；出厂 disabled 自动跳过。
        let intents = trigger.poll(&BandEffects::from_cfg(&needs, 30.0, 80.0), 0);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].action_id, "ACT-N-01");
        assert_eq!(intents[0].bubble_pool.as_deref(), Some("begFood"));
        let n01 = catalog.find("ACT-N-01").expect("目录应含 ACT-N-01");
        assert!(n01.disabled, "批次 B 资源未交付 → disabled 自动跳过");
        assert!(ActionRequest::from_cfg(n01, ActionSource::Ambient).is_none(), "R3：disabled 跳过");
        // 解除 disabled（模拟资源交付）→ 提交 → 起播。
        let mut n01_on = n01.clone();
        n01_on.disabled = false;
        let req = ActionRequest::from_cfg(&n01_on, ActionSource::Ambient).expect("解除后应可转换");
        assert!(matches!(ArbProbe::submit(&mut arb, req, 0), Arbitration::Play), "讨食应起播");

        // ② AC-22：满档无分档候选；喂食完成且饱食 ≥ 满档（70）→ ACT-N-03。
        assert!(trigger.poll(&BandEffects::from_cfg(&needs, 80.0, 80.0), 1_000).is_empty());
        let done = NeedsActionTrigger::feed_completed(80.0, &needs).expect("饱食≥70 应触发");
        assert_eq!(done, "ACT-N-03");
        let mut n03_on = catalog.find(done).expect("ACT-N-03 应在目录").clone();
        n03_on.disabled = false;
        let req = ActionRequest::from_cfg(&n03_on, ActionSource::Ambient).expect("解除后应可转换");
        match ArbProbe::submit(&mut arb, req, 2_000) {
            Arbitration::Queued { .. } | Arbitration::DeferToLoopEnd => {}
            other => panic!("讨食（循环）进行中，吃饱满足应入队/延后：{other:?}"),
        }

        // ③ AC-22：Cleanliness=10（filthy）→ ACT-N-06 求洗澡；洗澡结束 → ACT-N-08。
        let intents = trigger.poll(&BandEffects::from_cfg(&needs, 80.0, 10.0), 3_000);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].action_id, "ACT-N-06");
        assert_eq!(intents[0].bubble_pool.as_deref(), Some("begBath"));
        let bath_end = NeedsActionTrigger::bath_completed(&needs);
        assert_eq!(bath_end, "ACT-N-08");
        let mut n08_on = catalog.find(bath_end).expect("ACT-N-08 应在目录").clone();
        n08_on.disabled = false;
        let req = ActionRequest::from_cfg(&n08_on, ActionSource::Ambient).expect("解除后应可转换");
        match ArbProbe::submit(&mut arb, req, 4_000) {
            Arbitration::Queued { .. } | Arbitration::DeferToLoopEnd => {}
            other => panic!("洗澡结束动作应入队/延后：{other:?}"),
        }
    }

    // -- 空仲裁器防御 -----------------------------------------------------------

    #[test]
    fn empty_arbiter_queries_are_safe() {
        let mut arb = ActionArbiter::new();
        assert!(arb.poll(0).is_none(), "空仲裁器 poll → None");
        assert!(arb.on_loop_segment_end(0).is_none());
        assert!(arb.on_action_finished(0).is_none());
        assert!(arb.current().is_none());
        assert!(!arb.is_performing());
        assert_eq!(arb.queue_len(), 0);
        assert!(arb.queue_snapshot().is_empty());
        // 域外优先级防御（绕过 from_cfg 的直提交路径）。
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-X-99", 0), 0), Arbitration::Dropped);
        assert_eq!(ArbProbe::submit(&mut arb, req("ACT-X-99", 11), 0), Arbitration::Dropped);
        assert!(arb.current().is_none(), "域外请求不留状态");
    }

    // -- 测试探针 ---------------------------------------------------------------

    /// 结果消费探针：[`Arbitration`] 带 `#[must_use]`，测试中经此显式消费
    /// 仲裁结果并回传以便断言。
    struct ArbProbe;

    impl ArbProbe {
        fn submit(arb: &mut ActionArbiter, request: ActionRequest, now_ms: u64) -> Arbitration {
            arb.submit(request, now_ms)
        }

        fn force(arb: &mut ActionArbiter, request: ActionRequest, now_ms: u64) -> Arbitration {
            arb.force_submit(request, now_ms)
        }
    }
}
