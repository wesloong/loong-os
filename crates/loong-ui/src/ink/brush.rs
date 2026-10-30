//! 毛笔笔触。
//!
//! 把线条路径（来自 SVG 或程序生成）当作“笔路”，沿笔路按压力曲线盖“笔肚”印（圆形笔迹），
//! 记录每个像素属于哪一印、离笔路中线多远，再逐像素加上：
//! - **起笔 / 行笔 / 收笔**：落笔先顿（略粗、藏锋的圆头），行笔略细并有轻微起伏，
//!   收笔或出锋（渐细成尖）或回锋（略顿后收圆）；转折处顿笔加粗；
//! - **飞白**：墨随笔路渐干，沿笔向拉长、横向很密的各向异性噪声让笔毛之间露出纸色，
//!   笔触两侧先干；
//! - **积墨**：笔触边缘比中间略浓，叠笔处更浓；
//! - **晕开**：湿笔处覆盖度模糊后沿纸纤维向外渗出一圈淡墨。

use resvg::usvg;
use tiny_skia::{PathSegment, Rect, Transform};

use super::noise::{self, Rng};
use super::raster::{Field, mix, smoothstep};

/// 笔法参数。
#[derive(Debug, Clone, Copy)]
pub struct BrushStyle {
    /// 行笔时的笔触宽度（像素）。
    pub width: f32,
    /// 粗细变化强度（0 = 等粗，1 = 标准书写）。
    pub pressure: f32,
    /// 飞白程度（0 = 湿笔，1 = 很干）。
    pub dryness: f32,
    /// 晕开程度（0 = 不晕，1 = 生宣上的明显晕染）。
    pub bleed: f32,
    /// 纹理细节（笔毛丝纹、毛边、飞白）的强度；小尺寸时应接近 0 以保证清晰。
    pub texture: f32,
    /// 闭合路径是否留一个小缺口（像“圆相”那样一笔画圆，收笔不完全闭合）。
    pub enso_gap: bool,
    /// 收笔方式：`Some(true)` 一律出锋（渐细成尖），`Some(false)` 一律回锋（收圆），
    /// `None` 由种子决定（较长的笔画约一半出锋）。
    pub taper: Option<bool>,
}

impl BrushStyle {
    /// 按笔触宽度给出合适的默认笔法：越小越“工整”，越大笔意越足。
    pub fn for_width(width: f32) -> BrushStyle {
        let detail = smoothstep(2.5, 9.0, width);
        BrushStyle {
            width,
            pressure: 0.35 + 0.65 * detail,
            dryness: 0.55 * detail,
            bleed: 0.15 + 0.55 * detail,
            texture: detail,
            enso_gap: false,
            taper: None,
        }
    }
}

/// 一笔的笔路（像素坐标的折线）。
#[derive(Debug, Clone)]
pub struct Stroke {
    pub pts: Vec<(f32, f32)>,
    pub closed: bool,
}

impl Stroke {
    pub fn open(pts: Vec<(f32, f32)>) -> Stroke {
        Stroke { pts, closed: false }
    }

    /// 折线长度。
    pub fn length(&self) -> f32 {
        let mut l: f32 = self
            .pts
            .windows(2)
            .map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt())
            .sum();
        if self.closed && self.pts.len() > 1 {
            let (a, b) = (self.pts[self.pts.len() - 1], self.pts[0]);
            l += ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        }
        l
    }
}

/// 从 SVG 中取出所有描边路径，变换到 `rect`（等比居中），展平成折线。
/// 返回笔路与 SVG 单位到像素的缩放比例。
pub fn svg_strokes(svg: &str, rect: Rect) -> (Vec<Stroke>, f32) {
    let Ok(tree) = usvg::Tree::from_str(svg, &usvg::Options::default()) else {
        return (Vec::new(), 1.0);
    };
    let size = tree.size();
    let scale = (rect.width() / size.width()).min(rect.height() / size.height());
    let tx = rect.x() + (rect.width() - size.width() * scale) / 2.0;
    let ty = rect.y() + (rect.height() - size.height() * scale) / 2.0;
    let fit = Transform::from_scale(scale, scale).post_translate(tx, ty);
    let mut out = Vec::new();
    collect(tree.root(), fit, &mut out);
    (out, scale)
}

fn collect(group: &usvg::Group, fit: Transform, out: &mut Vec<Stroke>) {
    for node in group.children() {
        match node {
            usvg::Node::Group(g) => collect(g, fit, out),
            usvg::Node::Path(p) if p.stroke().is_some() => {
                let ts = p.abs_transform().post_concat(fit);
                flatten(p.data(), ts, out);
            }
            _ => {}
        }
    }
}

/// 把路径展平成折线（曲线按约 0.75 像素一段细分）。
pub fn flatten(path: &tiny_skia::Path, ts: Transform, out: &mut Vec<Stroke>) {
    let map = |p: tiny_skia::Point| {
        let mut q = p;
        ts.map_point(&mut q);
        (q.x, q.y)
    };
    let mut cur: Vec<(f32, f32)> = Vec::new();
    let mut last = (0.0f32, 0.0f32);
    let finish = |cur: &mut Vec<(f32, f32)>, closed: bool, out: &mut Vec<Stroke>| {
        if !cur.is_empty() {
            let mut pts = std::mem::take(cur);
            let mut closed = closed;
            // 首尾重合的路径也算闭合。
            if pts.len() > 2 {
                let (a, b) = (pts[0], pts[pts.len() - 1]);
                if (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01 {
                    pts.pop();
                    closed = true;
                }
            }
            out.push(Stroke { pts, closed });
        }
    };
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                finish(&mut cur, false, out);
                last = map(p);
                cur.push(last);
            }
            PathSegment::LineTo(p) => {
                last = map(p);
                cur.push(last);
            }
            PathSegment::QuadTo(c, p) => {
                let (c, p) = (map(c), map(p));
                let n = seg_count(&[last, c, p]);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    cur.push((
                        u * u * last.0 + 2.0 * u * t * c.0 + t * t * p.0,
                        u * u * last.1 + 2.0 * u * t * c.1 + t * t * p.1,
                    ));
                }
                last = p;
            }
            PathSegment::CubicTo(c1, c2, p) => {
                let (c1, c2, p) = (map(c1), map(c2), map(p));
                let n = seg_count(&[last, c1, c2, p]);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    cur.push((
                        a * last.0 + b * c1.0 + c * c2.0 + d * p.0,
                        a * last.1 + b * c1.1 + c * c2.1 + d * p.1,
                    ));
                }
                last = p;
            }
            PathSegment::Close => {
                let first = cur.first().copied();
                finish(&mut cur, true, out);
                if let Some(f) = first {
                    last = f;
                }
            }
        }
    }
    finish(&mut cur, false, out);
}

fn seg_count(ctrl: &[(f32, f32)]) -> usize {
    let len: f32 = ctrl
        .windows(2)
        .map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt())
        .sum();
    ((len / 0.75).ceil() as usize).clamp(2, 400)
}

/// 调整笔顺：横向的笔画从左往右，其余从上往下；闭合路径从左上角起笔、顺时针行笔。
pub fn normalize_direction(s: &mut Stroke) {
    if s.pts.len() < 2 {
        return;
    }
    if s.closed {
        let mut area = 0.0;
        for i in 0..s.pts.len() {
            let (a, b) = (s.pts[i], s.pts[(i + 1) % s.pts.len()]);
            area += a.0 * b.1 - b.0 * a.1;
        }
        if area < 0.0 {
            s.pts.reverse();
        }
        let start = s
            .pts
            .iter()
            .enumerate()
            .min_by(|a, b| {
                (a.1.0 + a.1.1)
                    .partial_cmp(&(b.1.0 + b.1.1))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        s.pts.rotate_left(start);
    } else {
        let (a, b) = (s.pts[0], s.pts[s.pts.len() - 1]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let flip = if dx.abs() > dy.abs() * 1.2 {
            dx < 0.0
        } else {
            dy < 0.0
        };
        if flip {
            s.pts.reverse();
        }
    }
}

/// 一个“笔肚印”。
#[derive(Clone, Copy)]
struct Dab {
    x: f32,
    y: f32,
    /// 半径。
    r: f32,
    /// 单位切向。
    dx: f32,
    dy: f32,
    /// 沿笔路的弧长位置。
    s: f32,
    /// 干湿（0 湿 … 1 干）。
    dry: f32,
    /// 墨量（浓淡）。
    ink: f32,
}

/// 沿笔路等距重采样（含闭合段），返回点与累积弧长。
fn resample(stroke: &Stroke, step: f32, extra: f32) -> (Vec<(f32, f32)>, Vec<f32>) {
    let mut pts = stroke.pts.clone();
    if stroke.closed && !pts.is_empty() {
        pts.push(pts[0]);
    }
    if pts.len() == 1 {
        return (pts.clone(), vec![0.0]);
    }
    let mut out = vec![pts[0]];
    let mut arc = vec![0.0];
    let mut carry = 0.0f32;
    let mut total = 0.0f32;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        if len < 1e-6 {
            continue;
        }
        let mut d = step - carry;
        while d <= len {
            let t = d / len;
            out.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
            arc.push(total + d);
            d += step;
        }
        carry = len - (d - step);
        total += len;
    }
    // 补上终点。
    let end = pts[pts.len() - 1];
    if carry > step * 0.2 {
        out.push(end);
        arc.push(total);
    }
    // 闭合路径多走一小段，让收笔压住起笔（叠墨）；extra < 0 表示留缺口。
    if stroke.closed && extra != 0.0 {
        if extra > 0.0 {
            let n0 = out.len();
            let mut k = 1;
            while k < n0 && arc[k] < extra {
                out.push(out[k]);
                arc.push(total + arc[k]);
                k += 1;
            }
        } else {
            while out.len() > 2 && *arc.last().unwrap() > total + extra {
                out.pop();
                arc.pop();
            }
        }
    }
    (out, arc)
}

/// 画一组笔画，返回墨量场（0..1）。
pub fn render(strokes: &[Stroke], style: &BrushStyle, w: usize, h: usize, seed: u32) -> Field {
    let mut canvas = Field::new(w, h);
    let mut wet = Field::new(w, h);
    for (i, s) in strokes.iter().enumerate() {
        let sd = seed.wrapping_mul(0x9e37).wrapping_add(i as u32 * 7919);
        paint_stroke(&mut canvas, &mut wet, s, style, sd, i);
    }
    apply_bleed(&mut canvas, &wet, style, seed);
    canvas
}

/// 晕开：湿笔处的覆盖度模糊后，沿纸纤维向外渗出一圈淡墨。
pub fn apply_bleed(canvas: &mut Field, wet: &Field, style: &BrushStyle, seed: u32) {
    if style.bleed <= 0.01 {
        return;
    }
    let mut halo = wet.clone();
    let r = (style.width * 0.16).clamp(1.0, 18.0);
    halo.blur(r);
    let strength = 0.42 * style.bleed;
    let w = canvas.w;
    for (i, (c, &hv)) in canvas.data.iter_mut().zip(&halo.data).enumerate() {
        if hv <= 0.003 {
            continue;
        }
        let (x, y) = ((i % w) as f32, (i / w) as f32);
        // 纤维方向的各向异性噪声：渗出的边缘呈细丝状而不是平滑的雾。
        let fib = noise::value(x * 0.55, y * 0.16, seed ^ 0xb1ee)
            * noise::value(x * 0.17, y * 0.6, seed ^ 0x51ab);
        let add = hv * strength * (0.35 + 1.6 * fib);
        *c += add.min(0.6) * (1.0 - *c);
    }
}

/// 压力曲线：返回相对粗细（1 = 标准笔宽）。起笔处（s = 0）笔已按下，最粗；
/// 随后提笔行进，“腰”略细；收笔或出锋渐细，或再按一下（垂露/回锋）。
fn pressure_at(s: f32, len: f32, w: f32, taper: bool, amount: f32, wobble: f32) -> f32 {
    // 起笔顿后提笔到行笔粗细。长笔画中段略细（两头重、中间轻）。
    let waist = if len > 4.0 * w { 0.86 } else { 0.94 };
    let lift = smoothstep(0.0, 1.6 * w, s);
    let mut p = mix(1.16, waist, lift);
    // 收笔。
    let tail = if taper {
        (2.4 * w).min(0.5 * len)
    } else {
        (1.3 * w).min(0.35 * len)
    }
    .max(1e-3);
    let to_end = len - s;
    if to_end < tail {
        let u = 1.0 - to_end / tail; // 0 → 1
        p = if taper {
            mix(p, 0.10, u.powf(1.3))
        } else {
            // 垂露/回锋：收笔前再按一下，末端收圆。
            mix(p, 1.10, smoothstep(0.0, 0.75, u)) - 0.22 * smoothstep(0.75, 1.0, u)
        };
    }
    let base = p + wobble;
    1.0 + (base - 1.0) * amount
}

fn paint_stroke(
    canvas: &mut Field,
    wet: &mut Field,
    stroke: &Stroke,
    style: &BrushStyle,
    seed: u32,
    index: usize,
) {
    let w = style.width;
    if w <= 0.0 || stroke.pts.is_empty() {
        return;
    }
    let mut rng = Rng::new(seed, 0xb0b);
    let len = stroke.length();
    let step = (w * 0.12).clamp(0.3, 2.0);

    // 点：路径极短（如 Lucide 用 h.01 表示的圆点）→ 一个略呈侧锋的墨点。
    if len < w * 0.6 {
        let (x, y) = stroke.pts[0];
        let r = w * 0.5 * (1.0 + 0.25 * style.pressure);
        let ang = 0.7f32; // 侧点：从左上向右下顿笔
        let (dx, dy) = (ang.cos(), ang.sin());
        let dabs: Vec<Dab> = (0..6)
            .map(|k| {
                let u = k as f32 / 5.0;
                let off = (u - 0.4) * r * 0.55 * style.pressure;
                Dab {
                    x: x + dx * off,
                    y: y + dy * off,
                    r: r * (0.78 + 0.22 * (u * std::f32::consts::PI).sin()),
                    dx,
                    dy,
                    s: u * r,
                    dry: 0.0,
                    ink: 1.0,
                }
            })
            .collect();
        rasterize(canvas, wet, &dabs, style, seed);
        return;
    }

    let closed_extra = if stroke.closed {
        if style.enso_gap {
            -(w * 0.9).min(len * 0.08)
        } else {
            w * 0.7
        }
    } else {
        0.0
    };
    let (pts, arc) = resample(stroke, step, closed_extra);
    if pts.len() < 2 {
        return;
    }
    let total = *arc.last().unwrap();
    let n = pts.len();
    // 切向（中心差分，平滑一点避免折线带来的抖动）。
    let span = ((w * 0.35 / step).round() as usize).max(1);
    let tangent = |i: usize| {
        let a = pts[i.saturating_sub(span)];
        let b = pts[(i + span).min(n - 1)];
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l = (dx * dx + dy * dy).sqrt().max(1e-6);
        (dx / l, dy / l)
    };
    let tangents: Vec<(f32, f32)> = (0..n).map(tangent).collect();
    // 转折强度：前后半个笔宽内的转角。
    let win = ((w * 0.6 / step).round() as usize).max(1);
    let corner: Vec<f32> = (0..n)
        .map(|i| {
            let a = tangents[i.saturating_sub(win)];
            let b = tangents[(i + win).min(n - 1)];
            let cos = (a.0 * b.0 + a.1 * b.1).clamp(-1.0, 1.0);
            smoothstep(0.55, 1.2, cos.acos())
        })
        .collect();

    // 这一笔的个性：收笔方式、墨量、起伏。
    let long = total > w * 5.0;
    let coin = rng.f32() < 0.55;
    let taper = !stroke.closed && style.taper.unwrap_or(long && coin);
    let ink0 = 0.90 + 0.10 * rng.f32();
    // 每三笔蘸一次墨：笔越往后越干。
    let dip = (index % 3) as f32 * 0.12;
    let wobble_seed = rng.next_u64() as u32;
    let capacity = w * (14.0 + 10.0 * rng.f32());

    let mut dabs = Vec::with_capacity(n + 4);
    for i in 0..n {
        let s = arc[i];
        let wob = (noise::fbm1(s / (w * 3.5), wobble_seed, 2) - 0.5) * 0.16;
        let mut p = pressure_at(s, total, w, taper, style.pressure, wob);
        p += 0.20 * corner[i] * style.pressure;
        let r = (w * 0.5 * p).max(0.35);
        // 干湿：随行笔渐干；出锋处速度快更干；转折处顿笔，墨多。
        let mut dry = (s / capacity) * 0.9 + dip;
        if taper {
            dry += 0.45 * smoothstep(total - 2.5 * w, total, s);
        }
        dry -= 0.5 * corner[i];
        dry -= 0.6 * (1.0 - smoothstep(0.0, 1.5 * w, s)); // 落笔处最湿
        let dry = (dry * style.dryness).clamp(0.0, 0.95);
        let ink = ink0 * (1.0 - 0.10 * (s / capacity).min(1.0));
        let (dx, dy) = tangents[i];
        dabs.push(Dab {
            x: pts[i].0,
            y: pts[i].1,
            r,
            dx,
            dy,
            s,
            dry,
            ink,
        });
    }
    // 露锋起笔：笔尖先从左上方轻轻落下（细），再顿下铺开（粗），然后转入笔路。
    if style.pressure > 0.15 {
        let d0 = dabs[0];
        // 入笔方向：来自笔路后方偏左上。
        let (mut ex, mut ey) = (-d0.dx - 0.75, -d0.dy - 0.75);
        let el = (ex * ex + ey * ey).sqrt().max(1e-3);
        ex /= el;
        ey /= el;
        let reach = w * (0.55 + 0.30 * style.pressure) * rng.range(0.85, 1.15);
        let steps = ((reach / step).ceil() as usize).max(2);
        let mut head = Vec::with_capacity(steps);
        for k in (1..=steps).rev() {
            let u = k as f32 / steps as f32; // 1 = 笔尖，0 = 已按下
            let grow = 1.0 - u;
            head.push(Dab {
                x: d0.x + ex * reach * u,
                y: d0.y + ey * reach * u,
                r: (d0.r * (0.12 + 0.88 * grow.powf(0.85))).max(0.35),
                // 入笔段的“笔向”沿入笔方向，飞白丝纹才顺。
                dx: -ex,
                dy: -ey,
                s: -reach * u,
                dry: 0.0,
                ..d0
            });
        }
        head.extend(dabs);
        dabs = head;
    }
    rasterize(canvas, wet, &dabs, style, seed);
}

/// 把一串笔肚印栅格化，并逐像素加上纹理后累加到画布。
fn rasterize(canvas: &mut Field, wet: &mut Field, dabs: &[Dab], style: &BrushStyle, seed: u32) {
    if dabs.is_empty() {
        return;
    }
    let (cw, ch) = (canvas.w as i32, canvas.h as i32);
    let pad = 2.0;
    let mut x0 = f32::MAX;
    let mut y0 = f32::MAX;
    let mut x1 = f32::MIN;
    let mut y1 = f32::MIN;
    for d in dabs {
        x0 = x0.min(d.x - d.r - pad);
        y0 = y0.min(d.y - d.r - pad);
        x1 = x1.max(d.x + d.r + pad);
        y1 = y1.max(d.y + d.r + pad);
    }
    let bx0 = (x0.floor() as i32).clamp(0, cw);
    let by0 = (y0.floor() as i32).clamp(0, ch);
    let bx1 = (x1.ceil() as i32).clamp(0, cw);
    let by1 = (y1.ceil() as i32).clamp(0, ch);
    if bx1 <= bx0 || by1 <= by0 {
        return;
    }
    let (bw, bh) = ((bx1 - bx0) as usize, (by1 - by0) as usize);
    // 每个像素：最近（相对半径）的印的编号、相对距离与横向位置。
    let mut best = vec![f32::MAX; bw * bh];
    let mut across = vec![0.0f32; bw * bh];
    let mut owner = vec![u32::MAX; bw * bh];
    for (k, d) in dabs.iter().enumerate() {
        let rr = d.r + 1.0;
        let ix0 = ((d.x - rr).floor() as i32).max(bx0);
        let iy0 = ((d.y - rr).floor() as i32).max(by0);
        let ix1 = ((d.x + rr).ceil() as i32).min(bx1);
        let iy1 = ((d.y + rr).ceil() as i32).min(by1);
        let inv_r = 1.0 / d.r;
        for py in iy0..iy1 {
            let vy = py as f32 + 0.5 - d.y;
            let row = (py - by0) as usize * bw;
            for px in ix0..ix1 {
                let vx = px as f32 + 0.5 - d.x;
                let d2 = vx * vx + vy * vy;
                if d2 > rr * rr {
                    continue;
                }
                let score = d2.sqrt() * inv_r;
                let j = row + (px - bx0) as usize;
                if score < best[j] {
                    best[j] = score;
                    across[j] = (d.dx * vy - d.dy * vx) * inv_r;
                    owner[j] = k as u32;
                }
            }
        }
    }
    let tex = style.texture;
    for by in 0..bh {
        for bx in 0..bw {
            let j = by * bw + bx;
            let k = owner[j];
            if k == u32::MAX {
                continue;
            }
            let d = &dabs[k as usize];
            let score = best[j];
            let t = across[j].clamp(-1.0, 1.0);
            let at = t.abs();
            let s = d.s;
            // 毛边：笔毛在边缘参差。
            let hair = noise::value(
                s / (d.r * 0.9 + 0.5),
                if t > 0.0 { 3.7 } else { 11.3 },
                seed,
            ) - 0.5;
            let edge = d.r * (1.0 - score) + 0.5 + hair * tex * (0.35 + 1.4 * d.dry) * d.r * 0.25;
            let cov = edge.clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            // 笔毛丝纹：横向很密、沿笔向很长的噪声。
            let bristles = (d.r * 1.1).clamp(3.0, 18.0);
            let streak_len = d.r * 2.6 + 2.0;
            let sx = t * bristles;
            let sy = s / streak_len;
            let streak = 0.65 * noise::value(sx, sy, seed ^ 0x5eed)
                + 0.35 * noise::value(sx * 2.3, sy * 1.7, seed ^ 0x7a11);
            // 飞白：越干、越靠两侧，露白越多。
            let dry = d.dry * (0.45 + 0.75 * at * at.sqrt());
            let gap = if dry > 0.01 {
                smoothstep(dry - 0.10, dry + 0.10, streak)
            } else {
                1.0
            };
            // 墨色：饱满均匀，边缘积墨略浓一点点，丝纹带来细微浓淡。
            let density = d.ink
                * (0.95 + 0.05 * smoothstep(0.6, 0.95, at))
                * (1.0 - 0.12 * tex * (streak - 0.5));
            let a = cov * density * mix(1.0, gap, tex.max(0.35));
            let (gx, gy) = (bx0 as usize + bx, by0 as usize + by);
            let c = canvas.at(gx, gy);
            *c = 1.0 - (1.0 - *c) * (1.0 - a.clamp(0.0, 1.0));
            let wv = wet.at(gx, gy);
            *wv = wv.max(cov * (1.0 - d.dry));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tapered_ends_are_thinner_than_the_middle() {
        let stroke = Stroke::open((0..=200).map(|i| (20.0 + i as f32, 50.0)).collect());
        let style = BrushStyle {
            width: 16.0,
            pressure: 1.0,
            dryness: 0.0,
            bleed: 0.0,
            texture: 0.0,
            enso_gap: false,
            taper: None,
        };
        // 笔形：笔尖（露锋）细 → 顿笔处粗 → 行笔“腰”略细 → 收笔出锋细（或回锋收圆）。
        // 多试几个种子，保证至少有出锋的情况。
        let mut saw_taper = false;
        for seed in 0..8 {
            let f = render(std::slice::from_ref(&stroke), &style, 260, 100, seed);
            let thickness = |x: usize| (0..100).filter(|&y| f.get(x, y) > 0.5).count();
            let mid = thickness(120);
            assert!(mid >= 12, "行笔太细: {mid}");
            let tip = (0..260).find(|&x| thickness(x) > 0).expect("有墨");
            assert!(tip < 20, "露锋应在起点之前入笔");
            assert!(
                thickness(tip + 1) * 2 < mid,
                "笔尖 {} 不够尖",
                thickness(tip + 1)
            );
            assert!(thickness(22) > mid, "起笔没有顿: {} ≤ {mid}", thickness(22));
            let end = thickness(218);
            assert!(end <= mid + 2, "收笔 {end} 远粗于行笔 {mid}");
            if end * 2 < mid {
                saw_taper = true;
            }
        }
        assert!(saw_taper, "没有出锋的笔画");
    }

    #[test]
    fn svg_symbols_flatten_to_strokes() {
        let svg = crate::icon::builtin_symbol("clock").unwrap();
        let (strokes, scale) = svg_strokes(svg, Rect::from_xywh(0.0, 0.0, 96.0, 96.0).unwrap());
        assert!((scale - 4.0).abs() < 1e-4);
        assert!(strokes.len() >= 2);
        let circle = strokes.iter().find(|s| s.closed).expect("圆是闭合路径");
        assert!((circle.length() - 2.0 * std::f32::consts::PI * 40.0).abs() < 4.0);
    }

    #[test]
    fn dots_render_as_ink_blobs() {
        let s = Stroke::open(vec![(30.0, 30.0), (30.04, 30.0)]);
        let f = render(&[s], &BrushStyle::for_width(10.0), 60, 60, 1);
        assert!(f.get(30, 30) > 0.6);
        assert!(f.get(5, 5) < 0.01);
    }
}
