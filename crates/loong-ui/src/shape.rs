//! 连续圆角。
//!
//! 普通圆角（圆弧）与直边相接处曲率突变，看起来有一道“折痕”。连续圆角用超椭圆
//! 曲线 |x|ⁿ + |y|ⁿ = 1（n = 4）代替圆弧：曲线在两端曲率为 0，与直边平滑过渡。
//! 为了让视觉大小接近同半径的圆角，曲线范围放大到 `radius × EXTENT`。
//!
//! 合成器里的窗口圆角着色器使用同样的参数，保证图标、卡片、窗口观感一致。

use tiny_skia::{Path, PathBuilder, Rect};

/// 超椭圆指数。
pub const EXPONENT: f32 = 4.0;
/// 角曲线覆盖范围相对“名义半径”的倍数。
pub const EXTENT: f32 = 1.28;

/// 连续圆角矩形路径。`radius` 为名义圆角半径。
pub fn continuous_rect(rect: Rect, radius: f32) -> Path {
    let (x, y, w, h) = (rect.x(), rect.y(), rect.width(), rect.height());
    let a = (radius * EXTENT).min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pb = PathBuilder::new();
    if a <= 0.01 {
        pb.push_rect(rect);
        return pb.finish().expect("矩形路径有效");
    }
    const STEPS: usize = 24;
    // 四个角的中心（曲线所在象限的原点）与方向。
    let corners = [
        (x + w - a, y + a, 1.0f32, -1.0f32), // 右上：从顶边到右边
        (x + w - a, y + h - a, 1.0, 1.0),    // 右下
        (x + a, y + h - a, -1.0, 1.0),       // 左下
        (x + a, y + a, -1.0, -1.0),          // 左上
    ];
    pb.move_to(x + a, y);
    for (ci, &(cx, cy, sx, sy)) in corners.iter().enumerate() {
        for i in 0..=STEPS {
            // 参数 t 从 0 到 π/2；依角的位置决定走向，保证路径顺时针连续。
            let t = (i as f32 / STEPS as f32) * std::f32::consts::FRAC_PI_2;
            // cos(π/2) 在浮点下是极小的负数，开方会得到 NaN，先夹到 0。
            let (s, c) = if ci % 2 == 0 {
                (t.cos(), t.sin())
            } else {
                (t.sin(), t.cos())
            };
            let (s, c) = (s.max(0.0), c.max(0.0));
            let px = cx + sx * a * c.powf(2.0 / EXPONENT);
            let py = cy + sy * a * s.powf(2.0 / EXPONENT);
            pb.line_to(px, py);
        }
    }
    pb.close();
    pb.finish().expect("圆角路径有效")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_stays_inside_rect_and_touches_edges() {
        let r = Rect::from_xywh(10.0, 20.0, 100.0, 60.0).unwrap();
        let p = continuous_rect(r, 12.0);
        let b = p.bounds();
        assert!((b.left() - 10.0).abs() < 0.01 && (b.right() - 110.0).abs() < 0.01);
        assert!((b.top() - 20.0).abs() < 0.01 && (b.bottom() - 80.0).abs() < 0.01);
    }

    #[test]
    fn huge_radius_is_clamped() {
        let r = Rect::from_xywh(0.0, 0.0, 40.0, 40.0).unwrap();
        let b = continuous_rect(r, 500.0).bounds();
        assert!(b.width() <= 40.01 && b.height() <= 40.01);
    }
}
