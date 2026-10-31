//! 集成测试：带子表面的真实 Wayland 客户端在无屏幕 loong-shell 中的渲染与触摸路由。
//!
//! 测试程序本身兼作客户端（`subsurface --client <日志> [--dmabuf]`）：一个全屏 toplevel，
//! 根表面（紫）之上有工具栏子表面（红，贴窗口顶边）、页面子表面（绿），页面里再嵌一个
//! 内部子表面（蓝）；每次触摸记一行日志（表面名与表面局部坐标）。`--dmabuf` 时工具栏的
//! 画面通过 linux-dmabuf 提交（memfd，线性布局；Mesa llvmpipe 可以导入），合成器不支持时
//! 退回 wl_shm。
//!
//! 检查：
//! - 每个表面收到落在它上面的点按，坐标相对该表面（含嵌套子表面的偏移）；
//! - 在工具栏上按下后拖到页面区域，移动事件仍然交给工具栏（隐式抓取），坐标超出工具栏；
//! - 上滑回主屏的过程中（窗口正随手指缩小）第二根手指的点按，按窗口此刻的缩放落到画面上
//!   对应的表面、对应的位置（与同一帧的截图比对）；
//! - 全屏时窗口四角的圆角裁掉贴边子表面的角，内部子表面的直角保持完整。
//!
//! 没有可用的 EGL（例如没有安装 Mesa）时跳过；`LOONG_SHELL_REQUIRE_EGL=1` 时视为失败。
//! 截图与日志保存在 `$CARGO_TARGET_TMPDIR/subsurface/`。

use std::{
    fs::File,
    io::{BufReader, Write},
    os::fd::{AsFd, FromRawFd, OwnedFd},
    path::{Path, PathBuf},
    process::Command,
};

use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_subcompositor::WlSubcompositor,
        wl_subsurface::WlSubsurface,
        wl_surface::WlSurface,
        wl_touch::{self, WlTouch},
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

// ───────────────────────── 布局（窗口坐标，逻辑点）─────────────────────────

const SCREEN: (i32, i32) = (1280, 800);
const TOOLBAR_H: i32 = 64;
/// 窗口底部露出根表面的高度。
const ROOT_STRIP: i32 = 48;
const INNER: (i32, i32) = (300, 200);
const CORNER_RADIUS: f64 = 36.0;

// 颜色都远离主屏水墨画面的灰阶，圆角处露出的背景不会被误认为某个表面。
const ROOT_COLOR: u32 = 0xFF6A_1B9A;
const TOOLBAR_COLOR: u32 = 0xFFC8_102E;
const PAGE_COLOR: u32 = 0xFF2E_8B57;
const INNER_COLOR: u32 = 0xFF1E_5AA8;

/// 各表面在窗口中的矩形（x, y, w, h），由窗口尺寸决定。
struct Layout {
    toolbar: (i32, i32, i32, i32),
    page: (i32, i32, i32, i32),
    /// 相对页面子表面。
    inner_in_page: (i32, i32),
}

impl Layout {
    fn new(w: i32, h: i32) -> Self {
        let page_h = h - TOOLBAR_H - ROOT_STRIP;
        Layout {
            toolbar: (0, 0, w, TOOLBAR_H),
            page: (0, TOOLBAR_H, w, page_h),
            inner_in_page: (w / 2 - INNER.0 / 2, page_h / 2 - INNER.1 / 2),
        }
    }

    /// 内部子表面的窗口坐标。
    fn inner(&self) -> (i32, i32, i32, i32) {
        (
            self.page.0 + self.inner_in_page.0,
            self.page.1 + self.inner_in_page.1,
            INNER.0,
            INNER.1,
        )
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--client") {
        let log = args.get(1).expect("--client 需要日志路径");
        let dmabuf = args.iter().any(|a| a == "--dmabuf");
        client::run(Path::new(log), dmabuf);
        return;
    }
    // cargo test 的过滤参数：与本测试无关时跳过。
    let filters: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if !filters.is_empty() && !filters.iter().any(|f| "subsurface".contains(f.as_str())) {
        return;
    }
    driver::run();
}

// ───────────────────────── 测试驱动 ─────────────────────────

mod driver {
    use super::*;

    const SCRIPT: &str = r#"{
  "fps": 30,
  "duration_ms": 6000,
  "clock": "9:41",
  "events": [
    { "at": 100, "launch": "browser" },
    { "at": 2600, "tap": [100, 30] },
    { "at": 2800, "tap": [100, 300] },
    { "at": 3000, "tap": [700, 400] },
    { "at": 3200, "tap": [100, 765] },
    { "at": 3400, "swipe": { "from": [300, 40], "to": [300, 200], "duration": 300 } },
    { "at": 4200, "swipe": { "from": [640, 795], "to": [640, 395], "duration": 1200 } },
    { "at": 4790, "tap": [640, 380] }
  ],
  "screenshots": [2500, 4800]
}"#;
    /// 上滑过程中第二根手指点按的位置与时刻（与上面的脚本一致）。
    const SCALED_TAP: (u32, u32) = (640, 380);

    pub fn run() {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("subsurface");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let device = dir.join("device.toml");
        std::fs::write(
            &device,
            format!(
                "name = \"测试平板\"\n[screen]\nwidth = {}\nheight = {}\nscale = 1.0\ncorner_radius = {CORNER_RADIUS}\n",
                SCREEN.0, SCREEN.1
            ),
        )
        .unwrap();
        let script = dir.join("script.json");
        std::fs::write(&script, SCRIPT).unwrap();
        let log = dir.join("client.log");
        let out = dir.join("out");
        let me = std::env::current_exe().unwrap();
        let app = format!(
            "browser={} --client {} --dmabuf",
            me.display(),
            log.display()
        );

        let output = Command::new(env!("CARGO_BIN_EXE_loong-shell"))
            .args(["--headless", "--realtime", "--device"])
            .arg(&device)
            .arg("--script")
            .arg(&script)
            .arg("--out")
            .arg(&out)
            .args(["--app", &app])
            .env("LOONG_APPS", dir.join("no-apps.toml"))
            .env("LOONG_THEME", "light")
            .env_remove("LOONG_REDUCED_MOTION")
            .output()
            .expect("启动 loong-shell");
        let stderr = String::from_utf8_lossy(&output.stderr);
        std::fs::write(
            dir.join("shell.log"),
            [output.stdout.as_slice(), output.stderr.as_slice()].concat(),
        )
        .unwrap();
        if !output.status.success() {
            if stderr.contains("EGL") && std::env::var_os("LOONG_SHELL_REQUIRE_EGL").is_none() {
                println!(
                    "跳过：没有可用的 EGL（{}）",
                    dir.join("shell.log").display()
                );
                return;
            }
            panic!("loong-shell 失败：{stderr}");
        }

        let events = parse_log(&std::fs::read_to_string(&log).expect("客户端日志"));
        let (w, h, toolbar_path) = events
            .iter()
            .find_map(|e| match e {
                Ev::Ready { w, h, toolbar } => Some((*w, *h, toolbar.clone())),
                _ => None,
            })
            .expect("客户端没有完成首次提交");
        assert_eq!((w, h), SCREEN, "全屏窗口尺寸");
        println!("工具栏缓冲：{toolbar_path}");
        let layout = Layout::new(w, h);
        let downs: Vec<&Ev> = events
            .iter()
            .filter(|e| matches!(e, Ev::Down { .. }))
            .collect();
        assert!(downs.len() >= 6, "收到的按下事件太少：{events:?}");

        // 全屏时的四次点按：工具栏、页面、内部子表面（嵌在页面里）、根表面。
        let inner = layout.inner();
        expect_down(downs[0], "toolbar", (100.0, 30.0));
        expect_down(downs[1], "page", (100.0, 300.0 - layout.page.1 as f64));
        expect_down(
            downs[2],
            "inner",
            (700.0 - inner.0 as f64, 400.0 - inner.1 as f64),
        );
        expect_down(downs[3], "root", (100.0, 765.0));

        // 隐式抓取：在工具栏上按下，拖到页面区域，移动事件仍交给工具栏（坐标超出工具栏高度）。
        expect_down(downs[4], "toolbar", (300.0, 40.0));
        let grab: Vec<&Ev> = events
            .iter()
            .skip_while(|e| !std::ptr::eq(*e, downs[4]))
            .skip(1)
            .take_while(|e| !matches!(e, Ev::Up))
            .collect();
        assert!(!grab.is_empty(), "拖动没有移动事件");
        for e in &grab {
            let Ev::Motion { surface, x, .. } = e else {
                panic!("拖动中出现了意外事件 {e:?}");
            };
            assert_eq!(surface, "toolbar");
            assert!((x - 300.0).abs() < 0.5);
        }
        let Ev::Motion { y: last_y, .. } = grab.last().unwrap() else {
            unreachable!()
        };
        assert!(
            (last_y - 200.0).abs() < 0.5 && *last_y > TOOLBAR_H as f64,
            "拖到页面区域后仍按工具栏坐标：{last_y}"
        );

        // 窗口缩放中的第二根手指：与同一帧截图上画出的位置比对。
        let shot = Image::load(&out.join("frame-04800.png"));
        let (tx, ty) = SCALED_TAP;
        let hit = shot.classify(tx, ty);
        assert_eq!(
            hit,
            Some("inner"),
            "调整脚本中 SCALED_TAP，使它落在缩小的窗口的内部子表面上"
        );
        let bbox = shot.bbox(INNER_COLOR).expect("截图中找不到内部子表面");
        let k = (bbox.2 - bbox.0) as f64 / INNER.0 as f64;
        assert!(k < 0.95, "截图时窗口没有缩小（k = {k}）");
        let expected = (
            (tx as f64 + 0.5 - bbox.0 as f64) / k,
            (ty as f64 + 0.5 - bbox.1 as f64) / k,
        );
        let Ev::Down { surface, x, y } = downs[5] else {
            unreachable!()
        };
        assert_eq!(surface, "inner", "缩放中的点按落错了表面");
        assert!(
            (x - expected.0).abs() < 4.0 / k && (y - expected.1).abs() < 4.0 / k,
            "缩放中的点按坐标 ({x:.1}, {y:.1})，截图推算 ({:.1}, {:.1})",
            expected.0,
            expected.1
        );

        // 圆角作用于整个窗口：贴边的工具栏、根表面的外角被裁掉，内部子表面的直角完整。
        let full = Image::load(&out.join("frame-02500.png"));
        let (w, h) = (w as u32, h as u32);
        assert_eq!(full.classify(w / 2, 20), Some("toolbar"));
        assert_eq!(full.classify(1, 1), None, "工具栏左上角应被窗口圆角裁掉");
        assert_eq!(
            full.classify(w - 2, 1),
            None,
            "工具栏右上角应被窗口圆角裁掉"
        );
        assert_eq!(
            full.classify(1, h - 2),
            None,
            "根表面左下角应被窗口圆角裁掉"
        );
        assert_eq!(full.classify(100, h - 20), Some("root"));
        let (ix, iy, iw, ih) = inner;
        for (x, y) in [
            (ix, iy),
            (ix + iw - 1, iy),
            (ix, iy + ih - 1),
            (ix + iw - 1, iy + ih - 1),
        ] {
            assert_eq!(
                full.classify(x as u32, y as u32),
                Some("inner"),
                "内部子表面的角 ({x}, {y}) 不应有圆角"
            );
        }
        // 圆角以外紧挨着的位置不被裁掉（窗口圆角只在四角）。
        let r = CORNER_RADIUS as u32;
        assert_eq!(full.classify(2 * r, 1), Some("toolbar"));
        println!("通过（截图与日志：{}）", dir.display());
    }

    fn expect_down(e: &Ev, want: &str, (wx, wy): (f64, f64)) {
        let Ev::Down { surface, x, y } = e else {
            panic!("应为按下事件：{e:?}");
        };
        assert_eq!(surface, want, "{e:?}");
        assert!(
            (x - wx).abs() < 0.5 && (y - wy).abs() < 0.5,
            "{want}：坐标 ({x}, {y})，应为 ({wx}, {wy})"
        );
    }

    #[derive(Debug)]
    enum Ev {
        Ready { w: i32, h: i32, toolbar: String },
        Down { surface: String, x: f64, y: f64 },
        Motion { surface: String, x: f64, y: f64 },
        Up,
    }

    fn parse_log(text: &str) -> Vec<Ev> {
        text.lines()
            .filter_map(|line| {
                let f: Vec<&str> = line.split_whitespace().collect();
                let num = |i: usize| f[i].parse::<f64>().unwrap();
                Some(match *f.first()? {
                    "ready" => Ev::Ready {
                        w: f[1].parse().unwrap(),
                        h: f[2].parse().unwrap(),
                        toolbar: f[3].to_string(),
                    },
                    "down" => Ev::Down {
                        surface: f[1].into(),
                        x: num(2),
                        y: num(3),
                    },
                    "motion" => Ev::Motion {
                        surface: f[1].into(),
                        x: num(2),
                        y: num(3),
                    },
                    "up" => Ev::Up,
                    _ => return None,
                })
            })
            .collect()
    }

    /// RGB 截图。
    struct Image {
        w: u32,
        h: u32,
        rgb: Vec<u8>,
    }

    impl Image {
        fn load(path: &Path) -> Image {
            let file = BufReader::new(File::open(path).expect("截图"));
            let mut reader = png::Decoder::new(file).read_info().unwrap();
            let mut buf = vec![0; reader.output_buffer_size().unwrap()];
            let info = reader.next_frame(&mut buf).unwrap();
            assert_eq!(info.color_type, png::ColorType::Rgb);
            buf.truncate(info.buffer_size());
            Image {
                w: info.width,
                h: info.height,
                rgb: buf,
            }
        }

        fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
            let i = ((y * self.w + x) * 3) as usize;
            [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
        }

        /// 该像素属于哪个表面（颜色足够接近才算）。
        fn classify(&self, x: u32, y: u32) -> Option<&'static str> {
            let p = self.pixel(x, y);
            [
                ("root", ROOT_COLOR),
                ("toolbar", TOOLBAR_COLOR),
                ("page", PAGE_COLOR),
                ("inner", INNER_COLOR),
            ]
            .into_iter()
            .find(|(_, c)| distance(p, *c) < 24)
            .map(|(name, _)| name)
        }

        /// 某种颜色的像素的包围盒 [x0, y0, x1, y1)。
        fn bbox(&self, color: u32) -> Option<(u32, u32, u32, u32)> {
            let mut b: Option<(u32, u32, u32, u32)> = None;
            for y in 0..self.h {
                for x in 0..self.w {
                    if distance(self.pixel(x, y), color) < 24 {
                        b = Some(match b {
                            None => (x, y, x + 1, y + 1),
                            Some((x0, y0, x1, y1)) => {
                                (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1))
                            }
                        });
                    }
                }
            }
            b
        }
    }

    fn distance(p: [u8; 3], argb: u32) -> u32 {
        let c = [(argb >> 16) as u8, (argb >> 8) as u8, argb as u8];
        p.iter()
            .zip(c)
            .map(|(a, b)| a.abs_diff(b) as u32)
            .max()
            .unwrap()
    }
}

// ───────────────────────── 测试客户端 ─────────────────────────

mod client {
    use super::*;

    /// DRM_FORMAT_ARGB8888（'AR24'）：与 wl_shm 的 ARGB8888 内存布局相同。
    const DRM_ARGB8888: u32 = 0x3432_5241;
    const DRM_MOD_LINEAR: u64 = 0;

    #[derive(Default)]
    struct App {
        compositor: Option<WlCompositor>,
        subcompositor: Option<WlSubcompositor>,
        shm: Option<WlShm>,
        wm_base: Option<XdgWmBase>,
        seat: Option<WlSeat>,
        dmabuf: Option<ZwpLinuxDmabufV1>,
        dmabuf_formats: Vec<(u32, u64)>,
        dmabuf_result: Option<Option<WlBuffer>>,
        touch: Option<WlTouch>,
        /// 收到的 toplevel 尺寸；xdg_surface.configure 之后才算配置完成。
        size: (i32, i32),
        configured: bool,
        closed: bool,
        surfaces: Vec<(WlSurface, &'static str)>,
        /// 触摸点 id → 按下时的表面名。
        touches: Vec<(i32, &'static str)>,
        log: Option<File>,
    }

    impl App {
        fn log(&mut self, line: String) {
            if let Some(f) = self.log.as_mut() {
                let _ = writeln!(f, "{line}");
                let _ = f.flush();
            }
        }

        fn name_of(&self, surface: &WlSurface) -> &'static str {
            self.surfaces
                .iter()
                .find(|(s, _)| s == surface)
                .map(|(_, n)| *n)
                .unwrap_or("unknown")
        }
    }

    pub fn run(log: &Path, want_dmabuf: bool) {
        let conn = Connection::connect_to_env().expect("连接 Wayland");
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let mut app = App {
            log: Some(File::create(log).expect("日志文件")),
            ..App::default()
        };
        conn.display().get_registry(&qh, ());
        queue.roundtrip(&mut app).unwrap();
        // 第二次往返：dmabuf 的格式事件、座位的能力事件。
        queue.roundtrip(&mut app).unwrap();

        let compositor = app.compositor.clone().expect("wl_compositor");
        let subcompositor = app.subcompositor.clone().expect("wl_subcompositor");
        let wm_base = app.wm_base.clone().expect("xdg_wm_base");

        let root = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&root, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_app_id("loong.test.subsurface".into());
        toplevel.set_title("子表面测试".into());
        root.commit();
        while !app.configured {
            queue.blocking_dispatch(&mut app).unwrap();
        }
        let (w, h) = match app.size {
            (w, h) if w > 0 && h > 0 => (w, h),
            _ => SCREEN,
        };
        let layout = Layout::new(w, h);

        let sub = |parent: &WlSurface, name: &'static str, app: &mut App| {
            let surface = compositor.create_surface(&qh, ());
            let subsurface: WlSubsurface = subcompositor.get_subsurface(&surface, parent, &qh, ());
            app.surfaces.push((surface.clone(), name));
            (surface, subsurface)
        };
        app.surfaces.push((root.clone(), "root"));
        let (toolbar, toolbar_sub) = sub(&root, "toolbar", &mut app);
        let (page, page_sub) = sub(&root, "page", &mut app);
        let (inner, inner_sub) = sub(&page, "inner", &mut app);
        toolbar_sub.set_position(layout.toolbar.0, layout.toolbar.1);
        page_sub.set_position(layout.page.0, layout.page.1);
        inner_sub.set_position(layout.inner_in_page.0, layout.inner_in_page.1);

        // 工具栏：尽量走 linux-dmabuf。
        let (tw, th) = (layout.toolbar.2, layout.toolbar.3);
        let mut toolbar_path = "shm";
        let toolbar_buffer = match want_dmabuf
            .then(|| dmabuf_buffer(&mut app, &mut queue, tw, th, TOOLBAR_COLOR))
            .flatten()
        {
            Some(b) => {
                toolbar_path = "dmabuf";
                b
            }
            None => shm_buffer(&app, &qh, tw, th, TOOLBAR_COLOR),
        };
        let attach = |surface: &WlSurface, buffer: &WlBuffer, w: i32, h: i32| {
            surface.attach(Some(buffer), 0, 0);
            surface.damage_buffer(0, 0, w, h);
            surface.commit();
        };
        // 子表面默认同步：状态在父表面提交时一起生效，所以先提交子表面，最后提交根表面。
        let inner_buffer = shm_buffer(&app, &qh, INNER.0, INNER.1, INNER_COLOR);
        attach(&inner, &inner_buffer, INNER.0, INNER.1);
        let page_buffer = shm_buffer(&app, &qh, layout.page.2, layout.page.3, PAGE_COLOR);
        attach(&page, &page_buffer, layout.page.2, layout.page.3);
        attach(&toolbar, &toolbar_buffer, tw, th);
        let root_buffer = shm_buffer(&app, &qh, w, h, ROOT_COLOR);
        attach(&root, &root_buffer, w, h);
        queue.roundtrip(&mut app).unwrap();
        app.log(format!("ready {w} {h} {toolbar_path}"));

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
        let pixel = color.to_le_bytes();
        let data: Vec<u8> = pixel.iter().copied().cycle().take(size).collect();
        file.write_all(&data).unwrap();
        file.into()
    }

    fn shm_buffer(app: &App, qh: &QueueHandle<App>, w: i32, h: i32, color: u32) -> WlBuffer {
        let size = (w * h * 4) as usize;
        let fd = memfd(size, color);
        let pool = app
            .shm
            .as_ref()
            .expect("wl_shm")
            .create_pool(fd.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, ());
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
        if !app.dmabuf_formats.contains(&(DRM_ARGB8888, DRM_MOD_LINEAR)) {
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
            DRM_ARGB8888,
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
                "wl_subcompositor" => app.subcompositor = Some(registry.bind(name, 1, qh, ())),
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => app.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => app.seat = Some(registry.bind(name, version.min(5), qh, ())),
                // v3：格式表随 modifier 事件下发（v4 的 feedback 由 WebKit 等使用）。
                "zwp_linux_dmabuf_v1" if version >= 3 => {
                    app.dmabuf = Some(registry.bind(name, 3, qh, ()));
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

    impl Dispatch<WlSeat, ()> for App {
        fn event(
            app: &mut Self,
            seat: &WlSeat,
            event: wl_seat::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_seat::Event::Capabilities {
                capabilities: WEnum::Value(caps),
            } = event
                && caps.contains(wl_seat::Capability::Touch)
                && app.touch.is_none()
            {
                app.touch = Some(seat.get_touch(qh, ()));
            }
        }
    }

    impl Dispatch<WlTouch, ()> for App {
        fn event(
            app: &mut Self,
            _: &WlTouch,
            event: wl_touch::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                wl_touch::Event::Down {
                    surface, id, x, y, ..
                } => {
                    let name = app.name_of(&surface);
                    app.touches.retain(|(i, _)| *i != id);
                    app.touches.push((id, name));
                    app.log(format!("down {name} {x:.2} {y:.2}"));
                }
                wl_touch::Event::Motion { id, x, y, .. } => {
                    let name = app.touch_name(id);
                    app.log(format!("motion {name} {x:.2} {y:.2}"));
                }
                wl_touch::Event::Up { id, .. } => {
                    let name = app.touch_name(id);
                    app.touches.retain(|(i, _)| *i != id);
                    app.log(format!("up {name}"));
                }
                _ => {}
            }
        }
    }

    impl App {
        fn touch_name(&self, id: i32) -> &'static str {
            self.touches
                .iter()
                .find(|(i, _)| *i == id)
                .map(|(_, n)| *n)
                .unwrap_or("unknown")
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
    delegate_noop!(App: ignore WlSubcompositor);
    delegate_noop!(App: ignore WlSubsurface);
    delegate_noop!(App: ignore WlSurface);
    delegate_noop!(App: ignore WlShm);
    delegate_noop!(App: ignore WlShmPool);
    delegate_noop!(App: ignore WlBuffer);
}
