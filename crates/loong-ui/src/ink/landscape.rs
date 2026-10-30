//! 「山水长卷」：程序生成的分层山水壁纸。
//!
//! 从远到近五层，每层是一张独立的位图，主屏翻页时各层按不同视差移动（展开手卷）：
//!
//! | 层 | 视差 | 内容 | 绘制分辨率 |
//! |---|---|---|---|
//! | 天 | 0.04 | 纸/绢底、天色淡墨、朱砂日（亮）/ 月（暗）、远雁 | 全分辨率（纸纹）+ 1/8 低频 |
//! | 远山 | 0.15 | 两重淡墨远山（带一点花青），山脚没入烟岚 | 1/4 后放大 |
//! | 中山 | 0.35 | 两重山，山脊干笔、披麻皴、苔点 | 1/2 后放大 |
//! | 近山 | 0.6 | 浓墨近山，干笔勾脊、皴擦、苔点，山脚赭石淡彩 | 全分辨率 |
//! | 烟 | 0.8 | 两条烟云（纸色/月白） | 1/4 后放大 |
//!
//! 构图：每重山由若干“山群”组成，山群之间是空白（留白），山低处整片淡入烟云——
//! 没有一条贯穿画面的山脊线。首屏的山群位置是设计好的（左侧主峰、右侧远峰承日、
//! 中间留白放图标），之后的画卷由种子随机展开。
//!
//! 每重山：山脊浓、向下淡开（云锁山腰）；山脊勾线粗细断续、带飞白；阴面（右坡）略深；
//! 山坳下有褶皱；山脊下一段有顺山势的披麻皴；山头点苔。

use tiny_skia::Pixmap;

use super::Layer;
use super::brush::{self, BrushStyle, Stroke};
use super::noise::{self, Rng};
use super::paper;
use super::raster::{Field, mix3, rgb, smoothstep, upscale};
use crate::theme::{Theme, pigment};

/// 视差系数（由远及近）。
pub const PARALLAX: [f32; 5] = [0.04, 0.15, 0.35, 0.6, 0.8];

/// 一重山。所有长度为该层的像素。
struct Hill {
    ridge: Vec<f32>,
    slope: Vec<f32>,
    /// 大尺度坡向（阴阳明暗用）。
    aspect: Vec<f32>,
    height: Vec<f32>,
    /// 山的“存在感”（0 = 此处无山，淡入烟云）。
    presence: Vec<f32>,
    /// 山坳褶皱强度。
    crease: Vec<f32>,
    /// 勾线宽度（沿山脊变化）。
    edge: Vec<f32>,
    style: HillStyle,
}

/// 山的笔墨。
#[derive(Clone, Copy)]
struct HillStyle {
    ink: [f32; 3],
    /// 山体墨色浓度。
    alpha: f32,
    /// 山脊勾线宽度（屏幕像素 @1600 高）与浓度。
    edge_w: f32,
    edge_alpha: f32,
    /// 山体向下淡开的距离（相对山高）。
    fade: f32,
    /// 皴擦、干笔强度。
    cun: f32,
    dry: f32,
    /// 山脊高光（暗色外观的月白/泥金）。
    rim: Option<([f32; 3], f32)>,
    /// 山脚淡彩（赭石）。
    tint: Option<([f32; 3], f32)>,
}

/// 一重山的布置（比例相对屏幕宽 W、高 H）。
struct RangeSpec {
    /// 山脚线高度。
    base: f32,
    /// 随机山群的典型高度。
    height: f32,
    /// 峰的半宽与峰高之比（越小越陡峭）。
    aspect: f32,
    /// 峰形指数（越小峰顶越尖）。
    sharp: f32,
    /// 山石起伏的幅度。
    crag: f32,
    /// 首屏设计好的山群：(中心 x, 半宽, 高)。
    groups: Vec<(f32, f32, f32)>,
    /// 首屏之后随机山群的间距。
    spacing: f32,
}

fn make_hill(
    spec: &RangeSpec,
    style: HillStyle,
    lw: usize,
    sw: f32,
    sh: f32,
    scale: f32,
    seed: u32,
) -> Hill {
    let mut rng = Rng::new(seed, 0x91d6e);
    let total = lw as f32 / scale;
    // 山群：首屏按设计，其后随机。
    let mut groups: Vec<(f32, f32, f32)> = spec
        .groups
        .iter()
        .map(|&(x, hw, h)| (x * sw, hw * sw, h * sh))
        .collect();
    let mut x = sw * (1.05 + rng.range(0.0, spec.spacing * 0.5));
    while x < total + sw * 0.3 {
        let hw = sw * rng.range(0.08, 0.22);
        let h = sh * spec.height * rng.range(0.45, 1.0);
        groups.push((x, hw, h));
        x += sw * spec.spacing * rng.range(0.6, 1.4);
    }
    // 每个山群若干座峰：主峰居中最高，旁峰较低。
    let mut peaks: Vec<(f32, f32, f32, f32)> = Vec::new();
    for &(gx, hw, gh) in &groups {
        let main_x = gx + hw * rng.range(-0.25, 0.25);
        let mut add = |px: f32, ph: f32, rng: &mut Rng| {
            let wl = ph * spec.aspect * rng.range(0.7, 1.35);
            let wr = ph * spec.aspect * rng.range(0.7, 1.35);
            peaks.push((px, ph, wl, wr));
        };
        add(main_x, gh, &mut rng);
        let n = 2 + (hw / (sw * 0.05)) as usize + (rng.f32() * 2.0) as usize;
        for _ in 0..n {
            let off = rng.range(-1.0, 1.0);
            let px = gx + off * hw;
            let ph = gh * rng.range(0.30, 0.78) * (1.0 - 0.45 * off.abs());
            add(px, ph, &mut rng);
        }
    }
    let crag_seed = rng.next_u64() as u32;
    let roll_seed = rng.next_u64() as u32;
    let hmax = sh * spec.height.max(0.05);
    let mut height = vec![0.0f32; lw];
    for (i, hgt) in height.iter_mut().enumerate() {
        let xs = i as f32 / scale;
        let mut h = 0.0f32;
        for &(c, hp, wl, wr) in &peaks {
            let u = if xs < c { (c - xs) / wl } else { (xs - c) / wr };
            if u < 3.5 {
                h = h.max(hp * (-u.powf(spec.sharp) * 1.3).exp());
            }
        }
        // 山石起伏：脊状分形噪声，峰越高起伏越大。
        let r = noise::fbm1(xs / (sw * 0.04), crag_seed, 3);
        let ridged = 1.0 - (2.0 * r - 1.0).abs();
        let roll = noise::fbm1(xs / (sw * 0.012), roll_seed, 2) - 0.5;
        let k = (h / hmax).min(1.2);
        *hgt = (h + (ridged - 0.55) * sh * spec.crag * k + roll * sh * 0.006 * k).max(0.0);
    }
    // 轻微平滑：去掉噪声锯齿，保留棱角。
    let smooth = box_smooth(&height, (3.0 * scale).max(1.0) as usize);
    let base = sh * spec.base * scale;
    let ridge: Vec<f32> = smooth.iter().map(|h| base - h * scale).collect();
    let slope_of = |r: &[f32], i: usize| {
        let a = r[i.saturating_sub(1)];
        let b = r[(i + 1).min(lw - 1)];
        (b - a) / 2.0
    };
    // 阴阳用大尺度的坡向（山石小起伏不应造成明暗竖纹）。
    let broad = box_smooth(&ridge, (sw * 0.012 * scale) as usize + 1);
    let slope: Vec<f32> = (0..lw).map(|i| slope_of(&ridge, i)).collect();
    let aspect: Vec<f32> = (0..lw).map(|i| slope_of(&broad, i)).collect();
    // 山体淡开的长度按大尺度山高算：山石小缺口下不应出现一道道竖向的浓淡。
    let height_px: Vec<f32> = box_smooth(&smooth, (sw * 0.02 * scale) as usize + 1)
        .iter()
        .map(|h| h * scale)
        .collect();
    let presence: Vec<f32> = smooth
        .iter()
        .map(|&h| smoothstep(sh * 0.012, sh * 0.06, h))
        .collect();
    // 山坳：山脊比两侧宽窗口的包络低的地方（再横向模糊，成为柔和的褶皱而非竖线）。
    let win = (sw * 0.035 * scale) as usize + 1;
    let crease_raw: Vec<f32> = (0..lw)
        .map(|i| {
            let a = i.saturating_sub(win);
            let b = (i + win).min(lw - 1);
            let hi = ridge[a].min(ridge[b]);
            smoothstep(0.0, sh * 0.04 * scale, ridge[i] - hi) * presence[i]
        })
        .collect();
    let crease = box_smooth(&crease_raw, (sw * 0.01 * scale) as usize + 1);
    // 勾线宽度：沿山脊时粗时细，峰顶（平缓处）更重。
    let edge_seed = rng.next_u64() as u32;
    let unit = sh / 1600.0;
    let edge: Vec<f32> = (0..lw)
        .map(|i| {
            let xs = i as f32 / scale;
            let n = noise::fbm1(xs / (sw * 0.025), edge_seed, 3);
            let flat = 1.0 - smoothstep(0.2, 1.5, slope[i].abs());
            style.edge_w * unit * scale * (0.25 + 1.5 * n * n) * (0.7 + 0.6 * flat)
        })
        .collect();
    Hill {
        ridge,
        slope,
        aspect,
        height: height_px,
        presence,
        crease,
        edge,
        style,
    }
}

/// 一维盒式平滑（前缀和，O(n)）。
fn box_smooth(v: &[f32], k: usize) -> Vec<f32> {
    let n = v.len();
    if k == 0 || n == 0 {
        return v.to_vec();
    }
    let mut prefix = vec![0.0f64; n + 1];
    for (i, &x) in v.iter().enumerate() {
        prefix[i + 1] = prefix[i] + x as f64;
    }
    (0..n)
        .map(|i| {
            let a = i.saturating_sub(k);
            let b = (i + k).min(n - 1);
            ((prefix[b + 1] - prefix[a]) / (b - a + 1) as f64) as f32
        })
        .collect()
}

/// 把若干重山画进一层（预乘 RGBA，按行并行）。
fn paint_hills(pm: &mut Pixmap, hills: &[Hill], mottle: &Field, scale: f32, seed: u32) {
    let w = pm.width() as usize;
    let top = hills
        .iter()
        .flat_map(|hl| hl.ridge.iter())
        .fold(f32::MAX, |a, &b| a.min(b));
    let y0 = (top - 3.0).max(0.0) as usize;
    let ms = 1.0 / 8.0;
    let data = pm.data_mut();
    // 噪声坐标换算回屏幕像素，保证不同分辨率下纹理的物理尺寸一致。
    let inv = 1.0 / scale;
    par_rows(&mut data[y0 * w * 4..], w, |yy, row| {
        let y = (y0 + yy) as f32 + 0.5;
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let xf = x as f32 + 0.5;
            let mut acc = [0.0f32; 4];
            for (hi, hl) in hills.iter().enumerate() {
                let pres = hl.presence[x];
                if pres <= 0.0 {
                    continue;
                }
                let st = &hl.style;
                let d = y - hl.ridge[x];
                if d < -2.0 {
                    continue;
                }
                let s = hl.slope[x];
                let kperp = 1.0 / (1.0 + s * s).sqrt();
                let dp = d * kperp;
                let hs = seed.wrapping_add(hi as u32 * 101);
                let ew = hl.edge[x].max(0.3);
                let (sx, sy) = (xf * inv, y * inv);
                // 轮廓：干笔的毛边。
                let near_edge = dp < ew * 2.5 + 3.0;
                let rough = if near_edge {
                    (noise::value(sx * 0.5, sy * 0.5, hs) - 0.5)
                        * (0.5 + 2.0 * st.dry)
                        * scale.max(0.5)
                } else {
                    0.0
                };
                let cov = (dp + 0.5 + rough).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let hh = hl.height[x].max(4.0);
                let fade_len = hh * st.fade;
                // 已在墨色淡尽之下（扰动后也不可能有墨）、又没有勾线与皴：跳过。
                if !near_edge && d * 0.45 >= fade_len && (st.cun <= 0.0 || d >= hh * 0.45) {
                    continue;
                }
                let m = mottle.sample(xf * ms, y * ms) - 0.5;
                // 山体：山脊下浓，向下淡开；边界被低频噪声扰动，像湿墨渗开。
                let body = 1.0 - smoothstep(0.0, fade_len, d * (1.0 + 1.1 * m));
                // 阴阳：右坡（山脊向右下降）为阴面，略深；山坳下有褶皱。
                let yin = 1.0 + 0.22 * (hl.aspect[x] * 1.5).clamp(-1.0, 1.0);
                let fold = 1.0 + 0.3 * hl.crease[x] * (1.0 - smoothstep(0.0, hh * 0.5, d));
                // body^1.25 = body · body^¼（避免逐像素 powf）。
                let body_k = body * body.sqrt().sqrt();
                let body_a = st.alpha * body_k * (0.80 + 0.5 * m) * yin * fold;
                // 山脊勾线 + 飞白（勾线本身也时断时续）。
                let mut edge_a = 0.0;
                if near_edge && st.edge_alpha > 0.0 {
                    let band = 1.0 - smoothstep(ew * 0.35, ew, dp);
                    let streak = noise::value(sx / 9.0, dp * inv / 1.3, hs ^ 0x5e);
                    let broken = noise::value(sx / 70.0, 0.5, hs ^ 0xb0);
                    let gap = smoothstep(st.dry * 0.6 - 0.1, st.dry * 0.6 + 0.12, streak)
                        * smoothstep(0.12, 0.4, broken);
                    edge_a = st.edge_alpha * band * gap;
                }
                // 皴：顺山势的长短墨线（披麻皴），只在山脊下一段。
                let mut cun_a = 0.0;
                if st.cun > 0.0 && d < hh * 0.45 {
                    let q = sx + hl.aspect[x] * d * inv * 0.9;
                    let c1 = noise::value(q / 2.6, d * inv / 22.0, hs ^ 0xc0);
                    let c2 = noise::value(q / 14.0, d * inv / 40.0, hs ^ 0xc1);
                    let lines = smoothstep(0.70, 0.88, c1) * smoothstep(0.50, 0.72, c2);
                    cun_a = st.cun
                        * lines
                        * (1.0 - smoothstep(hh * 0.03, hh * 0.45, d))
                        * (0.5 + 0.5 * yin);
                }
                let a = (cov * pres * (body_a + (1.0 - body_a) * (edge_a + cun_a).min(1.0)))
                    .clamp(0.0, 1.0);
                if a <= 0.001 {
                    continue;
                }
                let mut c = st.ink;
                if let Some((tint, k)) = st.tint {
                    let t = smoothstep(hh * 0.10, hh * 0.6, d) * k * (1.0 - edge_a);
                    c = mix3(c, tint, t);
                }
                if let Some((rim, k)) = st.rim {
                    // 月光/泥金只落在高处平缓的山脊上，断断续续。
                    let band = 1.0 - smoothstep(0.0, ew * 1.1, dp);
                    let streak = noise::value(sx / 7.0, dp * inv / 1.1, hs ^ 0x7e);
                    let broken = noise::value(sx / 55.0, 0.5, hs ^ 0x7f);
                    let t =
                        band * smoothstep(0.4, 0.7, streak) * smoothstep(0.35, 0.65, broken) * k;
                    c = mix3(c, rim, t.min(1.0));
                }
                let inv_a = 1.0 - a;
                acc[0] = c[0] * a + acc[0] * inv_a;
                acc[1] = c[1] * a + acc[1] * inv_a;
                acc[2] = c[2] * a + acc[2] * inv_a;
                acc[3] = a + acc[3] * inv_a;
            }
            if acc[3] > 0.0 {
                px[0] = (acc[0] * 255.0 + 0.5) as u8;
                px[1] = (acc[1] * 255.0 + 0.5) as u8;
                px[2] = (acc[2] * 255.0 + 0.5) as u8;
                px[3] = (acc[3] * 255.0 + 0.5) as u8;
            }
        }
    });
}

/// 按行并行处理预乘 RGBA 数据。
pub(crate) fn par_rows(data: &mut [u8], w: usize, f: impl Fn(usize, &mut [u8]) + Sync) {
    let rows = data.len() / (w * 4);
    if rows == 0 {
        return;
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8);
    let per = rows.div_ceil(threads);
    std::thread::scope(|scope| {
        for (ci, chunk) in data.chunks_mut(per * w * 4).enumerate() {
            let f = &f;
            scope.spawn(move || {
                for (ri, row) in chunk.chunks_exact_mut(w * 4).enumerate() {
                    f(ci * per + ri, row);
                }
            });
        }
    });
}

fn layer_width(sw: u32, parallax: f32, pages: usize) -> u32 {
    sw + (parallax * (pages.max(1) as f32 - 1.0) * sw as f32).ceil() as u32
}

/// 天：纸底 + 天色 + 日/月 + 远雁。
fn sky(theme: Theme, sw: u32, sh: u32, lw: u32, seed: u32) -> Pixmap {
    let (w, h) = (lw as usize, sh as usize);
    let (swf, shf) = (sw as f32, sh as f32);
    let pal = theme.palette();
    let base = rgb(pal.paper);
    let m = swf.min(shf);
    // 日/月放在图标网格右侧的空白里（横屏）；竖屏放在右上角状态栏下方。
    let (sun_x, sun_y, sun_r) = if swf >= shf {
        (swf * 0.925, shf * 0.25, m * 0.05)
    } else {
        (swf * 0.80, shf * 0.10, m * 0.06)
    };
    // 低频：亮色为上方一抹“烘天”淡墨与日旁微暖；暗色为地平线与月旁微亮。
    let ds = 8usize;
    let low = Field::from_fn(w.div_ceil(ds) + 1, h.div_ceil(ds) + 1, |x, y| {
        let (x, y) = (x * ds as f32, y * ds as f32);
        let v = y / shf;
        let n = noise::fbm(x / (swf * 0.35), y / (shf * 0.25), seed ^ 0x5c1, 4) - 0.5;
        let r2 = (x - sun_x).powi(2) + (y - sun_y).powi(2);
        match theme {
            Theme::Light => {
                let wash = (1.0 - smoothstep(0.0, 0.40, v + n * 0.22)) * 0.085;
                let glow = (-r2 / (sun_r * 7.0).powi(2)).exp();
                wash * (1.0 - glow) - glow * 0.03
            }
            Theme::Dark => {
                let horizon = (-((v - 0.62) / 0.16).powi(2)).exp() * 0.12;
                let moon = (-r2 / (sun_r * 6.0).powi(2)).exp() * 0.55;
                ((horizon + moon) * (0.8 + 0.6 * n)).max(0.0)
            }
        }
    });
    let wash = match theme {
        Theme::Light => rgb(0x8F8A80),
        Theme::Dark => rgb(0x34302A),
    };
    let warm = rgb(0xF6E7CF);
    let tile = paper::tile(theme);
    let mut pm = crate::pixmap(lw, sh);
    let k_tex = match theme {
        Theme::Light => 1.0,
        Theme::Dark => 1.5,
    };
    // 纸浆云絮（低频）与暗角（长卷四周略旧）也都是低频量，和天色一起在 1/8 分辨率算好，
    // 逐像素只做插值。
    let shade = Field::from_fn(w.div_ceil(ds) + 1, h.div_ceil(ds) + 1, |x, y| {
        let (x, y) = (x * ds as f32, y * ds as f32);
        let mot = noise::fbm(x / 260.0, y / 260.0, seed ^ 0x3ff, 4) - 0.5;
        let vx = (x / swf - 0.5).abs() * 2.0;
        let vy = (y / shf - 0.5).abs() * 2.0;
        let vig = 1.0 - 0.04 * smoothstep(0.6, 1.1, vx.min(1.2).max(vy));
        (1.0 + 0.04 * mot * k_tex) * vig
    });
    par_rows(pm.data_mut(), w, |y, row| {
        let yf = y as f32;
        let sy = yf / ds as f32 + 0.5;
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let sx = x as f32 / ds as f32 + 0.5;
            let l = low.sample(sx, sy);
            let t = shade.sample(sx, sy) + tile.at(x as i32, y as i32) * k_tex;
            let c = if l >= 0.0 {
                mix3(base, wash, l.min(1.0))
            } else {
                mix3(base, warm, (-l * 8.0).min(1.0) * 0.5)
            };
            px[0] = ((c[0] * t).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            px[1] = ((c[1] * t).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            px[2] = ((c[2] * t).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            px[3] = 255;
        }
    });
    disc(&mut pm, theme, sun_x, sun_y, sun_r, seed);
    geese(
        &mut pm,
        theme,
        sun_x + sun_r * 0.6,
        sun_y + sun_r * 2.8,
        m,
        seed,
    );
    pm
}

/// 朱砂日 / 月。
fn disc(pm: &mut Pixmap, theme: Theme, cx: f32, cy: f32, r: f32, seed: u32) {
    let w = pm.width() as usize;
    let (color, alpha, halo) = match theme {
        Theme::Light => (rgb(pigment::ZHUSHA), 0.84, 0.0),
        Theme::Dark => (rgb(0xEAE4D4), 0.95, 0.16),
    };
    let reach = r * if halo > 0.0 { 5.0 } else { 1.3 };
    let (x0, x1) = (
        (cx - reach).max(0.0) as usize,
        ((cx + reach) as usize).min(w),
    );
    let (y0, y1) = (
        (cy - reach).max(0.0) as usize,
        ((cy + reach) as usize).min(pm.height() as usize),
    );
    let data = pm.data_mut();
    for y in y0..y1 {
        for x in x0..x1 {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let d = (dx * dx + dy * dy).sqrt();
            let n = noise::fbm(
                x as f32 / (r * 0.35),
                y as f32 / (r * 0.35),
                seed ^ 0xd15c,
                3,
            ) - 0.5;
            // 颜料边缘被水推开，略不圆、边上略浓。
            let edge = r * (1.0 + n * 0.05);
            let cov = (edge - d + 0.8).clamp(0.0, 1.6) / 1.6;
            let i = y * w + x;
            if halo > 0.0 && d > r * 0.9 {
                let g = (-(d - r).max(0.0) / (r * 1.1)).exp() * halo;
                super::raster::blend_px(data, i, color, g * (1.0 - cov));
            }
            if cov > 0.0 {
                let gran = noise::value(x as f32 * 0.7, y as f32 * 0.7, seed ^ 0x9a) - 0.5;
                let ring = smoothstep(0.78, 1.0, d / r) * 0.12;
                let c = match theme {
                    Theme::Light => mix3(color, [0.50, 0.12, 0.08], (ring + gran * 0.10).max(0.0)),
                    Theme::Dark => {
                        let maria = (noise::fbm(
                            x as f32 / (r * 0.5),
                            y as f32 / (r * 0.5),
                            seed ^ 0x3f,
                            3,
                        ) - 0.45)
                            .max(0.0)
                            * 0.35;
                        mix3(color, [0.62, 0.58, 0.50], maria + gran.max(0.0) * 0.05)
                    }
                };
                super::raster::blend_px(data, i, c, cov * alpha * (0.94 + 0.12 * gran));
            }
        }
    }
}

/// 远雁：几只极小的“人”字笔触，疏落成行。
fn geese(pm: &mut Pixmap, theme: Theme, x: f32, y: f32, m: f32, seed: u32) {
    let mut rng = Rng::new(seed, 0x6ee5e);
    let size = m * 0.0075;
    let mut strokes = Vec::new();
    let (mut gx, mut gy) = (x, y);
    for i in 0..4 {
        let s = size * rng.range(0.7, 1.2) * (1.0 - 0.12 * i as f32);
        let lift = rng.range(0.35, 0.8);
        let tilt = rng.range(-0.25, 0.25);
        strokes.push(Stroke::open(vec![
            (gx - s, gy + s * (0.25 + tilt)),
            (gx - s * 0.45, gy - s * lift * 0.5),
            (gx, gy),
        ]));
        strokes.push(Stroke::open(vec![
            (gx, gy),
            (gx + s * 0.5, gy - s * lift),
            (gx + s * 1.05, gy - s * (0.1 + tilt)),
        ]));
        gx -= size * rng.range(3.0, 5.5);
        gy += size * rng.range(-0.8, 1.6);
    }
    let pad = size * 3.0;
    let bx0 = (gx - pad).max(0.0);
    let by0 = (y - size * 10.0).max(0.0);
    let (bw, bh) = (
        (x - gx + 2.0 * pad + size * 2.0) as usize,
        (size * 22.0) as usize,
    );
    let local: Vec<Stroke> = strokes
        .into_iter()
        .map(|s| Stroke::open(s.pts.iter().map(|&(px, py)| (px - bx0, py - by0)).collect()))
        .collect();
    let mut style = BrushStyle::for_width((size * 0.30).max(1.2));
    style.dryness *= 0.5;
    let f = brush::render(&local, &style, bw.max(1), bh.max(1), seed ^ 0x6e);
    let ink = match theme {
        Theme::Light => rgb(0x4A4741),
        Theme::Dark => rgb(0x8A8578),
    };
    blit_field(pm, &f, bx0 as i32, by0 as i32, ink, 0.7);
}

/// 把局部墨量场按偏移叠加到位图上。
fn blit_field(pm: &mut Pixmap, f: &Field, ox: i32, oy: i32, color: [f32; 3], opacity: f32) {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    for y in 0..f.h as i32 {
        let gy = oy + y;
        if gy < 0 || gy >= h {
            continue;
        }
        for x in 0..f.w as i32 {
            let gx = ox + x;
            if gx < 0 || gx >= w {
                continue;
            }
            let a = f.data[(y as usize) * f.w + x as usize] * opacity;
            if a > 0.0 {
                super::raster::blend_px(data, (gy * w + gx) as usize, color, a);
            }
        }
    }
}

/// 苔点：只点在较高的山头上，几簇疏落的横点。
fn moss(pm: &mut Pixmap, hill: &Hill, color: [f32; 3], dot: f32, density: f32, seed: u32) {
    let w = hill.ridge.len();
    let mut rng = Rng::new(seed, 0x3055);
    let hmax = hill.height.iter().copied().fold(1.0, f32::max);
    // 候选：局部最高点。
    let win = (dot * 18.0) as usize + 2;
    let mut x = win;
    while x + win < w {
        let h = hill.height[x];
        let is_top = h >= hmax * 0.30
            && hill.presence[x] > 0.9
            && (x - win..x + win).all(|j| hill.height[j] <= h);
        if is_top && rng.f32() < density {
            let cluster = 3 + (rng.f32() * 4.0) as usize;
            let mut strokes = Vec::new();
            let span = dot * 4.0;
            for _ in 0..cluster {
                let dx = rng.normal() * span * 0.5;
                let px = (x as f32 + dx).clamp(0.0, (w - 1) as f32);
                let ry = hill.ridge[px as usize];
                let py = ry + rng.range(0.0, 1.2) * dot;
                let len = dot * rng.range(0.35, 1.0);
                let tilt = rng.range(-0.3, 0.1) * len;
                strokes.push(Stroke::open(vec![
                    (px - len * 0.5, py - tilt * 0.5),
                    (px + len * 0.5, py + tilt * 0.5),
                ]));
            }
            let pad = span * 2.0 + dot * 3.0;
            let (bx0, by0) = ((x as f32 - pad).max(0.0), (hill.ridge[x] - pad).max(0.0));
            let local: Vec<Stroke> = strokes
                .into_iter()
                .map(|s| Stroke::open(s.pts.iter().map(|&(a, b)| (a - bx0, b - by0)).collect()))
                .collect();
            let mut style = BrushStyle::for_width(dot);
            style.dryness *= 0.3;
            let size = (pad * 2.0) as usize + 1;
            let f = brush::render(&local, &style, size, size, rng.next_u64() as u32);
            blit_field(pm, &f, bx0 as i32, by0 as i32, color, 0.9);
            x += win * 2;
        } else {
            x += win / 2 + 1;
        }
    }
}

/// 一层山（在 `scale` 分辨率下绘制后放大到层尺寸）。
#[allow(clippy::too_many_arguments)]
fn mountain_layer(
    sw: u32,
    sh: u32,
    lw: u32,
    scale: f32,
    specs: &[(RangeSpec, HillStyle)],
    moss_spec: Option<([f32; 3], f32, f32)>,
    mist: Option<(&MistSpec, [f32; 3])>,
    seed: u32,
) -> Pixmap {
    let w = ((lw as f32) * scale).ceil() as usize;
    let h = ((sh as f32) * scale).ceil() as usize;
    let mut pm = crate::pixmap(w as u32, h as u32);
    let hills: Vec<Hill> = specs
        .iter()
        .enumerate()
        .map(|(i, (spec, st))| {
            make_hill(
                spec,
                *st,
                w,
                sw as f32,
                sh as f32,
                scale,
                seed.wrapping_add(i as u32 * 31),
            )
        })
        .collect();
    let mottle = Field::from_fn(w.div_ceil(8) + 1, h.div_ceil(8) + 1, |x, y| {
        noise::fbm(
            x * 8.0 / (110.0 * scale),
            y * 8.0 / (45.0 * scale),
            seed ^ 0x77,
            4,
        )
    });
    paint_hills(&mut pm, &hills, &mottle, scale, seed);
    if let Some((color, dot, density)) = moss_spec {
        let unit = sh as f32 / 1600.0 * scale;
        for (k, hill) in hills.iter().rev().take(2).enumerate() {
            let f = if k == 0 { 1.0 } else { 0.8 };
            moss(
                &mut pm,
                hill,
                color,
                dot * unit * f,
                density * f,
                seed ^ (0x1234 + k as u32),
            );
        }
    }
    if let Some((spec, color)) = mist {
        paint_mist(&mut pm, spec, color, scale, sh as f32, seed ^ 0x1515);
    }
    if scale < 1.0 {
        upscale(&pm, lw, sh)
    } else {
        pm
    }
}

/// 烟云带：(中心高度, 厚度, 浓度)，比例相对屏幕高。
struct MistSpec {
    bands: Vec<(f32, f32, f32)>,
}

/// 在位图上画烟云（叠加，source-over）。
fn paint_mist(pm: &mut Pixmap, spec: &MistSpec, color: [f32; 3], scale: f32, sh: f32, seed: u32) {
    let w = pm.width() as usize;
    let unit = sh * scale;
    par_rows(pm.data_mut(), w, |y, row| {
        let yf = y as f32 + 0.5;
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let xf = x as f32 + 0.5;
            let mut a_tot = 0.0f32;
            for (bi, &(cy, th, k)) in spec.bands.iter().enumerate() {
                let bs = seed.wrapping_add(bi as u32 * 977);
                // 带的中线与厚度沿 x 缓慢起伏；云团沿水平方向拉长，时聚时散。
                let wave = (noise::fbm1(xf / (unit * 0.9), bs, 3) - 0.5) * unit * 0.10;
                let thick = th * unit * (0.4 + 1.1 * noise::fbm1(xf / (unit * 0.5), bs ^ 0x7, 3));
                let dy = (yf - cy * unit - wave) / thick.max(1.0);
                if dy.abs() > 2.4 {
                    continue;
                }
                let lump = noise::fbm(xf / (unit * 0.20), yf / (unit * 0.05), bs ^ 0x33, 4);
                // 时断时续：烟云成片，不是一条贯穿的带子。
                let patches = smoothstep(0.30, 0.62, noise::fbm1(xf / (unit * 0.45), bs ^ 0x99, 3));
                let body = (-dy * dy * 1.3).exp();
                let a = smoothstep(0.28, 0.80, body * (0.45 + 1.0 * lump))
                    * k
                    * (0.25 + 0.75 * patches);
                a_tot = 1.0 - (1.0 - a_tot) * (1.0 - a);
            }
            if a_tot > 0.002 {
                let inv = 1.0 - a_tot;
                px[0] = (color[0] * a_tot * 255.0 + px[0] as f32 * inv + 0.5) as u8;
                px[1] = (color[1] * a_tot * 255.0 + px[1] as f32 * inv + 0.5) as u8;
                px[2] = (color[2] * a_tot * 255.0 + px[2] as f32 * inv + 0.5) as u8;
                px[3] = (a_tot * 255.0 + px[3] as f32 * inv + 0.5) as u8;
            }
        }
    });
}

/// 五层山水的笔墨与布局。
struct Plan {
    far: Vec<(RangeSpec, HillStyle)>,
    mid: Vec<(RangeSpec, HillStyle)>,
    near: Vec<(RangeSpec, HillStyle)>,
    far_mist: (MistSpec, [f32; 3]),
    front_mist: (MistSpec, [f32; 3]),
    moss_mid: ([f32; 3], f32, f32),
    moss_near: ([f32; 3], f32, f32),
}

fn plan(theme: Theme) -> Plan {
    let dark = theme == Theme::Dark;
    let none = HillStyle {
        ink: [0.0; 3],
        alpha: 0.0,
        edge_w: 0.0,
        edge_alpha: 0.0,
        fade: 0.8,
        cun: 0.0,
        dry: 0.0,
        rim: None,
        tint: None,
    };
    // 布局（两种外观相同）：远山承日、中山居左、近山压左下角，中间留白。
    let far_specs = [
        RangeSpec {
            base: 0.60,
            height: 0.24,
            aspect: 0.9,
            sharp: 1.5,
            crag: 0.05,
            groups: vec![(0.87, 0.13, 0.30), (0.17, 0.12, 0.19), (0.50, 0.06, 0.09)],
            spacing: 0.45,
        },
        RangeSpec {
            base: 0.645,
            height: 0.13,
            aspect: 1.1,
            sharp: 1.6,
            crag: 0.04,
            groups: vec![(0.63, 0.10, 0.13), (0.05, 0.10, 0.12), (0.33, 0.09, 0.07)],
            spacing: 0.35,
        },
    ];
    let mid_specs = [
        RangeSpec {
            base: 0.77,
            height: 0.17,
            aspect: 1.25,
            sharp: 1.7,
            crag: 0.045,
            groups: vec![(0.27, 0.17, 0.20), (0.97, 0.08, 0.10)],
            spacing: 0.5,
        },
        RangeSpec {
            base: 0.81,
            height: 0.09,
            aspect: 1.4,
            sharp: 1.8,
            crag: 0.035,
            groups: vec![(0.56, 0.11, 0.08), (0.08, 0.08, 0.06)],
            spacing: 0.4,
        },
    ];
    let near_specs = [
        RangeSpec {
            base: 0.98,
            height: 0.20,
            aspect: 1.15,
            sharp: 1.8,
            crag: 0.04,
            groups: vec![(0.03, 0.15, 0.37), (0.87, 0.11, 0.15)],
            spacing: 0.55,
        },
        RangeSpec {
            base: 1.03,
            height: 0.12,
            aspect: 1.5,
            sharp: 2.0,
            crag: 0.035,
            groups: vec![(0.22, 0.12, 0.13), (0.69, 0.09, 0.08), (0.99, 0.08, 0.11)],
            spacing: 0.45,
        },
    ];
    // 笔墨：亮色为纸上墨；暗色为夜绢上的月白（山脊受月光、山脚隐入夜色），近山勾泥金。
    let styles: [HillStyle; 6] = if dark {
        let moon = rgb(0xD8D1C1);
        let gold = rgb(pigment::NIJIN);
        [
            HillStyle {
                ink: moon,
                alpha: 0.10,
                edge_w: 10.0,
                edge_alpha: 0.06,
                fade: 0.9,
                ..none
            },
            HillStyle {
                ink: moon,
                alpha: 0.13,
                edge_w: 10.0,
                edge_alpha: 0.10,
                fade: 0.8,
                dry: 0.1,
                ..none
            },
            HillStyle {
                ink: moon,
                alpha: 0.17,
                edge_w: 8.0,
                edge_alpha: 0.30,
                fade: 0.7,
                cun: 0.10,
                dry: 0.45,
                ..none
            },
            HillStyle {
                ink: moon,
                alpha: 0.20,
                edge_w: 8.0,
                edge_alpha: 0.34,
                fade: 0.65,
                cun: 0.11,
                dry: 0.5,
                ..none
            },
            HillStyle {
                ink: moon,
                alpha: 0.20,
                edge_w: 10.0,
                edge_alpha: 0.55,
                fade: 0.55,
                cun: 0.14,
                dry: 0.55,
                rim: Some((gold, 0.85)),
                ..none
            },
            HillStyle {
                ink: moon,
                alpha: 0.23,
                edge_w: 10.0,
                edge_alpha: 0.60,
                fade: 0.5,
                cun: 0.15,
                dry: 0.6,
                rim: Some((gold, 0.85)),
                ..none
            },
        ]
    } else {
        let mid = rgb(0x57534B);
        let near = rgb(0x24221D);
        let ochre = rgb(pigment::ZHESHI);
        [
            HillStyle {
                ink: mix3(rgb(0x9C978C), rgb(pigment::HUAQING), 0.32),
                alpha: 0.30,
                edge_w: 10.0,
                edge_alpha: 0.10,
                fade: 0.95,
                ..none
            },
            HillStyle {
                ink: mix3(rgb(0x8A857A), rgb(pigment::HUAQING), 0.18),
                alpha: 0.40,
                edge_w: 10.0,
                edge_alpha: 0.16,
                fade: 0.85,
                dry: 0.1,
                ..none
            },
            HillStyle {
                ink: mid,
                alpha: 0.48,
                edge_w: 8.0,
                edge_alpha: 0.42,
                fade: 0.75,
                cun: 0.24,
                dry: 0.45,
                ..none
            },
            HillStyle {
                ink: mid,
                alpha: 0.56,
                edge_w: 8.0,
                edge_alpha: 0.48,
                fade: 0.7,
                cun: 0.26,
                dry: 0.5,
                ..none
            },
            HillStyle {
                ink: near,
                alpha: 0.80,
                edge_w: 10.0,
                edge_alpha: 0.62,
                fade: 0.62,
                cun: 0.36,
                dry: 0.55,
                tint: Some((ochre, 0.24)),
                ..none
            },
            HillStyle {
                ink: near,
                alpha: 0.86,
                edge_w: 10.0,
                edge_alpha: 0.66,
                fade: 0.6,
                cun: 0.38,
                dry: 0.6,
                tint: Some((ochre, 0.26)),
                ..none
            },
        ]
    };
    let [f0, f1] = far_specs;
    let [m0, m1] = mid_specs;
    let [n0, n1] = near_specs;
    // 烟云即“留白”：两种外观都用纸色把山脚抹去。
    let mist_rgb = rgb(theme.palette().paper);
    let (far_k, front_k) = if dark { (0.72, 0.78) } else { (0.78, 0.82) };
    Plan {
        far: vec![(f0, styles[0]), (f1, styles[1])],
        mid: vec![(m0, styles[2]), (m1, styles[3])],
        near: vec![(n0, styles[4]), (n1, styles[5])],
        far_mist: (
            MistSpec {
                bands: vec![(0.635, 0.045, far_k)],
            },
            mist_rgb,
        ),
        front_mist: (
            MistSpec {
                bands: vec![(0.875, 0.045, front_k), (0.72, 0.020, front_k * 0.55)],
            },
            mist_rgb,
        ),
        moss_mid: (if dark { rgb(0x6E695E) } else { rgb(0x3A3833) }, 7.0, 0.5),
        moss_near: (
            if dark {
                mix3(rgb(pigment::NIJIN), rgb(0x15140F), 0.2)
            } else {
                rgb(0x141310)
            },
            9.0,
            0.7,
        ),
    }
}

/// 册页底部的一抹远山（与长卷同一画法：两重淡山 + 山脚烟云），透明底，`w × h`。
pub(crate) fn distant_range(theme: Theme, w: u32, h: u32, seed: u32) -> Pixmap {
    let p = plan(theme);
    let back = RangeSpec {
        base: 0.90,
        height: 0.16,
        aspect: 1.0,
        sharp: 1.5,
        crag: 0.045,
        groups: vec![(0.20, 0.10, 0.19), (0.78, 0.12, 0.15), (0.50, 0.05, 0.05)],
        spacing: 1.0,
    };
    let front = RangeSpec {
        base: 0.95,
        height: 0.10,
        aspect: 1.3,
        sharp: 1.7,
        crag: 0.04,
        groups: vec![(0.08, 0.10, 0.12), (0.34, 0.07, 0.05), (0.92, 0.09, 0.09)],
        spacing: 1.0,
    };
    let mist = MistSpec {
        bands: vec![(0.955, 0.03, if theme == Theme::Dark { 0.7 } else { 0.75 })],
    };
    let ranges = [(back, p.far[0].1), (front, p.far[1].1)];
    mountain_layer(
        w,
        h,
        w,
        0.25,
        &ranges,
        None,
        Some((&mist, rgb(theme.palette().paper))),
        seed,
    )
}

/// 生成全部图层。
pub fn landscape(theme: Theme, width: u32, height: u32, pages: usize, seed: u32) -> Vec<Layer> {
    let (sw, sh) = (width.max(16), height.max(16));
    let lws: Vec<u32> = PARALLAX
        .iter()
        .map(|&p| layer_width(sw, p, pages))
        .collect();
    let plan = plan(theme);
    let mut out: Vec<Option<Pixmap>> = vec![None, None, None, None, None];
    std::thread::scope(|scope| {
        let h_sky = scope.spawn(|| sky(theme, sw, sh, lws[0], seed));
        let h_far = scope.spawn(|| {
            let (spec, c) = &plan.far_mist;
            mountain_layer(
                sw,
                sh,
                lws[1],
                0.25,
                &plan.far,
                None,
                Some((spec, *c)),
                seed ^ 0xfa,
            )
        });
        let h_mid = scope.spawn(|| {
            mountain_layer(
                sw,
                sh,
                lws[2],
                0.5,
                &plan.mid,
                Some(plan.moss_mid),
                None,
                seed ^ 0x3d,
            )
        });
        let h_near = scope.spawn(|| {
            mountain_layer(
                sw,
                sh,
                lws[3],
                1.0,
                &plan.near,
                Some(plan.moss_near),
                None,
                seed ^ 0x2e,
            )
        });
        let h_mist = scope.spawn(|| {
            let lw = lws[4];
            let scale = 0.25;
            let (w, h) = (
                ((lw as f32) * scale).ceil() as u32,
                ((sh as f32) * scale).ceil() as u32,
            );
            let mut pm = crate::pixmap(w, h);
            let (spec, c) = &plan.front_mist;
            paint_mist(&mut pm, spec, *c, scale, sh as f32, seed ^ 0x0e);
            upscale(&pm, lw, sh)
        });
        out[0] = Some(h_sky.join().expect("天"));
        out[1] = Some(h_far.join().expect("远山"));
        out[2] = Some(h_mid.join().expect("中山"));
        out[3] = Some(h_near.join().expect("近山"));
        out[4] = Some(h_mist.join().expect("烟"));
    });
    out.into_iter()
        .zip(PARALLAX)
        .map(|(pm, parallax)| Layer {
            pixmap: pm.expect("图层"),
            parallax,
        })
        .collect()
}

/// 把各层按视差合成为一张屏幕大小的画（`scroll` 为主屏横向滚动的像素），用于预览与测试。
pub fn composite(layers: &[Layer], width: u32, height: u32, scroll: f32) -> Pixmap {
    let mut out = crate::pixmap(width, height);
    for l in layers {
        out.draw_pixmap(
            -(l.parallax * scroll).round() as i32,
            0,
            l.pixmap.as_ref(),
            &tiny_skia::PixmapPaint::default(),
            tiny_skia::Transform::identity(),
            None,
        );
    }
    out
}
