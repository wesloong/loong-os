//! 屏幕键盘的位图（「墨韵」）：在 CPU 上用 `loong_ui::ink` 画好，GPU 只做位移与混合
//! （键盘升起/收起、按下着墨、候选滚动都不重画位图）。
//!
//! | 纹理 | 内容 | 何时重画 |
//! |---|---|---|
//! | `KeyboardPanel` | 熟宣背板（暗色为墨绢）+ 上缘向上投的墨影 + 候选栏下一笔渐淡的淡墨 + 全部键帽小笺（素笺、毛边、贴纸的墨影） | 外观或尺寸变化。三层键盘共用一个网格，换层不用重画 |
//! | `KeyboardGlyphs` | 键帽上的文楷字与毛笔符号（Shift、退格、收起） | `Keyboard::revision` 变化（换层、Shift、中/英）；字形按文字缓存 |
//! | `KeyPreview` | 按下字符键时浮起的放大小笺 + 大号字 | 按下的键变化（小笺底只画一次） |
//! | `KeyAlternates` | 长按备选条（选中的一格浓墨底、纸色字：水破墨） | 备选或选中项变化 |
//! | `CandidatePreedit` / `CandidateCell(i)` | 拼音与候选（文楷） | 候选栏内容变化 |
//! | `CandidateMark` | 高亮候选下的一笔浓墨 | 外观变化 |

use std::collections::HashMap;

use loong_ime::keyboard::{AlternatesPopup, KeyKind, KeyLabel, KeySymbol, LayoutParams};
use loong_ui::{
    Pixmap,
    ink::{
        self,
        brush::{self, BrushStyle, Stroke},
        noise, paper,
        raster::{Field, path_mask, rgb},
    },
    shape,
    text::{FontRole, TextStyle},
    theme::{self, Theme},
    tiny_skia::{Rect, Transform},
};

use super::{Assets, draw, px};
use crate::shell::{
    Shell, TexKey,
    keyboard::{CellKind, PANEL_SHADOW, POPUP_SHADOW, ScreenKeyboard},
};

// 键帽符号：Lucide（ISC 许可）的 arrow-big-up / arrow-big-up-dash / delete，以及按 Lucide
// 风格画的“收起键盘”（键盘轮廓 + 向下的折线）。由 `ink::brush_symbol` 转成毛笔笔触。
const SVG_SHIFT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18v-6H5l7-7 7 7h-4v6H9z"/></svg>"#;
const SVG_CAPS_LOCK: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 19h6"/><path d="M9 15v-3H5l7-7 7 7h-4v3H9z"/></svg>"#;
const SVG_BACKSPACE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 5H9l-7 7 7 7h11a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2Z"/><path d="m18 9-6 6"/><path d="m12 9 6 6"/></svg>"#;
const SVG_HIDE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="18" height="12" rx="2"/><path d="M7 7h2"/><path d="M11 7h2"/><path d="M15 7h2"/><path d="M8 11h8"/><path d="m9 18 3 3 3-3"/></svg>"#;

/// 字母键帽上拉丁字母相对 `Metrics::letter_font_size` 的放大倍数。
const LATIN_SCALE: f64 = 1.18;

/// 键盘各部分的纸色与墨色（0xRRGGBB）。
struct Tones {
    /// 背板（熟宣 / 墨绢）。
    panel: u32,
    /// 字符键小笺（素笺）。
    letter: u32,
    /// 功能键小笺（略深的纸）。
    function: u32,
    /// 字符键上的字。
    glyph: u32,
    /// 功能键上的字与符号。
    label: u32,
    /// 角标、翻页箭头。
    hint: u32,
    /// “选中的一笔”与备选条选中格（亮色浓墨，暗色泥金）。
    control: u32,
}

fn tones(theme: Theme) -> Tones {
    let ink = theme.palette().ink;
    match theme {
        Theme::Light => Tones {
            panel: 0xE4DCC9,
            letter: 0xF9F6EE,
            function: 0xD9D0BC,
            glyph: ink[1],
            label: ink[2],
            hint: ink[3],
            control: ink[1],
        },
        Theme::Dark => Tones {
            panel: 0x191813,
            letter: 0x2B2821,
            function: 0x211F19,
            glyph: ink[0],
            label: ink[2],
            hint: ink[3],
            control: theme::pigment::NIJIN,
        },
    }
}

/// 文字位图缓存的键：文字、字号（物理像素 × 100）、颜色、字体。
type TextKey = (String, u32, u32, bool);

/// 键盘位图的状态（哪些纹理是按什么画的），避免重复绘制。
#[derive(Default)]
pub struct KeyboardAssets {
    panel_for: Option<LayoutParams>,
    glyphs_for: Option<(LayoutParams, u64)>,
    preview_base: Option<((u32, u32), Pixmap)>,
    preview_for: Option<String>,
    cells_for: Option<u64>,
    mark_built: bool,
    popup_for: Option<AlternatesPopup>,
    /// 键帽、候选文字位图（换层、Shift 来回切换时不必重排字形）。
    text_cache: HashMap<TextKey, Pixmap>,
    /// 毛笔符号位图（符号、边长、颜色）。
    symbol_cache: HashMap<(KeySymbol, u32, u32), Pixmap>,
    /// 键帽小笺与它的墨影（宽、高、种类）：同样大小的键共用。
    tile_cache: HashMap<(u32, u32, bool), (Pixmap, Pixmap)>,
}

impl Assets {
    /// 键盘显示或即将显示时，按需重画键盘的位图。
    pub(super) fn sync_keyboard(&mut self, shell: &Shell) {
        let kb = &shell.keyboard;
        // 输入框没激活时不画（开机不为键盘花时间）；收起动画中沿用已有的位图。
        if !kb.is_active() && !kb.is_shown() {
            return;
        }
        let params = kb.layout().params;
        if self.kb.panel_for != Some(params) {
            let t0 = std::time::Instant::now();
            let panel = self.keyboard_panel(kb);
            self.put(TexKey::KeyboardPanel, panel);
            self.kb.panel_for = Some(params);
            self.kb.glyphs_for = None;
            self.kb.preview_base = None;
            self.kb.preview_for = None;
            self.kb.cells_for = None;
            tracing::debug!("键盘背板：{:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
        }
        let revision = kb.input_method().keyboard().revision();
        if self.kb.glyphs_for != Some((params, revision)) {
            let glyphs = self.keyboard_glyphs(kb);
            self.put(TexKey::KeyboardGlyphs, glyphs);
            self.kb.glyphs_for = Some((params, revision));
        }
        if !self.kb.mark_built {
            let mark = self.candidate_mark();
            self.put(TexKey::CandidateMark, mark);
            self.kb.mark_built = true;
        }
        if self.kb.cells_for != Some(kb.candidate_version()) {
            self.candidate_cells(kb);
            self.kb.cells_for = Some(kb.candidate_version());
        }
        if let Some(id) = kb.preview_key() {
            let label = label_text(&kb.input_method().keyboard().label(id));
            if self.kb.preview_for.as_deref() != Some(label.as_str()) {
                let preview = self.key_preview(kb, &label);
                self.put(TexKey::KeyPreview, preview);
                self.kb.preview_for = Some(label);
            }
        }
        let popup = kb.input_method().keyboard().popup().cloned();
        if popup.is_some() && self.kb.popup_for != popup {
            let p = popup.clone().expect("有备选条");
            let pm = self.alternates(kb, &p);
            self.put(TexKey::KeyAlternates, pm);
            self.kb.popup_for = popup;
        }
    }

    fn text_pixmap(&mut self, text: &str, size_pt: f64, color: u32, display: bool) -> Pixmap {
        let size = (size_pt * self.scale) as f32;
        let key = (text.to_owned(), (size * 100.0) as u32, color, display);
        if let Some(pm) = self.kb.text_cache.get(&key) {
            return pm.clone();
        }
        let pm = self.text.render(
            text,
            &TextStyle {
                family: if display {
                    FontRole::Display
                } else {
                    FontRole::Body
                },
                size,
                weight: 400,
                color: theme::hex(color, 1.0),
                shadow: false,
            },
        );
        self.kb.text_cache.insert(key, pm.clone());
        pm
    }

    fn symbol_pixmap(&mut self, symbol: KeySymbol, size: u32, color: u32) -> Pixmap {
        let key = (symbol, size, color);
        if let Some(pm) = self.kb.symbol_cache.get(&key) {
            return pm.clone();
        }
        let svg = match symbol {
            KeySymbol::Shift | KeySymbol::ShiftOnce => SVG_SHIFT,
            KeySymbol::CapsLock => SVG_CAPS_LOCK,
            KeySymbol::Backspace => SVG_BACKSPACE,
            KeySymbol::HideKeyboard => SVG_HIDE,
        };
        let mut pm = loong_ui::pixmap(size, size);
        let rect = Rect::from_xywh(0.0, 0.0, size as f32, size as f32).expect("尺寸有效");
        ink::brush_symbol(
            &mut pm,
            svg,
            rect,
            theme::hex(color, 1.0),
            0.9,
            0x6b0 + symbol as u32,
        );
        self.kb.symbol_cache.insert(key, pm.clone());
        pm
    }

    /// 一张键帽小笺（素笺 + 毛边）与它贴在背板上的墨影（四周留 `spread`）。
    fn key_tile(&mut self, w: u32, h: u32, function: bool) -> (Pixmap, Pixmap) {
        if let Some(t) = self.kb.tile_cache.get(&(w, h, function)) {
            return t.clone();
        }
        let theme = self.theme;
        let s = self.scale as f32;
        let tone = tones(theme);
        let base = if function { tone.function } else { tone.letter };
        let seed = 0x7ab + w * 31 + h * 7 + u32::from(function);
        let radius = (h as f32 / s * 0.11).round().clamp(5.0, 8.0) * s;
        let tile = paper_tile(theme, w, h, radius, base, (0.7 * s).max(0.7), seed);
        let spread = (3.0 * s).round().max(2.0) as u32;
        let shadow = ink::ink_shadow(theme, w, h, radius, spread);
        self.kb
            .tile_cache
            .insert((w, h, function), (tile.clone(), shadow.clone()));
        (tile, shadow)
    }

    /// 背板：上缘墨影 + 熟宣 / 墨绢 + 候选栏下的一笔淡墨 + 键帽小笺。
    fn keyboard_panel(&mut self, kb: &ScreenKeyboard) -> Pixmap {
        let theme = self.theme;
        let s = self.scale;
        let layout = kb.layout();
        let (w, h) = (layout.params.width, layout.params.height);
        let (pw, ph, sh) = (px(w, s), px(h, s), px(PANEL_SHADOW, s));
        let tone = tones(theme);
        let mut out = loong_ui::pixmap(pw, ph + sh);

        // 上缘的墨影：纸浮在应用之上，向上淡开，略有浓淡起伏。
        let (shadow_rgb, k) = match theme {
            Theme::Light => (rgb(0x3A3020), 0.24),
            Theme::Dark => ([0.0, 0.0, 0.0], 0.45),
        };
        {
            let data = out.data_mut();
            for y in 0..sh {
                let t = (y as f32 + 0.5) / sh as f32;
                for x in 0..pw {
                    let lumpy = 0.8 + 0.4 * noise::fbm(x as f32 / (90.0 * s as f32), 0.5, 0x5ad, 2);
                    let a = (k * t.powf(2.4) * lumpy).min(1.0);
                    let i = ((y * pw + x) * 4) as usize;
                    data[i] = (shadow_rgb[0] * a * 255.0 + 0.5) as u8;
                    data[i + 1] = (shadow_rgb[1] * a * 255.0 + 0.5) as u8;
                    data[i + 2] = (shadow_rgb[2] * a * 255.0 + 0.5) as u8;
                    data[i + 3] = (a * 255.0 + 0.5) as u8;
                }
            }
        }
        let sheet = paper::paper_with(theme, pw, ph, tone.panel, 0x4b0);
        draw(&mut out, &sheet, 0, sh as i32);

        // 候选栏与键区之间：一笔从左向右渐淡的淡墨。
        let bar_h = layout.candidate_bar.h;
        let y = (sh as f64 + bar_h * s) as f32;
        let x0 = (16.0 * s) as f32;
        let x1 = pw as f32 - x0;
        let mut field = brush::render(
            &[Stroke::open(vec![(x0, y), (x1, y)])],
            &BrushStyle {
                taper: Some(true),
                ..BrushStyle::for_width((1.3 * s) as f32)
            },
            pw as usize,
            (ph + sh) as usize,
            0x11e,
        );
        for (i, v) in field.data.iter_mut().enumerate() {
            let fx = (i % pw as usize) as f32 / pw as f32;
            *v *= 0.55 * (1.0 - fx).powf(0.7);
        }
        blend(&mut out, &field, tone.hint);

        // 键帽：同样大小的键共用一张小笺。
        for key in &layout.keys {
            let r = key.rect;
            let (x, y) = (
                (r.x * s).round() as i32,
                (r.y * s).round() as i32 + sh as i32,
            );
            let (w, h) = (px(r.w, s), px(r.h, s));
            let function = key.kind.is_function();
            let (tile, shadow) = self.key_tile(w, h, function);
            let spread = ((shadow.width() - w) / 2) as i32;
            draw(&mut out, &shadow, x - spread, y - spread);
            draw(&mut out, &tile, x, y);
        }
        out
    }

    /// 键帽上的字与符号（与键盘等大的透明位图）。
    fn keyboard_glyphs(&mut self, kb: &ScreenKeyboard) -> Pixmap {
        let s = self.scale;
        let layout = kb.layout().clone();
        let m = layout.metrics;
        let tone = tones(self.theme);
        let ink = self.theme.palette().ink;
        let mut out = loong_ui::pixmap(px(layout.params.width, s), px(layout.params.height, s));
        let keyboard = kb.input_method().keyboard();
        for (id, key) in layout.keys.iter().enumerate() {
            let r = key.rect;
            let (cx, cy) = ((r.x + r.w / 2.0) * s, (r.y + r.h / 2.0) * s);
            match keyboard.label(id) {
                KeyLabel::Text(text) => {
                    let (size, color) = match key.kind {
                        // 文楷的拉丁小写字母偏小，放大一些与汉字、标点的视觉大小相当。
                        KeyKind::Letter(_) => (m.letter_font_size * LATIN_SCALE, tone.glyph),
                        KeyKind::Char(_) => (m.letter_font_size, tone.glyph),
                        _ => (m.label_font_size, tone.label),
                    };
                    let pm = self.text_pixmap(&text, size, color, true);
                    draw_at_center(&mut out, &pm, cx, cy);
                }
                KeyLabel::TextWithHint(text, hint) => {
                    let pm = self.text_pixmap(&text, m.letter_font_size, tone.glyph, true);
                    draw_at_center(&mut out, &pm, cx, cy);
                    let pm = self.text_pixmap(&hint, m.label_font_size * 0.78, tone.hint, true);
                    let hx = (r.x + r.w) * s - pm.width() as f64 - 5.0 * s;
                    draw(&mut out, &pm, hx as i32, (r.y * s + 2.0 * s) as i32);
                }
                KeyLabel::Symbol(symbol) => {
                    let active = matches!(symbol, KeySymbol::ShiftOnce | KeySymbol::CapsLock);
                    let color = if active { ink[0] } else { tone.label };
                    let size = (r.h.min(r.w) * 0.46 * s).round() as u32;
                    let pm = self.symbol_pixmap(symbol, size, color);
                    draw_at_center(&mut out, &pm, cx, cy);
                    if active {
                        // Shift 生效：符号下方添一笔浓墨（“选中的一笔”）。
                        let w = size as f32 * 0.7;
                        let y = (cy + size as f64 * 0.62) as f32;
                        let stroke =
                            Stroke::open(vec![(cx as f32 - w / 2.0, y), (cx as f32 + w / 2.0, y)]);
                        let field = brush::render(
                            &[stroke],
                            &BrushStyle {
                                taper: Some(true),
                                ..BrushStyle::for_width((2.6 * s) as f32)
                            },
                            out.width() as usize,
                            out.height() as usize,
                            0x5f1,
                        );
                        blend(&mut out, &field, tone.control);
                    }
                }
                KeyLabel::Language { chinese } => {
                    // “中 / 英”：当前模式焦墨稍大，另一个清墨稍小。
                    let big = m.label_font_size * 1.15;
                    let small = m.label_font_size * 0.85;
                    let (zh, en) = if chinese {
                        (
                            self.text_pixmap("中", big, ink[0], true),
                            self.text_pixmap("英", small, ink[3], true),
                        )
                    } else {
                        (
                            self.text_pixmap("中", small, ink[3], true),
                            self.text_pixmap("英", big, ink[0], true),
                        )
                    };
                    let slash = self.text_pixmap("/", small, ink[4], true);
                    let total = (zh.width() + slash.width() + en.width()) as f64;
                    let mut x = cx - total / 2.0;
                    for pm in [&zh, &slash, &en] {
                        draw(
                            &mut out,
                            pm,
                            x as i32,
                            (cy - pm.height() as f64 / 2.0).round() as i32,
                        );
                        x += pm.width() as f64;
                    }
                }
            }
        }
        out
    }

    /// 候选栏的拼音与候选格子。
    fn candidate_cells(&mut self, kb: &ScreenKeyboard) {
        let s = self.scale;
        let tone = tones(self.theme);
        let ink = self.theme.palette().ink;
        let bar_h = kb.layout().candidate_bar.h;
        let bh = px(bar_h, s);
        if let Some((text, w)) = kb.preedit_cell() {
            let mut pm = loong_ui::pixmap(px(w, s), bh);
            let t = self.text_pixmap(text, kb.preedit_font_size(), tone.label, true);
            draw_at_center(&mut pm, &t, w * s / 2.0, bar_h * s / 2.0);
            // 右侧一道清墨，隔开拼音与候选。
            let x = (w * s) as f32 - 1.0;
            let field = brush::render(
                &[Stroke::open(vec![
                    (x, (bar_h * s * 0.28) as f32),
                    (x, (bar_h * s * 0.72) as f32),
                ])],
                &BrushStyle {
                    taper: Some(true),
                    ..BrushStyle::for_width((1.2 * s) as f32)
                },
                pm.width() as usize,
                pm.height() as usize,
                0x9e1,
            );
            blend(&mut pm, &field, ink[4]);
            self.put(TexKey::CandidatePreedit, pm);
        }
        let font = kb.candidate_font_size();
        let highlighted = kb.highlighted_candidate();
        for (i, cell) in kb.candidate_cells().to_vec().iter().enumerate() {
            let mut pm = loong_ui::pixmap(px(cell.w, s), bh);
            let (size, color) = match cell.kind {
                // 高亮的候选焦墨（按空格就选它），其余浓墨。
                CellKind::Candidate(c) if Some(c) == highlighted => (font, ink[0]),
                CellKind::Candidate(_) => (font, ink[1]),
                CellKind::PrevPage | CellKind::NextPage => (font * 1.1, tone.hint),
            };
            let t = self.text_pixmap(&cell.text, size, color, true);
            draw_at_center(&mut pm, &t, cell.w * s / 2.0, bar_h * s / 2.0);
            self.put(TexKey::CandidateCell(i), pm);
        }
    }

    /// 高亮候选下的一笔浓墨（拉伸绘制）。
    fn candidate_mark(&mut self) -> Pixmap {
        let s = self.scale as f32;
        let (w, h) = ((160.0 * s) as u32, (6.0 * s).ceil() as u32);
        let mut pm = loong_ui::pixmap(w, h);
        let y = h as f32 / 2.0;
        let field = brush::render(
            &[Stroke::open(vec![
                (2.0 * s, y),
                (w as f32 - 2.0 * s, y + 0.4 * s),
            ])],
            &BrushStyle {
                taper: Some(true),
                ..BrushStyle::for_width(3.0 * s)
            },
            w as usize,
            h as usize,
            0x3c7,
        );
        blend(&mut pm, &field, tones(self.theme).control);
        pm
    }

    /// 按下字符键时浮起的放大小笺：小笺底（含墨影）只画一次，再写上大号字。
    fn key_preview(&mut self, kb: &ScreenKeyboard, label: &str) -> Pixmap {
        let s = self.scale;
        let (w, h) = kb.preview_size();
        let (pw, ph) = (px(w, s), px(h, s));
        let base = match &self.kb.preview_base {
            Some((size, pm)) if *size == (pw, ph) => pm.clone(),
            _ => {
                let spread = px(POPUP_SHADOW, s);
                let theme = self.theme;
                let radius = (kb.layout().metrics.corner_radius * 1.3 * s) as f32;
                let tile = paper_tile(
                    theme,
                    pw,
                    ph,
                    radius,
                    tones(theme).letter,
                    (0.9 * s as f32).max(0.8),
                    0x2fe,
                );
                let mut pm = ink::ink_shadow(theme, pw, ph, radius, spread);
                draw(&mut pm, &tile, spread as i32, spread as i32);
                self.kb.preview_base = Some(((pw, ph), pm.clone()));
                pm
            }
        };
        let mut pm = base;
        let size = kb.layout().metrics.letter_font_size * 1.5 * LATIN_SCALE;
        let t = self.text_pixmap(label, size, tones(self.theme).glyph, true);
        let spread = px(POPUP_SHADOW, s) as f64;
        draw_at_center(
            &mut pm,
            &t,
            spread + pw as f64 / 2.0,
            spread + ph as f64 / 2.0,
        );
        pm
    }

    /// 长按备选条：素笺 + 墨影，选中的一格浓墨底、纸色字。
    fn alternates(&mut self, kb: &ScreenKeyboard, popup: &AlternatesPopup) -> Pixmap {
        let s = self.scale;
        let theme = self.theme;
        let tone = tones(theme);
        let f = popup.frame;
        let (pw, ph) = (px(f.w, s), px(f.h, s));
        let spread = px(POPUP_SHADOW, s);
        let radius = (kb.layout().metrics.corner_radius * s) as f32;
        let mut pm = ink::ink_shadow(theme, pw, ph, radius, spread);
        let tile = paper_tile(
            theme,
            pw,
            ph,
            radius,
            tone.letter,
            (0.8 * s as f32).max(0.7),
            0x1a7,
        );
        draw(&mut pm, &tile, spread as i32, spread as i32);
        let font = kb.layout().metrics.letter_font_size;
        let paper_color = theme.palette().paper;
        for (i, (r, text)) in popup.cells.iter().enumerate() {
            let selected = i == popup.selected;
            let (x, y) = (
                spread as f64 + (r.x - f.x) * s,
                spread as f64 + (r.y - f.y) * s,
            );
            if selected {
                let inset = 3.0 * s;
                let cell = Rect::from_xywh(
                    (x + inset) as f32,
                    (y + inset) as f32,
                    (r.w * s - 2.0 * inset) as f32,
                    (r.h * s - 2.0 * inset) as f32,
                )
                .expect("尺寸有效");
                let mut paint = loong_ui::tiny_skia::Paint {
                    anti_alias: true,
                    ..Default::default()
                };
                paint.set_color(theme::hex(tone.control, 0.92));
                pm.fill_path(
                    &shape::continuous_rect(cell, radius * 0.8),
                    &paint,
                    loong_ui::tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
            let color = if selected { paper_color } else { tone.glyph };
            let t = self.text_pixmap(text, font, color, true);
            draw_at_center(&mut pm, &t, x + r.w * s / 2.0, y + r.h * s / 2.0);
        }
        pm
    }
}

/// 键帽内容的文字（放大小笺上显示）。
fn label_text(label: &KeyLabel) -> String {
    match label {
        KeyLabel::Text(t) | KeyLabel::TextWithHint(t, _) => t.clone(),
        KeyLabel::Symbol(_) | KeyLabel::Language { .. } => String::new(),
    }
}

/// 一张纸笺：`base` 色宣纸 + 连续圆角 + 毛边（`deckle` 像素）。
fn paper_tile(
    theme: Theme,
    w: u32,
    h: u32,
    radius: f32,
    base: u32,
    deckle: f32,
    seed: u32,
) -> Pixmap {
    let mut pm = paper::paper_with(theme, w, h, base, seed);
    let r = Rect::from_xywh(0.5, 0.5, w as f32 - 1.0, h as f32 - 1.0).expect("尺寸有效");
    let mut mask = path_mask(
        &shape::continuous_rect(r, radius),
        w as usize,
        h as usize,
        Transform::identity(),
    );
    paper::deckle(&mut mask, deckle, seed ^ 0xd3c);
    for (px, m) in pm.data_mut().chunks_exact_mut(4).zip(&mask.data) {
        let m = m.clamp(0.0, 1.0);
        for c in px.iter_mut() {
            *c = (*c as f32 * m + 0.5) as u8;
        }
    }
    pm
}

/// 把墨量场按 `color` 叠到位图上（source-over，预乘）。
fn blend(pm: &mut Pixmap, field: &Field, color: u32) {
    loong_ui::ink::raster::blend_field(pm, field, rgb(color), 1.0);
}

/// 把 `src` 的中心对齐到 `(cx, cy)`（物理像素）。
fn draw_at_center(dst: &mut Pixmap, src: &Pixmap, cx: f64, cy: f64) {
    draw(
        dst,
        src,
        (cx - src.width() as f64 / 2.0).round() as i32,
        (cy - src.height() as f64 / 2.0).round() as i32,
    );
}
