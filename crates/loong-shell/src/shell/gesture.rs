//! 手势识别的通用部分：方向锁定、带橡皮筋的滚动范围。

use loong_motion::{RUBBER_BAND_COEFFICIENT, rubber_band_clamp, rubber_band_inverse};

/// 拖动方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// 方向锁定（设计规范 9.2）：位移超过起手阈值 `slop` 后，与某条主轴的夹角小于 30°
/// 就锁定为该方向；落在两者之间时继续等待，位移超过 3 倍阈值仍未确定则取位移较大的方向。
pub fn lock_axis(dx: f64, dy: f64, slop: f64) -> Option<Axis> {
    let distance = dx.hypot(dy);
    if distance <= slop {
        return None;
    }
    // 与水平轴的夹角（0..90°）。
    let angle = dy.abs().atan2(dx.abs()).to_degrees();
    if angle < 30.0 {
        Some(Axis::Horizontal)
    } else if angle > 60.0 {
        Some(Axis::Vertical)
    } else if distance > 3.0 * slop {
        Some(if dx.abs() >= dy.abs() {
            Axis::Horizontal
        } else {
            Axis::Vertical
        })
    } else {
        None
    }
}

/// 拖动时：手指对应的位置 `raw` → 显示的位置。`[lo, hi]` 以内 1:1，越界部分按橡皮筋
/// 阻尼（设计规范 7.4，`dimension` 为该方向上的可视尺寸）。
pub fn rubber_clamp(raw: f64, lo: f64, hi: f64, dimension: f64) -> f64 {
    rubber_band_clamp(raw, lo, hi, dimension, RUBBER_BAND_COEFFICIENT)
}

/// [`rubber_clamp`] 的反函数：手指按到正在回弹的内容时，由显示位置还原手指位置，避免跳变。
pub fn rubber_clamp_inverse(shown: f64, lo: f64, hi: f64, dimension: f64) -> f64 {
    let c = RUBBER_BAND_COEFFICIENT;
    if shown < lo {
        lo + rubber_band_inverse(shown - lo, dimension, c)
    } else if shown > hi {
        hi + rubber_band_inverse(shown - hi, dimension, c)
    } else {
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_lock() {
        assert_eq!(lock_axis(3.0, 4.0, 8.0), None);
        assert_eq!(lock_axis(20.0, 5.0, 8.0), Some(Axis::Horizontal));
        assert_eq!(lock_axis(-20.0, 5.0, 8.0), Some(Axis::Horizontal));
        assert_eq!(lock_axis(3.0, -20.0, 8.0), Some(Axis::Vertical));
        // 45° 附近：先等待，走远了再按主导方向决定。
        assert_eq!(lock_axis(8.0, -9.0, 8.0), None);
        assert_eq!(lock_axis(20.0, -22.0, 8.0), Some(Axis::Vertical));
        assert_eq!(lock_axis(-22.0, 20.0, 8.0), Some(Axis::Horizontal));
    }

    #[test]
    fn rubber_clamp_is_invertible() {
        let (lo, hi, d) = (0.0, 1280.0, 1280.0);
        assert_eq!(rubber_clamp(600.0, lo, hi, d), 600.0);
        let over = rubber_clamp(hi + 300.0, lo, hi, d);
        assert!(over > hi && over < hi + 300.0 * 0.56);
        for raw in [-900.0, -1.0, 0.0, 640.0, 1300.0, 3000.0] {
            let shown = rubber_clamp(raw, lo, hi, d);
            assert!(
                (rubber_clamp_inverse(shown, lo, hi, d) - raw).abs() < 1e-6,
                "{raw}"
            );
        }
    }
}
