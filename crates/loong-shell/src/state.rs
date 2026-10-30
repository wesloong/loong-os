//! 合成器全局状态：Wayland 协议状态 + 系统界面模型。

use std::{ffi::OsString, sync::Arc};

use smithay::{
    desktop::{PopupManager, Window},
    input::{Seat, SeatState, keyboard::XkbConfig},
    output::Output,
    reexports::{
        calloop::{EventLoop, Interest, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
        },
    },
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

use crate::{device::DeviceConfig, shell::Shell};

/// 合成器状态。
pub struct LoongState {
    pub display_handle: DisplayHandle,
    pub socket_name: OsString,
    #[allow(dead_code, reason = "真机后端用于退出事件循环")]
    pub loop_signal: LoopSignal,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    /// 只需持有，保证 xdg-output 全局对象存在。
    #[allow(dead_code)]
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<LoongState>,
    pub data_device_state: DataDeviceState,
    pub popups: PopupManager,
    pub seat: Seat<LoongState>,

    /// 所有客户端窗口（含后台应用）。
    pub windows: Vec<Window>,
    /// 唯一的输出（平板只有一块屏幕）。
    pub output: Option<Output>,
    /// 系统界面。
    pub shell: Shell,
    /// 当前时间（秒），由后端驱动（真实 vblank 时间或测试用虚拟时钟）。
    pub now: f64,
}

impl LoongState {
    pub fn new(
        event_loop: &mut EventLoop<Self>,
        display: Display<Self>,
        device: &DeviceConfig,
    ) -> anyhow::Result<Self> {
        let dh = display.handle();
        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);

        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(XkbConfig::default(), 400, 30)?;
        seat.add_pointer();
        seat.add_touch();

        let socket_name = Self::init_wayland_listener(display, event_loop)?;
        let (w, h) = device.logical_size();

        Ok(LoongState {
            display_handle: dh,
            socket_name,
            loop_signal: event_loop.get_signal(),
            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            popups: PopupManager::default(),
            seat,
            windows: Vec::new(),
            output: None,
            shell: Shell::new(w, h, device.screen.corner_radius),
            now: 0.0,
        })
    }

    fn init_wayland_listener(
        display: Display<Self>,
        event_loop: &mut EventLoop<Self>,
    ) -> anyhow::Result<OsString> {
        let socket = ListeningSocketSource::new_auto()?;
        let name = socket.socket_name().to_os_string();
        let handle = event_loop.handle();
        handle
            .insert_source(socket, |stream, _, state| {
                if let Err(e) = state
                    .display_handle
                    .insert_client(stream, Arc::new(ClientState::default()))
                {
                    tracing::warn!("接受客户端失败: {e}");
                }
            })
            .map_err(|e| anyhow::anyhow!("监听 Wayland socket 失败: {e}"))?;
        handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // SAFETY: display 在事件源存在期间不会被 drop。
                    unsafe {
                        display.get_mut().dispatch_clients(state).ok();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| anyhow::anyhow!("注册 Wayland display 失败: {e}"))?;
        Ok(name)
    }
}

/// 每个客户端附带的数据。
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
