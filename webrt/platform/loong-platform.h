/*
 * loong-platform：Loong OS 自己的 WPEPlatform 后端（“loong” 平台）。
 *
 * WPE 自带的 Wayland 平台把一个 toplevel 下的所有 view 画到同一个 wl_surface 上，
 * 浏览器的界面（chrome）和网页内容因此无法同屏。本后端在进程内实现 WPEDisplay /
 * WPEToplevel / WPEView：一个 toplevel 对应一个 xdg_toplevel 窗口，窗口的根表面只画
 * 一层纸色底；每个 WPEView 是根表面的一个 wl_subsurface，由应用决定位置、大小和层次。
 *
 *   WPEDisplay *display = loong_display_new();
 *   wpe_display_connect(display, &error);
 *   WPEToplevel *toplevel = wpe_display_create_toplevel(display, 0);   // 0：view 数不限
 *   WebKitWebView *web_view = g_object_new(WEBKIT_TYPE_WEB_VIEW, "display", display, …);
 *   WPEView *view = webkit_web_view_get_wpe_view(web_view);
 *   wpe_view_set_toplevel(view, toplevel);
 *   loong_view_set_geometry(LOONG_VIEW(view), 0, 76, 1280, 724);
 *
 * 详见 docs/adr/0003-web-内核-wpe-webkit.md。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#pragma once

#include <wpe/wpe-platform.h>

G_BEGIN_DECLS

#define LOONG_TYPE_DISPLAY (loong_display_get_type())
G_DECLARE_FINAL_TYPE(LoongDisplay, loong_display, LOONG, DISPLAY, WPEDisplay)

#define LOONG_TYPE_TOPLEVEL (loong_toplevel_get_type())
G_DECLARE_FINAL_TYPE(LoongToplevel, loong_toplevel, LOONG, TOPLEVEL, WPEToplevel)

#define LOONG_TYPE_VIEW (loong_view_get_type())
G_DECLARE_FINAL_TYPE(LoongView, loong_view, LOONG, VIEW, WPEView)

#define LOONG_TYPE_SCREEN (loong_screen_get_type())
G_DECLARE_FINAL_TYPE(LoongScreen, loong_screen, LOONG, SCREEN, WPEScreen)

/* 创建 loong 显示（尚未连接）。第一个创建的 WPEDisplay 会成为 WebKit 的主显示。 */
WPEDisplay *loong_display_new(void);

/* 合成器提供的能力，便于诊断与测试。 */
gboolean loong_display_has_linux_dmabuf(LoongDisplay *display);
gboolean loong_display_has_fractional_scale(LoongDisplay *display);
gboolean loong_display_has_app_protocol(LoongDisplay *display);

/*
 * 窗口的 Wayland app_id（默认取 g_get_prgname()）。需在第一次 configure 之前调用。
 */
void loong_toplevel_set_app_id(LoongToplevel *toplevel, const char *app_id);

/* 根表面的底色（0xRRGGBB，不透明）：没有被任何 view 覆盖的区域显示这个颜色。 */
void loong_toplevel_set_background(LoongToplevel *toplevel, guint32 rgb);

/*
 * 系统状态栏（loong-shell 画在全屏窗口顶部 24pt 上）用哪种墨，经 Loong 私有协议 loong_app_v1
 * 告诉合成器（protocols/loong-app-v1.xml，docs/api/loong-app-protocol.md）。合成器没有这个协议
 * 时不做任何事。值与协议的 status_bar_style 相同。
 */
typedef enum {
    LOONG_STATUS_BAR_AUTO = 0,          /* 由合成器决定：主题色 → 取样 → 系统外观 */
    LOONG_STATUS_BAR_DARK_CONTENT = 1,  /* 浓墨（深色字形），窗口顶部是亮的 */
    LOONG_STATUS_BAR_LIGHT_CONTENT = 2, /* 月白（浅色字形），窗口顶部是暗的 */
} LoongStatusBarStyle;

void loong_toplevel_set_status_bar_style(LoongToplevel *toplevel, LoongStatusBarStyle style);

/* 窗口顶部（状态栏下方）的颜色 0xAARRGGBB，非预乘；alpha 为 0 表示没有（默认）。
 * 只在样式为 LOONG_STATUS_BAR_AUTO 时起作用。 */
void loong_toplevel_set_theme_color(LoongToplevel *toplevel, guint32 argb);

/* 窗口是否已收到第一次 configure（之后 view 才会映射、开始出帧）。 */
gboolean loong_toplevel_is_configured(LoongToplevel *toplevel);

/*
 * view 在窗口中的位置与大小（逻辑像素，相对窗口左上角）。
 * 没有调用过时，view 铺满整个窗口并随窗口缩放（与 WPE 自带平台的行为相同）。
 */
void loong_view_set_geometry(LoongView *view, int x, int y, int width, int height);
void loong_view_get_position(LoongView *view, int *x, int *y);

/* 层次：layer 大的在上；同一层中后加入窗口的在上。默认 0。 */
void loong_view_set_layer(LoongView *view, int layer);

/* 把键盘焦点交给这个 view（同一窗口中的其他 view 失去焦点）。 */
void loong_view_focus(LoongView *view);

G_END_DECLS
