/*
 * LoongDisplay：连接 Wayland 合成器，绑定全局对象，把 wl_display 的事件接入 GLib 主循环。
 *
 * 必需：wl_compositor、wl_subcompositor、xdg_wm_base、wl_shm。
 * 可选：wp_viewporter + wp_fractional_scale_manager_v1（分数缩放）、
 *       wp_single_pixel_buffer_manager_v1（根表面的纯色底不占内存）、
 *       zwp_linux_dmabuf_v1（DMA-BUF 零拷贝；没有时 WebKit 走共享内存）、
 *       zwp_text_input_manager_v3（输入法）。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include "loong-platform-private.h"

#include <errno.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#ifdef HAVE_LIBDRM
#include <xf86drm.h>
#endif

G_DEFINE_FINAL_TYPE(LoongDisplay, loong_display, WPE_TYPE_DISPLAY)

/* ───────────────────────── wl_display → GLib 主循环 ───────────────────────── */

typedef struct {
    GSource source;
    GPollFD pfd;
    LoongDisplay *display;
} LoongEventSource;

static void
report_disconnected(LoongDisplay *self, const char *message)
{
    g_autoptr(GError) error =
        g_error_new(WPE_DISPLAY_ERROR, WPE_DISPLAY_ERROR_CONNECTION_LOST, "%s: %s", message, g_strerror(errno));
    wpe_display_disconnected(WPE_DISPLAY(self), error);
}

static gboolean
source_prepare(GSource *base, gint *timeout)
{
    LoongEventSource *source = (LoongEventSource *)base;
    struct wl_display *display = source->display->wl_display;
    *timeout = -1;
    while (wl_display_prepare_read(display) != 0) {
        if (wl_display_dispatch_pending(display) < 0)
            return FALSE;
    }
    wl_display_flush(display);
    return FALSE;
}

static gboolean
source_check(GSource *base)
{
    LoongEventSource *source = (LoongEventSource *)base;
    struct wl_display *display = source->display->wl_display;
    if (source->pfd.revents & (G_IO_ERR | G_IO_HUP)) {
        wl_display_cancel_read(display);
        return TRUE;
    }
    if (source->pfd.revents & G_IO_IN) {
        if (wl_display_read_events(display) < 0) {
            source->pfd.revents |= G_IO_ERR;
            return TRUE;
        }
    } else {
        wl_display_cancel_read(display);
    }
    return !!source->pfd.revents;
}

static gboolean
source_dispatch(GSource *base, GSourceFunc callback, gpointer user_data)
{
    LoongEventSource *source = (LoongEventSource *)base;
    LoongDisplay *self = source->display;
    if (source->pfd.revents & (G_IO_ERR | G_IO_HUP)) {
        report_disconnected(self, "与 Wayland 合成器的连接已断开");
        return G_SOURCE_REMOVE;
    }
    if (wl_display_dispatch_pending(self->wl_display) < 0) {
        report_disconnected(self, "分发 Wayland 事件失败");
        return G_SOURCE_REMOVE;
    }
    source->pfd.revents = 0;
    return G_SOURCE_CONTINUE;
}

static GSourceFuncs source_funcs = {
    .prepare = source_prepare,
    .check = source_check,
    .dispatch = source_dispatch,
};

/* ───────────────────────── linux-dmabuf ───────────────────────── */

static void
dmabuf_format(void *data, struct zwp_linux_dmabuf_v1 *dmabuf, uint32_t format)
{
}

static void
dmabuf_modifier(void *data, struct zwp_linux_dmabuf_v1 *dmabuf, uint32_t format, uint32_t hi, uint32_t lo)
{
    LoongDisplay *self = data;
    LoongDmabufFormat f = { format, ((guint64)hi << 32) | lo };
    g_array_append_val(self->dmabuf_formats, f);
}

static const struct zwp_linux_dmabuf_v1_listener dmabuf_listener = {
    .format = dmabuf_format,
    .modifier = dmabuf_modifier,
};

static void
feedback_done(void *data, struct zwp_linux_dmabuf_feedback_v1 *feedback)
{
}

static void
feedback_format_table(void *data, struct zwp_linux_dmabuf_feedback_v1 *feedback, int32_t fd, uint32_t size)
{
    /* 格式表只在 v4 且不发 modifier 事件时需要；v4 的 modifier 事件已废弃，这里直接读表。 */
    LoongDisplay *self = data;
    void *table = mmap(NULL, size, PROT_READ, MAP_PRIVATE, fd, 0);
    close(fd);
    if (table == MAP_FAILED)
        return;
    const struct {
        guint32 format;
        guint32 padding;
        guint64 modifier;
    } *entries = table;
    g_array_set_size(self->dmabuf_formats, 0);
    for (guint i = 0; i < size / 16; i++) {
        LoongDmabufFormat f = { entries[i].format, entries[i].modifier };
        g_array_append_val(self->dmabuf_formats, f);
    }
    munmap(table, size);
}

static void
feedback_main_device(void *data, struct zwp_linux_dmabuf_feedback_v1 *feedback, struct wl_array *device)
{
#ifdef HAVE_LIBDRM
    LoongDisplay *self = data;
    dev_t id;
    if (device->size != sizeof(id))
        return;
    memcpy(&id, device->data, sizeof(id));
    drmDevicePtr drm;
    if (drmGetDeviceFromDevId(id, 0, &drm) != 0)
        return;
    if (drm->available_nodes & (1 << DRM_NODE_PRIMARY)) {
        g_clear_pointer(&self->drm_device, wpe_drm_device_unref);
        self->drm_device = wpe_drm_device_new(drm->nodes[DRM_NODE_PRIMARY],
                                              (drm->available_nodes & (1 << DRM_NODE_RENDER)) ? drm->nodes[DRM_NODE_RENDER] : NULL);
    }
    drmFreeDevice(&drm);
#endif
}

static void
feedback_tranche_done(void *data, struct zwp_linux_dmabuf_feedback_v1 *feedback)
{
}

static void
feedback_tranche_target_device(void *data, struct zwp_linux_dmabuf_feedback_v1 *feedback, struct wl_array *device)
{
}

static void
feedback_tranche_formats(void *data, struct zwp_linux_dmabuf_feedback_v1 *feedback, struct wl_array *indices)
{
}

static void
feedback_tranche_flags(void *data, struct zwp_linux_dmabuf_feedback_v1 *feedback, uint32_t flags)
{
}

static const struct zwp_linux_dmabuf_feedback_v1_listener feedback_listener = {
    .done = feedback_done,
    .format_table = feedback_format_table,
    .main_device = feedback_main_device,
    .tranche_done = feedback_tranche_done,
    .tranche_target_device = feedback_tranche_target_device,
    .tranche_formats = feedback_tranche_formats,
    .tranche_flags = feedback_tranche_flags,
};

/* ───────────────────────── 注册表 ───────────────────────── */

static void
wm_base_ping(void *data, struct xdg_wm_base *wm_base, uint32_t serial)
{
    xdg_wm_base_pong(wm_base, serial);
}

static const struct xdg_wm_base_listener wm_base_listener = {
    .ping = wm_base_ping,
};

static void
registry_global(void *data, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version)
{
    LoongDisplay *self = data;
    if (!strcmp(interface, wl_compositor_interface.name)) {
        self->compositor = wl_registry_bind(registry, name, &wl_compositor_interface, MIN(version, 6));
    } else if (!strcmp(interface, wl_subcompositor_interface.name)) {
        self->subcompositor = wl_registry_bind(registry, name, &wl_subcompositor_interface, 1);
    } else if (!strcmp(interface, xdg_wm_base_interface.name)) {
        self->wm_base = wl_registry_bind(registry, name, &xdg_wm_base_interface, MIN(version, 2));
        xdg_wm_base_add_listener(self->wm_base, &wm_base_listener, self);
    } else if (!strcmp(interface, wl_shm_interface.name)) {
        self->shm = wl_registry_bind(registry, name, &wl_shm_interface, 1);
    } else if (!strcmp(interface, wl_seat_interface.name)) {
        if (!self->seat.wl_seat)
            loong_seat_init(self, wl_registry_bind(registry, name, &wl_seat_interface, MIN(version, 7)));
    } else if (!strcmp(interface, wl_output_interface.name)) {
        struct wl_output *output = wl_registry_bind(registry, name, &wl_output_interface, MIN(version, 4));
        LoongScreen *screen = loong_screen_new(name, output);
        g_ptr_array_add(self->screens, screen);
        wpe_display_screen_added(WPE_DISPLAY(self), WPE_SCREEN(screen));
    } else if (!strcmp(interface, wp_viewporter_interface.name)) {
        self->viewporter = wl_registry_bind(registry, name, &wp_viewporter_interface, 1);
    } else if (!strcmp(interface, wp_fractional_scale_manager_v1_interface.name)) {
        self->fractional_scale_manager = wl_registry_bind(registry, name, &wp_fractional_scale_manager_v1_interface, 1);
    } else if (!strcmp(interface, wp_single_pixel_buffer_manager_v1_interface.name)) {
        self->single_pixel = wl_registry_bind(registry, name, &wp_single_pixel_buffer_manager_v1_interface, 1);
    } else if (!strcmp(interface, zwp_linux_dmabuf_v1_interface.name)) {
        if (version < 3)
            return;
        self->dmabuf = wl_registry_bind(registry, name, &zwp_linux_dmabuf_v1_interface, MIN(version, 4));
        if (version >= 4) {
            self->dmabuf_feedback = zwp_linux_dmabuf_v1_get_default_feedback(self->dmabuf);
            zwp_linux_dmabuf_feedback_v1_add_listener(self->dmabuf_feedback, &feedback_listener, self);
        } else {
            zwp_linux_dmabuf_v1_add_listener(self->dmabuf, &dmabuf_listener, self);
        }
    } else if (!strcmp(interface, zwp_text_input_manager_v3_interface.name)) {
        self->text_input_manager = wl_registry_bind(registry, name, &zwp_text_input_manager_v3_interface, 1);
    }
}

static void
registry_global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
    LoongDisplay *self = data;
    for (guint i = 0; i < self->screens->len; i++) {
        LoongScreen *screen = g_ptr_array_index(self->screens, i);
        if (screen->name == name) {
            g_object_ref(screen);
            g_ptr_array_remove_index(self->screens, i);
            wpe_display_screen_removed(WPE_DISPLAY(self), WPE_SCREEN(screen));
            g_object_unref(screen);
            return;
        }
    }
}

static const struct wl_registry_listener registry_listener = {
    .global = registry_global,
    .global_remove = registry_global_remove,
};

/* ───────────────────────── WPEDisplay 虚函数 ───────────────────────── */

static gboolean
loong_display_connect(WPEDisplay *display, GError **error)
{
    LoongDisplay *self = LOONG_DISPLAY(display);
    if (self->wl_display) {
        g_set_error_literal(error, WPE_DISPLAY_ERROR, WPE_DISPLAY_ERROR_CONNECTION_FAILED, "已经连接到 Wayland 合成器");
        return FALSE;
    }
    self->wl_display = wl_display_connect(NULL);
    if (!self->wl_display) {
        g_set_error(error, WPE_DISPLAY_ERROR, WPE_DISPLAY_ERROR_CONNECTION_FAILED, "无法连接 Wayland 合成器（WAYLAND_DISPLAY=%s）",
                    g_getenv("WAYLAND_DISPLAY") ? g_getenv("WAYLAND_DISPLAY") : "wayland-0");
        return FALSE;
    }
    self->registry = wl_display_get_registry(self->wl_display);
    wl_registry_add_listener(self->registry, &registry_listener, self);
    /* 第一次往返拿到全局对象；第二次拿到 seat 能力、输出参数、dmabuf 格式。 */
    if (wl_display_roundtrip(self->wl_display) < 0 || wl_display_roundtrip(self->wl_display) < 0) {
        g_set_error_literal(error, WPE_DISPLAY_ERROR, WPE_DISPLAY_ERROR_CONNECTION_FAILED, "与 Wayland 合成器往返失败");
        return FALSE;
    }
    const char *missing = !self->compositor ? "wl_compositor"
                          : !self->subcompositor ? "wl_subcompositor"
                          : !self->wm_base ? "xdg_wm_base"
                          : !self->shm ? "wl_shm"
                                       : NULL;
    if (missing) {
        g_set_error(error, WPE_DISPLAY_ERROR, WPE_DISPLAY_ERROR_NOT_SUPPORTED, "合成器缺少 %s", missing);
        return FALSE;
    }
    if (self->text_input_manager && self->seat.wl_seat)
        self->text_input = zwp_text_input_manager_v3_get_text_input(self->text_input_manager, self->seat.wl_seat);

    GSource *source = g_source_new(&source_funcs, sizeof(LoongEventSource));
    LoongEventSource *event_source = (LoongEventSource *)source;
    event_source->display = self;
    event_source->pfd.fd = wl_display_get_fd(self->wl_display);
    event_source->pfd.events = G_IO_IN | G_IO_ERR | G_IO_HUP;
    g_source_add_poll(source, &event_source->pfd);
    g_source_set_name(source, "loong-platform Wayland");
    g_source_set_priority(source, G_PRIORITY_DEFAULT);
    g_source_set_can_recurse(source, TRUE);
    g_source_attach(source, g_main_context_get_thread_default());
    self->source = source;

    g_debug("loong 平台已连接：viewporter=%d fractional-scale=%d single-pixel=%d linux-dmabuf=%d text-input-v3=%d",
            !!self->viewporter, !!self->fractional_scale_manager, !!self->single_pixel, !!self->dmabuf,
            !!self->text_input_manager);
    return TRUE;
}

static WPEView *
loong_display_create_view(WPEDisplay *display)
{
    return g_object_new(LOONG_TYPE_VIEW, "display", display, NULL);
}

static WPEToplevel *
loong_display_create_toplevel(WPEDisplay *display, guint max_views)
{
    return g_object_new(LOONG_TYPE_TOPLEVEL, "display", display, "max-views", max_views, NULL);
}

static WPEInputMethodContext *
loong_display_create_input_method_context(WPEDisplay *display, WPEView *view)
{
    return loong_im_context_new(LOONG_DISPLAY(display), view);
}

static WPEKeymap *
loong_display_get_keymap(WPEDisplay *display)
{
    /* transfer none：WPEDisplay 自己持有引用。 */
    return LOONG_DISPLAY(display)->seat.keymap;
}

static WPEBufferFormats *
loong_display_get_preferred_buffer_formats(WPEDisplay *display)
{
    LoongDisplay *self = LOONG_DISPLAY(display);
    if (!self->dmabuf || !self->dmabuf_formats->len)
        return NULL;
    WPEBufferFormatsBuilder *builder = wpe_buffer_formats_builder_new(self->drm_device);
    wpe_buffer_formats_builder_append_group(builder, NULL, WPE_BUFFER_FORMAT_USAGE_RENDERING);
    for (guint i = 0; i < self->dmabuf_formats->len; i++) {
        LoongDmabufFormat *f = &g_array_index(self->dmabuf_formats, LoongDmabufFormat, i);
        wpe_buffer_formats_builder_append_format(builder, f->fourcc, f->modifier);
    }
    return wpe_buffer_formats_builder_end(builder);
}

static guint
loong_display_get_n_screens(WPEDisplay *display)
{
    return LOONG_DISPLAY(display)->screens->len;
}

static WPEScreen *
loong_display_get_screen(WPEDisplay *display, guint index)
{
    LoongDisplay *self = LOONG_DISPLAY(display);
    return index < self->screens->len ? g_ptr_array_index(self->screens, index) : NULL;
}

static WPEDRMDevice *
loong_display_get_drm_device(WPEDisplay *display)
{
    return LOONG_DISPLAY(display)->drm_device;
}

/* ───────────────────────── GObject ───────────────────────── */

static void
loong_display_dispose(GObject *object)
{
    LoongDisplay *self = LOONG_DISPLAY(object);
    if (self->source) {
        g_source_destroy(self->source);
        g_clear_pointer(&self->source, g_source_unref);
    }
    loong_seat_finalize(self);
    if (self->screens) {
        for (guint i = 0; i < self->screens->len; i++)
            wpe_screen_invalidate(g_ptr_array_index(self->screens, i));
        g_clear_pointer(&self->screens, g_ptr_array_unref);
    }
    g_clear_pointer(&self->text_input, zwp_text_input_v3_destroy);
    g_clear_pointer(&self->text_input_manager, zwp_text_input_manager_v3_destroy);
    g_clear_pointer(&self->dmabuf_feedback, zwp_linux_dmabuf_feedback_v1_destroy);
    g_clear_pointer(&self->dmabuf, zwp_linux_dmabuf_v1_destroy);
    g_clear_pointer(&self->single_pixel, wp_single_pixel_buffer_manager_v1_destroy);
    g_clear_pointer(&self->fractional_scale_manager, wp_fractional_scale_manager_v1_destroy);
    g_clear_pointer(&self->viewporter, wp_viewporter_destroy);
    g_clear_pointer(&self->shm, wl_shm_destroy);
    g_clear_pointer(&self->wm_base, xdg_wm_base_destroy);
    g_clear_pointer(&self->subcompositor, wl_subcompositor_destroy);
    g_clear_pointer(&self->compositor, wl_compositor_destroy);
    g_clear_pointer(&self->registry, wl_registry_destroy);
    g_clear_pointer(&self->wl_display, wl_display_disconnect);
    g_clear_pointer(&self->drm_device, wpe_drm_device_unref);
    g_clear_pointer(&self->dmabuf_formats, g_array_unref);
    G_OBJECT_CLASS(loong_display_parent_class)->dispose(object);
}

static void
loong_display_init(LoongDisplay *self)
{
    self->screens = g_ptr_array_new_with_free_func(g_object_unref);
    self->dmabuf_formats = g_array_new(FALSE, FALSE, sizeof(LoongDmabufFormat));
}

static void
loong_display_class_init(LoongDisplayClass *klass)
{
    G_OBJECT_CLASS(klass)->dispose = loong_display_dispose;
    WPEDisplayClass *display_class = WPE_DISPLAY_CLASS(klass);
    display_class->connect = loong_display_connect;
    display_class->create_view = loong_display_create_view;
    display_class->create_toplevel = loong_display_create_toplevel;
    display_class->create_input_method_context = loong_display_create_input_method_context;
    display_class->get_keymap = loong_display_get_keymap;
    display_class->get_preferred_buffer_formats = loong_display_get_preferred_buffer_formats;
    display_class->get_n_screens = loong_display_get_n_screens;
    display_class->get_screen = loong_display_get_screen;
    display_class->get_drm_device = loong_display_get_drm_device;
}

/* ───────────────────────── 公开与内部接口 ───────────────────────── */

WPEDisplay *
loong_display_new(void)
{
    return g_object_new(LOONG_TYPE_DISPLAY, NULL);
}

gboolean
loong_display_has_linux_dmabuf(LoongDisplay *display)
{
    g_return_val_if_fail(LOONG_IS_DISPLAY(display), FALSE);
    return display->dmabuf != NULL;
}

gboolean
loong_display_has_fractional_scale(LoongDisplay *display)
{
    g_return_val_if_fail(LOONG_IS_DISPLAY(display), FALSE);
    return display->fractional_scale_manager && display->viewporter;
}

LoongScreen *
loong_display_find_screen(LoongDisplay *display, struct wl_output *output)
{
    for (guint i = 0; i < display->screens->len; i++) {
        LoongScreen *screen = g_ptr_array_index(display->screens, i);
        if (screen->wl_output == output)
            return screen;
    }
    return NULL;
}

struct wl_shm *
loong_display_get_shm(LoongDisplay *display)
{
    return display->shm;
}
