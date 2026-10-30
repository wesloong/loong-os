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
use crate::shell::{Align, DrawItem, Scene, Shell, TexKey, geometry::RectF};

/// CPU 端位图资源（物理像素）。
pub struct Assets {
    scale: f64,
    text: TextRenderer,
    pixmaps: HashMap<TexKey, (Pixmap, u64)>,
    version: u64,
    clock_text: String,
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
        }
        let status_icons = assets.status_icons();
        assets.put(TexKey::StatusIcons, status_icons);
        assets.refresh_clock(shell);
        assets
    }

    fn put(&mut self, key: TexKey, pixmap: Pixmap) {
        self.version += 1;
        self.pixmaps.insert(key, (pixmap, self.version));
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

    /// 状态栏时间变化时重新生成。
    pub fn refresh_clock(&mut self, shell: &Shell) {
        if self.clock_text != shell.clock_text {
            self.clock_text = shell.clock_text.clone();
            let style = self.status_style();
            let pm = self.text.render(&self.clock_text, &style);
            self.put(TexKey::StatusTime, pm);
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
