//! 已安装 Web 应用的位图：小笺图标（manifest 图标或名称首字）、主屏名称、长按菜单面板；
//! 以及应用列表变化后的增量重画（见 `shell::webapps`）。

use std::hash::{DefaultHasher, Hash, Hasher};

use loong_ui::{
    Pixmap,
    ink::{self, WebIconArt, WebIconSpec},
    text::{FontRole, TextStyle},
    theme::{self, Ink, Theme},
    tiny_skia::{Color, Paint, Rect, Transform},
};

use super::{Assets, app_icon, draw, draw_centered, px};
use crate::shell::{
    Shell, TexKey,
    apps::{AppEntry, id_seed},
    layout::LABEL_HALO,
    webapps::{CONFIRM_BUTTONS_H, MENU_SHADOW, MenuButton, MenuStep, MenuView},
};

/// 主屏名称的最大宽度（占图标边长的倍数）：更长的名称截断加“…”。
const LABEL_MAX: f64 = 1.55;

impl Assets {
    /// 第 `index` 个应用的小笺：内置应用用笔触符号；已安装的应用用 manifest 图标，没有（或解不出）
    /// 时用名称首字。
    pub(super) fn app_icon_pixmap(&mut self, app: &AppEntry, index: usize, size: u32) -> Pixmap {
        let Some(web) = &app.web else {
            return app_icon(app, self.theme, size, index);
        };
        let seed = id_seed(&app.id);
        let decoded = web.icon.as_ref().and_then(|icon| {
            let side = if icon.maskable {
                size
            } else {
                (size as f32 * ink::ANY_ICON_SCALE).round().max(1.0) as u32
            };
            ink::decode_icon(&icon.data, side).map(|pm| (pm, icon.maskable))
        });
        if let Some((pixmap, maskable)) = decoded {
            return ink::web_app_icon(
                size,
                &WebIconSpec {
                    art: WebIconArt::Image {
                        pixmap: &pixmap,
                        maskable,
                    },
                    pigment: app.pigment,
                    theme: self.theme,
                    seed,
                },
            );
        }
        // 名称首字（拉丁字母大写），文楷。
        let first: String = app
            .name
            .chars()
            .find(|c| !c.is_whitespace())
            .map_or_else(|| "·".into(), |c| c.to_uppercase().collect());
        let glyph = self.text.render(
            &first,
            &TextStyle {
                family: FontRole::Display,
                size: size as f32 * 0.62,
                weight: 400,
                color: Color::BLACK,
                shadow: false,
            },
        );
        ink::web_app_icon(
            size,
            &WebIconSpec {
                art: WebIconArt::Glyph(&glyph),
                pigment: app.pigment,
                theme: self.theme,
                seed,
            },
        )
    }

    /// 主屏图标下的名称：亮色浓墨、暗色月白，四周一圈纸色光晕；太长时截断。
    pub(super) fn label_pixmap(&mut self, name: &str, icon: f64) -> Pixmap {
        let pal = self.theme.palette();
        let style = TextStyle {
            family: FontRole::Display,
            size: ((theme::font::ICON_LABEL + 1.0) as f64 * self.scale) as f32,
            weight: 400,
            color: match self.theme {
                Theme::Light => pal.ink(Ink::Nong),
                Theme::Dark => pal.ink(Ink::Jiao),
            },
            shadow: false,
        };
        let halo = theme::hex(
            pal.paper,
            if self.theme == Theme::Light {
                0.82
            } else {
                0.7
            },
        );
        let max = (icon * LABEL_MAX * self.scale) as f32;
        let text = self.fit_text(name, &style, max);
        self.text
            .render_halo(&text, &style, halo, (LABEL_HALO * self.scale) as f32)
    }

    /// 把文字截到 `max` 像素宽以内（末尾加“…”）。
    fn fit_text(&mut self, text: &str, style: &TextStyle, max: f32) -> String {
        if self.text.measure(text, style).0 <= max {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        for n in (1..chars.len()).rev() {
            let s: String = chars[..n].iter().collect::<String>().trim_end().to_string() + "…";
            if self.text.measure(&s, style).0 <= max {
                return s;
            }
        }
        "…".into()
    }

    /// 第 `i` 个应用的图标、名称、多任务标签（已移除的应用删掉这些位图）。
    pub(super) fn put_app_textures(&mut self, shell: &Shell, i: usize) {
        let app = &shell.apps[i];
        self.pixmaps.remove(&TexKey::Placeholder(i));
        self.pending_sheets.remove(&i);
        if app.removed {
            for key in [TexKey::Icon(i), TexKey::Label(i), TexKey::CardLabel(i)] {
                self.pixmaps.remove(&key);
            }
            return;
        }
        let icon_px = px(shell.layout.icon, self.scale);
        let icon = self.app_icon_pixmap(app, i, icon_px);
        self.put(TexKey::Icon(i), icon);
        let label = self.label_pixmap(&app.name, shell.layout.icon);
        self.put(TexKey::Label(i), label);
        let card_label = self.card_label(app, i);
        self.put(TexKey::CardLabel(i), card_label);
    }

    /// 应用列表、排列或菜单变了：只重画变了的应用；页数或内置图标的位置变了（整页山水里预先画好的
    /// 墨影不对了）就整个重建。
    pub(super) fn sync_apps(&mut self, shell: &Shell) {
        if shell.apps_revision() != self.apps_revision {
            if layout_print(shell) != self.layout_print {
                tracing::info!("主屏页数或内置图标的位置变了，重画主屏");
                self.build(shell);
            } else {
                for i in 0..shell.apps.len() {
                    let print = app_print(&shell.apps[i]);
                    if self.app_prints.get(i) != Some(&print) {
                        self.put_app_textures(shell, i);
                    }
                }
                self.app_prints = shell.apps.iter().map(app_print).collect();
                self.apps_revision = shell.apps_revision();
            }
        }
        if let Some(view) = shell.home_menu()
            && shell.menu_revision() != self.menu_revision
        {
            self.menu_revision = shell.menu_revision();
            let pm = self.menu_panel(shell, &view);
            self.put(TexKey::HomeMenu, pm);
        }
    }

    /// 记下这一版应用列表与排列（[`Assets::build`] 结束时调用）。
    pub(super) fn record_apps(&mut self, shell: &Shell) {
        self.app_prints = shell.apps.iter().map(app_print).collect();
        self.apps_revision = shell.apps_revision();
        self.layout_print = layout_print(shell);
        // 换了外观：菜单面板也要重画。
        self.menu_revision = u64::MAX;
    }

    /// 长按菜单的面板：墨影 + 绢面 + 内容（一行“移除应用”，或确认一步）。
    fn menu_panel(&mut self, shell: &Shell, view: &MenuView) -> Pixmap {
        let s = self.scale;
        let theme = self.theme;
        let pal = theme.palette();
        let pad = px(MENU_SHADOW, s);
        let (pw, ph) = (px(view.rect.w, s), px(view.rect.h, s));
        let mut pm = loong_ui::pixmap(pw + 2 * pad, ph + 2 * pad);
        let radius = (theme::radius::CARD as f64 * s) as f32;
        let shadow = ink::ink_shadow(theme, pw, ph, radius, pad);
        draw(&mut pm, &shadow, 0, 0);
        // 绢：取菜单底下那块山水模糊（停在哪页都用第一页的，模糊后看不出差别）。
        let panel_rect = Rect::from_xywh(
            (view.rect.x * s) as f32,
            (view.rect.y * s) as f32,
            pw as f32,
            ph as f32,
        )
        .unwrap_or_else(|| Rect::from_xywh(0.0, 0.0, pw as f32, ph as f32).expect("尺寸有效"));
        let silk = ink::silk_panel(theme, &self.backdrop, panel_rect, radius);
        draw(&mut pm, &silk, pad as i32, pad as i32);

        let o = pad as f32;
        let sf = s as f32;
        let zhusha = theme::hex(theme::pigment::ZHUSHA, 1.0);
        let style = |size: f32, family: FontRole, color: Color| TextStyle {
            family,
            size: size * sf,
            weight: 500,
            color,
            shadow: false,
        };
        let name = shell.apps.get(view.app).map_or("", |a| a.name.as_str());
        match view.step {
            MenuStep::Actions => {
                // 笔触符号“废纸篓” + “移除应用”（朱砂：警示）。
                let r = view.buttons[0].1;
                let sym = 22.0 * sf;
                let sx = o + 20.0 * sf;
                let sy = o + ((r.y + r.h / 2.0) as f32) * sf - sym / 2.0;
                if let (Some(svg), Some(rect)) = (
                    loong_ui::icon::builtin_symbol("trash-2"),
                    Rect::from_xywh(sx, sy, sym, sym),
                ) {
                    ink::brush_symbol(&mut pm, svg, rect, zhusha, 1.0, 7);
                }
                let text = self
                    .text
                    .render("移除应用", &style(17.0, FontRole::Display, zhusha));
                let tx = sx + sym + 14.0 * sf;
                let ty = o + ((r.y + r.h / 2.0) as f32) * sf - text.height() as f32 / 2.0;
                draw(&mut pm, &text, tx as i32, ty as i32);
            }
            MenuStep::Confirm => {
                let cx = o + (view.rect.w as f32) * sf / 2.0;
                let title_style = style(19.0, FontRole::Display, pal.ink(Ink::Jiao));
                let short = self.fit_text(name, &title_style, (view.rect.w as f32 - 120.0) * sf);
                let title = self
                    .text
                    .render(&format!("移除「{short}」？"), &title_style);
                draw_centered(&mut pm, &title, cx, o + 20.0 * sf);
                let message = self.text.render(
                    "它的数据也会一并删除。",
                    &style(14.0, FontRole::Body, pal.ink(Ink::Zhong)),
                );
                draw_centered(&mut pm, &message, cx, o + 58.0 * sf);
                // 分隔：一道淡墨细线，两个按钮之间一道短竖线。
                let line_y = o + (view.rect.h - CONFIRM_BUTTONS_H) as f32 * sf;
                let mut paint = Paint {
                    anti_alias: true,
                    ..Paint::default()
                };
                paint.set_color(theme::hex(pal.ink[3], 0.45));
                let hair = (1.0 * sf).max(1.0);
                let inset = 14.0 * sf;
                if let Some(rect) =
                    Rect::from_xywh(o + inset, line_y, pw as f32 - 2.0 * inset, hair)
                {
                    pm.fill_rect(rect, &paint, Transform::identity(), None);
                }
                if let Some(rect) = Rect::from_xywh(
                    cx - hair / 2.0,
                    line_y + 10.0 * sf,
                    hair,
                    (CONFIRM_BUTTONS_H as f32 - 20.0) * sf,
                ) {
                    pm.fill_rect(rect, &paint, Transform::identity(), None);
                }
                for (button, r) in &view.buttons {
                    let (label, color) = match button {
                        MenuButton::Cancel => ("取消", pal.ink(Ink::Nong)),
                        _ => ("移除", zhusha),
                    };
                    let text = self
                        .text
                        .render(label, &style(17.0, FontRole::Display, color));
                    let bx = o + (r.x + r.w / 2.0) as f32 * sf;
                    let by = o + (r.y + r.h / 2.0) as f32 * sf - text.height() as f32 / 2.0;
                    draw_centered(&mut pm, &text, bx, by);
                }
            }
        }
        pm
    }
}

/// 应用的“指纹”：名称、符号、颜料、图标内容、是否移除——变了就要重画它的位图。
pub(super) fn app_print(app: &AppEntry) -> u64 {
    let mut h = DefaultHasher::new();
    (&app.name, &app.symbol, app.pigment, app.removed).hash(&mut h);
    if let Some(web) = &app.web {
        web.icon
            .as_ref()
            .map(|i| (&i.data[..], i.maskable))
            .hash(&mut h);
    }
    h.finish()
}

/// 排列的“指纹”：页数，以及墨影预先画在整页山水里的那些图标（内置应用）的位置。
pub(super) fn layout_print(shell: &Shell) -> u64 {
    let mut h = DefaultHasher::new();
    shell.layout.pages.hash(&mut h);
    for (i, r) in shell.layout.icons.iter().enumerate() {
        if shell.layout.shown[i] && shell.layout.page_of[i].is_some() && shell.shadow_baked(i) {
            (i, r.x.to_bits(), r.y.to_bits()).hash(&mut h);
        }
    }
    h.finish()
}
