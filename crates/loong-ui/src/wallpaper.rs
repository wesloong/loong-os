//! 默认壁纸：深色底 + 几团柔和的彩色光晕（青、蓝、紫），程序生成，零资源文件。

use tiny_skia::{
    Color, GradientStop, Paint, Pixmap, Point, RadialGradient, Rect, SpreadMode, Transform,
};

/// 生成 `width`×`height` 的默认壁纸。结果只与尺寸有关（确定性）。
pub fn default_wallpaper(width: u32, height: u32) -> Pixmap {
    let mut pm = crate::pixmap(width, height);
    let (w, h) = (width as f32, height as f32);
    pm.fill(Color::from_rgba8(12, 18, 38, 255));

    // (中心 x, 中心 y, 半径, 颜色)，坐标为相对尺寸。
    let blobs = [
        (
            0.18,
            0.22,
            0.75,
            crate::theme::hex(crate::theme::ACCENT, 0.85),
        ),
        (0.85, 0.30, 0.65, Color::from_rgba8(59, 91, 219, 220)),
        (0.62, 0.92, 0.80, Color::from_rgba8(124, 58, 237, 190)),
        (0.05, 0.95, 0.55, Color::from_rgba8(14, 116, 144, 200)),
    ];
    let diag = (w * w + h * h).sqrt();
    for (cx, cy, r, color) in blobs {
        let mut transparent = color;
        transparent.set_alpha(0.0);
        let shader = RadialGradient::new(
            Point::from_xy(cx * w, cy * h),
            0.0,
            Point::from_xy(cx * w, cy * h),
            r * diag * 0.6,
            vec![
                GradientStop::new(0.0, color),
                GradientStop::new(0.55, {
                    let mut c = color;
                    c.set_alpha(color.alpha() * 0.35);
                    c
                }),
                GradientStop::new(1.0, transparent),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        );
        if let Some(shader) = shader {
            let paint = Paint {
                shader,
                anti_alias: false,
                ..Paint::default()
            };
            pm.fill_rect(
                Rect::from_xywh(0.0, 0.0, w, h).unwrap(),
                &paint,
                Transform::identity(),
                None,
            );
        }
    }
    pm
}
