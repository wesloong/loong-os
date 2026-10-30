/// 橡皮筋系数。0.55 是 iOS 滚动越界的经验值：越界越多阻力越大，但永远拉不到 `dimension`。
pub const RUBBER_BAND_COEFFICIENT: f64 = 0.55;

/// 橡皮筋阻尼：手指越界 `overshoot` 时，内容实际显示的越界量。
///
/// `f(x) = (1 − 1 / (x·c/d + 1)) · d`，其中 `d` 为可视区域尺寸，`c` 为系数。
/// 起始斜率为 `c`，并随越界量增大而趋近于 `d`。保留符号。
pub fn rubber_band(overshoot: f64, dimension: f64, coefficient: f64) -> f64 {
    if dimension <= 0.0 {
        return 0.0;
    }
    let x = overshoot.abs();
    let y = (1.0 - 1.0 / (x * coefficient / dimension + 1.0)) * dimension;
    y.copysign(overshoot)
}

/// [`rubber_band`] 的反函数：由显示的越界量求手指越界量。
///
/// 当动画进行到越界区域时用户再次按下手指，需要用它恢复“手指坐标”，避免跳变。
pub fn rubber_band_inverse(displayed: f64, dimension: f64, coefficient: f64) -> f64 {
    if dimension <= 0.0 || coefficient <= 0.0 {
        return 0.0;
    }
    let y = displayed.abs().min(dimension * (1.0 - 1e-9));
    let x = dimension / coefficient * (1.0 / (1.0 - y / dimension) - 1.0);
    x.copysign(displayed)
}

/// 把 `value` 限制在 `[min, max]` 内，越界部分按橡皮筋阻尼显示。
pub fn rubber_band_clamp(value: f64, min: f64, max: f64, dimension: f64, coefficient: f64) -> f64 {
    if value < min {
        min + rubber_band(value - min, dimension, coefficient)
    } else if value > max {
        max + rubber_band(value - max, dimension, coefficient)
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const C: f64 = RUBBER_BAND_COEFFICIENT;

    #[test]
    fn starts_with_slope_c_and_stays_below_dimension() {
        let d = 800.0;
        assert_eq!(rubber_band(0.0, d, C), 0.0);
        let slope = rubber_band(0.01, d, C) / 0.01;
        assert!((slope - C).abs() < 1e-4);
        let mut prev = 0.0;
        for i in 1..1000 {
            let y = rubber_band(i as f64 * 10.0, d, C);
            assert!(y > prev && y < d);
            prev = y;
        }
    }

    #[test]
    fn symmetric_and_invertible() {
        let d = 1024.0;
        for x in [-500.0, -3.0, 0.5, 42.0, 900.0] {
            let y = rubber_band(x, d, C);
            assert!((rubber_band(-x, d, C) + y).abs() < 1e-12);
            assert!((rubber_band_inverse(y, d, C) - x).abs() < 1e-6);
        }
    }

    #[test]
    fn clamp_only_affects_overshoot() {
        assert_eq!(rubber_band_clamp(50.0, 0.0, 100.0, 300.0, C), 50.0);
        let over = rubber_band_clamp(160.0, 0.0, 100.0, 300.0, C);
        assert!(over > 100.0 && over < 160.0);
        let under = rubber_band_clamp(-60.0, 0.0, 100.0, 300.0, C);
        assert!(under < 0.0 && under > -60.0);
    }
}
