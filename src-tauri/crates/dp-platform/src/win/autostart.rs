#![cfg(windows)]

//! 开机自启（`01 FR-1-9` / `03 S5-M4`）——注册表 `Run` 项写入与删除。
//!
//! ## 为什么自实现而不引第三方插件
//! `02 §2.5` 曾列入 `tauri-plugin-autostart`。本卡（S5-M4 交付物清单明列
//! `dp-platform/src/win/autostart.rs`）选择**自实现**，理由三条：
//!   1. **零新增依赖**：`Run` 项读写只需 `RegCreateKeyExW` / `RegSetValueExW` /
//!      `RegDeleteValueW` / `RegQueryValueExW`，全部已在本 crate 的 `windows` 依赖面内
//!      （新增 feature `Win32_System_Registry`，见 `src-tauri/Cargo.toml` 的 C9 登记）；
//!   2. **C9 零网络**：不引入任何"自动更新/遥测"通道，纯本机注册表操作；
//!   3. **可控的错误面**：插件把失败包装成不透明错误；本模块返回带操作名的可读错误，
//!      并**显式区分**「键不存在」（正常态，非错误）与「访问被拒」（需提示用户）。
//!
//! ## 口径（`01 FR-1-9`：注册表 Run 键/计划任务，静默启动不弹窗）
//! - 写 `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`（HKCU = 当前用户，
//!   **不需要管理员权限**；HKLM 需提权，不做）；
//! - 值名固定为产品名 [`AUTOSTART_VALUE_NAME`]（与 `tauri.conf.json.productName` 一致，
//!   由单测锁定），值类型 `REG_SZ`，数据为**带引号的绝对 exe 路径**（路径含空格时
//!   Windows 需要引号才能正确解析命令行）；
//! - 「取消即删」：删除值项本身（不写空串——空串 Run 项会让 Windows 尝试执行空命令）。
//!
//! ## 时间纪律（C3）
//! 本模块**零时钟**：不读系统时间、不做去抖（去抖由调用方按单调节拍处理）。
//!
//! ## 测试口径
//! 纯函数（[`build_command`] / [`matches_current_exe`] / [`normalize_command`]）在有单测；
//! 真实注册表读写**不在单测内**触碰（避免污染开发机自启项，且 CI 无交互桌面）——
//! 由 S5-M4 的真机 QA 走「开启 → 重启/查询 → 关闭」人工验证。

use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS,
    REG_SZ,
};

use crate::traits::{PlatformError, Result};

/// `Run` 键路径（HKCU；写此键**无需管理员权限**）。
pub const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// 自启值名（与 `tauri.conf.json` 的 `productName` 一致；由单测锁定）。
///
/// 改名即等于「旧值项成为孤儿」——单测会失败，强制维护者同步考虑清理策略。
pub const AUTOSTART_VALUE_NAME: &str = "DesktopPet";

/// 自启项当前状态（`01 FR-1-9` 设置开关的三态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutostartState {
    /// 未设置（键/值不存在）——关闭态。
    Absent,
    /// 已设置，且命令行指向**当前运行的可执行文件**——正常开启态。
    Enabled,
    /// 已设置，但指向**另一个路径**（旧安装位置 / 手工改过）——需视为"待修正"。
    ///
    /// 语义提示：调用方若要「开启自启」，直接覆盖写入即可；此态主要用于 UI 如实显示
    /// 「已开启（路径已过期）」而不是简单显示"关"。
    StalePath(String),
}

impl AutostartState {
    /// 是否处于"用户可感知的开启"（含过期路径，UI 不应显示为关闭）。
    #[must_use]
    pub fn is_on(&self) -> bool {
        !matches!(self, Self::Absent)
    }
}

/// 把 exe 绝对路径构造成 `Run` 项命令行（**带引号**）。
///
/// 规则：始终加双引号（`"C:\a b\DesktopPet.exe"`）。Windows 解析 `Run` 值时按命令行规则；
/// 无空格路径加引号同样合法，故统一处理避免"路径里出现空格就静默失效"的经典坑。
#[must_use]
pub fn build_command(exe: &Path) -> String {
    let text = exe.to_string_lossy();
    format!("\"{}\"", text.trim_matches('"'))
}

/// 归一化命令行用于比较：去首尾空白 → 去掉包裹双引号 → 统一小写（Windows 路径不区分大小写）。
#[must_use]
pub fn normalize_command(command: &str) -> String {
    command.trim().trim_matches('"').trim().to_ascii_lowercase()
}

/// 判断已存命令行是否指向给定 exe（大小写 / 引号不敏感；`01 FR-1-9`）。
#[must_use]
pub fn matches_current_exe(stored: &str, exe: &Path) -> bool {
    normalize_command(stored) == normalize_command(&exe.to_string_lossy())
}

/// 读取自启项当前状态（`RegQueryValueExW`；值不存在 → [`AutostartState::Absent`]）。
///
/// # Errors
/// 仅当注册表 API 返回**真实错误**（如访问被拒）时返回 `Err`；「键/值不存在」是正常态，
/// 映射为 `Ok(Absent)`（与 `Plan9`/Win32 的 `ERROR_FILE_NOT_FOUND` 语义区分）。
pub fn query(exe: &Path) -> Result<AutostartState> {
    let key = open_run_key(KEY_QUERY_VALUE)?;
    let Some(key) = key else {
        return Ok(AutostartState::Absent);
    };
    let stored = read_value(key, AUTOSTART_VALUE_NAME);
    // SAFETY: key 由 open_run_key 返回的有效句柄，读完立即关闭。
    unsafe {
        let _ = RegCloseKey(key);
    }
    match stored {
        Ok(Some(command)) if matches_current_exe(&command, exe) => Ok(AutostartState::Enabled),
        Ok(Some(command)) => Ok(AutostartState::StalePath(command)),
        Ok(None) => Ok(AutostartState::Absent),
        Err(err) => Err(err),
    }
}

/// 开启自启：写入/覆盖 `Run` 值（幂等）。
///
/// # Errors
/// 注册表写入失败（访问被拒 / 无权限）返回带操作名的 [`PlatformError::Win32`]。
pub fn enable(exe: &Path) -> Result<()> {
    let command = build_command(exe);
    let sub_key = to_wide(RUN_KEY_PATH);
    let value_name = to_wide(AUTOSTART_VALUE_NAME);
    let data = to_wide(&command);
    let mut key = HKEY::default();
    // SAFETY: 传出的 key 在成功时由 RegCloseKey 关闭；PCWSTR 由本函数持有的 Vec<u16> 支撑，
    // 生命周期覆盖调用（`to_wide` 的缓冲区在本函数栈上，调用期间有效）。
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub_key.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()
        .map_err(|err| win32("RegCreateKeyExW(Run)", err))?;
        let bytes: Vec<u8> = data.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        let res = RegSetValueExW(key, PCWSTR(value_name.as_ptr()), None, REG_SZ, Some(&bytes));
        let _ = RegCloseKey(key);
        res.ok().map_err(|err| win32("RegSetValueExW(autostart)", err))?;
    }
    Ok(())
}

/// 取消自启：删除 `Run` 值（幂等——值本就不存在视为成功）。
///
/// # Errors
/// 仅注册表 API 真实失败时返回 `Err`；「值不存在」映射为成功（取消自启的目标态已达成）。
pub fn disable() -> Result<()> {
    let key = open_run_key(KEY_SET_VALUE)?;
    let Some(key) = key else {
        return Ok(());
    };
    let value_name = to_wide(AUTOSTART_VALUE_NAME);
    // SAFETY: key 为有效句柄；PCWSTR 由栈上缓冲区支撑。
    let code = unsafe { RegDeleteValueW(key, PCWSTR(value_name.as_ptr())) };
    // SAFETY: 句柄用完即关。
    unsafe {
        let _ = RegCloseKey(key);
    }
    if code == WIN32_ERROR(0) || code == ERROR_FILE_NOT_FOUND {
        // 成功，或「值不存在」（目标态已达成）。
        return Ok(());
    }
    Err(win32_code("RegDeleteValueW(autostart)", code))
}

/// 按开关态同步自启项（`01 FR-1-9` 设置项的唯一落地点）。
///
/// 这是**设置热更新**的调用面：设置页改开关 → 命令层调用本函数 → 真实写注册表。
/// 返回同步后的真实状态（`enabled=true` 时通常为 [`AutostartState::Enabled`]）。
///
/// # Errors
/// 注册表操作失败时返回 `Err`（调用方降级：记录日志 + 把开关回滚为真实状态）。
pub fn set_enabled(on: bool, exe: &Path) -> Result<AutostartState> {
    if on {
        enable(exe)?;
    } else {
        disable()?;
    }
    query(exe)
}

// ---------------------------------------------------------------------------
// 内部实现
// ---------------------------------------------------------------------------

/// UTF-16（NUL 结尾）转换（Win32 宽字符 API 入参）。
fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 打开 `Run` 键；键不存在 → `Ok(None)`（正常态，不视为错误）。
fn open_run_key(access: REG_SAM_FLAGS) -> Result<Option<HKEY>> {
    let sub_key = to_wide(RUN_KEY_PATH);
    let mut key = HKEY::default();
    // SAFETY: 输出句柄由调用方负责关闭；PCWSTR 由栈上缓冲区支撑。
    let code = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub_key.as_ptr()),
            None,
            access,
            &mut key,
        )
    };
    if code == WIN32_ERROR(0) {
        return Ok(Some(key));
    }
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    Err(win32_code("RegOpenKeyExW(Run)", code))
}

/// 读 `Run` 值（`REG_SZ`）：`Ok(None)` = 值不存在。
fn read_value(key: HKEY, name: &str) -> Result<Option<String>> {
    let value_name = to_wide(name);
    let mut kind = REG_SZ;
    let mut size: u32 = 0;
    // 第一趟：取长度。
    // SAFETY: 只传空数据指针问长度（Windows 允许 lpData = null 取所需缓冲大小）。
    let code = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if code != WIN32_ERROR(0) {
        return Err(win32_code("RegQueryValueExW(size)", code));
    }
    if size == 0 {
        return Ok(None);
    }
    let mut buffer = vec![0u8; size as usize];
    // 第二趟：取数据。
    // SAFETY: buffer 长度 = 上一步问到的 size，足够容纳。
    let code = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut kind),
            Some(buffer.as_mut_ptr()),
            Some(&mut size),
        )
    };
    if code != WIN32_ERROR(0) {
        return Err(win32_code("RegQueryValueExW(data)", code));
    }
    // UTF-16LE → String（截到首个 NUL；末尾 NUL 属正常）。
    let units: Vec<u16> = buffer
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let end = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
    Ok(Some(String::from_utf16_lossy(&units[..end])))
}

/// 把 `windows` 错误转成带操作名的平台错误（`02 §7.4`）。
fn win32(op: &'static str, err: windows::core::Error) -> PlatformError {
    PlatformError::Win32 { op, code: err.code().0 as u32 }
}

/// 把原始 `WIN32_ERROR` 转成带操作名的平台错误（注册表 API 返回码形态）。
fn win32_code(op: &'static str, code: WIN32_ERROR) -> PlatformError {
    PlatformError::Win32 { op, code: code.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn run_key_path_and_value_name_are_frozen() {
        // 键路径与值名是**外部可见契约**（注册表里能看到的字面量，改名 = 孤儿值项）。
        assert_eq!(RUN_KEY_PATH, r"Software\Microsoft\Windows\CurrentVersion\Run");
        assert_eq!(AUTOSTART_VALUE_NAME, "DesktopPet");
    }

    #[test]
    fn build_command_always_quotes_and_avoids_double_quoting() {
        let exe = PathBuf::from(r"C:\Program Files\DesktopPet\DesktopPet.exe");
        assert_eq!(build_command(&exe), r#""C:\Program Files\DesktopPet\DesktopPet.exe""#);
        // 已带引号的输入不再叠加引号（幂等）。
        let quoted = PathBuf::from(r#""C:\DesktopPet.exe""#);
        assert_eq!(build_command(&quoted), r#""C:\DesktopPet.exe""#);
    }

    #[test]
    fn matches_current_exe_ignores_case_and_quotes() {
        let exe = PathBuf::from(r"C:\Apps\DesktopPet.exe");
        assert!(matches_current_exe(r#""C:\Apps\DesktopPet.exe""#, &exe));
        assert!(matches_current_exe(r"C:\APPS\DESKTOPPET.EXE", &exe));
        assert!(matches_current_exe(r#"  "c:\apps\desktoppet.exe"  "#, &exe));
        assert!(!matches_current_exe(r#""D:\Other\DesktopPet.exe""#, &exe));
        assert!(!matches_current_exe("", &exe));
    }

    #[test]
    fn normalize_command_strips_wrapping_quotes_only() {
        assert_eq!(normalize_command("\"a b\""), "a b");
        assert_eq!(normalize_command("  a b  "), "a b");
        // 内部引号不动（只有首尾包裹引号是 Windows 约定，内部引号属路径非法字符）。
        assert_eq!(normalize_command("\"a\"b\""), "a\"b");
    }

    #[test]
    fn autostart_state_is_on_only_when_present() {
        assert!(!AutostartState::Absent.is_on());
        assert!(AutostartState::Enabled.is_on());
        assert!(AutostartState::StalePath("x".to_string()).is_on());
    }
}
