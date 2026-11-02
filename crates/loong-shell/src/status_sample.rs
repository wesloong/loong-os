//! 状态栏取样：应用既没有指定样式、也没有给出主题色时，看状态栏下方应用画的是亮还是暗。
//!
//! - 取窗口表面树中盖住状态栏区域（宽 × 24pt）大半的**最上层**表面（普通应用就是根表面，
//!   浏览器网页全屏时是网页的子表面），只看这个表面落在状态栏区域里的那一块。
//! - 只读 CPU 能直接读的缓冲：`wl_shm`（ARGB/XRGB/ABGR/XBGR8888）与单像素缓冲。dmabuf 等
//!   GPU 缓冲不回读（要等 GPU、打断流水线），这时返回 `None`，由调用方退回系统外观。
//! - 只在提交的损坏区域与这一块相交（或这一块的位置、纸色变了）时重新取样，结果缓存在表面上
//!   （[`SampleCache`]）；其余帧只比较一下提交计数。
//! - 降采样：至多 [`SAMPLE_COLS`] × [`SAMPLE_ROWS`] 个点。半透明的像素按窗口下面的纸色合成。
//!   每个点按 WCAG 对比度“投票”（浓墨对比度更高的算亮），亮点的比例经回差（[`ink_for_share`]）
//!   换算成墨。投票而不是取平均色：顶部有一小块高对比度的图案（标志、头像）时不至于被拉偏。

use std::sync::Mutex;

use smithay::{
    backend::renderer::utils::{CommitCounter, RendererSurfaceStateUserData},
    desktop::Window,
    reexports::wayland_server::{
        Resource,
        protocol::{wl_buffer::WlBuffer, wl_shm},
    },
    utils::{Buffer as BufferCoord, Logical, Point, Rectangle, Size, Transform},
    wayland::{
        compositor::{SurfaceData, TraversalAction, with_surface_tree_downward},
        shm::with_buffer_contents,
        single_pixel_buffer::get_single_pixel_buffer,
    },
};

use crate::shell::status::{StatusInk, ink_for_color, ink_for_share};

/// 横向至多取多少个点。
pub const SAMPLE_COLS: usize = 64;
/// 纵向至多取多少行。
pub const SAMPLE_ROWS: usize = 4;

/// 32 位像素在内存中的字节顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOrder {
    /// B, G, R, A（wl_shm 的 ARGB8888 / XRGB8888，小端）。
    Bgra,
    /// R, G, B, A（wl_shm 的 ABGR8888 / XBGR8888）。
    Rgba,
}

/// 一块 32 位像素（预乘 alpha，与 wl_shm 一致）。
#[derive(Debug, Clone, Copy)]
pub struct Pixels<'a> {
    pub data: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub order: ByteOrder,
    /// X 格式：alpha 字节没有意义，按不透明处理。
    pub opaque: bool,
}

/// 像素矩形 [x0, x1) × [y0, y1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

/// 预乘像素合成到不透明的纸色（0xRRGGBB）上。
fn over_paper([r, g, b, a]: [u8; 4], paper: u32) -> u32 {
    let k = 255 - a as u32;
    let ch = |c: u8, shift: u32| (c as u32 + (((paper >> shift) & 0xff) * k + 127) / 255).min(255);
    (ch(r, 16) << 16) | (ch(g, 8) << 8) | ch(b, 0)
}

/// 在 `rect` 内均匀取至多 [`SAMPLE_COLS`] × [`SAMPLE_ROWS`] 个点，返回“亮”点（浓墨的对比度更高）
/// 的比例。区域为空或越界时返回 `None`。
pub fn light_share(px: &Pixels<'_>, rect: PixelRect, paper: u32) -> Option<f32> {
    let x1 = rect.x1.min(px.width);
    let y1 = rect.y1.min(px.height);
    if rect.x0 >= x1 || rect.y0 >= y1 {
        return None;
    }
    let (w, h) = (x1 - rect.x0, y1 - rect.y0);
    let cols = w.min(SAMPLE_COLS);
    let rows = h.min(SAMPLE_ROWS);
    let (mut light, mut total) = (0usize, 0usize);
    for j in 0..rows {
        // 每格取中心点。
        let y = rect.y0 + (2 * j + 1) * h / (2 * rows);
        for i in 0..cols {
            let x = rect.x0 + (2 * i + 1) * w / (2 * cols);
            let at = y * px.stride + x * 4;
            let p = px.data.get(at..at + 4)?;
            let [r, g, b, a] = match px.order {
                ByteOrder::Bgra => [p[2], p[1], p[0], p[3]],
                ByteOrder::Rgba => [p[0], p[1], p[2], p[3]],
            };
            let a = if px.opaque { 255 } else { a };
            if ink_for_color(over_paper([r, g, b, a], paper)) == StatusInk::Dark {
                light += 1;
            }
            total += 1;
        }
    }
    Some(light as f32 / total as f32)
}

/// 每个表面缓存的取样结果。
#[derive(Debug, Default)]
struct SampleCache {
    /// 取样时的提交计数。
    commit: Option<CommitCounter>,
    /// 取样的缓冲区域与纸色：变了就重新取样。
    key: Option<(Rectangle<i32, BufferCoord>, u32)>,
    /// 结论（缓冲不可读时为 `None`）。
    ink: Option<StatusInk>,
}

/// 窗口状态栏区域下方的内容用哪种墨（`None`：没法取样，例如 dmabuf）。
///
/// `strip` 为状态栏区域的大小（逻辑点，贴窗口可见区域的顶边），`paper` 为窗口下面的纸色。
pub fn sample_window(window: &Window, strip: Size<f64, Logical>, paper: u32) -> Option<StatusInk> {
    let root = window.toplevel()?.wl_surface().clone();
    if !root.is_alive() {
        return None;
    }
    let geo = window.geometry();
    let strip = Rectangle::<f64, Logical>::new(geo.loc.to_f64(), strip);
    // 最上层、盖住状态栏区域大半的表面（向下遍历时最上层的先到）。
    let mut found: Option<Option<StatusInk>> = None;
    with_surface_tree_downward(
        &root,
        Point::<i32, Logical>::from((0, 0)),
        |_, states, loc| match surface_view(states) {
            Some((offset, _)) => TraversalAction::DoChildren(*loc + offset),
            None => TraversalAction::SkipChildren,
        },
        |_, states, loc| {
            if found.is_some() {
                return;
            }
            let Some((offset, size)) = surface_view(states) else {
                return;
            };
            let rect = Rectangle::new(*loc + offset, size).to_f64();
            let Some(overlap) = rect.intersection(strip) else {
                return;
            };
            if overlap.size.w * overlap.size.h < 0.5 * strip.size.w * strip.size.h {
                return;
            }
            // 表面局部坐标中的那一块。
            let local = Rectangle::new(overlap.loc - rect.loc, overlap.size);
            found = Some(sample_surface(states, local, paper));
        },
        |_, _, _| true,
    );
    found.flatten()
}

/// 表面相对父表面的偏移与显示尺寸（逻辑点）；没有映射时为 `None`。
fn surface_view(states: &SurfaceData) -> Option<(Point<i32, Logical>, Size<i32, Logical>)> {
    let data = states.data_map.get::<RendererSurfaceStateUserData>()?;
    let view = data.lock().ok()?.view()?;
    Some((view.offset, view.dst))
}

/// 取样一个表面上的一块（表面局部逻辑坐标），带缓存。
fn sample_surface(
    states: &SurfaceData,
    local: Rectangle<f64, Logical>,
    paper: u32,
) -> Option<StatusInk> {
    let data = states.data_map.get::<RendererSurfaceStateUserData>()?;
    let rs = data.lock().ok()?;
    let view = rs.view()?;
    let buffer = rs.buffer()?;
    if rs.buffer_transform() != Transform::Normal || view.dst.w <= 0 || view.dst.h <= 0 {
        return None;
    }
    // 表面局部逻辑坐标 → 缓冲像素：先按 viewport 的源矩形缩放，再乘缓冲缩放。
    let scale = rs.buffer_scale().max(1) as f64;
    let kx = view.src.size.w / view.dst.w as f64;
    let ky = view.src.size.h / view.dst.h as f64;
    let to_buffer = |x: f64, y: f64| {
        (
            (view.src.loc.x + x * kx) * scale,
            (view.src.loc.y + y * ky) * scale,
        )
    };
    let (bx0, by0) = to_buffer(local.loc.x, local.loc.y);
    let (bx1, by1) = to_buffer(local.loc.x + local.size.w, local.loc.y + local.size.h);
    let region = Rectangle::<i32, BufferCoord>::from_extremities(
        (bx0.floor() as i32, by0.floor() as i32),
        (bx1.ceil() as i32, by1.ceil() as i32),
    );
    if region.is_empty() {
        return None;
    }

    states
        .data_map
        .insert_if_missing_threadsafe(|| Mutex::new(SampleCache::default()));
    let mut cache = states
        .data_map
        .get::<Mutex<SampleCache>>()
        .expect("刚插入")
        .lock()
        .ok()?;
    let key = (region, paper);
    let stale = cache.key != Some(key)
        || cache.commit.is_none()
        || rs
            .damage_since(cache.commit)
            .iter()
            .any(|d| d.overlaps(region));
    if stale {
        let share = read_light_share(buffer, region, paper);
        let previous = cache.ink;
        cache.ink = share.map(|s| ink_for_share(s, previous));
        cache.commit = Some(rs.current_commit());
        cache.key = Some(key);
        tracing::debug!("状态栏取样 {:?}：亮点 {share:?} → {:?}", region, cache.ink);
    }
    cache.ink
}

/// 读缓冲中 `region`（缓冲像素）的亮点比例。只读 wl_shm 与单像素缓冲。
fn read_light_share(
    buffer: &WlBuffer,
    region: Rectangle<i32, BufferCoord>,
    paper: u32,
) -> Option<f32> {
    if let Ok(pixel) = get_single_pixel_buffer(buffer) {
        // 单像素缓冲的值是预乘的。
        let rgb = over_paper(pixel.rgba8888(), paper);
        return Some(if ink_for_color(rgb) == StatusInk::Dark {
            1.0
        } else {
            0.0
        });
    }
    with_buffer_contents(buffer, |ptr, len, data| {
        let (order, opaque) = match data.format {
            wl_shm::Format::Argb8888 => (ByteOrder::Bgra, false),
            wl_shm::Format::Xrgb8888 => (ByteOrder::Bgra, true),
            wl_shm::Format::Abgr8888 => (ByteOrder::Rgba, false),
            wl_shm::Format::Xbgr8888 => (ByteOrder::Rgba, true),
            _ => return None,
        };
        let (offset, stride) = (
            usize::try_from(data.offset).ok()?,
            usize::try_from(data.stride).ok()?,
        );
        let (width, height) = (
            usize::try_from(data.width).ok()?,
            usize::try_from(data.height).ok()?,
        );
        let size = stride.checked_mul(height)?;
        if offset.checked_add(size)? > len || stride < width * 4 {
            return None;
        }
        // SAFETY: Smithay 保证 ptr..ptr+len 在本次回调期间映射有效（读到被截断的部分会以 SIGBUS
        // 形式报告给客户端）；上面已检查缓冲落在池内。内容可能被客户端同时改写，读到的只是
        // 某一时刻的像素，不影响内存安全。
        let bytes = unsafe { std::slice::from_raw_parts(ptr.add(offset), size) };
        let px = Pixels {
            data: bytes,
            width,
            height,
            stride,
            order,
            opaque,
        };
        let rect = PixelRect {
            x0: region.loc.x.max(0) as usize,
            y0: region.loc.y.max(0) as usize,
            x1: (region.loc.x + region.size.w).max(0) as usize,
            y1: (region.loc.y + region.size.h).max(0) as usize,
        };
        light_share(&px, rect, paper)
    })
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const XUAN: u32 = 0xF3EEE2;
    const YEMO_RAISED: u32 = 0x1F1D17;

    /// w × h 的 BGRA 缓冲，每个像素由 `f(x, y)` 给出（0xAARRGGBB，预乘）。
    fn bgra(w: usize, h: usize, stride: usize, f: impl Fn(usize, usize) -> u32) -> Vec<u8> {
        let mut data = vec![0xAB; stride * h];
        for y in 0..h {
            for x in 0..w {
                let at = y * stride + x * 4;
                data[at..at + 4].copy_from_slice(&f(x, y).to_le_bytes());
            }
        }
        data
    }

    fn px(data: &[u8], w: usize, h: usize, stride: usize) -> Pixels<'_> {
        Pixels {
            data,
            width: w,
            height: h,
            stride,
            order: ByteOrder::Bgra,
            opaque: false,
        }
    }

    fn top(w: usize, h: usize) -> PixelRect {
        PixelRect {
            x0: 0,
            y0: 0,
            x1: w,
            y1: h,
        }
    }

    #[test]
    fn uniform_light_and_dark_strips() {
        let (w, h, stride) = (1280, 800, 1280 * 4 + 16);
        let light = bgra(w, h, stride, |_, _| 0xFFF3EEE2);
        let dark = bgra(w, h, stride, |_, _| 0xFF15140F);
        let strip = top(w, 24);
        assert_eq!(
            light_share(&px(&light, w, h, stride), strip, XUAN),
            Some(1.0)
        );
        assert_eq!(
            light_share(&px(&dark, w, h, stride), strip, XUAN),
            Some(0.0)
        );
    }

    #[test]
    fn only_the_strip_is_sampled() {
        // 顶部 24 行亮、其余暗：只看状态栏区域。
        let (w, h, stride) = (640, 400, 640 * 4);
        let data = bgra(
            w,
            h,
            stride,
            |_, y| if y < 24 { 0xFFFFFFFF } else { 0xFF000000 },
        );
        assert_eq!(
            light_share(&px(&data, w, h, stride), top(w, 24), XUAN),
            Some(1.0)
        );
        let below = PixelRect {
            x0: 0,
            y0: 24,
            x1: w,
            y1: 48,
        };
        assert_eq!(
            light_share(&px(&data, w, h, stride), below, XUAN),
            Some(0.0)
        );
    }

    #[test]
    fn mixed_content_votes() {
        // 左边 3/4 暗、右边 1/4 亮。
        let (w, h, stride) = (1280, 24, 1280 * 4);
        let data = bgra(
            w,
            h,
            stride,
            |x, _| {
                if x < 960 { 0xFF202020 } else { 0xFFF0F0F0 }
            },
        );
        let share = light_share(&px(&data, w, h, stride), top(w, h), XUAN).unwrap();
        assert!((share - 0.25).abs() < 0.02, "{share}");
        assert_eq!(ink_for_share(share, None), StatusInk::Light);
    }

    #[test]
    fn transparent_pixels_use_the_paper_below() {
        let (w, h, stride) = (64, 24, 256);
        let clear = bgra(w, h, stride, |_, _| 0x00000000);
        assert_eq!(
            light_share(&px(&clear, w, h, stride), top(w, h), XUAN),
            Some(1.0)
        );
        assert_eq!(
            light_share(&px(&clear, w, h, stride), top(w, h), YEMO_RAISED),
            Some(0.0)
        );
        // X 格式忽略 alpha 字节（这里是 0），按不透明的白处理。
        let x = bgra(w, h, stride, |_, _| 0x00FFFFFF);
        let opaque = Pixels {
            opaque: true,
            ..px(&x, w, h, stride)
        };
        assert_eq!(light_share(&opaque, top(w, h), YEMO_RAISED), Some(1.0));
    }

    #[test]
    fn byte_order_matters() {
        // 内存中的字节 10 10 F0 FF：按 R,G,B,A 是蓝 #1010F0（亮度 0.07，用月白），
        // 按 B,G,R,A 是红 #F01010（亮度 0.19，用浓墨）。
        let (w, h) = (8, 4);
        let data: Vec<u8> = [0x10, 0x10, 0xF0, 0xFF].repeat(w * h);
        let rgba = Pixels {
            data: &data,
            width: w,
            height: h,
            stride: w * 4,
            order: ByteOrder::Rgba,
            opaque: false,
        };
        let bgra = Pixels {
            order: ByteOrder::Bgra,
            ..rgba
        };
        assert_eq!(light_share(&rgba, top(w, h), XUAN), Some(0.0));
        assert_eq!(light_share(&bgra, top(w, h), XUAN), Some(1.0));
    }

    #[test]
    fn downsampling_reads_few_pixels() {
        // 2560 宽、48 行的区域只读 64 × 4 个点：其余像素是会被判为亮的白，采样点都落在暗格上。
        let (w, h, stride) = (2560, 48, 2560 * 4);
        let cols = SAMPLE_COLS;
        let rows = SAMPLE_ROWS;
        let data = bgra(w, h, stride, |x, y| {
            let on_x = (0..cols).any(|i| x == (2 * i + 1) * w / (2 * cols));
            let on_y = (0..rows).any(|j| y == (2 * j + 1) * h / (2 * rows));
            if on_x && on_y { 0xFF000000 } else { 0xFFFFFFFF }
        });
        assert_eq!(
            light_share(&px(&data, w, h, stride), top(w, h), XUAN),
            Some(0.0)
        );
    }

    #[test]
    fn empty_or_out_of_range_regions() {
        let (w, h, stride) = (16, 16, 64);
        let data = bgra(w, h, stride, |_, _| 0xFFFFFFFF);
        let p = px(&data, w, h, stride);
        let empty = PixelRect {
            x0: 4,
            y0: 4,
            x1: 4,
            y1: 8,
        };
        assert_eq!(light_share(&p, empty, XUAN), None);
        // 超出缓冲的部分被裁掉。
        let wide = PixelRect {
            x0: 8,
            y0: 0,
            x1: 100,
            y1: 100,
        };
        assert_eq!(light_share(&p, wide, XUAN), Some(1.0));
        // 数据比声明的小：不越界读。
        let short = Pixels {
            data: &data[..100],
            ..p
        };
        assert_eq!(light_share(&short, top(w, h), XUAN), None);
    }

    #[test]
    fn premultiplied_pixels_over_paper() {
        // 半透明白（预乘 0x80808080）合成到夜墨绢上：#8F8E8B，亮。
        assert_eq!(over_paper([0x80, 0x80, 0x80, 0x80], YEMO_RAISED), 0x8F8E8B);
        assert_eq!(over_paper([0, 0, 0, 0], XUAN), XUAN);
        assert_eq!(over_paper([0x12, 0x34, 0x56, 0xFF], XUAN), 0x123456);
    }
}
