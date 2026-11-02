/*
 * loong-app-surface：Loong 私有协议 loong_app_v1 的客户端（protocols/loong-app-v1.xml，
 * 说明见 docs/api/loong-app-protocol.md），供跑在 WPE 自带 Wayland 平台上的 loong-webapp 使用。
 *
 * WPE 的 Wayland 平台不认识这个协议，它的 wl_display 事件由 WPE 自己的 GSource 分发。这里在
 * 同一个连接上建一个**单独的事件队列**做注册表往返、绑定 loong_app_manager_v1，不碰 WPE 的
 * 默认队列；协议只有请求、没有事件，之后这个队列不需要再分发。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#pragma once

#include <glib.h>
#include <wayland-client.h>

G_BEGIN_DECLS

typedef struct _LoongAppSurface LoongAppSurface;

/* 状态栏样式（与协议的 status_bar_style 相同）。 */
typedef enum {
    LOONG_APP_STATUS_BAR_AUTO = 0,
    LOONG_APP_STATUS_BAR_DARK_CONTENT = 1,
    LOONG_APP_STATUS_BAR_LIGHT_CONTENT = 2,
} LoongAppStatusBarStyle;

/*
 * 为窗口的根表面 surface 建立 loong_app_surface_v1。合成器没有 loong_app_manager_v1
 * （不是 loong-shell）时返回 NULL，应用照常运行。
 */
LoongAppSurface *loong_app_surface_new(struct wl_display *display, struct wl_surface *surface);

/* 顶部区域的颜色 0xAARRGGBB（非预乘）；alpha 为 0 表示没有。与上次相同时不发送。 */
void loong_app_surface_set_theme_color(LoongAppSurface *self, guint32 argb);

/* 状态栏样式。与上次相同时不发送。 */
void loong_app_surface_set_status_bar_style(LoongAppSurface *self, LoongAppStatusBarStyle style);

/* 销毁对象（状态栏回到 auto）。可以传 NULL。 */
void loong_app_surface_free(LoongAppSurface *self);

G_END_DECLS
