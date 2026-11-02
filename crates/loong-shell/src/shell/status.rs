//! 自适应状态栏：前台应用之上的状态栏用哪种墨（浓墨 / 月白），以及两种墨之间的交叉淡化。
//!
//! 状态栏画在全屏应用的顶部 24pt 上。只跟随系统外观时，应用内容与系统外观不一致（系统
//! 夜墨、网页是亮的，或者反过来）就看不清。前台应用的墨按以下顺序决定（协议见
//! `protocols/loong-app-v1.xml` 与 `docs/api/loong-app-protocol.md`）：
//!
//! 1. 应用用 `loong_app_surface_v1.set_status_bar_style` 指定的样式（不是 `auto` 时）；
//! 2. 应用给出的主题色（`set_theme_color`）：取 WCAG 对比度更高的那种墨；
//! 3. 取样：读状态栏下方的像素（只读共享内存与单像素缓冲，见 `status_sample.rs`）；
//! 4. 系统外观。
//!
//! 锁屏、主屏、多任务始终跟随系统外观；打开 / 关闭应用的过程中，卡片盖住状态栏区域的
//! 大半、且内容已经显现时才换成应用的墨（见 `Shell::status_target`）。
//!
//! 两种墨的状态栏位图都预先画好（`render::Assets`），换墨时用 `SMOOTH` 弹簧交叉淡化。

use loong_motion::{Spring, SpringAnimation};
use loong_ui::theme::{self, Theme};

use super::geometry::RectF;

/// 状态栏的墨。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusInk {
    /// 浓墨（深色字形），用于亮的背景。对应协议的 `dark_content`。
    Dark,
    /// 月白（浅色字形），用于暗的背景。对应协议的 `light_content`。
    Light,
}

impl StatusInk {
    /// 系统外观下的墨：宣纸上用浓墨，夜墨上用月白。
    pub fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Light => StatusInk::Dark,
            Theme::Dark => StatusInk::Light,
        }
    }

    /// 画这种墨用哪套调色板：状态栏的字与符号都取该调色板的「浓墨」级
    /// （宣纸的浓墨 `#2A2824`，夜墨的浓墨即月白 `#CFC8B8`）。
    pub fn palette_theme(self) -> Theme {
        match self {
            StatusInk::Dark => Theme::Light,
            StatusInk::Light => Theme::Dark,
        }
    }

    /// 字形颜色（0xRRGGBB）。
    pub fn rgb(self) -> u32 {
        self.palette_theme().palette().ink[1]
    }

    /// 交叉淡化的数值：浓墨 0，月白 1。
    fn value(self) -> f64 {
        match self {
            StatusInk::Dark => 0.0,
            StatusInk::Light => 1.0,
        }
    }

    fn name(self) -> &'static str {
        match self {
            StatusInk::Dark => "浓墨",
            StatusInk::Light => "月白",
        }
    }
}

/// 应用指定的状态栏样式（协议的 `status_bar_style`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusStyle {
    /// 由系统决定（主题色、取样、系统外观）。
    #[default]
    Auto,
    /// 深色字形（浓墨）。
    DarkContent,
    /// 浅色字形（月白）。
    LightContent,
}

/// 应用通过协议给出的线索。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppHints {
    pub style: StatusStyle,
    /// 顶部区域的颜色 0xAARRGGBB（非预乘，alpha 不为 0）。
    pub theme_color: Option<u32>,
}

/// 这一帧的墨是根据什么定的（只用于日志）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// 锁屏、主屏、多任务，或者前台应用没有任何线索。
    System,
    /// 应用指定的样式。
    Style,
    /// 应用给出的主题色（0xRRGGBB，已合成到纸色上）。
    ThemeColor(u32),
    /// 取样。
    Sample,
}

/// 两种墨中，与颜色 `rgb`（0xRRGGBB）的 WCAG 对比度更高的那种。相等时用浓墨。
pub fn ink_for_color(rgb: u32) -> StatusInk {
    let dark = theme::contrast_ratio(rgb, StatusInk::Dark.rgb());
    let light = theme::contrast_ratio(rgb, StatusInk::Light.rgb());
    if dark >= light {
        StatusInk::Dark
    } else {
        StatusInk::Light
    }
}

/// 把非预乘的 0xAARRGGBB 合成到不透明的纸色 `paper`（0xRRGGBB）上，返回 0xRRGGBB。
pub fn flatten(argb: u32, paper: u32) -> u32 {
    let a = (argb >> 24) & 0xff;
    let mix = |shift: u32| {
        let c = (argb >> shift) & 0xff;
        let p = (paper >> shift) & 0xff;
        (c * a + p * (255 - a) + 127) / 255
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

/// 取样的回差：状态栏下方“亮”的点（浓墨对比度更高）超过一半就用浓墨，但已经是某种墨时，
/// 要越过一半再多 [`SAMPLE_HYSTERESIS`] 才换，避免内容在临界处时来回跳。
pub const SAMPLE_HYSTERESIS: f32 = 0.1;

/// 取样结果（亮点占比 `light_share`，0..1）→ 墨。`previous` 为这个表面上一次取样的结论。
pub fn ink_for_share(light_share: f32, previous: Option<StatusInk>) -> StatusInk {
    let threshold = match previous {
        None => 0.5,
        Some(StatusInk::Dark) => 0.5 - SAMPLE_HYSTERESIS,
        Some(StatusInk::Light) => 0.5 + SAMPLE_HYSTERESIS,
    };
    if light_share >= threshold {
        StatusInk::Dark
    } else {
        StatusInk::Light
    }
}

/// 前台应用的墨：显式样式 > 主题色 > 取样 > 系统外观。
///
/// `paper` 为应用窗口下面的纸色（半透明的主题色合成到它上面）；`sample` 只在需要时调用
/// （取样要读缓冲，前两步已经能决定时不必做）。
pub fn choose(
    hints: &AppHints,
    paper: u32,
    sample: impl FnOnce() -> Option<StatusInk>,
    system: StatusInk,
) -> (StatusInk, Basis) {
    match hints.style {
        StatusStyle::DarkContent => return (StatusInk::Dark, Basis::Style),
        StatusStyle::LightContent => return (StatusInk::Light, Basis::Style),
        StatusStyle::Auto => {}
    }
    if let Some(argb) = hints.theme_color {
        let rgb = flatten(argb, paper);
        return (ink_for_color(rgb), Basis::ThemeColor(rgb));
    }
    match sample() {
        Some(ink) => (ink, Basis::Sample),
        None => (system, Basis::System),
    }
}

/// 卡片（屏幕矩形）盖住了状态栏区域（宽 `w`、高 `h`，贴屏幕顶边）的多大比例。
pub fn strip_coverage(card: RectF, w: f64, h: f64) -> f64 {
    let x0 = card.x.max(0.0);
    let x1 = (card.x + card.w).min(w);
    let y0 = card.y.max(0.0);
    let y1 = (card.y + card.h).min(h);
    if x1 <= x0 || y1 <= y0 || w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    (x1 - x0) * (y1 - y0) / (w * h)
}

/// 状态栏的墨与交叉淡化。
#[derive(Debug, Default)]
pub struct StatusBar {
    /// 浓墨 0 … 月白 1。第一次 [`StatusBar::update`] 之前为 `None`（直接用目标墨，不淡化）。
    mix: Option<SpringAnimation<f64>>,
    basis: Option<Basis>,
}

impl StatusBar {
    /// 每帧调用：目标墨变了就以 `SMOOTH` 弹簧淡过去（保持速度连续）。第一次调用时直接就位。
    pub fn update(&mut self, (ink, basis): (StatusInk, Basis), now: f64) {
        let v = ink.value();
        match &mut self.mix {
            None => self.mix = Some(SpringAnimation::at_rest(Spring::SMOOTH, v, now)),
            Some(mix) if mix.target() != v => mix.retarget(v, now),
            Some(_) => {}
        }
        if self.basis != Some(basis) {
            match basis {
                Basis::System => tracing::debug!("状态栏：{}（系统外观）", ink.name()),
                Basis::Style => tracing::info!("状态栏：{}（应用指定的样式）", ink.name()),
                Basis::ThemeColor(rgb) => {
                    tracing::info!("状态栏：{}（应用主题色 #{rgb:06x}）", ink.name())
                }
                Basis::Sample => tracing::info!("状态栏：{}（取样）", ink.name()),
            }
            self.basis = Some(basis);
        }
    }

    /// 当前的目标墨（还没有更新过时为 `None`）。
    #[allow(dead_code, reason = "测试使用")]
    pub fn target(&self) -> Option<StatusInk> {
        self.mix.as_ref().map(|m| {
            if m.target() >= 0.5 {
                StatusInk::Light
            } else {
                StatusInk::Dark
            }
        })
    }

    /// `now` 时刻的混合值（浓墨 0 … 月白 1）。
    pub fn mix(&self, now: f64) -> Option<f64> {
        self.mix.as_ref().map(|m| m.value(now).clamp(0.0, 1.0))
    }

    pub fn is_idle(&self, now: f64) -> bool {
        self.mix.as_ref().is_none_or(|m| m.is_done(now))
    }
}

/// 交叉淡化的画法：占多数的墨在下层、不透明；另一种墨叠在上面，不透明度为它的份额。
/// 两种墨的字形完全重合，这样任何时刻字形都是实的（不会在中途变淡），停稳后只剩一种墨。
/// 返回（下层墨, 上层墨, 上层不透明度）。
pub fn crossfade_layers(mix: f64) -> (StatusInk, StatusInk, f64) {
    let mix = mix.clamp(0.0, 1.0);
    if mix < 0.5 {
        (StatusInk::Dark, StatusInk::Light, mix)
    } else {
        (StatusInk::Light, StatusInk::Dark, 1.0 - mix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XUAN: u32 = 0xF3EEE2;
    const YEMO: u32 = 0x15140F;

    #[test]
    fn inks_are_the_nong_level_of_each_palette() {
        assert_eq!(StatusInk::Dark.rgb(), 0x2A2824);
        assert_eq!(StatusInk::Light.rgb(), 0xCFC8B8);
        assert_eq!(StatusInk::for_theme(Theme::Light), StatusInk::Dark);
        assert_eq!(StatusInk::for_theme(Theme::Dark), StatusInk::Light);
    }

    #[test]
    fn theme_color_picks_the_ink_with_more_contrast() {
        // 纸色：宣纸用浓墨、夜墨用月白（与系统外观一致）。
        assert_eq!(ink_for_color(XUAN), StatusInk::Dark);
        assert_eq!(ink_for_color(YEMO), StatusInk::Light);
        assert_eq!(ink_for_color(0xFFFFFF), StatusInk::Dark);
        assert_eq!(ink_for_color(0x000000), StatusInk::Light);
        // 颜料：石青（暗）用月白，藤黄（亮）用浓墨。
        assert_eq!(ink_for_color(0x2E5E74), StatusInk::Light);
        assert_eq!(ink_for_color(0xD8A21E), StatusInk::Dark);
        // 朱砂：月白 3.4 : 1，浓墨 2.6 : 1。
        assert_eq!(ink_for_color(0xB83A2B), StatusInk::Light);
        // 选中的墨对比度确实不低于另一种。
        for rgb in [0x3F7D68, 0x3B5878, 0x808080, 0x9D2B3C, 0xC8A45A] {
            let chosen = theme::contrast_ratio(rgb, ink_for_color(rgb).rgb());
            for ink in [StatusInk::Dark, StatusInk::Light] {
                assert!(chosen >= theme::contrast_ratio(rgb, ink.rgb()), "{rgb:06x}");
            }
        }
    }

    #[test]
    fn translucent_theme_color_is_composited_over_paper() {
        assert_eq!(flatten(0xFF2E5E74, XUAN), 0x2E5E74);
        assert_eq!(flatten(0x00000000, XUAN), XUAN);
        assert_eq!(flatten(0x80000000, 0xFFFFFF), 0x7F7F7F);
        // 半透明的黑在宣纸上仍是亮的，在夜墨上是暗的。
        assert_eq!(ink_for_color(flatten(0x20000000, XUAN)), StatusInk::Dark);
        assert_eq!(ink_for_color(flatten(0x20000000, YEMO)), StatusInk::Light);
    }

    #[test]
    fn decision_order() {
        let system = StatusInk::Light;
        let no_sample = || None;
        let sampled_dark = || Some(StatusInk::Dark);
        let auto = AppHints::default();
        // 没有任何线索：系统外观。
        assert_eq!(
            choose(&auto, YEMO, no_sample, system),
            (StatusInk::Light, Basis::System)
        );
        // 只有取样。
        assert_eq!(
            choose(&auto, YEMO, sampled_dark, system),
            (StatusInk::Dark, Basis::Sample)
        );
        // 主题色优先于取样（取样不会被调用）。
        let themed = AppHints {
            theme_color: Some(0xFF15140F),
            ..AppHints::default()
        };
        assert_eq!(
            choose(&themed, XUAN, || panic!("不应取样"), StatusInk::Dark),
            (StatusInk::Light, Basis::ThemeColor(YEMO))
        );
        // 显式样式优先于主题色。
        for (style, ink) in [
            (StatusStyle::DarkContent, StatusInk::Dark),
            (StatusStyle::LightContent, StatusInk::Light),
        ] {
            let hints = AppHints { style, ..themed };
            assert_eq!(
                choose(&hints, XUAN, || panic!("不应取样"), system),
                (ink, Basis::Style)
            );
        }
    }

    #[test]
    fn sampling_has_hysteresis() {
        assert_eq!(ink_for_share(0.55, None), StatusInk::Dark);
        assert_eq!(ink_for_share(0.45, None), StatusInk::Light);
        // 已经是月白：亮点要超过 60% 才换成浓墨。
        assert_eq!(
            ink_for_share(0.55, Some(StatusInk::Light)),
            StatusInk::Light
        );
        assert_eq!(ink_for_share(0.65, Some(StatusInk::Light)), StatusInk::Dark);
        // 已经是浓墨：亮点少于 40% 才换成月白。
        assert_eq!(ink_for_share(0.45, Some(StatusInk::Dark)), StatusInk::Dark);
        assert_eq!(ink_for_share(0.35, Some(StatusInk::Dark)), StatusInk::Light);
        assert_eq!(ink_for_share(1.0, Some(StatusInk::Light)), StatusInk::Dark);
        assert_eq!(ink_for_share(0.0, Some(StatusInk::Dark)), StatusInk::Light);
    }

    #[test]
    fn coverage_of_the_status_bar_strip() {
        let (w, h) = (1280.0, 24.0);
        assert_eq!(
            strip_coverage(RectF::new(0.0, 0.0, 1280.0, 800.0), w, h),
            1.0
        );
        // 卡片缩小、离开顶边：不再盖住状态栏。
        assert_eq!(
            strip_coverage(RectF::new(100.0, 40.0, 1000.0, 600.0), w, h),
            0.0
        );
        let half = strip_coverage(RectF::new(640.0, 0.0, 1280.0, 800.0), w, h);
        assert!((half - 0.5).abs() < 1e-9);
        let partial = strip_coverage(RectF::new(0.0, 12.0, 1280.0, 800.0), w, h);
        assert!((partial - 0.5).abs() < 1e-9);
    }

    #[test]
    fn crossfade_is_continuous_and_never_fades_out() {
        // 停稳时只有一种墨。
        assert_eq!(
            crossfade_layers(0.0),
            (StatusInk::Dark, StatusInk::Light, 0.0)
        );
        assert_eq!(
            crossfade_layers(1.0),
            (StatusInk::Light, StatusInk::Dark, 0.0)
        );
        // 实心像素的颜色 = 下层 × (1 − a) + 上层 × a，在 0.5 两侧连续。
        let light_weight = |m: f64| {
            let (bottom, _, a) = crossfade_layers(m);
            match bottom {
                StatusInk::Dark => a,
                StatusInk::Light => 1.0 - a,
            }
        };
        for m in [0.0, 0.2, 0.49999, 0.5, 0.50001, 0.8, 1.0] {
            assert!((light_weight(m) - m).abs() < 1e-9, "{m}");
        }
    }

    #[test]
    fn status_bar_snaps_first_then_crossfades() {
        let mut bar = StatusBar::default();
        assert_eq!(bar.mix(0.0), None);
        bar.update((StatusInk::Light, Basis::System), 0.0);
        // 第一次直接就位，不从浓墨淡过来。
        assert_eq!(bar.mix(0.0), Some(1.0));
        assert!(bar.is_idle(0.0));
        bar.update((StatusInk::Dark, Basis::Sample), 1.0);
        assert_eq!(bar.target(), Some(StatusInk::Dark));
        assert!(!bar.is_idle(1.0));
        // SMOOTH：连续地从月白淡到浓墨，约 0.75 秒停稳。
        let mut last = 1.0;
        for i in 1..=100 {
            let m = bar.mix(1.0 + i as f64 * 0.01).unwrap();
            assert!(m <= last + 1e-9, "单调（临界阻尼不回弹）");
            last = m;
        }
        assert!(bar.mix(1.2).unwrap() > 0.05 && bar.mix(1.2).unwrap() < 0.95);
        assert!(bar.is_idle(2.0));
        assert_eq!(bar.mix(2.0), Some(0.0));
    }
}
