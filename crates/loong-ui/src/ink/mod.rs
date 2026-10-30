//! # 水墨绘画工具（「墨韵」设计语言）
//!
//! 在 CPU 上把系统界面的静态元素“画”出来：宣纸、毛笔笔触、山水长卷、宣纸小笺图标、
//! 印章、状态栏控件。全部程序生成（不占资源体积），只在开机或切换外观时画一次，
//! 之后由合成器在 GPU 上做动画。
//!
//! 本文件定义对外接口（合成器依赖它）；各函数的实现位于子模块。
//! 约定：所有尺寸为物理像素；颜色为 0xRRGGBB；同样的参数与 `seed` 必须得到完全相同的结果。

use tiny_skia::{Color, Pixmap, Rect};

use crate::theme::Theme;

pub mod noise;

/// 山水长卷的一层。
pub struct Layer {
    /// 这一层的位图。宽度 = 屏幕宽 + ceil(parallax × (页数 − 1) × 屏幕宽)，高度 = 屏幕高。
    pub pixmap: Pixmap,
    /// 视差系数：主屏横向滚动 `s` 像素时，这一层移动 `parallax × s` 像素（0 = 不动）。
    pub parallax: f32,
}

/// 壁纸「山水长卷」：从远到近的若干层（第一层为天色/纸底，不透明；其余带透明度）。
///
/// `pages` 为主屏页数（≥ 1），用于决定每层的宽度。
pub fn landscape(theme: Theme, width: u32, height: u32, pages: usize, seed: u32) -> Vec<Layer> {
    // 占位实现：单层纸色。WT1 替换为分层山水。
    let _ = (pages, seed);
    let mut pm = crate::pixmap(width, height);
    pm.fill(crate::theme::hex(theme.palette().paper, 1.0));
    vec![Layer {
        pixmap: pm,
        parallax: 0.0,
    }]
}

/// 宣纸纹理（不透明）。
pub fn paper(theme: Theme, width: u32, height: u32, seed: u32) -> Pixmap {
    let _ = seed;
    let mut pm = crate::pixmap(width, height);
    pm.fill(crate::theme::hex(theme.palette().paper_raised, 1.0));
    pm
}

/// 应用图标的描述。
#[derive(Debug, Clone, Copy)]
pub struct IconSpec<'a> {
    /// 符号的 SVG（线条图标，如 [`crate::icon::builtin_symbol`] 返回的 Lucide 图标）。
    pub symbol_svg: &'a str,
    /// 淡彩点缀颜料（0xRRGGBB，见 [`crate::theme::pigment`]）。
    pub pigment: u32,
    /// 外观。
    pub theme: Theme,
    /// 随机种子（每个应用不同，让纸纹和笔触各不相同）。
    pub seed: u32,
}

/// 「宣纸小笺」应用图标：连续圆角宣纸底板 + 淡彩色晕 + 毛笔笔触符号 + 墨影。
/// 返回 `size × size` 的位图（底板外透明）。
pub fn app_icon(size: u32, spec: &IconSpec<'_>) -> Pixmap {
    // 占位实现：沿用旧图标，用颜料做渐变。
    let style = crate::icon::IconStyle {
        top: crate::theme::hex(spec.pigment, 0.75),
        bottom: crate::theme::hex(spec.pigment, 1.0),
        glyph: Color::WHITE,
    };
    crate::icon::app_icon(size, spec.symbol_svg, style)
}

/// 把线条 SVG 符号用毛笔笔触画到 `target` 的 `rect` 区域。`weight` 为笔触相对粗细（1.0 = 标准）。
pub fn brush_symbol(
    target: &mut Pixmap,
    svg: &str,
    rect: Rect,
    ink: Color,
    weight: f32,
    seed: u32,
) {
    let _ = seed;
    crate::icon::draw_symbol(target, svg, rect, ink, Some(2.0 * weight));
}

/// 印章：`size × size`，朱砂印泥，`text` 为阴刻留白的字（1–4 个汉字）。
pub fn seal(theme: Theme, size: u32, text: &str, seed: u32) -> Pixmap {
    let _ = (text, seed);
    let mut pm = crate::pixmap(size, size);
    pm.fill(crate::theme::hex(theme.palette().accent, 1.0));
    pm
}

/// 状态栏电池：毛笔画的电池外框 + 按电量填墨。`level` 为 0..1，`charging` 时加闪电。
pub fn battery(theme: Theme, height: u32, level: f32, charging: bool) -> Pixmap {
    let _ = (level, charging);
    let svg = crate::icon::builtin_symbol("battery-full").expect("内置图标");
    let mut pm = crate::pixmap(height * 2, height);
    let ink = theme.palette().ink(crate::theme::Ink::Nong);
    brush_symbol(
        &mut pm,
        svg,
        Rect::from_xywh(0.0, 0.0, (height * 2) as f32, height as f32).unwrap(),
        ink,
        1.0,
        0,
    );
    pm
}

/// 状态栏 Wi‑Fi：毛笔画的三道弧，`strength` 为 0..=3 格。
pub fn wifi(theme: Theme, height: u32, strength: u8) -> Pixmap {
    let _ = strength;
    let svg = crate::icon::builtin_symbol("wifi").expect("内置图标");
    let mut pm = crate::pixmap(height, height);
    let ink = theme.palette().ink(crate::theme::Ink::Nong);
    brush_symbol(
        &mut pm,
        svg,
        Rect::from_xywh(0.0, 0.0, height as f32, height as f32).unwrap(),
        ink,
        1.0,
        0,
    );
    pm
}

/// 「绢」面板：取 `background` 中 `rect` 区域，模糊 + 暖色调 + 纤维纹理 + 柔和毛边。
pub fn silk_panel(theme: Theme, background: &Pixmap, rect: Rect, radius: f32) -> Pixmap {
    let tint = crate::theme::hex(theme.palette().paper, 0.45);
    crate::material::frosted_panel(background, rect, radius, 24.0, tint)
}

/// 「墨影」：`width × height` 的卡片在纸上投下的柔和阴影（含外扩边距 `spread`，返回位图更大）。
pub fn ink_shadow(theme: Theme, width: u32, height: u32, radius: f32, spread: u32) -> Pixmap {
    let w = width + 2 * spread;
    let h = height + 2 * spread;
    let mut pm = crate::pixmap(w, h);
    let mut alpha = vec![0u8; (w * h) as usize];
    for y in spread..spread + height {
        for x in spread..spread + width {
            alpha[(y * w + x) as usize] = 255;
        }
    }
    crate::material::blur_alpha(
        &mut alpha,
        w as usize,
        h as usize,
        (spread / 2).max(1) as usize,
    );
    let c = match theme {
        Theme::Light => [60u8, 50, 35],
        Theme::Dark => [0, 0, 0],
    };
    let strength = match theme {
        Theme::Light => 0.28,
        Theme::Dark => 0.55,
    };
    let _ = radius;
    for (i, a) in alpha.iter().enumerate() {
        let a = (*a as f32 * strength) as u8;
        let px = &mut pm.data_mut()[i * 4..i * 4 + 4];
        px[0] = (c[0] as u16 * a as u16 / 255) as u8;
        px[1] = (c[1] as u16 * a as u16 / 255) as u8;
        px[2] = (c[2] as u16 * a as u16 / 255) as u8;
        px[3] = a;
    }
    pm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landscape_layers_cover_the_scroll_range() {
        for pages in [1, 2, 3] {
            let layers = landscape(Theme::Light, 320, 200, pages, 7);
            assert!(!layers.is_empty());
            for l in &layers {
                assert_eq!(l.pixmap.height(), 200);
                let need = 320 + (l.parallax * (pages as f32 - 1.0) * 320.0).ceil() as u32;
                assert!(l.pixmap.width() >= need, "宽 {} < {need}", l.pixmap.width());
            }
            // 第一层必须不透明（壁纸底）。
            assert!(layers[0].pixmap.pixels().iter().all(|p| p.alpha() == 255));
        }
    }

    #[test]
    fn icon_is_transparent_outside_the_tile() {
        let svg = crate::icon::builtin_symbol("compass").unwrap();
        for theme in [Theme::Light, Theme::Dark] {
            let pm = app_icon(
                96,
                &IconSpec {
                    symbol_svg: svg,
                    pigment: crate::theme::pigment::SHIQING,
                    theme,
                    seed: 1,
                },
            );
            assert_eq!((pm.width(), pm.height()), (96, 96));
            assert_eq!(pm.pixel(0, 0).unwrap().alpha(), 0);
            assert!(pm.pixel(48, 10).unwrap().alpha() > 200);
        }
    }

    #[test]
    fn generation_is_deterministic() {
        let a = landscape(Theme::Dark, 200, 120, 2, 42);
        let b = landscape(Theme::Dark, 200, 120, 2, 42);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.pixmap.data(), y.pixmap.data());
        }
    }
}
