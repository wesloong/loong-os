//! 把 [`Scene`](crate::shell::Scene) 变成 GPU 渲染元素。
//!
//! - [`Assets`]：在 CPU 上用 `loong_ui::ink` 把山水、图标、文字、印章等“画”成位图（只做一次，
//!   切换外观时重画）。
//! - [`Painter`]：把位图上传为纹理，并按场景生成 Smithay 渲染元素；水墨印记（点墨、笔迹）
//!   与晕染 / 墨散遮罩由着色器现算。

pub mod element;

use std::collections::HashMap;

use loong_ui::{
    Pixmap, icon,
    ink::{self, IconSpec},
    text::{FontRole, TextRenderer, TextStyle},
    theme::{self, Ink, Theme},
    tiny_skia::{Color, FillRule, Paint, PixmapPaint, Rect, SpreadMode, Transform},
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportMem, Texture,
            element::{
                AsRenderElements, Id, surface::WaylandSurfaceRenderElement,
                utils::RescaleRenderElement,
            },
            gles::{GlesError, GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture},
            utils::CommitCounter,
        },
    },
    desktop::Window,
    utils::{Physical, Point, Rectangle, Scale, Size},
};

use self::element::{
    InkElement, LayerElement, RevealUniforms, RoundedSurface, ShellElement, SurfaceContent,
};
use crate::shell::{
    Align, DrawItem, InkDab, Reveal, RevealMode, Scene, Shell, TexKey,
    apps::AppEntry,
    geometry::{RectF, WindowPlacement},
    ink::DabKind,
    layout::{ICON_SHADOW_SPREAD, LABEL_HALO},
    switcher::{self, SwitcherLayout},
};

/// 山水长卷的种子（固定：每次开机同一幅画）。
const LANDSCAPE_SEED: u32 = 0x5EED;
/// 视差系数不小于该值的层是烟云（远 0.15 / 中 0.35 / 近 0.6 / 烟 0.8），随翻页速度额外漂移。
const MIST_PARALLAX: f32 = 0.7;
/// 锁屏印章的字。
const SEAL_TEXT: &str = "龙";

/// CPU 端位图资源（物理像素）。
pub struct Assets {
    scale: f64,
    theme: Theme,
    text: TextRenderer,
    pixmaps: HashMap<TexKey, (Pixmap, u64)>,
    version: u64,
    clock_text: String,
    date_text: String,
    /// 山水各层（第 i 个对应 [`TexKey::WallpaperLayer`]`(i)`）。
    layers: Vec<LayerInfo>,
    /// 各层合成后的整屏山水（滚动位置 0），锁屏、Dock、多任务背景都由它而来。
    backdrop: Pixmap,
    /// 正在后台线程中画的「册页」（应用下标 → 线程）。手指按下图标时就开始画，
    /// 抬起打开应用时多半已经画好，首次打开不卡顿。
    pending_sheets: HashMap<usize, std::thread::JoinHandle<Pixmap>>,
}

impl Assets {
    /// 生成主屏所需的全部位图。
    pub fn new(shell: &Shell, scale: f64) -> Self {
        let mut assets = Assets {
            scale,
            theme: shell.theme,
            text: TextRenderer::new(),
            pixmaps: HashMap::new(),
            version: 0,
            clock_text: String::new(),
            date_text: String::new(),
            layers: Vec::new(),
            backdrop: loong_ui::pixmap(1, 1),
            pending_sheets: HashMap::new(),
        };
        assets.build(shell);
        assets
    }

    /// 外观改变时重画全部位图；时间、日期改变时重画状态栏与锁屏。
    pub fn sync(&mut self, shell: &Shell) {
        if shell.theme != self.theme {
            tracing::info!("切换外观：{:?}", shell.theme);
            self.build(shell);
        }
        self.refresh_clock(shell);
    }

    /// 山水各层的视差系数与纵向位置。
    pub fn layers(&self) -> &[LayerInfo] {
        &self.layers
    }

    fn build(&mut self, shell: &Shell) {
        // 旧外观的所有位图（含按需生成的占位画面）作废；版本号递增，纹理会重新上传。
        self.pixmaps.clear();
        // 旧外观的册页作废（线程分离，结果丢弃）。
        self.pending_sheets.clear();
        self.clock_text.clear();
        self.date_text.clear();
        self.theme = shell.theme;
        let theme = self.theme;
        let scale = self.scale;
        let (w, h) = shell.screen_size();
        let (pw, ph) = (px(w, scale), px(h, scale));

        let layers = ink::landscape(theme, pw, ph, shell.layout.pages, LANDSCAPE_SEED);
        let backdrop = ink::composite(&layers, pw, ph, 0.0);
        let icon_px = px(shell.layout.icon, scale);
        let spread = px(shell.layout.icon * ICON_SHADOW_SPREAD, scale);
        let icon_shadow = ink::ink_shadow(
            theme,
            icon_px,
            icon_px,
            icon_px as f32 * theme::radius::ICON_RATIO * 0.96,
            spread,
        );
        // 每页停稳时的整幅画（山水 + 这一页网格图标的墨影）：主屏静止、开关应用、解锁时
        // 只需画一张不透明纹理，而不是逐层混合、逐个画墨影。
        for p in 0..shell.layout.pages {
            let mut page = if p == 0 {
                backdrop.clone()
            } else {
                ink::composite(&layers, pw, ph, p as f32 * pw as f32)
            };
            let l = &shell.layout;
            let s = l.icon * ICON_SHADOW_SPREAD;
            for (r, _) in l
                .icons
                .iter()
                .zip(&l.page_of)
                .filter(|(_, pg)| **pg == Some(p))
            {
                let x = ((r.x - p as f64 * l.page_width - s) * scale).round() as i32;
                let y = ((r.y - s) * scale).round() as i32;
                draw(&mut page, &icon_shadow, x, y);
            }
            self.put(TexKey::LandscapePage(p), page);
        }
        self.put(TexKey::IconShadow, icon_shadow);
        self.layers.clear();
        for (i, layer) in layers.into_iter().enumerate() {
            // 除最底下的天色外，各层上下大片透明（山在下半部、烟云是一条带）：只保留有墨的行，
            // 每帧少混合大量透明像素。
            let (pixmap, top) = if i == 0 {
                (layer.pixmap, 0)
            } else {
                crop_rows(layer.pixmap)
            };
            self.layers.push(LayerInfo {
                parallax: layer.parallax,
                top: top as f64 / scale,
            });
            self.put(TexKey::WallpaperLayer(i), pixmap);
        }

        let d = shell.layout.dock;
        let dock_rect = Rect::from_xywh(
            (d.x * scale) as f32,
            (d.y * scale) as f32,
            (d.w * scale) as f32,
            (d.h * scale) as f32,
        )
        .expect("Dock 尺寸有效");
        let dock = ink::silk_panel(
            theme,
            &backdrop,
            dock_rect,
            (theme::radius::DOCK as f64 * scale) as f32,
        );
        self.put(TexKey::Dock, dock);
        self.put(
            TexKey::SwitcherBackdrop,
            switcher_backdrop(&backdrop, theme, scale),
        );
        self.put(
            TexKey::CardShadow,
            card_shadow(&shell.switcher, theme, scale),
        );
        let pal = theme.palette();
        self.put(TexKey::PageDot, page_dot(pal.ink[0], scale));
        // 当前页：亮色浓墨，暗色泥金（暗色外观中“激活”的控件用泥金）。
        let active = match theme {
            Theme::Light => pal.ink[0],
            Theme::Dark => theme::pigment::NIJIN,
        };
        self.put(TexKey::PageDotActive, page_dot(active, scale));

        // 图标名称：亮色浓墨、暗色月白，四周一圈纸色光晕（落在浓墨山体上也读得清）。
        let label_style = TextStyle {
            family: FontRole::Display,
            size: ((theme::font::ICON_LABEL + 1.0) as f64 * scale) as f32,
            weight: 400,
            color: match theme {
                Theme::Light => pal.ink(Ink::Nong),
                Theme::Dark => pal.ink(Ink::Jiao),
            },
            shadow: false,
        };
        let halo = theme::hex(pal.paper, if theme == Theme::Light { 0.82 } else { 0.7 });
        let halo_px = (LABEL_HALO * scale) as f32;
        for (i, app) in shell.apps.iter().enumerate() {
            self.put(TexKey::Icon(i), app_icon(app, theme, icon_px, i));
            let label = self
                .text
                .render_halo(&app.name, &label_style, halo, halo_px);
            self.put(TexKey::Label(i), label);
            let card_label = self.card_label(app, i);
            self.put(TexKey::CardLabel(i), card_label);
        }
        let status_icons = self.status_icons();
        self.put(TexKey::StatusIcons, status_icons);
        self.backdrop = backdrop;
        self.refresh_clock(shell);
    }

    fn put(&mut self, key: TexKey, pixmap: Pixmap) {
        self.version += 1;
        self.pixmaps.insert(key, (pixmap, self.version));
    }

    fn ink(&self, level: Ink) -> Color {
        self.theme.palette().ink(level)
    }

    /// 多任务卡片上方的标签：24pt 宣纸小笺图标 + 8pt 间距 + 名称（文楷，墨色）。
    /// 位图高度固定为 [`switcher::LABEL_HEIGHT`]，图标垂直居中，便于 shell 排版。
    fn card_label(&mut self, app: &AppEntry, index: usize) -> Pixmap {
        let s = self.scale;
        let icon_px = px(switcher::LABEL_ICON, s);
        let icon = app_icon(app, self.theme, icon_px, index);
        let text = self.text.render(
            &app.name,
            &TextStyle {
                family: FontRole::Display,
                size: (16.0 * s) as f32,
                weight: 500,
                color: self.ink(Ink::Nong),
                shadow: false,
            },
        );
        // 文字位图四周留有 1 像素边距（与 TextRenderer::render 一致），这里扣掉，保证视觉间距为 8pt。
        let pad = 1;
        let text_x = icon_px as i32 + (8.0 * s).round() as i32 - pad;
        let h = px(switcher::LABEL_HEIGHT, s);
        let w = (text_x + text.width() as i32).max(icon_px as i32) as u32;
        let mut pm = loong_ui::pixmap(w, h);
        draw(&mut pm, &icon, 0, (h as i32 - icon_px as i32) / 2);
        draw(
            &mut pm,
            &text,
            text_x,
            (h as i32 - text.height() as i32) / 2,
        );
        pm
    }

    fn status_style(&self, size: f32) -> TextStyle {
        TextStyle {
            family: FontRole::Display,
            size: size * self.scale as f32,
            weight: 500,
            color: self.ink(Ink::Nong),
            shadow: false,
        }
    }

    /// 状态栏右侧：毛笔画的 Wi‑Fi、电量百分比（文楷）、毛笔画的电池。
    fn status_icons(&mut self) -> Pixmap {
        let s = self.scale as f32;
        let percent = self.text.render("100%", &self.status_style(14.0));
        let wifi = ink::wifi(self.theme, (16.0 * s).round() as u32, 3);
        let battery = ink::battery(self.theme, (13.0 * s).round() as u32, 1.0, false);
        let gap = (6.0 * s).round() as i32;
        let w = wifi.width() as i32 + gap + percent.width() as i32 + gap + battery.width() as i32;
        let h = percent.height().max(wifi.height()).max(battery.height()) as i32;
        let mut pm = loong_ui::pixmap(w as u32, h as u32);
        let mut x = 0;
        for part in [&wifi, &percent, &battery] {
            draw(&mut pm, part, x, (h - part.height() as i32) / 2);
            x += part.width() as i32 + gap;
        }
        pm
    }

    /// 时间、日期变化时重新生成状态栏时间与锁屏。
    pub fn refresh_clock(&mut self, shell: &Shell) {
        if self.clock_text == shell.clock_text && self.date_text == shell.date_text {
            return;
        }
        self.clock_text = shell.clock_text.clone();
        self.date_text = shell.date_text.clone();
        let style = self.status_style(15.0);
        let pm = self.text.render(&self.clock_text, &style);
        self.put(TexKey::StatusTime, pm);
        let sheet = self.lock_sheet();
        self.put(TexKey::LockSheet, sheet);
    }

    /// 锁屏整张“纸”：山水 + 一层薄纸色罩染 + 农历日期 + 大号文楷时间 + 角落的朱砂印章 +
    /// “上滑解锁”提示 + 墨色指示条。
    fn lock_sheet(&mut self) -> Pixmap {
        let s = self.scale as f32;
        let theme = self.theme;
        let pal = theme.palette();
        let mut pm = self.backdrop.clone();
        let (pw, ph) = (pm.width() as f32, pm.height() as f32);
        let all = Rect::from_xywh(0.0, 0.0, pw, ph).expect("尺寸有效");
        let mut veil = Paint::default();
        veil.set_color(theme::hex(
            pal.paper,
            if theme == Theme::Light { 0.32 } else { 0.38 },
        ));
        pm.fill_rect(all, &veil, Transform::identity(), None);

        let date = self.text.render(
            &self.date_text,
            &TextStyle {
                family: FontRole::Display,
                size: 22.0 * s,
                weight: 500,
                color: pal.ink(Ink::Zhong),
                shadow: false,
            },
        );
        let time = self.text.render(
            &self.clock_text,
            &TextStyle {
                family: FontRole::Display,
                size: 112.0 * s,
                weight: 400,
                color: pal.ink(Ink::Jiao),
                shadow: false,
            },
        );
        let top = ph * 0.13;
        draw_centered(&mut pm, &date, pw / 2.0, top);
        draw_centered(&mut pm, &time, pw / 2.0, top + 28.0 * s);

        let seal_px = (40.0 * s).round() as u32;
        let seal = ink::seal(theme, seal_px, SEAL_TEXT, 9);
        let margin = (36.0 * s).round() as i32;
        draw(
            &mut pm,
            &seal,
            pw as i32 - margin - seal.width() as i32,
            ph as i32 - margin - seal.height() as i32,
        );

        let hint = self.text.render(
            "上滑解锁",
            &TextStyle {
                family: FontRole::Body,
                size: 15.0 * s,
                weight: 500,
                color: pal.ink(Ink::Dan),
                shadow: false,
            },
        );
        draw_centered(&mut pm, &hint, pw / 2.0, ph - 46.0 * s);
        let pill = Rect::from_xywh(pw / 2.0 - 70.0 * s, ph - 11.5 * s, 140.0 * s, 5.0 * s)
            .expect("尺寸有效");
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(theme::hex(pal.ink[1], 0.7));
        pm.fill_path(
            &loong_ui::shape::continuous_rect(pill, 2.5 * s),
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        pm
    }

    /// 手指按在某个演示应用的图标上：在后台线程开始画它的「册页」。
    pub fn prefetch(&mut self, shell: &Shell) {
        let Some(app) = shell.pressed_app() else {
            return;
        };
        let entry = &shell.apps[app];
        if entry.command.is_some()
            || self.pixmaps.contains_key(&TexKey::Placeholder(app))
            || self.pending_sheets.contains_key(&app)
        {
            return;
        }
        let (theme, pigment) = (self.theme, entry.pigment);
        let (w, h) = shell.screen_size();
        let (pw, ph) = (px(w, self.scale), px(h, self.scale));
        let svg = symbol_svg(&entry.symbol);
        let seed = sheet_seed(app);
        self.pending_sheets.insert(
            app,
            std::thread::spawn(move || ink::app_sheet(theme, pw, ph, svg, pigment, seed)),
        );
    }

    /// 取位图（占位画面按需生成）。
    pub fn get(&mut self, key: TexKey, shell: &Shell) -> Option<&(Pixmap, u64)> {
        if let TexKey::Placeholder(app) = key
            && !self.pixmaps.contains_key(&key)
        {
            let pm = self.placeholder(shell, app);
            self.put(key, pm);
        }
        self.pixmaps.get(&key)
    }

    /// 演示应用的占位画面：「册页」（纸、大片淡彩、大幅笔触符号、远山、小印，见
    /// [`ink::app_sheet`]），顶部留白处写文楷标题与一行小字。
    fn placeholder(&mut self, shell: &Shell, app: usize) -> Pixmap {
        let theme = self.theme;
        let pal = theme.palette();
        let s = self.scale as f32;
        let (w, h) = shell.screen_size();
        let (pw, ph) = (px(w, self.scale), px(h, self.scale));
        let entry = &shell.apps[app];
        // 后台已经在画就等它画完（通常已完成）；线程出错或没有预取时就地画。
        let mut pm = match self.pending_sheets.remove(&app).map(|t| t.join()) {
            Some(Ok(sheet)) => sheet,
            _ => ink::app_sheet(
                theme,
                pw,
                ph,
                symbol_svg(&entry.symbol),
                entry.pigment,
                sheet_seed(app),
            ),
        };
        let title = self.text.render(
            &entry.name,
            &TextStyle {
                family: FontRole::Display,
                size: 40.0 * s,
                weight: 500,
                color: pal.ink(Ink::Jiao),
                shadow: false,
            },
        );
        let sub = self.text.render(
            "Loong OS · 演示应用",
            &TextStyle {
                family: FontRole::Body,
                size: 14.0 * s,
                weight: 400,
                color: pal.ink(Ink::Dan),
                shadow: false,
            },
        );
        // 标题与小字一起在顶部 22% 的留白中垂直居中。
        let block = title.height() as f32 + 4.0 * s + sub.height() as f32;
        let top = (ph as f32 * 0.22 - block) / 2.0 + 6.0 * s;
        draw_centered(&mut pm, &title, pw as f32 / 2.0, top);
        draw_centered(
            &mut pm,
            &sub,
            pw as f32 / 2.0,
            top + title.height() as f32 + 4.0 * s,
        );
        pm
    }
}

fn px(v: f64, scale: f64) -> u32 {
    (v * scale).round().max(1.0) as u32
}

/// 把 `src` 原样（source-over）画到 `dst` 的 `(x, y)`。
fn draw(dst: &mut Pixmap, src: &Pixmap, x: i32, y: i32) {
    dst.draw_pixmap(
        x,
        y,
        src.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}

/// 水平居中画到 `cx`，顶边在 `y`。
fn draw_centered(dst: &mut Pixmap, src: &Pixmap, cx: f32, y: f32) {
    draw(dst, src, (cx - src.width() as f32 / 2.0) as i32, y as i32);
}

/// 内置符号（找不到时用罗盘）。
fn symbol_svg(name: &str) -> &'static str {
    icon::builtin_symbol(name).unwrap_or(icon::builtin_symbol("compass").expect("内置图标"))
}

/// 第 `app` 个应用的册页种子。
fn sheet_seed(app: usize) -> u32 {
    100 + app as u32
}

/// 宣纸小笺图标。
fn app_icon(app: &AppEntry, theme: Theme, size: u32, index: usize) -> Pixmap {
    let svg = symbol_svg(&app.symbol);
    ink::app_icon(
        size,
        &IconSpec {
            symbol_svg: svg,
            pigment: app.pigment,
            theme,
            seed: 31 + index as u32 * 17,
        },
    )
}

/// 山水的一层：视差系数，以及（裁掉上下透明行后）在屏幕上的顶边（点）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerInfo {
    pub parallax: f32,
    pub top: f64,
}

/// 裁掉位图上下完全透明的行，返回（裁剪后的位图，顶边行号）。全透明时保留一行。
fn crop_rows(pixmap: Pixmap) -> (Pixmap, u32) {
    let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
    let row_has_ink = |y: usize| {
        pixmap.data()[y * w * 4..(y + 1) * w * 4]
            .chunks_exact(4)
            .any(|p| p[3] != 0)
    };
    let Some(top) = (0..h).find(|&y| row_has_ink(y)) else {
        return (loong_ui::pixmap(w as u32, 1), 0);
    };
    let bottom = (0..h).rev().find(|&y| row_has_ink(y)).unwrap_or(top) + 1;
    if top == 0 && bottom == h {
        return (pixmap, 0);
    }
    let mut out = loong_ui::pixmap(w as u32, (bottom - top) as u32);
    out.data_mut()
        .copy_from_slice(&pixmap.data()[top * w * 4..bottom * w * 4]);
    (out, top as u32)
}

/// 主屏页面指示点：一点墨（颜色 `rgb`），边缘略有起伏。
fn page_dot(rgb: u32, scale: f64) -> Pixmap {
    let d = px(crate::shell::layout::PAGE_DOT, scale);
    let mut pm = loong_ui::pixmap(d, d);
    let c = theme::hex(rgb, 1.0);
    let r0 = d as f32 * 0.44;
    let mid = d as f32 / 2.0;
    let data = pm.data_mut();
    for y in 0..d {
        for x in 0..d {
            let (dx, dy) = (x as f32 + 0.5 - mid, y as f32 + 0.5 - mid);
            let dist = (dx * dx + dy * dy).sqrt();
            let (ux, uy) = (dx / dist.max(0.01), dy / dist.max(0.01));
            let wobble = ink::noise::value(ux * 1.5 + 3.0, uy * 1.5 + 3.0, 21);
            let r = r0 * (0.88 + 0.22 * wobble);
            let grain = 0.86 + 0.14 * ink::noise::value(x as f32 * 0.7, y as f32 * 0.7, 5);
            let a = ((r - dist) / (0.9 * scale as f32) + 0.5).clamp(0.0, 1.0) * grain;
            let i = ((y * d + x) * 4) as usize;
            data[i] = (c.red() * a * 255.0).round() as u8;
            data[i + 1] = (c.green() * a * 255.0).round() as u8;
            data[i + 2] = (c.blue() * a * 255.0).round() as u8;
            data[i + 3] = (a * 255.0).round() as u8;
        }
    }
    pm
}

/// 模糊类位图在 CPU 上以 1/4 分辨率生成，GPU 绘制时再放大：模糊后的画面放大看不出差别。
const BLUR_DOWNSAMPLE: f64 = 4.0;

/// 多任务背景：山水虚化（模糊 σ 24pt）+ 一层烟云罩染（亮色为纸色，暗色为夜色）。
/// 只在启动或切换外观时算一次。
fn switcher_backdrop(backdrop: &Pixmap, theme: Theme, scale: f64) -> Pixmap {
    use loong_ui::tiny_skia::{FilterQuality, Pattern};
    let k = (1.0 / BLUR_DOWNSAMPLE) as f32;
    let w = (backdrop.width() as f32 * k).ceil() as u32;
    let h = (backdrop.height() as f32 * k).ceil() as u32;
    let mut small = loong_ui::pixmap(w, h);
    let all = Rect::from_xywh(0.0, 0.0, w as f32, h as f32).expect("尺寸有效");
    let paint = Paint {
        shader: Pattern::new(
            backdrop.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Bilinear,
            1.0,
            Transform::from_scale(k, k),
        ),
        ..Paint::default()
    };
    small.fill_rect(all, &paint, Transform::identity(), None);
    // 三次盒式模糊的半径约等于高斯 σ。
    let radius = (24.0 * scale / BLUR_DOWNSAMPLE).round().max(1.0) as usize;
    loong_ui::material::blur(&mut small, radius);
    let mut tint = Paint::default();
    tint.set_color(match theme {
        Theme::Light => theme::hex(theme.palette().paper, 0.42),
        Theme::Dark => theme::hex(0x0B0A08, 0.45),
    });
    small.fill_rect(all, &tint, Transform::identity(), None);
    small
}

/// 多任务卡片的墨影，按标准卡片尺寸以 1/4 分辨率生成，四周各留 [`switcher::SHADOW_SPREAD`]；
/// 向下的偏移由 shell 排版时加上。
fn card_shadow(layout: &SwitcherLayout, theme: Theme, scale: f64) -> Pixmap {
    let k = scale / BLUR_DOWNSAMPLE;
    ink::ink_shadow(
        theme,
        px(layout.card_w, k),
        px(layout.card_h, k),
        (switcher::CARD_RADIUS * k) as f32,
        px(switcher::SHADOW_SPREAD, k),
    )
}

/// 山水长卷第 i 层在屏幕上的矩形（逻辑点）：`viewport` 为屏幕可视区域（随主屏缩放），
/// 层的大小 `layer_size`（宽、高，高度为裁剪后的）、顶边 `layer.top`；烟云层再多漂移 `drift`。
fn layer_rect(
    viewport: RectF,
    scroll: f64,
    drift: f64,
    layer: LayerInfo,
    layer_size: (f64, f64),
    screen_w: f64,
) -> RectF {
    let LayerInfo { parallax, top } = layer;
    let (layer_w, layer_h) = layer_size;
    let k = viewport.w / screen_w;
    let base = parallax as f64 * scroll;
    // 烟云的额外漂移不能让这一层露出边缘：在两端按剩余的余量做软限制（tanh，连续可导）。
    let mist = if parallax >= MIST_PARALLAX {
        let room = if drift > 0.0 {
            (layer_w - screen_w - base).max(0.0)
        } else {
            base.max(0.0)
        };
        if room > 1e-6 {
            room * (drift / room).tanh()
        } else {
            0.0
        }
    } else {
        0.0
    };
    let x = -(base + mist);
    RectF::new(
        viewport.x + x * k,
        viewport.y + top * k,
        layer_w * k,
        layer_h * k,
    )
}

/// 逻辑矩形 → 物理像素矩形（边缘取整，保证相邻元素无缝）。
fn to_physical(r: RectF, scale: f64) -> Rectangle<i32, Physical> {
    let x0 = (r.x * scale).round() as i32;
    let y0 = (r.y * scale).round() as i32;
    let x1 = ((r.x + r.w) * scale).round() as i32;
    let y1 = ((r.y + r.h) * scale).round() as i32;
    Rectangle::new(
        Point::from((x0, y0)),
        Size::from(((x1 - x0).max(1), (y1 - y0).max(1))),
    )
}

/// 0xRRGGBB → 预乘 RGBA（不透明度 `a`）。
fn premul(rgb: u32, a: f32) -> [f32; 4] {
    let c = theme::hex(rgb, 1.0);
    [c.red() * a, c.green() * a, c.blue() * a, a]
}

struct UploadedTexture {
    texture: GlesTexture,
    id: Id,
    version: u64,
}

/// GPU 端：纹理缓存 + 生成渲染元素。
pub struct Painter {
    scale: f64,
    program: GlesTexProgram,
    ink_program: GlesPixelProgram,
    textures: HashMap<TexKey, UploadedTexture>,
    white: GlesTexture,
    solid_ids: Vec<Id>,
    ink_ids: Vec<Id>,
    /// 帧计数：水墨印记每帧都在变化，用它作为提交计数，保证重画。
    frame: usize,
    /// 带遮罩绘制的纹理（遮罩每帧都在变，需要重画；遮罩结束的那一帧也要重画一次）。
    revealed: HashMap<TexKey, (bool, CommitCounter)>,
    /// 上一帧带遮罩绘制的窗口（很少，用 Vec）。
    revealed_windows: Vec<Window>,
}

impl Painter {
    pub fn new(renderer: &mut GlesRenderer, scale: f64) -> Result<Self, GlesError> {
        let program = element::compile_program(renderer)?;
        let ink_program = element::compile_ink_program(renderer)?;
        let white = renderer.import_memory(
            &[255, 255, 255, 255],
            Fourcc::Abgr8888,
            (1, 1).into(),
            false,
        )?;
        Ok(Painter {
            scale,
            program,
            ink_program,
            textures: HashMap::new(),
            white,
            solid_ids: Vec::new(),
            ink_ids: Vec::new(),
            frame: 0,
            revealed: HashMap::new(),
            revealed_windows: Vec::new(),
        })
    }

    fn texture(
        &mut self,
        renderer: &mut GlesRenderer,
        key: TexKey,
        assets: &mut Assets,
        shell: &Shell,
    ) -> Result<Option<(GlesTexture, Id)>, GlesError> {
        let Some((pixmap, version)) = assets.get(key, shell) else {
            return Ok(None);
        };
        let stale = self
            .textures
            .get(&key)
            .is_none_or(|t| t.version != *version);
        if stale {
            // tiny-skia 的预乘 RGBA 字节序 = DRM 的 ABGR8888（小端）。
            let texture = renderer.import_memory(
                pixmap.data(),
                Fourcc::Abgr8888,
                (pixmap.width() as i32, pixmap.height() as i32).into(),
                false,
            )?;
            self.textures.insert(
                key,
                UploadedTexture {
                    texture,
                    id: Id::new(),
                    version: *version,
                },
            );
        }
        let t = &self.textures[&key];
        Ok(Some((t.texture.clone(), t.id.clone())))
    }

    /// 遮罩 → 着色器参数（物理像素）。前沿积墨用焦墨。
    fn reveal_uniforms(&self, reveal: Option<&Reveal>, theme: Theme) -> RevealUniforms {
        let Some(r) = reveal else {
            return RevealUniforms::NONE;
        };
        let s = self.scale as f32;
        let (mode, strength) = match r.mode {
            RevealMode::Spread => (1.0, if theme == Theme::Light { 0.55 } else { 0.4 }),
            RevealMode::Dissolve => (2.0, if theme == Theme::Light { 0.16 } else { 0.12 }),
        };
        let ink = premul(ink::stroke_ink(theme), 1.0);
        RevealUniforms {
            reveal: [
                r.center.0 as f32 * s,
                r.center.1 as f32 * s,
                r.amount as f32 * s,
                r.softness as f32 * s,
            ],
            mode: [mode, r.seed, r.amount as f32, s],
            ink: [ink[0], ink[1], ink[2], strength],
        }
    }

    /// 带遮罩绘制的纹理每帧都要重画；遮罩结束的那一帧再重画一次，之后恢复按内容变化重画。
    fn reveal_commit(&mut self, key: TexKey, active: bool) -> CommitCounter {
        let entry = self
            .revealed
            .entry(key)
            .or_insert((false, CommitCounter::default()));
        if active || entry.0 {
            entry.1.increment();
        }
        entry.0 = active;
        entry.1
    }

    /// 水墨印记 → 渲染元素。
    fn ink_element(&mut self, dab: &InkDab, index: usize, theme: Theme) -> InkElement {
        if self.ink_ids.len() <= index {
            self.ink_ids.push(Id::new());
        }
        let s = self.scale;
        let geometry = to_physical(dab.bounds(), s);
        let (ox, oy) = (geometry.loc.x as f64, geometry.loc.y as f64);
        let local = |(x, y): (f64, f64)| [(x * s - ox) as f32, (y * s - oy) as f32];
        let [ax, ay] = local(dab.a);
        let [bx, by] = local(dab.b);
        let kind = match dab.kind {
            DabKind::Drop => 0.0,
            DabKind::Stroke => 1.0,
        };
        InkElement {
            id: self.ink_ids[index].clone(),
            commit: CommitCounter::from(self.frame),
            geometry,
            seg: [ax, ay, bx, by],
            shape: [
                (dab.ra * s) as f32,
                (dab.rb * s) as f32,
                dab.along as f32,
                s as f32,
            ],
            params: [kind, dab.density as f32, dab.dry as f32, dab.seed],
            ink: premul(ink::stroke_ink(theme), 1.0),
            caps: [
                f32::from(u8::from(dab.caps.0)),
                f32::from(u8::from(dab.caps.1)),
            ],
            program: self.ink_program.clone(),
        }
    }

    /// 按场景生成渲染元素（返回顺序：最上层在前，符合 Smithay 约定）。
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        scene: &Scene,
        assets: &mut Assets,
        shell: &Shell,
    ) -> Result<Vec<ShellElement>, GlesError> {
        assets.sync(shell);
        assets.prefetch(shell);
        self.frame = self.frame.wrapping_add(1);
        let theme = shell.theme;
        let scale = self.scale;
        let mut out: Vec<ShellElement> = Vec::with_capacity(scene.len() + 8);
        let mut solid_index = 0;
        let mut ink_index = 0;
        let mut revealed_windows = Vec::new();
        for item in scene {
            match item {
                DrawItem::Texture {
                    key,
                    rect,
                    alpha,
                    radius,
                    clip,
                    reveal,
                } => {
                    if *alpha <= 0.001 {
                        continue;
                    }
                    if let Some((texture, id)) = self.texture(renderer, *key, assets, shell)? {
                        let uniforms = self.reveal_uniforms(reveal.as_ref(), theme);
                        let mut layer = self.layer(
                            id,
                            texture,
                            to_physical(*rect, scale),
                            *alpha,
                            *radius,
                            [1.0; 4],
                        );
                        layer.clip = clip.map(|c| to_physical(c, scale));
                        layer.reveal = uniforms;
                        // 锁屏是一整张不透明的纸（墨散时除外）。
                        layer.opaque = *key == TexKey::LockSheet && *radius == 0.0;
                        layer.commit = self.reveal_commit(*key, uniforms.is_active());
                        out.push(ShellElement::Layer(layer));
                    }
                }
                DrawItem::Landscape {
                    rect,
                    scroll,
                    drift,
                    page,
                } => {
                    let (sw, _) = shell.screen_size();
                    // 停在某一页：画预先合成好的整页（不透明，其下不必再画）。
                    if let Some(p) = page
                        && let Some((texture, id)) =
                            self.texture(renderer, TexKey::LandscapePage(*p), assets, shell)?
                    {
                        let mut layer =
                            self.layer(id, texture, to_physical(*rect, scale), 1.0, 0.0, [1.0; 4]);
                        layer.opaque = true;
                        out.push(ShellElement::Layer(layer));
                        continue;
                    }
                    let layers = assets.layers().to_vec();
                    for (i, info) in layers.into_iter().enumerate() {
                        let key = TexKey::WallpaperLayer(i);
                        if let Some((texture, id)) = self.texture(renderer, key, assets, shell)? {
                            let size = texture.size();
                            let layer_size = (size.w as f64 / scale, size.h as f64 / scale);
                            let r = layer_rect(*rect, *scroll, *drift, info, layer_size, sw);
                            let mut layer =
                                self.layer(id, texture, to_physical(r, scale), 1.0, 0.0, [1.0; 4]);
                            // 最底下的天色层不透明。
                            layer.opaque = i == 0;
                            out.push(ShellElement::Layer(layer));
                        }
                    }
                }
                DrawItem::Ink(dab) => {
                    let element = self.ink_element(dab, ink_index, theme);
                    ink_index += 1;
                    out.push(ShellElement::Ink(element));
                }
                DrawItem::Natural {
                    key,
                    x,
                    y,
                    align,
                    alpha,
                    scale: extra,
                } => {
                    if *alpha <= 0.001 {
                        continue;
                    }
                    if let Some((texture, id)) = self.texture(renderer, *key, assets, shell)? {
                        let size = texture.size();
                        let (w, h) = (size.w as f64 / scale * extra, size.h as f64 / scale * extra);
                        let x0 = match align {
                            Align::Left => *x,
                            Align::Center => x - w / 2.0,
                            Align::Right => x - w,
                        };
                        let rect = to_physical(RectF::new(x0, *y, w, h), scale);
                        out.push(ShellElement::Layer(
                            self.layer(id, texture, rect, *alpha, 0.0, [1.0; 4]),
                        ));
                    }
                }
                DrawItem::Solid {
                    rect,
                    color,
                    radius,
                } => {
                    if self.solid_ids.len() <= solid_index {
                        self.solid_ids.push(Id::new());
                    }
                    let id = self.solid_ids[solid_index].clone();
                    solid_index += 1;
                    // 预乘
                    let c = [
                        color[0] * color[3],
                        color[1] * color[3],
                        color[2] * color[3],
                        color[3],
                    ];
                    out.push(ShellElement::Layer(self.layer(
                        id,
                        self.white.clone(),
                        to_physical(*rect, scale),
                        1.0,
                        *radius,
                        c,
                    )));
                }
                DrawItem::Window {
                    window,
                    rect,
                    alpha,
                    radius,
                    clip,
                    reveal,
                } => {
                    let (sw, _) = shell.screen_size();
                    let placement = WindowPlacement::new(*rect, sw);
                    let geo = window.geometry();
                    let (ox, oy) = placement.origin((geo.loc.x as f64, geo.loc.y as f64));
                    let loc = Point::<i32, Physical>::from((
                        (ox * scale).round() as i32,
                        (oy * scale).round() as i32,
                    ));
                    let uniforms = self.reveal_uniforms(reveal.as_ref(), theme);
                    let force_damage =
                        uniforms.is_active() || self.revealed_windows.contains(window);
                    if uniforms.is_active() {
                        revealed_windows.push(window.clone());
                    }
                    // 整个表面树按输出缩放生成，再以窗口原点为中心整体缩放到卡片大小。
                    // （render_elements 的缩放参数只作用于子表面的位置，表面尺寸总是按输出
                    // 缩放计算，不能用它缩放窗口。）
                    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                        window.render_elements(renderer, loc, Scale::from(scale), *alpha as f32);
                    // 圆角与裁剪按整个窗口算（窗口的可见矩形，屏幕坐标）：贴着窗口边缘的子表面
                    // 带上窗口的圆角，窗口内部的子表面不受影响。
                    let window_clip = to_physical(clip.unwrap_or(*rect), scale);
                    // render_elements 返回“上层在前”，这里先反转成“下层在前”，最后统一再反转。
                    for surface in surfaces.into_iter().rev() {
                        out.push(ShellElement::Surface(RoundedSurface {
                            content: SurfaceContent::of(&surface),
                            white: self.white.clone(),
                            inner: RescaleRenderElement::from_element(
                                surface,
                                loc,
                                placement.scale,
                            ),
                            radius: (*radius * scale) as f32,
                            clip: window_clip,
                            reveal: uniforms,
                            force_damage,
                            program: self.program.clone(),
                        }));
                    }
                }
            }
        }
        self.revealed_windows = revealed_windows;
        out.reverse();
        Ok(out)
    }

    fn layer(
        &self,
        id: Id,
        texture: GlesTexture,
        geometry: Rectangle<i32, Physical>,
        alpha: f64,
        radius: f64,
        color: [f32; 4],
    ) -> LayerElement {
        LayerElement {
            id,
            commit: CommitCounter::default(),
            texture,
            geometry,
            alpha: alpha.clamp(0.0, 1.0) as f32,
            radius: (radius * self.scale) as f32,
            clip: None,
            color,
            reveal: RevealUniforms::NONE,
            opaque: false,
            program: self.program.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(parallax: f32) -> LayerInfo {
        LayerInfo { parallax, top: 0.0 }
    }

    #[test]
    fn landscape_layers_move_with_their_parallax() {
        let screen = RectF::new(0.0, 0.0, 1280.0, 800.0);
        let full = |w: f64| (w, 800.0);
        // 远山 0.15：翻一页（1280 点）只移动 192 点；不动的层不随翻页移动。
        let far = layer_rect(screen, 1280.0, 0.0, layer(0.15), full(1472.0), 1280.0);
        assert!((far.x + 192.0).abs() < 1e-3);
        assert_eq!(far.w, 1472.0);
        assert_eq!(
            layer_rect(screen, 1280.0, 30.0, layer(0.0), full(1280.0), 1280.0).x,
            0.0
        );
        // 末页时层的右边缘正好对齐屏幕右边缘（层宽 = 屏宽 + p × 最大滚动）。
        let near = layer_rect(screen, 1280.0, 0.0, layer(0.6), full(2048.0), 1280.0);
        assert!((near.x + near.w - 1280.0).abs() < 1e-3);
        // 只有烟云层随速度额外漂移（中间有余量时几乎是原值）。
        let mist = layer_rect(screen, 640.0, 20.0, layer(0.8), full(2304.0), 1280.0);
        assert!((mist.x + 0.8 * 640.0 + 20.0).abs() < 0.5, "{}", mist.x);
        // 在两端漂移被软限制，层的边缘不会露出来。
        for (scroll, drift) in [(0.0, -50.0), (10.0, -50.0), (1280.0, 50.0), (1270.0, 50.0)] {
            let m = layer_rect(screen, scroll, drift, layer(0.8), full(2304.0), 1280.0);
            assert!(
                m.x <= 1e-9 && m.x + m.w >= 1280.0 - 1e-3,
                "{scroll} {drift}: {m:?}"
            );
        }
        let near = layer_rect(screen, 640.0, 20.0, layer(0.6), full(2048.0), 1280.0);
        assert!((near.x + 0.6 * 640.0).abs() < 1e-3);
        // 主屏缩放时，视差位移、层的大小与顶边一起缩放。
        let zoomed = screen.scaled_about(640.0, 400.0, 1.1);
        let cropped = LayerInfo {
            parallax: 0.15,
            top: 300.0,
        };
        let z = layer_rect(zoomed, 1280.0, 0.0, cropped, (1472.0, 500.0), 1280.0);
        assert!((z.x - (zoomed.x - 192.0 * 1.1)).abs() < 1e-3);
        assert!((z.w - 1472.0 * 1.1).abs() < 1e-9);
        assert!((z.y - (zoomed.y + 300.0 * 1.1)).abs() < 1e-9);
        assert!((z.h - 500.0 * 1.1).abs() < 1e-9);
    }

    #[test]
    fn transparent_rows_are_cropped() {
        let mut pm = loong_ui::pixmap(4, 10);
        let (full, top) = crop_rows(pm.clone());
        assert_eq!((full.height(), top), (1, 0));
        for y in [3, 6] {
            pm.data_mut()[(y * 4 + 1) * 4 + 3] = 255;
        }
        let (cropped, top) = crop_rows(pm.clone());
        assert_eq!((cropped.width(), cropped.height(), top), (4, 4, 3));
        assert_eq!(cropped.data()[(4 + 1) * 4 + 3], 0);
        assert_eq!(cropped.data()[4 + 3], 255);
    }
}
