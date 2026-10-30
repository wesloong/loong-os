//! 把 [`Scene`](crate::shell::Scene) 变成 GPU 渲染元素。
//!
//! - [`Assets`]：在 CPU 上把图标、文字、面板等栅格化为位图（只做一次）。
//! - [`Painter`]：把位图上传为纹理，并按场景生成 Smithay 渲染元素。

pub mod element;

use std::collections::HashMap;

use loong_ui::{
    Pixmap,
    icon::{self, IconStyle},
    text::{TextRenderer, TextStyle},
    theme,
    tiny_skia::{Color, Rect},
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportMem, Texture,
            element::{AsRenderElements, Id, surface::WaylandSurfaceRenderElement},
            gles::{GlesError, GlesRenderer, GlesTexProgram, GlesTexture},
            utils::CommitCounter,
        },
    },
    utils::{Physical, Point, Rectangle, Scale, Size},
};

use self::element::{LayerElement, RoundedSurface, ShellElement};
use crate::shell::{
    Align, DrawItem, Scene, Shell, TexKey,
    apps::AppEntry,
    geometry::RectF,
    switcher::{self, SwitcherLayout},
};

/// CPU 端位图资源（物理像素）。
pub struct Assets {
    scale: f64,
    text: TextRenderer,
    pixmaps: HashMap<TexKey, (Pixmap, u64)>,
    version: u64,
    clock_text: String,
    date_text: String,
}

impl Assets {
    /// 生成主屏所需的全部位图。
    pub fn new(shell: &Shell, scale: f64) -> Self {
        let mut assets = Assets {
            scale,
            text: TextRenderer::new(),
            pixmaps: HashMap::new(),
            version: 0,
            clock_text: String::new(),
            date_text: String::new(),
        };
        let (w, h) = shell.screen_size();
        let wallpaper = loong_ui::wallpaper::default_wallpaper(px(w, scale), px(h, scale));

        let d = shell.layout.dock;
        let dock_rect = Rect::from_xywh(
            (d.x * scale) as f32,
            (d.y * scale) as f32,
            (d.w * scale) as f32,
            (d.h * scale) as f32,
        )
        .expect("Dock 尺寸有效");
        let dock = loong_ui::material::frosted_panel(
            &wallpaper,
            dock_rect,
            (theme::radius::DOCK as f64 * scale) as f32,
            (30.0 * scale) as f32,
            Color::from_rgba8(255, 255, 255, 46),
        );
        assets.put(
            TexKey::SwitcherBackdrop,
            switcher_backdrop(&wallpaper, scale),
        );
        assets.put(TexKey::CardShadow, card_shadow(&shell.switcher, scale));
        assets.put(TexKey::Wallpaper, wallpaper);
        assets.put(TexKey::Dock, dock);

        let icon_px = px(shell.layout.icon, scale);
        for (i, app) in shell.apps.iter().enumerate() {
            let svg = icon::builtin_symbol(&app.symbol)
                .unwrap_or(icon::builtin_symbol("compass").expect("内置图标"));
            let style = IconStyle {
                top: theme::hex(app.colors.0, 1.0),
                bottom: theme::hex(app.colors.1, 1.0),
                glyph: Color::WHITE,
            };
            assets.put(TexKey::Icon(i), icon::app_icon(icon_px, svg, style));
            let label_style = TextStyle {
                size: (theme::font::ICON_LABEL as f64 * scale) as f32,
                weight: 500,
                color: theme::label_on_wallpaper(),
                shadow: true,
            };
            let label = assets.text.render(&app.name, &label_style);
            assets.put(TexKey::Label(i), label);
            let card_label = assets.card_label(app);
            assets.put(TexKey::CardLabel(i), card_label);
        }
        let status_icons = assets.status_icons();
        assets.put(TexKey::StatusIcons, status_icons);
        let hint = assets.text.render(
            "上滑解锁",
            &TextStyle {
                size: (15.0 * scale) as f32,
                weight: 500,
                color: Color::WHITE,
                shadow: true,
            },
        );
        assets.put(TexKey::LockHint, hint);
        assets.refresh_clock(shell);
        assets
    }

    fn put(&mut self, key: TexKey, pixmap: Pixmap) {
        self.version += 1;
        self.pixmaps.insert(key, (pixmap, self.version));
    }

    /// 多任务卡片上方的标签：24pt 图标 + 8pt 间距 + 名称（`subhead` 15pt 半粗，白色带阴影）。
    /// 位图高度固定为 [`switcher::LABEL_HEIGHT`]，图标垂直居中，便于 shell 排版。
    fn card_label(&mut self, app: &AppEntry) -> Pixmap {
        let s = self.scale;
        let icon_px = px(switcher::LABEL_ICON, s);
        let svg = icon::builtin_symbol(&app.symbol)
            .unwrap_or(icon::builtin_symbol("compass").expect("内置图标"));
        let style = IconStyle {
            top: theme::hex(app.colors.0, 1.0),
            bottom: theme::hex(app.colors.1, 1.0),
            glyph: Color::WHITE,
        };
        let icon = icon::app_icon(icon_px, svg, style);
        let size = (15.0 * s) as f32;
        let text = self.text.render(
            &app.name,
            &TextStyle {
                size,
                weight: 600,
                color: Color::WHITE,
                shadow: true,
            },
        );
        // 文字位图四周留有阴影边距（与 TextRenderer::render 一致），这里扣掉，保证视觉间距为 8pt。
        let pad = (size * 0.25).ceil() as i32;
        let text_x = icon_px as i32 + (8.0 * s).round() as i32 - pad;
        let h = px(switcher::LABEL_HEIGHT, s);
        let w = (text_x + text.width() as i32).max(icon_px as i32) as u32;
        let mut pm = loong_ui::pixmap(w, h);
        pm.draw_pixmap(
            0,
            (h as i32 - icon_px as i32) / 2,
            icon.as_ref(),
            &Default::default(),
            Default::default(),
            None,
        );
        pm.draw_pixmap(
            text_x,
            (h as i32 - text.height() as i32) / 2,
            text.as_ref(),
            &Default::default(),
            Default::default(),
            None,
        );
        pm
    }

    fn status_style(&self) -> TextStyle {
        TextStyle {
            size: (theme::font::STATUS as f64 * self.scale) as f32,
            weight: 600,
            color: Color::WHITE,
            shadow: true,
        }
    }

    fn status_icons(&mut self) -> Pixmap {
        let s = self.scale as f32;
        let percent = self.text.render("100%", &self.status_style());
        let sym = (18.0 * s).round();
        let gap = (6.0 * s).round();
        let w = percent.width() as f32 + gap + sym * 2.0 + gap;
        let h = (percent.height() as f32).max(sym);
        let mut pm = loong_ui::pixmap(w.ceil() as u32, h.ceil() as u32);
        let wifi = icon::builtin_symbol("wifi").expect("内置图标");
        icon::draw_symbol(
            &mut pm,
            wifi,
            Rect::from_xywh(0.0, (h - sym) / 2.0, sym, sym).unwrap(),
            Color::WHITE,
            Some(2.25),
        );
        pm.draw_pixmap(
            (sym + gap) as i32,
            ((h - percent.height() as f32) / 2.0) as i32,
            percent.as_ref(),
            &Default::default(),
            Default::default(),
            None,
        );
        let battery = icon::builtin_symbol("battery-full").expect("内置图标");
        let bx = sym + gap + percent.width() as f32 + gap;
        icon::draw_symbol(
            &mut pm,
            battery,
            Rect::from_xywh(bx, (h - sym) / 2.0, sym, sym).unwrap(),
            Color::WHITE,
            Some(2.0),
        );
        pm
    }

    /// 时间、日期变化时重新生成状态栏与锁屏文字。
    pub fn refresh_clock(&mut self, shell: &Shell) {
        let s = self.scale as f32;
        if self.clock_text != shell.clock_text {
            self.clock_text = shell.clock_text.clone();
            let style = self.status_style();
            let pm = self.text.render(&self.clock_text, &style);
            self.put(TexKey::StatusTime, pm);
            let big = self.text.render(
                &self.clock_text,
                &TextStyle {
                    size: 96.0 * s,
                    weight: 300,
                    color: Color::WHITE,
                    shadow: true,
                },
            );
            self.put(TexKey::LockTime, big);
        }
        if self.date_text != shell.date_text {
            self.date_text = shell.date_text.clone();
            let pm = self.text.render(
                &self.date_text,
                &TextStyle {
                    size: 20.0 * s,
                    weight: 500,
                    color: Color::WHITE,
                    shadow: true,
                },
            );
            self.put(TexKey::LockDate, pm);
        }
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

    /// 演示应用的占位画面：应用主题色背景 + 大图标 + 名称。
    fn placeholder(&mut self, shell: &Shell, app: usize) -> Pixmap {
        use loong_ui::tiny_skia::{
            GradientStop, LinearGradient, Paint, Point as SkPoint, SpreadMode, Transform,
        };
        let (w, h) = shell.screen_size();
        let (pw, ph) = (px(w, self.scale), px(h, self.scale));
        let entry = &shell.apps[app];
        let mut pm = loong_ui::pixmap(pw, ph);
        let paint = Paint {
            shader: LinearGradient::new(
                SkPoint::from_xy(0.0, 0.0),
                SkPoint::from_xy(0.0, ph as f32),
                vec![
                    GradientStop::new(0.0, theme::hex(entry.colors.1, 1.0)),
                    GradientStop::new(1.0, theme::hex(entry.colors.0, 1.0)),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            )
            .expect("渐变有效"),
            ..Paint::default()
        };
        pm.fill_rect(
            Rect::from_xywh(0.0, 0.0, pw as f32, ph as f32).unwrap(),
            &paint,
            Transform::identity(),
            None,
        );

        let s = self.scale as f32;
        let glyph = (120.0 * s).round();
        let svg = icon::builtin_symbol(&entry.symbol)
            .unwrap_or(icon::builtin_symbol("compass").expect("内置图标"));
        let gy = ph as f32 * 0.36 - glyph / 2.0;
        icon::draw_symbol(
            &mut pm,
            svg,
            Rect::from_xywh((pw as f32 - glyph) / 2.0, gy, glyph, glyph).unwrap(),
            Color::WHITE,
            Some(1.5),
        );

        let title = self.text.render(
            &entry.name,
            &TextStyle {
                size: theme::font::LARGE_TITLE * s,
                weight: 700,
                color: Color::WHITE,
                shadow: false,
            },
        );
        let ty = (gy + glyph + 28.0 * s) as i32;
        pm.draw_pixmap(
            (pw as i32 - title.width() as i32) / 2,
            ty,
            title.as_ref(),
            &Default::default(),
            Transform::identity(),
            None,
        );
        let sub = self.text.render(
            "Loong OS · 演示应用",
            &TextStyle {
                size: theme::font::BODY * s,
                weight: 400,
                color: Color::from_rgba8(255, 255, 255, 200),
                shadow: false,
            },
        );
        pm.draw_pixmap(
            (pw as i32 - sub.width() as i32) / 2,
            ty + title.height() as i32 + (8.0 * s) as i32,
            sub.as_ref(),
            &Default::default(),
            Transform::identity(),
            None,
        );
        pm
    }
}

fn px(v: f64, scale: f64) -> u32 {
    (v * scale).round().max(1.0) as u32
}

/// 模糊类位图在 CPU 上以 1/4 分辨率生成，GPU 绘制时再放大：模糊后的画面放大看不出差别。
const BLUR_DOWNSAMPLE: f64 = 4.0;

/// 多任务背景（设计规范 8.6）：主屏壁纸 + `thin` 材质（模糊 σ 24pt，深色叠加色）。
/// 壁纸是静态的，只在启动时算一次。
fn switcher_backdrop(wallpaper: &Pixmap, scale: f64) -> Pixmap {
    use loong_ui::tiny_skia::{FilterQuality, Paint, Pattern, SpreadMode, Transform};
    let k = (1.0 / BLUR_DOWNSAMPLE) as f32;
    let w = (wallpaper.width() as f32 * k).ceil() as u32;
    let h = (wallpaper.height() as f32 * k).ceil() as u32;
    let mut small = loong_ui::pixmap(w, h);
    let all = Rect::from_xywh(0.0, 0.0, w as f32, h as f32).expect("尺寸有效");
    let paint = Paint {
        shader: Pattern::new(
            wallpaper.as_ref(),
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
    tint.set_color(theme::hex(0x121619, 0.42));
    small.fill_rect(all, &tint, Transform::identity(), None);
    small
}

/// 多任务卡片阴影（`shadowHigh` 深色：模糊 48pt、黑色 60%），按标准卡片尺寸生成，
/// 四周各留 [`switcher::SHADOW_SPREAD`]；向下的偏移由 shell 排版时加上。
fn card_shadow(layout: &SwitcherLayout, scale: f64) -> Pixmap {
    use loong_ui::tiny_skia::{FillRule, Paint, Transform};
    let k = scale / BLUR_DOWNSAMPLE;
    let spread = switcher::SHADOW_SPREAD;
    let mut pm = loong_ui::pixmap(
        px(layout.card_w + 2.0 * spread, k),
        px(layout.card_h + 2.0 * spread, k),
    );
    let card = Rect::from_xywh(
        (spread * k) as f32,
        (spread * k) as f32,
        (layout.card_w * k) as f32,
        (layout.card_h * k) as f32,
    )
    .expect("卡片尺寸有效");
    let path = loong_ui::shape::continuous_rect(card, (switcher::CARD_RADIUS * k) as f32);
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(Color::from_rgba8(0, 0, 0, 153));
    pm.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    // CSS 的模糊半径 48pt ≈ 高斯 σ 24pt。
    loong_ui::material::blur(&mut pm, (24.0 * k).round().max(1.0) as usize);
    pm
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

struct UploadedTexture {
    texture: GlesTexture,
    id: Id,
    version: u64,
}

/// GPU 端：纹理缓存 + 生成渲染元素。
pub struct Painter {
    scale: f64,
    program: GlesTexProgram,
    textures: HashMap<TexKey, UploadedTexture>,
    white: GlesTexture,
    solid_ids: Vec<Id>,
}

impl Painter {
    pub fn new(renderer: &mut GlesRenderer, scale: f64) -> Result<Self, GlesError> {
        let program = element::compile_program(renderer)?;
        let white = renderer.import_memory(
            &[255, 255, 255, 255],
            Fourcc::Abgr8888,
            (1, 1).into(),
            false,
        )?;
        Ok(Painter {
            scale,
            program,
            textures: HashMap::new(),
            white,
            solid_ids: Vec::new(),
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

    /// 按场景生成渲染元素（返回顺序：最上层在前，符合 Smithay 约定）。
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        scene: &Scene,
        assets: &mut Assets,
        shell: &Shell,
    ) -> Result<Vec<ShellElement>, GlesError> {
        assets.refresh_clock(shell);
        let scale = self.scale;
        let mut out: Vec<ShellElement> = Vec::with_capacity(scene.len() + 4);
        let mut solid_index = 0;
        for item in scene {
            match item {
                DrawItem::Texture {
                    key,
                    rect,
                    alpha,
                    radius,
                    clip,
                } => {
                    if *alpha <= 0.001 {
                        continue;
                    }
                    if let Some((texture, id)) = self.texture(renderer, *key, assets, shell)? {
                        let mut layer = self.layer(
                            id,
                            texture,
                            to_physical(*rect, scale),
                            *alpha,
                            *radius,
                            [1.0; 4],
                        );
                        layer.clip = clip.map(|c| to_physical(c, scale));
                        out.push(ShellElement::Layer(layer));
                    }
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
                } => {
                    let (sw, _) = shell.screen_size();
                    let content_scale = rect.w / sw;
                    let geo = window.geometry();
                    let loc = Point::<i32, Physical>::from((
                        ((rect.x - geo.loc.x as f64 * content_scale) * scale).round() as i32,
                        ((rect.y - geo.loc.y as f64 * content_scale) * scale).round() as i32,
                    ));
                    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = window
                        .render_elements(
                            renderer,
                            loc,
                            Scale::from(scale * content_scale),
                            *alpha as f32,
                        );
                    // render_elements 返回“上层在前”，这里先反转成“下层在前”，最后统一再反转。
                    for surface in surfaces.into_iter().rev() {
                        out.push(ShellElement::Surface(RoundedSurface {
                            inner: surface,
                            radius: (*radius * scale) as f32,
                            clip: clip.map(|c| to_physical(c, scale)),
                            program: self.program.clone(),
                        }));
                    }
                }
            }
        }
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
            program: self.program.clone(),
        }
    }
}
