//! 真机后端：libseat 会话 + udev + DRM/KMS + GBM + GLES + libinput。
//!
//! 平板只有一块屏幕、一个 GPU，所以这里刻意保持简单：启动时选定主 GPU 和第一个已连接的
//! 显示接口；渲染由 vblank 驱动——只在有动画、输入或客户端提交新画面时才画下一帧，
//! 画面静止时 GPU 完全空闲（省电）。
//!
//! 在 QEMU（virtio-gpu 无 3D）中 Mesa 会退回软件渲染（kms_swrast），同样可用。

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow};
use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
            compositor::{DrmCompositor, FrameFlags, PrimaryPlaneElement},
            exporter::gbm::GbmFramebufferExporter,
        },
        egl::{EGLContext, EGLDevice, EGLDisplay},
        input::{
            AbsolutePositionEvent, ButtonState, Event, InputEvent, KeyboardKeyEvent,
            PointerButtonEvent, TouchEvent,
        },
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{ImportDma, gles::GlesRenderer},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{all_gpus, primary_gpu},
    },
    input::keyboard::FilterResult,
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, Mode as LoopMode, PostAction,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{ModeTypeFlags, connector},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::Display,
    },
    utils::{DeviceFd, SERIAL_COUNTER, Transform},
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner, SimpleCrtcMapper};

use crate::{
    device::DeviceConfig,
    render::{Assets, Painter},
    state::LoongState,
};

type Compositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// 后端运行时数据（放在事件循环共享数据之外，避免与 LoongState 互相借用）。
struct Backend {
    renderer: GlesRenderer,
    compositor: Compositor,
    painter: Painter,
    assets: Assets,
    output: Output,
    /// 已提交一帧、等待 vblank。
    frame_pending: bool,
    /// 画面没有变化但仍需要按帧推进（例如手指按住不动时的停顿检测）时，用定时器代替 vblank。
    tick_pending: bool,
    frame_interval: Duration,
    handle: LoopHandle<'static, Data>,
    /// 需要再画一帧（有新输入/客户端提交/动画）。
    dirty: bool,
    /// 鼠标（QEMU 平板设备）左键按下时当作手指。
    pointer_down: bool,
    pointer_pos: (f64, f64),
    start: Instant,
    /// `start` 时刻的 CLOCK_MONOTONIC（秒），用于把内核输入事件时间换算到同一时间轴。
    mono_at_start: f64,
    size: (f64, f64),
}

pub struct UdevOptions {
    pub app_commands: Vec<(String, String)>,
    pub autostart: Vec<String>,
}

/// 事件循环的共享数据。
struct Data {
    state: LoongState,
    backend: Backend,
}

pub fn run(device: &DeviceConfig, opts: UdevOptions) -> anyhow::Result<()> {
    let mut event_loop: EventLoop<'static, Data> = EventLoop::try_new()?;
    let (mut session, session_notifier) =
        LibSeatSession::new().context("无法建立 libseat 会话（seatd 是否在运行？）")?;

    // ── GPU ──
    let gpu_path: PathBuf = match std::env::var("LOONG_DRM_DEVICE") {
        Ok(p) => p.into(),
        Err(_) => primary_gpu(session.seat())?
            .or_else(|| all_gpus(session.seat()).ok()?.into_iter().next())
            .context("找不到 GPU")?,
    };
    let node = DrmNode::from_path(&gpu_path)?;
    tracing::info!("使用 GPU {}", gpu_path.display());
    let fd = session
        .open(
            &gpu_path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .context("打开 DRM 设备失败")?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (mut drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;

    // ── 渲染器（允许软件渲染：QEMU 开发用）──
    // SAFETY: gbm 设备在渲染器生命周期内有效。
    let egl = unsafe { EGLDisplay::new(gbm.clone()) }.context("EGL 初始化失败")?;
    let context = EGLContext::new(&egl)?;
    // SAFETY: 上下文只在本线程使用。
    let renderer = unsafe { GlesRenderer::new(context) }?;
    let render_node = node.node_with_type(NodeType::Render).and_then(|n| n.ok());

    // ── 显示接口 ──
    let mut scanner = DrmScanner::<SimpleCrtcMapper>::new();
    let (conn, crtc) = scanner
        .scan_connectors(&drm)?
        .into_iter()
        .find_map(|e| match e {
            DrmScanEvent::Connected {
                connector,
                crtc: Some(crtc),
            } if connector.state() == connector::State::Connected => Some((connector, crtc)),
            _ => None,
        })
        .context("没有已连接的显示器")?;
    let mode = conn
        .modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| conn.modes().first())
        .copied()
        .context("显示器没有可用模式")?;
    let (mw, mh) = mode.size();
    tracing::info!(
        "显示接口 {:?}，模式 {}x{}@{}",
        conn.interface(),
        mw,
        mh,
        mode.vrefresh()
    );

    let scale = device.scale();
    let output = Output::new(
        format!("{:?}-{}", conn.interface(), conn.interface_id()),
        PhysicalProperties {
            size: conn
                .size()
                .map(|(w, h)| (w as i32, h as i32))
                .unwrap_or((0, 0))
                .into(),
            subpixel: Subpixel::Unknown,
            make: "Loong".into(),
            model: device.name.clone(),
            serial_number: "0".into(),
        },
    );
    let output_mode = OutputMode {
        size: (mw as i32, mh as i32).into(),
        refresh: mode.vrefresh() as i32 * 1000,
    };

    let surface = drm.create_surface(crtc, mode, &[conn.handle()])?;
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(gbm.clone(), render_node.into());
    let render_formats = renderer.dmabuf_formats();
    let compositor = Compositor::new(
        &output,
        surface,
        None,
        allocator,
        exporter,
        [
            Fourcc::Argb8888,
            Fourcc::Xrgb8888,
            Fourcc::Abgr8888,
            Fourcc::Xbgr8888,
        ],
        render_formats,
        drm.cursor_size(),
        Some(gbm),
    )
    .map_err(|e| anyhow!("创建 DRM 合成器失败: {e:?}"))?;

    // ── Wayland 状态 ──
    // 逻辑尺寸以实际显示模式为准（device.toml 的宽高只用于缺省值）。
    let mut device = device.clone();
    device.screen.width = mw as u32;
    device.screen.height = mh as u32;
    let display: Display<LoongState> = Display::new()?;
    let mut state = LoongState::new_detached(display, &device)?;
    for (id, cmd) in &opts.app_commands {
        state.shell.set_command(id, Some(cmd.clone()));
    }
    // linux-dmabuf：主设备为渲染所用 GPU 的 render node（没有 render node 的驱动用 primary
    // node），WebKit 由此找到 GPU 并用 GBM 分配缓冲。合成器自己都是软件渲染时（QEMU 没有
    // 3D 加速，LIBGL_ALWAYS_SOFTWARE）不公布设备：客户端在这块“GPU”上也只能软件渲染，
    // 还得经 GBM 分配缓冲，不如直接走共享内存（与无屏幕后端的 llvmpipe 相同）。
    let software = EGLDevice::device_for_display(renderer.egl_context().display())
        .is_ok_and(|d| d.is_software())
        || std::env::var("LIBGL_ALWAYS_SOFTWARE").is_ok_and(|v| !matches!(v.as_str(), "" | "0"));
    let main_device = (!software).then(|| render_node.unwrap_or(node));
    state.init_dmabuf(main_device, renderer.dmabuf_formats());
    let _global = output.create_global::<LoongState>(&state.display_handle);
    output.change_current_state(
        Some(output_mode),
        Some(Transform::Normal),
        Some(OutputScale::Fractional(scale)),
        Some((0, 0).into()),
    );
    output.set_preferred(output_mode);
    state.output = Some(output.clone());

    // 开机即锁屏；时间取本地时间。
    state
        .shell
        .set_locked(std::env::var_os("LOONG_NO_LOCK").is_none());
    (state.shell.clock_text, state.shell.date_text) = crate::clock::now_strings();
    // 已安装的 Web 应用（PWA 注册表），并监视新的安装。
    crate::registry::service::init(&mut state, &event_loop.handle(), |d: &mut Data| {
        &mut d.state
    });
    let assets = Assets::new(&state.shell, scale);
    let mut renderer = renderer;
    let painter = Painter::new(&mut renderer, scale)?;
    let size = state.shell.screen_size();

    let mut data = Data {
        state,
        backend: Backend {
            renderer,
            compositor,
            painter,
            assets,
            output,
            frame_pending: false,
            tick_pending: false,
            frame_interval: Duration::from_secs_f64(1.0 / f64::from(mode.vrefresh().max(30))),
            handle: event_loop.handle(),
            dirty: true,
            pointer_down: false,
            pointer_pos: (0.0, 0.0),
            start: Instant::now(),
            mono_at_start: monotonic_secs(),
            size,
        },
    };

    let handle = event_loop.handle();
    data.state
        .register_sources(&handle, |d: &mut Data| &mut d.state)?;
    // loong-sysd（系统总线）：状态栏电量与 Wi‑Fi、控制中心。
    data.state.start_sysd(
        &handle,
        |d: &mut Data| &mut d.state,
        Some(crate::sysd_client::BusTarget::System),
    );
    // 拼音引擎在后台线程启动，就绪前键盘只能输入英文。
    crate::ime::spawn_engine(&handle, |d: &mut Data| &mut d.state)?;

    // vblank → 释放上一帧，按需画下一帧。
    handle
        .insert_source(drm_notifier, move |event, _, d: &mut Data| match event {
            DrmEvent::VBlank(_) => {
                if let Err(e) = d.backend.compositor.frame_submitted() {
                    tracing::warn!("frame_submitted: {e:?}");
                }
                d.backend.frame_pending = false;
                render(d);
            }
            DrmEvent::Error(e) => tracing::error!("DRM 错误: {e:?}"),
        })
        .map_err(|e| anyhow!("{e}"))?;

    // 每 10 秒检查一次时间，分钟变化时重画状态栏与锁屏。
    handle
        .insert_source(
            Timer::from_duration(Duration::from_secs(10)),
            |_, _, d: &mut Data| {
                let (clock, date) = crate::clock::now_strings();
                if clock != d.state.shell.clock_text || date != d.state.shell.date_text {
                    d.state.shell.clock_text = clock;
                    d.state.shell.date_text = date;
                    d.backend.dirty = true;
                    render(d);
                }
                TimeoutAction::ToDuration(Duration::from_secs(10))
            },
        )
        .map_err(|e| anyhow!("{e}"))?;

    // 系统外观：“设置”写入 appearance.toml（rename 替换），这里监视并切换宣纸/夜墨。
    if let Some(theme) = watch_appearance(&handle, &device) {
        data.state.shell.set_theme(theme);
    }

    // 输入
    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&session.seat())
        .map_err(|_| anyhow!("libinput 绑定 seat 失败"))?;
    handle
        .insert_source(
            LibinputInputBackend::new(libinput.clone()),
            |event, _, d: &mut Data| {
                process_input(d, event);
                d.backend.dirty = true;
                render(d);
            },
        )
        .map_err(|e| anyhow!("{e}"))?;

    // 会话切换（VT）
    handle
        .insert_source(
            session_notifier,
            move |event, _, d: &mut Data| match event {
                SessionEvent::PauseSession => {
                    libinput.suspend();
                    drm.pause();
                }
                SessionEvent::ActivateSession => {
                    let _ = libinput.resume();
                    if let Err(e) = drm.activate(false) {
                        tracing::error!("恢复 DRM 失败: {e}");
                    }
                    d.backend.dirty = true;
                    d.backend.frame_pending = false;
                    render(d);
                }
            },
        )
        .map_err(|e| anyhow!("{e}"))?;

    // SAFETY: 在启动任何子进程之前设置。
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &data.state.socket_name) };
    tracing::info!(
        "WAYLAND_DISPLAY={}",
        data.state.socket_name.to_string_lossy()
    );
    for cmd in &opts.autostart {
        spawn(cmd, &data.state);
    }

    render(&mut data);
    event_loop.run(None, &mut data, |d| {
        d.state.import_pending_dmabufs(&mut d.backend.renderer);
        // 输入法：焦点跟随前台应用、发送上屏文字与合成按键、调整窗口高度。
        d.state.sync_ime();
        // 每轮事件处理后：客户端有新提交或动画进行中 → 需要重画。
        if d.state.take_damage()
            || !d
                .state
                .shell
                .is_idle(d.backend.start.elapsed().as_secs_f64())
        {
            d.backend.dirty = true;
        }
        render(d);
        for app in d.state.shell.take_pending_spawns() {
            if let Some(cmd) = d.state.shell.apps[app].command.clone() {
                spawn(&cmd, &d.state);
            }
        }
        let _ = d.state.display_handle.flush_clients();
    })?;
    Ok(())
}

fn render(d: &mut Data) {
    let b = &mut d.backend;
    if b.frame_pending || !b.dirty {
        return;
    }
    let now = b.start.elapsed().as_secs_f64();
    d.state.now = now;
    d.state.sync_registry();
    d.state.shell.update(now);
    let scene = d.state.shell.scene(now);
    let elements = match b
        .painter
        .elements(&mut b.renderer, &scene, &mut b.assets, &d.state.shell)
    {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("生成渲染元素失败: {e}");
            return;
        }
    };
    match b.compositor.render_frame(
        &mut b.renderer,
        &elements,
        [0.0, 0.0, 0.0, 1.0],
        FrameFlags::DEFAULT,
    ) {
        Ok(result) => {
            // 驱动不支持显式同步（如 QEMU virtio-gpu、部分软件渲染路径）时，
            // 必须等 GPU 画完再提交，否则会扫描输出半成品画面。
            if result.needs_sync()
                && let PrimaryPlaneElement::Swapchain(element) = &result.primary_element
                && let Err(e) = element.sync.wait()
            {
                tracing::warn!("等待渲染完成失败: {e:?}");
            }
            if !result.is_empty {
                match b.compositor.queue_frame(()) {
                    Ok(()) => b.frame_pending = true,
                    Err(e) => tracing::warn!("提交帧失败: {e:?}"),
                }
            }
        }
        Err(e) => tracing::error!("渲染失败: {e:?}"),
    }
    b.dirty = !d.state.shell.is_idle(now);
    // 画面没变（没有提交新帧，也就不会有 vblank），但动画/手势仍在进行：约一帧后再检查一次。
    if b.dirty && !b.frame_pending && !b.tick_pending {
        b.tick_pending = true;
        let scheduled = b.handle.insert_source(
            Timer::from_duration(b.frame_interval),
            |_, _, d: &mut Data| {
                d.backend.tick_pending = false;
                render(d);
                TimeoutAction::Drop
            },
        );
        if scheduled.is_err() {
            b.tick_pending = false;
        }
    }
    let output = b.output.clone();
    let time = b.start.elapsed();
    for window in &d.state.windows {
        window.send_frame(&output, time, None, |_, _| Some(output.clone()));
    }
}

/// CLOCK_MONOTONIC（秒）。libinput 的事件时间使用同一个时钟。
fn monotonic_secs() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: 只写入我们提供的 timespec。
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

fn process_input(d: &mut Data, event: InputEvent<LibinputInputBackend>) {
    let (w, h) = d.backend.size;
    // 手势时间用事件发生的时间（内核时间戳），而不是处理事件的时间：
    // 系统繁忙时事件会被批量处理，用处理时间会丢失手指停顿、算错速度。
    let elapsed = d.backend.start.elapsed().as_secs_f64();
    d.state.now = elapsed;
    if let Some(us) = event_time_us(&event) {
        d.state.now = (us as f64 / 1e6 - d.backend.mono_at_start).clamp(0.0, elapsed);
    }
    match event {
        InputEvent::TouchDown { event } => {
            let p = event.position_transformed((w as i32, h as i32).into());
            d.state
                .touch_down(slot_id(&event), p.x, p.y, event.time().micros());
        }
        InputEvent::TouchMotion { event } => {
            let p = event.position_transformed((w as i32, h as i32).into());
            d.state
                .touch_motion(slot_id(&event), p.x, p.y, event.time().micros());
        }
        InputEvent::TouchUp { event } => d.state.touch_up(slot_id(&event), event.time().micros()),
        // QEMU 的 usb-tablet / virtio-tablet：绝对坐标鼠标，左键当作单指触摸。
        InputEvent::PointerMotionAbsolute { event } => {
            let p = event.position_transformed((w as i32, h as i32).into());
            d.backend.pointer_pos = (p.x, p.y);
            if d.backend.pointer_down {
                d.state.touch_motion(0, p.x, p.y, event.time().micros());
            }
        }
        InputEvent::PointerButton { event } => {
            let (x, y) = d.backend.pointer_pos;
            match event.state() {
                ButtonState::Pressed if !d.backend.pointer_down => {
                    d.backend.pointer_down = true;
                    d.state.touch_down(0, x, y, event.time().micros());
                }
                ButtonState::Released if d.backend.pointer_down => {
                    d.backend.pointer_down = false;
                    d.state.touch_up(0, event.time().micros());
                }
                _ => {}
            }
        }
        InputEvent::Keyboard { event } => {
            if let Some(keyboard) = d.state.seat.get_keyboard() {
                let time = event.time();
                keyboard.input::<(), _>(
                    &mut d.state,
                    event.key_code(),
                    event.state(),
                    SERIAL_COUNTER.next_serial(),
                    time,
                    |_, _, _| FilterResult::Forward,
                );
            }
        }
        _ => {}
    }
}

fn event_time_us(event: &InputEvent<LibinputInputBackend>) -> Option<u64> {
    Some(match event {
        InputEvent::TouchDown { event } => event.time().micros(),
        InputEvent::TouchMotion { event } => event.time().micros(),
        InputEvent::TouchUp { event } => event.time().micros(),
        InputEvent::PointerMotionAbsolute { event } => event.time().micros(),
        InputEvent::PointerButton { event } => event.time().micros(),
        _ => return None,
    })
}

fn slot_id<E: TouchEvent<LibinputInputBackend>>(event: &E) -> u32 {
    i32::from(event.slot()).max(0) as u32
}

fn spawn(cmd: &str, state: &LoongState) {
    tracing::info!("启动: {cmd}");
    if let Err(e) = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .env("WAYLAND_DISPLAY", &state.socket_name)
        .spawn()
    {
        tracing::warn!("启动 {cmd} 失败: {e}");
    }
}

/// 监视外观文件，返回启动时应使用的外观。监视失败只记录警告（外观停在 device.toml 的值）。
fn watch_appearance(
    handle: &LoopHandle<'static, Data>,
    device: &DeviceConfig,
) -> Option<loong_ui::theme::Theme> {
    use crate::appearance;
    let Some(path) = appearance::path() else {
        tracing::warn!("没有 HOME/XDG_CONFIG_HOME，外观不跟随设置");
        return None;
    };
    let watch = match appearance::Watch::new(&path) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("无法监视 {}: {e}", path.display());
            return None;
        }
    };
    let device_theme = device.theme.clone();
    let current = move || {
        let env = std::env::var("LOONG_THEME").ok();
        appearance::resolve(
            env.as_deref(),
            appearance::read(&path),
            device_theme.as_deref(),
        )
    };
    // 启动时就按外观文件来（device.toml 只是出厂默认）。
    let initial = current();
    let source = Generic::new(watch, Interest::READ, LoopMode::Level);
    let inserted = handle.insert_source(source, move |_, watch, d: &mut Data| {
        if watch.drain() {
            let theme = current();
            if theme != d.state.shell.theme {
                tracing::info!("系统外观改为 {theme:?}");
                d.state.shell.set_theme(theme);
                d.backend.dirty = true;
                render(d);
            }
        }
        Ok(PostAction::Continue)
    });
    if let Err(e) = inserted {
        tracing::warn!("无法监视外观文件: {e}");
    }
    Some(initial)
}
