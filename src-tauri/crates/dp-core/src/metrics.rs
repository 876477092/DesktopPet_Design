//! `metrics`：性能度量模型与分级降级策略（**S6-M1**，T-16 段 · 上，`02 §5 K-8` / §10.2 R12）。
//!
//! ## 模块职责（纯逻辑内核，可单测，零平台依赖）
//!
//! ```text
//!   ① 度量模型    PerfSample（fps / cpu / mem 一次采样）+ PerfWire（pet://perf 载荷）
//!   ② 降级策略    DegradeController（内存分级滞回 + 降帧档位决策，K-8）
//!   ③ 执行层      dp-app::supervisor（采样 → decide → 应用档位 / 发射 pet://perf）
//! ```
//!
//! 本模块**只做度量与降级策略**（`03 S6-M1` 卡片边界：不改渲染后端实现，属 S9）。
//! 换装插槽纹理卸载 / FrameRenderer LRU 压制的**执行**由 S9 承接；本模块交付
//! **决策产物**（[`DegradeDecision`]）+ 事件载荷（[`PerfWire`]）+ 配置阈值
//! （`animation.json.degrade`），供执行层与设置页消费。
//!
//! ## 阈值口径（`02 §5 K-8` / §10.2 R12 / §5 K-4 省电档）
//!
//! | 条件 | 动作 | 恢复 |
//! |---|---|---|
//! | 内存 > 200MB | 卸载换装插槽纹理与粒子图集、`physics.level=primaryOnly` | < 170MB |
//! | 内存 > 225MB | 切 FrameRenderer + 图集 LRU 压到 32MB | 重启后用户选择 |
//! | 前台全屏 / 隐身 | 省电档 4fps（K-4） | 退出全屏 / 恢复可见 |
//! | 电池放电 | fps 上限 15（K-8） | 插电后 |
//!
//! ## 纪律
//! - **C3 零时钟**：滞回与 CPU 保持用 **tick 计数**（supervisor 每 5s 调用一次 `decide`，
//!   1 tick = 5s），不读任何系统时钟；
//! - **C8**：不新增事件名；`pet://perf` 早已登记（`02 §7.6`），本卡只补生产者；
//! - **C9**：纯 std + serde，零新增依赖、零网络。

use serde::{Deserialize, Serialize};

use crate::anim::FpsTier;
use crate::config::DegradeCfg;

// ---------------------------------------------------------------------------
// ① 度量模型
// ---------------------------------------------------------------------------

/// 一次性能采样（supervisor 每 5s 组装；fps 来自帧发布计数差分，cpu/mem 来自平台）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerfSample {
    /// 实际帧率（帧发布数 / 采样窗；无帧发布时按 0 计，供巡检判定）。
    pub fps: f32,
    /// 应用 CPU 占用（%，单核归一；与 AC-01「空闲 ≤3%、动画 ≤8%」同口径）。
    pub cpu: f32,
    /// 应用内存占用（MB；PrivateUsage，AC-40「峰值 ≤209MB / 红线 250MB / 告警 200MB」）。
    pub mem_mb: f32,
}

/// `pet://perf` 载荷（`02 §7.6`：core → 设置窗口，`{fps,cpu,mem,level}`，5s 周期）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfWire {
    /// 载荷版本（v1）。
    pub version: u32,
    /// 实际帧率（fps）。
    pub fps: f32,
    /// 应用 CPU 占用（%）。
    pub cpu: f32,
    /// 应用内存占用（MB）。
    pub mem: f32,
    /// 降级等级名（[`DegradeLevel`] camelCase：normal / memoryWarn / memoryHard）。
    pub level: String,
}

impl Default for PerfWire {
    fn default() -> Self {
        Self {
            version: 1,
            fps: 0.0,
            cpu: 0.0,
            mem: 0.0,
            level: DegradeLevel::Normal.as_str().to_string(),
        }
    }
}

impl PerfWire {
    /// 由采样与降级等级组装线上载荷。
    #[must_use]
    pub fn from_sample(sample: &PerfSample, level: DegradeLevel) -> Self {
        Self {
            version: 1,
            fps: sample.fps,
            cpu: sample.cpu,
            mem: sample.mem_mb,
            level: level.as_str().to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// ② 降级等级与动作（K-8）
// ---------------------------------------------------------------------------

/// 降级等级（severity 单调：Normal < MemoryWarn < MemoryHard）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DegradeLevel {
    /// 正常。
    Normal,
    /// 内存告警（>200MB）：卸插槽纹理、物理主档。
    MemoryWarn,
    /// 内存硬阈值（>225MB）：切 FrameRenderer + LRU 32MB（恢复 = 重启后用户选择）。
    MemoryHard,
}

impl DegradeLevel {
    /// 线上等级名（`pet://perf.level` 取值）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DegradeLevel::Normal => "normal",
            DegradeLevel::MemoryWarn => "memoryWarn",
            DegradeLevel::MemoryHard => "memoryHard",
        }
    }
}

/// 降级动作（K-8 决策输出；有序，供执行层逐项消费）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeAction {
    /// >200MB：卸载换装插槽纹理与粒子图集（执行归 S9；本阶段决策 + 日志 + 事件）。
    UnloadSlotTextures,
    /// >200MB：`physics.level=primaryOnly`（执行归 S9/S7；决策已下发）。
    PhysicsPrimaryOnly,
    /// >225MB：图集 LRU 压到指定 MB（当前渲染后端即 FrameRenderer，LRU 压制执行归 S9）。
    FrameRendererLru(u32),
}

/// 降级决策输入（一次采样 + 场景标志；`fullscreen` 含 BelowFullscreen 隐藏态）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DegradeEnv {
    /// 应用内存（MB）。
    pub mem_mb: f32,
    /// 前台全屏（全屏检测入隐藏态，`02 §5 K-4` 省电档触发条件）。
    pub fullscreen: bool,
    /// 隐身（宠物窗口不可见：手动隐藏 / 全屏隐藏；非前台降帧的统一口径）。
    pub hidden: bool,
    /// 电池放电。
    pub battery: bool,
}

/// 降级决策输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradeDecision {
    /// 降级等级（`pet://perf.level` 真源）。
    pub level: DegradeLevel,
    /// 需执行的降级动作（稳定有序）。
    pub actions: Vec<DegradeAction>,
    /// 降帧档位覆盖（`None` = 维持播放器当前档位；`Some` = 强制切档）。
    pub tier_override: Option<FpsTier>,
}

impl Default for DegradeDecision {
    fn default() -> Self {
        Self {
            level: DegradeLevel::Normal,
            actions: Vec::new(),
            tier_override: None,
        }
    }
}

/// 滞回降级控制器（**每 5s 调用一次 `decide`**；内部只计 tick，不读时钟，C3）。
///
/// 内存滞回（K-8）：
///   - Normal：> `warnMb` → MemoryWarn；> `hardMb` → MemoryHard；
///   - MemoryWarn：> `hardMb` → MemoryHard；< `recoverMb` → Normal；
///   - MemoryHard：**不自动恢复**（K-8：恢复 = 重启后用户选择），恒保持。
#[derive(Debug, Clone, Copy)]
pub struct DegradeController {
    /// 当前等级。
    level: DegradeLevel,
}

impl Default for DegradeController {
    fn default() -> Self {
        Self::new()
    }
}

impl DegradeController {
    /// 新建控制器（初始 Normal）。
    #[must_use]
    pub fn new() -> Self {
        Self { level: DegradeLevel::Normal }
    }

    /// 当前等级（供执行层查询 / 日志）。
    #[must_use]
    pub const fn level(&self) -> DegradeLevel {
        self.level
    }

    /// 依据一次采样与场景标志产出决策（纯函数；`cfg` 取 `animation.json.degrade`）。
    #[must_use]
    pub fn decide(&mut self, env: &DegradeEnv, cfg: &DegradeCfg) -> DegradeDecision {
        self.level = memory_level(self.level, env.mem_mb, cfg);

        // 动作：按等级稳定有序（不重复、与 K-8 表一致）。
        let mut actions: Vec<DegradeAction> = Vec::new();
        if self.level >= DegradeLevel::MemoryWarn {
            if cfg.slot_unload {
                actions.push(DegradeAction::UnloadSlotTextures);
            }
            if cfg.physics_primary_only {
                actions.push(DegradeAction::PhysicsPrimaryOnly);
            }
        }
        if self.level >= DegradeLevel::MemoryHard {
            actions.push(DegradeAction::FrameRendererLru(cfg.memory.lru_mb));
        }

        // 降帧（K-4 省电档 / K-8 电池上限）：全屏 / 隐身 → 省电档；电池 → 高负载档。
        // 优先级：全屏/隐身 > 电池（全屏隐藏时窗口不可见，省电最强）。
        let tier_override = if env.fullscreen || env.hidden {
            Some(FpsTier::from_fps(cfg.fps.fullscreen).unwrap_or(FpsTier::PowerSave))
        } else if env.battery {
            Some(FpsTier::from_fps(cfg.fps.battery).unwrap_or(FpsTier::HighLoad))
        } else {
            None
        };

        DegradeDecision { level: self.level, actions, tier_override }
    }
}

/// 内存等级滞回（纯函数，可单测；K-8 / R12 口径）。
#[must_use]
pub fn memory_level(current: DegradeLevel, mem_mb: f32, cfg: &DegradeCfg) -> DegradeLevel {
    let (warn, hard, recover) =
        (cfg.memory.warn_mb as f32, cfg.memory.hard_mb as f32, cfg.memory.recover_mb as f32);
    match current {
        // 硬阈值不自动恢复（K-8：恢复 = 重启后用户选择）。
        DegradeLevel::MemoryHard => DegradeLevel::MemoryHard,
        DegradeLevel::MemoryWarn => {
            if mem_mb > hard {
                DegradeLevel::MemoryHard
            } else if mem_mb < recover {
                DegradeLevel::Normal
            } else {
                DegradeLevel::MemoryWarn
            }
        }
        DegradeLevel::Normal => {
            if mem_mb > hard {
                DegradeLevel::MemoryHard
            } else if mem_mb > warn {
                DegradeLevel::MemoryWarn
            } else {
                DegradeLevel::Normal
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DegradeCfg;

    /// 默认降级配置（与 animation.json 同源）。
    fn cfg() -> DegradeCfg {
        DegradeCfg::default()
    }

    fn env(mem_mb: f32) -> DegradeEnv {
        DegradeEnv { mem_mb, fullscreen: false, hidden: false, battery: false }
    }

    #[test]
    fn memory_level_normal_to_warn_then_hard() {
        let mut c = DegradeController::new();
        let d = c.decide(&env(100.0), &cfg());
        assert_eq!(d.level, DegradeLevel::Normal);
        assert!(d.actions.is_empty());
        assert_eq!(d.tier_override, None);

        let d = c.decide(&env(210.0), &cfg());
        assert_eq!(d.level, DegradeLevel::MemoryWarn);
        assert!(d.actions.contains(&DegradeAction::UnloadSlotTextures));
        assert!(d.actions.contains(&DegradeAction::PhysicsPrimaryOnly));
        assert!(!d.actions.contains(&DegradeAction::FrameRendererLru(32)));

        let d = c.decide(&env(230.0), &cfg());
        assert_eq!(d.level, DegradeLevel::MemoryHard);
        assert!(d.actions.contains(&DegradeAction::FrameRendererLru(32)));
    }

    #[test]
    fn memory_warn_recovers_below_threshold() {
        let mut c = DegradeController::new();
        let _ = c.decide(&env(210.0), &cfg());
        let d = c.decide(&env(160.0), &cfg());
        assert_eq!(d.level, DegradeLevel::Normal, "<170MB 应回退 Normal");
    }

    #[test]
    fn memory_warn_stays_within_band() {
        let mut c = DegradeController::new();
        let _ = c.decide(&env(210.0), &cfg());
        let d = c.decide(&env(190.0), &cfg());
        assert_eq!(d.level, DegradeLevel::MemoryWarn, "190MB 在 170~200 区间应保持 Warn");
    }

    #[test]
    fn memory_hard_is_sticky_until_restart() {
        let mut c = DegradeController::new();
        let _ = c.decide(&env(230.0), &cfg());
        // 即使内存回落也保持 Hard（K-8：恢复 = 重启后用户选择）。
        let d = c.decide(&env(150.0), &cfg());
        assert_eq!(d.level, DegradeLevel::MemoryHard);
    }

    #[test]
    fn hard_skips_warn_from_normal() {
        // 直接越过告警档进入硬阈值。
        let d = memory_level(DegradeLevel::Normal, 250.0, &cfg());
        assert_eq!(d, DegradeLevel::MemoryHard);
    }

    #[test]
    fn fullscreen_and_hidden_force_power_save_tier() {
        let mut c = DegradeController::new();
        let d = c.decide(
            &DegradeEnv { mem_mb: 100.0, fullscreen: true, hidden: false, battery: false },
            &cfg(),
        );
        assert_eq!(d.tier_override, Some(FpsTier::PowerSave), "全屏 → 省电档 4fps");

        let d = c.decide(
            &DegradeEnv { mem_mb: 100.0, fullscreen: false, hidden: true, battery: false },
            &cfg(),
        );
        assert_eq!(d.tier_override, Some(FpsTier::PowerSave), "隐身 → 省电档 4fps");
    }

    #[test]
    fn battery_caps_fps_at_high_load_tier() {
        let mut c = DegradeController::new();
        let d = c.decide(
            &DegradeEnv { mem_mb: 100.0, fullscreen: false, hidden: false, battery: true },
            &cfg(),
        );
        assert_eq!(d.tier_override, Some(FpsTier::HighLoad), "电池 → 15fps 上限");
    }

    #[test]
    fn fullscreen_wins_over_battery() {
        let mut c = DegradeController::new();
        let d = c.decide(
            &DegradeEnv { mem_mb: 100.0, fullscreen: true, hidden: false, battery: true },
            &cfg(),
        );
        assert_eq!(d.tier_override, Some(FpsTier::PowerSave), "全屏优先级高于电池");
    }

    #[test]
    fn degrade_level_str_names_are_stable() {
        assert_eq!(DegradeLevel::Normal.as_str(), "normal");
        assert_eq!(DegradeLevel::MemoryWarn.as_str(), "memoryWarn");
        assert_eq!(DegradeLevel::MemoryHard.as_str(), "memoryHard");
    }

    #[test]
    fn perf_wire_from_sample_carries_level() {
        let sample = PerfSample { fps: 4.0, cpu: 1.2, mem_mb: 210.0 };
        let wire = PerfWire::from_sample(&sample, DegradeLevel::MemoryWarn);
        assert_eq!(wire.version, 1);
        assert_eq!(wire.fps, 4.0);
        assert_eq!(wire.cpu, 1.2);
        assert_eq!(wire.mem, 210.0);
        assert_eq!(wire.level, "memoryWarn");

        // 默认载荷为 Normal 等级。
        let default_wire = PerfWire::default();
        assert_eq!(default_wire.level, "normal");
    }

    #[test]
    fn actions_are_stable_and_ordered() {
        let mut c = DegradeController::new();
        let d = c.decide(&env(230.0), &cfg());
        assert_eq!(
            d.actions,
            vec![
                DegradeAction::UnloadSlotTextures,
                DegradeAction::PhysicsPrimaryOnly,
                DegradeAction::FrameRendererLru(32),
            ]
        );
    }
}
