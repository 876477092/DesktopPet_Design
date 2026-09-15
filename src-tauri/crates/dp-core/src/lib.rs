//! `dp-core`：纯逻辑内核（`02 §3`）。
//!
//! 承载内容（`02 §3`）：`lib/state/event/metrics`、`emotion/`、`needs`、
//! `motion/`、`anim/`、`interaction/`、`perception/`、`save/`、`config/`。
//!
//! 设计约束：
//!   - 纯逻辑、可 `cargo test`，不得依赖具体平台 API（平台能力一律走 `dp-platform` 端口）；
//!   - 时间纪律（C3，**2026-09-12 复核裁定**，取代早先「WallClock 落地后回改」
//!     的未完成承诺）：**业务时间**禁止裸读墙钟（`Utc::now()` / `Local::now()` /
//!     `SystemTime`）——时段判定（晨/午/昏/夜）、番茄钟、离线补偿、存档时间戳等
//!     一律经 `perception::time::WallClock` 端口注入（`FakeWallClock` 供单测）；
//!   - 时间纪律（续）：**单调节拍允许直读 `std::time::Instant`**——监督循环 tick、
//!     帧播放器节拍、全屏迟滞等「间隔 / 超时」语义用单调钟。此处**不可改用墙钟**：
//!     系统时间回拨会让绝对锚定网格（`start + k×间隔`）的 deadline 计算产生巨量
//!     负补偿，反而破坏 AC-11 的恢复时延上限。现行实现（`dp-app::supervisor`、
//!     `dp-app::bridge`）即按此口径，属合规而非欠账；
//!   - 时间纪律（续）：`dp-core` 内**零时钟**——时间全部由调用方注入 `now_ms`；
//!     唯一豁免点是 `perception::time::SystemWallClock`（端口的墙钟实装）。
//!   - 配置键 camelCase + 单位后缀（C7）；数值一律外置到 `resources/config/*.json`。
//!
//! 已落地模块：
//!   - S1-M5（T-03）：`config/` 配置中心——六份外置 JSON 的数据模型与
//!     `ConfigService::load_all`（缺字段补默认 + 告警，`02 §4.2`；加载失败降级内置
//!     默认启动不崩，R19；needs.coupling 构建期拓扑环检查，`02 §5.10`）；
//!   - S2-M2（T-04 段 · 下）：`anim/` 动作目录与播放器——`actions.json` 53 条全量
//!     元数据目录（批次 A 29 条启用视图）、轮播播放器（动画帧号 = elapsed × 动作
//!     fps、loopRange 回绕、非循环停留末帧）、fps 档位 2/4/6/15/30/60 可切
//!     （K-4 tick 节拍档位，CPU 随档位变化）、镜像规则（K-4：元数据 mirror + 朝向）；
//!   - S2-M4（T-05）：`motion/` 运动决策引擎——漫游决策（间隔绝对锚定 + pace
//!     缩放、12 次采样）、光标热区避让（150px + tangent 绕行纯函数）、跨屏
//!     边缘插值（AC-13 不瞬移）、拔屏 2s 迁移（FR-1-4）、站立面端口
//!     （`StandSurface`，S2-M6 PlatformGraph 复用）；
//!   - S5-M1（T-14 段 · 上）：`save/` 存档——v2 Schema（段归属划分）、原子写
//!     （tmp + fsync + rename，AC-14 机理）、30s 定时 + 2s 合并窗口、三级降级链
//!     （`bak` 恢复 / `corrupt`·`future` 隔离 / `v1` 待迁移禁写盘）；
//!   - S5-M2（T-14 段 · 下）：离线补偿接入——`store::SaveStore::away_ms` 推导离线
//!     真实时长（墙钟回拨钳 0）+ `EmotionEngine::restore`（供 `offline_compensate`
//!     使用，RV-16 封顶 L4、3h/4h/5h 边界见 AC-37）。
pub mod anim;
pub mod config;
// S6-M1（T-16 段 · 上）：性能度量与分级降级策略——`metrics.rs`（K-8 / R12：
// 内存 200/225/170MB 滞回 + 全屏/隐身/电池降帧；`pet://perf` 生产者；决策动作
// 供 S9 执行）。纯逻辑、零平台依赖、零时钟（tick 计数滞回）。
pub mod metrics;
// S4-M1（T-11 段 · 上）：情绪数值与六维状态机内核——`emotion/`（P 累积 + L0~L5 阶段
// 结算 + Mood 一阶低通 + 离线补偿）与 `state`（六维数值 + 会话暂停窗口）。
// 边界：七因子归 S7-M4、台词/道歉三部曲归 S4-M3/M5。
pub mod emotion;
// S4-M2（T-11 段 · 下）：事件总线映射与 1Hz 快照投影——`EmotionEvent` → `pet://state` /
// `pet://emotion` 的纯函数映射（C8：事件名真源在内核，emit 动作在 `dp-app` 壳）；
// `PetSnapshotV2`（`02 §4.3` TS 接口的 Rust 侧镜像）。前端先行契约见 `shared/ipc.ts`。
pub mod event;
// S3-M2 / S3-M3（T-08 段 · 中/下）：交互内核——像素命中判定源（hit）、
// 手势状态机与轨迹识别（gesture）、意图路由与 ACT 码映射（router）。
pub mod interaction;
pub mod motion;
// S2-M7（T-07）：感知服务内核侧类型与端口（骨架接线，实现随 S2-M7 填充）。
pub mod perception;
// S5-M1（T-14 段 · 上）：存档——v2 Schema（`SaveFileV2`，`02 §5 K-7` 全表）、
// 原子写（tmp + fsync + rename）、30s 定时与 2s 合并窗口、损坏/未来版本/待迁移
// 三级降级链（隔离 `save.{corrupt,future}.<ts>.json`）。
// S5-M2（T-14 段 · 下）：离线段（`away_ms` 推导）+ 启动补偿接入（`EmotionEngine::restore`
// → `offline_compensate`，RV-16 `min(4)` 封顶）。
// 边界：v1→v2 迁移归 S8-M7（本模块只报 `MigrationPending` 并禁写盘保护原档）；
// 各段归属见 `save::schema::SaveFileV2` 文档；单实例与文件锁归 S5-M4。
pub mod save;
pub mod state;
