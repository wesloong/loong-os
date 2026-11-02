//! 已安装 Web 应用（PWA）的「宣纸小笺」：把 manifest 的图标画到宣纸底板上，没有图标时用名称首字。
//!
//! - `maskable` 图标：图标铺满小笺（连续圆角遮罩 + 毛边），表面压一层极淡的纸纹，像印在纸上；
//! - `any` 图标：小笺照常是宣纸 + 一团淡彩，图标缩到 70% 居中放在纸上（设计规范 §10.1）；
//! - 没有图标：名称首字用文楷写在纸上，再按毛笔的方式晕开（积墨、飞白），颜料取自 `theme_color`。
//!
//! 图标位图由 [`decode_icon`] 从 manifest 图标文件（PNG / SVG）解码，尺寸与像素数都有上限，
//! 不信任文件声明的大小。

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Rect, Transform};

use super::brush::{self, BrushStyle};
use super::icon::{pigment_on, stroke_ink, wash_blob};
use super::noise::{self, Rng};
use super::paper;
use super::raster::{Field, mix3, path_mask, rgb, smoothstep};
use crate::theme::{Theme, pigment};

/// 图标文件解码的上限：边长与总像素数（PNG 头里声明的尺寸超过它就不解码）。
pub const MAX_ICON_SIDE: u32 = 2048;
const MAX_ICON_PIXELS: u64 = 2048 * 2048;

/// `any` 图标在小笺上所占的比例（设计规范 §10.1）。
pub const ANY_ICON_SCALE: f32 = 0.70;

/// 小笺上画什么。
#[derive(Clone, Copy)]
pub enum WebIconArt<'a> {
    /// manifest 中的图标（预乘 RGBA，已缩放到要画的大小：`maskable` 为小笺边长，
    /// 否则为边长的 [`ANY_ICON_SCALE`]；尺寸不符时按比例缩放并居中）。
    Image { pixmap: &'a Pixmap, maskable: bool },
    /// 没有图标：名称首字的字形（只用 alpha 通道，任意大小，按墨迹的外接框居中）。
    Glyph(&'a Pixmap),
}

/// Web 应用图标的描述。
#[derive(Clone, Copy)]
pub struct WebIconSpec<'a> {
    pub art: WebIconArt<'a>,
    /// 淡彩颜料（0xRRGGBB，见 [`pigment_for_color`]）。
    pub pigment: u32,
    pub theme: Theme,
    pub seed: u32,
}

/// 小笺底色（与内置图标一致）。
fn tile_base(theme: Theme) -> u32 {
    match theme {
        Theme::Light => 0xF8F4EA,
        Theme::Dark => 0x2A2720,
    }
}

/// 已安装 Web 应用的「宣纸小笺」，`size × size`，底板外透明（墨影由合成器另画）。
pub fn web_app_icon(size: u32, spec: &WebIconSpec<'_>) -> Pixmap {
    let n = size.max(8) as usize;
    let s = n as f32;
    let theme = spec.theme;
    let mut rng = Rng::new(spec.seed, 0x3eb);

    // 底板：连续圆角 + 毛边（与内置图标相同）。
    let inset = (s * 0.02).max(1.0);
    let tile_rect = Rect::from_xywh(inset, inset, s - 2.0 * inset, s - 2.0 * inset).unwrap();
    let path = crate::shape::continuous_rect(
        tile_rect,
        tile_rect.width() * crate::theme::radius::ICON_RATIO,
    );
    let mut mask = path_mask(&path, n, n, Transform::identity());
    paper::deckle(&mut mask, (s * 0.014).max(0.7), spec.seed ^ 0xdec);
    let mut edge = mask.clone();
    edge.blur((s * 0.008).max(0.8));

    // 纸面：底色 + 纸纹 + 左上受光。
    let base = rgb(tile_base(theme));
    let tile = paper::tile(theme);
    let (ox, oy) = ((rng.f32() * 300.0) as i32, (rng.f32() * 300.0) as i32);
    let tex_k = match theme {
        Theme::Light => 0.9,
        Theme::Dark => 1.4,
    };
    let grain = |i: usize| tile.at((i % n) as i32 + ox, (i / n) as i32 + oy) * tex_k;
    let light = |i: usize| 0.02 * (1.0 - ((i % n) + (i / n)) as f32 / (2.0 * s));
    let mut buf: Vec<[f32; 3]> = (0..n * n)
        .map(|i| {
            let l = 1.0 + grain(i) + light(i);
            [base[0] * l, base[1] * l, base[2] * l]
        })
        .collect();

    let wash = |buf: &mut [[f32; 3]], rng: &mut Rng, k: f32| {
        let cx = s * rng.range(0.48, 0.60);
        let cy = s * rng.range(0.50, 0.62);
        let r = s * rng.range(0.32, 0.38);
        let f = wash_blob(
            n,
            n,
            cx,
            cy,
            r * 1.1,
            r * 0.9,
            rng.range(-0.6, 0.6),
            spec.seed ^ 0x3a5,
        );
        let pig = pigment_on(theme, spec.pigment);
        for (c, &a) in buf.iter_mut().zip(&f.data) {
            if a > 0.0 {
                *c = mix3(*c, pig, a * k);
            }
        }
    };
    let wash_k = match theme {
        Theme::Light => 0.40,
        Theme::Dark => 0.55,
    };

    match spec.art {
        WebIconArt::Image {
            pixmap,
            maskable: true,
        } => {
            // 图标铺满小笺；纸纹减半压在上面，像印在宣纸上。
            let img = fit(pixmap, n as u32, n as u32);
            over(&mut buf, &img, 0, 0, n);
            for (i, c) in buf.iter_mut().enumerate() {
                let l = 1.0 + 0.5 * grain(i) + light(i);
                *c = [c[0] * l, c[1] * l, c[2] * l];
            }
        }
        WebIconArt::Image {
            pixmap,
            maskable: false,
        } => {
            wash(&mut buf, &mut rng, wash_k * 0.6);
            let side = (s * ANY_ICON_SCALE).round().max(1.0) as u32;
            let img = fit(pixmap, side, side);
            let off = ((n as u32 - side) / 2) as i32;
            over(&mut buf, &img, off, off, n);
            // 极淡的纸纹透上来。
            for (i, c) in buf.iter_mut().enumerate() {
                let l = 1.0 + 0.3 * grain(i);
                *c = [c[0] * l, c[1] * l, c[2] * l];
            }
        }
        WebIconArt::Glyph(glyph) => {
            wash(&mut buf, &mut rng, wash_k);
            let ink = brushed_glyph(glyph, n, spec.seed);
            let ink_rgb = rgb(stroke_ink(theme));
            for (c, &a) in buf.iter_mut().zip(&ink.data) {
                if a > 0.0 {
                    *c = mix3(*c, ink_rgb, (a * 0.97).min(1.0));
                }
            }
        }
    }

    // 纸边一圈极淡的墨线（与内置图标一致）。
    let rim_rgb = rgb(theme.palette().ink[3]);
    let mut pm = crate::pixmap(size, size);
    let data = pm.data_mut();
    for i in 0..n * n {
        let a = mask.data[i];
        if a <= 0.0 {
            continue;
        }
        let rim = (1.0 - smoothstep(0.55, 0.97, edge.data[i])) * 0.30;
        let c = mix3(buf[i], rim_rgb, rim);
        let px = &mut data[i * 4..i * 4 + 4];
        px[0] = (c[0].clamp(0.0, 1.0) * a * 255.0 + 0.5) as u8;
        px[1] = (c[1].clamp(0.0, 1.0) * a * 255.0 + 0.5) as u8;
        px[2] = (c[2].clamp(0.0, 1.0) * a * 255.0 + 0.5) as u8;
        px[3] = (a * 255.0 + 0.5) as u8;
    }
    pm
}

/// 把预乘位图 `img` 以 source-over 叠到 `buf`（n × n 的非预乘 RGB）的 `(x0, y0)` 处。
fn over(buf: &mut [[f32; 3]], img: &Pixmap, x0: i32, y0: i32, n: usize) {
    let w = img.width() as i32;
    for (j, px) in img.data().chunks_exact(4).enumerate() {
        let a = px[3] as f32 / 255.0;
        if a <= 0.0 {
            continue;
        }
        let (x, y) = (x0 + j as i32 % w, y0 + j as i32 / w);
        if x < 0 || y < 0 || x as usize >= n || y as usize >= n {
            continue;
        }
        let c = &mut buf[y as usize * n + x as usize];
        for k in 0..3 {
            c[k] = px[k] as f32 / 255.0 + c[k] * (1.0 - a);
        }
    }
}

/// 把位图按比例缩放进 `w × h`（居中，四周透明）。
fn fit(src: &Pixmap, w: u32, h: u32) -> Pixmap {
    if src.width() == w && src.height() == h {
        return src.clone();
    }
    let k = (w as f32 / src.width() as f32).min(h as f32 / src.height() as f32);
    let (sw, sh) = (
        (src.width() as f32 * k).round().max(1.0) as u32,
        (src.height() as f32 * k).round().max(1.0) as u32,
    );
    let scaled = resize(src, sw, sh);
    let mut out = crate::pixmap(w, h);
    out.draw_pixmap(
        ((w - sw.min(w)) / 2) as i32,
        ((h - sh.min(h)) / 2) as i32,
        scaled.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    out
}

/// 缩放到 `w × h`：缩小时按面积平均（每个目标像素取它覆盖的源像素的平均，预乘空间，
/// 大图标缩成小图标也不起锯齿）；放大时双三次。
pub fn resize(src: &Pixmap, w: u32, h: u32) -> Pixmap {
    let (sw, sh) = (src.width(), src.height());
    let (w, h) = (w.max(1), h.max(1));
    if w > sw || h > sh {
        let mut out = crate::pixmap(w, h);
        let paint = PixmapPaint {
            quality: FilterQuality::Bicubic,
            ..PixmapPaint::default()
        };
        let ts = Transform::from_scale(w as f32 / sw as f32, h as f32 / sh as f32);
        out.draw_pixmap(0, 0, src.as_ref(), &paint, ts, None);
        return out;
    }
    // 每个目标列/行覆盖的源区间 [a, b)（像素坐标，浮点）。
    let spans = |dst: u32, srcn: u32| -> Vec<(f32, f32)> {
        let k = srcn as f32 / dst as f32;
        (0..dst)
            .map(|i| (i as f32 * k, (i + 1) as f32 * k))
            .collect()
    };
    let (cols, rows) = (spans(w, sw), spans(h, sh));
    let data = src.data();
    let mut out = crate::pixmap(w, h);
    let od = out.data_mut();
    for (y, &(ya, yb)) in rows.iter().enumerate() {
        for (x, &(xa, xb)) in cols.iter().enumerate() {
            let mut acc = [0f32; 4];
            let mut area = 0f32;
            let mut sy = ya.floor() as u32;
            while (sy as f32) < yb && sy < sh {
                let wy = (yb.min(sy as f32 + 1.0) - ya.max(sy as f32)).max(0.0);
                let mut sx = xa.floor() as u32;
                while (sx as f32) < xb && sx < sw {
                    let wx = (xb.min(sx as f32 + 1.0) - xa.max(sx as f32)).max(0.0);
                    let wgt = wx * wy;
                    let i = ((sy * sw + sx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += data[i + c] as f32 * wgt;
                    }
                    area += wgt;
                    sx += 1;
                }
                sy += 1;
            }
            let o = (y * w as usize + x) * 4;
            for c in 0..4 {
                od[o + c] = (acc[c] / area.max(1e-6)).round().clamp(0.0, 255.0) as u8;
            }
            // 预乘：颜色分量不超过 alpha。
            for c in 0..3 {
                od[o + c] = od[o + c].min(od[o + 3]);
            }
        }
    }
    out
}

/// 首字的字形 → 毛笔墨迹：按墨迹外接框居中、缩放到小笺的约 52%，再加积墨、晕开与飞白。
fn brushed_glyph(glyph: &Pixmap, n: usize, seed: u32) -> Field {
    let (gw, gh) = (glyph.width() as usize, glyph.height() as usize);
    let alpha = |x: usize, y: usize| glyph.data()[(y * gw + x) * 4 + 3];
    // 墨迹的外接框（字形位图四周常有留白）。
    let (mut x0, mut y0, mut x1, mut y1) = (gw, gh, 0, 0);
    for y in 0..gh {
        for x in 0..gw {
            if alpha(x, y) > 8 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    let mut cov = Field::new(n, n);
    if x1 <= x0 || y1 <= y0 {
        return cov;
    }
    let (bw, bh) = ((x1 - x0) as f32, (y1 - y0) as f32);
    let target = n as f32 * 0.52;
    let k = target / bw.max(bh);
    // 视觉中心略偏下（字的重心偏上，放低一点更稳）。
    let (cx, cy) = (n as f32 * 0.5, n as f32 * 0.52);
    for y in 0..n {
        for x in 0..n {
            let sx = (x as f32 + 0.5 - cx) / k + x0 as f32 + bw / 2.0;
            let sy = (y as f32 + 0.5 - cy) / k + y0 as f32 + bh / 2.0;
            *cov.at(x, y) = sample_alpha(glyph, sx, sy);
        }
    }
    // 积墨：笔画边缘略浓、中间略淡；飞白：沿横向的纤维噪声扯出一些空隙。
    let mut wet = cov.clone();
    wet.blur((n as f32 * 0.012).max(0.6));
    for (i, (c, &wv)) in cov.data.iter_mut().zip(&wet.data).enumerate() {
        if *c <= 0.0 {
            continue;
        }
        let (x, y) = ((i % n) as f32, (i / n) as f32);
        let edge = (1.0 - wv).clamp(0.0, 1.0) * 0.18;
        let dry = noise::value(x * 0.9, y * 0.22, seed ^ 0x9f1);
        let gap = smoothstep(0.78, 0.95, dry) * 0.45;
        *c = (*c * (0.86 + edge) * (1.0 - gap)).clamp(0.0, 1.0);
    }
    let style = BrushStyle::for_width(n as f32 * 0.09);
    brush::apply_bleed(&mut cov, &wet, &style, seed);
    cov
}

/// 双线性取字形位图的 alpha（0..1），界外为 0。
fn sample_alpha(pm: &Pixmap, x: f32, y: f32) -> f32 {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
    let (tx, ty) = (fx - ix as f32, fy - iy as f32);
    let get = |x: i32, y: i32| {
        if x < 0 || y < 0 || x >= w || y >= h {
            0.0
        } else {
            pm.data()[((y * w + x) * 4 + 3) as usize] as f32 / 255.0
        }
    };
    let top = get(ix, iy) * (1.0 - tx) + get(ix + 1, iy) * tx;
    let bottom = get(ix, iy + 1) * (1.0 - tx) + get(ix + 1, iy + 1) * tx;
    top * (1.0 - ty) + bottom * ty
}

/// 图标文件的格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconFormat {
    Png,
    Svg,
}

/// 按内容识别图标格式（PNG 签名；SVG 为文本且开头 1 KiB 内有 `<svg`）。
pub fn sniff_icon(data: &[u8]) -> Option<IconFormat> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(IconFormat::Png);
    }
    let head = &data[..data.len().min(1024)];
    let text = std::str::from_utf8(head).ok().or_else(|| {
        // 截断处可能在多字节字符中间：退回到最后一个完整字符。
        std::str::from_utf8(&head[..head.len().saturating_sub(3)]).ok()
    })?;
    text.contains("<svg").then_some(IconFormat::Svg)
}

/// PNG 头（IHDR）里声明的宽高。
pub fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 24 || &data[12..16] != b"IHDR" {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
    Some((be(16), be(20)))
}

/// 解码 manifest 图标（PNG 或 SVG），按比例缩放进 `size × size`（居中，预乘 RGBA）。
/// 格式不认识、尺寸超限或内容损坏时返回 `None`。
pub fn decode_icon(data: &[u8], size: u32) -> Option<Pixmap> {
    let size = size.max(1);
    match sniff_icon(data)? {
        IconFormat::Png => {
            let (w, h) = png_size(data)?;
            if w == 0 || h == 0 || w.max(h) > MAX_ICON_SIDE || w as u64 * h as u64 > MAX_ICON_PIXELS
            {
                return None;
            }
            let pm = Pixmap::decode_png(data).ok()?;
            Some(fit(&pm, size, size))
        }
        IconFormat::Svg => {
            let opts = resvg::usvg::Options::default();
            let tree = resvg::usvg::Tree::from_data(data, &opts).ok()?;
            let ts = tree.size();
            let k = (size as f32 / ts.width()).min(size as f32 / ts.height());
            if !k.is_finite() || k <= 0.0 {
                return None;
            }
            let tx = (size as f32 - ts.width() * k) / 2.0;
            let ty = (size as f32 - ts.height() * k) / 2.0;
            let mut pm = crate::pixmap(size, size);
            resvg::render(
                &tree,
                Transform::from_scale(k, k).post_translate(tx, ty),
                &mut pm.as_mut(),
            );
            Some(pm)
        }
    }
}

/// 由任意颜色（manifest 的 `theme_color` 等，0xRRGGBB）选一种中国传统颜料作淡彩：按色相取最近的
/// 胭脂、藤黄、赭石、石绿、石青、花青（朱砂留给品牌与提示）。颜色太灰（黑、白、灰）时返回 `None`。
pub fn pigment_for_color(color: u32) -> Option<u32> {
    let (h, chroma, l) = hue_chroma(color);
    // 用色度（最大与最小分量之差）而不是 HSL 饱和度判断“灰”：近白的纸色饱和度可以很高，
    // 但几乎没有颜色。
    if chroma < 0.12 || !(0.06..=0.94).contains(&l) {
        return None;
    }
    // 各颜料的代表色相（度）。红色一侧归胭脂，偏橙的暖色看亮度分藤黄 / 赭石。
    use pigment::*;
    let candidates = [
        (YANZHI, 350.0),
        (YANZHI, -10.0),
        (ZHESHI, 22.0),
        (TENGHUANG, 44.0),
        (SHILV, 140.0),
        (SHIQING, 196.0),
        (HUAQING, 218.0),
        (HUAQING, 260.0),
        (YANZHI, 310.0),
    ];
    let mut best = (f32::MAX, ZHESHI);
    for (p, hue) in candidates {
        let d = (h - hue).abs().min(360.0 - (h - hue).abs());
        if d < best.0 {
            best = (d, p);
        }
    }
    // 暗而偏暖（棕色）的颜色更像赭石，而不是藤黄。
    if best.1 == TENGHUANG && l < 0.38 {
        return Some(ZHESHI);
    }
    Some(best.1)
}

/// 没有可用颜色时，按种子在颜料中轮换（同一应用总是同一种）。
pub fn fallback_pigment(seed: u32) -> u32 {
    use pigment::*;
    const ALL: [u32; 6] = [SHIQING, SHILV, HUAQING, TENGHUANG, ZHESHI, YANZHI];
    ALL[(seed % ALL.len() as u32) as usize]
}

/// 0xRRGGBB → (色相 0..360, 色度 0..1, 亮度 0..1)。
fn hue_chroma(c: u32) -> (f32, f32, f32) {
    let [r, g, b] = rgb(c);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l);
    }
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, d, l)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Pixmap {
        let mut pm = crate::pixmap(w, h);
        for px in pm.data_mut().chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        pm
    }

    #[test]
    fn pigments_follow_the_hue() {
        use pigment::*;
        assert_eq!(pigment_for_color(0x2E5E74), Some(SHIQING));
        assert_eq!(pigment_for_color(0x1A73E8), Some(HUAQING));
        assert_eq!(pigment_for_color(0x34A853), Some(SHILV));
        assert_eq!(pigment_for_color(0xD93025), Some(YANZHI));
        assert_eq!(pigment_for_color(0xF9AB00), Some(TENGHUANG));
        assert_eq!(pigment_for_color(0x7A4A22), Some(ZHESHI));
        // 黑、白、灰没有颜料。
        for c in [0x000000, 0xFFFFFF, 0x808080, 0xF3EEE2] {
            assert_eq!(pigment_for_color(c), None, "{c:06X}");
        }
        // 朱砂不给应用。
        for c in [0xFF0000, 0xB83A2B, 0xE91E63] {
            assert_ne!(pigment_for_color(c), Some(ZHUSHA));
        }
        assert_eq!(fallback_pigment(7), fallback_pigment(7));
    }

    #[test]
    fn sniffs_and_bounds_icons() {
        let mut png = Vec::new();
        {
            let pm = solid(4, 2, [255, 0, 0, 255]);
            png.extend(pm.encode_png().unwrap());
        }
        assert_eq!(sniff_icon(&png), Some(IconFormat::Png));
        assert_eq!(png_size(&png), Some((4, 2)));
        let svg = br##"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#2e5e74"/></svg>"##;
        assert_eq!(sniff_icon(svg), Some(IconFormat::Svg));
        assert_eq!(sniff_icon(b"GIF89a...."), None);
        assert_eq!(sniff_icon(b"<html><body>404</body></html>"), None);

        // PNG：按比例缩进正方形，居中。
        let pm = decode_icon(&png, 8).unwrap();
        assert_eq!((pm.width(), pm.height()), (8, 8));
        assert_eq!(pm.pixel(4, 0).unwrap().alpha(), 0);
        assert_eq!(pm.pixel(4, 4).unwrap().alpha(), 255);
        // SVG：渲染到目标大小。
        let pm = decode_icon(svg, 16).unwrap();
        assert_eq!(pm.pixel(8, 8).unwrap().alpha(), 255);
        // 声明的尺寸超限：不解码。
        let mut huge = png.clone();
        huge[16..20].copy_from_slice(&40_000u32.to_be_bytes());
        assert!(decode_icon(&huge, 8).is_none());
        assert!(decode_icon(b"\x89PNG\r\n\x1a\nbroken", 8).is_none());
    }

    #[test]
    fn downscale_averages_area() {
        // 左半黑、右半白的 4×2 缩成 2×1：两个像素分别是黑、白；缩成 1×1 是中灰。
        let mut pm = solid(4, 2, [255, 255, 255, 255]);
        for y in 0..2 {
            for x in 0..2 {
                let i = (y * 4 + x) * 4;
                pm.data_mut()[i..i + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        let half = resize(&pm, 2, 1);
        assert_eq!(half.pixel(0, 0).unwrap().red(), 0);
        assert_eq!(half.pixel(1, 0).unwrap().red(), 255);
        let one = resize(&pm, 1, 1);
        assert!((one.pixel(0, 0).unwrap().red() as i32 - 128).abs() <= 1);
        let up = resize(&pm, 8, 4);
        assert_eq!((up.width(), up.height()), (8, 4));
    }

    #[test]
    fn web_icons_keep_the_tile_shape() {
        let img = solid(48, 48, [46, 94, 116, 255]);
        let glyph = {
            let mut g = crate::pixmap(40, 40);
            for y in 8..32 {
                for x in 18..22 {
                    g.data_mut()[(y * 40 + x) * 4 + 3] = 255;
                }
            }
            g
        };
        for theme in [Theme::Light, Theme::Dark] {
            for art in [
                WebIconArt::Image {
                    pixmap: &img,
                    maskable: true,
                },
                WebIconArt::Image {
                    pixmap: &img,
                    maskable: false,
                },
                WebIconArt::Glyph(&glyph),
            ] {
                let spec = WebIconSpec {
                    art,
                    pigment: pigment::SHIQING,
                    theme,
                    seed: 9,
                };
                let pm = web_app_icon(96, &spec);
                assert_eq!((pm.width(), pm.height()), (96, 96));
                // 底板外透明，中心不透明；同样的输入得到同样的结果。
                assert_eq!(pm.pixel(0, 0).unwrap().alpha(), 0);
                assert_eq!(pm.pixel(48, 48).unwrap().alpha(), 255);
                assert_eq!(pm.data(), web_app_icon(96, &spec).data());
            }
        }
        // maskable 图标铺满小笺：靠近边缘处是图标的颜色；any 图标在边缘处是纸色。
        let full = web_app_icon(
            96,
            &WebIconSpec {
                art: WebIconArt::Image {
                    pixmap: &img,
                    maskable: true,
                },
                pigment: pigment::SHIQING,
                theme: Theme::Light,
                seed: 1,
            },
        );
        let inset = web_app_icon(
            96,
            &WebIconSpec {
                art: WebIconArt::Image {
                    pixmap: &img,
                    maskable: false,
                },
                pigment: pigment::SHIQING,
                theme: Theme::Light,
                seed: 1,
            },
        );
        let edge = |pm: &Pixmap| pm.pixel(48, 6).unwrap();
        assert!(edge(&full).red() < 90, "{:?}", edge(&full));
        assert!(edge(&inset).red() > 180, "{:?}", edge(&inset));
        // 首字：中间那一竖是墨。
        let glyph_icon = web_app_icon(
            96,
            &WebIconSpec {
                art: WebIconArt::Glyph(&glyph),
                pigment: pigment::SHIQING,
                theme: Theme::Light,
                seed: 1,
            },
        );
        let bar: u32 = (30..66)
            .flat_map(|y| (46..50).map(move |x| (x, y)))
            .map(|(x, y)| glyph_icon.pixel(x, y).unwrap().red() as u32)
            .sum::<u32>()
            / (36 * 4);
        assert!(bar < 130, "{bar}");
    }
}
