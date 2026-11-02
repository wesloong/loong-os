//! 合成器全局状态：Wayland 协议状态 + 系统界面模型。

use std::{ffi::OsString, sync::Arc};

use smithay::{
    desktop::{PopupManager, Window},
    input::{Seat, SeatState, keyboard::XkbConfig},
    output::Output,
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
        },
    },
    wayland::{
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

use crate::{
    app_protocol::AppProtocolState, device::DeviceConfig, dmabuf::DmabufSupport,
    protocols::SurfaceProtocols, shell::Shell,
};

/// 合成器状态。
pub struct LoongState {
    pub display_handle: DisplayHandle,
    pub socket_name: OsString,
    /// 尚未注册到事件循环的 display 与 socket（见 [`LoongState::register_sources`]）。
    pending: Option<(Display<LoongState>, ListeningSocketSource)>,
    /// 客户端提交了新画面，需要重画。
    damaged: bool,

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
    /// linux-dmabuf（由后端在渲染器就绪后调用 [`LoongState::init_dmabuf`] 创建全局对象）。
    pub dmabuf: DmabufSupport,
    /// viewporter、单像素缓冲、分数缩放（只需持有）。
    _surface_protocols: SurfaceProtocols,
    /// Loong 私有协议 loong_app_v1（状态栏样式、主题色，只需持有）。
    _app_protocol: AppProtocolState,

    /// 所有客户端窗口（含后台应用）。
    pub windows: Vec<Window>,
    /// 按下时交给了应用的手指（触摸槽位 → 窗口）：抬起之前的移动与抬起都交给它。
    pub client_touches: Vec<(u32, Window)>,
    /// 唯一的输出（平板只有一块屏幕）。
    pub output: Option<Output>,
    /// 系统界面。
    pub shell: Shell,
    /// 当前时间（秒），由后端驱动（真实时间或测试用虚拟时钟）。
    pub now: f64,
    /// PWA 注册表（`registry::service::init` 之后才有）。
    pub registry: Option<crate::registry::service::Service>,
}

impl LoongState {
    /// 创建状态并注册到以 `LoongState` 为数据的事件循环（headless 后端用）。
    pub fn new(
        event_loop: &mut EventLoop<'static, Self>,
        display: Display<Self>,
        device: &DeviceConfig,
    ) -> anyhow::Result<Self> {
        let mut state = Self::new_detached(display, device)?;
        state.register_sources(&event_loop.handle(), |s: &mut LoongState| s)?;
        Ok(state)
    }

    /// 创建状态，但暂不注册事件源（事件循环的数据类型由后端决定）。
    pub fn new_detached(display: Display<Self>, device: &DeviceConfig) -> anyhow::Result<Self> {
        let dh = display.handle();
        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let surface_protocols = SurfaceProtocols::new(&dh);
        let app_protocol = AppProtocolState::new(&dh);

        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(XkbConfig::default(), 400, 30)?;
        seat.add_pointer();
        seat.add_touch();
        // 屏幕键盘：text-input-v3，合成器自己当输入法（见 ime.rs）。
        crate::ime::init_text_input(&dh, &seat);

        let socket = ListeningSocketSource::new_auto()?;
        let socket_name = socket.socket_name().to_os_string();
        let (w, h) = device.logical_size();
        let mut shell = Shell::new(w, h, device.screen.corner_radius);
        shell.set_theme(device.theme());
        shell.set_reduced_motion(crate::device::reduced_motion());

        Ok(LoongState {
            display_handle: dh,
            socket_name,
            pending: Some((display, socket)),
            damaged: true,
            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            popups: PopupManager::default(),
            seat,
            dmabuf: DmabufSupport::default(),
            _surface_protocols: surface_protocols,
            _app_protocol: app_protocol,
            windows: Vec::new(),
            client_touches: Vec::new(),
            output: None,
            shell,
            now: 0.0,
            registry: None,
        })
    }

    /// 把 Wayland socket 与 display 注册到事件循环。`get` 从事件循环数据中取出 `LoongState`。
    pub fn register_sources<D: 'static>(
        &mut self,
        handle: &LoopHandle<'static, D>,
        get: fn(&mut D) -> &mut LoongState,
    ) -> anyhow::Result<()> {
        // dmabuf 隐式同步：缓冲可读后，通知该客户端被挂起的提交可以生效。
        let waiter_handle = handle.clone();
        self.dmabuf.set_waiter(Box::new(move |source, client| {
            waiter_handle
                .insert_source(source, move |_, _, data| {
                    let state = get(data);
                    let dh = state.display_handle.clone();
                    state
                        .client_compositor_state(&client)
                        .blocker_cleared(state, &dh);
                    Ok(())
                })
                .is_ok()
        }));
        let (display, socket) = self
            .pending
            .take()
            .ok_or_else(|| anyhow::anyhow!("事件源已注册"))?;
        handle
            .insert_source(socket, move |stream, _, data| {
                let state = get(data);
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
                move |_, display, data| {
                    let state = get(data);
                    // SAFETY: display 在事件源存在期间不会被 drop。
                    unsafe {
                        display.get_mut().dispatch_clients(state).ok();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| anyhow::anyhow!("注册 Wayland display 失败: {e}"))?;
        Ok(())
    }

    /// 标记需要重画（客户端提交新画面时调用）。
    pub fn mark_damaged(&mut self) {
        self.damaged = true;
    }

    /// 取出并清除“需要重画”标记。
    pub fn take_damage(&mut self) -> bool {
        std::mem::take(&mut self.damaged)
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
