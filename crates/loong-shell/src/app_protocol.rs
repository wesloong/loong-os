//! Loong 私有协议 `loong_app_v1`（`protocols/loong-app-v1.xml`，说明见
//! `docs/api/loong-app-protocol.md`）：应用告诉系统界面状态栏用哪种墨、顶部是什么颜色。
//!
//! - 全局对象 `loong_app_manager_v1`，`get_app_surface` 为某个 `wl_surface`（应用 toplevel 的
//!   根表面）建立 `loong_app_surface_v1`。
//! - 状态**立即生效**（不随 `wl_surface.commit` 双缓冲）：换墨本来就要交叉淡化，不需要与某一帧
//!   画面同步；而 WebKit 等工具包掌握着表面的提交时机，应用自己没法为改个颜色去提交表面。
//! - 状态存在 `wl_surface` 的 data map 里（[`AppSurfaceCell`]），系统界面每帧按前台窗口的根表面
//!   读取（[`hints`]），决定顺序见 [`crate::shell::status`]。
//! - `wl_surface` 先销毁时 `loong_app_surface_v1` 变为惰性对象，请求被忽略；
//!   `loong_app_surface_v1` 销毁（含客户端断开）时状态复位。

use std::sync::Mutex;

use smithay::{
    reexports::wayland_server::{
        Client, DataInit, DisplayHandle, New, Resource, WEnum, Weak, backend::ClientId,
        backend::GlobalId, protocol::wl_surface::WlSurface,
    },
    wayland::{Dispatch2, GlobalDispatch2, compositor::with_states},
};

use self::server::{
    loong_app_manager_v1::{self, LoongAppManagerV1},
    loong_app_surface_v1::{self, LoongAppSurfaceV1},
};
use crate::{
    shell::status::{AppHints, StatusStyle},
    state::LoongState,
};

/// 由 wayland-scanner 按协议 XML 生成的服务端代码。
#[allow(
    missing_docs,
    non_upper_case_globals,
    non_camel_case_types,
    unused_imports,
    clippy::all,
    clippy::pedantic
)]
pub mod server {
    use smithay::reexports::wayland_server;
    use smithay::reexports::wayland_server::protocol::*;

    pub mod __interfaces {
        use smithay::reexports::wayland_server::backend as wayland_backend;
        use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("../../protocols/loong-app-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("../../protocols/loong-app-v1.xml");
}

/// 协议版本。
const VERSION: u32 = 1;

/// 某个表面上应用给出的状态。
#[derive(Debug, Default, Clone, Copy)]
struct AppSurfaceState {
    /// 已经有 `loong_app_surface_v1` 对象（一个表面只能有一个）。
    claimed: bool,
    hints: AppHints,
}

/// 存在 `wl_surface` data map 中的状态。
#[derive(Debug, Default)]
struct AppSurfaceCell(Mutex<AppSurfaceState>);

fn with_cell<T>(surface: &WlSurface, f: impl FnOnce(&mut AppSurfaceState) -> T) -> T {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(AppSurfaceCell::default);
        let cell = states.data_map.get::<AppSurfaceCell>().expect("刚插入");
        f(&mut cell.0.lock().expect("锁"))
    })
}

/// 表面上应用给出的线索（没有时为默认值：`auto`、没有主题色）。
pub fn hints(surface: &WlSurface) -> AppHints {
    if !surface.is_alive() {
        return AppHints::default();
    }
    with_states(surface, |states| {
        states
            .data_map
            .get::<AppSurfaceCell>()
            .map(|cell| cell.0.lock().expect("锁").hints)
            .unwrap_or_default()
    })
}

/// 全局对象（持有即可）。
pub struct AppProtocolState {
    _global: GlobalId,
}

impl AppProtocolState {
    pub fn new(dh: &DisplayHandle) -> Self {
        AppProtocolState {
            _global: dh
                .create_global::<LoongState, LoongAppManagerV1, _>(VERSION, AppManagerGlobal),
        }
    }
}

/// 全局对象的数据。
pub struct AppManagerGlobal;

impl GlobalDispatch2<LoongAppManagerV1, LoongState> for AppManagerGlobal {
    fn bind(
        &self,
        _state: &mut LoongState,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<LoongAppManagerV1>,
        data_init: &mut DataInit<'_, LoongState>,
    ) {
        data_init.init(resource, AppManager);
    }
}

/// `loong_app_manager_v1` 对象的数据。
pub struct AppManager;

impl Dispatch2<LoongAppManagerV1, LoongState> for AppManager {
    fn request(
        &self,
        _state: &mut LoongState,
        _client: &Client,
        manager: &LoongAppManagerV1,
        request: loong_app_manager_v1::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, LoongState>,
    ) {
        match request {
            loong_app_manager_v1::Request::GetAppSurface { id, surface } => {
                let fresh = with_cell(&surface, |s| !std::mem::replace(&mut s.claimed, true));
                if !fresh {
                    // 新对象必须初始化；随后的协议错误会断开这个客户端。
                    data_init.init(id, AppSurface { surface: None });
                    manager.post_error(
                        loong_app_manager_v1::Error::AlreadyConstructed,
                        "该 wl_surface 已经有 loong_app_surface_v1",
                    );
                    return;
                }
                data_init.init(
                    id,
                    AppSurface {
                        surface: Some(surface.downgrade()),
                    },
                );
            }
            loong_app_manager_v1::Request::Destroy => {}
        }
    }
}

/// `loong_app_surface_v1` 对象的数据：所属表面（弱引用，表面先销毁后对象变为惰性）。
pub struct AppSurface {
    surface: Option<Weak<WlSurface>>,
}

impl AppSurface {
    fn surface(&self) -> Option<WlSurface> {
        self.surface
            .as_ref()
            .and_then(|w| w.upgrade().ok())
            .filter(|s| s.is_alive())
    }
}

impl Dispatch2<LoongAppSurfaceV1, LoongState> for AppSurface {
    fn request(
        &self,
        state: &mut LoongState,
        _client: &Client,
        resource: &LoongAppSurfaceV1,
        request: loong_app_surface_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, LoongState>,
    ) {
        let Some(surface) = self.surface() else {
            return;
        };
        match request {
            loong_app_surface_v1::Request::SetStatusBarStyle { style } => {
                let style = match style {
                    WEnum::Value(loong_app_surface_v1::StatusBarStyle::Auto) => StatusStyle::Auto,
                    WEnum::Value(loong_app_surface_v1::StatusBarStyle::DarkContent) => {
                        StatusStyle::DarkContent
                    }
                    WEnum::Value(loong_app_surface_v1::StatusBarStyle::LightContent) => {
                        StatusStyle::LightContent
                    }
                    WEnum::Unknown(_) => {
                        resource.post_error(
                            loong_app_surface_v1::Error::InvalidStyle,
                            format!("未知的状态栏样式 {style:?}"),
                        );
                        return;
                    }
                };
                tracing::debug!("{surface:?}：状态栏样式 {style:?}");
                with_cell(&surface, |s| s.hints.style = style);
            }
            loong_app_surface_v1::Request::SetThemeColor { argb } => {
                let color = (argb >> 24 != 0).then_some(argb);
                tracing::debug!("{surface:?}：主题色 {color:08x?}");
                with_cell(&surface, |s| s.hints.theme_color = color);
            }
            loong_app_surface_v1::Request::Destroy => {}
        }
        // 下一帧重新决定状态栏的墨。
        state.mark_damaged();
    }

    fn destroyed(&self, state: &mut LoongState, _client: ClientId, _resource: &LoongAppSurfaceV1) {
        if let Some(surface) = self.surface() {
            with_cell(&surface, |s| *s = AppSurfaceState::default());
            state.mark_damaged();
        }
    }
}
