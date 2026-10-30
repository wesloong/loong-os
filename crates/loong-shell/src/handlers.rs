//! Wayland 协议回调。

use smithay::{
    backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state},
    desktop::{PopupKind, Window, find_popup_root_surface},
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DndGrabHandler, GrabType, Source},
        pointer::CursorImageStatus,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Client, Resource,
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent,
            is_sync_subsurface, with_states,
        },
        output::OutputHandler,
        selection::{
            SelectionHandler,
            data_device::{
                DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus,
            },
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
        },
        shm::{ShmHandler, ShmState},
    },
};

use crate::state::{ClientState, LoongState};

impl CompositorHandler for LoongState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("客户端数据")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.mark_damaged();
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self
                .windows
                .iter()
                .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
            {
                window.on_commit();
                let has_buffer =
                    with_renderer_surface_state(&root, |s| s.buffer().is_some()).unwrap_or(false);
                if has_buffer {
                    let window = window.clone();
                    self.shell.on_window_ready(&window, self.now);
                }
            }
        }
        ensure_initial_configure(self, surface);
        self.popups.commit(surface);
    }
}

/// xdg-shell 要求在第一次 commit 后发送 configure。
fn ensure_initial_configure(state: &mut LoongState, surface: &WlSurface) {
    if let Some(window) = state
        .windows
        .iter()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
    {
        let toplevel = window.toplevel().expect("xdg 窗口");
        let initial_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|d| d.lock().expect("锁").initial_configure_sent)
                .unwrap_or(true)
        });
        if !initial_sent {
            toplevel.send_configure();
        }
        return;
    }
    if let Some(PopupKind::Xdg(xdg)) = state.popups.find_popup(surface)
        && !xdg.is_initial_configure_sent()
    {
        xdg.send_configure().ok();
    }
}

impl BufferHandler for LoongState {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for LoongState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for LoongState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // 平板上所有应用都是全屏的（v1 不做自由窗口）。
        let (w, h) = self.shell.screen_size();
        surface.with_pending_state(|state| {
            state.size = Some((w.round() as i32, h.round() as i32).into());
            state.states.set(xdg_toplevel::State::Fullscreen);
            state.states.set(xdg_toplevel::State::Activated);
        });
        let window = Window::new_wayland_window(surface.clone());
        self.windows.push(window.clone());
        self.shell.on_new_window(window, self.now);
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(surface.wl_surface().clone()), Serial::from(0));
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.windows
            .retain(|w| w.toplevel().is_some_and(|t| t != &surface));
        self.shell.on_window_closed(surface.wl_surface(), self.now);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    fn grab(&mut self, surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        let _ = find_popup_root_surface(&PopupKind::Xdg(surface));
    }
}

impl SeatHandler for LoongState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|s| self.display_handle.get_client(s.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client);
    }
}

impl SelectionHandler for LoongState {
    type SelectionUserData = ();
}

impl DataDeviceHandler for LoongState {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl DndGrabHandler for LoongState {}

impl WaylandDndGrabHandler for LoongState {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        _seat: Seat<Self>,
        _serial: Serial,
        _type: GrabType,
    ) {
        // 平板 v1 不支持拖放。
        source.cancel();
    }
}

impl OutputHandler for LoongState {}

impl smithay::wayland::pointer_constraints::PointerConstraintsHandler for LoongState {}

smithay::delegate_dispatch2!(LoongState);
