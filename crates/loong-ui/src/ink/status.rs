//! 状态栏控件：毛笔画的电池与 Wi‑Fi。尺寸很小（14–18 pt），笔法收敛：
//! 粗细变化轻、几乎不飞白，保证一眼可读。

use tiny_skia::{PathBuilder, Pixmap, Rect, Transform};

use super::brush::{self, BrushStyle, Stroke};
use super::raster::{blend_field, rgb, smoothstep};
use crate::theme::{Ink, Theme, pigment};

fn small_style(width: f32) -> BrushStyle {
    let mut s = BrushStyle::for_width(width);
    // 笔压照常（粗细变化在小尺寸下仍可见、且不损可读性），只收住飞白与晕。
    s.pressure = 0.35 + 0.45 * smoothstep(1.5, 3.5, width);
    s.dryness = s.dryness.min(0.08);
    s.bleed = s.bleed.min(0.12);
    s.texture = s.texture.min(0.25);
    s
}

/// 圆角矩形笔路（顺时针，从左上角起笔）。
fn round_rect(r: Rect, radius: f32) -> Stroke {
    let mut pb = PathBuilder::new();
    let (x0, y0, x1, y1) = (r.left(), r.top(), r.right(), r.bottom());
    let k = radius * 0.5523;
    pb.move_to(x0 + radius, y0);
    pb.line_to(x1 - radius, y0);
    pb.cubic_to(x1 - radius + k, y0, x1, y0 + radius - k, x1, y0 + radius);
    pb.line_to(x1, y1 - radius);
    pb.cubic_to(x1, y1 - radius + k, x1 - radius + k, y1, x1 - radius, y1);
    pb.line_to(x0 + radius, y1);
    pb.cubic_to(x0 + radius - k, y1, x0, y1 - radius + k, x0, y1 - radius);
    pb.line_to(x0, y0 + radius);
    pb.cubic_to(x0, y0 + radius - k, x0 + radius - k, y0, x0 + radius, y0);
    pb.close();
    let path = pb.finish().expect("圆角矩形");
    let mut out = Vec::new();
    brush::flatten(&path, Transform::identity(), &mut out);
    let mut s = out.remove(0);
    brush::normalize_direction(&mut s);
    s
}

/// 状态栏电池：宽 = 2 × 高。
pub fn battery(theme: Theme, height: u32, level: f32, charging: bool) -> Pixmap {
    let h = height.max(8) as f32;
    let (pw, ph) = ((h * 2.0).round() as u32, height.max(8));
    let (w, hh) = (pw as usize, ph as usize);
    let mut pm = crate::pixmap(pw, ph);
    let pal = theme.palette();
    let line = (h * 0.105).max(1.4);
    let body = Rect::from_ltrb(h * 0.12, h * 0.24, h * 1.70, h * 0.76).unwrap();

    // 外框一笔，电极一顿。
    let frame = round_rect(body, h * 0.14);
    let nub = Stroke::open(vec![(h * 1.84, h * 0.41), (h * 1.84, h * 0.59)]);
    let mut style = small_style(line);
    let f = brush::render(&[frame], &style, w, hh, 11);
    blend_field(&mut pm, &f, rgb(pal.ink[1]), 1.0);
    style.width = line * 1.15;
    let f = brush::render(&[nub], &style, w, hh, 12);
    blend_field(&mut pm, &f, rgb(pal.ink[1]), 1.0);

    // 电量：一笔横扫的墨，长度按电量；低电量用朱砂，充电用石绿。
    let level = level.clamp(0.0, 1.0);
    let gap = line * 0.5 + h * 0.07;
    let inner_l = body.left() + line * 0.5 + gap;
    let inner_r = body.right() - line * 0.5 - gap;
    let inner_h = body.height() - line - 2.0 * gap;
    if level > 0.01 && inner_h > 0.5 {
        let fill_w = (inner_r - inner_l) * level.max(0.08);
        let y = body.top() + body.height() / 2.0;
        let thick = inner_h.max(1.0);
        let stroke = Stroke::open(vec![
            (inner_l + thick * 0.45, y),
            (inner_l + fill_w - thick * 0.45, y),
        ]);
        let mut fs = small_style(thick);
        fs.pressure = 0.5;
        fs.taper = Some(false);
        let f = brush::render(&[stroke], &fs, w, hh, 13);
        let color = if charging {
            rgb(match theme {
                Theme::Light => pigment::SHILV,
                Theme::Dark => 0x7FB89E,
            })
        } else if level <= 0.2 {
            rgb(pal.accent)
        } else {
            rgb(pal.ink[1])
        };
        blend_field(&mut pm, &f, color, 0.92);
    }
    if charging {
        // 闪电：纸色挖空 + 一道细墨。
        let (cx, cy) = (
            (body.left() + body.right()) / 2.0,
            body.top() + body.height() / 2.0,
        );
        let s = body.height() * 0.42;
        let bolt = Stroke::open(vec![
            (cx + s * 0.35, cy - s),
            (cx - s * 0.30, cy + s * 0.08),
            (cx + s * 0.30, cy - s * 0.08),
            (cx - s * 0.35, cy + s),
        ]);
        let mut bs = small_style(line * 1.5);
        bs.pressure = 0.2;
        let knock = brush::render(std::slice::from_ref(&bolt), &bs, w, hh, 14);
        blend_field(&mut pm, &knock, rgb(pal.paper), 1.0);
        bs.width = line * 0.75;
        let f = brush::render(&[bolt], &bs, w, hh, 15);
        blend_field(&mut pm, &f, rgb(pal.ink[0]), 1.0);
    }
    pm
}

/// 状态栏 Wi‑Fi：一点三弧，`strength` 格以内用浓墨，其余淡墨。宽 = 高 × 1.2。
pub fn wifi(theme: Theme, height: u32, strength: u8) -> Pixmap {
    let h = height.max(8) as f32;
    let (pw, ph) = ((h * 1.2).round() as u32, height.max(8));
    let (w, hh) = (pw as usize, ph as usize);
    let mut pm = crate::pixmap(pw, ph);
    let pal = theme.palette();
    let line = (h * 0.11).max(1.4);
    let (cx, cy) = (w as f32 / 2.0, h * 0.84);
    let strong = rgb(pal.ink[Ink::Nong as usize]);
    let weak = rgb(pal.ink[Ink::Qing as usize]);

    // 圆点。
    let dot = Stroke::open(vec![(cx, cy - line * 0.2), (cx + 0.02, cy - line * 0.2)]);
    let mut ds = small_style(line * 1.7);
    ds.pressure = 0.2;
    let f = brush::render(&[dot], &ds, w, hh, 21);
    blend_field(&mut pm, &f, if strength > 0 { strong } else { weak }, 1.0);

    let half = 0.80f32; // 半张角（弧度）
    for k in 0..3u8 {
        let r = h * (0.30 + 0.235 * k as f32);
        let n = 24;
        let pts: Vec<(f32, f32)> = (0..=n)
            .map(|i| {
                let a = -std::f32::consts::FRAC_PI_2 - half + 2.0 * half * i as f32 / n as f32;
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        // 每道弧一笔：尖入、中段饱满、出锋，成新月形。
        let mut style = small_style(line * (1.15 + 0.06 * k as f32));
        style.taper = Some(true);
        let f = brush::render(&[Stroke::open(pts)], &style, w, hh, 22 + k as u32);
        blend_field(&mut pm, &f, if k < strength { strong } else { weak }, 1.0);
    }
    pm
}
