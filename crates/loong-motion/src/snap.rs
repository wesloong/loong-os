//! 手势结束时的落点决策。
//!
//! 核心思路（来自 iOS 的流体界面设计）：松手时不看“手指停在哪”，而看
//! “按当前速度惯性滑行会停在哪”，再吸附到最近的合法位置。这样轻轻一甩
//! 也能翻页，慢慢拖过一半也能翻页，符合直觉。

use crate::Decay;

/// 预测落点后，选出最近的吸附点。`points` 为空时返回预测落点本身。
pub fn nearest_snap_point(position: f64, velocity: f64, decay: Decay, points: &[f64]) -> f64 {
    let projected = position + decay.project(velocity);
    points
        .iter()
        .copied()
        .min_by(|a, b| (a - projected).abs().total_cmp(&(b - projected).abs()))
        .unwrap_or(projected)
}

/// 分页滚动松手时的目标页。
///
/// - `offset`：当前滚动偏移（第 n 页起点为 `n * page_size`）；
/// - `velocity`：偏移的变化速度（单位/秒，正数表示往后翻）；
/// - `start_page`：手势开始时所在页。一次手势最多翻一页，和 iPad 主屏一致。
pub fn page_for_release(
    offset: f64,
    velocity: f64,
    page_size: f64,
    page_count: usize,
    start_page: usize,
) -> usize {
    if page_count == 0 || page_size <= 0.0 {
        return 0;
    }
    let projected = offset + Decay::FAST.project(velocity);
    let raw = (projected / page_size).round();
    let lo = start_page.saturating_sub(1) as f64;
    let hi = (start_page + 1).min(page_count - 1) as f64;
    raw.clamp(lo, hi) as usize
}

/// 二选一的手势（例如：回主屏 or 留在应用；打开面板 or 关闭面板）。
///
/// `progress` 为手势进度 0..1，`velocity` 为进度速度（1/秒）。
/// 返回 `true` 表示应完成手势（进度去 1）。
pub fn should_complete(progress: f64, velocity: f64, decay: Decay) -> bool {
    progress + decay.project(velocity) > 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_drag_snaps_to_nearest() {
        let pts = [0.0, 100.0, 200.0];
        assert_eq!(nearest_snap_point(40.0, 0.0, Decay::NORMAL, &pts), 0.0);
        assert_eq!(nearest_snap_point(60.0, 0.0, Decay::NORMAL, &pts), 100.0);
    }

    #[test]
    fn flick_carries_to_next_point() {
        let pts = [0.0, 100.0, 200.0];
        // 只拖了 20，但速度很快：应该去 100 甚至更远。
        assert_eq!(nearest_snap_point(20.0, 300.0, Decay::NORMAL, &pts), 200.0);
        assert_eq!(nearest_snap_point(20.0, 200.0, Decay::FAST, &pts), 0.0);
        assert_eq!(nearest_snap_point(20.0, 800.0, Decay::FAST, &pts), 100.0);
    }

    #[test]
    fn paging_moves_at_most_one_page() {
        let w = 1000.0;
        // 慢拖过半：翻页
        assert_eq!(page_for_release(1600.0, 0.0, w, 5, 1), 2);
        // 慢拖不到一半：回弹
        assert_eq!(page_for_release(1300.0, 0.0, w, 5, 1), 1);
        // 轻甩：翻页
        assert_eq!(page_for_release(1100.0, 6000.0, w, 5, 1), 2);
        // 大力甩：也只翻一页
        assert_eq!(page_for_release(1100.0, 90000.0, w, 5, 1), 2);
        // 往回甩
        assert_eq!(page_for_release(950.0, -6000.0, w, 5, 1), 0);
        // 边界
        assert_eq!(page_for_release(4100.0, 9000.0, w, 5, 4), 4);
        assert_eq!(page_for_release(-50.0, -9000.0, w, 5, 0), 0);
    }

    #[test]
    fn binary_gesture_decision() {
        assert!(!should_complete(0.3, 0.0, Decay::FAST));
        assert!(should_complete(0.6, 0.0, Decay::FAST));
        assert!(should_complete(0.2, 5.0, Decay::FAST));
        assert!(!should_complete(0.7, -5.0, Decay::FAST));
    }
}
