//! 毛玻璃材质。
//!
//! 主屏壁纸是静态的，所以 Dock 等面板背后的模糊只需在壁纸变化时计算一次，
//! 平时零开销。动态内容上的实时模糊由合成器的 GPU 着色器负责（后续里程碑）。

use tiny_skia::{Color, FillRule, Paint, Pixmap, Rect, Transform};

/// 对单通道数据做近似高斯模糊（三次盒式模糊）。
pub fn blur_alpha(data: &mut [u8], width: usize, height: usize, radius: usize) {
    if radius == 0 || width == 0 || height == 0 {
        return;
    }
    let mut buf: Vec<f32> = data.iter().map(|&v| v as f32).collect();
    for _ in 0..3 {
        box_blur_channel(&mut buf, width, height, radius);
    }
    for (d, v) in data.iter_mut().zip(buf) {
        *d = v.round().clamp(0.0, 255.0) as u8;
    }
}

fn box_blur_channel(buf: &mut [f32], w: usize, h: usize, r: usize) {
    let mut tmp = vec![0.0f32; buf.len()];
    let norm = 1.0 / (2 * r + 1) as f32;
    // 横向
    for y in 0..h {
        let row = &buf[y * w..(y + 1) * w];
        let mut acc: f32 = (0..=r).map(|i| row[i.min(w - 1)]).sum::<f32>() + row[0] * r as f32;
        for x in 0..w {
            tmp[y * w + x] = acc * norm;
            let add = row[(x + r + 1).min(w - 1)];
            let sub = row[x.saturating_sub(r)];
            acc += add - sub;
        }
    }
    // 纵向
    for x in 0..w {
        let col = |y: usize| tmp[y.min(h - 1) * w + x];
        let mut acc: f32 = (0..=r).map(col).sum::<f32>() + col(0) * r as f32;
        for y in 0..h {
            buf[y * w + x] = acc * norm;
            acc += col(y + r + 1) - col(y.saturating_sub(r));
        }
    }
}

/// 对整张 RGBA 位图做模糊。
pub fn blur(pixmap: &mut Pixmap, radius: usize) {
    let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
    let data = pixmap.data_mut();
    for c in 0..4 {
        let mut ch: Vec<u8> = data.iter().skip(c).step_by(4).copied().collect();
        blur_alpha(&mut ch, w, h, radius);
        for (i, v) in ch.into_iter().enumerate() {
            data[i * 4 + c] = v;
        }
    }
}

/// 生成毛玻璃面板：取 `background` 中 `rect` 区域，缩小后模糊再放大（省时），
/// 叠加 `tint`，并裁成连续圆角。
pub fn frosted_panel(
    background: &Pixmap,
    rect: Rect,
    radius: f32,
    blur_radius: f32,
    tint: Color,
) -> Pixmap {
    let (w, h) = (rect.width().ceil() as u32, rect.height().ceil() as u32);
    // 1/4 分辨率模糊，效果相同但快 16 倍。
    const DOWN: f32 = 4.0;
    let (sw, sh) = (
        ((w as f32) / DOWN).ceil() as u32 + 1,
        ((h as f32) / DOWN).ceil() as u32 + 1,
    );
    let mut small = crate::pixmap(sw, sh);
    let pattern = tiny_skia::Pattern::new(
        background.as_ref(),
        tiny_skia::SpreadMode::Pad,
        tiny_skia::FilterQuality::Bilinear,
        1.0,
        Transform::from_translate(-rect.x(), -rect.y()).post_scale(1.0 / DOWN, 1.0 / DOWN),
    );
    let paint = Paint {
        shader: pattern,
        ..Paint::default()
    };
    small.fill_rect(
        Rect::from_xywh(0.0, 0.0, sw as f32, sh as f32).unwrap(),
        &paint,
        Transform::identity(),
        None,
    );
    blur(&mut small, ((blur_radius / DOWN).round() as usize).max(1));

    let mut out = crate::pixmap(w, h);
    let path = crate::shape::continuous_rect(
        Rect::from_xywh(0.0, 0.0, w as f32, h as f32).unwrap(),
        radius,
    );
    let up = tiny_skia::Pattern::new(
        small.as_ref(),
        tiny_skia::SpreadMode::Pad,
        tiny_skia::FilterQuality::Bicubic,
        1.0,
        Transform::from_scale(DOWN, DOWN),
    );
    let paint = Paint {
        shader: up,
        anti_alias: true,
        ..Paint::default()
    };
    out.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    let mut tint_paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    tint_paint.set_color(tint);
    out.fill_path(
        &path,
        &tint_paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_preserves_flat_image_and_spreads_points() {
        let mut flat = vec![100u8; 64 * 64];
        blur_alpha(&mut flat, 64, 64, 4);
        assert!(flat.iter().all(|&v| v == 100));

        let mut dot = vec![0u8; 31 * 31];
        dot[15 * 31 + 15] = 255;
        blur_alpha(&mut dot, 31, 31, 2);
        assert!(dot[15 * 31 + 15] < 255 && dot[15 * 31 + 17] > 0);
    }
}
