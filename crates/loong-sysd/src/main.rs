//! loong-sysd：Loong OS 的系统服务（以 root 运行）。
//!
//! 在系统总线上占用 `org.loong.System1`，提供：
//!
//! | 对象路径 | 接口 | 内容 |
//! |---|---|---|
//! | `/org/loong/System1/Backlight` | `org.loong.System1.Backlight` | 屏幕亮度 |
//! | `/org/loong/System1/Power` | `org.loong.System1.Power` | 电池与电源 |
//! | `/org/loong/System1/Wifi` | `org.loong.System1.Wifi` | Wi‑Fi（封装 iwd） |
//!
//! 另外占用 `org.loong.Update1`（对象 `/org/loong/Update1`）：A/B 系统更新，见 `docs/api/loong-update.md`。
//!
//! 属性对所有人可读；写操作只允许 root 与会话用户 loong（uid 1000）。
//! 完整的接口说明见 `docs/api/loong-sysd.md`。
//!
//! ```text
//! loong-sysd [--bus system|session|<D-Bus 地址>]
//! ```
//!
//! 环境变量：`LOONG_SYSD_BUS`（同 `--bus`）、`LOONG_SYSFS_ROOT`（sysfs 根目录，默认 `/sys`）、
//! `LOONG_BACKLIGHT`（指定背光设备）、`LOONG_BACKLIGHT_ALLOW_OFF=1`（允许亮度 0 关掉背光）、
//! `LOONG_UPDATE_ROOT`（系统更新用的根目录，测试用）、`RUST_LOG`（error/warn/info/debug/trace，默认 info）。

mod access;
mod backlight;
mod bus;
mod error;
mod power;
mod sysfs;
mod uevent;
mod update;
mod wifi;

use std::{process::ExitCode, sync::Arc};

use anyhow::{Context, bail};
use async_executor::LocalExecutor;
use tracing::Level;
use zbus::connection::Builder;

use crate::{
    backlight::{Backlight, BacklightIface},
    power::{Power, PowerIface},
    sysfs::Sysfs,
    update::{Update, UpdateIface},
    wifi::{Agent, Wifi, WifiIface},
};

/// 环境变量：连接哪条总线。
const BUS_ENV: &str = "LOONG_SYSD_BUS";

/// 要连接的总线。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Bus {
    System,
    Session,
    Address(String),
}

impl Bus {
    fn parse(s: &str) -> Bus {
        match s {
            "" | "system" => Bus::System,
            "session" => Bus::Session,
            address => Bus::Address(address.to_owned()),
        }
    }
}

fn parse_args() -> anyhow::Result<Bus> {
    let mut bus = Bus::parse(&std::env::var(BUS_ENV).unwrap_or_default());
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bus" => bus = Bus::parse(&it.next().context("--bus 需要参数")?),
            "--session" => bus = Bus::Session,
            "-h" | "--help" => {
                println!("用法: loong-sysd [--bus system|session|<D-Bus 地址>]");
                println!("Loong OS 系统服务：背光、电源、Wi‑Fi（org.loong.System1）");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("loong-sysd {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => bail!("未知参数: {other}（--help 查看用法）"),
        }
    }
    Ok(bus)
}

fn init_logging() {
    let spec = std::env::var("RUST_LOG")
        .unwrap_or_default()
        .to_ascii_lowercase();
    // 兼容 `loong_sysd=debug` 这类写法：取最后一个 `=` 后面的级别。
    let level = match spec.rsplit(['=', ',']).next().unwrap_or("").trim() {
        "error" => Level::ERROR,
        "warn" => Level::WARN,
        "debug" => Level::DEBUG,
        "trace" => Level::TRACE,
        _ => Level::INFO,
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

async fn run(bus: Bus) -> anyhow::Result<()> {
    let sysfs = Sysfs::from_env();
    tracing::info!(
        "loong-sysd {} 启动（sysfs: {}）",
        env!("CARGO_PKG_VERSION"),
        sysfs.root().display()
    );
    let backlight = Arc::new(Backlight::new(&sysfs, &backlight::Options::from_env()));
    let power = Arc::new(Power::new(sysfs));
    let wifi = Arc::new(Wifi::default());
    let update = Arc::new(Update::default());

    let builder = match &bus {
        Bus::System => Builder::system()?,
        Bus::Session => Builder::session()?,
        Bus::Address(address) => Builder::address(address.as_str())?,
    };
    let conn = builder
        .serve_at(backlight::PATH, BacklightIface(backlight.clone()))?
        .serve_at(power::PATH, PowerIface(power.clone()))?
        .serve_at(wifi::PATH, WifiIface(wifi.clone()))?
        .serve_at(wifi::AGENT_PATH, Agent(wifi.clone()))?
        .serve_at(update::PATH, UpdateIface(update.clone()))?
        .name(bus::BUS_NAME)?
        .name(update::BUS_NAME)?
        .build()
        .await
        .with_context(|| {
            format!(
                "无法连接 D-Bus（{bus:?}）或占用 {} / {}",
                bus::BUS_NAME,
                update::BUS_NAME
            )
        })?;
    tracing::info!("已在 D-Bus 上注册 {}、{}", bus::BUS_NAME, update::BUS_NAME);

    let ex = LocalExecutor::new();
    ex.spawn(backlight::run_animator(backlight.clone()))
        .detach();
    ex.spawn(power::run_poller(power.clone(), conn.clone()))
        .detach();
    ex.spawn(uevent::run(backlight, power, conn.clone()))
        .detach();
    ex.spawn(wifi::run_watcher(wifi, conn.clone())).detach();
    ex.spawn(update::init(update, conn.clone())).detach();
    // 总线断开（例如 dbus 重启）时退出，由 supervise-daemon 重新拉起。
    ex.run(conn.closed()).await;
    bail!("D-Bus 连接已断开")
}

fn main() -> ExitCode {
    let bus = match parse_args() {
        Ok(bus) => bus,
        Err(e) => {
            eprintln!("loong-sysd: {e:#}");
            return ExitCode::from(2);
        }
    };
    init_logging();
    match async_io::block_on(run(bus)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_selection() {
        assert_eq!(Bus::parse(""), Bus::System);
        assert_eq!(Bus::parse("system"), Bus::System);
        assert_eq!(Bus::parse("session"), Bus::Session);
        assert_eq!(
            Bus::parse("unix:path=/tmp/bus"),
            Bus::Address("unix:path=/tmp/bus".into())
        );
    }
}
