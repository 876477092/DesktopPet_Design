//! QA 独立探针测试（S2-M6 窗口枚举过滤 + S2-M7 光标/系统采样纯函数，回归验证）。
//!
//! 由 QA（严过关）新增，**不改动任何生产代码**。仅使用 `dp-platform` 公开 API，
//! 目标：在工程师自测之外，独立复现并尝试证伪其结论。
//!
//! 覆盖点（对应 QA 任务第 4 条）：
//!   P7 winenum 过滤纯函数：不可见 / 最小化 / Shell / 零标题（各一条构造输入）
//!      + TOOLWINDOW 位 + cloaked → 均被过滤；全干净基线通过；
//!   P8 cursor 速度纯函数：零时间间隔防除零（dt=0 / dt<0 / 静止点 dt=0 →
//!      None，绝不产生 inf/NaN）；极小 dt=1ms 结果有限；
//!   P9 system 负载差分纯函数：total=0 / idle>total 防御 + 正常值。
//!
//! 标注：本文件为 QA 交付物，非工程师生产代码。

#![cfg(windows)]

use dp_platform::traits::Vec2;
use dp_platform::win::cursor::cursor_speed_px_per_sec;
use dp_platform::win::system::load_from_deltas;
use dp_platform::win::winenum::titlebar_candidate;

// ---------------------------------------------------------------------------
// P7 winenum 过滤纯函数（六条规则，逐条构造输入验证被过滤）
// ---------------------------------------------------------------------------

#[test]
fn qa_titlebar_candidate_filters_each_bad_input() {
    // 全干净基线：可见 / 非最小化 / 非壳层 / 无工具窗口位 / 标题非空 / 未 cloaked。
    assert!(titlebar_candidate(true, false, false, 0, 4, false), "干净基线应为候选");

    // 逐条构造单一坏输入（其余条件全干净）→ 必须被过滤。
    assert!(
        !titlebar_candidate(false, false, false, 0, 4, false),
        "不可见窗口必须被过滤"
    );
    assert!(
        !titlebar_candidate(true, true, false, 0, 4, false),
        "最小化窗口必须被过滤"
    );
    assert!(
        !titlebar_candidate(true, false, true, 0, 4, false),
        "Shell 窗口必须被过滤"
    );
    assert!(
        !titlebar_candidate(true, false, false, 0, 0, false),
        "零标题窗口必须被过滤"
    );
    // WS_EX_TOOLWINDOW = 0x0000_0080（Win32 定义，与 winenum 实现同值）。
    assert!(
        !titlebar_candidate(true, false, false, 0x0000_0080, 4, false),
        "TOOLWINDOW 位窗口必须被过滤"
    );
    assert!(
        !titlebar_candidate(true, false, false, 0, 4, true),
        "DWM cloaked（UWP 幽灵）必须被过滤"
    );

    // 其余扩展样式位不得误伤（WS_EX_TOPMOST = 0x0000_0008）。
    assert!(
        titlebar_candidate(true, false, false, 0x0000_0008, 1, false),
        "其他 EXSTYLE 位不参与判定"
    );
}

// ---------------------------------------------------------------------------
// P8 cursor 速度纯函数：零时间间隔防除零
// ---------------------------------------------------------------------------

#[test]
fn qa_cursor_speed_zero_interval_never_divides_by_zero() {
    // dt=0 且位移非零 → None（不得 panic / inf / NaN）。
    let r = cursor_speed_px_per_sec(Vec2::ZERO, 1_000, Vec2::new(300.0, 400.0), 1_000);
    assert!(r.is_none(), "dt=0 须返回 None，实际 {r:?}");
    // dt=0 且两点重合（静止）→ 仍 None（而非 0.0）。
    let r = cursor_speed_px_per_sec(Vec2::new(5.0, 5.0), 1_000, Vec2::new(5.0, 5.0), 1_000);
    assert!(r.is_none(), "静止点 dt=0 须返回 None，实际 {r:?}");
    // dt<0（时间倒流）→ None。
    let r = cursor_speed_px_per_sec(Vec2::ZERO, 2_000, Vec2::new(1.0, 1.0), 1_000);
    assert!(r.is_none(), "dt<0 须返回 None，实际 {r:?}");
}

#[test]
fn qa_cursor_speed_one_ms_interval_is_finite_and_correct() {
    // 3-4-5 位移 5px / 1ms = 5000 px/s：极小间隔结果有限且数值正确。
    let s = cursor_speed_px_per_sec(Vec2::ZERO, 1_000, Vec2::new(3.0, 4.0), 1_001)
        .expect("dt=1ms > 0 应有值");
    assert!(s.is_finite(), "速度必须有限");
    assert!((s - 5000.0).abs() < 1e-3, "5px/1ms = 5000px/s，实际 {s}");
}

// ---------------------------------------------------------------------------
// P9 system 负载差分纯函数
// ---------------------------------------------------------------------------

#[test]
fn qa_load_from_deltas_defense_and_values() {
    assert!(load_from_deltas(0, 0).is_none(), "total=0（时间未前进）→ None");
    assert!(load_from_deltas(1, 0).is_none(), "total=0 且 idle>0 → None");
    assert!(load_from_deltas(401, 400).is_none(), "idle_d > total_d → None");
    let q = load_from_deltas(100, 400).expect("正常差分应有值");
    assert!((q - 0.75).abs() < 1e-6, "idle 100/400 → 负载 0.75，实际 {q}");
    assert!((load_from_deltas(400, 400).unwrap()).abs() < 1e-6, "全空闲 → 0");
    assert!((load_from_deltas(0, 400).unwrap() - 1.0).abs() < 1e-6, "全忙 → 1");
}
