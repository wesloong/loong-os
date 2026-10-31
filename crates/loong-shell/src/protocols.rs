//! 表面缩放与缓冲相关的协议：
//!
//! - `wp_viewporter`：客户端指定表面的目标尺寸、裁剪缓冲（浏览器用它把单像素缓冲拉伸成整个
//!   窗口的纯色底，分数缩放时用物理像素缓冲配逻辑尺寸）。
//! - `wp_single_pixel_buffer_manager_v1`：1×1 的纯色缓冲，不占内存（否则 2560×1600 平板上
//!   一张纸色底就是 16MB 共享内存）。渲染见 [`SurfaceContent::Solid`]。
//! - `wp_fractional_scale_v1`：告诉客户端屏幕的分数缩放，客户端按物理像素出图。
//!
//! 视口与单像素缓冲由 Smithay 在提交时处理（表面视图、缓冲类型），这里只需创建全局对象。
//!
//! [`SurfaceContent::Solid`]: crate::render::element::SurfaceContent::Solid

use smithay::{
    reexports::wayland_server::{DisplayHandle, protocol::wl_surface::WlSurface},
    wayland::{
        compositor::with_states,
        fractional_scale::{
            FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale,
        },
        single_pixel_buffer::SinglePixelBufferState,
        viewporter::ViewporterState,
    },
};

use crate::state::LoongState;

/// 这些全局对象的状态（持有即可）。
pub struct SurfaceProtocols {
    _viewporter: ViewporterState,
    _single_pixel_buffer: SinglePixelBufferState,
    _fractional_scale: FractionalScaleManagerState,
}

impl SurfaceProtocols {
    pub fn new(dh: &DisplayHandle) -> Self {
        SurfaceProtocols {
            _viewporter: ViewporterState::new::<LoongState>(dh),
            _single_pixel_buffer: SinglePixelBufferState::new::<LoongState>(dh),
            _fractional_scale: FractionalScaleManagerState::new::<LoongState>(dh),
        }
    }
}

impl FractionalScaleHandler for LoongState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // 只有一块屏幕，缩放固定：客户端一建立分数缩放对象就告诉它。
        let scale = self
            .output
            .as_ref()
            .map(|o| o.current_scale().fractional_scale())
            .unwrap_or(1.0);
        with_states(&surface, |states| {
            with_fractional_scale(states, |fs| fs.set_preferred_scale(scale));
        });
    }
}
