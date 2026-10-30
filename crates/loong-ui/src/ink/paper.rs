//! 宣纸与绢的质感。
//!
//! 纸纹由一块可平铺的“纹理砖”提供（纤维、帘纹、杂质斑点、细颗粒），按主题各生成一次
//! 并缓存；整张纸 = 底色 × (1 + 纹理砖 + 低频云絮)。暗色外观用“绢”：经纬细织纹 + 少量丝絮。

use std::sync::OnceLock;

use tiny_skia::Pixmap;

use super::noise::{self, Rng};
use super::raster::{Field, mix, rgb, smoothstep, write_opaque};
use crate::theme::Theme;

/// 纹理砖边长（像素）。
pub const TILE: usize = 384;

/// 纹理砖：每个像素是亮度偏移（约 −0.1 … +0.1）。
pub struct Tile {
    pub luma: Field,
}

impl Tile {
    #[inline]
    pub fn at(&self, x: i32, y: i32) -> f32 {
        let xi = x.rem_euclid(TILE as i32) as usize;
        let yi = y.rem_euclid(TILE as i32) as usize;
        self.luma.data[yi * TILE + xi]
    }
}

/// 取某外观的纹理砖（首次调用时生成，之后复用）。
pub fn tile(theme: Theme) -> &'static Tile {
    static LIGHT: OnceLock<Tile> = OnceLock::new();
    static DARK: OnceLock<Tile> = OnceLock::new();
    match theme {
        Theme::Light => LIGHT.get_or_init(|| make_tile(theme)),
        Theme::Dark => DARK.get_or_init(|| make_tile(theme)),
    }
}

/// 在可平铺场上画一根柔和的细线（纤维），超出边界的部分从另一侧绕回。
fn splat_line(f: &mut Field, pts: &[(f32, f32)], width: f32, amount: f32) {
    let n = f.w as i32;
    let r = width * 0.5 + 0.8;
    for seg in pts.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let steps = (len / 0.5).ceil().max(1.0) as usize;
        for k in 0..steps {
            let t = k as f32 / steps as f32;
            let (cx, cy) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            let (ix0, iy0) = ((cx - r).floor() as i32, (cy - r).floor() as i32);
            let (ix1, iy1) = ((cx + r).ceil() as i32, (cy + r).ceil() as i32);
            for py in iy0..=iy1 {
                for px in ix0..=ix1 {
                    let d =
                        ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2)).sqrt();
                    let c = (width * 0.5 + 0.5 - d).clamp(0.0, 1.0);
                    if c > 0.0 {
                        let i = py.rem_euclid(n) as usize * f.w + px.rem_euclid(n) as usize;
                        // 每步都会叠到同一像素，取最大值避免沿线出现珠串。
                        let v = &mut f.data[i];
                        if amount > 0.0 {
                            *v = v.max(c * amount);
                        } else {
                            *v = v.min(c * amount);
                        }
                    }
                }
            }
        }
    }
}

fn make_tile(theme: Theme) -> Tile {
    let n = TILE;
    let seed = match theme {
        Theme::Light => 0x0005_eed1,
        Theme::Dark => 0x0005_eed2,
    };
    let mut rng = Rng::new(seed, 1);
    // 细颗粒 + 中频絮状（可平铺）。
    let mut luma = Field::from_fn(n, n, |x, y| {
        let grain = noise::value_periodic(
            x * 0.9,
            y * 0.9,
            (n as f32 * 0.9) as i32,
            (n as f32 * 0.9) as i32,
            seed,
        ) - 0.5;
        let floc = noise::fbm_periodic(x / 24.0, y / 24.0, (n / 24) as i32, seed ^ 0x77, 3) - 0.5;
        0.022 * grain + 0.035 * floc
    });
    let mut fibers = Field::new(n, n);
    let mut dark_fibers = Field::new(n, n);
    let count = match theme {
        Theme::Light => 520,
        Theme::Dark => 260,
    };
    for _ in 0..count {
        let len = rng.range(8.0, 46.0);
        let mut ang = rng.range(0.0, std::f32::consts::TAU);
        let curl = rng.range(-0.08, 0.08);
        let (mut x, mut y) = (rng.range(0.0, n as f32), rng.range(0.0, n as f32));
        let mut pts = vec![(x, y)];
        let segs = (len / 3.0).ceil() as usize;
        for _ in 0..segs {
            ang += curl + rng.range(-0.12, 0.12);
            x += ang.cos() * 3.0;
            y += ang.sin() * 3.0;
            pts.push((x, y));
        }
        let width = rng.range(0.5, 1.3);
        if rng.f32() < 0.78 {
            splat_line(&mut fibers, &pts, width, rng.range(0.4, 1.0));
        } else {
            splat_line(&mut dark_fibers, &pts, width * 0.8, -rng.range(0.4, 1.0));
        }
    }
    // 杂质斑点：宣纸里偶见的树皮屑。
    let flecks = match theme {
        Theme::Light => 26,
        Theme::Dark => 10,
    };
    for _ in 0..flecks {
        let (x, y) = (rng.range(0.0, n as f32), rng.range(0.0, n as f32));
        let ang = rng.range(0.0, std::f32::consts::TAU);
        let l = rng.range(0.6, 3.0);
        let pts = [(x, y), (x + ang.cos() * l, y + ang.sin() * l)];
        splat_line(
            &mut dark_fibers,
            &pts,
            rng.range(0.7, 1.6),
            -rng.range(1.5, 3.0),
        );
    }
    let (fib_amt, dark_amt) = match theme {
        Theme::Light => (0.040, 0.050),
        Theme::Dark => (0.055, 0.030),
    };
    for i in 0..n * n {
        luma.data[i] += fibers.data[i] * fib_amt + dark_fibers.data[i] * dark_amt;
    }
    if theme == Theme::Dark {
        // 绢：经纬细织纹。
        for y in 0..n {
            for x in 0..n {
                let warp = ((x as f32) * std::f32::consts::TAU / 3.0).sin();
                let weft = ((y as f32) * std::f32::consts::TAU / 3.0).sin();
                // 丝线粗细不匀：沿经线、纬线方向都有短的“竹节”，避免单向竖纹。
                let slub_v = noise::value_periodic(
                    x as f32 / 3.0,
                    y as f32 / 16.0,
                    (n / 3) as i32,
                    (n / 16) as i32,
                    seed ^ 9,
                );
                let slub_h = noise::value_periodic(
                    x as f32 / 16.0,
                    y as f32 / 3.0,
                    (n / 16) as i32,
                    (n / 3) as i32,
                    seed ^ 10,
                );
                luma.data[y * n + x] += 0.010 * warp * weft + 0.006 * (slub_v + slub_h - 1.0);
            }
        }
    } else {
        // 帘纹：抄纸竹帘留下的极淡横纹。
        for y in 0..n {
            let chain = ((y as f32) * std::f32::consts::TAU / 4.0).sin() * 0.004;
            for x in 0..n {
                luma.data[y * n + x] += chain;
            }
        }
    }
    Tile { luma }
}

/// 低频云絮（纸浆不匀），返回与纸等大的场。在 1/16 分辨率计算后放大。
pub fn mottle(w: usize, h: usize, seed: u32, scale: f32) -> Field {
    let (sw, sh) = (w.div_ceil(16) + 1, h.div_ceil(16) + 1);
    let small = Field::from_fn(sw, sh, |x, y| {
        noise::fbm(x * 16.0 / scale, y * 16.0 / scale, seed ^ 0x3ff, 4) - 0.5
    });
    small.resized(w, h)
}

/// 用 `base` 色画一整张纸（不透明）。`extra(x, y)` 可额外调整亮度（如晕染、暗角）。
pub fn paper_with(theme: Theme, w: u32, h: u32, base: u32, seed: u32) -> Pixmap {
    let mut pm = crate::pixmap(w, h);
    let t = tile(theme);
    let m = mottle(w as usize, h as usize, seed, 220.0);
    let c = rgb(base);
    let mut rng = Rng::new(seed, 2);
    let (ox, oy) = (
        (rng.f32() * TILE as f32) as i32,
        (rng.f32() * TILE as f32) as i32,
    );
    let ww = w as usize;
    let k = match theme {
        Theme::Light => 1.0,
        Theme::Dark => 1.6,
    };
    write_opaque(&mut pm, |x, y| {
        let l = 1.0 + (t.at(x as i32 + ox, y as i32 + oy) + 0.05 * m.data[y * ww + x]) * k;
        [c[0] * l, c[1] * l, c[2] * l]
    });
    pm
}

/// 把纸纹叠加到已有的不透明/半透明位图上（只改变亮度，保持透明度）。
pub fn texture_pixmap(pm: &mut Pixmap, theme: Theme, seed: u32, strength: f32) {
    let t = tile(theme);
    let w = pm.width() as usize;
    let mut rng = Rng::new(seed, 3);
    let (ox, oy) = (
        (rng.f32() * TILE as f32) as i32,
        (rng.f32() * TILE as f32) as i32,
    );
    for (i, px) in pm.data_mut().chunks_exact_mut(4).enumerate() {
        if px[3] == 0 {
            continue;
        }
        let l = 1.0 + t.at((i % w) as i32 + ox, (i / w) as i32 + oy) * strength;
        // 预乘：颜色分量不能超过 alpha。
        let a = px[3] as f32;
        for c in px.iter_mut().take(3) {
            *c = (*c as f32 * l).round().min(a) as u8;
        }
    }
}

/// 毛边：把形状遮罩的边缘做成纸张撕边般参差而柔和。`amount` 为毛边宽度（像素）。
pub fn deckle(mask: &mut Field, amount: f32, seed: u32) {
    if amount <= 0.0 {
        return;
    }
    let mut soft = mask.clone();
    soft.blur((amount * 0.5).max(1.0));
    let w = mask.w;
    let k = 0.5 / amount.max(0.5);
    for (i, (m, &s)) in mask.data.iter_mut().zip(&soft.data).enumerate() {
        if s <= 0.0 || s >= 1.0 {
            // 远离边缘处保持原样（外部 0，内部 1）。
            *m = if s >= 1.0 { 1.0 } else { 0.0 };
            continue;
        }
        let (x, y) = ((i % w) as f32, (i / w) as f32);
        let n = noise::fbm(x / (amount * 1.6), y / (amount * 1.6), seed, 3) - 0.5;
        let fiber = noise::value(x * 0.7, y * 0.7, seed ^ 0xf1b) - 0.5;
        let v = s + n * 0.55 + fiber * 0.18;
        *m = smoothstep(0.5 - k, 0.5 + k, v) * smoothstep(0.0, 0.25, s);
    }
}

/// 外观的纸色（亮色为宣纸，暗色为墨绢）。
pub fn paper_rgb(theme: Theme) -> [f32; 3] {
    rgb(theme.palette().paper)
}

/// 混合两种 0xRRGGBB 颜色。
pub fn mix_hex(a: u32, b: u32, t: f32) -> [f32; 3] {
    let (a, b) = (rgb(a), rgb(b));
    [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_is_opaque_and_near_base_colour() {
        for theme in [Theme::Light, Theme::Dark] {
            let base = theme.palette().paper;
            let pm = paper_with(theme, 120, 80, base, 3);
            let c = rgb(base);
            let mut sum = [0.0f32; 3];
            for p in pm.pixels() {
                assert_eq!(p.alpha(), 255);
                sum[0] += p.red() as f32 / 255.0;
                sum[1] += p.green() as f32 / 255.0;
                sum[2] += p.blue() as f32 / 255.0;
            }
            let n = (120 * 80) as f32;
            for k in 0..3 {
                assert!((sum[k] / n - c[k]).abs() < 0.03, "{theme:?} 通道 {k}");
            }
        }
    }

    #[test]
    fn deckle_keeps_inside_and_outside() {
        let mut m = Field::from_fn(60, 60, |x, y| {
            if (10.0..50.0).contains(&x) && (10.0..50.0).contains(&y) {
                1.0
            } else {
                0.0
            }
        });
        deckle(&mut m, 2.0, 1);
        assert_eq!(m.get(30, 30), 1.0);
        assert_eq!(m.get(2, 2), 0.0);
    }
}
