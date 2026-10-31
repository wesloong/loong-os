/*
 * 输入：把 wl_pointer / wl_keyboard / wl_touch 事件分发给正确的 view。
 *
 * 目标 view 的确定：
 *   - 事件落在某个 view 的子表面上（合成器做了子表面命中测试）：就是这个 view，坐标即表面坐标；
 *   - 事件落在窗口根表面上（合成器不认子表面，例如目前的 loong-shell）：按 view 的位置与
 *     层次自己做命中测试，坐标换算成 view 坐标。
 *   - 指针按下后隐式抓取：直到松开，事件都发给按下时的 view；触点同理，按下时定下目标。
 * 键盘焦点跟随最后一次按下（点按 / 触摸）的 view，应用也可以用 loong_view_focus() 指定。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include "loong-platform-private.h"

#include <linux/input-event-codes.h>
#include <string.h>

/* 表面在窗口坐标中的原点；返回所属窗口。 */
static LoongToplevel *
surface_origin(struct wl_surface *surface, double *ox, double *oy)
{
    GObject *owner = loong_surface_owner(surface);
    *ox = *oy = 0;
    if (!owner)
        return NULL;
    if (LOONG_IS_TOPLEVEL(owner))
        return LOONG_TOPLEVEL(owner);
    WPEToplevel *toplevel = wpe_view_get_toplevel(WPE_VIEW(owner));
    if (!toplevel || !LOONG_IS_TOPLEVEL(toplevel))
        return NULL;
    int x, y;
    loong_view_get_position(LOONG_VIEW(owner), &x, &y);
    *ox = x;
    *oy = y;
    return LOONG_TOPLEVEL(toplevel);
}

/* 找到 (surface, sx, sy) 处的目标 view 与 view 坐标。 */
static WPEView *
resolve_target(struct wl_surface *surface, double sx, double sy, double *vx, double *vy, LoongToplevel **out_toplevel)
{
    GObject *owner = loong_surface_owner(surface);
    if (out_toplevel)
        *out_toplevel = NULL;
    if (!owner)
        return NULL;
    if (LOONG_IS_VIEW(owner) && wpe_view_get_mapped(WPE_VIEW(owner))) {
        WPEToplevel *toplevel = wpe_view_get_toplevel(WPE_VIEW(owner));
        if (out_toplevel && toplevel && LOONG_IS_TOPLEVEL(toplevel))
            *out_toplevel = LOONG_TOPLEVEL(toplevel);
        *vx = sx;
        *vy = sy;
        return WPE_VIEW(owner);
    }
    double ox, oy;
    LoongToplevel *toplevel = surface_origin(surface, &ox, &oy);
    if (!toplevel)
        return NULL;
    if (out_toplevel)
        *out_toplevel = toplevel;
    double tx = ox + sx, ty = oy + sy;
    WPEView *view = loong_toplevel_view_at(toplevel, tx, ty);
    if (!view)
        return NULL;
    int x, y;
    loong_view_get_position(LOONG_VIEW(view), &x, &y);
    *vx = tx - x;
    *vy = ty - y;
    return view;
}

/* 窗口中 (surface, sx, sy) 相对某个 view 的坐标（用于抓取：手指可以移出 view）。 */
static void
coords_relative_to(WPEView *view, struct wl_surface *surface, double sx, double sy, double *vx, double *vy)
{
    double ox, oy;
    surface_origin(surface, &ox, &oy);
    int x, y;
    loong_view_get_position(LOONG_VIEW(view), &x, &y);
    *vx = ox + sx - x;
    *vy = oy + sy - y;
}

WPEModifiers
loong_seat_modifiers(LoongDisplay *display)
{
    LoongSeat *seat = &display->seat;
    return (WPEModifiers)(seat->keyboard_modifiers | seat->pointer_modifiers);
}

static void
emit(WPEView *view, WPEEvent *event)
{
    wpe_view_event(view, event);
    wpe_event_unref(event);
}

/* ───────────────────────── 指针 ───────────────────────── */

static void
set_pointer_view(LoongDisplay *display, WPEView *view, double vx, double vy)
{
    LoongSeat *seat = &display->seat;
    WPEModifiers mods = loong_seat_modifiers(display);
    if (seat->pointer_view != view) {
        if (seat->pointer_view)
            emit(seat->pointer_view, wpe_event_pointer_move_new(WPE_EVENT_POINTER_LEAVE, seat->pointer_view, WPE_INPUT_SOURCE_MOUSE,
                                                                seat->pointer_time, mods, -1, -1, 0, 0));
        g_set_weak_pointer(&seat->pointer_view, view);
        if (view)
            emit(view, wpe_event_pointer_move_new(WPE_EVENT_POINTER_ENTER, view, WPE_INPUT_SOURCE_MOUSE, seat->pointer_time, mods, vx, vy, 0, 0));
    }
    seat->view_x = vx;
    seat->view_y = vy;
}

/* 根据当前指针位置重新确定目标（有抓取时目标不变）。 */
static void
update_pointer_target(LoongDisplay *display)
{
    LoongSeat *seat = &display->seat;
    double vx = 0, vy = 0;
    if (seat->grab_view) {
        coords_relative_to(seat->grab_view, seat->pointer_surface, seat->pointer_sx, seat->pointer_sy, &vx, &vy);
        set_pointer_view(display, seat->grab_view, vx, vy);
        return;
    }
    WPEView *view = seat->pointer_surface ? resolve_target(seat->pointer_surface, seat->pointer_sx, seat->pointer_sy, &vx, &vy, NULL) : NULL;
    set_pointer_view(display, view, vx, vy);
}

static void
pointer_enter(void *data, struct wl_pointer *pointer, uint32_t serial, struct wl_surface *surface, wl_fixed_t sx, wl_fixed_t sy)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    if (!loong_surface_owner(surface))
        return;
    double ox, oy;
    g_set_weak_pointer(&seat->pointer_toplevel, surface_origin(surface, &ox, &oy));
    seat->pointer_surface = surface;
    seat->pointer_sx = wl_fixed_to_double(sx);
    seat->pointer_sy = wl_fixed_to_double(sy);
    seat->pointer_enter_serial = serial;
    update_pointer_target(display);
    g_debug("指针进入%s表面 (%.0f, %.0f) → view %p", LOONG_IS_TOPLEVEL(loong_surface_owner(surface)) ? "根" : "子",
            seat->pointer_sx, seat->pointer_sy, (void *)seat->pointer_view);
}

static void
pointer_leave(void *data, struct wl_pointer *pointer, uint32_t serial, struct wl_surface *surface)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    if (surface != seat->pointer_surface)
        return;
    seat->pointer_surface = NULL;
    g_clear_weak_pointer(&seat->grab_view);
    set_pointer_view(display, NULL, 0, 0);
    g_clear_weak_pointer(&seat->pointer_toplevel);
}

static void
pointer_motion(void *data, struct wl_pointer *pointer, uint32_t time, wl_fixed_t sx, wl_fixed_t sy)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    if (!seat->pointer_surface)
        return;
    double old_x = seat->view_x, old_y = seat->view_y;
    WPEView *old_view = seat->pointer_view;
    seat->pointer_time = time;
    seat->pointer_sx = wl_fixed_to_double(sx);
    seat->pointer_sy = wl_fixed_to_double(sy);
    update_pointer_target(display);
    if (!seat->pointer_view)
        return;
    double dx = seat->pointer_view == old_view ? seat->view_x - old_x : 0;
    double dy = seat->pointer_view == old_view ? seat->view_y - old_y : 0;
    emit(seat->pointer_view, wpe_event_pointer_move_new(WPE_EVENT_POINTER_MOVE, seat->pointer_view, WPE_INPUT_SOURCE_MOUSE, time,
                                                        loong_seat_modifiers(display), seat->view_x, seat->view_y, dx, dy));
}

static void
pointer_button(void *data, struct wl_pointer *pointer, uint32_t serial, uint32_t time, uint32_t button, uint32_t state)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    if (!seat->pointer_surface)
        return;
    guint wpe_button;
    switch (button) {
    case BTN_LEFT:
        wpe_button = WPE_BUTTON_PRIMARY;
        break;
    case BTN_MIDDLE:
        wpe_button = WPE_BUTTON_MIDDLE;
        break;
    case BTN_RIGHT:
        wpe_button = WPE_BUTTON_SECONDARY;
        break;
    default:
        wpe_button = button - BTN_MOUSE + 1;
        break;
    }
    guint32 mask = 0;
    if (wpe_button >= 1 && wpe_button <= 5)
        mask = WPE_MODIFIER_POINTER_BUTTON1 << (wpe_button - 1);
    gboolean pressed = state == WL_POINTER_BUTTON_STATE_PRESSED;
    seat->pointer_time = time;

    if (pressed && !seat->grab_view) {
        update_pointer_target(display);
        if (seat->pointer_view) {
            g_set_weak_pointer(&seat->grab_view, seat->pointer_view);
            WPEToplevel *toplevel = wpe_view_get_toplevel(seat->pointer_view);
            if (toplevel && LOONG_IS_TOPLEVEL(toplevel))
                loong_toplevel_set_focused_view(LOONG_TOPLEVEL(toplevel), seat->pointer_view);
        }
    }
    if (pressed)
        seat->pointer_modifiers |= mask;
    else
        seat->pointer_modifiers &= ~mask;

    WPEView *view = seat->grab_view ? seat->grab_view : seat->pointer_view;
    g_debug("指针按键 %u %s → view %p (%.0f, %.0f)", wpe_button, pressed ? "按下" : "松开", (void *)view, seat->view_x, seat->view_y);
    if (view) {
        guint count = pressed ? wpe_view_compute_press_count(view, seat->view_x, seat->view_y, wpe_button, time) : 0;
        emit(view, wpe_event_pointer_button_new(pressed ? WPE_EVENT_POINTER_DOWN : WPE_EVENT_POINTER_UP, view, WPE_INPUT_SOURCE_MOUSE,
                                                time, loong_seat_modifiers(display), wpe_button, seat->view_x, seat->view_y, count));
    }
    if (!pressed && !(seat->pointer_modifiers & (WPE_MODIFIER_POINTER_BUTTON1 | WPE_MODIFIER_POINTER_BUTTON2 | WPE_MODIFIER_POINTER_BUTTON3 |
                                                 WPE_MODIFIER_POINTER_BUTTON4 | WPE_MODIFIER_POINTER_BUTTON5))) {
        g_clear_weak_pointer(&seat->grab_view);
        update_pointer_target(display);
    }
}

static void
flush_scroll(LoongDisplay *display)
{
    LoongSeat *seat = &display->seat;
    WPEView *view = seat->grab_view ? seat->grab_view : seat->pointer_view;
    if (view) {
        WPEEvent *event = NULL;
        if (seat->scroll.vx || seat->scroll.vy) {
            event = wpe_event_scroll_new(view, seat->scroll.source, seat->pointer_time, loong_seat_modifiers(display), seat->scroll.vx,
                                         seat->scroll.vy, FALSE, FALSE, seat->view_x, seat->view_y);
        } else if (seat->scroll.stop || seat->scroll.dx || seat->scroll.dy) {
            event = wpe_event_scroll_new(view, seat->scroll.source, seat->pointer_time, loong_seat_modifiers(display), seat->scroll.dx,
                                         seat->scroll.dy, TRUE, seat->scroll.stop && !seat->scroll.dx && !seat->scroll.dy, seat->view_x,
                                         seat->view_y);
        }
        if (event)
            emit(view, event);
    }
    memset(&seat->scroll, 0, sizeof(seat->scroll));
    seat->scroll.source = WPE_INPUT_SOURCE_MOUSE;
}

static void
pointer_axis(void *data, struct wl_pointer *pointer, uint32_t time, uint32_t axis, wl_fixed_t value)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    if (!seat->pointer_surface)
        return;
    if (axis == WL_POINTER_AXIS_HORIZONTAL_SCROLL)
        seat->scroll.dx = -wl_fixed_to_double(value);
    else
        seat->scroll.dy = -wl_fixed_to_double(value);
    seat->pointer_time = time;
    if (wl_pointer_get_version(pointer) < WL_POINTER_FRAME_SINCE_VERSION)
        flush_scroll(display);
}

static void
pointer_frame(void *data, struct wl_pointer *pointer)
{
    LoongDisplay *display = data;
    if (display->seat.pointer_surface)
        flush_scroll(display);
}

static void
pointer_axis_source(void *data, struct wl_pointer *pointer, uint32_t source)
{
    LoongSeat *seat = &((LoongDisplay *)data)->seat;
    switch (source) {
    case WL_POINTER_AXIS_SOURCE_FINGER:
        seat->scroll.source = WPE_INPUT_SOURCE_TOUCHPAD;
        break;
    case WL_POINTER_AXIS_SOURCE_CONTINUOUS:
        seat->scroll.source = WPE_INPUT_SOURCE_TRACKPOINT;
        break;
    default:
        seat->scroll.source = WPE_INPUT_SOURCE_MOUSE;
        break;
    }
}

static void
pointer_axis_stop(void *data, struct wl_pointer *pointer, uint32_t time, uint32_t axis)
{
    LoongSeat *seat = &((LoongDisplay *)data)->seat;
    if (axis == WL_POINTER_AXIS_HORIZONTAL_SCROLL)
        seat->scroll.dx = 0;
    else
        seat->scroll.dy = 0;
    seat->scroll.stop = TRUE;
    seat->pointer_time = time;
}

static void
pointer_axis_discrete(void *data, struct wl_pointer *pointer, uint32_t axis, int32_t discrete)
{
    LoongSeat *seat = &((LoongDisplay *)data)->seat;
    if (axis == WL_POINTER_AXIS_HORIZONTAL_SCROLL)
        seat->scroll.vx = -discrete;
    else
        seat->scroll.vy = -discrete;
}

static const struct wl_pointer_listener pointer_listener = {
    .enter = pointer_enter,
    .leave = pointer_leave,
    .motion = pointer_motion,
    .button = pointer_button,
    .axis = pointer_axis,
    .frame = pointer_frame,
    .axis_source = pointer_axis_source,
    .axis_stop = pointer_axis_stop,
    .axis_discrete = pointer_axis_discrete,
};

/* ───────────────────────── 键盘 ───────────────────────── */

static WPEView *
keyboard_view(LoongDisplay *display)
{
    LoongSeat *seat = &display->seat;
    return seat->keyboard_toplevel ? loong_toplevel_get_focused_view(seat->keyboard_toplevel) : NULL;
}

static void stop_repeat(LoongSeat *seat);

static void
handle_key(LoongDisplay *display, guint32 time, guint32 keycode, gboolean pressed, gboolean from_repeat);

static gboolean
repeat_cb(gpointer data)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    if (!seat->repeat.key) {
        seat->repeat.source = NULL;
        return G_SOURCE_REMOVE;
    }
    handle_key(display, seat->keyboard_time, seat->repeat.key, TRUE, TRUE);
    return G_SOURCE_CONTINUE;
}

static gboolean
repeat_delay_cb(gpointer data)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    seat->repeat.source = NULL;
    if (!seat->repeat.key)
        return G_SOURCE_REMOVE;
    int rate = seat->repeat.rate > 0 ? seat->repeat.rate : 25;
    handle_key(display, seat->keyboard_time, seat->repeat.key, TRUE, TRUE);
    if (seat->repeat.key) {
        guint id = g_timeout_add(MAX(1, 1000 / rate), repeat_cb, display);
        seat->repeat.source = g_main_context_find_source_by_id(NULL, id);
    }
    return G_SOURCE_REMOVE;
}

static void
stop_repeat(LoongSeat *seat)
{
    seat->repeat.key = 0;
    if (seat->repeat.source) {
        g_source_destroy(seat->repeat.source);
        seat->repeat.source = NULL;
    }
}

static void
handle_key(LoongDisplay *display, guint32 time, guint32 keycode, gboolean pressed, gboolean from_repeat)
{
    LoongSeat *seat = &display->seat;
    WPEView *view = keyboard_view(display);
    if (!view)
        return;
    struct xkb_state *state = wpe_keymap_xkb_get_xkb_state(WPE_KEYMAP_XKB(seat->keymap));
    struct xkb_keymap *keymap = wpe_keymap_xkb_get_xkb_keymap(WPE_KEYMAP_XKB(seat->keymap));
    if (!state || !keymap)
        return;
    xkb_keysym_t keyval = xkb_state_key_get_one_sym(state, keycode);
    if (keyval == XKB_KEY_NoSymbol)
        return;
    guint32 mods = loong_seat_modifiers(display);
    guint32 bit = 0;
    switch (keyval) {
    case XKB_KEY_Control_L:
    case XKB_KEY_Control_R:
        bit = WPE_MODIFIER_KEYBOARD_CONTROL;
        break;
    case XKB_KEY_Shift_L:
    case XKB_KEY_Shift_R:
        bit = WPE_MODIFIER_KEYBOARD_SHIFT;
        break;
    case XKB_KEY_Alt_L:
    case XKB_KEY_Alt_R:
        bit = WPE_MODIFIER_KEYBOARD_ALT;
        break;
    case XKB_KEY_Meta_L:
    case XKB_KEY_Meta_R:
        bit = WPE_MODIFIER_KEYBOARD_META;
        break;
    default:
        break;
    }
    if (bit)
        mods = pressed ? (mods | bit) : (mods & ~bit);
    seat->keyboard_time = time;
    emit(view, wpe_event_keyboard_new(pressed ? WPE_EVENT_KEYBOARD_KEY_DOWN : WPE_EVENT_KEYBOARD_KEY_UP, view, WPE_INPUT_SOURCE_KEYBOARD,
                                      time, (WPEModifiers)mods, keycode, keyval));
    if (from_repeat)
        return;
    if (!pressed) {
        if (seat->repeat.key == keycode)
            stop_repeat(seat);
        return;
    }
    if (!xkb_keymap_key_repeats(keymap, keycode) || seat->repeat.rate == 0)
        return;
    stop_repeat(seat);
    seat->repeat.key = keycode;
    int delay = seat->repeat.delay > 0 ? seat->repeat.delay : 400;
    guint id = g_timeout_add(delay, repeat_delay_cb, display);
    seat->repeat.source = g_main_context_find_source_by_id(NULL, id);
}

static void
keyboard_keymap(void *data, struct wl_keyboard *keyboard, uint32_t format, int32_t fd, uint32_t size)
{
    LoongDisplay *display = data;
    wpe_keymap_xkb_update(WPE_KEYMAP_XKB(display->seat.keymap), format, fd, size);
}

static void
keyboard_enter(void *data, struct wl_keyboard *keyboard, uint32_t serial, struct wl_surface *surface, struct wl_array *keys)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    double ox, oy;
    LoongToplevel *toplevel = surface_origin(surface, &ox, &oy);
    if (!toplevel)
        return;
    if (seat->keyboard_toplevel && seat->keyboard_toplevel != toplevel)
        loong_toplevel_keyboard_leave(seat->keyboard_toplevel);
    g_set_weak_pointer(&seat->keyboard_toplevel, toplevel);
    loong_toplevel_keyboard_enter(toplevel);
}

static void
keyboard_leave(void *data, struct wl_keyboard *keyboard, uint32_t serial, struct wl_surface *surface)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    stop_repeat(seat);
    if (seat->keyboard_toplevel)
        loong_toplevel_keyboard_leave(seat->keyboard_toplevel);
    g_clear_weak_pointer(&seat->keyboard_toplevel);
}

static void
keyboard_key(void *data, struct wl_keyboard *keyboard, uint32_t serial, uint32_t time, uint32_t key, uint32_t state)
{
    LoongDisplay *display = data;
    if (!display->seat.keyboard_toplevel)
        return;
    handle_key(display, time, key + 8, state == WL_KEYBOARD_KEY_STATE_PRESSED, FALSE);
}

static void
keyboard_modifiers(void *data, struct wl_keyboard *keyboard, uint32_t serial, uint32_t depressed, uint32_t latched, uint32_t locked,
                   uint32_t group)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    struct xkb_state *state = wpe_keymap_xkb_get_xkb_state(WPE_KEYMAP_XKB(seat->keymap));
    if (!state)
        return;
    xkb_state_update_mask(state, depressed, latched, locked, 0, 0, group);
    seat->keyboard_modifiers = wpe_keymap_get_modifiers(seat->keymap);
}

static void
keyboard_repeat_info(void *data, struct wl_keyboard *keyboard, int32_t rate, int32_t delay)
{
    LoongSeat *seat = &((LoongDisplay *)data)->seat;
    seat->repeat.rate = rate;
    seat->repeat.delay = delay;
}

static const struct wl_keyboard_listener keyboard_listener = {
    .keymap = keyboard_keymap,
    .enter = keyboard_enter,
    .leave = keyboard_leave,
    .key = keyboard_key,
    .modifiers = keyboard_modifiers,
    .repeat_info = keyboard_repeat_info,
};

/* ───────────────────────── 触摸 ───────────────────────── */

static void
touch_point_free(gpointer data)
{
    LoongTouchPoint *point = data;
    g_weak_ref_clear(&point->view);
    g_free(point);
}

static void
touch_down(void *data, struct wl_touch *touch, uint32_t serial, uint32_t time, struct wl_surface *surface, int32_t id, wl_fixed_t fx,
           wl_fixed_t fy)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    double sx = wl_fixed_to_double(fx), sy = wl_fixed_to_double(fy), vx, vy;
    LoongToplevel *toplevel = NULL;
    WPEView *view = resolve_target(surface, sx, sy, &vx, &vy, &toplevel);
    if (!view)
        return;
    LoongTouchPoint *point = g_new0(LoongTouchPoint, 1);
    g_weak_ref_init(&point->view, view);
    point->dx = vx - sx;
    point->dy = vy - sy;
    point->x = vx;
    point->y = vy;
    g_hash_table_replace(seat->touch_points, GINT_TO_POINTER(id), point);
    if (toplevel)
        loong_toplevel_set_focused_view(toplevel, view);
    g_debug("触摸按下 #%d %s表面 (%.0f, %.0f) → view %p (%.0f, %.0f)", id, LOONG_IS_TOPLEVEL(loong_surface_owner(surface)) ? "根" : "子", sx,
            sy, (void *)view, vx, vy);
    emit(view, wpe_event_touch_new(WPE_EVENT_TOUCH_DOWN, view, WPE_INPUT_SOURCE_TOUCHSCREEN, time, loong_seat_modifiers(display), id, vx, vy));
}

static void
touch_up(void *data, struct wl_touch *touch, uint32_t serial, uint32_t time, int32_t id)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    LoongTouchPoint *point = g_hash_table_lookup(seat->touch_points, GINT_TO_POINTER(id));
    if (!point)
        return;
    g_autoptr(WPEView) view = g_weak_ref_get(&point->view);
    if (view)
        emit(view, wpe_event_touch_new(WPE_EVENT_TOUCH_UP, view, WPE_INPUT_SOURCE_TOUCHSCREEN, time, loong_seat_modifiers(display), id, point->x,
                                       point->y));
    g_hash_table_remove(seat->touch_points, GINT_TO_POINTER(id));
}

static void
touch_motion(void *data, struct wl_touch *touch, uint32_t time, int32_t id, wl_fixed_t fx, wl_fixed_t fy)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    LoongTouchPoint *point = g_hash_table_lookup(seat->touch_points, GINT_TO_POINTER(id));
    if (!point)
        return;
    point->x = wl_fixed_to_double(fx) + point->dx;
    point->y = wl_fixed_to_double(fy) + point->dy;
    g_autoptr(WPEView) view = g_weak_ref_get(&point->view);
    if (view)
        emit(view, wpe_event_touch_new(WPE_EVENT_TOUCH_MOVE, view, WPE_INPUT_SOURCE_TOUCHSCREEN, time, loong_seat_modifiers(display), id, point->x,
                                       point->y));
}

static void
touch_frame(void *data, struct wl_touch *touch)
{
}

static void
cancel_touch_points(LoongDisplay *display, WPEView *only)
{
    LoongSeat *seat = &display->seat;
    GHashTableIter iter;
    gpointer key, value;
    g_hash_table_iter_init(&iter, seat->touch_points);
    while (g_hash_table_iter_next(&iter, &key, &value)) {
        LoongTouchPoint *point = value;
        g_autoptr(WPEView) view = g_weak_ref_get(&point->view);
        if (only && view != only)
            continue;
        if (view)
            emit(view, wpe_event_touch_new(WPE_EVENT_TOUCH_CANCEL, view, WPE_INPUT_SOURCE_TOUCHSCREEN, 0, loong_seat_modifiers(display),
                                           GPOINTER_TO_INT(key), point->x, point->y));
        g_hash_table_iter_remove(&iter);
    }
}

static void
touch_cancel(void *data, struct wl_touch *touch)
{
    cancel_touch_points(data, NULL);
}

static void
touch_shape(void *data, struct wl_touch *touch, int32_t id, wl_fixed_t major, wl_fixed_t minor)
{
}

static void
touch_orientation(void *data, struct wl_touch *touch, int32_t id, wl_fixed_t orientation)
{
}

static const struct wl_touch_listener touch_listener = {
    .down = touch_down,
    .up = touch_up,
    .motion = touch_motion,
    .frame = touch_frame,
    .cancel = touch_cancel,
    .shape = touch_shape,
    .orientation = touch_orientation,
};

/* ───────────────────────── seat ───────────────────────── */

static void
seat_capabilities(void *data, struct wl_seat *wl_seat, uint32_t caps)
{
    LoongDisplay *display = data;
    LoongSeat *seat = &display->seat;
    seat->capabilities = caps;
    g_debug("seat 能力：指针=%d 键盘=%d 触摸=%d", !!(caps & WL_SEAT_CAPABILITY_POINTER), !!(caps & WL_SEAT_CAPABILITY_KEYBOARD),
            !!(caps & WL_SEAT_CAPABILITY_TOUCH));

    if ((caps & WL_SEAT_CAPABILITY_POINTER) && !seat->pointer) {
        seat->pointer = wl_seat_get_pointer(wl_seat);
        wl_pointer_add_listener(seat->pointer, &pointer_listener, display);
    } else if (!(caps & WL_SEAT_CAPABILITY_POINTER) && seat->pointer) {
        set_pointer_view(display, NULL, 0, 0);
        seat->pointer_surface = NULL;
        g_clear_weak_pointer(&seat->grab_view);
        wl_pointer_release(seat->pointer);
        seat->pointer = NULL;
    }

    if ((caps & WL_SEAT_CAPABILITY_KEYBOARD) && !seat->keyboard) {
        seat->keyboard = wl_seat_get_keyboard(wl_seat);
        wl_keyboard_add_listener(seat->keyboard, &keyboard_listener, display);
    } else if (!(caps & WL_SEAT_CAPABILITY_KEYBOARD) && seat->keyboard) {
        stop_repeat(seat);
        wl_keyboard_release(seat->keyboard);
        seat->keyboard = NULL;
    }

    if ((caps & WL_SEAT_CAPABILITY_TOUCH) && !seat->touch) {
        seat->touch = wl_seat_get_touch(wl_seat);
        wl_touch_add_listener(seat->touch, &touch_listener, display);
    } else if (!(caps & WL_SEAT_CAPABILITY_TOUCH) && seat->touch) {
        cancel_touch_points(display, NULL);
        wl_touch_release(seat->touch);
        seat->touch = NULL;
    }

    guint devices = WPE_AVAILABLE_INPUT_DEVICE_NONE;
    if (seat->pointer)
        devices |= WPE_AVAILABLE_INPUT_DEVICE_MOUSE;
    if (seat->keyboard)
        devices |= WPE_AVAILABLE_INPUT_DEVICE_KEYBOARD;
    if (seat->touch)
        devices |= WPE_AVAILABLE_INPUT_DEVICE_TOUCHSCREEN;
    wpe_display_set_available_input_devices(WPE_DISPLAY(display), (WPEAvailableInputDevices)devices);
}

static void
seat_name(void *data, struct wl_seat *wl_seat, const char *name)
{
}

static const struct wl_seat_listener seat_listener = {
    .capabilities = seat_capabilities,
    .name = seat_name,
};

void
loong_seat_init(LoongDisplay *display, struct wl_seat *wl_seat)
{
    LoongSeat *seat = &display->seat;
    seat->wl_seat = wl_seat;
    seat->keymap = wpe_keymap_xkb_new();
    seat->touch_points = g_hash_table_new_full(NULL, NULL, NULL, touch_point_free);
    seat->repeat.rate = -1;
    seat->scroll.source = WPE_INPUT_SOURCE_MOUSE;
    wl_seat_add_listener(wl_seat, &seat_listener, display);
}

void
loong_seat_finalize(LoongDisplay *display)
{
    LoongSeat *seat = &display->seat;
    stop_repeat(seat);
    g_clear_weak_pointer(&seat->pointer_view);
    g_clear_weak_pointer(&seat->grab_view);
    g_clear_weak_pointer(&seat->pointer_toplevel);
    g_clear_weak_pointer(&seat->keyboard_toplevel);
    g_clear_pointer(&seat->touch_points, g_hash_table_unref);
    g_clear_pointer(&seat->pointer, wl_pointer_destroy);
    g_clear_pointer(&seat->keyboard, wl_keyboard_destroy);
    g_clear_pointer(&seat->touch, wl_touch_destroy);
    g_clear_pointer(&seat->wl_seat, wl_seat_destroy);
    g_clear_object(&seat->keymap);
}

void
loong_seat_view_gone(LoongDisplay *display, WPEView *view)
{
    LoongSeat *seat = &display->seat;
    if (!seat->wl_seat)
        return;
    if (seat->grab_view == view)
        g_clear_weak_pointer(&seat->grab_view);
    if (seat->pointer_view == view)
        g_clear_weak_pointer(&seat->pointer_view);
    if (seat->touch_points)
        cancel_touch_points(display, view);
}
