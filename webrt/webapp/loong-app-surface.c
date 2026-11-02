/*
 * loong-app-surface：loong_app_v1 客户端（单独的事件队列，见 loong-app-surface.h）。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include "loong-app-surface.h"

#include <string.h>

#include "loong-app-v1-client-protocol.h"

struct _LoongAppSurface {
    struct wl_display *display;
    struct wl_event_queue *queue;
    struct loong_app_manager_v1 *manager;
    struct loong_app_surface_v1 *app_surface;
    gboolean sent_color, sent_style;
    guint32 color;
    LoongAppStatusBarStyle style;
};

static void
registry_global(void *data, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version)
{
    LoongAppSurface *self = data;
    if (!strcmp(interface, loong_app_manager_v1_interface.name) && !self->manager)
        self->manager = wl_registry_bind(registry, name, &loong_app_manager_v1_interface, 1);
}

static void
registry_global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
}

static const struct wl_registry_listener registry_listener = {
    .global = registry_global,
    .global_remove = registry_global_remove,
};

LoongAppSurface *
loong_app_surface_new(struct wl_display *display, struct wl_surface *surface)
{
    g_return_val_if_fail(display && surface, NULL);
    LoongAppSurface *self = g_new0(LoongAppSurface, 1);
    self->display = display;
    self->queue = wl_display_create_queue(display);

    /* 注册表放在自己的队列上：往返只分发这个队列，WPE 默认队列上的事件原样留给 WPE。 */
    struct wl_display *wrapper = wl_proxy_create_wrapper(display);
    wl_proxy_set_queue((struct wl_proxy *)wrapper, self->queue);
    struct wl_registry *registry = wl_display_get_registry(wrapper);
    wl_proxy_wrapper_destroy(wrapper);
    wl_registry_add_listener(registry, &registry_listener, self);
    int ok = wl_display_roundtrip_queue(display, self->queue);
    /* 之后不再分发这个队列：注册表销毁，免得新的全局对象事件堆在队列里。 */
    wl_registry_destroy(registry);
    if (ok < 0 || !self->manager) {
        if (ok < 0)
            g_warning("loong_app_v1：注册表往返失败");
        else
            g_debug("合成器没有 loong_app_manager_v1，状态栏由合成器自行决定");
        loong_app_surface_free(self);
        return NULL;
    }
    self->app_surface = loong_app_manager_v1_get_app_surface(self->manager, surface);
    wl_display_flush(display);
    g_debug("loong_app_v1：已为窗口建立 loong_app_surface_v1");
    return self;
}

void
loong_app_surface_set_theme_color(LoongAppSurface *self, guint32 argb)
{
    if (!self)
        return;
    if (!(argb >> 24))
        argb = 0;
    if (self->sent_color && self->color == argb)
        return;
    self->color = argb;
    self->sent_color = TRUE;
    loong_app_surface_v1_set_theme_color(self->app_surface, argb);
    /* 立即发出：请求在下一轮主循环之前不会被 WPE 刷出（WPE 只在 prepare 时 flush）。 */
    wl_display_flush(self->display);
    g_debug("loong_app_v1：主题色 %08x", argb);
}

void
loong_app_surface_set_status_bar_style(LoongAppSurface *self, LoongAppStatusBarStyle style)
{
    if (!self)
        return;
    if (self->sent_style && self->style == style)
        return;
    self->style = style;
    self->sent_style = TRUE;
    loong_app_surface_v1_set_status_bar_style(self->app_surface, style);
    wl_display_flush(self->display);
    g_debug("loong_app_v1：状态栏样式 %d", style);
}

void
loong_app_surface_free(LoongAppSurface *self)
{
    if (!self)
        return;
    g_clear_pointer(&self->app_surface, loong_app_surface_v1_destroy);
    g_clear_pointer(&self->manager, loong_app_manager_v1_destroy);
    if (self->display)
        wl_display_flush(self->display);
    g_clear_pointer(&self->queue, wl_event_queue_destroy);
    g_free(self);
}
