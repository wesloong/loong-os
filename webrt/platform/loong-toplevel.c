/*
 * LoongToplevel：一个 xdg_toplevel 窗口。
 *
 * 根表面只画一层不透明的底色（有 wp_single_pixel_buffer / wp_viewporter 时不占内存）；
 * 每个 view 是它的一个子表面，按 (layer, 加入顺序) 从下到上排列。
 * 子表面的位置与层次属于父表面的状态，在根表面下一次 commit 时生效，
 * 因此布局变化后统一在空闲时提交一次根表面。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include "loong-platform-private.h"

#include <errno.h>
#include <math.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

G_DEFINE_FINAL_TYPE(LoongToplevel, loong_toplevel, WPE_TYPE_TOPLEVEL)

enum {
    SIGNAL_SIZE_CHANGED,
    N_SIGNALS
};
static guint signals[N_SIGNALS];

static LoongDisplay *
toplevel_display(LoongToplevel *self)
{
    return LOONG_DISPLAY(wpe_toplevel_get_display(WPE_TOPLEVEL(self)));
}

/* ───────────────────────── 缩放 ───────────────────────── */

/* 缓冲缩放：分数缩放 > preferred_buffer_scale > 所在输出的最大整数缩放 > 第一个输出 > 1。 */
double
loong_toplevel_buffer_scale(LoongToplevel *self)
{
    if (self->fractional_scale_value > 0)
        return self->fractional_scale_value;
    if (self->preferred_buffer_scale > 0)
        return self->preferred_buffer_scale;
    double scale = 0;
    for (guint i = 0; i < self->screens->len; i++)
        scale = MAX(scale, wpe_screen_get_scale(g_ptr_array_index(self->screens, i)));
    if (scale > 0)
        return scale;
    LoongDisplay *display = toplevel_display(self);
    if (display && display->screens->len)
        return MAX(1.0, wpe_screen_get_scale(g_ptr_array_index(display->screens, 0)));
    return 1.0;
}

static void
update_scale(LoongToplevel *self)
{
    double scale = loong_toplevel_buffer_scale(self);
    if (scale == wpe_toplevel_get_scale(WPE_TOPLEVEL(self)))
        return;
    self->bg_dirty = TRUE;
    wpe_toplevel_scale_changed(WPE_TOPLEVEL(self), scale);
    for (GList *l = self->stack; l; l = l->next)
        loong_view_scale_changed(l->data);
    loong_toplevel_schedule_commit(self);
}

static void
screen_scale_changed(LoongToplevel *self)
{
    update_scale(self);
}

static void
surface_enter(void *data, struct wl_surface *surface, struct wl_output *output)
{
    LoongToplevel *self = data;
    LoongDisplay *display = toplevel_display(self);
    LoongScreen *screen = display ? loong_display_find_screen(display, output) : NULL;
    if (!screen)
        return;
    g_ptr_array_add(self->screens, g_object_ref(screen));
    g_signal_connect_object(screen, "notify::scale", G_CALLBACK(screen_scale_changed), self, G_CONNECT_SWAPPED);
    update_scale(self);
    wpe_toplevel_screen_changed(WPE_TOPLEVEL(self));
}

static void
surface_leave(void *data, struct wl_surface *surface, struct wl_output *output)
{
    LoongToplevel *self = data;
    for (guint i = 0; i < self->screens->len; i++) {
        LoongScreen *screen = g_ptr_array_index(self->screens, i);
        if (screen->wl_output == output) {
            g_signal_handlers_disconnect_by_data(screen, self);
            g_ptr_array_remove_index(self->screens, i);
            update_scale(self);
            wpe_toplevel_screen_changed(WPE_TOPLEVEL(self));
            return;
        }
    }
}

static void
surface_preferred_buffer_scale(void *data, struct wl_surface *surface, int32_t factor)
{
    LoongToplevel *self = data;
    self->preferred_buffer_scale = factor;
    update_scale(self);
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
fractional_preferred_scale(void *data, struct wp_fractional_scale_v1 *fractional, uint32_t scale)
{
    LoongToplevel *self = data;
    self->fractional_scale_value = scale / 120.0;
    update_scale(self);
}

static const struct wp_fractional_scale_v1_listener fractional_listener = {
    .preferred_scale = fractional_preferred_scale,
};

/* ───────────────────────── 底色缓冲 ───────────────────────── */

static void
shm_buffer_release(void *data, struct wl_buffer *buffer)
{
    /* 底色缓冲在替换时直接销毁（见 ensure_background）；这里什么都不用做。 */
}

static const struct wl_buffer_listener bg_buffer_listener = {
    .release = shm_buffer_release,
};

static struct wl_buffer *
create_solid_shm_buffer(LoongDisplay *display, int width, int height, guint32 rgb)
{
    size_t stride = (size_t)width * 4;
    size_t size = stride * height;
    int fd = memfd_create("loong-background", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    if (fd < 0)
        return NULL;
    if (ftruncate(fd, size) < 0) {
        close(fd);
        return NULL;
    }
    guint32 *pixels = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (pixels == MAP_FAILED) {
        close(fd);
        return NULL;
    }
    guint32 argb = 0xff000000u | (rgb & 0xffffff);
    for (size_t i = 0; i < size / 4; i++)
        pixels[i] = argb;
    munmap(pixels, size);
    struct wl_shm_pool *pool = wl_shm_create_pool(loong_display_get_shm(display), fd, size);
    struct wl_buffer *buffer = wl_shm_pool_create_buffer(pool, 0, width, height, stride, WL_SHM_FORMAT_XRGB8888);
    wl_shm_pool_destroy(pool);
    close(fd);
    wl_buffer_add_listener(buffer, &bg_buffer_listener, NULL);
    return buffer;
}

/* 换下来的缓冲在下一次提交之后才销毁（合成器可能还在用它）。 */
static void
retire_background(LoongToplevel *self)
{
    if (!self->bg_buffer)
        return;
    g_clear_pointer(&self->bg_retired, wl_buffer_destroy);
    self->bg_retired = g_steal_pointer(&self->bg_buffer);
}

static void
ensure_background(LoongToplevel *self)
{
    if (!self->bg_dirty && self->bg_buffer)
        return;
    LoongDisplay *display = toplevel_display(self);
    int width, height;
    wpe_toplevel_get_size(WPE_TOPLEVEL(self), &width, &height);
    if (width <= 0 || height <= 0)
        return;
    double scale = wpe_toplevel_get_scale(WPE_TOPLEVEL(self));
    self->bg_dirty = FALSE;
    if (self->bg_color_dirty) {
        retire_background(self);
        self->bg_color_dirty = FALSE;
    }

    if (self->viewport) {
        /* 1×1 的纯色缓冲，由 viewport 拉伸到整个窗口。 */
        if (!self->bg_buffer || self->bg_width != 1) {
            retire_background(self);
            if (display->single_pixel) {
                guint32 r = (self->background >> 16) & 0xff, g = (self->background >> 8) & 0xff, b = self->background & 0xff;
                self->bg_buffer = wp_single_pixel_buffer_manager_v1_create_u32_rgba_buffer(
                    display->single_pixel, r * (UINT32_MAX / 255), g * (UINT32_MAX / 255), b * (UINT32_MAX / 255), UINT32_MAX);
            } else {
                self->bg_buffer = create_solid_shm_buffer(display, 1, 1, self->background);
            }
            self->bg_width = self->bg_height = 1;
        }
        wl_surface_set_buffer_scale(self->surface, 1);
        wp_viewport_set_destination(self->viewport, width, height);
    } else {
        /* 没有 viewporter：整窗大小的缓冲（整数缩放）。 */
        int iscale = MAX(1, (int)ceil(scale));
        int bw = width * iscale, bh = height * iscale;
        if (!self->bg_buffer || self->bg_width != bw || self->bg_height != bh) {
            retire_background(self);
            self->bg_buffer = create_solid_shm_buffer(display, bw, bh, self->background);
            self->bg_width = bw;
            self->bg_height = bh;
        }
        wl_surface_set_buffer_scale(self->surface, iscale);
    }
    if (!self->bg_buffer)
        return;
    wl_surface_attach(self->surface, self->bg_buffer, 0, 0);
    wl_surface_damage_buffer(self->surface, 0, 0, INT32_MAX, INT32_MAX);

    /* 不透明区域：整个窗口（合成器可以跳过下层的绘制）。 */
    struct wl_region *region = wl_compositor_create_region(display->compositor);
    wl_region_add(region, 0, 0, width, height);
    wl_surface_set_opaque_region(self->surface, region);
    wl_region_destroy(region);
}

static gboolean
commit_idle(gpointer data)
{
    LoongToplevel *self = data;
    self->commit_idle = 0;
    if (!self->surface)
        return G_SOURCE_REMOVE;
    if (self->configured)
        ensure_background(self);
    wl_surface_commit(self->surface);
    g_clear_pointer(&self->bg_retired, wl_buffer_destroy);
    return G_SOURCE_REMOVE;
}

void
loong_toplevel_schedule_commit(LoongToplevel *self)
{
    if (!self->commit_idle)
        self->commit_idle = g_idle_add_full(G_PRIORITY_HIGH_IDLE, commit_idle, self, NULL);
}

/* ───────────────────────── xdg_toplevel ───────────────────────── */

static void
apply_size(LoongToplevel *self, int width, int height)
{
    int old_width, old_height;
    wpe_toplevel_get_size(WPE_TOPLEVEL(self), &old_width, &old_height);
    if (width == old_width && height == old_height)
        return;
    wpe_toplevel_resized(WPE_TOPLEVEL(self), width, height);
    self->bg_dirty = TRUE;
    /* 没有自定几何的 view 铺满窗口。 */
    for (GList *l = self->stack; l; l = l->next) {
        if (!loong_view_has_geometry(l->data))
            wpe_view_resized(WPE_VIEW(l->data), width, height);
    }
    g_signal_emit(self, signals[SIGNAL_SIZE_CHANGED], 0, width, height);
}

static void
xdg_surface_configure(void *data, struct xdg_surface *xdg_surface, uint32_t serial)
{
    LoongToplevel *self = data;
    int width = self->pending.width, height = self->pending.height;
    if (width <= 0 || height <= 0)
        wpe_toplevel_get_size(WPE_TOPLEVEL(self), &width, &height);
    apply_size(self, width, height);
    wpe_toplevel_state_changed(WPE_TOPLEVEL(self), self->pending.state);
    self->pending.width = self->pending.height = 0;
    self->pending.state = WPE_TOPLEVEL_STATE_NONE;
    xdg_surface_ack_configure(xdg_surface, serial);

    gboolean first = !self->configured;
    self->configured = TRUE;
    if (first) {
        for (GList *l = self->stack; l; l = l->next)
            loong_view_toplevel_configured(l->data);
    }
    /* ack 之后必须提交一次根表面；同步提交，保证尺寸与 ack 一致。 */
    g_clear_handle_id(&self->commit_idle, g_source_remove);
    commit_idle(self);
}

static const struct xdg_surface_listener xdg_surface_listener = {
    .configure = xdg_surface_configure,
};

static void
xdg_toplevel_configure(void *data, struct xdg_toplevel *xdg_toplevel, int32_t width, int32_t height,
                       struct wl_array *states)
{
    LoongToplevel *self = data;
    self->pending.width = width;
    self->pending.height = height;
    guint32 state = WPE_TOPLEVEL_STATE_NONE;
    guint32 *s;
    wl_array_for_each(s, states) {
        switch (*s) {
        case XDG_TOPLEVEL_STATE_FULLSCREEN:
            state |= WPE_TOPLEVEL_STATE_FULLSCREEN;
            break;
        case XDG_TOPLEVEL_STATE_MAXIMIZED:
            state |= WPE_TOPLEVEL_STATE_MAXIMIZED;
            break;
        case XDG_TOPLEVEL_STATE_ACTIVATED:
            state |= WPE_TOPLEVEL_STATE_ACTIVE;
            break;
        default:
            break;
        }
    }
    self->pending.state = state;
}

static void
xdg_toplevel_close(void *data, struct xdg_toplevel *xdg_toplevel)
{
    wpe_toplevel_closed(WPE_TOPLEVEL(data));
}

static void
xdg_toplevel_configure_bounds(void *data, struct xdg_toplevel *xdg_toplevel, int32_t width, int32_t height)
{
}

static void
xdg_toplevel_wm_capabilities(void *data, struct xdg_toplevel *xdg_toplevel, struct wl_array *capabilities)
{
}

static const struct xdg_toplevel_listener xdg_toplevel_listener = {
    .configure = xdg_toplevel_configure,
    .close = xdg_toplevel_close,
    .configure_bounds = xdg_toplevel_configure_bounds,
    .wm_capabilities = xdg_toplevel_wm_capabilities,
};

/* ───────────────────────── view 的层次、命中与焦点 ───────────────────────── */

static gint
compare_views(gconstpointer a, gconstpointer b)
{
    LoongView *va = (LoongView *)a, *vb = (LoongView *)b;
    int la = loong_view_get_layer(va), lb = loong_view_get_layer(vb);
    if (la != lb)
        return la < lb ? -1 : 1;
    guint oa = loong_view_get_order(va), ob = loong_view_get_order(vb);
    return oa < ob ? -1 : (oa > ob ? 1 : 0);
}

void
loong_toplevel_restack(LoongToplevel *self)
{
    self->stack = g_list_sort(self->stack, compare_views);
    struct wl_surface *below = self->surface;
    for (GList *l = self->stack; l; l = l->next) {
        struct wl_subsurface *subsurface = loong_view_get_subsurface(l->data);
        if (!subsurface)
            continue;
        wl_subsurface_place_above(subsurface, below);
        below = loong_view_get_wl_surface(l->data);
    }
    loong_toplevel_schedule_commit(self);
}

void
loong_toplevel_add_view(LoongToplevel *self, LoongView *view)
{
    if (g_list_find(self->stack, view))
        return;
    loong_view_set_order(view, self->next_order++);
    self->stack = g_list_append(self->stack, view);
    loong_view_attach_to_parent(view, self->surface);
    if (!loong_view_has_geometry(view)) {
        int width, height;
        wpe_toplevel_get_size(WPE_TOPLEVEL(self), &width, &height);
        wpe_view_resized(WPE_VIEW(view), width, height);
    }
    loong_toplevel_restack(self);
}

void
loong_toplevel_remove_view(LoongToplevel *self, LoongView *view)
{
    if (!g_list_find(self->stack, view))
        return;
    if (self->focused_view == WPE_VIEW(view))
        loong_toplevel_set_focused_view(self, NULL);
    self->stack = g_list_remove(self->stack, view);
    loong_view_detach_from_parent(view);
    loong_toplevel_schedule_commit(self);
}

WPEView *
loong_toplevel_view_at(LoongToplevel *self, double x, double y)
{
    for (GList *l = g_list_last(self->stack); l; l = l->prev) {
        WPEView *view = l->data;
        if (!wpe_view_get_mapped(view))
            continue;
        int vx, vy;
        loong_view_get_position(LOONG_VIEW(view), &vx, &vy);
        if (x >= vx && y >= vy && x < vx + wpe_view_get_width(view) && y < vy + wpe_view_get_height(view))
            return view;
    }
    return NULL;
}

WPEView *
loong_toplevel_get_focused_view(LoongToplevel *self)
{
    if (self->focused_view)
        return self->focused_view;
    /* 没有指定时：最上层的已映射 view。 */
    for (GList *l = g_list_last(self->stack); l; l = l->prev) {
        if (wpe_view_get_mapped(l->data))
            return l->data;
    }
    return NULL;
}

void
loong_toplevel_set_focused_view(LoongToplevel *self, WPEView *view)
{
    WPEView *old = loong_toplevel_get_focused_view(self);
    g_set_weak_pointer(&self->focused_view, view);
    WPEView *now = loong_toplevel_get_focused_view(self);
    if (!self->keyboard_inside || old == now)
        return;
    if (old)
        wpe_view_focus_out(old);
    if (now)
        wpe_view_focus_in(now);
}

void
loong_toplevel_keyboard_enter(LoongToplevel *self)
{
    self->keyboard_inside = TRUE;
    WPEView *view = loong_toplevel_get_focused_view(self);
    if (view)
        wpe_view_focus_in(view);
}

void
loong_toplevel_keyboard_leave(LoongToplevel *self)
{
    WPEView *view = loong_toplevel_get_focused_view(self);
    self->keyboard_inside = FALSE;
    if (view)
        wpe_view_focus_out(view);
}

/* ───────────────────────── WPEToplevel 虚函数 ───────────────────────── */

static void
loong_toplevel_set_title(WPEToplevel *toplevel, const char *title)
{
    LoongToplevel *self = LOONG_TOPLEVEL(toplevel);
    if (!self->xdg_toplevel)
        return;
    if (!title || !g_utf8_validate(title, -1, NULL)) {
        xdg_toplevel_set_title(self->xdg_toplevel, "");
        return;
    }
    /* Wayland 消息上限约 4KB；标题截短到 1000 个字符以内。 */
    g_autofree char *short_title = g_utf8_substring(title, 0, MIN(g_utf8_strlen(title, -1), 1000));
    xdg_toplevel_set_title(self->xdg_toplevel, short_title);
}

static WPEScreen *
loong_toplevel_get_screen(WPEToplevel *toplevel)
{
    LoongToplevel *self = LOONG_TOPLEVEL(toplevel);
    if (self->screens->len)
        return g_ptr_array_index(self->screens, self->screens->len - 1);
    LoongDisplay *display = toplevel_display(self);
    return display && display->screens->len ? g_ptr_array_index(display->screens, 0) : NULL;
}

static gboolean
loong_toplevel_resize(WPEToplevel *toplevel, int width, int height)
{
    LoongToplevel *self = LOONG_TOPLEVEL(toplevel);
    if (wpe_toplevel_get_state(toplevel) & (WPE_TOPLEVEL_STATE_FULLSCREEN | WPE_TOPLEVEL_STATE_MAXIMIZED))
        return FALSE;
    apply_size(self, width, height);
    loong_toplevel_schedule_commit(self);
    return TRUE;
}

static gboolean
loong_toplevel_set_fullscreen(WPEToplevel *toplevel, gboolean fullscreen)
{
    LoongToplevel *self = LOONG_TOPLEVEL(toplevel);
    if (!self->xdg_toplevel)
        return FALSE;
    if (fullscreen)
        xdg_toplevel_set_fullscreen(self->xdg_toplevel, NULL);
    else
        xdg_toplevel_unset_fullscreen(self->xdg_toplevel);
    return TRUE;
}

static gboolean
loong_toplevel_set_maximized(WPEToplevel *toplevel, gboolean maximized)
{
    LoongToplevel *self = LOONG_TOPLEVEL(toplevel);
    if (!self->xdg_toplevel)
        return FALSE;
    if (maximized)
        xdg_toplevel_set_maximized(self->xdg_toplevel);
    else
        xdg_toplevel_unset_maximized(self->xdg_toplevel);
    return TRUE;
}

static gboolean
loong_toplevel_set_minimized(WPEToplevel *toplevel)
{
    LoongToplevel *self = LOONG_TOPLEVEL(toplevel);
    if (!self->xdg_toplevel)
        return FALSE;
    xdg_toplevel_set_minimized(self->xdg_toplevel);
    return TRUE;
}

/* ───────────────────────── GObject ───────────────────────── */

static void
loong_toplevel_constructed(GObject *object)
{
    G_OBJECT_CLASS(loong_toplevel_parent_class)->constructed(object);
    LoongToplevel *self = LOONG_TOPLEVEL(object);
    LoongDisplay *display = toplevel_display(self);
    g_return_if_fail(display && display->compositor);

    self->surface = wl_compositor_create_surface(display->compositor);
    wl_surface_add_listener(self->surface, &surface_listener, self);
    if (display->viewporter)
        self->viewport = wp_viewporter_get_viewport(display->viewporter, self->surface);
    if (display->fractional_scale_manager && display->viewporter) {
        self->fractional_scale = wp_fractional_scale_manager_v1_get_fractional_scale(display->fractional_scale_manager, self->surface);
        wp_fractional_scale_v1_add_listener(self->fractional_scale, &fractional_listener, self);
    }
    self->xdg_surface = xdg_wm_base_get_xdg_surface(display->wm_base, self->surface);
    xdg_surface_add_listener(self->xdg_surface, &xdg_surface_listener, self);
    self->xdg_toplevel = xdg_surface_get_toplevel(self->xdg_surface);
    xdg_toplevel_add_listener(self->xdg_toplevel, &xdg_toplevel_listener, self);
    const char *title = g_get_application_name() ? g_get_application_name() : g_get_prgname();
    xdg_toplevel_set_title(self->xdg_toplevel, title ? title : "");
    self->app_id = g_strdup(g_get_prgname());
    if (self->app_id)
        xdg_toplevel_set_app_id(self->xdg_toplevel, self->app_id);

    /* 初始缩放：第一个输出（收到 enter 之前）。 */
    double scale = loong_toplevel_buffer_scale(self);
    if (scale != 1.0)
        wpe_toplevel_scale_changed(WPE_TOPLEVEL(self), scale);

    /* 第一次（不带缓冲的）提交放到空闲时：调用者还可以先设置 app_id、全屏等状态。 */
    loong_toplevel_schedule_commit(self);
}

static void
loong_toplevel_dispose(GObject *object)
{
    LoongToplevel *self = LOONG_TOPLEVEL(object);
    g_clear_handle_id(&self->commit_idle, g_source_remove);
    while (self->stack)
        loong_toplevel_remove_view(self, self->stack->data);
    g_clear_weak_pointer(&self->focused_view);
    if (self->screens) {
        for (guint i = 0; i < self->screens->len; i++)
            g_signal_handlers_disconnect_by_data(g_ptr_array_index(self->screens, i), self);
        g_clear_pointer(&self->screens, g_ptr_array_unref);
    }
    g_clear_pointer(&self->fractional_scale, wp_fractional_scale_v1_destroy);
    g_clear_pointer(&self->viewport, wp_viewport_destroy);
    g_clear_pointer(&self->xdg_toplevel, xdg_toplevel_destroy);
    g_clear_pointer(&self->xdg_surface, xdg_surface_destroy);
    g_clear_pointer(&self->bg_buffer, wl_buffer_destroy);
    g_clear_pointer(&self->bg_retired, wl_buffer_destroy);
    g_clear_pointer(&self->surface, wl_surface_destroy);
    g_clear_pointer(&self->app_id, g_free);
    G_OBJECT_CLASS(loong_toplevel_parent_class)->dispose(object);
}

static void
loong_toplevel_init(LoongToplevel *self)
{
    self->background = 0xf3eee2; /* 宣纸 */
    self->screens = g_ptr_array_new_with_free_func(g_object_unref);
    self->bg_dirty = TRUE;
}

static void
loong_toplevel_class_init(LoongToplevelClass *klass)
{
    GObjectClass *object_class = G_OBJECT_CLASS(klass);
    object_class->constructed = loong_toplevel_constructed;
    object_class->dispose = loong_toplevel_dispose;
    WPEToplevelClass *toplevel_class = WPE_TOPLEVEL_CLASS(klass);
    toplevel_class->set_title = loong_toplevel_set_title;
    toplevel_class->get_screen = loong_toplevel_get_screen;
    toplevel_class->resize = loong_toplevel_resize;
    toplevel_class->set_fullscreen = loong_toplevel_set_fullscreen;
    toplevel_class->set_maximized = loong_toplevel_set_maximized;
    toplevel_class->set_minimized = loong_toplevel_set_minimized;

    /**
     * LoongToplevel::size-changed:
     * 窗口尺寸变了（合成器 configure 或 wpe_toplevel_resize）。应用在这里重新布局 view；
     * 回调返回后根表面才提交，布局与新尺寸同时生效。
     */
    signals[SIGNAL_SIZE_CHANGED] = g_signal_new("size-changed", G_TYPE_FROM_CLASS(klass), G_SIGNAL_RUN_LAST, 0, NULL,
                                                NULL, NULL, G_TYPE_NONE, 2, G_TYPE_INT, G_TYPE_INT);
}

/* ───────────────────────── 公开接口 ───────────────────────── */

void
loong_toplevel_set_app_id(LoongToplevel *self, const char *app_id)
{
    g_return_if_fail(LOONG_IS_TOPLEVEL(self));
    g_free(self->app_id);
    self->app_id = g_strdup(app_id);
    if (self->xdg_toplevel && app_id)
        xdg_toplevel_set_app_id(self->xdg_toplevel, app_id);
}

void
loong_toplevel_set_background(LoongToplevel *self, guint32 rgb)
{
    g_return_if_fail(LOONG_IS_TOPLEVEL(self));
    rgb &= 0xffffff;
    if (self->background == rgb)
        return;
    self->background = rgb;
    self->bg_color_dirty = TRUE;
    self->bg_dirty = TRUE;
    loong_toplevel_schedule_commit(self);
}

gboolean
loong_toplevel_is_configured(LoongToplevel *self)
{
    g_return_val_if_fail(LOONG_IS_TOPLEVEL(self), FALSE);
    return self->configured;
}
