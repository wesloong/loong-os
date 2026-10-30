//! 设计令牌。完整规范见 `docs/design/设计规范.md`。

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
    /// 系统界面拉丁字体。
    pub const FAMILY: &str = "Inter";
    /// 状态栏时间。
    pub const STATUS: f32 = 14.0;
    /// 图标名称。
    pub const ICON_LABEL: f32 = 12.0;
    /// 大标题。
    pub const LARGE_TITLE: f32 = 34.0;
    /// 正文。
    pub const BODY: f32 = 17.0;
}
