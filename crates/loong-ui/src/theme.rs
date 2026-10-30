//! 设计令牌（「墨韵」水墨设计语言）。完整规范见 `docs/design/设计规范.md`。

use tiny_skia::Color;

/// 由 0xRRGGBB 与不透明度构造颜色。
pub fn hex(rgb: u32, alpha: f32) -> Color {
    let r = ((rgb >> 16) & 0xff) as u8;
    let g = ((rgb >> 8) & 0xff) as u8;
    let b = (rgb & 0xff) as u8;
    Color::from_rgba8(r, g, b, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// 品牌主色“Loong 青”。
pub const ACCENT: u32 = 0x14B8A6;

/// 主屏文字（图标名称、状态栏）：白色，叠加在壁纸上。
pub fn label_on_wallpaper() -> Color {
    hex(0xFFFFFF, 0.95)
}

/// 圆角（逻辑点）。
pub mod radius {
    /// 应用图标圆角占边长的比例。
    pub const ICON_RATIO: f32 = 0.225;
    /// 屏幕圆角（平板默认，实际以 device.toml 为准）。
    pub const SCREEN: f32 = 18.0;
    /// Dock。
    pub const DOCK: f32 = 30.0;
    /// 卡片 / 面板。
    pub const CARD: f32 = 16.0;
}

/// 字号（逻辑点）。
pub mod font {
    /// 正文字体（拉丁）；中文自动回退到思源黑体。
    pub const FAMILY: &str = "Inter";
    /// 展示字体：标题、锁屏时间、应用名（霞鹜文楷屏幕版；缺失时回退到正文字体）。
    pub const DISPLAY_FAMILY: &str = "LXGW WenKai Screen";
    /// 状态栏时间。
    pub const STATUS: f32 = 14.0;
    /// 图标名称。
    pub const ICON_LABEL: f32 = 12.0;
    /// 大标题。
    pub const LARGE_TITLE: f32 = 34.0;
    /// 正文。
    pub const BODY: f32 = 17.0;
}

/// 外观：亮色「宣纸」/ 暗色「夜墨」。默认亮色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Theme {
    #[default]
    Light,
    Dark,
}

impl Theme {
    /// 按名称解析（`light` / `dark`，大小写不敏感）。
    pub fn from_name(name: &str) -> Option<Theme> {
        match name.trim().to_ascii_lowercase().as_str() {
            "light" | "亮色" => Some(Theme::Light),
            "dark" | "暗色" => Some(Theme::Dark),
            _ => None,
        }
    }

    /// 这套外观的调色板。
    pub fn palette(self) -> Palette {
        match self {
            Theme::Light => Palette {
                paper: 0xF3EEE2,
                paper_raised: 0xEAE3D3,
                ink: [0x141310, 0x2A2824, 0x4A4741, 0x7D7970, 0xB4AEA2],
                accent: pigment::ZHUSHA,
            },
            Theme::Dark => Palette {
                paper: 0x15140F,
                paper_raised: 0x1F1D17,
                ink: [0xE6E0D2, 0xCFC8B8, 0xA39D90, 0x7A756A, 0x5A564C],
                accent: pigment::ZHUSHA,
            },
        }
    }
}

/// 墨分五色：从最浓到最淡。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// 焦墨：最重要的文字与笔触。
    Jiao,
    /// 浓墨：正文。
    Nong,
    /// 重墨：次要文字。
    Zhong,
    /// 淡墨：说明文字、远山。
    Dan,
    /// 清墨：分隔、最远的层次。
    Qing,
}

/// 调色板（0xRRGGBB）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// 纸（背景）。
    pub paper: u32,
    /// 抬起的纸面（卡片、面板）。
    pub paper_raised: u32,
    /// 墨五色，顺序同 [`Ink`]。
    pub ink: [u32; 5],
    /// 强调色（朱砂）：印章、角标、警示。
    pub accent: u32,
}

impl Palette {
    /// 某一级墨色。
    pub fn ink(&self, level: Ink) -> Color {
        hex(self.ink[level as usize], 1.0)
    }
}

/// WCAG 2.x 相对亮度（0xRRGGBB）。
pub fn relative_luminance(rgb: u32) -> f32 {
    let lin = |c: u32| {
        let v = (c & 0xff) as f32 / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(rgb >> 16) + 0.7152 * lin(rgb >> 8) + 0.0722 * lin(rgb)
}

/// WCAG 对比度（1..21）。正文要求 ≥ 4.5，大字 ≥ 3。
pub fn contrast_ratio(a: u32, b: u32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// 中国传统颜料（0xRRGGBB）。
pub mod pigment {
    /// 朱砂：品牌与重要提示。
    pub const ZHUSHA: u32 = 0xB83A2B;
    /// 胭脂。
    pub const YANZHI: u32 = 0x9D2B3C;
    /// 藤黄。
    pub const TENGHUANG: u32 = 0xD8A21E;
    /// 赭石。
    pub const ZHESHI: u32 = 0xA0623A;
    /// 石青。
    pub const SHIQING: u32 = 0x2E5E74;
    /// 石绿。
    pub const SHILV: u32 = 0x3F7D68;
    /// 花青。
    pub const HUAQING: u32 = 0x3B5878;
    /// 泥金（暗色外观的点缀）。
    pub const NIJIN: u32 = 0xC8A45A;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_ratio_matches_known_values() {
        assert!((contrast_ratio(0x000000, 0xFFFFFF) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(0x777777, 0xFFFFFF) - 4.48).abs() < 0.02);
        assert_eq!(contrast_ratio(0x123456, 0x123456), 1.0);
    }

    #[test]
    fn body_text_is_readable_on_paper_in_both_themes() {
        for theme in [Theme::Light, Theme::Dark] {
            let p = theme.palette();
            for paper in [p.paper, p.paper_raised] {
                // 焦墨、浓墨、重墨可用于正文与次要文字。
                for level in [Ink::Jiao, Ink::Nong, Ink::Zhong] {
                    let c = contrast_ratio(p.ink[level as usize], paper);
                    assert!(c >= 4.5, "{theme:?} {level:?} 对比度 {c:.2}");
                }
                // 淡墨用于大字/说明，至少 3:1。
                let c = contrast_ratio(p.ink[Ink::Dan as usize], paper);
                assert!(c >= 3.0, "{theme:?} 淡墨对比度 {c:.2}");
            }
        }
    }
}
