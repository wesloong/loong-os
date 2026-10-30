//! loong-shell：Loong OS 的系统界面（Wayland 合成器）。
//!
//! ```text
//! loong-shell [--device device.toml] [--client 命令]... [--app 应用id=命令]...   # 真机（DRM/KMS）
//! loong-shell --headless [--script 脚本.json] [--out 目录] [--size 1280x800] [--realtime]
//! ```

mod backend;
mod device;
mod handlers;
mod input;
mod render;
mod script;
mod shell;
mod state;

use std::path::PathBuf;

use anyhow::{Context, bail};

use crate::{backend::headless, device::DeviceConfig, script::Script};

struct Args {
    headless: bool,
    device: Option<String>,
    size: Option<(u32, u32)>,
    scale: f64,
    script: Option<String>,
    out: PathBuf,
    realtime: bool,
    clients: Vec<String>,
    app_commands: Vec<(String, String)>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        headless: false,
        device: None,
        size: None,
        scale: 1.0,
        script: None,
        out: PathBuf::from("out"),
        realtime: false,
        clients: vec![],
        app_commands: vec![],
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().with_context(|| format!("{name} 需要参数"));
        match a.as_str() {
            "--headless" => args.headless = true,
            "--realtime" => args.realtime = true,
            "--device" => args.device = Some(value("--device")?),
            "--script" => args.script = Some(value("--script")?),
            "--out" | "-o" => args.out = PathBuf::from(value("--out")?),
            "--client" => args.clients.push(value("--client")?),
            "--scale" => args.scale = value("--scale")?.parse()?,
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v.split_once('x').context("--size 格式为 宽x高")?;
                args.size = Some((w.parse()?, h.parse()?));
            }
            "--app" => {
                let v = value("--app")?;
                let (id, cmd) = v.split_once('=').context("--app 格式为 应用id=命令")?;
                args.app_commands.push((id.into(), cmd.into()));
            }
            "-h" | "--help" => {
                println!(
                    "{}",
                    include_str!("main.rs")
                        .lines()
                        .skip(3)
                        .take(4)
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                std::process::exit(0);
            }
            other => bail!("未知参数: {other}"),
        }
    }
    Ok(args)
}

fn ensure_runtime_dir() {
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        let dir = format!("/tmp/loong-runtime-{}", std::process::id());
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        // SAFETY: 在创建任何线程之前设置。
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir) };
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    ensure_runtime_dir();
    let args = parse_args()?;

    let device = match (&args.device, args.size) {
        (Some(path), _) => DeviceConfig::load(path).with_context(|| format!("读取 {path} 失败"))?,
        (None, Some((w, h))) => DeviceConfig::fallback(w, h, args.scale),
        (None, None) => DeviceConfig::load(device::DEFAULT_PATH)
            .unwrap_or_else(|_| DeviceConfig::fallback(1280, 800, 1.0)),
    };
    tracing::info!(
        "设备: {}，{}x{} @{}x",
        device.name,
        device.screen.width,
        device.screen.height,
        device.scale()
    );

    if !args.headless {
        return backend::udev::run(
            &device,
            backend::udev::UdevOptions {
                app_commands: args.app_commands,
                autostart: args.clients,
            },
        );
    }
    let script = match &args.script {
        Some(path) => Script::parse(&std::fs::read_to_string(path)?)
            .with_context(|| format!("解析脚本 {path} 失败"))?,
        None => Script::still(),
    };
    let opts = headless::HeadlessOptions {
        script,
        out_dir: args.out,
        realtime: args.realtime,
        clients: args.clients,
        app_commands: args.app_commands,
    };
    headless::run(&device, opts)
}
