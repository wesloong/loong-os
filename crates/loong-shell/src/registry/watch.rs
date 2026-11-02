//! 监视注册表目录（inotify）。
//!
//! inotify 不递归：注册表目录本身监视子目录的创建、删除、改名（安装新应用、卸载），每个应用子目录
//! 再各自监视 `manifest.json` 与图标文件的写入与 rename（浏览器重新安装、更新图标）。应用子目录
//! 同时是 loong-webapp 的 WebKit 数据目录，里面的其他文件变化很频繁，这里按文件名过滤掉，
//! 不触发重新扫描。

use std::{
    ffi::CString,
    io,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::DirBuilderExt},
    },
    path::{Path, PathBuf},
};

/// 应用子目录里与注册表有关的文件。
const WATCHED_FILES: [&[u8]; 3] = [b"manifest.json", b"icon.png", b"icon.svg"];

/// 注册表目录的监视。可读时调用 [`Watch::drain`]，返回真时重新扫描。
pub struct Watch {
    fd: OwnedFd,
    root: PathBuf,
    root_wd: i32,
}

impl Watch {
    /// 监视 `root`（不存在时以 0700 创建）及其中已有的应用子目录。
    pub fn new(root: &Path) -> io::Result<Watch> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        // SAFETY: 普通系统调用；返回的描述符立即交给 OwnedFd 管理。
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: raw 是刚创建、未被其他对象持有的有效描述符。
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mask = libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_DELETE_SELF
            | libc::IN_ONLYDIR;
        let root_wd = add_watch(&fd, root, mask)?;
        let w = Watch {
            fd,
            root: root.to_path_buf(),
            root_wd,
        };
        w.watch_subdirs();
        Ok(w)
    }

    /// 给应用子目录加上监视（扫描之前调用：先监视再读，不会漏掉之间的写入）。对已经监视的目录
    /// 再加一次只是返回同一个 wd，所以每次都对全部子目录调用即可（应用不多）；已删除的目录
    /// 由 inotify 自己移除监视。
    pub fn watch_subdirs(&self) {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return;
        };
        let mask = libc::IN_CLOSE_WRITE
            | libc::IN_MOVED_TO
            | libc::IN_MOVED_FROM
            | libc::IN_DELETE
            | libc::IN_ONLYDIR;
        for e in entries.filter_map(Result::ok) {
            if e.file_type().is_ok_and(|t| t.is_dir())
                && let Err(err) = add_watch(&self.fd, &e.path(), mask)
            {
                tracing::warn!("无法监视 {}: {err}", e.path().display());
            }
        }
    }

    /// 读走所有待处理的事件（非阻塞），返回其中是否有与注册表有关的变化。
    pub fn drain(&self) -> bool {
        let mut buf = [0u8; 8192];
        let mut relevant = false;
        loop {
            // SAFETY: buf 可写、长度正确；fd 为非阻塞的 inotify 描述符。
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return relevant;
            }
            relevant |= events_relevant(&buf[..n as usize], self.root_wd);
        }
    }
}

impl AsFd for Watch {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

fn add_watch(fd: &OwnedFd, dir: &Path, mask: u32) -> io::Result<i32> {
    let path = CString::new(dir.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: fd 有效，path 是以 NUL 结尾的路径。
    let wd = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), mask) };
    if wd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(wd)
}

/// 解析一段 inotify 事件（`struct inotify_event` 紧挨着排列，名字以 NUL 补齐），判断是否需要重新扫描：
/// 注册表目录本身的任何事件（应用目录增删、改名）、队列溢出，或应用子目录里那几个文件的变化。
fn events_relevant(mut buf: &[u8], root_wd: i32) -> bool {
    const HEADER: usize = std::mem::size_of::<libc::inotify_event>();
    let mut relevant = false;
    while buf.len() >= HEADER {
        let int = |at: usize| i32::from_ne_bytes(buf[at..at + 4].try_into().expect("4 字节"));
        let uint = |at: usize| u32::from_ne_bytes(buf[at..at + 4].try_into().expect("4 字节"));
        let (wd, mask, len) = (int(0), uint(4), uint(12) as usize);
        let end = (HEADER + len).min(buf.len());
        let name = &buf[HEADER..end];
        let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
        if mask & libc::IN_Q_OVERFLOW != 0
            || (wd == root_wd && mask & libc::IN_IGNORED == 0)
            || WATCHED_FILES.contains(&name)
        {
            relevant = true;
        }
        buf = &buf[end..];
    }
    relevant
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(wd: i32, mask: u32, name: &str) -> Vec<u8> {
        let padded = (name.len() + 1).div_ceil(16) * 16;
        let len = if name.is_empty() { 0 } else { padded };
        let mut v = Vec::new();
        v.extend(wd.to_ne_bytes());
        v.extend(mask.to_ne_bytes());
        v.extend(0u32.to_ne_bytes());
        v.extend((len as u32).to_ne_bytes());
        let mut n = name.as_bytes().to_vec();
        n.resize(len, 0);
        v.extend(n);
        v
    }

    #[test]
    fn filters_events_by_name() {
        let root = 1;
        // WebKit 在应用目录里写数据：不扫描。
        let noise = [
            event(2, libc::IN_CLOSE_WRITE, "localstorage.sqlite3"),
            event(2, libc::IN_MOVED_TO, ".manifest.json.ABC123"),
        ]
        .concat();
        assert!(!events_relevant(&noise, root));
        // manifest 被 rename 进来、图标写完、应用目录新建：扫描。
        for e in [
            event(2, libc::IN_MOVED_TO, "manifest.json"),
            event(3, libc::IN_CLOSE_WRITE, "icon.png"),
            event(root, libc::IN_CREATE | libc::IN_ISDIR, "example-com"),
            event(-1, libc::IN_Q_OVERFLOW, ""),
        ] {
            assert!(events_relevant(&[noise.clone(), e].concat(), root));
        }
    }

    #[test]
    fn sees_install_update_and_removal() {
        let root =
            std::env::temp_dir().join(format!("loong-registry-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let w = Watch::new(&root).unwrap();
        assert!(!w.drain());
        // 安装：新建目录 → 扫描前补上子目录监视 → 写入 manifest（临时文件 + rename）。
        let app = root.join("example-com");
        std::fs::create_dir(&app).unwrap();
        assert!(w.drain());
        w.watch_subdirs();
        std::fs::write(app.join("localstorage"), b"x").unwrap();
        assert!(!w.drain(), "WebKit 数据不触发扫描");
        std::fs::write(app.join(".manifest.tmp"), b"{}").unwrap();
        std::fs::rename(app.join(".manifest.tmp"), app.join("manifest.json")).unwrap();
        assert!(w.drain());
        std::fs::write(app.join("icon.png"), b"png").unwrap();
        assert!(w.drain());
        // 卸载：整个目录删除。
        std::fs::remove_dir_all(&app).unwrap();
        assert!(w.drain());
        w.watch_subdirs();
        assert!(!w.drain());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
