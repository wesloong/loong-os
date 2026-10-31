/*
 * LoongView：一个 WPEView = 窗口根表面的一个 wl_subsurface（desync 模式，各自独立出帧）。
 *
 * 出帧流程（WebKit → 合成器）：
 *   render_buffer(WPEBuffer) → attach/damage/frame/commit
 *   wl_surface.frame 完成 → wpe_view_buffer_rendered()（WebKit 才会画下一帧）
 *
 * 共享内存（WPEBufferSHM，当前 CPU 渲染走这条路）：WebKit 的共享内存不能直接交给合成器
 * （拿不到 fd），因此每个 view 自带一组 wl_shm 缓冲（交换链，最多 4 个），把 WebKit 的
 * 像素拷进去。拷贝只拷“这个缓冲落后的区域”（按包围盒累计的损伤），拷完立即把 WPEBuffer
 * 还给 WebKit。view 隐藏时也继续拷，重新显示时把最近的内容挂回去，不必等 WebKit 重画。
 *
 * DMA-BUF（WPEBufferDMABuf）：合成器提供 zwp_linux_dmabuf_v1 时零拷贝，
 * wl_buffer.release → wpe_view_buffer_released()。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include "loong-platform-private.h"

#include <math.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define MAX_SHM_BUFFERS 4
/* 隐藏的 view 不会收到 frame 回调，用定时器按约 60Hz 回应 WebKit。 */
#define HIDDEN_FRAME_INTERVAL_MS 16

typedef struct {
    LoongView *view; /* view 已销毁时为 NULL，合成器释放后再销毁缓冲 */
    struct wl_buffer *wl_buffer;
    guint8 *data;
    size_t size;
    int width, height, stride;
    gboolean busy;
    /* 这个缓冲落后于最新画面的区域（包围盒，x1/y1 不含） */
    gboolean dirty_all;
    int x0, y0, x1, y1;
} LoongShmBuffer;

struct _LoongView {
    WPEView parent_instance;

    struct wl_surface *surface;
    struct wl_subsurface *subsurface;
    struct wp_viewport *viewport;
    LoongToplevel *toplevel; /* 当前所在的窗口（不持有引用；WPEView 自己持有） */

    int x, y;
    int geom_width, geom_height;
    gboolean has_geometry;
    int layer;
    guint order;

    gboolean attached;     /* 表面上挂着缓冲（可见） */
    gboolean full_damage;  /* 下一帧整面损伤（重新挂上之后） */
    int surface_scale;     /* 最近一次 wl_surface_set_buffer_scale 的值 */
    struct wl_callback *frame_callback;
    WPEBuffer *frame_buffer; /* 等 frame 完成后回报 rendered */
    guint hidden_frame_id;

    GPtrArray *shm_buffers;  /* LoongShmBuffer* */
    LoongShmBuffer *current; /* 最新画面所在的缓冲 */
    gboolean reattach_pending;
    WPEBuffer *hidden_dmabuf; /* 隐藏期间收到的 DMA-BUF 帧 */
};

G_DEFINE_FINAL_TYPE(LoongView, loong_view, WPE_TYPE_VIEW)

static LoongDisplay *
view_display(LoongView *self)
{
    return LOONG_DISPLAY(wpe_view_get_display(WPE_VIEW(self)));
}

/* ───────────────────────── 共享内存交换链 ───────────────────────── */

static void
shm_buffer_free(LoongShmBuffer *buffer)
{
    if (buffer->wl_buffer)
        wl_buffer_destroy(buffer->wl_buffer);
    if (buffer->data)
        munmap(buffer->data, buffer->size);
    g_free(buffer);
}

static void
shm_buffer_release(void *data, struct wl_buffer *wl_buffer)
{
    LoongShmBuffer *buffer = data;
    buffer->busy = FALSE;
    LoongView *self = buffer->view;
    if (!self) {
        shm_buffer_free(buffer);
        return;
    }
    /* 尺寸已过时且不是当前画面：直接回收。 */
    if (buffer != self->current && self->current &&
        (buffer->width != self->current->width || buffer->height != self->current->height)) {
        g_ptr_array_remove(self->shm_buffers, buffer);
        return;
    }
    if (buffer == self->current && self->reattach_pending) {
        self->reattach_pending = FALSE;
        if (wpe_view_get_mapped(WPE_VIEW(self)) && self->subsurface) {
            wl_surface_attach(self->surface, buffer->wl_buffer, 0, 0);
            wl_surface_damage_buffer(self->surface, 0, 0, INT32_MAX, INT32_MAX);
            wl_surface_commit(self->surface);
            buffer->busy = TRUE;
            self->attached = TRUE;
        }
    }
}

static const struct wl_buffer_listener shm_buffer_listener = {
    .release = shm_buffer_release,
};

/* 从交换链里移除时：合成器还占着的缓冲交给释放回调去销毁。 */
static void
shm_buffer_discard(gpointer data)
{
    LoongShmBuffer *buffer = data;
    if (buffer->view && buffer->view->current == buffer)
        buffer->view->current = NULL;
    buffer->view = NULL;
    if (!buffer->busy)
        shm_buffer_free(buffer);
}

static LoongShmBuffer *
shm_buffer_new(LoongView *self, int width, int height, int stride)
{
    size_t size = (size_t)stride * height;
    int fd = memfd_create("loong-view", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    if (fd < 0)
        return NULL;
    if (ftruncate(fd, size) < 0) {
        close(fd);
        return NULL;
    }
    void *data = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (data == MAP_FAILED) {
        close(fd);
        return NULL;
    }
    LoongShmBuffer *buffer = g_new0(LoongShmBuffer, 1);
    buffer->view = self;
    buffer->data = data;
    buffer->size = size;
    buffer->width = width;
    buffer->height = height;
    buffer->stride = stride;
    buffer->dirty_all = TRUE;
    struct wl_shm_pool *pool = wl_shm_create_pool(loong_display_get_shm(view_display(self)), fd, size);
    /* WebKit 的像素是预乘 alpha 的 BGRA（小端 ARGB8888），正好是 wl_shm 的 ARGB8888。 */
    buffer->wl_buffer = wl_shm_pool_create_buffer(pool, 0, width, height, stride, WL_SHM_FORMAT_ARGB8888);
    wl_shm_pool_destroy(pool);
    close(fd);
    wl_buffer_add_listener(buffer->wl_buffer, &shm_buffer_listener, buffer);
    return buffer;
}

static LoongShmBuffer *
acquire_shm_buffer(LoongView *self, int width, int height, int stride)
{
    LoongShmBuffer *best = NULL;
    for (guint i = 0; i < self->shm_buffers->len;) {
        LoongShmBuffer *buffer = g_ptr_array_index(self->shm_buffers, i);
        if (buffer->width != width || buffer->height != height || buffer->stride != stride) {
            /* 尺寸变了：旧缓冲作废（占用中的等释放后销毁）。 */
            g_ptr_array_remove_index(self->shm_buffers, i);
            continue;
        }
        if (!buffer->busy && buffer != self->current) {
            /* 选落后区域最小的那个，拷得最少。 */
            if (!best || (best->dirty_all && !buffer->dirty_all) ||
                (!best->dirty_all && !buffer->dirty_all &&
                 (buffer->x1 - buffer->x0) * (buffer->y1 - buffer->y0) < (best->x1 - best->x0) * (best->y1 - best->y0)))
                best = buffer;
        }
        i++;
    }
    if (best)
        return best;
    /* 当前画面所在的缓冲如果空闲也可以复用（隐藏时常见）。 */
    if (self->current && !self->current->busy && self->current->width == width && self->current->height == height &&
        self->current->stride == stride)
        return self->current;
    if (self->shm_buffers->len >= MAX_SHM_BUFFERS)
        return NULL;
    LoongShmBuffer *buffer = shm_buffer_new(self, width, height, stride);
    if (buffer)
        g_ptr_array_add(self->shm_buffers, buffer);
    return buffer;
}

static void
dirty_add(LoongShmBuffer *buffer, int x0, int y0, int x1, int y1)
{
    if (buffer->dirty_all)
        return;
    if (buffer->x0 >= buffer->x1 || buffer->y0 >= buffer->y1) {
        buffer->x0 = x0;
        buffer->y0 = y0;
        buffer->x1 = x1;
        buffer->y1 = y1;
        return;
    }
    buffer->x0 = MIN(buffer->x0, x0);
    buffer->y0 = MIN(buffer->y0, y0);
    buffer->x1 = MAX(buffer->x1, x1);
    buffer->y1 = MAX(buffer->y1, y1);
}

/* 把 WebKit 的帧拷进交换链缓冲。返回拷贝后的缓冲；(rx0..ry1) 为本帧相对上一帧的损伤包围盒。 */
static LoongShmBuffer *
copy_shm_frame(LoongView *self, WPEBufferSHM *source, int rx0, int ry0, int rx1, int ry1, GError **error)
{
    WPEBuffer *buffer = WPE_BUFFER(source);
    int width = wpe_buffer_get_width(buffer), height = wpe_buffer_get_height(buffer);
    int stride = wpe_buffer_shm_get_stride(source);
    gsize bytes_size = 0;
    const guint8 *pixels = g_bytes_get_data(wpe_buffer_shm_get_data(source), &bytes_size);
    if (wpe_buffer_shm_get_format(source) != WPE_PIXEL_FORMAT_ARGB8888 || !pixels ||
        bytes_size < (gsize)stride * height) {
        g_set_error_literal(error, WPE_VIEW_ERROR, WPE_VIEW_ERROR_RENDER_FAILED, "不支持的共享内存缓冲");
        return NULL;
    }
    LoongShmBuffer *target = acquire_shm_buffer(self, width, height, stride);
    if (!target) {
        g_set_error_literal(error, WPE_VIEW_ERROR, WPE_VIEW_ERROR_RENDER_FAILED, "没有空闲的共享内存缓冲");
        return NULL;
    }

    /* 要拷的区域 = 这个缓冲原来落后的区域 ∪ 本帧损伤。 */
    int x0 = rx0, y0 = ry0, x1 = rx1, y1 = ry1;
    if (target->dirty_all) {
        x0 = y0 = 0;
        x1 = width;
        y1 = height;
    } else if (target->x0 < target->x1 && target->y0 < target->y1) {
        x0 = MIN(x0, target->x0);
        y0 = MIN(y0, target->y0);
        x1 = MAX(x1, target->x1);
        y1 = MAX(y1, target->y1);
    }
    x0 = CLAMP(x0, 0, width);
    x1 = CLAMP(x1, 0, width);
    y0 = CLAMP(y0, 0, height);
    y1 = CLAMP(y1, 0, height);
    if (x0 < x1 && y0 < y1) {
        if (x0 == 0 && x1 == width) {
            memcpy(target->data + (size_t)y0 * stride, pixels + (size_t)y0 * stride, (size_t)(y1 - y0) * stride);
        } else {
            size_t row = (size_t)(x1 - x0) * 4;
            for (int y = y0; y < y1; y++)
                memcpy(target->data + (size_t)y * stride + (size_t)x0 * 4, pixels + (size_t)y * stride + (size_t)x0 * 4, row);
        }
    }
    target->dirty_all = FALSE;
    target->x0 = target->y0 = target->x1 = target->y1 = 0;

    /* 其他缓冲落后了本帧的损伤。 */
    for (guint i = 0; i < self->shm_buffers->len; i++) {
        LoongShmBuffer *other = g_ptr_array_index(self->shm_buffers, i);
        if (other != target)
            dirty_add(other, rx0, ry0, rx1, ry1);
    }
    self->current = target;
    return target;
}

/* ───────────────────────── DMA-BUF ───────────────────────── */

typedef struct {
    struct wl_buffer *wl_buffer;
    GWeakRef view;
} LoongDmabufData;

static void
dmabuf_data_free(gpointer data)
{
    LoongDmabufData *d = data;
    if (d->wl_buffer)
        wl_buffer_destroy(d->wl_buffer);
    g_weak_ref_clear(&d->view);
    g_free(d);
}

static void
dmabuf_buffer_release(void *data, struct wl_buffer *wl_buffer)
{
    WPEBuffer *buffer = data;
    LoongDmabufData *d = wpe_buffer_get_user_data(buffer);
    if (!d)
        return;
    g_autoptr(WPEView) view = g_weak_ref_get(&d->view);
    if (view)
        wpe_view_buffer_released(view, buffer);
}

static const struct wl_buffer_listener dmabuf_buffer_listener = {
    .release = dmabuf_buffer_release,
};

static struct wl_buffer *
dmabuf_wl_buffer(LoongView *self, WPEBufferDMABuf *source, GError **error)
{
    WPEBuffer *buffer = WPE_BUFFER(source);
    LoongDmabufData *d = wpe_buffer_get_user_data(buffer);
    if (d)
        return d->wl_buffer;
    LoongDisplay *display = view_display(self);
    if (!display->dmabuf) {
        g_set_error_literal(error, WPE_VIEW_ERROR, WPE_VIEW_ERROR_RENDER_FAILED, "合成器不支持 zwp_linux_dmabuf_v1");
        return NULL;
    }
    guint64 modifier = wpe_buffer_dma_buf_get_modifier(source);
    struct zwp_linux_buffer_params_v1 *params = zwp_linux_dmabuf_v1_create_params(display->dmabuf);
    guint32 planes = wpe_buffer_dma_buf_get_n_planes(source);
    for (guint32 i = 0; i < planes; i++) {
        zwp_linux_buffer_params_v1_add(params, wpe_buffer_dma_buf_get_fd(source, i), i, wpe_buffer_dma_buf_get_offset(source, i),
                                       wpe_buffer_dma_buf_get_stride(source, i), modifier >> 32, modifier & 0xffffffff);
    }
    struct wl_buffer *wl_buffer = zwp_linux_buffer_params_v1_create_immed(
        params, wpe_buffer_get_width(buffer), wpe_buffer_get_height(buffer), wpe_buffer_dma_buf_get_format(source), 0);
    zwp_linux_buffer_params_v1_destroy(params);
    if (!wl_buffer) {
        g_set_error_literal(error, WPE_VIEW_ERROR, WPE_VIEW_ERROR_RENDER_FAILED, "无法从 DMA-BUF 创建 wl_buffer");
        return NULL;
    }
    d = g_new0(LoongDmabufData, 1);
    d->wl_buffer = wl_buffer;
    g_weak_ref_init(&d->view, self);
    wl_buffer_add_listener(wl_buffer, &dmabuf_buffer_listener, buffer);
    wpe_buffer_set_user_data(buffer, d, dmabuf_data_free);
    return wl_buffer;
}

/* ───────────────────────── 出帧 ───────────────────────── */

static void
emit_rendered(LoongView *self)
{
    WPEBuffer *buffer = g_steal_pointer(&self->frame_buffer);
    if (buffer) {
        wpe_view_buffer_rendered(WPE_VIEW(self), buffer);
        g_object_unref(buffer);
    }
}

static void
frame_done(void *data, struct wl_callback *callback, uint32_t time)
{
    LoongView *self = data;
    if (self->frame_callback == callback)
        self->frame_callback = NULL;
    wl_callback_destroy(callback);
    emit_rendered(self);
}

static const struct wl_callback_listener frame_listener = {
    .done = frame_done,
};

static gboolean
hidden_frame_cb(gpointer data)
{
    LoongView *self = data;
    self->hidden_frame_id = 0;
    emit_rendered(self);
    return G_SOURCE_REMOVE;
}

/* 设置缓冲缩放：有 viewport 时用目标尺寸（支持分数缩放），否则用整数 buffer_scale。 */
static void
apply_buffer_scale(LoongView *self, int buffer_width, int buffer_height)
{
    double scale = self->toplevel ? loong_toplevel_buffer_scale(self->toplevel) : 1.0;
    if (self->viewport) {
        int w = MAX(1, (int)lround(buffer_width / scale));
        int h = MAX(1, (int)lround(buffer_height / scale));
        wp_viewport_set_destination(self->viewport, w, h);
        return;
    }
    int iscale = MAX(1, (int)lround(scale));
    if (buffer_width % iscale || buffer_height % iscale)
        iscale = 1; /* 尺寸不整除时宁可显示得大一点，也不能触发协议错误 */
    if (iscale != self->surface_scale) {
        wl_surface_set_buffer_scale(self->surface, iscale);
        self->surface_scale = iscale;
    }
}

static void
release_hidden_dmabuf(LoongView *self)
{
    WPEBuffer *buffer = g_steal_pointer(&self->hidden_dmabuf);
    if (buffer) {
        wpe_view_buffer_released(WPE_VIEW(self), buffer);
        g_object_unref(buffer);
    }
}

static gboolean
loong_view_render_buffer(WPEView *view, WPEBuffer *buffer, const WPERectangle *damage, guint n_damage, GError **error)
{
    LoongView *self = LOONG_VIEW(view);
    if (!self->subsurface) {
        g_set_error_literal(error, WPE_VIEW_ERROR, WPE_VIEW_ERROR_RENDER_FAILED, "view 不在窗口中");
        return FALSE;
    }
    int width = wpe_buffer_get_width(buffer), height = wpe_buffer_get_height(buffer);
    /* 本帧损伤的包围盒（缓冲坐标）。 */
    int x0 = 0, y0 = 0, x1 = width, y1 = height;
    if (n_damage && !self->full_damage) {
        x0 = y0 = G_MAXINT;
        x1 = y1 = G_MININT;
        for (guint i = 0; i < n_damage; i++) {
            x0 = MIN(x0, damage[i].x);
            y0 = MIN(y0, damage[i].y);
            x1 = MAX(x1, damage[i].x + damage[i].width);
            y1 = MAX(y1, damage[i].y + damage[i].height);
        }
    }
    gboolean mapped = wpe_view_get_mapped(view);

    struct wl_buffer *wl_buffer = NULL;
    if (WPE_IS_BUFFER_SHM(buffer)) {
        LoongShmBuffer *target = copy_shm_frame(self, WPE_BUFFER_SHM(buffer), x0, y0, x1, y1, error);
        if (!target)
            return FALSE;
        self->reattach_pending = FALSE;
        /* 像素已经拷走，WebKit 可以马上复用这块缓冲。 */
        wpe_view_buffer_released(view, buffer);
        if (mapped) {
            wl_buffer = target->wl_buffer;
            target->busy = TRUE;
        }
    } else if (WPE_IS_BUFFER_DMA_BUF(buffer)) {
        if (mapped) {
            wl_buffer = dmabuf_wl_buffer(self, WPE_BUFFER_DMA_BUF(buffer), error);
            if (!wl_buffer)
                return FALSE;
        } else {
            release_hidden_dmabuf(self);
            self->hidden_dmabuf = g_object_ref(buffer);
        }
    } else {
        g_set_error_literal(error, WPE_VIEW_ERROR, WPE_VIEW_ERROR_RENDER_FAILED, "不支持的缓冲类型");
        return FALSE;
    }

    g_clear_object(&self->frame_buffer);
    self->frame_buffer = g_object_ref(buffer);

    if (!wl_buffer) {
        /* 隐藏中：不提交，按定时器回应 WebKit。 */
        if (!self->hidden_frame_id)
            self->hidden_frame_id = g_timeout_add(HIDDEN_FRAME_INTERVAL_MS, hidden_frame_cb, self);
        return TRUE;
    }

    apply_buffer_scale(self, width, height);
    wl_surface_attach(self->surface, wl_buffer, 0, 0);
    if (self->full_damage || !n_damage) {
        wl_surface_damage_buffer(self->surface, 0, 0, INT32_MAX, INT32_MAX);
    } else {
        for (guint i = 0; i < n_damage; i++)
            wl_surface_damage_buffer(self->surface, damage[i].x, damage[i].y, damage[i].width, damage[i].height);
    }
    self->full_damage = FALSE;
    self->attached = TRUE;
    if (self->frame_callback)
        wl_callback_destroy(self->frame_callback);
    self->frame_callback = wl_surface_frame(self->surface);
    wl_callback_add_listener(self->frame_callback, &frame_listener, self);
    wl_surface_commit(self->surface);
    return TRUE;
}

/* ───────────────────────── 映射 / 隐藏 ───────────────────────── */

static void
on_mapped_changed(LoongView *self)
{
    if (!self->subsurface)
        return;
    if (wpe_view_get_mapped(WPE_VIEW(self))) {
        /* 重新显示：把隐藏前（或隐藏期间）的最新画面挂回去。 */
        self->full_damage = TRUE;
        LoongShmBuffer *current = self->current;
        if (current && !self->attached) {
            if (current->busy) {
                self->reattach_pending = TRUE;
            } else {
                apply_buffer_scale(self, current->width, current->height);
                wl_surface_attach(self->surface, current->wl_buffer, 0, 0);
                wl_surface_damage_buffer(self->surface, 0, 0, INT32_MAX, INT32_MAX);
                wl_surface_commit(self->surface);
                current->busy = TRUE;
                self->attached = TRUE;
            }
        }
        if (self->toplevel)
            loong_toplevel_schedule_commit(self->toplevel);
        return;
    }
    /* 隐藏：卸下缓冲（子表面随之不可见），等待中的帧立即回应。 */
    self->reattach_pending = FALSE;
    if (self->attached) {
        wl_surface_attach(self->surface, NULL, 0, 0);
        wl_surface_commit(self->surface);
        self->attached = FALSE;
    }
    g_clear_pointer(&self->frame_callback, wl_callback_destroy);
    emit_rendered(self);
    /* 只留最新画面，其余空闲缓冲释放掉，省内存。 */
    for (guint i = 0; i < self->shm_buffers->len;) {
        LoongShmBuffer *buffer = g_ptr_array_index(self->shm_buffers, i);
        if (buffer != self->current && !buffer->busy)
            g_ptr_array_remove_index(self->shm_buffers, i);
        else
            i++;
    }
    LoongDisplay *display = view_display(self);
    if (display)
        loong_seat_view_gone(display, WPE_VIEW(self));
    if (self->toplevel)
        loong_toplevel_schedule_commit(self->toplevel);
}

static void
on_toplevel_changed(LoongView *self)
{
    WPEToplevel *toplevel = wpe_view_get_toplevel(WPE_VIEW(self));
    LoongToplevel *old = self->toplevel;
    if (old && (WPEToplevel *)old != toplevel) {
        self->toplevel = NULL;
        loong_toplevel_remove_view(old, self);
    }
    if (!toplevel) {
        wpe_view_unmap(WPE_VIEW(self));
        return;
    }
    g_return_if_fail(LOONG_IS_TOPLEVEL(toplevel));
    if ((WPEToplevel *)self->toplevel == toplevel)
        return;
    self->toplevel = LOONG_TOPLEVEL(toplevel);
    loong_toplevel_add_view(self->toplevel, self);
    wpe_view_map(WPE_VIEW(self));
}

/* ───────────────────────── 内部接口（供 LoongToplevel 调用） ───────────────────────── */

void
loong_view_attach_to_parent(LoongView *self, struct wl_surface *parent)
{
    LoongDisplay *display = view_display(self);
    g_clear_pointer(&self->subsurface, wl_subsurface_destroy);
    self->subsurface = wl_subcompositor_get_subsurface(display->subcompositor, self->surface, parent);
    wl_subsurface_set_desync(self->subsurface);
    wl_subsurface_set_position(self->subsurface, self->x, self->y);
    self->surface_scale = 0;
    self->full_damage = TRUE;
}

void
loong_view_detach_from_parent(LoongView *self)
{
    if (self->attached) {
        wl_surface_attach(self->surface, NULL, 0, 0);
        wl_surface_commit(self->surface);
        self->attached = FALSE;
    }
    g_clear_pointer(&self->frame_callback, wl_callback_destroy);
    g_clear_pointer(&self->subsurface, wl_subsurface_destroy);
    self->toplevel = NULL;
    emit_rendered(self);
    LoongDisplay *display = view_display(self);
    if (display)
        loong_seat_view_gone(display, WPE_VIEW(self));
}

struct wl_surface *
loong_view_get_wl_surface(LoongView *self)
{
    return self->surface;
}

struct wl_subsurface *
loong_view_get_subsurface(LoongView *self)
{
    return self->subsurface;
}

int
loong_view_get_layer(LoongView *self)
{
    return self->layer;
}

guint
loong_view_get_order(LoongView *self)
{
    return self->order;
}

void
loong_view_set_order(LoongView *self, guint order)
{
    self->order = order;
}

gboolean
loong_view_has_geometry(LoongView *self)
{
    return self->has_geometry;
}

void
loong_view_toplevel_configured(LoongView *self)
{
    wpe_view_map(WPE_VIEW(self));
}

void
loong_view_scale_changed(LoongView *self)
{
    /* 新缩放下的缓冲由 WebKit 重画后送来；这里只需让下一帧整面提交。 */
    self->full_damage = TRUE;
}

/* ───────────────────────── WPEView 虚函数 ───────────────────────── */

static gboolean
loong_view_can_be_mapped(WPEView *view)
{
    LoongView *self = LOONG_VIEW(view);
    return self->toplevel && loong_toplevel_is_configured(self->toplevel) && self->subsurface;
}

static void
loong_view_set_opaque_rectangles(WPEView *view, WPERectangle *rects, guint n_rects)
{
    LoongView *self = LOONG_VIEW(view);
    LoongDisplay *display = view_display(self);
    struct wl_region *region = NULL;
    if (rects) {
        region = wl_compositor_create_region(display->compositor);
        for (guint i = 0; i < n_rects; i++)
            wl_region_add(region, rects[i].x, rects[i].y, rects[i].width, rects[i].height);
    }
    wl_surface_set_opaque_region(self->surface, region);
    if (region)
        wl_region_destroy(region);
}

static void
loong_view_set_cursor_from_name(WPEView *view, const char *name)
{
    /* 平板上没有光标；有鼠标时由合成器画默认光标。 */
}

/* ───────────────────────── GObject ───────────────────────── */

static void
surface_enter(void *data, struct wl_surface *surface, struct wl_output *output)
{
}

static void
surface_leave(void *data, struct wl_surface *surface, struct wl_output *output)
{
}

static void
surface_preferred_buffer_scale(void *data, struct wl_surface *surface, int32_t factor)
{
}

static void
surface_preferred_buffer_transform(void *data, struct wl_surface *surface, uint32_t transform)
{
}

static const struct wl_surface_listener surface_listener = {
    .enter = surface_enter,
    .leave = surface_leave,
    .preferred_buffer_scale = surface_preferred_buffer_scale,
    .preferred_buffer_transform = surface_preferred_buffer_transform,
};

static void
loong_view_constructed(GObject *object)
{
    G_OBJECT_CLASS(loong_view_parent_class)->constructed(object);
    LoongView *self = LOONG_VIEW(object);
    LoongDisplay *display = view_display(self);
    g_return_if_fail(display && display->compositor);
    self->surface = wl_compositor_create_surface(display->compositor);
    /* user_data 指向 view：输入事件据此找到目标。 */
    wl_surface_add_listener(self->surface, &surface_listener, self);
    if (display->viewporter && !g_getenv("LOONG_PLATFORM_NO_VIEWPORTER"))
        self->viewport = wp_viewporter_get_viewport(display->viewporter, self->surface);

    g_signal_connect(self, "notify::toplevel", G_CALLBACK(on_toplevel_changed), NULL);
    g_signal_connect(self, "notify::mapped", G_CALLBACK(on_mapped_changed), NULL);
}

static void
loong_view_dispose(GObject *object)
{
    LoongView *self = LOONG_VIEW(object);
    g_signal_handlers_disconnect_by_func(self, on_toplevel_changed, NULL);
    g_signal_handlers_disconnect_by_func(self, on_mapped_changed, NULL);
    if (self->toplevel) {
        LoongToplevel *toplevel = self->toplevel;
        self->toplevel = NULL;
        loong_toplevel_remove_view(toplevel, self);
    }
    g_clear_handle_id(&self->hidden_frame_id, g_source_remove);
    g_clear_pointer(&self->frame_callback, wl_callback_destroy);
    g_clear_object(&self->frame_buffer);
    release_hidden_dmabuf(self);
    g_clear_pointer(&self->shm_buffers, g_ptr_array_unref);
    self->current = NULL;
    g_clear_pointer(&self->viewport, wp_viewport_destroy);
    g_clear_pointer(&self->subsurface, wl_subsurface_destroy);
    if (self->surface) {
        LoongDisplay *display = view_display(self);
        if (display)
            loong_seat_surface_destroyed(display, self->surface);
        g_clear_pointer(&self->surface, wl_surface_destroy);
    }
    G_OBJECT_CLASS(loong_view_parent_class)->dispose(object);
}

static void
loong_view_init(LoongView *self)
{
    self->shm_buffers = g_ptr_array_new_with_free_func(shm_buffer_discard);
}

static void
loong_view_class_init(LoongViewClass *klass)
{
    GObjectClass *object_class = G_OBJECT_CLASS(klass);
    object_class->constructed = loong_view_constructed;
    object_class->dispose = loong_view_dispose;
    WPEViewClass *view_class = WPE_VIEW_CLASS(klass);
    view_class->render_buffer = loong_view_render_buffer;
    view_class->can_be_mapped = loong_view_can_be_mapped;
    view_class->set_opaque_rectangles = loong_view_set_opaque_rectangles;
    view_class->set_cursor_from_name = loong_view_set_cursor_from_name;
}

/* ───────────────────────── 公开接口 ───────────────────────── */

void
loong_view_set_geometry(LoongView *self, int x, int y, int width, int height)
{
    g_return_if_fail(LOONG_IS_VIEW(self));
    self->has_geometry = TRUE;
    if (x != self->x || y != self->y) {
        self->x = x;
        self->y = y;
        if (self->subsurface) {
            wl_subsurface_set_position(self->subsurface, x, y);
            loong_toplevel_schedule_commit(self->toplevel);
        }
    }
    self->geom_width = width;
    self->geom_height = height;
    wpe_view_resized(WPE_VIEW(self), MAX(width, 1), MAX(height, 1));
}

void
loong_view_get_position(LoongView *self, int *x, int *y)
{
    g_return_if_fail(LOONG_IS_VIEW(self));
    if (x)
        *x = self->x;
    if (y)
        *y = self->y;
}

void
loong_view_set_layer(LoongView *self, int layer)
{
    g_return_if_fail(LOONG_IS_VIEW(self));
    if (self->layer == layer)
        return;
    self->layer = layer;
    if (self->toplevel)
        loong_toplevel_restack(self->toplevel);
}

void
loong_view_focus(LoongView *self)
{
    g_return_if_fail(LOONG_IS_VIEW(self));
    if (self->toplevel)
        loong_toplevel_set_focused_view(self->toplevel, WPE_VIEW(self));
}
