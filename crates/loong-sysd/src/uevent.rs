//! 监听内核 uevent（netlink `NETLINK_KOBJECT_UEVENT`，内核组播组 1）。
//!
//! 插拔充电器、电量变化（部分驱动）、亮度热键都会产生 uevent。收到 `power_supply`
//! 事件时立即刷新电源状态，收到 `backlight` 事件时重新读取亮度。这只是加快响应：
//! 打不开 netlink 套接字（例如在容器里）时只记一条日志，电源状态仍靠定时轮询。

use std::{io, os::fd::OwnedFd, sync::Arc, time::Duration};

use async_io::{Async, Timer};
use rustix::net::{
    AddressFamily, RecvFlags, SocketFlags, SocketType, bind,
    netlink::{self, SocketAddrNetlink},
    recv, socket_with,
};
use zbus::Connection;

use crate::{
    backlight::{self, Backlight},
    power::{self, Power},
};

/// 一批事件之后再等这么久，把紧接着的事件合并成一次刷新。
const COALESCE: Duration = Duration::from_millis(200);

/// 需要刷新的子系统。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Interest {
    /// 电源（`power_supply`）。
    pub power: bool,
    /// 背光（`backlight`）。
    pub backlight: bool,
}

impl Interest {
    fn add(&mut self, message: &[u8]) {
        match subsystem(message) {
            Some("power_supply") => self.power = true,
            Some("backlight") => self.backlight = true,
            _ => {}
        }
    }

    fn all() -> Self {
        Interest {
            power: true,
            backlight: true,
        }
    }
}

/// 从内核 uevent 消息中取出 `SUBSYSTEM`。
///
/// 内核消息的格式是 `动作@设备路径\0KEY=VALUE\0KEY=VALUE\0…`；udev 转发的消息以
/// `libudev` 开头，是二进制头，这里不处理（我们只订阅内核的组）。
pub fn subsystem(message: &[u8]) -> Option<&str> {
    let mut fields = message.split(|&b| b == 0);
    let header = fields.next()?;
    if header.starts_with(b"libudev") || !header.contains(&b'@') {
        return None;
    }
    fields
        .filter_map(|f| f.strip_prefix(b"SUBSYSTEM="))
        .find_map(|v| std::str::from_utf8(v).ok())
}

fn open() -> io::Result<Async<OwnedFd>> {
    let fd = socket_with(
        AddressFamily::NETLINK,
        SocketType::DGRAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        Some(netlink::KOBJECT_UEVENT),
    )?;
    bind(&fd, &SocketAddrNetlink::new(0, 1))?;
    Async::new(fd)
}

/// 后台任务：监听 uevent 并触发刷新。
pub async fn run(backlight: Arc<Backlight>, power: Arc<Power>, conn: Connection) {
    let socket = match open() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("无法监听内核 uevent（{e}），电源状态只靠定时轮询");
            return;
        }
    };
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let mut interest = Interest::default();
        let first = socket
            .read_with(|fd| Ok(recv(fd, &mut buf[..], RecvFlags::empty())?.0))
            .await;
        match first {
            Ok(n) => interest.add(&buf[..n]),
            // 接收缓冲区溢出：丢了事件，全部刷新一次。
            Err(e) if e.raw_os_error() == Some(rustix::io::Errno::NOBUFS.raw_os_error()) => {
                interest = Interest::all();
            }
            Err(e) => {
                tracing::warn!("读取 uevent 失败（{e}），停止监听");
                return;
            }
        }
        if interest == Interest::default() {
            continue;
        }
        Timer::after(COALESCE).await;
        loop {
            match recv(socket.get_ref(), &mut buf[..], RecvFlags::DONTWAIT) {
                Ok((n, _)) => interest.add(&buf[..n]),
                Err(rustix::io::Errno::NOBUFS) => interest = Interest::all(),
                Err(_) => break,
            }
        }
        if interest.power {
            power::refresh_and_notify(&power, &conn).await;
        }
        if interest.backlight {
            backlight::reload_and_notify(&backlight, &conn).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kernel_messages() {
        let msg = b"change@/devices/platform/battery/power_supply/BAT0\0ACTION=change\0\
DEVPATH=/devices/platform/battery/power_supply/BAT0\0SUBSYSTEM=power_supply\0\
POWER_SUPPLY_NAME=BAT0\0SEQNUM=1234\0";
        assert_eq!(subsystem(msg), Some("power_supply"));
        let msg = b"change@/devices/pci0000:00/backlight/intel_backlight\0ACTION=change\0\
SUBSYSTEM=backlight\0SOURCE=hotkey\0";
        assert_eq!(subsystem(msg), Some("backlight"));

        let mut i = Interest::default();
        i.add(msg);
        assert_eq!(
            i,
            Interest {
                power: false,
                backlight: true
            }
        );
    }

    #[test]
    fn ignores_udev_and_garbage() {
        assert_eq!(
            subsystem(b"libudev\0\xfe\xed\0SUBSYSTEM=power_supply\0"),
            None
        );
        assert_eq!(subsystem(b""), None);
        assert_eq!(subsystem(b"no header\0SUBSYSTEM=x\0"), None);
        assert_eq!(subsystem(b"add@/devices/x\0ACTION=add\0"), None);
        assert_eq!(subsystem(b"add@/x\0SUBSYSTEM=\xff\0"), None);
    }
}
