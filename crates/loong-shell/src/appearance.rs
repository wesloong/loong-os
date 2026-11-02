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

/// 外观的名称（文件里的 `theme` 值）。
pub fn name(theme: Theme) -> &'static str {
    match theme {
        Theme::Light => "light",
        Theme::Dark => "dark",
    }
}

/// 外观文件的内容（与“设置”写入的格式相同，见 `webrt/webapp/loong-bridge-core.c`）。
pub fn format(theme: Theme) -> String {
    format!(
        "# Loong OS 外观：light（宣纸）或 dark（夜墨）。\n\
         # 由“设置”或控制中心写入；loong-shell 与所有 Web 应用读取，见 docs/api/loong-js-bridge.md。\n\
         theme = \"{}\"\n",
        name(theme)
    )
}

/// 写外观文件（控制中心的「夜墨」开关）：与“设置”相同，先写同目录下的临时文件再 `rename`，
/// 读取方不会读到半个文件；文件 0644，目录 0700。所有监视者（loong-shell 自己、每个
/// loong-webapp）随后各自切换。
pub fn write(path: &Path, theme: Theme) -> io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "外观文件没有上级目录"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let tmp = dir.join(format!(".appearance.toml.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&tmp)?;
        file.write_all(format(theme).as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// 启动时读外观文件并在它变化时切换系统界面（`Shell::set_theme` + 请求重画）。返回启动时
/// 应使用的外观；没有 HOME/XDG_CONFIG_HOME 或无法监视时返回 `None`（外观停在原来的值）。
///
/// 无屏幕后端在脚本要求时（`"follow_appearance": true`）使用；真机后端有自己的一份
/// （`backend/udev.rs` 的 `watch_appearance`），行为相同。
pub fn follow<D: 'static>(
    handle: &smithay::reexports::calloop::LoopHandle<'static, D>,
    get: fn(&mut D) -> &mut crate::state::LoongState,
    device_theme: Option<String>,
) -> Option<Theme> {
    use smithay::reexports::calloop::{Interest, Mode, PostAction, generic::Generic};
    let path = path()?;
    let watch = match Watch::new(&path) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("无法监视 {}: {e}", path.display());
            return None;
        }
    };
    let current = move || {
        let env = std::env::var("LOONG_THEME").ok();
        resolve(env.as_deref(), read(&path), device_theme.as_deref())
    };
    let initial = current();
    let inserted = handle.insert_source(
        Generic::new(watch, Interest::READ, Mode::Level),
        move |_, watch, data| {
            if watch.drain() {
                let state = get(data);
                let theme = current();
                if theme != state.shell.theme {
                    tracing::info!("系统外观改为 {theme:?}");
                    state.shell.set_theme(theme);
                    state.mark_damaged();
                }
            }
            Ok(PostAction::Continue)
        },
    );
    if let Err(e) = inserted {
        tracing::warn!("无法监视外观文件: {e}");
        return None;
    }
    Some(initial)
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

    #[test]
    fn write_replaces_the_file_atomically() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("loong-appearance-w-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("loong").join("appearance.toml");
        // 目录不存在时创建（0700）；监视者收到 rename。
        write(&file, Theme::Dark).unwrap();
        let w = Watch::new(&file).unwrap();
        assert_eq!(read(&file), Some(Theme::Dark));
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(file.parent().unwrap()), 0o700);
        assert_eq!(mode(&file), 0o644);
        write(&file, Theme::Light).unwrap();
        assert!(w.drain());
        assert_eq!(read(&file), Some(Theme::Light));
        assert_eq!(parse(&format(Theme::Dark)), Some(Theme::Dark));
        // 不留下临时文件。
        let names: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("appearance.toml")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
