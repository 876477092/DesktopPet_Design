//! `dp-platform` 端口定义（落地 `02 §4.3` 冻结契约）。
//!
//! 本文件只放**跨模块最小契约**：`PlatformWindow` / `HitSource` 两个 trait，
//! 以及 `02` 中给出签名但**未给出具体定义**的载体类型
//! （`Vec2` / `HitMask` / `HitResult` / `RawWindowHandle` / `TopmostMode`）。
//!
//! ⚠️ 端口级最小定义说明（重要）：
//!   `Vec2` / `HitMask` / `HitResult` / `RawWindowHandle` 在 `02` 中**只有签名没有定义**。
//!   此处给出「足够本模块编译与单测」的最小实现，**后续模块（S2 运动 / S3 交互）
//!   需要时再统一或替换为正式类型**（届时保持方法语义不变即可平滑迁移）。
//!   本文件**不引入 `dp-core` 依赖**，也**不发明新的跨 crate 契约**。
//!
//! ⚠️ `PlatformWindow` / `HitSource` 的**方法名与参数逐字冻结**（`02 §4.3`），
//!   任何模块都不得改名或改参数。

use std::ops::{Add, AddAssign, Mul, Sub};
use std::sync::Arc;

use thiserror::Error;

// ---------------------------------------------------------------------------
// 错误与结果
// ---------------------------------------------------------------------------

/// 平台层统一错误。
///
/// 遵循 `02 §7.4`：内核用 `thiserror` + `#[non_exhaustive]`；
/// **平台调用失败一律降级不崩溃**，调用方负责把本错误转成降级行为或日志。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// Win32 调用返回失败（含 HRESULT / GetLastError 数值）。
    #[error("Win32 调用失败：{op}（错误码 {code:#010X}）")]
    Win32 {
        /// 出错的 Win32 操作名（便于定位）。
        op: &'static str,
        /// 错误码（`GetLastError` 或 HRESULT，统一按位模式呈现）。
        code: u32,
    },
    /// 窗口句柄为空 / 已失效。
    #[error("窗口句柄无效（空指针）")]
    InvalidHandle,
    /// 当前没有可用显示器（多屏拔插瞬态）。
    #[error("没有可用显示器")]
    NoMonitor,
    /// 命中掩码尺寸或字节数非法。
    #[error("命中掩码非法：{0}")]
    InvalidMask(String),
}

/// 平台层统一 `Result`。
pub type Result<T> = std::result::Result<T, PlatformError>;

// ---------------------------------------------------------------------------
// 端口级最小载体类型
// ---------------------------------------------------------------------------

/// 二维向量（VDC / SPC / 窗口本地坐标通用；`f32`，单位随坐标系语义而定）。
///
/// 端口级最小定义（`02 §4.3` 未给定 `Vec2` 具体结构）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    /// X 分量。
    pub x: f32,
    /// Y 分量。
    pub y: f32,
}

impl Vec2 {
    /// 零向量。
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    /// 构造一个向量。
    #[inline]
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// 分量缩放（等价于 `self * k`，保留为具名方法便于阅读）。
    #[inline]
    #[must_use]
    pub fn scale(self, k: f32) -> Vec2 {
        Vec2::new(self.x * k, self.y * k)
    }

    /// 欧氏长度。
    #[inline]
    #[must_use]
    pub fn length(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    #[inline]
    fn add(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl Sub for Vec2 {
    type Output = Vec2;
    #[inline]
    fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl Mul<f32> for Vec2 {
    type Output = Vec2;
    #[inline]
    fn mul(self, rhs: f32) -> Vec2 {
        Vec2::new(self.x * rhs, self.y * rhs)
    }
}

impl AddAssign for Vec2 {
    #[inline]
    fn add_assign(&mut self, rhs: Vec2) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}

/// 像素命中掩码。
///
/// 端口级最小定义：每像素 1 bit，按「每行左对齐、字节内高位在前」排布。
/// **只提供存储与 `bit_at` 查询**；像素级命中判定（`02 §5 K-2`）属 S3-M2，
/// 本模块仅把它作为 `PlatformWindow::set_hit_mask` 的载体保存。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HitMask {
    width: u32,
    height: u32,
    bits: Vec<u8>,
}

impl HitMask {
    /// 每行占用的字节数（向上取整到字节）。
    #[inline]
    fn stride(width: u32) -> usize {
        width.div_ceil(8) as usize
    }

    /// 构造全 0（无命中）掩码。
    pub fn blank(width: u32, height: u32) -> Result<Self> {
        let len = HitMask::stride(width)
            .checked_mul(height as usize)
            .ok_or_else(|| PlatformError::InvalidMask(format!("尺寸溢出：{width}x{height}")))?;
        Ok(Self { width, height, bits: vec![0u8; len] })
    }

    /// 由既有位图字节构造掩码（字节数必须与 `width/height` 匹配）。
    pub fn from_bytes(width: u32, height: u32, bits: Vec<u8>) -> Result<Self> {
        let expect = HitMask::stride(width)
            .checked_mul(height as usize)
            .ok_or_else(|| PlatformError::InvalidMask(format!("尺寸溢出：{width}x{height}")))?;
        if bits.len() != expect {
            return Err(PlatformError::InvalidMask(format!(
                "字节数 {} 与 {width}x{height}（应为 {expect}）不符",
                bits.len()
            )));
        }
        Ok(Self { width, height, bits })
    }

    /// 掩码宽度（像素）。
    #[inline]
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 掩码高度（像素）。
    #[inline]
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 原始位图字节。
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bits
    }

    /// 查询 `(x, y)` 是否命中（越界返回 `false`）。
    #[inline]
    #[must_use]
    pub fn bit_at(&self, x: u32, y: u32) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let stride = HitMask::stride(self.width);
        let idx = y as usize * stride + (x / 8) as usize;
        let mask = 1u8 << (7 - (x % 8));
        self.bits[idx] & mask != 0
    }
}

/// 命中区域档位（端口级最小定义；`02 §5 K-2` 主热区 / 次级热区）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HitZone {
    /// 主热区：命中即吞事件。
    Primary,
    /// 次级热区：悬停有效、点击落主判定。
    Secondary,
}

/// 命中判定结果（端口级最小定义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HitResult {
    /// 未命中。
    Miss,
    /// 命中指定档位热区。
    Hit(HitZone),
}

impl HitResult {
    /// 是否命中。
    #[inline]
    #[must_use]
    pub fn is_hit(&self) -> bool {
        matches!(self, HitResult::Hit(_))
    }
}

/// 原生窗口句柄（端口级最小定义）。
///
/// 当前平台实装填 **Win32 `HWND` 的指针值**（`isize`）。正式化时（引入
/// `raw-window-handle` 或统一到 `dp-core`）保持 `hwnd()` 语义不变即可平滑迁移。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawWindowHandle {
    hwnd: isize,
}

impl RawWindowHandle {
    /// 由原生句柄值构造。
    #[inline]
    #[must_use]
    pub const fn from_hwnd(hwnd: isize) -> Self {
        Self { hwnd }
    }

    /// 原生句柄值（Win32 下为 `HWND` 指针值）。
    #[inline]
    #[must_use]
    pub const fn hwnd(&self) -> isize {
        self.hwnd
    }

    /// 是否为空句柄。
    #[inline]
    #[must_use]
    pub const fn is_null(&self) -> bool {
        self.hwnd == 0
    }
}

/// 置顶三态（`02 §5 K-1` 置顶策略状态机）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopmostMode {
    /// 常驻 `HWND_TOPMOST`。
    Always,
    /// 前台全屏时 `HWND_NOTOPMOST` + `SW_HIDE`，退出全屏 3s 后恢复。
    BelowFullscreen,
    /// `HWND_NOTOPMOST`（不置顶）。
    Never,
}

impl Default for TopmostMode {
    /// 默认常驻置顶（桌面宠物默认体验）。
    fn default() -> Self {
        TopmostMode::Always
    }
}

// ---------------------------------------------------------------------------
// 冻结端口
// ---------------------------------------------------------------------------

/// 窗口端口（逐字照抄 `02 §4.3`，**不得改名 / 改参数**）。
pub trait PlatformWindow: Send + Sync {
    /// 设置窗口位置；入参为 **VDC**，内部按 `02 §7.2`（RV-17）换算 **SPC** 后落窗口。
    fn set_position_vdc(&self, p: Vec2) -> Result<()>;
    /// 设置窗口尺寸；入参为**逻辑像素**（96-dpi），内部乘 `userScale × 显示器 scale` 得物理像素。
    fn set_size_logical(&self, w: u32, h: u32) -> Result<()>;
    /// 设置置顶策略（三态）。
    fn set_topmost(&self, mode: TopmostMode) -> Result<()>;
    /// 开关鼠标穿透（`WS_EX_TRANSPARENT` + Tauri `set_ignore_cursor_events`）。
    fn set_click_through(&self, on: bool) -> Result<()>;
    /// 存储命中掩码（供后续消费；像素命中判定属 S3-M2）。
    fn set_hit_mask(&self, mask: Arc<HitMask>) -> Result<()>;
    /// 显示 / 隐藏窗口（`SW_SHOWNOACTIVATE` / `SW_HIDE`）。
    fn set_visible(&self, v: bool) -> Result<()>;
    /// 自检并补回缺失样式（B-09 门禁后确定的透明路径）。
    fn ensure_styles(&self) -> Result<()>;
    /// 取原生窗口句柄。
    fn raw_handle(&self) -> RawWindowHandle;
}

/// 命中源端口（逐字照抄 `02 §4.3`；实装在 S3）。
pub trait HitSource: Send {
    /// 判定本地坐标 `local` 在剪辑 `clip`、相位 `phase` 下是否命中。
    fn test(&self, local: Vec2, clip: &str, phase: f32) -> HitResult;
}

// ---------------------------------------------------------------------------
// S7-M1：活动感知端口（FR-6-3 增量 / `02 §5.6`）
// ---------------------------------------------------------------------------

/// 活动感知端口（S7-M1，T-20）。
///
/// 抽象粒度 = 「一次**采样**」与「一次**隐私开关切换**」；实装在 `dp-app`
/// （由于它需要同时持有鼠标钩子 sink 与键盘钩子服务，见「端口范式」：trait 在
/// `dp-platform`、实装在 `dp-app`）。
///
/// ## 隐私契约（`01 §6.8` FR-8-5 / `02 §5.6`，实现方**必须**满足）
///
///   1. [`Self::foreground_hash`] **只返回哈希**——进程名明文不得跨出实现所在模块，
///      不得进日志、不得进存档；
///   2. [`Self::set_activity_sensing`] 置 `false` 时，实现方必须**卸载键盘钩子**并
///      停止全部活动采样；此后 [`Self::foreground_hash`] / [`Self::intensity`] 返回
///      `None`（调用方据此退化为**纯时间模型**：`presence` 恒在场、`P_Cap` 取 `capFree`）；
///   3. 本端口**不含任何**「内容型」取值口（无按键值、无窗口标题、无文本）——这是
///      接口层面的隐私保证，而非仅靠实现纪律。
pub trait ActivitySensing: Send + Sync {
    /// 前台窗口所属进程的**类别哈希**（FNV-1a 64；`None` = 不可用 / 感知已关闭）。
    fn foreground_hash(&self) -> Option<u64>;

    /// 键鼠空闲时长（毫秒；`GetLastInputInfo` 口径；`None` = 不可用）。
    fn idle_ms(&self) -> Option<u64>;

    /// 自上次调用以来的输入强度（速率口径；`None` = 窗口不可信 / 感知已关闭）。
    ///
    /// `now_ms` 由调用方注入（C3：平台层同样不裸读墙钟，节拍由上层装配给定）。
    fn intensity(&mut self, now_ms: i64) -> Option<InputIntensity>;

    /// 活动感知是否开启（`false` = 隐私关闭：调用方应视为「无感知信息」）。
    fn is_activity_sensing(&self) -> bool;

    /// 置活动感知开关（隐私一键关闭）。实现方负责卸载 / 重装键盘钩子。
    fn set_activity_sensing(&self, on: bool);
}

/// 输入强度（速率口径；`dp-platform` 侧定义，`dp-core::perception::ActivitySample`
/// 为其线上对等类型，装配层按字段直转）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InputIntensity {
    /// 键击强度（键/秒）。
    pub key_kps: f32,
    /// 点击强度（次/分钟）。
    pub clicks_per_min: f32,
    /// 鼠标移动强度（物理像素/分钟）。
    pub move_px_per_min: f32,
}

impl InputIntensity {
    /// 全零强度（所有源不可用时的显式取值）。
    pub const ZERO: InputIntensity =
        InputIntensity { key_kps: 0.0, clicks_per_min: 0.0, move_px_per_min: 0.0 };
}
