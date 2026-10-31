//! 无屏幕后端：EGL surfaceless（Mesa llvmpipe 也可用）+ 离屏渲染。
//!
//! 用于自动化测试与 CI：虚拟时钟逐帧推进，回放脚本中的触摸，按时截图 / 录制 APNG。
//! 渲染路径（着色器、圆角、合成）与真机完全相同。

use std::{
    collections::BTreeSet,
    fs::File,
    io::BufWriter,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow};
use smithay::{
    backend::{
        allocator::Fourcc,
        egl::{EGLContext, EGLDevice, EGLDisplay, native::EGLSurfacelessDisplay},
        renderer::{
            Bind, ExportMem, ImportDma, Offscreen,
            damage::OutputDamageTracker,
            gles::{GlesRenderbuffer, GlesRenderer},
        },
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::{calloop::EventLoop, wayland_server::Display},
    utils::{Rectangle, Transform},
};

use crate::{
    device::DeviceConfig,
    render::{Assets, Painter},
    script::{Action, Script},
    state::LoongState,
};

/// 无屏幕后端参数。
pub struct HeadlessOptions {
    pub script: Script,
    pub out_dir: PathBuf,
    /// 按真实时间推进（需要连接真实客户端时使用）。
    pub realtime: bool,
    /// 启动时运行的客户端命令。
    pub clients: Vec<String>,
    /// （应用 id, 启动命令）。
    pub app_commands: Vec<(String, String)>,
}

pub fn run(device: &DeviceConfig, opts: HeadlessOptions) -> anyhow::Result<()> {
    let mut event_loop: EventLoop<'static, LoongState> = EventLoop::try_new()?;
    let display: Display<LoongState> = Display::new()?;
    let mut state = LoongState::new(&mut event_loop, display, device)?;
    for (id, cmd) in &opts.app_commands {
        state.shell.set_command(id, Some(cmd.clone()));
    }
    if let Some(clock) = &opts.script.clock {
        state.shell.clock_text = clock.clone();
    }
    if let Some(date) = &opts.script.date {
        state.shell.date_text = date.clone();
    }
    state.shell.set_locked(opts.script.locked);
    if let Some(name) = &opts.script.theme {
        let theme = loong_ui::theme::Theme::from_name(name)
            .with_context(|| format!("未知外观 {name}（应为 light 或 dark）"))?;
        state.shell.set_theme(theme);
    }

    // SAFETY: EGLSurfacelessDisplay 不持有任何原生资源。
    let egl = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }
        .context("初始化 EGL（surfaceless）失败")?;
    let context = EGLContext::new(&egl).context("创建 EGL 上下文失败")?;
    // SAFETY: 上下文只在本线程使用。
    let mut renderer = unsafe { GlesRenderer::new(context) }.context("创建 GLES 渲染器失败")?;
    // linux-dmabuf：主设备为 EGL 设备的 render node（有 GPU 时）；llvmpipe 等软件渲染没有
    // DRM 设备，只公布格式表。
    let render_node = EGLDevice::device_for_display(renderer.egl_context().display())
        .ok()
        .filter(|d| !d.is_software())
        .and_then(|d| d.try_get_render_node().ok().flatten());
    state.init_dmabuf(render_node, renderer.dmabuf_formats());

    let scale = device.scale();
    let fps = opts.script.fps.max(1);
    let (pw, ph) = (device.screen.width as i32, device.screen.height as i32);
    let output = Output::new(
        "headless-1".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Loong".into(),
            model: "Headless".into(),
            serial_number: "0".into(),
        },
    );
    let mode = OutputMode {
        size: (pw, ph).into(),
        refresh: (fps * 1000) as i32,
    };
    let _global = output.create_global::<LoongState>(&state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(OutputScale::Fractional(scale)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    state.output = Some(output.clone());

    let mut damage = OutputDamageTracker::from_output(&output);
    let mut buffer: GlesRenderbuffer = renderer
        .create_buffer(Fourcc::Abgr8888, (pw, ph).into())
        .context("创建离屏缓冲失败")?;
    let mut assets = Assets::new(&state.shell, scale);
    let mut painter = Painter::new(&mut renderer, scale).context("编译着色器失败")?;

    for cmd in &opts.clients {
        spawn(cmd, &state);
    }

    std::fs::create_dir_all(&opts.out_dir)?;
    let actions = opts.script.expand();
    let total_frames = opts.script.duration_ms * fps as u64 / 1000;
    let shots: BTreeSet<u64> = opts
        .script
        .screenshots
        .iter()
        .map(|ms| ms * fps as u64 / 1000)
        .collect();
    let mut recorder = match &opts.script.record {
        Some(r) => {
            let every = r.every.max(1) as u64;
            let frames = (0..=total_frames).filter(|f| f % every == 0).count() as u32;
            let factor = (1.0 / r.scale.clamp(0.05, 1.0)).round().max(1.0) as u32;
            let path = opts.out_dir.join("anim.png");
            Some((
                every,
                Recorder::new(
                    &path,
                    pw as u32,
                    ph as u32,
                    factor,
                    frames,
                    (fps as u64 / every) as u16,
                )?,
            ))
        }
        None => None,
    };

    let start = Instant::now();
    let mut timing = FrameTiming::default();
    let mut next_action = 0;
    for frame in 0..=total_frames {
        let now = frame as f64 / fps as f64;
        if opts.realtime {
            while start.elapsed().as_secs_f64() < now {
                let left = now - start.elapsed().as_secs_f64();
                event_loop.dispatch(
                    Some(Duration::from_secs_f64(left.clamp(0.0, 0.005))),
                    &mut state,
                )?;
                state.import_pending_dmabufs(&mut renderer);
            }
        }
        state.now = now;
        while next_action < actions.len() && actions[next_action].0 <= now {
            let time_us = (actions[next_action].0 * 1e6) as u64;
            match actions[next_action].1.clone() {
                Action::Down { slot, x, y } => state.touch_down(slot, x, y, time_us),
                Action::Move { slot, x, y } => state.touch_motion(slot, x, y, time_us),
                Action::Up { slot } => state.touch_up(slot, time_us),
                Action::Launch(id) => {
                    if let Some(app) = state.shell.apps.iter().position(|a| a.id == id) {
                        state.shell.launch(app, now);
                    }
                }
            }
            next_action += 1;
        }
        event_loop.dispatch(Some(Duration::ZERO), &mut state)?;
        state.import_pending_dmabufs(&mut renderer);
        state.shell.update(now);
        for app in state.shell.take_pending_spawns() {
            if let Some(cmd) = state.shell.apps[app].command.clone() {
                spawn(&cmd, &state);
            }
        }

        // 计时：生成画面 + 渲染元素 + GPU 画完（glFinish），不含截图与编码。
        let t0 = Instant::now();
        let scene = state.shell.scene(now);
        let elements = painter.elements(&mut renderer, &scene, &mut assets, &state.shell)?;
        let mut fb = renderer.bind(&mut buffer)?;
        damage
            .render_output(&mut renderer, &mut fb, 0, &elements, [0.0, 0.0, 0.0, 1.0])
            .map_err(|e| anyhow!("渲染失败: {e:?}"))?;
        // SAFETY: 只调用 glFinish，不改变任何 GL 状态。
        renderer.with_context(|gl| unsafe { gl.Finish() })?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        timing.push(ms);
        tracing::debug!(
            "帧 {frame}（{:.0} ms）：渲染 {ms:.1} ms，{} 个元素",
            now * 1000.0,
            elements.len()
        );

        let want_shot = shots.contains(&frame);
        let want_record = recorder
            .as_ref()
            .is_some_and(|(every, _)| frame % every == 0);
        if want_shot || want_record {
            let region = Rectangle::from_size((pw, ph).into());
            let mapping = renderer.copy_framebuffer(&fb, region, Fourcc::Abgr8888)?;
            let rgba = renderer.map_texture(&mapping)?.to_vec();
            if want_shot {
                let ms = frame * 1000 / fps as u64;
                let path = opts.out_dir.join(format!("frame-{ms:05}.png"));
                write_png(&path, pw as u32, ph as u32, &rgba)?;
                tracing::info!("截图 {}", path.display());
            }
            if let Some((_, rec)) = recorder.as_mut().filter(|_| want_record) {
                rec.push(&rgba)?;
            }
        }
        drop(fb);

        for window in &state.windows {
            window.send_frame(
                &output,
                Duration::from_secs_f64(now),
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
        state.popups.cleanup();
        state.display_handle.flush_clients()?;
    }
    if let Some((_, rec)) = recorder {
        rec.finish()?;
        tracing::info!("动画已保存 {}", opts.out_dir.join("anim.png").display());
    }
    timing.report();
    Ok(())
}

/// 每帧渲染耗时统计（毫秒）。
#[derive(Default)]
struct FrameTiming {
    samples: Vec<f64>,
}

impl FrameTiming {
    fn push(&mut self, ms: f64) {
        self.samples.push(ms);
    }

    /// 打印平均值与分位数（第一帧含着色器编译与纹理上传，单独列出）。
    fn report(&self) {
        let Some((&first, rest)) = self.samples.split_first() else {
            return;
        };
        if rest.is_empty() {
            tracing::info!("渲染耗时：首帧 {first:.1} ms");
            return;
        }
        let mut sorted = rest.to_vec();
        sorted.sort_by(f64::total_cmp);
        let pct = |p: f64| sorted[((sorted.len() - 1) as f64 * p).round() as usize];
        let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
        tracing::info!(
            "渲染耗时：首帧 {first:.1} ms；其余 {} 帧 平均 {mean:.1} ms，中位 {:.1} ms，p95 {:.1} ms，最大 {:.1} ms",
            sorted.len(),
            pct(0.5),
            pct(0.95),
            pct(1.0),
        );
    }
}

fn spawn(cmd: &str, state: &LoongState) {
    tracing::info!("启动: {cmd}");
    let result = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .env("WAYLAND_DISPLAY", &state.socket_name)
        .spawn();
    if let Err(e) = result {
        tracing::warn!("启动 {cmd} 失败: {e}");
    }
}

/// 预乘 RGBA → 不透明 RGB（截图背景不透明）。
fn to_rgb(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect()
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> anyhow::Result<()> {
    let mut enc = png::Encoder::new(BufWriter::new(File::create(path)?), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(&to_rgb(rgba))?;
    writer.finish()?;
    Ok(())
}

/// APNG 录制器（可整数倍缩小，控制文件大小）。
struct Recorder {
    writer: png::Writer<BufWriter<File>>,
    w: u32,
    h: u32,
    factor: u32,
}

impl Recorder {
    fn new(
        path: &Path,
        w: u32,
        h: u32,
        factor: u32,
        frames: u32,
        fps: u16,
    ) -> anyhow::Result<Self> {
        let (ow, oh) = (w / factor, h / factor);
        let mut enc = png::Encoder::new(BufWriter::new(File::create(path)?), ow, oh);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_animated(frames.max(1), 0)?;
        enc.set_frame_delay(1, fps.max(1))?;
        Ok(Recorder {
            writer: enc.write_header()?,
            w,
            h,
            factor,
        })
    }

    fn push(&mut self, rgba: &[u8]) -> anyhow::Result<()> {
        let f = self.factor as usize;
        let (w, h) = (self.w as usize, self.h as usize);
        let (ow, oh) = (w / f, h / f);
        let mut out = Vec::with_capacity(ow * oh * 3);
        for y in 0..oh {
            for x in 0..ow {
                let mut acc = [0u32; 3];
                for dy in 0..f {
                    for dx in 0..f {
                        let i = ((y * f + dy) * w + (x * f + dx)) * 4;
                        for c in 0..3 {
                            acc[c] += rgba[i + c] as u32;
                        }
                    }
                }
                let n = (f * f) as u32;
                out.extend(acc.iter().map(|v| (v / n) as u8));
            }
        }
        self.writer.write_image_data(&out)?;
        Ok(())
    }

    fn finish(self) -> anyhow::Result<()> {
        self.writer.finish()?;
        Ok(())
    }
}
