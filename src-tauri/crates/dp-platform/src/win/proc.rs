//! 前台进程类别感知实装（S7-M1，T-20 / FR-6-3 增量 / `02 §5.6` / `02 §5 K-10`）。
//!
//! 职责：`GetForegroundWindow` → `GetWindowThreadProcessId` → `OpenProcess`
//! （`PROCESS_QUERY_LIMITED_INFORMATION`）→ `QueryFullProcessImageNameW` → 归一化
//! → **FNV-1a 64**，对外只交付 `u64`。
//!
//! ## 隐私红线（`01 §6.8` FR-8-5 / `02 §5.6` 隐私设计 ①）
//!
//! **进程名明文不出函数**：从 Win32 缓冲到哈希的全部中间态（UTF-16 解码串、
//! 归一化小写基名）都只存在于本模块**私有**作用域内——不跨模块、不进日志、
//! 不进存档、不返回调用方。本模块对外**唯一**的感知出参是 [`foreground_process_hash`]
//! 的 `Option<u64>`；可单测的纯函数只有 [`fnv1a64`]（输入为字节，不含进程名语义）
//! 与 `#[cfg(test)]` 作用域内的归一化断言。
//!
//! 口径（`02 §5.6`）：采样 0.2 Hz（5s），单次 ~40μs，CPU 增量 <0.02%。
//! 采样失败一律返回 `None`（不重试、不 panic，`02 §1.4`）。
//!
//! 边界：本模块**不做**忙碌档位判定（`busyness` 因子归 S7-M4，本模块只交付
//! 类别哈希与计数类原始量）；不新增 `pet://` 事件（C8）；零网络（C9）。

/// FNV-1a 64 位偏移基准（64-bit offset basis，FNV 规范常量）。
pub const FNV1A64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 位质数（FNV prime）。
pub const FNV1A64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64 位哈希（纯函数，可单测；`02 §5.6` 规定的「进程名 → u64」算法）。
///
/// 逐字节：`hash = (hash ^ byte) × prime`（先异或后乘，FNV-1a 变体）。
#[must_use]
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A64_OFFSET_BASIS;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(FNV1A64_PRIME);
    }
    hash
}

/// 归一化可执行文件名并哈希（**私有**：明文在此终止）。
///
/// 归一化规则（`02 §5.6`）：取路径分隔符（`\` 与 `/`）之后的基名 → 全小写。
/// 目的：同一可执行文件的不同调用路径（`C:\a\x.exe` 与 `C:\b\x.exe`）得到同一类别，
/// 且大小写不敏感（Windows 语义）。
///
/// 隐私：本函数是**明文唯一的出口之前一站**——其返回值仍是明文（小写基名），
/// 故**不得**提升为 `pub`；仅 [`foreground_process_hash`] 在函数内部消费它。
fn exe_category_hash(raw_path: &str) -> u64 {
    let base = raw_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(raw_path);
    fnv1a64(base.to_lowercase().as_bytes())
}

/// 当前前台窗口所属进程的**类别哈希**（`None` = 不可用：无前台窗口 / 句柄无效 /
/// 权限不足 / 读取失败）。
///
/// 返回值仅 `u64`——调用方（`dp-app` 装配）把它放进感知事件载荷即可，
/// **任何路径下都不会拿到进程名明文**（见模块文档「隐私红线」）。
#[must_use]
pub fn foreground_process_hash() -> Option<u64> {
    imp::foreground_process_hash()
}

#[cfg(windows)]
mod imp {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    /// 前台进程路径缓冲长度（字符数）：`MAX_PATH` 之上留余量，覆盖超长路径前缀场景。
    const EXE_PATH_CHARS: usize = (MAX_PATH as usize) * 2;

    /// `GetForegroundWindow` → 进程 id → 进程路径 → [`super::exe_category_hash`]。
    ///
    /// `unsafe` 说明：全部为只读查询类 Win32 调用；进程句柄在**同一函数内**成对
    /// `CloseHandle` 释放（含提前返回路径），不泄句柄。
    pub fn foreground_process_hash() -> Option<u64> {
        // SAFETY: 无入参、无副作用；返回 HWND（可能为 0 = 无前台窗口）。
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return None;
        }

        let mut pid = 0u32;
        // SAFETY: `pid` 为栈上有效可写变量；返回值为线程 id（0 = 失败）。
        let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if tid == 0 || pid == 0 {
            return None;
        }

        // SAFETY: 只申请查询权限；失败（如系统进程 / 会话隔离）返回 Err → `ok()?` 降级。
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;

        let mut buf = [0u16; EXE_PATH_CHARS];
        let mut len = EXE_PATH_CHARS as u32;
        // SAFETY: 缓冲区长度已按字符数给出；`len` 由 API 回写实际写入字符数。
        let queried = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
        }
        .is_ok();
        // SAFETY: 句柄由本函数创建，未移交他处；无论查询成败都必须释放。
        let _ = unsafe { CloseHandle(handle) };
        if !queried || len == 0 {
            return None;
        }

        // 明文到此为止：UTF-16 → String → 归一化 → 哈希，全程在本函数栈内。
        let raw = String::from_utf16_lossy(&buf[..len as usize]);
        Some(super::exe_category_hash(&raw))
    }
}

#[cfg(not(windows))]
mod imp {
    /// 非 Windows 平台占位（本项目仅面向 Windows；保留以维持模块可编译性）。
    pub fn foreground_process_hash() -> Option<u64> {
        None
    }
}

// ---------------------------------------------------------------------------
// 单元测试（哈希向量 / 归一化口径）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_matches_reference_vectors() {
        // FNV-1a 64 官方测试向量（offset basis 与首字符散列）。
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn fnv1a64_is_deterministic_and_collision_free_on_small_set() {
        let a = fnv1a64(b"chrome.exe");
        let b = fnv1a64(b"chrome.exe");
        assert_eq!(a, b, "同输入必须同输出（确定性）");
        assert_ne!(a, fnv1a64(b"code.exe"));
        assert_ne!(a, fnv1a64(b"chrome.exe "), "末尾空格属不同输入");
    }

    #[test]
    fn exe_category_hash_uses_lowercase_basename_only() {
        // 路径不同、基名相同 → 同一类别（隐私：路径不参与语义）。
        let a = exe_category_hash("C:\\Program Files\\X\\Chrome.EXE");
        let b = exe_category_hash("D:\\other\\chrome.exe");
        assert_eq!(a, b, "仅基名小写参与哈希");
        // 大小写不敏感（Windows 语义）。
        assert_eq!(exe_category_hash("CODE.exe"), exe_category_hash("code.EXE"));
        // 正/反斜杠均为分隔符。
        assert_eq!(exe_category_hash("a/b/c.exe"), exe_category_hash("a\\b\\c.exe"));
        // 无分隔符时整体即基名。
        assert_eq!(exe_category_hash("wechat.exe"), fnv1a64(b"wechat.exe"));
    }

    #[test]
    fn foreground_process_hash_is_callable_and_never_panics() {
        // 无头 / 无前台窗口环境返回 None 亦属合法降级（`02 §1.4`：失败不重试不崩）。
        let hash = foreground_process_hash();
        if let Some(h) = hash {
            assert_ne!(h, 0, "有效类别哈希不应为 0（偏移基准本身非 0）");
        }
    }
}
