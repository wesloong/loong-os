//! 「墨韵」画廊：把水墨绘画工具的全部产物画成 PNG，供视觉评审。
//!
//! 用法：`cargo run --release -p loong-ui --example ink_gallery [输出目录]`（默认 `out/ink`）。

use std::path::{Path, PathBuf};
use std::time::Instant;

use loong_ui::icon::builtin_symbol;
use loong_ui::ink::{self, IconSpec};
use loong_ui::text::{FontRole, TextRenderer, TextStyle};
use loong_ui::theme::{Ink, Theme, hex};
use loong_ui::tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Rect, Transform};

const SYMBOLS: [(&str, &str); 34] = [
    ("compass", "浏览器"),
    ("message-circle", "信息"),
    ("mail", "邮件"),
    ("music", "音乐"),
    ("settings", "设置"),
    ("image", "相册"),
    ("notebook-pen", "备忘录"),
    ("calendar", "日历"),
    ("clock", "时钟"),
    ("folder", "文件"),
    ("camera", "相机"),
    ("map", "地图"),
    ("calculator", "计算器"),
    ("cloud-sun", "天气"),
    ("play-circle", "视频"),
    ("book-open", "图书"),
    ("mic", "录音机"),
    ("list-checks", "提醒事项"),
    ("contact", "通讯录"),
    ("heart-pulse", "健康"),
    ("trending-up", "股票"),
    ("languages", "翻译"),
    ("podcast", "播客"),
    ("house", "家庭"),
    ("wallet", "钱包"),
    ("shopping-bag", "商店"),
    ("gamepad-2", "游戏"),
    ("newspaper", "新闻"),
    ("timer", "计时器"),
    ("ruler", "测距仪"),
    ("scan-line", "扫描"),
    ("terminal", "终端"),
    ("wifi", "无线"),
    ("battery-full", "电池"),
];

fn save(pm: &Pixmap, dir: &Path, name: &str) {
    let p = dir.join(name);
    pm.save_png(&p).expect("写 PNG");
    println!("  {}", p.display());
}

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

fn scaled(src: &Pixmap, k: f32) -> Pixmap {
    let (w, h) = (
        (src.width() as f32 * k).round() as u32,
        (src.height() as f32 * k).round() as u32,
    );
    let mut out = loong_ui::pixmap(w, h);
    let paint = PixmapPaint {
        quality: if k >= 1.0 {
            FilterQuality::Nearest
        } else {
            FilterQuality::Bicubic
        },
        ..Default::default()
    };
    out.draw_pixmap(
        0,
        0,
        src.as_ref(),
        &paint,
        Transform::from_scale(k, k),
        None,
    );
    out
}

fn label(text: &mut TextRenderer, s: &str, size: f32, color: loong_ui::Color) -> Pixmap {
    text.render(
        s,
        &TextStyle {
            family: FontRole::Display,
            size,
            weight: 400,
            color,
            shadow: false,
        },
    )
}

fn icon_spec(theme: Theme, i: usize, sym: &'static str) -> IconSpec<'static> {
    IconSpec {
        symbol_svg: builtin_symbol(sym).unwrap(),
        pigment: ink::suggested_pigment(sym),
        theme,
        seed: 1000 + i as u32 * 17,
    }
}

/// 图标网格：纸面上排开全部图标（带墨影与文楷名称）。
fn icon_grid(theme: Theme, size: u32, text: &mut TextRenderer) -> Pixmap {
    let cols = 9;
    let rows = SYMBOLS.len().div_ceil(cols);
    let cell_w = size as f32 * 1.55;
    let cell_h = size as f32 * 1.75;
    let (w, h) = (
        (cols as f32 * cell_w + size as f32 * 0.6) as u32,
        (rows as f32 * cell_h + size as f32 * 0.6) as u32,
    );
    let mut pm = ink::paper(theme, w, h, 9);
    let pal = theme.palette();
    pm.fill_rect(
        Rect::from_xywh(0.0, 0.0, w as f32, h as f32).unwrap(),
        &{
            let mut p = loong_ui::tiny_skia::Paint::default();
            p.set_color(hex(pal.paper, 0.6));
            p
        },
        Transform::identity(),
        None,
    );
    let spread = (size as f32 * 0.16) as u32;
    let shadow = ink::ink_shadow(theme, size, size, size as f32 * 0.225 * 0.96, spread);
    for (i, (sym, name)) in SYMBOLS.iter().enumerate() {
        let (c, r) = (i % cols, i / cols);
        let x = (size as f32 * 0.3 + c as f32 * cell_w + (cell_w - size as f32) / 2.0) as i32;
        let y = (size as f32 * 0.3 + r as f32 * cell_h) as i32;
        draw(&mut pm, &shadow, x - spread as i32, y - spread as i32);
        let icon = ink::app_icon(size, &icon_spec(theme, i, sym));
        draw(&mut pm, &icon, x, y);
        let l = label(text, name, size as f32 * 0.17, pal.ink(Ink::Nong));
        draw(
            &mut pm,
            &l,
            x + (size as i32 - l.width() as i32) / 2,
            y + (size as f32 * 1.06) as i32,
        );
    }
    pm
}

/// 主屏模拟：壁纸 + 状态栏 + 图标 + 绢 Dock，用于判断整体气质。
fn home_mock(
    theme: Theme,
    layers: &[ink::Layer],
    w: u32,
    h: u32,
    scroll: f32,
    text: &mut TextRenderer,
) -> Pixmap {
    let mut pm = ink::composite(layers, w, h, scroll);
    let scale = w as f32 / 1280.0;
    let pal = theme.palette();
    let icon = (76.0 * scale) as u32;
    let spread = (icon as f32 * 0.16) as u32;
    let shadow = ink::ink_shadow(theme, icon, icon, icon as f32 * 0.225 * 0.96, spread);
    // 状态栏。
    let bar_h = (16.0 * scale) as u32;
    let time = label(text, "9:41", 15.0 * scale, pal.ink(Ink::Jiao));
    draw(&mut pm, &time, (22.0 * scale) as i32, (5.0 * scale) as i32);
    let bat = ink::battery(theme, bar_h, 0.8, false);
    draw(
        &mut pm,
        &bat,
        (w - bat.width()) as i32 - (20.0 * scale) as i32,
        (7.0 * scale) as i32,
    );
    let wifi = ink::wifi(theme, bar_h, 3);
    draw(
        &mut pm,
        &wifi,
        (w - bat.width() - wifi.width()) as i32 - (30.0 * scale) as i32,
        (7.0 * scale) as i32,
    );
    // 图标网格（6 × 4），不含 Dock 的 5 个。
    let page_offset = (scroll / w as f32).round() as usize;
    let grid: Vec<usize> = (5..SYMBOLS.len()).collect();
    let (cols, rows) = (6usize, 4usize);
    let (gx0, gy0) = (197.0 * scale, 86.0 * scale);
    let (dx, dy) = (162.0 * scale, 144.0 * scale);
    for r in 0..rows {
        for c in 0..cols {
            let k = page_offset * cols * rows + r * cols + c;
            let Some(&i) = grid.get(k) else { continue };
            let (sym, name) = SYMBOLS[i];
            let x = (gx0 + c as f32 * dx) as i32 - (scroll % w as f32) as i32;
            let y = (gy0 + r as f32 * dy) as i32;
            draw(&mut pm, &shadow, x - spread as i32, y - spread as i32);
            draw(
                &mut pm,
                &ink::app_icon(icon, &icon_spec(theme, i, sym)),
                x,
                y,
            );
            let style = TextStyle {
                family: FontRole::Display,
                size: 13.0 * scale,
                weight: 400,
                color: pal.ink(Ink::Jiao),
                shadow: false,
            };
            let l = text.render_halo(name, &style, hex(pal.paper, 0.85), 5.0 * scale);
            let pad = (5.0 * scale).ceil() as i32 + 1;
            draw(
                &mut pm,
                &l,
                x + (icon as i32 - l.width() as i32) / 2,
                y + icon as i32 + (5.0 * scale) as i32 - pad,
            );
        }
    }
    // 翻页点。
    for p in 0..2 {
        let d = (7.0 * scale) as u32;
        let dot = {
            let mut f = ink::raster::Field::new(d as usize * 3, d as usize * 3);
            let s = ink::brush::Stroke::open(vec![
                (d as f32 * 1.5, d as f32 * 1.5),
                (d as f32 * 1.5 + 0.02, d as f32 * 1.5),
            ]);
            f = ink::brush::render(
                &[s],
                &ink::brush::BrushStyle::for_width(d as f32),
                f.w,
                f.h,
                3 + p,
            );
            let mut pm = loong_ui::pixmap(d * 3, d * 3);
            let lvl = if p == page_offset as u32 { 0 } else { 4 };
            ink::raster::blend_field(&mut pm, &f, ink::raster::rgb(pal.ink[lvl]), 1.0);
            pm
        };
        draw(
            &mut pm,
            &dot,
            (w as f32 / 2.0 - 1.5 * d as f32 + (p as f32 - 0.5) * 16.0 * scale) as i32,
            (650.0 * scale) as i32,
        );
    }
    // Dock：绢。
    let dock = Rect::from_xywh(389.0 * scale, 680.0 * scale, 502.0 * scale, 106.0 * scale).unwrap();
    let silk = ink::silk_panel(theme, &pm.clone(), dock, 30.0 * scale);
    let dsh = ink::ink_shadow(theme, silk.width(), silk.height(), 30.0 * scale, spread);
    draw(
        &mut pm,
        &dsh,
        dock.x() as i32 - spread as i32,
        dock.y() as i32 - spread as i32,
    );
    draw(&mut pm, &silk, dock.x() as i32, dock.y() as i32);
    for (i, (sym, _)) in SYMBOLS.iter().enumerate().take(5) {
        let x = (dock.x() + 15.0 * scale + i as f32 * 99.0 * scale) as i32;
        let y = (dock.y() + 15.0 * scale) as i32;
        draw(&mut pm, &shadow, x - spread as i32, y - spread as i32);
        draw(
            &mut pm,
            &ink::app_icon(icon, &icon_spec(theme, i, sym)),
            x,
            y,
        );
    }
    pm
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "out/ink".into()));
    std::fs::create_dir_all(&dir).expect("创建输出目录");
    let mut text = TextRenderer::new();
    let (w, h) = (2560u32, 1600u32);
    let only = std::env::var("ONLY").unwrap_or_default();
    let want = |k: &str| only.is_empty() || only.split(',').any(|o| o == k);

    for theme in [Theme::Light, Theme::Dark] {
        let tn = match theme {
            Theme::Light => "light",
            Theme::Dark => "dark",
        };
        println!("{tn}:");
        if want("wallpaper") || want("home") {
            let t = Instant::now();
            let layers = ink::landscape(theme, w, h, 2, 7);
            println!("  山水长卷 {:?}", t.elapsed());
            if want("wallpaper") {
                let full = ink::composite(&layers, w, h, 0.0);
                save(&full, &dir, &format!("wallpaper-{tn}.png"));
                save(
                    &scaled(&full, 0.5),
                    &dir,
                    &format!("wallpaper-{tn}-half.png"),
                );
                let mid = ink::composite(&layers, w, h, w as f32 * 0.5);
                save(
                    &scaled(&mid, 0.5),
                    &dir,
                    &format!("wallpaper-{tn}-scroll50-half.png"),
                );
                let p2 = ink::composite(&layers, w, h, w as f32);
                save(
                    &scaled(&p2, 0.5),
                    &dir,
                    &format!("wallpaper-{tn}-page2-half.png"),
                );
                for (i, l) in layers.iter().enumerate() {
                    // 透明层垫在中灰上看得更清楚。
                    let mut bg = loong_ui::pixmap(l.pixmap.width(), l.pixmap.height());
                    bg.fill(hex(0x808080, 1.0));
                    draw(&mut bg, &l.pixmap, 0, 0);
                    save(
                        &scaled(&bg, 0.25),
                        &dir,
                        &format!("layer-{tn}-{i}-p{:.2}.png", l.parallax),
                    );
                }
                // 局部 1:1 细节。
                let crop = |x: u32, y: u32, cw: u32, ch: u32| {
                    let mut c = loong_ui::pixmap(cw, ch);
                    draw(&mut c, &full, -(x as i32), -(y as i32));
                    c
                };
                save(
                    &crop(0, 900, 1100, 700),
                    &dir,
                    &format!("wallpaper-{tn}-detail-near.png"),
                );
                save(
                    &crop(1500, 150, 1060, 800),
                    &dir,
                    &format!("wallpaper-{tn}-detail-sun.png"),
                );
            }
            if want("home") {
                let home = home_mock(theme, &layers, w, h, 0.0, &mut text);
                save(&scaled(&home, 0.5), &dir, &format!("home-{tn}-half.png"));
                save(&home, &dir, &format!("home-{tn}.png"));
            }
        }
        if want("icons") {
            let t = Instant::now();
            let g2 = icon_grid(theme, 152, &mut text);
            println!("  图标网格 2× {:?}", t.elapsed());
            save(&g2, &dir, &format!("icons-{tn}-2x.png"));
            let g1 = icon_grid(theme, 76, &mut text);
            save(&g1, &dir, &format!("icons-{tn}-1x.png"));
            // 单个大图标细看。
            for (i, sym) in ["compass", "music", "settings", "calendar"]
                .iter()
                .enumerate()
            {
                let big = ink::app_icon(304, &icon_spec(theme, i, sym));
                let mut bg = ink::paper(theme, 340, 340, 1);
                draw(&mut bg, &big, 18, 18);
                save(&bg, &dir, &format!("icon-{tn}-{sym}-304.png"));
            }
        }
        if want("status") {
            let pal = theme.palette();
            let mut pm = ink::paper(theme, 760, 260, 2);
            let mut x = 16;
            for (hgt, y) in [(14u32, 16), (18, 50), (28, 100), (36, 160)] {
                x = 16;
                for (lvl, ch) in [(1.0, false), (0.55, false), (0.15, false), (0.6, true)] {
                    let b = ink::battery(theme, hgt, lvl, ch);
                    draw(&mut pm, &b, x, y);
                    x += b.width() as i32 + 14;
                }
                for s in 0..=3 {
                    let wf = ink::wifi(theme, hgt, s);
                    draw(&mut pm, &wf, x, y);
                    x += wf.width() as i32 + 12;
                }
                let t = label(&mut text, "9:41", hgt as f32 * 1.05, pal.ink(Ink::Jiao));
                draw(&mut pm, &t, x + 8, y - (hgt as f32 * 0.2) as i32);
            }
            let _ = x;
            save(&pm, &dir, &format!("status-{tn}.png"));
            save(&scaled(&pm, 2.0), &dir, &format!("status-{tn}-zoom.png"));
        }
        if want("seal") {
            let mut pm = ink::paper(theme, 760, 240, 3);
            let mut x = 20;
            for (size, txt) in [
                (40u32, "龙"),
                (64, "墨韵"),
                (96, "山水"),
                (128, "长乐未央"),
                (160, "墨韵"),
                (192, "龙"),
            ] {
                let s = ink::seal(theme, size, txt, size);
                draw(&mut pm, &s, x, 220 - size as i32);
                x += size as i32 + 18;
            }
            save(&pm, &dir, &format!("seal-{tn}.png"));
        }
        if want("sheet") {
            for (sym, pig) in [("music", None), ("cloud-sun", None), ("map", None)] {
                let pig = pig.unwrap_or(ink::suggested_pigment(sym));
                let t = Instant::now();
                let sheet = ink::app_sheet(theme, w, h, builtin_symbol(sym).unwrap(), pig, 4);
                println!("  册页 {sym} {:?}", t.elapsed());
                save(
                    &scaled(&sheet, 0.5),
                    &dir,
                    &format!("sheet-{tn}-{sym}-half.png"),
                );
            }
        }
        if want("brush") {
            // 笔触样张：同一符号在不同尺寸下的笔意。
            let pal = theme.palette();
            let mut pm = ink::paper(theme, 1500, 420, 5);
            let mut x = 20.0;
            for size in [48.0f32, 96.0, 180.0, 380.0] {
                let r = Rect::from_xywh(x, 20.0, size, size).unwrap();
                ink::brush_symbol(
                    &mut pm,
                    builtin_symbol("languages").unwrap(),
                    r,
                    pal.ink(Ink::Jiao),
                    1.0,
                    7,
                );
                x += size + 30.0;
            }
            let r = Rect::from_xywh(x, 20.0, 380.0, 380.0).unwrap();
            ink::brush_symbol(
                &mut pm,
                builtin_symbol("settings").unwrap(),
                r,
                pal.ink(Ink::Jiao),
                1.0,
                9,
            );
            save(&pm, &dir, &format!("brush-{tn}.png"));
        }
    }
}
