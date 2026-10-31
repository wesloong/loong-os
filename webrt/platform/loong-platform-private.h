/*
 * loong-platform 内部结构（只在本库的 .c 之间共享）。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#pragma once

#include "loong-platform.h"

#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>

#include "fractional-scale-v1-client-protocol.h"
#include "linux-dmabuf-v1-client-protocol.h"
#include "single-pixel-buffer-v1-client-protocol.h"
#include "text-input-unstable-v3-client-protocol.h"
#include "viewporter-client-protocol.h"
#include "xdg-shell-client-protocol.h"

G_BEGIN_DECLS

typedef struct {
    guint32 fourcc;
    guint64 modifier;
} LoongDmabufFormat;

/* 一个触点：按下时落在哪个 view，以及事件表面坐标到 view 坐标的偏移。 */
typedef struct {
    GWeakRef view;
    double dx, dy;
    double x, y; /* view 坐标 */
} LoongTouchPoint;

/* 输入设备与焦点。坐标一律先换算成“窗口坐标”（相对根表面），再换算成 view 坐标。 */
typedef struct {
    struct wl_seat *wl_seat;
    guint32 capabilities;
    struct wl_pointer *pointer;
    struct wl_keyboard *keyboard;
    struct wl_touch *touch;
    WPEKeymap *keymap;

    /* 指针 */
    LoongToplevel *pointer_toplevel; /* 弱引用 */
    struct wl_surface *pointer_surface;
    double pointer_sx, pointer_sy; /* 相对 pointer_surface */
    WPEView *pointer_view;         /* 当前在指针下（或被按键抓住）的 view，弱引用 */
    WPEView *grab_view;            /* 按下按键时的隐式抓取，弱引用 */
    double view_x, view_y;         /* 相对 pointer_view */
    guint32 pointer_enter_serial;
    guint32 pointer_time;
    guint32 pointer_modifiers; /* WPE_MODIFIER_POINTER_BUTTON* */
    struct {
        double dx, dy;     /* 连续滚动量 */
        int vx, vy;        /* 离散档位 */
        gboolean stop;
        WPEInputSource source;
    } scroll;

    /* 键盘 */
    LoongToplevel *keyboard_toplevel; /* 弱引用 */
    guint32 keyboard_modifiers;
    guint32 keyboard_time;
    struct {
        int rate, delay; /* rate=0 表示合成器关闭了重复；-1 表示未收到 repeat_info */
        guint32 key;
        GSource *source;
        gint64 deadline;
    } repeat;

    /* 触摸 */
    GHashTable *touch_points; /* id → LoongTouchPoint* */
} LoongSeat;

struct _LoongDisplay {
    WPEDisplay parent_instance;

    struct wl_display *wl_display;
    struct wl_registry *registry;
    struct wl_compositor *compositor;
    struct wl_subcompositor *subcompositor;
    struct xdg_wm_base *wm_base;
    struct wl_shm *shm;
    struct wp_viewporter *viewporter;
    struct wp_fractional_scale_manager_v1 *fractional_scale_manager;
    struct wp_single_pixel_buffer_manager_v1 *single_pixel;
    struct zwp_linux_dmabuf_v1 *dmabuf;
    struct zwp_linux_dmabuf_feedback_v1 *dmabuf_feedback;
    struct zwp_text_input_manager_v3 *text_input_manager;
    struct zwp_text_input_v3 *text_input;
    GArray *dmabuf_formats; /* LoongDmabufFormat */
    WPEDRMDevice *drm_device;
    GPtrArray *screens; /* LoongScreen */
    GSource *source;
    LoongSeat seat;
};

struct _LoongScreen {
    WPEScreen parent_instance;
    struct wl_output *wl_output;
    guint32 name;
    struct {
        int x, y, width, height, scale;
    } pending;
};

struct _LoongToplevel {
    WPEToplevel parent_instance;

    struct wl_surface *surface;
    struct xdg_surface *xdg_surface;
    struct xdg_toplevel *xdg_toplevel;
    struct wp_viewport *viewport;
    struct wp_fractional_scale_v1 *fractional_scale;
    char *app_id;

    guint32 background;
    struct wl_buffer *bg_buffer;
    struct wl_buffer *bg_retired; /* 已换下、下次提交后销毁 */
    int bg_width, bg_height;      /* 底色缓冲的像素尺寸（单像素缓冲为 1×1） */
    gboolean bg_dirty;
    gboolean bg_color_dirty;

    gboolean configured;
    struct {
        int width, height;
        WPEToplevelState state;
    } pending;

    GList *stack;            /* LoongView*，从下到上 */
    guint next_order;        /* 加入窗口的先后，决定同层的上下 */
    WPEView *focused_view;   /* 键盘焦点，弱引用 */
    gboolean keyboard_inside;

    GPtrArray *screens;      /* 根表面所在的输出 */
    int preferred_buffer_scale;  /* wl_surface.preferred_buffer_scale；0 = 未收到 */
    double fractional_scale_value; /* wp_fractional_scale_v1；0 = 未收到 */
    guint commit_idle;
};

/* loong-display.c */
LoongScreen *loong_display_find_screen(LoongDisplay *display, struct wl_output *output);
struct wl_shm *loong_display_get_shm(LoongDisplay *display);

/* loong-im-context.c：合成器不支持 text-input-v3 时返回 NULL（WPE 退回到无输入法）。 */
WPEInputMethodContext *loong_im_context_new(LoongDisplay *display, WPEView *view);
void loong_im_surface_destroyed(LoongDisplay *display, struct wl_surface *surface);

/* loong-screen.c */
LoongScreen *loong_screen_new(guint32 name, struct wl_output *output);

/* loong-seat.c */
void loong_seat_init(LoongDisplay *display, struct wl_seat *seat);
void loong_seat_finalize(LoongDisplay *display);
WPEModifiers loong_seat_modifiers(LoongDisplay *display);
/* view 被移走或隐藏时，清掉输入状态中对它的引用。 */
void loong_seat_view_gone(LoongDisplay *display, WPEView *view);
/* 表面即将销毁：清掉指针与输入法记住的表面。 */
void loong_seat_surface_destroyed(LoongDisplay *display, struct wl_surface *surface);

/* loong-toplevel.c */
void loong_toplevel_add_view(LoongToplevel *toplevel, LoongView *view);
void loong_toplevel_remove_view(LoongToplevel *toplevel, LoongView *view);
void loong_toplevel_restack(LoongToplevel *toplevel);
void loong_toplevel_schedule_commit(LoongToplevel *toplevel);
/* 窗口坐标 (x, y) 处最上层、已映射的 view。 */
WPEView *loong_toplevel_view_at(LoongToplevel *toplevel, double x, double y);
void loong_toplevel_set_focused_view(LoongToplevel *toplevel, WPEView *view);
WPEView *loong_toplevel_get_focused_view(LoongToplevel *toplevel);
void loong_toplevel_keyboard_enter(LoongToplevel *toplevel);
void loong_toplevel_keyboard_leave(LoongToplevel *toplevel);
double loong_toplevel_buffer_scale(LoongToplevel *toplevel);

/* loong-view.c */
struct wl_surface *loong_view_get_wl_surface(LoongView *view);
void loong_view_attach_to_parent(LoongView *view, struct wl_surface *parent);
void loong_view_detach_from_parent(LoongView *view);
struct wl_subsurface *loong_view_get_subsurface(LoongView *view);
int loong_view_get_layer(LoongView *view);
guint loong_view_get_order(LoongView *view);
void loong_view_set_order(LoongView *view, guint order);
gboolean loong_view_has_geometry(LoongView *view);
void loong_view_toplevel_configured(LoongView *view);
void loong_view_scale_changed(LoongView *view);

/* 由 wl_surface 找到所属对象：LoongToplevel（根表面）或 LoongView（子表面）。 */
static inline GObject *
loong_surface_owner(struct wl_surface *surface)
{
    if (!surface)
        return NULL;
    gpointer data = wl_surface_get_user_data(surface);
    if (data && (LOONG_IS_TOPLEVEL(data) || LOONG_IS_VIEW(data)))
        return G_OBJECT(data);
    return NULL;
}

G_END_DECLS
