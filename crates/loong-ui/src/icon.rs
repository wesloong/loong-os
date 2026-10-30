//! 应用图标与符号。
//!
//! 线性符号来自 Lucide（ISC 许可，见 `assets/icons/LICENSE`）。图标底板为
//! 连续圆角 + 渐变 + 顶部高光，由本模块绘制。

use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, Pixmap, Point, Rect, SpreadMode,
    Transform,
};

/// 内置符号（Lucide）。
pub fn builtin_symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "compass" => include_str!("../assets/icons/compass.svg"),
        "settings" => include_str!("../assets/icons/settings.svg"),
        "image" => include_str!("../assets/icons/image.svg"),
        "music" => include_str!("../assets/icons/music.svg"),
        "notebook-pen" => include_str!("../assets/icons/notebook-pen.svg"),
        "calendar" => include_str!("../assets/icons/calendar.svg"),
        "clock" => include_str!("../assets/icons/clock.svg"),
        "folder" => include_str!("../assets/icons/folder.svg"),
        "camera" => include_str!("../assets/icons/camera.svg"),
        "map" => include_str!("../assets/icons/map.svg"),
        "mail" => include_str!("../assets/icons/mail.svg"),
        "message-circle" => include_str!("../assets/icons/message-circle.svg"),
        "calculator" => include_str!("../assets/icons/calculator.svg"),
        "cloud-sun" => include_str!("../assets/icons/cloud-sun.svg"),
        "play-circle" => include_str!("../assets/icons/play-circle.svg"),
        "book-open" => include_str!("../assets/icons/book-open.svg"),
        "wifi" => include_str!("../assets/icons/wifi.svg"),
        "battery-full" => include_str!("../assets/icons/battery-full.svg"),
        _ => return None,
    })
}

/// 把 SVG 符号以指定颜色绘制到 `target` 的 `rect` 区域内。
pub fn draw_symbol(
    target: &mut Pixmap,
    svg: &str,
    rect: Rect,
    color: Color,
    stroke_width: Option<f32>,
) {
    let rgb = format!(
        "#{:02x}{:02x}{:02x}",
        (color.red() * 255.0) as u8,
        (color.green() * 255.0) as u8,
        (color.blue() * 255.0) as u8
    );
    let mut svg = svg.replace("currentColor", &rgb);
    if let Some(sw) = stroke_width {
        svg = svg.replace("stroke-width=\"2\"", &format!("stroke-width=\"{sw}\""));
    }
    let Ok(tree) = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()) else {
        return;
    };
    let size = tree.size();
    let scale = (rect.width() / size.width()).min(rect.height() / size.height());
    let tx = rect.x() + (rect.width() - size.width() * scale) / 2.0;
    let ty = rect.y() + (rect.height() - size.height() * scale) / 2.0;
    let transform = Transform::from_scale(scale, scale).post_translate(tx, ty);
    if color.alpha() >= 0.999 {
        resvg::render(&tree, transform, &mut target.as_mut());
    } else {
        let mut layer = crate::pixmap(target.width(), target.height());
        resvg::render(&tree, transform, &mut layer.as_mut());
        let paint = tiny_skia::PixmapPaint {
            opacity: color.alpha(),
            ..Default::default()
        };
        target.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), None);
    }
}

/// 应用图标外观。
#[derive(Debug, Clone, Copy)]
pub struct IconStyle {
    /// 渐变顶部颜色。
    pub top: Color,
    /// 渐变底部颜色。
    pub bottom: Color,
    /// 符号颜色。
    pub glyph: Color,
}

/// 绘制 `size`×`size` 像素的应用图标。
pub fn app_icon(size: u32, symbol_svg: &str, style: IconStyle) -> Pixmap {
    let mut pm = crate::pixmap(size, size);
    let s = size as f32;
    let rect = Rect::from_xywh(0.0, 0.0, s, s).unwrap();
    let path = crate::shape::continuous_rect(rect, s * crate::theme::radius::ICON_RATIO);

    let paint = Paint {
        anti_alias: true,
        shader: LinearGradient::new(
            Point::from_xy(0.0, 0.0),
            Point::from_xy(0.0, s),
            vec![
                GradientStop::new(0.0, style.top),
                GradientStop::new(1.0, style.bottom),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        )
        .expect("渐变有效"),
        ..Paint::default()
    };
    pm.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    // 顶部柔和高光，增加层次感。
    let hl = Paint {
        anti_alias: true,
        shader: LinearGradient::new(
            Point::from_xy(0.0, 0.0),
            Point::from_xy(0.0, s * 0.55),
            vec![
                GradientStop::new(0.0, Color::from_rgba8(255, 255, 255, 46)),
                GradientStop::new(1.0, Color::from_rgba8(255, 255, 255, 0)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        )
        .expect("渐变有效"),
        ..Paint::default()
    };
    pm.fill_path(&path, &hl, FillRule::Winding, Transform::identity(), None);

    let inset = s * 0.24;
    let glyph_rect = Rect::from_xywh(inset, inset, s - 2.0 * inset, s - 2.0 * inset).unwrap();
    draw_symbol(&mut pm, symbol_svg, glyph_rect, style.glyph, Some(1.75));
    pm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtin_symbols_parse() {
        for name in [
            "compass",
            "settings",
            "image",
            "music",
            "notebook-pen",
            "calendar",
            "clock",
            "folder",
            "camera",
            "map",
            "mail",
            "message-circle",
            "calculator",
            "cloud-sun",
            "play-circle",
            "book-open",
            "wifi",
            "battery-full",
        ] {
            let svg = builtin_symbol(name).unwrap();
            assert!(
                resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).is_ok(),
                "{name}"
            );
        }
    }

    #[test]
    fn icon_has_transparent_corners_and_opaque_center() {
        let style = IconStyle {
            top: Color::from_rgba8(40, 200, 180, 255),
            bottom: Color::from_rgba8(10, 120, 140, 255),
            glyph: Color::WHITE,
        };
        let pm = app_icon(120, builtin_symbol("compass").unwrap(), style);
        assert_eq!(pm.pixel(0, 0).unwrap().alpha(), 0);
        assert_eq!(pm.pixel(60, 10).unwrap().alpha(), 255);
        // 符号是白色线条：中心区域应能找到接近白色的像素。
        let has_white = (30..90).any(|x| {
            let p = pm.pixel(x, 60).unwrap();
            p.red() > 230 && p.green() > 230 && p.blue() > 230
        });
        assert!(has_white);
    }
}
