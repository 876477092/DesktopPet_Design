//! S3-M1 QA 独立验证（**纯测试代码，不改任何生产代码**）。
//!
//! 目的：证明 `ports.rs` 新增的 `impl dp_platform::win::hook::HitTest for PetBBoxHandle`
//! 与既有 `PetBBoxHandle::contains` **逐位同构**（四角 / 边界 / 退化 / 负坐标域），
//! 即 S3-M1 新建的跨 crate 端口没有引入第二套包含语义。
//!
//! 覆盖矩阵（设计补充 §2.2 / 派工单 E3）：
//!   - 四角 + 半开边界（右开 / 下开）；
//!   - 退化：`left == right`（零宽）、`top == bottom`（零高）、`left > right`、
//!     `top > bottom`、全零（未刷新 bbox）——须恒 `false`、不 panic；
//!   - 负坐标域（左侧副屏）：半开区间在负区间同样成立；
//!   - 每个点同时断言 **trait 方法** 与 **固有方法** 结果一致。

#![cfg(windows)]

use dp_app::ports::PetBBoxHandle;
use dp_platform::win::hook::HitTest;

/// 对给定矩形与点集，逐点断言 trait 方法与固有方法结果**逐位一致**。
fn assert_isomorphic(rect: (i32, i32, i32, i32), pts: &[(i32, i32)]) {
    let h = PetBBoxHandle::new();
    h.store(rect.0, rect.1, rect.2, rect.3);
    for &(x, y) in pts {
        let via_trait = HitTest::contains(&h, x, y);
        let via_inherent = PetBBoxHandle::contains(&h, x, y);
        assert_eq!(
            via_trait,
            via_inherent,
            "trait/inherent 不一致 @ ({x},{y}) rect={rect:?}（应逐位同构）"
        );
    }
}

#[test]
fn hittest_impl_four_corners_and_half_open_edges() {
    let rect = (10, 30, 20, 40);
    // 九个点：左上闭 / 右下减一闭 / 右开 / 下开 / 四向外 / 内点。
    assert_isomorphic(
        rect,
        &[
            (10, 30), // 左上闭
            (19, 39), // 右下减一闭
            (15, 35), // 内点
            (20, 30), // 右开（x == right）
            (10, 40), // 下开（y == bottom）
            (20, 40), // 右下角外（双开）
            (9, 30),  // 左外
            (25, 35), // 右外
            (10, 29), // 上外
            (15, 45), // 下外
        ],
    );

    // 显式语义（防「trait 与固有方法一起错」的盲区）。
    let h = PetBBoxHandle::new();
    h.store(10, 30, 20, 40);
    assert!(HitTest::contains(&h, 10, 30), "左上闭");
    assert!(HitTest::contains(&h, 19, 39), "右下减一闭");
    assert!(!HitTest::contains(&h, 20, 30), "右开");
    assert!(!HitTest::contains(&h, 10, 40), "下开");
}

#[test]
fn hittest_impl_degenerate_rects_are_false_and_do_not_panic() {
    // 零宽 / 零高 / left>right / top>bottom / 全零（未刷新 bbox）。
    for rect in [
        (10, 30, 10, 40),
        (10, 30, 20, 30),
        (10, 40, 20, 30),
        (20, 30, 10, 40),
        (0, 0, 0, 0),
    ] {
        assert_isomorphic(rect, &[(10, 35), (15, 30), (15, 35), (0, 0), (-5, -5)]);
        // 退化矩形任一点都不得命中。
        let h = PetBBoxHandle::new();
        h.store(rect.0, rect.1, rect.2, rect.3);
        assert!(!HitTest::contains(&h, rect.0, rect.1), "退化 rect={rect:?} 不应命中");
    }
}

#[test]
fn hittest_impl_negative_coordinate_domain() {
    // 左侧副屏：物理域 x ∈ [-1920, 0)，半开区间在负区间同样成立。
    assert_isomorphic(
        (-1920, 0, 0, 1080),
        &[(-1, 0), (0, 0), (-1920, 0), (-1921, 0), (-960, 540), (-960, 1080)],
    );
    let h = PetBBoxHandle::new();
    h.store(-1920, 0, 0, 1080);
    assert!(HitTest::contains(&h, -1, 0), "负域左上闭");
    assert!(!HitTest::contains(&h, 0, 0), "负域右开");
    assert!(!HitTest::contains(&h, -1921, 0), "负域左外");
}
