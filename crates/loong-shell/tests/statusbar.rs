//! 集成测试：自适应状态栏（Loong 私有协议 loong_app_v1 + 取样）在无屏幕 loong-shell 中的效果。
//!
//! 测试程序本身兼作客户端（`statusbar --client <日志> <app_id> <RRGGBB> [--theme AARRGGBB]
//! [--style auto|dark|light] [--dmabuf]`）：一个全屏 toplevel，整窗一种颜色；按参数通过
//! loong_app_v1 告诉合成器主题色、状态栏样式，或者什么也不说（只能靠合成器取样），`--dmabuf`
//! 时画面经 linux-dmabuf 提交（合成器不回读 GPU 缓冲，只能退回系统外观）。
//!
//! 两次运行（系统夜墨、系统宣纸），每次依次打开几个应用、截图，检查状态栏字形的墨：
//!
//! | 系统 | 应用画面 | 协议 | 期望 | 依据 |
//! |---|---|---|---|---|
//! | 夜墨 | 亮 | 主题色（亮） | 浓墨 | 主题色 |
//! | 夜墨 | 亮 | 无 | 浓墨 | 取样 |
//! | 夜墨 | 暗 | 主题色（亮） | 浓墨 | 主题色优先于取样 |
//! | 夜墨 | 暗 | 主题色（暗） | 月白 | 主题色；随后经多任务切到第一个应用（交叉淡化）|
//! | 宣纸 | 暗 | 主题色（暗） | 月白 | 主题色 |
//! | 宣纸 | 暗 | 无 | 月白 | 取样 |
//! | 宣纸 | 暗 | 无，dmabuf | 浓墨 | 系统外观（不回读 GPU 缓冲）|
//! | 宣纸 | 亮 | 样式 light_content + 主题色（亮） | 月白 | 样式优先于主题色 |
//!
//! 没有可用的 EGL 时跳过；`LOONG_SHELL_REQUIRE_EGL=1` 时视为失败。
//! 截图与日志保存在 `$CARGO_TARGET_TMPDIR/statusbar/`。

use std::{
    fs::File,
    io::{BufReader, Write},
    os::fd::{AsFd, FromRawFd, OwnedFd},
    path::{Path, PathBuf},
    process::Command,
};

use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
};
use wayland_protocols::{
    wp::linux_dmabuf::zv1::client::{
        zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
        zwp_linux_dmabuf_v1::{self, ZwpLinuxDmabufV1},
    },
    xdg::shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::{self, XdgToplevel},
        xdg_wm_base::{self, XdgWmBase},
    },
};

/// loong_app_v1 的客户端代码（与 loong-shell 用同一份 XML）。
#[allow(
    missing_docs,
    non_upper_case_globals,
    non_camel_case_types,
    unused_imports,
    clippy::all
)]
mod loong_app {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("../../protocols/loong-app-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("../../protocols/loong-app-v1.xml");
}

use loong_app::{
    loong_app_manager_v1::LoongAppManagerV1,
    loong_app_surface_v1::{LoongAppSurfaceV1, StatusBarStyle},
};

const SCREEN: (u32, u32) = (1280, 800);
/// 状态栏的两种墨（宣纸 / 夜墨调色板的浓墨级）。
const NONG: u32 = 0x2A2824;
const YUEBAI: u32 = 0xCFC8B8;

const LIGHT_PAGE: u32 = 0xF6F2E8;
const WHITE_PAGE: u32 = 0xFAF9F6;
const DARK_PAGE: u32 = 0x1C1B17;
const NIGHT_PAGE: u32 = 0x14171B;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--client") {
        client::run(&args[1..]);
        return;
    }
    let filters: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if !filters.is_empty() && !filters.iter().any(|f| "statusbar".contains(f.as_str())) {
        return;
    }
    driver::run();
}

// ───────────────────────── 测试驱动 ─────────────────────────

mod driver {
    use super::*;

    /// 一个应用：（应用 id, app_id, 画面颜色, 其余客户端参数）。
    struct App {
        id: &'static str,
        app_id: &'static str,
        pixels: u32,
        extra: &'static str,
    }

    /// 一次截图检查：（截图时刻，画面颜色，期望的墨，是否要求可读）。
    struct Expect {
        at: u64,
        what: &'static str,
        background: u32,
        ink: u32,
    }

    pub fn run() {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("statusbar");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // 系统夜墨：亮应用（主题色 / 取样）用浓墨；暗画面但主题色是亮的：主题色优先；
        // 最后从暗应用经多任务切回亮应用（录下交叉淡化）。
        let dark = [
            App {
                id: "browser",
                app_id: "loong.browser",
                pixels: LIGHT_PAGE,
                extra: "--theme FFF6F2E8",
            },
            App {
                id: "settings",
                app_id: "loong.settings",
                pixels: WHITE_PAGE,
                extra: "",
            },
            App {
                id: "calendar",
                app_id: "loong.calendar",
                pixels: NIGHT_PAGE,
                extra: "--theme FFF6F2E8",
            },
            App {
                id: "notes",
                app_id: "loong.notes",
                pixels: DARK_PAGE,
                extra: "--theme FF1C1B17",
            },
        ];
        // 打开第 i 个应用在 100 + 3600·i 毫秒，1.8 秒后截图；最后一个应用之后进入多任务，
        // 再从多任务打开第一个应用。
        let script = format!(
            r#"{{
  "fps": 30,
  "duration_ms": 17000,
  "clock": "9:41",
  "status": {{ "battery": 76, "wifi": 80, "ssid": "听雨轩" }},
  "theme": "dark",
  "events": [
    {apps},
    {{ "at": 12900, "swipe": {{ "from": [640, 795], "to": [640, 600], "duration": 250, "hold": 400 }} }},
    {{ "at": 14600, "launch": "browser" }}
  ],
  "screenshots": [1900, 3300, 5500, 9100, 12700, 14400, 16800]
}}"#,
            apps = launches(&dark)
        );
        let checks = [
            Expect {
                at: 1900,
                what: "夜墨 + 亮应用（主题色）",
                background: LIGHT_PAGE,
                ink: NONG,
            },
            Expect {
                at: 3300,
                what: "夜墨主屏",
                background: 0,
                ink: YUEBAI,
            },
            Expect {
                at: 5500,
                what: "夜墨 + 亮应用（取样）",
                background: WHITE_PAGE,
                ink: NONG,
            },
            Expect {
                at: 9100,
                what: "夜墨 + 暗画面、亮主题色（主题色优先于取样）",
                background: NIGHT_PAGE,
                ink: NONG,
            },
            Expect {
                at: 12700,
                what: "夜墨 + 暗应用（主题色）",
                background: DARK_PAGE,
                ink: YUEBAI,
            },
            Expect {
                at: 14400,
                what: "多任务",
                background: 0,
                ink: YUEBAI,
            },
            Expect {
                at: 16800,
                what: "从多任务切回亮应用",
                background: LIGHT_PAGE,
                ink: NONG,
            },
        ];
        let Some(out) = run_shell(&root.join("dark"), &script, &dark) else {
            return;
        };
        check(&out, &checks);

        // 系统宣纸：暗应用（主题色 / 取样）用月白；dmabuf 不取样，退回系统外观（浓墨）；
        // 样式优先于主题色。
        let light = [
            App {
                id: "browser",
                app_id: "loong.browser",
                pixels: DARK_PAGE,
                extra: "--theme FF1C1B17",
            },
            App {
                id: "settings",
                app_id: "loong.settings",
                pixels: NIGHT_PAGE,
                extra: "",
            },
            App {
                id: "mail",
                app_id: "loong.mail",
                pixels: NIGHT_PAGE,
                extra: "--dmabuf",
            },
            App {
                id: "music",
                app_id: "loong.music",
                pixels: LIGHT_PAGE,
                extra: "--theme FFF6F2E8 --style light",
            },
        ];
        let script = format!(
            r#"{{
  "fps": 30,
  "duration_ms": 12900,
  "clock": "9:41",
  "status": {{ "battery": 76, "wifi": 80, "ssid": "听雨轩" }},
  "theme": "light",
  "events": [
    {apps}
  ],
  "screenshots": [1900, 3300, 5500, 9100, 12700]
}}"#,
            apps = launches(&light)
        );
        let Some(out) = run_shell(&root.join("light"), &script, &light) else {
            return;
        };
        let mail_path = std::fs::read_to_string(out.join("mail.log")).unwrap_or_default();
        let dmabuf = mail_path.contains("dmabuf");
        println!("mail 的缓冲：{}", if dmabuf { "dmabuf" } else { "shm" });
        let checks = [
            Expect {
                at: 1900,
                what: "宣纸 + 暗应用（主题色）",
                background: DARK_PAGE,
                ink: YUEBAI,
            },
            Expect {
                at: 3300,
                what: "宣纸主屏",
                background: 0,
                ink: NONG,
            },
            Expect {
                at: 5500,
                what: "宣纸 + 暗应用（取样）",
                background: NIGHT_PAGE,
                ink: YUEBAI,
            },
            Expect {
                at: 9100,
                what: "宣纸 + 暗应用（dmabuf，不取样 → 系统外观）",
                background: NIGHT_PAGE,
                // 合成器不支持 dmabuf 时客户端退回共享内存，这时会取样。
                ink: if dmabuf { NONG } else { YUEBAI },
            },
            Expect {
                at: 12700,
                what: "宣纸 + 亮应用、样式 light_content（样式优先于主题色）",
                background: LIGHT_PAGE,
                ink: YUEBAI,
            },
        ];
        check(&out, &checks);
        println!("通过（截图与日志：{}）", root.display());
    }

    /// 依次打开各应用：第 i 个在 100 + 3600·i 毫秒打开，1.8 秒后截图，1.9 秒时上滑回主屏
    /// （回主屏后 1.4 秒截主屏），3.6 秒后打开下一个。
    fn launches(apps: &[App]) -> String {
        let mut events = Vec::new();
        for (i, app) in apps.iter().enumerate() {
            let t = 100 + i as u64 * 3600;
            events.push(format!(r#"{{ "at": {t}, "launch": "{}" }}"#, app.id));
            if i + 1 < apps.len() {
                events.push(format!(
                    r#"{{ "at": {}, "swipe": {{ "from": [640, 795], "to": [640, 430], "duration": 170 }} }}"#,
                    t + 1900
                ));
            }
        }
        events.join(",\n    ")
    }

    /// 运行一次无屏幕 loong-shell，返回截图目录（没有 EGL 时为 `None`）。
    fn run_shell(dir: &Path, script: &str, apps: &[App]) -> Option<PathBuf> {
        std::fs::create_dir_all(dir).unwrap();
        let script_path = dir.join("script.json");
        std::fs::write(&script_path, script).unwrap();
        let out = dir.join("out");
        let me = std::env::current_exe().unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_loong-shell"));
        cmd.args(["--headless", "--realtime", "--size"])
            .arg(format!("{}x{}", SCREEN.0, SCREEN.1))
            .arg("--script")
            .arg(&script_path)
            .arg("--out")
            .arg(&out)
            .env("LOONG_APPS", dir.join("no-apps.toml"))
            .env_remove("LOONG_THEME")
            .env_remove("LOONG_REDUCED_MOTION");
        for app in apps {
            let log = out.join(format!("{}.log", app.id));
            cmd.arg("--app").arg(format!(
                "{}={} --client {} {} {:06X} {}",
                app.id,
                me.display(),
                log.display(),
                app.app_id,
                app.pixels,
                app.extra
            ));
        }
        std::fs::create_dir_all(&out).unwrap();
        let output = cmd.output().expect("启动 loong-shell");
        std::fs::write(
            dir.join("shell.log"),
            [output.stdout.as_slice(), output.stderr.as_slice()].concat(),
        )
        .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            if stderr.contains("EGL") && std::env::var_os("LOONG_SHELL_REQUIRE_EGL").is_none() {
                println!(
                    "跳过：没有可用的 EGL（{}）",
                    dir.join("shell.log").display()
                );
                return None;
            }
            panic!("loong-shell 失败：{stderr}");
        }
        Some(out)
    }

    fn check(out: &Path, checks: &[Expect]) {
        for e in checks {
            let img = Image::load(&out.join(format!("frame-{:05}.png", e.at)));
            for (region, name) in [(img.time_region(), "时间"), (img.icons_region(), "图标")] {
                let (glyph, background) = img.glyph(region, e.background);
                let nearest = if distance(glyph, NONG) <= distance(glyph, YUEBAI) {
                    NONG
                } else {
                    YUEBAI
                };
                assert_eq!(
                    nearest, e.ink,
                    "{}（{} ms，{name}）：字形颜色 #{glyph:06x}，背景 #{background:06x}",
                    e.what, e.at
                );
                if e.background != 0 {
                    let ratio = contrast(glyph, background);
                    assert!(
                        ratio >= 3.0 || e.ink != ink_for(background),
                        "{}（{name}）：对比度只有 {ratio:.1}",
                        e.what
                    );
                }
            }
        }
    }

    /// 对比度更高的墨。
    fn ink_for(rgb: u32) -> u32 {
        if contrast(rgb, NONG) >= contrast(rgb, YUEBAI) {
            NONG
        } else {
            YUEBAI
        }
    }

    fn luminance(rgb: u32) -> f64 {
        let lin = |c: u32| {
            let v = (c & 0xff) as f64 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(rgb >> 16) + 0.7152 * lin(rgb >> 8) + 0.0722 * lin(rgb)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    fn distance(a: u32, b: u32) -> u32 {
        [16, 8, 0]
            .iter()
            .map(|s| ((a >> s) & 0xff).abs_diff((b >> s) & 0xff))
            .sum()
    }

    /// RGB 截图。
    struct Image {
        w: u32,
        rgb: Vec<u8>,
    }

    impl Image {
        fn load(path: &Path) -> Image {
            let file = BufReader::new(
                File::open(path).unwrap_or_else(|e| panic!("截图 {}：{e}", path.display())),
            );
            let mut reader = png::Decoder::new(file).read_info().unwrap();
            let mut buf = vec![0; reader.output_buffer_size().unwrap()];
            let info = reader.next_frame(&mut buf).unwrap();
            buf.truncate(info.buffer_size());
            Image {
                w: info.width,
                rgb: buf,
            }
        }

        fn pixel(&self, x: u32, y: u32) -> u32 {
            let i = ((y * self.w + x) * 3) as usize;
            u32::from(self.rgb[i]) << 16
                | u32::from(self.rgb[i + 1]) << 8
                | u32::from(self.rgb[i + 2])
        }

        /// 状态栏时间所在的区域（x 20pt 起，字高约 20pt）。
        fn time_region(&self) -> (u32, u32, u32, u32) {
            (20, 2, 80, 24)
        }

        /// 状态栏右侧图标所在的区域（右对齐到 w − 18pt）。
        fn icons_region(&self) -> (u32, u32, u32, u32) {
            (self.w - 130, 2, self.w - 18, 24)
        }

        /// 区域内与背景差别最大的像素（字形的墨最浓处）与背景色。`background` 为 0 时
        /// 取区域内最常见的颜色作背景（主屏、多任务）。
        fn glyph(&self, (x0, y0, x1, y1): (u32, u32, u32, u32), background: u32) -> (u32, u32) {
            let background = if background != 0 {
                background
            } else {
                let mut counts = std::collections::HashMap::new();
                for y in y0..y1 {
                    for x in x0..x1 {
                        *counts.entry(self.pixel(x, y) & 0xF0F0F0).or_insert(0) += 1;
                    }
                }
                let (&c, _) = counts.iter().max_by_key(|(_, n)| **n).unwrap();
                c | 0x080808
            };
            let mut best = (background, 0);
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = self.pixel(x, y);
                    let d = distance(p, background);
                    if d > best.1 {
                        best = (p, d);
                    }
                }
            }
            (best.0, background)
        }
    }
}

// ───────────────────────── 测试客户端 ─────────────────────────

mod client {
    use super::*;

    /// DRM_FORMAT_XRGB8888（'XR24'）：与 wl_shm 的 XRGB8888 内存布局相同。
    const DRM_XRGB8888: u32 = 0x3432_5258;
    const DRM_MOD_LINEAR: u64 = 0;

    #[derive(Default)]
    struct App {
        compositor: Option<WlCompositor>,
        shm: Option<WlShm>,
        wm_base: Option<XdgWmBase>,
        dmabuf: Option<ZwpLinuxDmabufV1>,
        dmabuf_formats: Vec<(u32, u64)>,
        dmabuf_result: Option<Option<WlBuffer>>,
        app_manager: Option<LoongAppManagerV1>,
        size: (i32, i32),
        configured: bool,
        closed: bool,
    }

    struct Options {
        log: PathBuf,
        app_id: String,
        pixels: u32,
        theme: Option<u32>,
        style: Option<StatusBarStyle>,
        dmabuf: bool,
    }

    fn parse(args: &[String]) -> Options {
        let hex = |s: &str| u32::from_str_radix(s, 16).expect("十六进制颜色");
        let mut opts = Options {
            log: PathBuf::from(&args[0]),
            app_id: args[1].clone(),
            pixels: hex(&args[2]),
            theme: None,
            style: None,
            dmabuf: false,
        };
        let mut it = args[3..].iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--theme" => opts.theme = Some(hex(it.next().expect("--theme 颜色"))),
                "--style" => {
                    opts.style = Some(match it.next().map(String::as_str) {
                        Some("dark") => StatusBarStyle::DarkContent,
                        Some("light") => StatusBarStyle::LightContent,
                        _ => StatusBarStyle::Auto,
                    })
                }
                "--dmabuf" => opts.dmabuf = true,
                other => panic!("未知参数 {other}"),
            }
        }
        opts
    }

    pub fn run(args: &[String]) {
        let opts = parse(args);
        let mut log = File::create(&opts.log).expect("日志文件");
        let conn = Connection::connect_to_env().expect("连接 Wayland");
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let mut app = App::default();
        conn.display().get_registry(&qh, ());
        queue.roundtrip(&mut app).unwrap();
        queue.roundtrip(&mut app).unwrap();

        let compositor = app.compositor.clone().expect("wl_compositor");
        let wm_base = app.wm_base.clone().expect("xdg_wm_base");
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_app_id(opts.app_id.clone());
        toplevel.set_title("状态栏测试".into());

        // 私有协议：先于第一帧画面告诉合成器（状态立即生效，不随提交）。
        let mut protocol = "none";
        let _app_surface = if opts.theme.is_some() || opts.style.is_some() {
            let manager = app.app_manager.clone().expect("loong_app_manager_v1");
            let app_surface = manager.get_app_surface(&surface, &qh, ());
            if let Some(argb) = opts.theme {
                app_surface.set_theme_color(argb);
            }
            if let Some(style) = opts.style {
                app_surface.set_status_bar_style(style);
            }
            protocol = "loong_app_v1";
            Some(app_surface)
        } else {
            None
        };

        surface.commit();
        while !app.configured {
            queue.blocking_dispatch(&mut app).unwrap();
        }
        let (w, h) = match app.size {
            (w, h) if w > 0 && h > 0 => (w, h),
            _ => (SCREEN.0 as i32, SCREEN.1 as i32),
        };
        let mut path = "shm";
        let buffer = match opts
            .dmabuf
            .then(|| dmabuf_buffer(&mut app, &mut queue, w, h, opts.pixels))
            .flatten()
        {
            Some(b) => {
                path = "dmabuf";
                b
            }
            None => shm_buffer(&app, &qh, w, h, opts.pixels),
        };
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w, h);
        surface.commit();
        queue.roundtrip(&mut app).unwrap();
        let _ = writeln!(log, "ready {w} {h} {path} {protocol}");
        let _ = log.flush();
        while !app.closed {
            if queue.blocking_dispatch(&mut app).is_err() {
                break;
            }
        }
    }

    fn memfd(size: usize, color: u32) -> OwnedFd {
        // SAFETY: memfd_create 返回新的文件描述符（失败时为 -1，下面检查）。
        let fd = unsafe { libc::memfd_create(c"loong-test".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(fd >= 0, "memfd_create 失败");
        // SAFETY: fd 是刚创建、只归这里所有的描述符。
        let mut file = unsafe { File::from_raw_fd(fd) };
        let pixel = (0xFF00_0000 | color).to_le_bytes();
        let data: Vec<u8> = pixel.iter().copied().cycle().take(size).collect();
        file.write_all(&data).unwrap();
        file.into()
    }

    /// 不带 alpha 的 XRGB8888：合成器取样时按不透明处理。
    fn shm_buffer(app: &App, qh: &QueueHandle<App>, w: i32, h: i32, color: u32) -> WlBuffer {
        let size = (w * h * 4) as usize;
        let fd = memfd(size, color);
        let pool = app
            .shm
            .as_ref()
            .expect("wl_shm")
            .create_pool(fd.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Xrgb8888, qh, ());
        pool.destroy();
        buffer
    }

    /// 以 memfd 作为单平面线性 dmabuf 提交（合成器用渲染器导入；失败返回 None）。
    fn dmabuf_buffer(
        app: &mut App,
        queue: &mut wayland_client::EventQueue<App>,
        w: i32,
        h: i32,
        color: u32,
    ) -> Option<WlBuffer> {
        let dmabuf = app.dmabuf.clone()?;
        if !app.dmabuf_formats.contains(&(DRM_XRGB8888, DRM_MOD_LINEAR)) {
            return None;
        }
        let qh = queue.handle();
        let fd = memfd((w * h * 4) as usize, color);
        let params = dmabuf.create_params(&qh, ());
        params.add(
            fd.as_fd(),
            0,
            0,
            (w * 4) as u32,
            (DRM_MOD_LINEAR >> 32) as u32,
            DRM_MOD_LINEAR as u32,
        );
        app.dmabuf_result = None;
        params.create(
            w,
            h,
            DRM_XRGB8888,
            zwp_linux_buffer_params_v1::Flags::empty(),
        );
        while app.dmabuf_result.is_none() {
            queue.blocking_dispatch(app).ok()?;
        }
        params.destroy();
        app.dmabuf_result.take().flatten()
    }

    impl Dispatch<WlRegistry, ()> for App {
        fn event(
            app: &mut Self,
            registry: &WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = event
            else {
                return;
            };
            match interface.as_str() {
                "wl_compositor" => {
                    app.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => app.wm_base = Some(registry.bind(name, 1, qh, ())),
                "zwp_linux_dmabuf_v1" if version >= 3 => {
                    app.dmabuf = Some(registry.bind(name, 3, qh, ()));
                }
                "loong_app_manager_v1" => {
                    app.app_manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }

    impl Dispatch<XdgWmBase, ()> for App {
        fn event(
            _: &mut Self,
            wm_base: &XdgWmBase,
            event: xdg_wm_base::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let xdg_wm_base::Event::Ping { serial } = event {
                wm_base.pong(serial);
            }
        }
    }

    impl Dispatch<XdgSurface, ()> for App {
        fn event(
            app: &mut Self,
            surface: &XdgSurface,
            event: xdg_surface::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let xdg_surface::Event::Configure { serial } = event {
                surface.ack_configure(serial);
                app.configured = true;
            }
        }
    }

    impl Dispatch<XdgToplevel, ()> for App {
        fn event(
            app: &mut Self,
            _: &XdgToplevel,
            event: xdg_toplevel::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                xdg_toplevel::Event::Configure { width, height, .. } => {
                    app.size = (width, height);
                }
                xdg_toplevel::Event::Close => app.closed = true,
                _ => {}
            }
        }
    }

    impl Dispatch<ZwpLinuxDmabufV1, ()> for App {
        fn event(
            app: &mut Self,
            _: &ZwpLinuxDmabufV1,
            event: zwp_linux_dmabuf_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let zwp_linux_dmabuf_v1::Event::Modifier {
                format,
                modifier_hi,
                modifier_lo,
            } = event
            {
                let modifier = (u64::from(modifier_hi) << 32) | u64::from(modifier_lo);
                app.dmabuf_formats.push((format, modifier));
            }
        }
    }

    impl Dispatch<ZwpLinuxBufferParamsV1, ()> for App {
        fn event(
            app: &mut Self,
            _: &ZwpLinuxBufferParamsV1,
            event: zwp_linux_buffer_params_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                zwp_linux_buffer_params_v1::Event::Created { buffer } => {
                    app.dmabuf_result = Some(Some(buffer));
                }
                zwp_linux_buffer_params_v1::Event::Failed => app.dmabuf_result = Some(None),
                _ => {}
            }
        }

        wayland_client::event_created_child!(App, ZwpLinuxBufferParamsV1, [
            zwp_linux_buffer_params_v1::EVT_CREATED_OPCODE => (WlBuffer, ()),
        ]);
    }

    delegate_noop!(App: ignore WlCompositor);
    delegate_noop!(App: ignore WlSurface);
    delegate_noop!(App: ignore WlShm);
    delegate_noop!(App: ignore WlShmPool);
    delegate_noop!(App: ignore WlBuffer);
    delegate_noop!(App: LoongAppManagerV1);
    delegate_noop!(App: LoongAppSurfaceV1);
}
