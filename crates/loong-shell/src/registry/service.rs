//! 注册表与合成器的衔接：开机时读入并排列、监视目录变化、执行用户确认的卸载、写 home.toml。
//!
//! 事件循环里只做两件事：inotify 可读时记下“需要重新扫描”并请求重画；其余都在
//! [`LoongState::sync_registry`] 里做（每画一帧前调用一次，那时 `state.now` 是这一帧的时间，
//! 动画的起点才准确）。
//!
//! 卸载：先结束应用的进程（窗口所属客户端的 pid，经 Wayland 套接字的对端凭据取得；只对 app_id
//! 是 `loong.webapp.<id>` 的窗口这样做），立即删掉 manifest 与数据目录，等进程退出后（最多约 5 秒，
//! 2 秒后还在就 SIGKILL）再删一次——WebKit 退出时可能又写了点数据。

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use smithay::{
    desktop::Window,
    reexports::{
        calloop::{
            Interest, LoopHandle, Mode, PostAction,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::Resource,
    },
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use super::{Change, WebApp, diff, launch_command, order, registry_dir, scan, uninstall, watch};
use crate::state::LoongState;

/// 等应用进程退出：超过这么久还在就 SIGKILL；再久就不等了。
const KILL_AFTER: Duration = Duration::from_secs(2);
const GIVE_UP_AFTER: Duration = Duration::from_secs(5);
/// 等待期间多久看一次。
const POLL: Duration = Duration::from_millis(200);

/// 一次卸载的善后：等进程退出后再删一次目录。
struct Purge {
    id: String,
    pids: Vec<i32>,
    started: Instant,
    killed: bool,
}

/// 注册表服务（[`LoongState`] 的一部分）。
pub struct Service {
    root: PathBuf,
    /// 上一次扫描的结果（按 id 排序）。
    current: Vec<WebApp>,
    /// loong-webapp 的命令（`LOONG_WEBAPP`）。
    webapp: String,
    order_path: Option<PathBuf>,
    /// inotify 报告了变化，下一帧重新扫描。
    rescan: bool,
    purges: Vec<Purge>,
    /// 过一会儿唤醒事件循环（请求重画，从而再调用一次 `sync_registry`）。
    wake: Box<dyn Fn(Duration)>,
}

/// 开机：扫描注册表、按 home.toml 排列主屏，把目录监视注册到事件循环。
/// 在生成主屏位图（`render::Assets::new`）之前调用，第一帧就是完整的主屏。
pub fn init<D: 'static>(
    state: &mut LoongState,
    handle: &LoopHandle<'static, D>,
    get: fn(&mut D) -> &mut LoongState,
) {
    let Some(root) = registry_dir() else {
        tracing::warn!("没有 HOME/XDG_DATA_HOME，不读取 PWA 注册表");
        return;
    };
    let watch = match watch::Watch::new(&root) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(
                "无法监视 {}：{e}（新安装的应用要重启后才出现）",
                root.display()
            );
            None
        }
    };
    let current = scan(&root, &state.shell.builtin_ids());
    let webapp = super::webapp_command();
    // home.toml 里没有的应用按安装时间排在后面。
    let mut initial = current.clone();
    initial.sort_by(|a, b| (a.installed, &a.id).cmp(&(b.installed, &b.id)));
    let now = state.now;
    state.shell.apply_webapp_changes(
        initial.into_iter().map(Change::Added).collect(),
        &|a| launch_command(a, &webapp),
        now,
        false,
    );
    let order_path = order::path();
    if let Some(path) = &order_path {
        let order = order::read(path);
        if !order.is_empty() {
            state.shell.set_home_order(&order, now);
        }
    }
    // 开机时的排列不必写回。
    state.shell.take_home_order();
    tracing::info!("PWA 注册表：{}，{} 个应用", root.display(), current.len());

    if let Some(watch) = watch {
        let inserted = handle.insert_source(
            Generic::new(watch, Interest::READ, Mode::Level),
            move |_, watch, data| {
                if watch.drain() {
                    // 新出现的应用目录先加上监视，再扫描（不漏掉之间写入的 manifest）。
                    watch.watch_subdirs();
                    let state = get(data);
                    if let Some(svc) = &mut state.registry {
                        svc.rescan = true;
                    }
                    state.mark_damaged();
                }
                Ok(PostAction::Continue)
            },
        );
        if let Err(e) = inserted {
            tracing::warn!("无法监视 PWA 注册表：{e}");
        }
    }
    let wake_handle = handle.clone();
    state.registry = Some(Service {
        root,
        current,
        webapp,
        order_path,
        rescan: false,
        purges: Vec::new(),
        wake: Box::new(move |after| {
            let _ = wake_handle.insert_source(Timer::from_duration(after), move |_, _, data| {
                get(data).mark_damaged();
                TimeoutAction::Drop
            });
        }),
    });
}

impl LoongState {
    /// 每帧（`self.now` 已是这一帧的时间、`shell.update` 之前）：重新扫描、执行卸载、写 home.toml、
    /// 等待卸载的应用退出后清理数据。没有事情时几乎不花时间。
    pub fn sync_registry(&mut self) {
        let Some(mut svc) = self.registry.take() else {
            return;
        };
        if std::mem::take(&mut svc.rescan) {
            let new = scan(&svc.root, &self.shell.builtin_ids());
            let changes = diff(&svc.current, &new);
            svc.current = new;
            if !changes.is_empty() {
                let webapp = svc.webapp.clone();
                self.shell.apply_webapp_changes(
                    changes,
                    &|a| launch_command(a, &webapp),
                    self.now,
                    true,
                );
            }
        }
        for un in self.shell.take_uninstalls() {
            let pids: Vec<i32> = un
                .windows
                .iter()
                .filter_map(|w| self.webapp_pid(w, &un.id))
                .collect();
            for &pid in &pids {
                tracing::info!("结束 {}（pid {pid}）", un.id);
                // SAFETY: 普通系统调用；pid 来自仍然连着的客户端。
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            match uninstall(&svc.root, &un.id) {
                Ok(()) => tracing::info!("已卸载 {}", un.id),
                Err(e) => tracing::warn!("卸载 {} 失败：{e}", un.id),
            }
            svc.current.retain(|a| a.id != un.id);
            svc.purges.push(Purge {
                id: un.id,
                pids,
                started: Instant::now(),
                killed: false,
            });
            (svc.wake)(POLL);
        }
        let root = svc.root.clone();
        let wake = &svc.wake;
        svc.purges.retain_mut(|p| {
            p.pids.retain(|&pid| process_alive(pid));
            let elapsed = p.started.elapsed();
            if p.pids.is_empty() || elapsed >= GIVE_UP_AFTER {
                // 进程退出时 WebKit 可能又写了数据：再删一次。
                if let Err(e) = uninstall(&root, &p.id) {
                    tracing::warn!("清理 {} 的数据失败：{e}", p.id);
                }
                return false;
            }
            if elapsed >= KILL_AFTER && !p.killed {
                p.killed = true;
                for &pid in &p.pids {
                    // SAFETY: 同上。
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
            wake(POLL);
            true
        });
        if let Some(order) = self.shell.take_home_order()
            && let Some(path) = &svc.order_path
            && let Err(e) = order::write(path, &order)
        {
            tracing::warn!("写入 {} 失败：{e}", path.display());
        }
        self.registry = Some(svc);
    }

    /// 窗口所属进程的 pid：只认 app_id 为 `loong.webapp.<id>` 的窗口（不误杀别的程序），
    /// 也不会是合成器自己。
    fn webapp_pid(&self, window: &Window, id: &str) -> Option<i32> {
        let surface = window.toplevel()?.wl_surface().clone();
        let app_id = with_states(&surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok().and_then(|d| d.app_id.clone()))
        })?;
        if app_id != crate::shell::apps::webapp_app_id(id) {
            tracing::warn!("{id} 的窗口 app_id 是 {app_id}，不结束它的进程");
            return None;
        }
        let pid = surface
            .client()?
            .get_credentials(&self.display_handle)
            .ok()?
            .pid;
        (pid > 1 && pid as u32 != std::process::id()).then_some(pid)
    }
}

/// 进程是否还在。是合成器的子进程时顺便回收（否则它会以僵尸的样子一直“在”）。
fn process_alive(pid: i32) -> bool {
    // SAFETY: 普通系统调用；不是子进程时 waitpid 返回 ECHILD，没有副作用。
    unsafe {
        libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}
