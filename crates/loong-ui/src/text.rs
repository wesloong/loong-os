//! 文字绘制。拉丁字母用 Inter，中文自动回退到 Noto Sans CJK。

use cosmic_text::{
    Attrs, Buffer, Color as TextColor, Family, FontSystem, Metrics, Shaping, SwashCache, Weight,
};
use tiny_skia::{Color, Pixmap};

/// 文字样式（单位：物理像素）。
#[derive(Debug, Clone, Copy)]
pub struct TextStyle {
    /// 字号。
    pub size: f32,
    /// 字重（400 常规，500 中等，600 半粗，700 粗）。
    pub weight: u16,
    /// 颜色。
    pub color: Color,
    /// 是否加一层柔和阴影（壁纸上的文字需要，保证可读性）。
    pub shadow: bool,
}

/// 文字排版器。创建时加载系统字体，开销较大，应长期复用。
pub struct TextRenderer {
    fonts: FontSystem,
    cache: SwashCache,
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextRenderer {
    /// 加载系统字体。
    pub fn new() -> Self {
        TextRenderer {
            fonts: FontSystem::new(),
            cache: SwashCache::new(),
        }
    }

    /// 测量单行文字宽高（物理像素）。
    pub fn measure(&mut self, text: &str, style: &TextStyle) -> (f32, f32) {
        let buffer = self.layout(text, style);
        let width = buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
        (width, style.size * 1.3)
    }

    fn layout(&mut self, text: &str, style: &TextStyle) -> Buffer {
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(style.size, style.size * 1.3));
        let mut attrs = Attrs::new();
        attrs.family = Family::Name(crate::theme::font::FAMILY);
        attrs.weight = Weight(style.weight);
        buffer.set_size(None, None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        buffer
    }

    /// 把单行文字绘制成刚好包住它的位图。
    pub fn render(&mut self, text: &str, style: &TextStyle) -> Pixmap {
        let pad = if style.shadow {
            (style.size * 0.25).ceil() as i32
        } else {
            1
        };
        let (w, h) = self.measure(text, style);
        let mut pixmap = crate::pixmap(
            w.ceil() as u32 + 2 * pad as u32,
            h.ceil() as u32 + 2 * pad as u32,
        );
        let mut buffer = self.layout(text, style);
        let c = style.color;
        let color = TextColor::rgba(
            (c.red() * 255.0) as u8,
            (c.green() * 255.0) as u8,
            (c.blue() * 255.0) as u8,
            (c.alpha() * 255.0) as u8,
        );
        let mut coverage = vec![0u8; (pixmap.width() * pixmap.height()) as usize];
        let pw = pixmap.width() as i32;
        let ph = pixmap.height() as i32;
        buffer.draw(
            &mut self.fonts,
            &mut self.cache,
            color,
            |x, y, gw, gh, gc| {
                for dy in 0..gh as i32 {
                    for dx in 0..gw as i32 {
                        let (px, py) = (x + dx + pad, y + dy + pad);
                        if px >= 0 && py >= 0 && px < pw && py < ph {
                            let i = (py * pw + px) as usize;
                            coverage[i] = coverage[i].max(gc.a());
                        }
                    }
                }
            },
        );
        if style.shadow {
            let mut shadow = coverage.clone();
            crate::material::blur_alpha(
                &mut shadow,
                pw as usize,
                ph as usize,
                (style.size * 0.12).max(1.0) as usize,
            );
            paint_coverage(&mut pixmap, &shadow, Color::from_rgba8(0, 0, 0, 90));
        }
        paint_coverage(&mut pixmap, &coverage, c);
        pixmap
    }
}

/// 按覆盖度把颜色叠加到位图上（source-over，预乘 alpha）。
fn paint_coverage(pixmap: &mut Pixmap, coverage: &[u8], color: Color) {
    let data = pixmap.data_mut();
    for (i, &cov) in coverage.iter().enumerate() {
        if cov == 0 {
            continue;
        }
        let a = color.alpha() * cov as f32 / 255.0;
        let src = [color.red() * a, color.green() * a, color.blue() * a, a];
        let px = &mut data[i * 4..i * 4 + 4];
        for k in 0..4 {
            let dst = px[k] as f32 / 255.0;
            px[k] = ((src[k] + dst * (1.0 - a)) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
}
