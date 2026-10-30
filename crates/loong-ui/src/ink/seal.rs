//! 朱砂印章（白文：印面朱红，字为阴刻留白）。
//!
//! 字用展示字体（霞鹜文楷）排进印面：1 字居中；2 字上下；3 字右一左二；4 字右上起
//! 竖读两列。字形拉伸填满格子（印章字的章法），边缘与印面都做出“刀味”与印泥不匀：
//! 外框残损、四角略圆、印泥颗粒与零星漏白。

use std::sync::Mutex;

use tiny_skia::{Pixmap, Rect, Transform};

use super::noise::{self, Rng};
use super::raster::{Field, mix3, path_mask, rgb, smoothstep};
use crate::text::{FontRole, TextRenderer, TextStyle};
use crate::theme::Theme;

/// 共享的文字排版器（加载字体开销大，只建一次）。
fn with_text<R>(f: impl FnOnce(&mut TextRenderer) -> R) -> R {
    static TEXT: Mutex<Option<TextRenderer>> = Mutex::new(None);
    let mut guard = TEXT.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(TextRenderer::new))
}

/// 单个字的覆盖度（裁到字形墨迹的包围盒）。
fn glyph(ch: char, px: f32) -> Option<Field> {
    let style = TextStyle {
        family: FontRole::Display,
        size: px,
        weight: 400,
        color: tiny_skia::Color::BLACK,
        shadow: false,
    };
    let pm = with_text(|t| t.render(&ch.to_string(), &style));
    let (w, h) = (pm.width() as usize, pm.height() as usize);
    let alpha: Vec<f32> = pm
        .pixels()
        .iter()
        .map(|p| p.alpha() as f32 / 255.0)
        .collect();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if alpha[y * w + x] > 0.25 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let mut f = Field::new(x1 - x0, y1 - y0);
    for y in y0..y1 {
        for x in x0..x1 {
            f.data[(y - y0) * f.w + (x - x0)] = alpha[y * w + x];
        }
    }
    Some(f)
}

/// 各字所在的格子（相对印面内框的 0..1 坐标）：章法从右往左、从上往下。
fn cells(count: usize) -> Vec<(f32, f32, f32, f32)> {
    let g = 0.06; // 字间朱红隔线
    let half = (1.0 - g) / 2.0;
    match count {
        0 => vec![],
        1 => vec![(0.0, 0.0, 1.0, 1.0)],
        2 => vec![(0.0, 0.0, 1.0, half), (0.0, half + g, 1.0, half)],
        3 => vec![
            (half + g, 0.0, half, 1.0),
            (0.0, 0.0, half, half),
            (0.0, half + g, half, half),
        ],
        _ => vec![
            (half + g, 0.0, half, half),
            (half + g, half + g, half, half),
            (0.0, 0.0, half, half),
            (0.0, half + g, half, half),
        ],
    }
}

/// 印章。
pub fn seal(theme: Theme, size: u32, text: &str, seed: u32) -> Pixmap {
    let n = size.max(8) as usize;
    let s = n as f32;
    let mut rng = Rng::new(seed, 0x5ea1);

    // 印面：略带圆角的方形，外框残损。
    let inset = s * 0.03;
    let rect = Rect::from_xywh(inset, inset, s - 2.0 * inset, s - 2.0 * inset).unwrap();
    let path = crate::shape::continuous_rect(rect, s * 0.07);
    let mut face = path_mask(&path, n, n, Transform::identity());
    let mut soft = face.clone();
    soft.blur((s * 0.02).max(1.0));
    let chip_seed = rng.next_u64() as u32;
    for (i, (f, &sv)) in face.data.iter_mut().zip(&soft.data).enumerate() {
        if *f <= 0.0 {
            continue;
        }
        let (x, y) = ((i % n) as f32, (i / n) as f32);
        // 边缘崩口：低频噪声决定哪里缺一块。
        let chip = noise::fbm(x / (s * 0.06), y / (s * 0.06), chip_seed, 3);
        let v = sv + (chip - 0.5) * 0.9;
        *f *= smoothstep(0.32, 0.5, v);
    }

    // 字：先排进内框，再模糊 + 噪声阈值，做出刀刻的毛涩边。
    let chars: Vec<char> = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(4)
        .collect();
    let frame = s * 0.12;
    let inner = s - 2.0 * frame;
    let mut carve = Field::new(n, n);
    for (ch, &(cx, cy, cw, chh)) in chars.iter().zip(cells(chars.len()).iter()) {
        let (gx, gy, gw, gh) = (
            frame + cx * inner,
            frame + cy * inner,
            cw * inner,
            chh * inner,
        );
        let Some(g) = glyph(*ch, (gh.max(gw) * 1.25).max(8.0)) else {
            continue;
        };
        // 拉伸填满格子（宽高比最多改变 1.3 倍，保持字形端正可认）。
        let (sx, sy) = (gw / g.w as f32, gh / g.h as f32);
        let (sx, sy) = (sx.min(sy * 1.3), sy.min(sx * 1.3));
        let (ox, oy) = (
            gx + (gw - g.w as f32 * sx) / 2.0,
            gy + (gh - g.h as f32 * sy) / 2.0,
        );
        for y in gy.floor() as usize..((gy + gh).ceil() as usize).min(n) {
            for x in gx.floor() as usize..((gx + gw).ceil() as usize).min(n) {
                let u = (x as f32 + 0.5 - ox) / sx;
                let v = (y as f32 + 0.5 - oy) / sy;
                if u < 0.0 || v < 0.0 || u >= g.w as f32 || v >= g.h as f32 {
                    continue;
                }
                let a = g.sample(u, v);
                let c = carve.at(x, y);
                *c = c.max(a);
            }
        }
    }
    // 字笔画略加粗（印章字要比书写体饱满），边缘刀味。
    let mut bold = carve.clone();
    bold.blur((s * 0.012).max(0.6));
    let edge_seed = rng.next_u64() as u32;
    for (i, c) in carve.data.iter_mut().enumerate() {
        let b = bold.data[i];
        if b <= 0.01 {
            *c = 0.0;
            continue;
        }
        let (x, y) = ((i % n) as f32, (i / n) as f32);
        let nz = noise::value(
            x / (s * 0.018).max(0.8),
            y / (s * 0.018).max(0.8),
            edge_seed,
        ) - 0.5;
        *c = smoothstep(0.22, 0.42, b.max(*c * 0.9) + nz * 0.22);
    }

    // 印泥：颗粒、漏白（印面没压实的地方）、四边略淡。
    let pal = theme.palette();
    let red = match theme {
        Theme::Light => rgb(pal.accent),
        Theme::Dark => mix3(rgb(pal.accent), rgb(0xE07A5F), 0.25),
    };
    let dark_red = mix3(red, [0.30, 0.05, 0.03], 0.25);
    let speck_seed = rng.next_u64() as u32;
    let mut pm = crate::pixmap(size, size);
    let data = pm.data_mut();
    for i in 0..n * n {
        let f = face.data[i];
        if f <= 0.0 {
            continue;
        }
        let (x, y) = ((i % n) as f32, (i / n) as f32);
        let gran = noise::value(x * 0.8, y * 0.8, speck_seed);
        let blot = noise::fbm(x / (s * 0.08), y / (s * 0.08), speck_seed ^ 0x33, 3);
        // 漏白：大块的淡处 + 细小的白点。
        let void = smoothstep(
            0.80,
            0.88,
            gran * 0.5 + blot * 0.65 + (1.0 - soft.data[i]) * 0.25,
        );
        let a = f * (1.0 - carve.data[i]) * (1.0 - void) * (0.86 + 0.14 * blot);
        if a <= 0.0 {
            continue;
        }
        let c = mix3(
            red,
            dark_red,
            (gran - 0.5).max(0.0) * 0.8 + (blot - 0.5).max(0.0) * 0.5,
        );
        let px = &mut data[i * 4..i * 4 + 4];
        px[0] = (c[0] * a * 255.0 + 0.5) as u8;
        px[1] = (c[1] * a * 255.0 + 0.5) as u8;
        px[2] = (c[2] * a * 255.0 + 0.5) as u8;
        px[3] = (a * 255.0 + 0.5) as u8;
    }
    pm
}
