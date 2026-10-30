//! 「绢」面板与「墨影」。

use tiny_skia::{Pixmap, Rect, Transform};

use super::noise;
use super::paper;
use super::raster::{Field, mix3, path_mask, rgb, smoothstep};
use crate::theme::Theme;

/// 「绢」：背景模糊 + 暖色调 + 绢纹 + 柔和毛边 + 极淡的边线。
pub fn silk_panel(theme: Theme, background: &Pixmap, rect: Rect, radius: f32) -> Pixmap {
    let (w, h) = (
        rect.width().ceil().max(1.0) as u32,
        rect.height().ceil().max(1.0) as u32,
    );
    let (wu, hu) = (w as usize, h as usize);
    // 模糊背景（1/4 分辨率模糊再放大，复用毛玻璃材质的做法），不着色。
    let blur_r = (rect.height().min(rect.width()) * 0.18).clamp(8.0, 48.0);
    let blurred = crate::material::frosted_panel(
        background,
        rect,
        0.0,
        blur_r,
        tiny_skia::Color::TRANSPARENT,
    );
    let pal = theme.palette();
    let (tint, tint_k) = match theme {
        Theme::Light => (rgb(pal.paper), 0.50),
        Theme::Dark => (rgb(pal.paper_raised), 0.50),
    };
    let warm = rgb(0xE9DCC0);

    // 形状与毛边。
    let shape = |inset: f32| {
        let r =
            Rect::from_xywh(inset, inset, w as f32 - 2.0 * inset, h as f32 - 2.0 * inset).unwrap();
        path_mask(
            &crate::shape::continuous_rect(r, (radius - inset + 1.0).max(1.0)),
            wu,
            hu,
            Transform::identity(),
        )
    };
    let mut mask = shape(1.0);
    let mut edge = mask.clone();
    edge.blur((radius * 0.25).clamp(2.0, 10.0));
    paper::deckle(&mut mask, (rect.height() * 0.012).clamp(0.8, 2.5), 0x5117);
    // 装裱的绫边：离边缘一小段的一圈细线。
    let inset = (rect.height() * 0.06).clamp(3.0, 14.0);
    let line_w = (rect.height() * 0.008).clamp(1.0, 2.5);
    let outer = shape(inset);
    let inner = shape(inset + line_w);
    let border_k = match theme {
        Theme::Light => 0.16,
        Theme::Dark => 0.22,
    };

    // 绢纹：经纬细织纹的纹理砖（暗色外观的纸纹就是绢）。
    let tile = paper::tile(Theme::Dark);
    let tex_k = match theme {
        Theme::Light => 1.3,
        Theme::Dark => 1.4,
    };
    let opacity = match theme {
        Theme::Light => 0.84,
        Theme::Dark => 0.86,
    };
    let rim = rgb(pal.ink[3]);
    let mut out = crate::pixmap(w, h);
    let src = blurred.data();
    let data = out.data_mut();
    for i in 0..wu * hu {
        let m = mask.data[i];
        if m <= 0.0 {
            continue;
        }
        let (x, y) = (i % wu, i / wu);
        let p = &src[i * 4..i * 4 + 4];
        let a = p[3] as f32 / 255.0;
        let bg = if a > 0.0 {
            [
                p[0] as f32 / 255.0 / a,
                p[1] as f32 / 255.0 / a,
                p[2] as f32 / 255.0 / a,
            ]
        } else {
            tint
        };
        let mut c = mix3(bg, tint, tint_k);
        if theme == Theme::Light {
            c = mix3(c, warm, 0.10);
        }
        // 上缘略亮、下缘略暗：绢面受光。
        let l = 1.0 + tile.at(x as i32, y as i32) * tex_k + 0.025 * (0.5 - y as f32 / h as f32);
        c = [c[0] * l, c[1] * l, c[2] * l];
        // 边缘一圈极淡的墨线（绢裱的边）。
        let e = edge.data[i];
        let rim_k = (1.0 - smoothstep(0.45, 0.8, e)) * 0.25;
        c = mix3(c, rim, rim_k);
        let ring = (outer.data[i] - inner.data[i]).max(0.0);
        c = mix3(c, rim, ring * border_k);
        let alpha = m * opacity;
        let px = &mut data[i * 4..i * 4 + 4];
        px[0] = (c[0].clamp(0.0, 1.0) * alpha * 255.0 + 0.5) as u8;
        px[1] = (c[1].clamp(0.0, 1.0) * alpha * 255.0 + 0.5) as u8;
        px[2] = (c[2].clamp(0.0, 1.0) * alpha * 255.0 + 0.5) as u8;
        px[3] = (alpha * 255.0 + 0.5) as u8;
    }
    out
}

/// 「墨影」：暖灰、柔和、略不规则；贴近卡片处略浓（接触阴影），向外淡开；略向下偏。
pub fn ink_shadow(theme: Theme, width: u32, height: u32, radius: f32, spread: u32) -> Pixmap {
    let (w, h) = (width + 2 * spread, height + 2 * spread);
    let (wu, hu) = (w as usize, h as usize);
    let sp = spread as f32;
    // 接触影贴近卡片、略向下；远影更散、更向下（光从上方来）。
    let card_at = |dy: f32, shrink: f32| {
        let r = Rect::from_xywh(
            sp + shrink,
            sp + dy + shrink,
            (width as f32 - 2.0 * shrink).max(1.0),
            (height as f32 - 2.0 * shrink).max(1.0),
        )
        .unwrap();
        path_mask(
            &crate::shape::continuous_rect(r, radius),
            wu,
            hu,
            Transform::identity(),
        )
    };
    let mut near = card_at(sp * 0.10, 0.0);
    near.blur((sp * 0.14).max(1.0));
    let mut far = card_at(sp * 0.38, sp * 0.12);
    far.blur((sp * 0.36).max(1.0));
    let (color, k_near, k_far) = match theme {
        Theme::Light => (rgb(0x3A3020), 0.12, 0.17),
        Theme::Dark => ([0.0, 0.0, 0.0], 0.35, 0.45),
    };
    // 低频噪声：影子边缘略有浓淡，像墨晕而不是数学阴影。
    let lumpy = Field::from_fn(wu.div_ceil(8), hu.div_ceil(8), |x, y| {
        0.80 + 0.40
            * noise::fbm(
                x * 8.0 / (sp * 2.0 + 8.0),
                y * 8.0 / (sp * 2.0 + 8.0),
                0x5ad0,
                3,
            )
    })
    .resized(wu, hu);
    let mut pm = crate::pixmap(w, h);
    let data = pm.data_mut();
    for i in 0..wu * hu {
        let a = ((near.data[i] * k_near + far.data[i] * k_far) * lumpy.data[i]).min(1.0);
        if a <= 0.0 {
            continue;
        }
        let px = &mut data[i * 4..i * 4 + 4];
        px[0] = (color[0] * a * 255.0 + 0.5) as u8;
        px[1] = (color[1] * a * 255.0 + 0.5) as u8;
        px[2] = (color[2] * a * 255.0 + 0.5) as u8;
        px[3] = (a * 255.0 + 0.5) as u8;
    }
    pm
}
