//! 控制中心的位图（「墨韵」）：在 CPU 上画好，GPU 只做位移、淡入与裁剪
//! （拉出面板、拖动亮度滑块都不重画位图）。
//!
//! | 纹理（[`Part`]） | 内容 | 何时重画 |
//! |---|---|---|
//! | `Backdrop` | 打开时取样的整屏：模糊（σ 24pt）+ 纸色罩染，1/4 分辨率 | 每次从收起开始展开（`ControlCenter::epoch`）、外观变化 |
//! | `Panel` | `ink::silk_panel` 的绢（背后透明，由 GPU 叠在取样上）+ 四周墨影 | 外观变化 |
//! | `Tile(…)` | 素笺小笺 + 毛笔符号、文楷标题、说明 + 墨影；开启的开关着一团颜料（亮色石青、暗色泥金） | 模块内容（[`TileView`]）变化 |
//! | `Track` / `Ink` | 一笔淡墨的轨道；同一笔的浓墨（暗色泥金）着墨段，按亮度裁剪 | 外观变化 |
//! | `Knob` | 纸团 + 墨影 | 外观变化 |
//!
//! **取样**：场景里排在 `Backdrop` 之前的元素就是控制中心下面的画面。`Painter` 遇到它且序号
//! 变了时，把这些元素按 1/4 的输出缩放再生成一遍、画到离屏缓冲并读回（GLES 读回 1/16 的像素，
//! 很快），在 CPU 上模糊、罩染。应用画面之后的变化不再反映到背后（被模糊和罩染盖住，几乎
//! 看不出来），换来每帧零开销。

use std::collections::HashMap;

use anyhow::anyhow;
use loong_ui::{
    Pixmap,
    ink::{
        self,
        brush::{self, BrushStyle, Stroke},
        noise,
        raster::{blend_field, rgb, smoothstep},
    },
    text::{FontRole, TextStyle},
    theme::{self, Ink, Theme, pigment},
    tiny_skia::{Paint, Rect, Transform},
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ExportMem, Offscreen,
            damage::OutputDamageTracker,
            gles::{GlesRenderbuffer, GlesRenderer},
        },
    },
    utils::{Rectangle, Transform as OutputTransform},
};

use super::{Assets, BLUR_DOWNSAMPLE, Painter, draw, keyboard::paper_tile, px};
use crate::shell::{
    DrawItem, Shell, TexKey,
    control_center::{
        CcLayout, KNOB, KNOB_SHADOW, PANEL_RADIUS, PANEL_SHADOW, PANEL_W, Part, Symbol,
        TILE_RADIUS, TILE_SHADOW, TRACK_H, TRACK_Y, Tile, TileView,
    },
    system::BatteryGlyph,
};

// 模块符号：Lucide（ISC 许可，见 crates/loong-ui/assets/icons/LICENSE）的 wifi-off / moon /
// sun / sun-dim，由 `ink::brush_symbol` 转成毛笔笔触。wifi 用 loong-ui 内置的同名符号。
const SVG_WIFI_OFF: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h.01"/><path d="M8.5 16.429a5 5 0 0 1 7 0"/><path d="M5 12.859a10 10 0 0 1 5.17-2.69"/><path d="M19 12.859a10 10 0 0 0-2.007-1.523"/><path d="M2 8.82a15 15 0 0 1 4.177-2.643"/><path d="M22 8.82a15 15 0 0 0-11.288-3.764"/><path d="m2 2 20 20"/></svg>"#;
const SVG_MOON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/></svg>"#;
const SVG_SUN: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2"/><path d="M12 20v2"/><path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/><path d="M2 12h2"/><path d="M20 12h2"/><path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/></svg>"#;
const SVG_SUN_DIM: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"/><path d="M12 4h.01"/><path d="M20 12h.01"/><path d="M12 20h.01"/><path d="M4 12h.01"/><path d="M17.657 6.343h.01"/><path d="M17.657 17.657h.01"/><path d="M6.343 17.657h.01"/><path d="M6.343 6.343h.01"/></svg>"#;

/// 绢底的模糊（设计规范 §6.1「绢」：σ 24pt）。
const BLUR_PT: f64 = 24.0;

/// 控制中心位图的状态。
#[derive(Default)]
pub(super) struct CcAssets {
    /// 已画好的模块内容。
    tiles: HashMap<Tile, TileView>,
    /// 模块墨影（按尺寸缓存，宽窄两种）。
    shadows: HashMap<(u32, u32), Pixmap>,
    /// 背后的取样对应的 `ControlCenter::epoch`。
    backdrop_epoch: Option<u64>,
}

impl CcAssets {
    /// 外观变了：全部作废（位图本身由 `Assets::build` 清掉）。
    pub(super) fn reset(&mut self) {
        *self = CcAssets::default();
    }
}

/// 开启的开关着的颜料：亮色石青、暗色泥金（暗色外观中“激活”的控件用泥金）。
fn active_pigment(theme: Theme) -> u32 {
    match theme {
        Theme::Light => pigment::SHIQING,
        Theme::Dark => pigment::NIJIN,
    }
}

/// 设计规范的 `control`：亮色浓墨、暗色泥金（滑块已着墨段）。
fn control_color(theme: Theme) -> u32 {
    match theme {
        Theme::Light => theme.palette().ink[1],
        Theme::Dark => pigment::NIJIN,
    }
}

/// 模块的素笺：宣纸提亮（暗色为略亮的墨绢，好从绢面板上浮起）。
fn sheet_color(theme: Theme) -> u32 {
    match theme {
        Theme::Light => 0xF9F6EE,
        Theme::Dark => 0x2A2720,
    }
}

/// 朱砂色文字（`accentText`，只放在素笺上）。
fn accent_text(theme: Theme) -> u32 {
    match theme {
        Theme::Light => pigment::ZHUSHA,
        Theme::Dark => 0xE06A55,
    }
}

impl Assets {
    /// 控制中心的位图按需生成（第一次打开时才画，开机不付出代价）。
    pub(super) fn ensure_control_center(&mut self, part: Part, shell: &Shell) {
        let key = TexKey::ControlCenter(part);
        match part {
            // 由 Painter 取样（见 `Painter::capture_backdrop`）。
            Part::Backdrop => {}
            Part::Tile(tile) => {
                let view = shell
                    .control_center
                    .tile_view(tile, &shell.system, shell.theme);
                if self.cc.tiles.get(&tile) != Some(&view) || !self.pixmaps.contains_key(&key) {
                    let pm = self.tile_pixmap(tile, &view, &shell.control_center.layout);
                    self.put(key, pm);
                    self.cc.tiles.insert(tile, view);
                }
            }
            _ if self.pixmaps.contains_key(&key) => {}
            Part::Panel => {
                let pm = panel_pixmap(self.theme, self.scale, &shell.control_center.layout);
                self.put(key, pm);
            }
            Part::Track | Part::Ink => {
                let (track, ink) =
                    slider_pixmaps(self.theme, self.scale, &shell.control_center.layout);
                self.put(TexKey::ControlCenter(Part::Track), track);
                self.put(TexKey::ControlCenter(Part::Ink), ink);
            }
            Part::Knob => {
                let pm = knob_pixmap(self.theme, self.scale);
                self.put(key, pm);
            }
        }
    }

    /// 背后的取样是否需要（重新）做。
    pub(super) fn needs_backdrop(&self, shell: &Shell) -> bool {
        self.cc.backdrop_epoch != Some(shell.control_center.epoch())
            || !self
                .pixmaps
                .contains_key(&TexKey::ControlCenter(Part::Backdrop))
    }

    /// 一个模块：墨影 + 素笺 + 内容，四周留 [`TILE_SHADOW`]。
    fn tile_pixmap(&mut self, tile: Tile, view: &TileView, layout: &CcLayout) -> Pixmap {
        let (theme, s) = (self.theme, self.scale);
        let r = layout.tile(tile);
        let (w, h) = (px(r.w, s), px(r.h, s));
        let sp = px(TILE_SHADOW, s);
        let radius = (TILE_RADIUS * s) as f32;
        let mut out = loong_ui::pixmap(w + 2 * sp, h + 2 * sp);
        let shadow = self
            .cc
            .shadows
            .entry((w, h))
            .or_insert_with(|| ink::ink_shadow(theme, w, h, radius, sp));
        draw(&mut out, shadow, 0, 0);
        let seed = 0xcc0 + tile as u32 * 13;
        let mut chip = paper_tile(
            theme,
            w,
            h,
            radius,
            sheet_color(theme),
            (0.8 * s as f32).max(0.8),
            seed,
        );
        let pal = theme.palette();
        let ink_of = |level: Ink| pal.ink[level as usize];
        // 左侧符号与右侧文字的位置（物理像素，相对素笺）。
        let pt = |v: f64| (v * s) as f32;
        match view {
            TileView::Toggle {
                symbol,
                title,
                subtitle,
                active,
                enabled,
            } => {
                // 符号下的一池墨（砚池）：开启时研开一池颜料（亮色石青、暗色泥金），符号用纸色
                // （水破墨）；关闭时只是一池清水般的淡墨。
                let (cx, cy) = (pt(36.0), h as f32 / 2.0);
                let (pool_color, density, color) = if *active {
                    let on_control = match theme {
                        Theme::Light => 0xFBF8F1,
                        Theme::Dark => 0x15140F,
                    };
                    (
                        ink::pigment_on(theme, active_pigment(theme)),
                        0.94,
                        on_control,
                    )
                } else if *enabled {
                    (rgb(ink_of(Ink::Nong)), 0.10, ink_of(Ink::Nong))
                } else {
                    (rgb(ink_of(Ink::Nong)), 0.06, ink_of(Ink::Dan))
                };
                pool(&mut chip, cx, cy, pt(21.0), pool_color, density, seed);
                let svg = match symbol {
                    Symbol::Wifi => loong_ui::icon::builtin_symbol("wifi").unwrap_or(SVG_WIFI_OFF),
                    Symbol::WifiOff => SVG_WIFI_OFF,
                    Symbol::Moon => SVG_MOON,
                };
                let size = pt(22.0);
                symbol_at(&mut chip, svg, cx, cy, size, color, seed);
                let title_color = if *enabled {
                    ink_of(Ink::Jiao)
                } else {
                    ink_of(Ink::Dan)
                };
                let title = self.text.render(
                    title,
                    &TextStyle {
                        family: FontRole::Display,
                        size: pt(17.0),
                        weight: 400,
                        color: theme::hex(title_color, 1.0),
                        shadow: false,
                    },
                );
                let x = pt(68.0) as i32;
                draw(&mut chip, &title, x, pt(15.0) as i32);
                let sub_style = TextStyle {
                    family: FontRole::Body,
                    size: pt(13.0),
                    weight: 400,
                    color: theme::hex(
                        if *enabled {
                            ink_of(Ink::Zhong)
                        } else {
                            ink_of(Ink::Dan)
                        },
                        1.0,
                    ),
                    shadow: false,
                };
                let room = w as f32 - x as f32 - pt(12.0);
                let sub = self.render_fitting(subtitle, &sub_style, room);
                draw(&mut chip, &sub, x, pt(43.0) as i32);
            }
            TileView::Brightness { available } => {
                let color = if *available {
                    ink_of(Ink::Zhong)
                } else {
                    ink_of(Ink::Dan)
                };
                let label = if *available {
                    "亮度"
                } else {
                    "亮度 · 不可用"
                };
                let title = self.text.render(
                    label,
                    &TextStyle {
                        family: FontRole::Display,
                        size: pt(15.0),
                        weight: 400,
                        color: theme::hex(color, 1.0),
                        shadow: false,
                    },
                );
                draw(&mut chip, &title, pt(16.0) as i32, pt(8.0) as i32);
                let cy = pt(TRACK_Y);
                symbol_at(&mut chip, SVG_SUN_DIM, pt(26.0), cy, pt(20.0), color, seed);
                symbol_at(
                    &mut chip,
                    SVG_SUN,
                    w as f32 - pt(26.0),
                    cy,
                    pt(24.0),
                    color,
                    seed + 1,
                );
            }
            TileView::Battery { glyph, text } => {
                let title = self.text.render(
                    "电池",
                    &TextStyle {
                        family: FontRole::Display,
                        size: pt(15.0),
                        weight: 400,
                        color: theme::hex(ink_of(Ink::Zhong), 1.0),
                        shadow: false,
                    },
                );
                draw(&mut chip, &title, pt(16.0) as i32, pt(8.0) as i32);
                let cy = pt(TRACK_Y);
                let mut x = pt(16.0);
                if let Some(BatteryGlyph { percent, charging }) = *glyph {
                    let battery = ink::battery(
                        theme,
                        pt(20.0).round() as u32,
                        percent as f32 / 100.0,
                        charging,
                    );
                    draw(
                        &mut chip,
                        &battery,
                        x as i32,
                        (cy - battery.height() as f32 / 2.0) as i32,
                    );
                    x += battery.width() as f32 + pt(10.0);
                    let low = BatteryGlyph { percent, charging }.is_low();
                    let number = self.text.render(
                        &format!("{percent}%"),
                        &TextStyle {
                            family: FontRole::Display,
                            size: pt(24.0),
                            weight: 400,
                            color: theme::hex(
                                if low {
                                    accent_text(theme)
                                } else {
                                    ink_of(Ink::Jiao)
                                },
                                1.0,
                            ),
                            shadow: false,
                        },
                    );
                    draw(
                        &mut chip,
                        &number,
                        x as i32,
                        (cy - number.height() as f32 / 2.0) as i32,
                    );
                    x += number.width() as f32 + pt(12.0);
                }
                let style = TextStyle {
                    family: FontRole::Body,
                    size: pt(14.0),
                    weight: 400,
                    color: theme::hex(ink_of(Ink::Zhong), 1.0),
                    shadow: false,
                };
                let line = self.render_fitting(text, &style, w as f32 - x - pt(14.0));
                draw(
                    &mut chip,
                    &line,
                    x as i32,
                    (cy - line.height() as f32 / 2.0) as i32,
                );
            }
        }
        draw(&mut out, &chip, sp as i32, sp as i32);
        out
    }

    /// 单行文字，超出 `width`（物理像素）时在末尾截断并加“…”。
    fn render_fitting(&mut self, text: &str, style: &TextStyle, width: f32) -> Pixmap {
        if self.text.measure(text, style).0 <= width {
            return self.text.render(text, style);
        }
        let chars: Vec<char> = text.chars().collect();
        for n in (0..chars.len()).rev() {
            let candidate: String = chars[..n].iter().chain(['…'].iter()).collect();
            if self.text.measure(&candidate, style).0 <= width {
                return self.text.render(&candidate, style);
            }
        }
        self.text.render("…", style)
    }
}

/// 把线条符号用毛笔笔触画到 `(cx, cy)`（物理像素），边长 `size`。
fn symbol_at(pm: &mut Pixmap, svg: &str, cx: f32, cy: f32, size: f32, color: u32, seed: u32) {
    let Some(rect) = Rect::from_xywh(cx - size / 2.0, cy - size / 2.0, size, size) else {
        return;
    };
    ink::brush_symbol(pm, svg, rect, theme::hex(color, 1.0), 1.0, seed);
}

/// 一池墨：以 `(cx, cy)` 为中心、半径 `r` 的圆，边缘被纸纤维扯出细微毛边、略积墨（水渍），
/// 中间带一点颜料颗粒。只落在素笺上（按已有像素的不透明度遮住）。
fn pool(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, color: [f32; 3], density: f32, seed: u32) {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let reach = (r * 1.15).ceil() as i32;
    let data = pm.data_mut();
    for y in (cy as i32 - reach).max(0)..(cy as i32 + reach).min(h) {
        for x in (cx as i32 - reach).max(0)..(cx as i32 + reach).min(w) {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let dist = (dx * dx + dy * dy).sqrt();
            // 边缘起伏约 ±0.8 像素量级（随半径），一像素宽的抗锯齿。
            let n = noise::fbm(x as f32 * 0.35, y as f32 * 0.35, seed, 3) - 0.5;
            let edge = dist + n * r * 0.08;
            let inside = (r - edge + 0.5).clamp(0.0, 1.0);
            if inside <= 0.0 {
                continue;
            }
            let rim = smoothstep(r * 0.72, r * 0.98, edge) * 0.18;
            let grain = 0.94 + 0.06 * noise::value(x as f32 * 0.9, y as f32 * 0.9, seed ^ 0x9a);
            let a = (inside * density * grain * (1.0 + rim)).clamp(0.0, 1.0);
            let i = ((y * w + x) * 4) as usize;
            let cover = data[i + 3] as f32 / 255.0;
            if cover <= 0.0 {
                continue;
            }
            for c in 0..3 {
                let v = data[i + c] as f32 / 255.0;
                data[i + c] =
                    ((v * (1.0 - a) + color[c] * a * cover) * 255.0 + 0.5).min(255.0) as u8;
            }
        }
    }
}

/// 「绢」面板：`ink::silk_panel`（背后透明——模糊的取样由 GPU 叠在下面）+ 四周墨影。
fn panel_pixmap(theme: Theme, s: f64, layout: &CcLayout) -> Pixmap {
    let (w, h) = (px(PANEL_W, s), px(layout.panel.h, s));
    let sp = px(PANEL_SHADOW, s);
    let radius = (PANEL_RADIUS * s) as f32;
    let mut out = ink::ink_shadow(theme, w, h, radius, sp);
    let rect = Rect::from_xywh(0.0, 0.0, w as f32, h as f32).expect("面板尺寸有效");
    let silk = ink::silk_panel(theme, &loong_ui::pixmap(1, 1), rect, radius);
    draw(&mut out, &silk, sp as i32, sp as i32);
    out
}

/// 亮度滑块：（一笔淡墨的轨道，同一笔的浓墨 / 泥金着墨段）。两张位图笔形完全相同，
/// 着墨段由 GPU 按亮度裁剪。
fn slider_pixmaps(theme: Theme, s: f64, layout: &CcLayout) -> (Pixmap, Pixmap) {
    let (x0, x1, _) = layout.track();
    let (w, h) = (px(x1 - x0, s), px(TRACK_H, s));
    let thick = (5.0 * s) as f32;
    let y = h as f32 / 2.0;
    let stroke = Stroke::open(vec![(thick * 0.6, y), (w as f32 - thick * 0.6, y)]);
    let mut style = BrushStyle::for_width(thick);
    style.pressure = 0.35;
    style.dryness = style.dryness.min(0.12);
    style.taper = Some(false);
    let field = brush::render(&[stroke], &style, w as usize, h as usize, 0x51d);
    let pal = theme.palette();
    let mut track = loong_ui::pixmap(w, h);
    blend_field(&mut track, &field, rgb(pal.ink[Ink::Dan as usize]), 0.55);
    let mut inked = loong_ui::pixmap(w, h);
    blend_field(&mut inked, &field, rgb(control_color(theme)), 0.95);
    (track, inked)
}

/// 滑块的纸团：圆形的纸（`knob` 色，左上受光、右下略暗）+ 一圈极淡的墨边 + 墨影。
fn knob_pixmap(theme: Theme, s: f64) -> Pixmap {
    let d = px(KNOB, s);
    let sp = px(KNOB_SHADOW, s);
    let radius = d as f32 / 2.0;
    let mut out = ink::ink_shadow(theme, d, d, radius, sp);
    let base = rgb(match theme {
        Theme::Light => 0xFBF8F1,
        Theme::Dark => 0xECE6D9,
    });
    let rim = rgb(theme.palette().ink[3]);
    let rim_k = match theme {
        Theme::Light => 0.45,
        Theme::Dark => 0.25,
    };
    let size = out.width() as i32;
    let c = size as f32 / 2.0;
    let data = out.data_mut();
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
            let dist = (dx * dx + dy * dy).sqrt();
            let a = (radius - dist + 0.5).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let shade = 1.0 - 0.05 * ((dx + dy) / radius).clamp(-1.0, 1.0)
                + 0.012 * (noise::value(x as f32 * 0.8, y as f32 * 0.8, 0x6b0) - 0.5);
            let edge = smoothstep(radius - 1.6 * s as f32, radius - 0.2, dist) * rim_k;
            let i = ((y * size + x) * 4) as usize;
            for k in 0..3 {
                let v = (base[k] * shade) * (1.0 - edge) + rim[k] * edge;
                let under = data[i + k] as f32 / 255.0;
                data[i + k] = ((v.clamp(0.0, 1.0) * a + under * (1.0 - a)) * 255.0 + 0.5) as u8;
            }
            let under_a = data[i + 3] as f32 / 255.0;
            data[i + 3] = ((a + under_a * (1.0 - a)) * 255.0 + 0.5) as u8;
        }
    }
    out
}

/// 取样结果（1/4 分辨率的整屏，不透明）→ 绢底：模糊 + 纸色罩染。
fn backdrop_pixmap(mut pm: Pixmap, theme: Theme, small_scale: f64) -> Pixmap {
    let radius = (BLUR_PT * small_scale).round().max(1.0) as usize;
    loong_ui::material::blur(&mut pm, radius);
    let all = Rect::from_xywh(0.0, 0.0, pm.width() as f32, pm.height() as f32).expect("尺寸有效");
    let mut veil = Paint::default();
    veil.set_color(match theme {
        Theme::Light => theme::hex(theme.palette().paper, 0.40),
        Theme::Dark => theme::hex(0x0B0A08, 0.48),
    });
    pm.fill_rect(all, &veil, Transform::identity(), None);
    pm
}

impl Painter {
    /// 控制中心收起后（场景里不再有绢底）释放取样：下次打开时反正要重新取样。面板、模块、
    /// 滑块的位图留着（每种外观画一次，再打开时不用重画）。
    pub(super) fn release_backdrop(&mut self, scene: &[DrawItem], assets: &mut Assets) {
        let key = TexKey::ControlCenter(Part::Backdrop);
        let drawn = scene
            .iter()
            .any(|item| matches!(item, DrawItem::Texture { key: k, .. } if *k == key));
        if !drawn && assets.pixmaps.remove(&key).is_some() {
            self.textures.remove(&key);
            assets.cc.backdrop_epoch = None;
        }
    }

    /// 取样控制中心下面的画面（`below` 为场景中排在背后纹理之前的元素），生成绢底。
    /// 失败时只记录警告：没有绢底，面板照样能用。
    pub(super) fn capture_backdrop(
        &mut self,
        renderer: &mut GlesRenderer,
        below: &[DrawItem],
        assets: &mut Assets,
        shell: &Shell,
    ) {
        assets.cc.backdrop_epoch = Some(shell.control_center.epoch());
        match self.render_small(renderer, below, assets, shell) {
            Ok(pm) => {
                let small = self.scale / BLUR_DOWNSAMPLE;
                let pm = backdrop_pixmap(pm, assets.theme, small);
                assets.put(TexKey::ControlCenter(Part::Backdrop), pm);
            }
            Err(e) => tracing::warn!("控制中心取样失败: {e}"),
        }
    }

    /// 把一组场景元素按 1/[`BLUR_DOWNSAMPLE`] 的输出缩放画到离屏缓冲并读回。
    fn render_small(
        &mut self,
        renderer: &mut GlesRenderer,
        items: &[DrawItem],
        assets: &mut Assets,
        shell: &Shell,
    ) -> anyhow::Result<Pixmap> {
        let full = self.scale;
        let small = full / BLUR_DOWNSAMPLE;
        let (w, h) = shell.screen_size();
        let (pw, ph) = (px(w, small), px(h, small));
        self.scale = small;
        let mut ignored = Vec::new();
        let built = self.build(renderer, items, assets, shell, &mut ignored);
        self.scale = full;
        let mut elements = built?;
        elements.reverse();
        let size = (pw as i32, ph as i32);
        let mut buffer: GlesRenderbuffer = renderer.create_buffer(Fourcc::Abgr8888, size.into())?;
        let mut fb = renderer.bind(&mut buffer)?;
        let mut damage = OutputDamageTracker::new(size, 1.0, OutputTransform::Normal);
        damage
            .render_output(renderer, &mut fb, 0, &elements, [0.0, 0.0, 0.0, 1.0])
            .map_err(|e| anyhow!("渲染失败: {e:?}"))?;
        let mapping =
            renderer.copy_framebuffer(&fb, Rectangle::from_size(size.into()), Fourcc::Abgr8888)?;
        let rgba = renderer.map_texture(&mapping)?.to_vec();
        drop(fb);
        let mut pm = loong_ui::pixmap(pw, ph);
        pm.data_mut()
            .copy_from_slice(&rgba[..(pw * ph * 4) as usize]);
        Ok(pm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backdrop_is_blurred_and_veiled() {
        // 一半黑一半白：模糊后边界变软，罩上纸色后黑处变亮。
        let (w, h) = (64u32, 16u32);
        let mut pm = loong_ui::pixmap(w, h);
        for (i, p) in pm.data_mut().chunks_exact_mut(4).enumerate() {
            let x = i as u32 % w;
            let v = if x < w / 2 { 0 } else { 255 };
            p.copy_from_slice(&[v, v, v, 255]);
        }
        let out = backdrop_pixmap(pm, Theme::Light, 0.25);
        let at = |x: u32| out.pixel(x, h / 2).unwrap().red();
        assert!(at(0) > 80, "罩染：{}", at(0));
        assert!(
            at(w / 2 - 2) > at(0) && at(w / 2 + 1) < at(w - 1),
            "边界模糊"
        );
        assert!(out.pixels().iter().all(|p| p.alpha() == 255), "不透明");
    }

    #[test]
    fn pool_stays_on_the_sheet() {
        let mut pm = loong_ui::pixmap(40, 40);
        // 只有左半边有纸。
        for (i, p) in pm.data_mut().chunks_exact_mut(4).enumerate() {
            if i % 40 < 20 {
                p.copy_from_slice(&[240, 238, 230, 255]);
            }
        }
        let shiqing = ink::pigment_on(Theme::Light, pigment::SHIQING);
        pool(&mut pm, 20.0, 20.0, 12.0, shiqing, 0.9, 1);
        assert_eq!(pm.pixel(26, 20).unwrap().alpha(), 0, "不画到纸外");
        let tinted = pm.pixel(16, 20).unwrap();
        assert!(tinted.blue() > tinted.red(), "石青偏蓝：{tinted:?}");
        assert_eq!(pm.pixel(2, 2).unwrap().red(), 240, "圆外不变");
    }
}
