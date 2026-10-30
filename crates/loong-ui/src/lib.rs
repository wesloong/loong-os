//! # loong-ui
//!
//! 系统界面元素的 CPU 绘制。采用 Core Animation 的思路：图标、文字、面板等
//! **只栅格化一次**成为位图（预乘 alpha 的 RGBA），之后由合成器在 GPU 上做
//! 位移/缩放/透明度动画，动画过程中不再重绘，省电。
//!
//! - [`theme`]：设计令牌（颜色、圆角、字号）。
//! - [`shape`]：连续圆角（超椭圆角）路径，所有圆角统一使用。
//! - [`text`]：文字排版与绘制（cosmic-text，支持中文）。
//! - [`icon`]：应用图标与符号（Lucide 线性图标，ISC 许可）。
//! - [`material`]：毛玻璃材质（模糊 + 着色）。
//! - [`wallpaper`]：程序生成的默认壁纸。

#![forbid(unsafe_code)]

pub mod icon;
pub mod material;
pub mod shape;
pub mod text;
pub mod theme;
pub mod wallpaper;

pub use tiny_skia;
pub use tiny_skia::{Color, Pixmap};

/// 新建透明位图。尺寸至少为 1×1。
pub fn pixmap(width: u32, height: u32) -> Pixmap {
    Pixmap::new(width.max(1), height.max(1)).expect("位图尺寸有效")
}
