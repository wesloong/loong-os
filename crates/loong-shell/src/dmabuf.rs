//! linux-dmabuf：客户端直接提交 GPU 缓冲（WebKit 的 GPU 渲染依赖它）。
//!
//! - **v4 + feedback**：主设备（main device）是合成器渲染所用 GPU 的 render node（没有 render
//!   node 时用 primary node）。WebKit 的 WPEPlatform 从这里找到 GPU：有了它，网页进程才会在
//!   这块 GPU 上用 GBM 分配缓冲、以 dmabuf 交给合成器；否则只能退回共享内存。
//! - 格式与 modifier 取自渲染器能导入的（`ImportDma::dmabuf_formats`）。只有主 tranche：
//!   应用窗口总要经过自定义着色器合成（圆角、晕染遮罩，见 [`RoundedSurface`]），
//!   不会直接扫描输出客户端缓冲，scanout tranche 只会误导客户端。
//! - 没有 DRM 设备（Mesa llvmpipe 等纯软件渲染）时退化为 v3：只公布格式表，不发 feedback
//!   （协议要求 feedback 必须给出一个设备）。
//! - 导入在 [`DmabufHandler::dmabuf_imported`] 中先排队，由后端在本轮事件处理之后交给
//!   渲染器（[`LoongState::import_pending_dmabufs`]）：渲染器归后端所有，协议回调里拿不到；
//!   `ImportNotifier` 本身就支持异步完成。
//! - 隐式同步：与 Smithay 的 anvil 相同，提交带 dmabuf 的画面前先等缓冲可读（客户端的
//!   GPU 渲染已完成），等待期间这次提交被挂起，合成器照常出帧、不被客户端拖慢。
//!
//! [`RoundedSurface`]: crate::render::element::RoundedSurface

use smithay::{
    backend::{
        allocator::{
            Buffer, Format,
            dmabuf::{Dmabuf, DmabufSource},
        },
        drm::DrmNode,
        renderer::{ImportDma, gles::GlesRenderer},
    },
    reexports::{
        calloop::Interest,
        wayland_server::{Client, DisplayHandle, Resource, protocol::wl_surface::WlSurface},
    },
    wayland::{
        compositor::{
            BufferAssignment, SurfaceAttributes, add_blocker, add_pre_commit_hook, with_states,
        },
        dmabuf::{
            DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
            get_dmabuf,
        },
    },
};

use crate::state::LoongState;

/// 把“等 dmabuf 可读”的事件源注册到事件循环；就绪后通知该客户端的提交队列。
pub type ReadyWaiter = Box<dyn Fn(DmabufSource, Client) -> bool>;

/// linux-dmabuf 的全部状态。
pub struct DmabufSupport {
    state: DmabufState,
    global: Option<DmabufGlobal>,
    /// 等待渲染器导入的缓冲。
    pending: Vec<(Dmabuf, ImportNotifier)>,
    waiter: Option<ReadyWaiter>,
}

impl Default for DmabufSupport {
    fn default() -> Self {
        DmabufSupport {
            state: DmabufState::new(),
            global: None,
            pending: Vec::new(),
            waiter: None,
        }
    }
}

impl DmabufSupport {
    /// 由 [`LoongState::register_sources`] 设置（事件循环的数据类型由后端决定）。
    pub fn set_waiter(&mut self, waiter: ReadyWaiter) {
        self.waiter = Some(waiter);
    }
}

/// 公布哪种 linux-dmabuf 全局对象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmabufMode {
    /// v4：带 feedback，主设备为该 DRM 节点。
    Feedback(DrmNode),
    /// v3：只有格式表（没有可公布的 DRM 设备）。
    FormatsOnly,
    /// 不公布（渲染器不能导入任何 dmabuf）。
    Disabled,
}

/// 根据渲染所用的 DRM 节点和可导入格式数决定公布方式。
pub fn choose_mode(node: Option<DrmNode>, format_count: usize) -> DmabufMode {
    match node {
        _ if format_count == 0 => DmabufMode::Disabled,
        Some(node) => DmabufMode::Feedback(node),
        None => DmabufMode::FormatsOnly,
    }
}

impl LoongState {
    /// 创建 linux-dmabuf 全局对象。`render_node` 为合成器渲染所用 GPU 的 DRM 节点
    /// （优先 render node），`formats` 为渲染器能导入的格式。
    pub fn init_dmabuf(
        &mut self,
        render_node: Option<DrmNode>,
        formats: impl IntoIterator<Item = Format>,
    ) -> DmabufMode {
        let formats: Vec<Format> = formats.into_iter().collect();
        let mode = choose_mode(render_node, formats.len());
        let dh = self.display_handle.clone();
        let codes = {
            let mut c: Vec<_> = formats.iter().map(|f| f.code).collect();
            c.dedup();
            c.len()
        };
        let global = match mode {
            DmabufMode::Feedback(node) => {
                match DmabufFeedbackBuilder::new(node.dev_id(), formats.iter().copied()).build() {
                    Ok(feedback) => {
                        tracing::info!(
                            "linux-dmabuf v4：主设备 {node}，{codes} 种格式（{} 个格式/modifier 组合）",
                            formats.len()
                        );
                        Some(
                            self.dmabuf
                                .state
                                .create_global_with_default_feedback::<LoongState>(&dh, &feedback),
                        )
                    }
                    Err(e) => {
                        tracing::warn!("生成 dmabuf feedback 失败（{e}），退回 v3");
                        Some(self.dmabuf.state.create_global::<LoongState>(&dh, formats))
                    }
                }
            }
            DmabufMode::FormatsOnly => {
                tracing::info!(
                    "linux-dmabuf v3：渲染器没有 DRM 设备（软件渲染），只公布 {codes} 种格式"
                );
                Some(self.dmabuf.state.create_global::<LoongState>(&dh, formats))
            }
            DmabufMode::Disabled => {
                tracing::info!("渲染器不能导入 dmabuf，不公布 linux-dmabuf");
                None
            }
        };
        self.dmabuf.global = global;
        mode
    }

    /// 把排队的 dmabuf 交给渲染器导入，并通知客户端结果。每轮事件处理之后调用。
    pub fn import_pending_dmabufs(&mut self, renderer: &mut GlesRenderer) {
        for (dmabuf, notifier) in std::mem::take(&mut self.dmabuf.pending) {
            match renderer.import_dmabuf(&dmabuf, None) {
                Ok(_) => {
                    let _ = notifier.successful::<LoongState>();
                }
                Err(e) => {
                    tracing::warn!(
                        "导入 dmabuf 失败（{:?} {}x{}）：{e}",
                        dmabuf.format(),
                        dmabuf.width(),
                        dmabuf.height()
                    );
                    notifier.failed();
                }
            }
        }
    }
}

impl DmabufHandler for LoongState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf.state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        self.dmabuf.pending.push((dmabuf, notifier));
        // 让后端尽快处理（真机后端在每轮事件之后检查）。
        self.mark_damaged();
    }
}

/// 新表面：提交 dmabuf 时先等缓冲可读（隐式同步），再让这次提交生效。
pub fn add_ready_hook(surface: &WlSurface) {
    add_pre_commit_hook::<LoongState, _>(surface, |state, _dh: &DisplayHandle, surface| {
        let Some(waiter) = state.dmabuf.waiter.as_ref() else {
            return;
        };
        let dmabuf = with_states(surface, |states| {
            match states
                .cached_state
                .get::<SurfaceAttributes>()
                .pending()
                .buffer
                .as_ref()
            {
                Some(BufferAssignment::NewBuffer(buffer)) => get_dmabuf(buffer).cloned().ok(),
                _ => None,
            }
        });
        let Some(dmabuf) = dmabuf else {
            return;
        };
        // 已经可读（例如软件渲染的缓冲）时不需要等待。
        if let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ)
            && let Some(client) = surface.client()
            && waiter(source, client)
        {
            add_blocker(surface, blocker);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_follows_device_and_formats() {
        assert_eq!(choose_mode(None, 0), DmabufMode::Disabled);
        assert_eq!(choose_mode(None, 12), DmabufMode::FormatsOnly);
    }
}
