//! 系统外观（宣纸/夜墨）：读取并监视 `$XDG_CONFIG_HOME/loong/appearance.toml`。
//!
//! 约定见 `docs/api/loong-js-bridge.md`「系统外观的约定」：文件由“设置”经 JS bridge 写入
//! （写临时文件再 `rename`），loong-shell 与所有 Web 应用读同一个文件，只认 `theme` 键。
//! 因为是 `rename` 替换，这里监视的是**所在目录**（inotify），任何变化都重新读一次文件。
//!
//! 优先级：环境变量 `LOONG_THEME` > 外观文件 > `device.toml` 的 `theme` > 亮色。

use std::{
    ffi::CString,
    io,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::DirBuilderExt},
    },
    path::{Path, PathBuf},
};

use loong_ui::theme::Theme;

/// 外观文件路径：`$XDG_CONFIG_HOME/loong/appearance.toml`，未设置时为 `~/.config/loong/appearance.toml`。
pub fn path() -> Option<PathBuf> {
    let non_empty = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let base = non_empty("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| non_empty("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("loong").join("appearance.toml"))
}

/// 解析文件内容。只看 `theme` 键；没有有效值时返回 `None`。
pub fn parse(text: &str) -> Option<Theme> {
    let table: toml::Table = text.parse().ok()?;
    table.get("theme")?.as_str().and_then(Theme::from_name)
}

/// 读取外观文件；文件不存在或内容无效时返回 `None`。
pub fn read(path: &Path) -> Option<Theme> {
    parse(&std::fs::read_to_string(path).ok()?)
}

/// 按优先级决定外观。
pub fn resolve(env: Option<&str>, file: Option<Theme>, device: Option<&str>) -> Theme {
    env.and_then(Theme::from_name)
        .or(file)
        .or_else(|| device.and_then(Theme::from_name))
        .unwrap_or_default()
}

/// 对外观文件所在目录的 inotify 监视。可读时调用 [`Watch::drain`] 取走事件。
pub struct Watch {
    fd: OwnedFd,
}

impl Watch {
    /// 监视 `file` 所在目录（不存在时以 0700 创建，与“设置”写入时的权限一致）。
    pub fn new(file: &Path) -> io::Result<Watch> {
        let dir = file
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "外观文件没有上级目录"))?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let dir = CString::new(dir.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        // SAFETY: 普通系统调用；返回的描述符立即交给 OwnedFd 管理。
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: raw 是刚创建、未被其他对象持有的有效描述符。
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mask = libc::IN_CLOSE_WRITE
            | libc::IN_MOVED_TO
            | libc::IN_MOVED_FROM
            | libc::IN_CREATE
            | libc::IN_DELETE;
        // SAFETY: fd 有效，dir 是以 NUL 结尾的路径。
        if unsafe { libc::inotify_add_watch(fd.as_raw_fd(), dir.as_ptr(), mask) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Watch { fd })
    }

    /// 读走所有待处理的事件（非阻塞），返回是否有事件。目录里文件很少，有事件就重读外观文件即可。
    pub fn drain(&self) -> bool {
        let mut buf = [0u8; 4096];
        let mut any = false;
        loop {
            // SAFETY: buf 可写、长度正确；fd 为非阻塞的 inotify 描述符。
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return any;
            }
            any = true;
        }
    }
}

impl AsFd for Watch {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_the_theme_key() {
        assert_eq!(parse("theme = \"dark\"\n"), Some(Theme::Dark));
        assert_eq!(
            parse("theme = \"light\"\nschedule = 1\n"),
            Some(Theme::Light)
        );
        assert_eq!(parse("theme = \"sepia\"\n"), None);
        assert_eq!(parse("theme = 3\n"), None);
        assert_eq!(parse("not toml ["), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn resolution_order() {
        assert_eq!(
            resolve(Some("light"), Some(Theme::Dark), Some("dark")),
            Theme::Light
        );
        assert_eq!(resolve(None, Some(Theme::Dark), Some("light")), Theme::Dark);
        assert_eq!(resolve(Some("bad"), None, Some("dark")), Theme::Dark);
        assert_eq!(resolve(None, None, None), Theme::Light);
    }

    #[test]
    fn watch_sees_atomic_replacement() {
        let dir = std::env::temp_dir().join(format!("loong-appearance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("loong").join("appearance.toml");
        let w = Watch::new(&file).unwrap();
        assert!(!w.drain());
        assert_eq!(read(&file), None);
        // 与“设置”相同的写法：写临时文件再 rename。
        let tmp = file.with_extension("tmp");
        std::fs::write(&tmp, "theme = \"dark\"\n").unwrap();
        std::fs::rename(&tmp, &file).unwrap();
        assert!(w.drain());
        assert_eq!(read(&file), Some(Theme::Dark));
        assert!(!w.drain());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
