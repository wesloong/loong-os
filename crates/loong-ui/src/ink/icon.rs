//! 「宣纸小笺」应用图标与「册页」应用画面。

use tiny_skia::{Color, Pixmap, Rect, Transform};

use super::IconSpec;
use super::brush::{self, BrushStyle};
use super::noise::{self, Rng};
use super::paper;
use super::raster::{Field, mix3, path_mask, rgb, smoothstep};
use crate::theme::{Theme, pigment};

/// 内置符号建议搭配的颜料（没骨淡彩），按应用气质选择；朱砂留给品牌与提示，不在此列。
pub fn suggested_pigment(symbol: &str) -> u32 {
    use pigment::*;
    match symbol {
        "compass" | "image" | "gamepad-2" | "scan-line" | "terminal" => SHIQING,
        "message-circle" | "map" | "list-checks" | "trending-up" => SHILV,
        "mail" | "clock" | "languages" | "newspaper" | "camera" => HUAQING,
        "music" | "play-circle" | "mic" | "heart-pulse" | "podcast" | "calendar" => YANZHI,
        "notebook-pen" | "cloud-sun" | "house" | "timer" => TENGHUANG,
        _ => ZHESHI,
    }
}

/// 某外观下笔触的墨色（亮色焦墨，暗色月白）。
pub fn stroke_ink(theme: Theme) -> u32 {
    theme.palette().ink[0]
}

/// 小笺底色：比壁纸的纸略亮（亮色）/ 略亮的墨绢（暗色），让图标从纸面上浮起。
fn tile_base(theme: Theme) -> u32 {
    match theme {
        Theme::Light => 0xF8F4EA,
        Theme::Dark => 0x2A2720,
    }
}

/// 颜料在纸上的显色：亮色直接用颜料；暗色提亮一些，像绢上的矿物色。
pub fn pigment_on(theme: Theme, pig: u32) -> [f32; 3] {
    match theme {
        Theme::Light => rgb(pig),
        // 暗色：像绢上的矿物色，提亮且略加饱和，避免在深底上发灰发浑。
        Theme::Dark => {
            let c = mix3(rgb(pig), rgb(0xE6E0D2), 0.30);
            let l = (c[0] + c[1] + c[2]) / 3.0;
            let sat = 1.35;
            [
                (l + (c[0] - l) * sat).clamp(0.0, 1.0),
                (l + (c[1] - l) * sat).clamp(0.0, 1.0),
                (l + (c[2] - l) * sat).clamp(0.0, 1.0),
            ]
        }
    }
}

/// 没骨色晕：一团边缘略浓（水渍）、边界不规则、带颜料颗粒的淡彩。返回 0..1 的浓度场。
#[allow(clippy::too_many_arguments)]
pub fn wash_blob(
    w: usize,
    h: usize,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    angle: f32,
    seed: u32,
) -> Field {
    let (ca, sa) = (angle.cos(), angle.sin());
    let scale = rx.max(ry);
    let mut f = Field::new(w, h);
    let x0 = ((cx - scale * 1.5).floor().max(0.0)) as usize;
    let x1 = ((cx + scale * 1.5).ceil().max(0.0) as usize).min(w);
    let y0 = ((cy - scale * 1.5).floor().max(0.0)) as usize;
    let y1 = ((cy + scale * 1.5).ceil().max(0.0) as usize).min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            let (px, py) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let u = (px * ca + py * sa) / rx;
            let v = (-px * sa + py * ca) / ry;
            let d = (u * u + v * v).sqrt();
            if d > 1.6 {
                continue;
            }
            let n = noise::fbm(
                x as f32 / (scale * 0.45),
                y as f32 / (scale * 0.45),
                seed,
                4,
            ) - 0.5;
            let dd = d + n * 0.55;
            if dd >= 1.0 {
                continue;
            }
            // 水渍：颜料被水推到边缘，边上一圈更浓；中间有不匀的“水花”。
            let body = 0.50
                + 0.18
                    * (noise::fbm(
                        x as f32 / (scale * 0.25),
                        y as f32 / (scale * 0.25),
                        seed ^ 0xa1,
                        3,
                    ) - 0.5);
            let ring = smoothstep(0.62, 0.97, dd) * (1.0 - smoothstep(0.97, 1.0, dd));
            let fade = 1.0 - smoothstep(0.90, 1.0, dd);
            let gran = 0.88 + 0.24 * noise::value(x as f32 * 0.6, y as f32 * 0.6, seed ^ 0x6a);
            f.data[y * w + x] = ((body + 0.55 * ring) * fade * gran).clamp(0.0, 1.0);
        }
    }
    f
}

/// 只画笔触、返回墨量场（与 `w × h` 画布等大）。
pub fn symbol_field(svg: &str, rect: Rect, w: usize, h: usize, weight: f32, seed: u32) -> Field {
    let (mut strokes, scale) = brush::svg_strokes(svg, rect);
    for s in &mut strokes {
        brush::normalize_direction(s);
    }
    let width = 2.0 * scale * weight * 1.12;
    let style = BrushStyle::for_width(width);
    brush::render(&strokes, &style, w, h, seed)
}

/// 「宣纸小笺」。
pub fn app_icon(size: u32, spec: &IconSpec<'_>) -> Pixmap {
    let n = size.max(8) as usize;
    let s = n as f32;
    let theme = spec.theme;
    let mut rng = Rng::new(spec.seed, 0x1c0);

    // 底板形状：连续圆角 + 毛边（四周留出约 2% 给毛边）。
    let inset = (s * 0.02).max(1.0);
    let tile_rect = Rect::from_xywh(inset, inset, s - 2.0 * inset, s - 2.0 * inset).unwrap();
    let path = crate::shape::continuous_rect(
        tile_rect,
        tile_rect.width() * crate::theme::radius::ICON_RATIO,
    );
    let mut mask = path_mask(&path, n, n, Transform::identity());
    paper::deckle(&mut mask, (s * 0.014).max(0.7), spec.seed ^ 0xdec);

    // 纸面：底色 + 纸纹 + 毛边处略透亮 + 左上受光。
    let mut edge = mask.clone();
    edge.blur((s * 0.008).max(0.8));
    let base = rgb(tile_base(theme));
    let tile = paper::tile(theme);
    let (ox, oy) = ((rng.f32() * 300.0) as i32, (rng.f32() * 300.0) as i32);
    let tex_k = match theme {
        Theme::Light => 0.9,
        Theme::Dark => 1.4,
    };
    let mut buf: Vec<[f32; 3]> = (0..n * n)
        .map(|i| {
            let (x, y) = (i % n, i / n);
            let light = 0.02 * (1.0 - (x + y) as f32 / (2.0 * s));
            let l = 1.0 + tile.at(x as i32 + ox, y as i32 + oy) * tex_k + light;
            [base[0] * l, base[1] * l, base[2] * l]
        })
        .collect();

    // 没骨淡彩。
    let cx = s * rng.range(0.50, 0.64);
    let cy = s * rng.range(0.52, 0.66);
    let r = s * rng.range(0.30, 0.36);
    let wash = wash_blob(
        n,
        n,
        cx,
        cy,
        r * 1.12,
        r * 0.88,
        rng.range(-0.6, 0.6),
        spec.seed ^ 0x3a5,
    );
    let pig = pigment_on(theme, spec.pigment);
    let wash_k = match theme {
        Theme::Light => 0.40,
        Theme::Dark => 0.55,
    };
    for (c, &a) in buf.iter_mut().zip(&wash.data) {
        if a > 0.0 {
            *c = mix3(*c, pig, a * wash_k);
        }
    }

    // 笔触符号。
    let inset_sym = s * 0.215;
    let sym_rect = Rect::from_xywh(
        inset_sym,
        inset_sym,
        s - 2.0 * inset_sym,
        s - 2.0 * inset_sym,
    )
    .unwrap();
    let ink = symbol_field(spec.symbol_svg, sym_rect, n, n, 1.0, spec.seed);
    let ink_rgb = rgb(stroke_ink(theme));
    let ink_k = match theme {
        Theme::Light => 0.97,
        Theme::Dark => 0.97,
    };
    for (c, &a) in buf.iter_mut().zip(&ink.data) {
        if a > 0.0 {
            *c = mix3(*c, ink_rgb, (a * ink_k).min(1.0));
        }
    }

    // 纸边一圈极淡的墨线，让小笺在同色的纸面壁纸上也有轮廓。
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

/// 把笔触符号画到位图上。
pub fn brush_symbol(
    target: &mut Pixmap,
    svg: &str,
    rect: Rect,
    ink: Color,
    weight: f32,
    seed: u32,
) {
    let (w, h) = (target.width() as usize, target.height() as usize);
    let field = symbol_field(svg, rect, w, h, weight, seed);
    let c = [ink.red(), ink.green(), ink.blue()];
    super::raster::blend_field(target, &field, c, ink.alpha());
}

/// 「册页」：全屏的一开画页，作为演示应用的内容。纸面 + 大片没骨淡彩 + 大幅笔触符号
/// + 一方小印；顶部居中留白给合成器画标题（约上方 22% 高度内不放浓墨）。
pub fn app_sheet(
    theme: Theme,
    width: u32,
    height: u32,
    symbol_svg: &str,
    pig: u32,
    seed: u32,
) -> Pixmap {
    let (w, h) = (width.max(16) as usize, height.max(16) as usize);
    let (wf, hf) = (w as f32, h as f32);
    let m = wf.min(hf);
    let mut rng = Rng::new(seed, 0x5ee7);
    let pal = theme.palette();
    let mut pm = paper::paper_with(theme, w as u32, h as u32, pal.paper, seed);

    // 远处两重淡山与山脚烟云，压在画面底部，使画页有“景”。
    let hills = super::landscape::distant_range(theme, w as u32, h as u32, seed ^ 0x41);
    pm.draw_pixmap(
        0,
        0,
        hills.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        Transform::identity(),
        None,
    );

    // 册页的“镶边”：四周一圈略深的纸色（像裱在册页上的画心与边框）。
    let frame = m * 0.035;
    let border = rgb(pal.paper_raised);
    {
        let data = pm.data_mut();
        for y in 0..h {
            for x in 0..w {
                let d = (x as f32)
                    .min(y as f32)
                    .min(wf - 1.0 - x as f32)
                    .min(hf - 1.0 - y as f32);
                let t = 1.0 - smoothstep(frame * 0.8, frame * 1.1, d);
                if t > 0.0 {
                    super::raster::blend_px(data, y * w + x, border, t * 0.9);
                }
            }
        }
    }

    // 主体位置：水平居中、略偏下，避开顶部标题区。
    let sym = m * 0.52;
    let (cx, cy) = (wf * 0.5, hf * 0.585);
    // 淡彩：一大团在符号后方偏右下，左上方再轻轻点一小团。
    let pig_rgb = pigment_on(theme, pig);
    let k = match theme {
        Theme::Light => 0.30,
        Theme::Dark => 0.42,
    };
    let blobs = [
        (
            cx + sym * 0.10,
            cy + sym * 0.08,
            sym * 0.60,
            sym * 0.46,
            rng.range(-0.5, 0.5),
            1.0,
        ),
        (
            cx - sym * 0.46,
            cy - sym * 0.36,
            sym * 0.16,
            sym * 0.12,
            rng.range(-0.8, 0.8),
            0.45,
        ),
    ];
    for (i, &(bx, by, rx, ry, ang, amt)) in blobs.iter().enumerate() {
        let f = wash_blob(w, h, bx, by, rx, ry, ang, seed ^ (0x77 + i as u32));
        super::raster::blend_field(&mut pm, &f, pig_rgb, k * amt);
    }

    // 大幅笔触。
    let rect = Rect::from_xywh(cx - sym * 0.5, cy - sym * 0.5, sym, sym).unwrap();
    let ink = pal.ink(crate::theme::Ink::Jiao);
    brush_symbol(&mut pm, symbol_svg, rect, ink, 0.9, seed ^ 0x51);

    // 落款处一方小印。
    let seal_size = (m * 0.07).round().max(12.0) as u32;
    let seal = super::seal::seal(theme, seal_size, "墨韵", seed ^ 0x5ea1);
    let sx = (cx + sym * 0.5 + m * 0.02).min(wf - frame * 2.0 - seal_size as f32);
    let sy = (cy + sym * 0.5 - seal_size as f32 * 1.2).min(hf - frame * 2.0 - seal_size as f32);
    pm.draw_pixmap(
        sx as i32,
        sy as i32,
        seal.as_ref(),
        &tiny_skia::PixmapPaint {
            opacity: 0.92,
            ..Default::default()
        },
        Transform::identity(),
        None,
    );
    pm
}
