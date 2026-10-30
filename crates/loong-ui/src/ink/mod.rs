//! # 水墨绘画工具（「墨韵」设计语言）
//!
//! 在 CPU 上把系统界面的静态元素“画”出来：宣纸、毛笔笔触、山水长卷、宣纸小笺图标、
//! 印章、状态栏控件。全部程序生成（不占资源体积），只在开机或切换外观时画一次，
//! 之后由合成器在 GPU 上做动画。
//!
//! 本文件定义对外接口（合成器依赖它）；各函数的实现位于子模块：
//! - [`noise`]：确定性噪声与伪随机数；
//! - [`raster`]：浮点灰度场、模糊、重采样与混合；
//! - [`paper`]：宣纸/绢的纹理与毛边；
//! - [`brush`]：毛笔笔触（起收笔、飞白、积墨、晕开）；
//! - `landscape`：山水长卷；`icon`：宣纸小笺与册页；`seal`：印章；`status`：电池与 Wi‑Fi；
//!   `panel`：绢面板与墨影。
//!
//! 约定：所有尺寸为物理像素；颜色为 0xRRGGBB；同样的参数与 `seed` 必须得到完全相同的结果。

use tiny_skia::{Color, Pixmap, Rect};

use crate::theme::Theme;

pub mod brush;
mod icon;
mod landscape;
pub mod noise;
mod panel;
pub mod paper;
pub mod raster;
mod seal;
mod status;

pub use icon::{pigment_on, stroke_ink, suggested_pigment};
pub use landscape::{PARALLAX, composite};

/// 山水长卷的一层。
pub struct Layer {
    /// 这一层的位图。宽度 = 屏幕宽 + ceil(parallax × (页数 − 1) × 屏幕宽)，高度 = 屏幕高。
    pub pixmap: Pixmap,
    /// 视差系数：主屏横向滚动 `s` 像素时，这一层移动 `parallax × s` 像素（0 = 不动）。
    pub parallax: f32,
}

/// 壁纸「山水长卷」：从远到近的若干层（第一层为天色/纸底，不透明；其余带透明度）。
///
/// `pages` 为主屏页数（≥ 1），用于决定每层的宽度。目前为五层：
/// 天（含日/月，视差 0.04）、远山（0.15）、中山（0.35）、近山（0.6）、烟云（0.8），
/// 见 [`PARALLAX`]。各层内部并行生成。
pub fn landscape(theme: Theme, width: u32, height: u32, pages: usize, seed: u32) -> Vec<Layer> {
    landscape::landscape(theme, width, height, pages, seed)
}

/// 宣纸纹理（不透明，底色为“抬起的纸面” `paper_raised`）。
pub fn paper(theme: Theme, width: u32, height: u32, seed: u32) -> Pixmap {
    paper::paper_with(theme, width, height, theme.palette().paper_raised, seed)
}

/// 应用图标的描述。
#[derive(Debug, Clone, Copy)]
pub struct IconSpec<'a> {
    /// 符号的 SVG（线条图标，如 [`crate::icon::builtin_symbol`] 返回的 Lucide 图标）。
    pub symbol_svg: &'a str,
    /// 淡彩点缀颜料（0xRRGGBB，见 [`crate::theme::pigment`]；可用 [`suggested_pigment`]）。
    pub pigment: u32,
    /// 外观。
    pub theme: Theme,
    /// 随机种子（每个应用不同，让纸纹和笔触各不相同）。
    pub seed: u32,
}

/// 「宣纸小笺」应用图标：连续圆角宣纸底板 + 淡彩色晕 + 毛笔笔触符号 + 毛边。
/// 返回 `size × size` 的位图（底板外透明；底板四周约留 2% 给毛边）。
/// 墨影不画在图标里（图标外必须透明），由合成器在图标下方叠 [`ink_shadow`]。
pub fn app_icon(size: u32, spec: &IconSpec<'_>) -> Pixmap {
    icon::app_icon(size, spec)
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
    icon::brush_symbol(target, svg, rect, ink, weight, seed)
}

/// 「册页」：演示应用的全屏占位画面（纸 + 大片淡彩 + 大幅笔触符号 + 远山 + 小印）。
/// 顶部居中约 22% 高度留白，供合成器画标题。
pub fn app_sheet(
    theme: Theme,
    width: u32,
    height: u32,
    symbol_svg: &str,
    pigment: u32,
    seed: u32,
) -> Pixmap {
    icon::app_sheet(theme, width, height, symbol_svg, pigment, seed)
}

/// 印章：`size × size`，朱砂印泥，`text` 为阴刻留白的字（1–4 个汉字）。印面外与字口透明。
pub fn seal(theme: Theme, size: u32, text: &str, seed: u32) -> Pixmap {
    seal::seal(theme, size, text, seed)
}

/// 状态栏电池：毛笔画的电池外框 + 按电量填墨。`level` 为 0..1，`charging` 时加闪电。
/// 返回 `round(2·height) × height`。
pub fn battery(theme: Theme, height: u32, level: f32, charging: bool) -> Pixmap {
    status::battery(theme, height, level, charging)
}

/// 状态栏 Wi‑Fi：毛笔画的一点三弧，`strength` 为 0..=3 格。返回 `round(1.2·height) × height`。
pub fn wifi(theme: Theme, height: u32, strength: u8) -> Pixmap {
    status::wifi(theme, height, strength)
}

/// 「绢」面板：取 `background` 中 `rect` 区域，模糊 + 暖色调 + 绢纹 + 柔和毛边。
pub fn silk_panel(theme: Theme, background: &Pixmap, rect: Rect, radius: f32) -> Pixmap {
    panel::silk_panel(theme, background, rect, radius)
}

/// 「墨影」：`width × height` 的卡片在纸上投下的柔和阴影（含外扩边距 `spread`，返回位图更大；
/// 卡片位于 `(spread, spread)`，影子略向下偏）。
pub fn ink_shadow(theme: Theme, width: u32, height: u32, radius: f32, spread: u32) -> Pixmap {
    panel::ink_shadow(theme, width, height, radius, spread)
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

    #[test]
    fn landscape_layers_are_exactly_sized_and_ordered() {
        let layers = landscape(Theme::Light, 400, 250, 2, 3);
        assert_eq!(layers.len(), PARALLAX.len());
        for (l, &p) in layers.iter().zip(PARALLAX.iter()) {
            assert_eq!(l.parallax, p);
            assert_eq!(l.pixmap.width(), 400 + (p * 400.0).ceil() as u32);
        }
        // 视差由远及近递增；近山有墨，烟云层大部分透明。
        assert!(PARALLAX.windows(2).all(|w| w[0] < w[1]));
        let near = &layers[3].pixmap;
        assert!(near.pixels().iter().any(|p| p.alpha() > 200));
        let mist = &layers[4].pixmap;
        let clear = mist.pixels().iter().filter(|p| p.alpha() < 8).count();
        assert!(clear * 2 > mist.pixels().len());
    }

    #[test]
    fn icons_seals_and_sheets_are_deterministic() {
        let svg = crate::icon::builtin_symbol("music").unwrap();
        let spec = IconSpec {
            symbol_svg: svg,
            pigment: crate::theme::pigment::YANZHI,
            theme: Theme::Light,
            seed: 5,
        };
        assert_eq!(app_icon(64, &spec).data(), app_icon(64, &spec).data());
        let other = IconSpec { seed: 6, ..spec };
        assert_ne!(app_icon(64, &spec).data(), app_icon(64, &other).data());
        assert_eq!(
            seal(Theme::Light, 48, "墨韵", 1).data(),
            seal(Theme::Light, 48, "墨韵", 1).data()
        );
        let a = app_sheet(
            Theme::Dark,
            160,
            120,
            svg,
            crate::theme::pigment::SHIQING,
            2,
        );
        let b = app_sheet(
            Theme::Dark,
            160,
            120,
            svg,
            crate::theme::pigment::SHIQING,
            2,
        );
        assert_eq!(a.data(), b.data());
        assert!(a.pixels().iter().all(|p| p.alpha() == 255));
    }

    #[test]
    fn status_icons_have_ink_and_expected_sizes() {
        for theme in [Theme::Light, Theme::Dark] {
            let b = battery(theme, 36, 0.6, false);
            assert_eq!((b.width(), b.height()), (72, 36));
            assert!(b.pixels().iter().any(|p| p.alpha() > 200));
            let w = wifi(theme, 36, 2);
            assert_eq!(w.height(), 36);
            assert!(w.pixels().iter().any(|p| p.alpha() > 200));
            let charging = battery(theme, 36, 0.6, true);
            assert_ne!(b.data(), charging.data());
        }
    }

    #[test]
    fn seal_is_mostly_vermilion_with_carved_gaps() {
        let pm = seal(Theme::Light, 96, "龙", 3);
        let px = pm.pixels();
        let solid = px.iter().filter(|p| p.alpha() > 200).count();
        let clear = px.iter().filter(|p| p.alpha() < 30).count();
        assert!(solid > px.len() / 3, "印面太少: {solid}");
        assert!(clear > px.len() / 20, "没有刻出字: {clear}");
        let p = pm.pixel(48, 4).unwrap();
        assert!(p.red() > p.green() * 2 || p.alpha() < 30);
    }

    #[test]
    fn shadow_and_silk_panel_have_expected_shapes() {
        let s = ink_shadow(Theme::Light, 100, 60, 12.0, 20);
        assert_eq!((s.width(), s.height()), (140, 100));
        assert_eq!(s.pixel(0, 0).unwrap().alpha(), 0);
        assert!(s.pixel(70, 50).unwrap().alpha() > 40);
        let bg = landscape(Theme::Light, 300, 200, 1, 1);
        let bg = composite(&bg, 300, 200, 0.0);
        let p = silk_panel(
            Theme::Light,
            &bg,
            Rect::from_xywh(40.0, 120.0, 220.0, 60.0).unwrap(),
            20.0,
        );
        assert_eq!((p.width(), p.height()), (220, 60));
        assert_eq!(p.pixel(0, 0).unwrap().alpha(), 0);
        assert!(p.pixel(110, 30).unwrap().alpha() > 180);
    }
}
