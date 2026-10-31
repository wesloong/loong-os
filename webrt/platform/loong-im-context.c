/*
 * LoongIMContext：基于 text-input-unstable-v3 的输入法上下文（对接 loong-shell 内置的
 * Rime 屏幕键盘）。语义与 WPE 自带的 WPEInputMethodContextWaylandV3 相同，区别是光标矩形
 * 要从 view 坐标换算到 text-input 进入的那个表面（根表面或 view 的子表面）。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include "loong-platform-private.h"

#include <string.h>

#define LOONG_TYPE_IM_CONTEXT (loong_im_context_get_type())
G_DECLARE_FINAL_TYPE(LoongIMContext, loong_im_context, LOONG, IM_CONTEXT, WPEInputMethodContext)

typedef struct {
    char *text;
    int cursor_begin, cursor_end;
} Preedit;

struct _LoongIMContext {
    WPEInputMethodContext parent_instance;
    Preedit pending_preedit;
    Preedit preedit;
    char *pending_commit;
    struct {
        int x, y, width, height;
    } cursor;
    struct {
        char *text;
        guint cursor, anchor;
    } surrounding;
    enum zwp_text_input_v3_change_cause cause;
    guint32 delete_before, delete_after;
};

G_DEFINE_FINAL_TYPE(LoongIMContext, loong_im_context, WPE_TYPE_INPUT_METHOD_CONTEXT)

/* 每个显示一份：text_input 对象是 seat 级别的，同一时刻只服务一个上下文。 */
typedef struct {
    struct zwp_text_input_v3 *text_input;
    LoongIMContext *current;
    struct wl_surface *surface; /* enter 的表面 */
    guint32 serial;
} IMState;

static IMState *im_state(LoongDisplay *display);

static IMState *
state_for(LoongIMContext *self)
{
    WPEDisplay *display = wpe_input_method_context_get_display(WPE_INPUT_METHOD_CONTEXT(self));
    if (!display || !LOONG_IS_DISPLAY(display))
        return NULL;
    IMState *state = im_state(LOONG_DISPLAY(display));
    if (!state->text_input || state->current != self)
        return NULL;
    return state;
}

static guint32
to_hints(WPEInputHints hints, WPEInputPurpose purpose)
{
    guint32 out = 0;
    if (hints & WPE_INPUT_HINT_SPELLCHECK)
        out |= ZWP_TEXT_INPUT_V3_CONTENT_HINT_SPELLCHECK;
    if (hints & WPE_INPUT_HINT_WORD_COMPLETION)
        out |= ZWP_TEXT_INPUT_V3_CONTENT_HINT_COMPLETION;
    if (hints & WPE_INPUT_HINT_LOWERCASE)
        out |= ZWP_TEXT_INPUT_V3_CONTENT_HINT_LOWERCASE;
    if (hints & WPE_INPUT_HINT_UPPERCASE_CHARS)
        out |= ZWP_TEXT_INPUT_V3_CONTENT_HINT_UPPERCASE;
    if (hints & WPE_INPUT_HINT_UPPERCASE_WORDS)
        out |= ZWP_TEXT_INPUT_V3_CONTENT_HINT_TITLECASE;
    if (hints & WPE_INPUT_HINT_UPPERCASE_SENTENCES)
        out |= ZWP_TEXT_INPUT_V3_CONTENT_HINT_AUTO_CAPITALIZATION;
    if (purpose == WPE_INPUT_PURPOSE_PIN || purpose == WPE_INPUT_PURPOSE_PASSWORD)
        out |= ZWP_TEXT_INPUT_V3_CONTENT_HINT_HIDDEN_TEXT | ZWP_TEXT_INPUT_V3_CONTENT_HINT_SENSITIVE_DATA;
    return out;
}

static guint32
to_purpose(WPEInputPurpose purpose)
{
    switch (purpose) {
    case WPE_INPUT_PURPOSE_ALPHA:
        return ZWP_TEXT_INPUT_V3_CONTENT_PURPOSE_ALPHA;
    case WPE_INPUT_PURPOSE_DIGITS:
        return ZWP_TEXT_INPUT_V3_CONTENT_PURPOSE_DIGITS;
    case WPE_INPUT_PURPOSE_NUMBER:
        return ZWP_TEXT_INPUT_V3_CONTENT_PURPOSE_NUMBER;
    case WPE_INPUT_PURPOSE_PHONE:
        return ZWP_TEXT_INPUT_V3_CONTENT_PURPOSE_PHONE;
    case WPE_INPUT_PURPOSE_URL:
        return ZWP_TEXT_INPUT_V3_CONTENT_PURPOSE_URL;
    case WPE_INPUT_PURPOSE_EMAIL:
        return ZWP_TEXT_INPUT_V3_CONTENT_PURPOSE_EMAIL;
    case WPE_INPUT_PURPOSE_NAME:
        return ZWP_TEXT_INPUT_V3_CONTENT_PURPOSE_NAME;
    case WPE_INPUT_PURPOSE_PASSWORD:
        return ZWP_TEXT_INPUT_V3_CONTENT_PURPOSE_PASSWORD;
    case WPE_INPUT_PURPOSE_PIN:
        return ZWP_TEXT_INPUT_V3_CONTENT_PURPOSE_PIN;
    case WPE_INPUT_PURPOSE_TERMINAL:
        return ZWP_TEXT_INPUT_V3_CONTENT_PURPOSE_TERMINAL;
    default:
        return ZWP_TEXT_INPUT_V3_CONTENT_PURPOSE_NORMAL;
    }
}

static void
send_cursor_rect(LoongIMContext *self, IMState *state)
{
    /* view 坐标 → enter 表面的坐标。 */
    int x = self->cursor.x, y = self->cursor.y;
    WPEView *view = wpe_input_method_context_get_view(WPE_INPUT_METHOD_CONTEXT(self));
    GObject *owner = loong_surface_owner(state->surface);
    if (view && owner && LOONG_IS_TOPLEVEL(owner)) {
        int vx, vy;
        loong_view_get_position(LOONG_VIEW(view), &vx, &vy);
        x += vx;
        y += vy;
    }
    zwp_text_input_v3_set_cursor_rectangle(state->text_input, x, y, self->cursor.width, self->cursor.height);
}

static void
send_surrounding(LoongIMContext *self, IMState *state)
{
    if (!self->surrounding.text)
        return;
    /* 协议限制约 4000 字节：太长时只发光标附近（按 UTF-8 字符边界截取）。 */
    const char *text = self->surrounding.text;
    size_t len = strlen(text);
    guint cursor = self->surrounding.cursor, anchor = self->surrounding.anchor;
    g_autofree char *slice = NULL;
    if (len >= 4000) {
        size_t center = MIN(cursor, len);
        size_t start = center > 2000 ? center - 2000 : 0;
        size_t end = MIN(len, start + 3990);
        const char *s = g_utf8_find_next_char(text + start - (start ? 1 : 0), NULL);
        if (!start)
            s = text;
        const char *e = end < len ? g_utf8_find_prev_char(text, text + end) : text + len;
        slice = g_strndup(s, e - s);
        cursor = CLAMP((long)cursor - (s - text), 0, (long)(e - s));
        anchor = CLAMP((long)anchor - (s - text), 0, (long)(e - s));
        text = slice;
    }
    zwp_text_input_v3_set_surrounding_text(state->text_input, text, cursor, anchor);
    zwp_text_input_v3_set_text_change_cause(state->text_input, self->cause);
}

static void
send_content_type(LoongIMContext *self, IMState *state)
{
    WPEInputMethodContext *context = WPE_INPUT_METHOD_CONTEXT(self);
    zwp_text_input_v3_set_content_type(state->text_input,
                                       to_hints(wpe_input_method_context_get_input_hints(context),
                                                wpe_input_method_context_get_input_purpose(context)),
                                       to_purpose(wpe_input_method_context_get_input_purpose(context)));
}

static void
commit_state(LoongIMContext *self, IMState *state)
{
    state->serial++;
    zwp_text_input_v3_commit(state->text_input);
    self->cause = ZWP_TEXT_INPUT_V3_CHANGE_CAUSE_INPUT_METHOD;
}

static void
update_state(LoongIMContext *self, enum zwp_text_input_v3_change_cause cause)
{
    IMState *state = state_for(self);
    if (!state)
        return;
    self->cause = cause;
    send_surrounding(self, state);
    send_content_type(self, state);
    send_cursor_rect(self, state);
    commit_state(self, state);
}

static void
enable(LoongIMContext *self, IMState *state)
{
    zwp_text_input_v3_enable(state->text_input);
    update_state(self, ZWP_TEXT_INPUT_V3_CHANGE_CAUSE_OTHER);
}

static void
disable(LoongIMContext *self, IMState *state)
{
    zwp_text_input_v3_disable(state->text_input);
    commit_state(self, state);
}

/* ───────────────────────── text_input 事件 ───────────────────────── */

static void
ti_enter(void *data, struct zwp_text_input_v3 *text_input, struct wl_surface *surface)
{
    IMState *state = data;
    state->surface = surface;
    if (state->current)
        enable(state->current, state);
}

static void
ti_leave(void *data, struct zwp_text_input_v3 *text_input, struct wl_surface *surface)
{
    IMState *state = data;
    if (state->current && state->surface)
        disable(state->current, state);
    state->surface = NULL;
}

static void
ti_preedit_string(void *data, struct zwp_text_input_v3 *text_input, const char *text, int32_t begin, int32_t end)
{
    IMState *state = data;
    if (!state->current)
        return;
    Preedit *p = &state->current->pending_preedit;
    g_free(p->text);
    p->text = g_strdup(text);
    p->cursor_begin = begin;
    p->cursor_end = end;
}

static void
ti_commit_string(void *data, struct zwp_text_input_v3 *text_input, const char *text)
{
    IMState *state = data;
    if (!state->current)
        return;
    g_free(state->current->pending_commit);
    state->current->pending_commit = g_strdup(text);
}

static void
ti_delete_surrounding_text(void *data, struct zwp_text_input_v3 *text_input, uint32_t before, uint32_t after)
{
    IMState *state = data;
    if (!state->current)
        return;
    state->current->delete_before = before;
    state->current->delete_after = after;
}

static void
ti_done(void *data, struct zwp_text_input_v3 *text_input, uint32_t serial)
{
    IMState *state = data;
    LoongIMContext *self = state->current;
    if (!self)
        return;
    gboolean valid = state->serial == serial;
    gboolean preedit_changed = g_strcmp0(self->pending_preedit.text, self->preedit.text) != 0;
    gboolean has_commit = self->pending_commit != NULL;

    /* 顺序按协议：先删周围文字，再插入提交的文字，最后更新预编辑。 */
    if (valid && (self->delete_before || self->delete_after))
        g_signal_emit_by_name(self, "delete-surrounding", -(int)self->delete_before, self->delete_before + self->delete_after);
    self->delete_before = self->delete_after = 0;

    if (valid && self->pending_commit)
        g_signal_emit_by_name(self, "committed", self->pending_commit);
    g_clear_pointer(&self->pending_commit, g_free);

    gboolean had = self->preedit.text != NULL, has = self->pending_preedit.text != NULL;
    if (valid && !had && has)
        g_signal_emit_by_name(self, "preedit-started");
    g_free(self->preedit.text);
    self->preedit = self->pending_preedit;
    memset(&self->pending_preedit, 0, sizeof(self->pending_preedit));
    if (valid)
        g_signal_emit_by_name(self, "preedit-changed");
    if (valid && had && !has)
        g_signal_emit_by_name(self, "preedit-finished");

    if (valid && (preedit_changed || has_commit))
        update_state(self, ZWP_TEXT_INPUT_V3_CHANGE_CAUSE_INPUT_METHOD);
}

static void
ti_action(void *data, struct zwp_text_input_v3 *text_input, uint32_t action, uint32_t serial)
{
}

static void
ti_language(void *data, struct zwp_text_input_v3 *text_input, const char *language)
{
}

static void
ti_preedit_hint(void *data, struct zwp_text_input_v3 *text_input, uint32_t start, uint32_t end, uint32_t hint)
{
}

static const struct zwp_text_input_v3_listener text_input_listener = {
    .enter = ti_enter,
    .leave = ti_leave,
    .preedit_string = ti_preedit_string,
    .commit_string = ti_commit_string,
    .delete_surrounding_text = ti_delete_surrounding_text,
    .done = ti_done,
#ifdef ZWP_TEXT_INPUT_V3_ACTION_SINCE_VERSION
    .action = ti_action,
#endif
#ifdef ZWP_TEXT_INPUT_V3_LANGUAGE_SINCE_VERSION
    .language = ti_language,
#endif
#ifdef ZWP_TEXT_INPUT_V3_PREEDIT_HINT_SINCE_VERSION
    .preedit_hint = ti_preedit_hint,
#endif
};

static IMState *
im_state(LoongDisplay *display)
{
    IMState *state = g_object_get_data(G_OBJECT(display), "loong-im-state");
    if (state)
        return state;
    state = g_new0(IMState, 1);
    state->text_input = display->text_input;
    if (state->text_input)
        zwp_text_input_v3_add_listener(state->text_input, &text_input_listener, state);
    g_object_set_data_full(G_OBJECT(display), "loong-im-state", state, g_free);
    return state;
}

/* ───────────────────────── WPEInputMethodContext 虚函数 ───────────────────────── */

/* text-input-v3 的预编辑光标以字节计，WPE 以字符计。 */
static guint
char_offset(const char *text, int byte_offset)
{
    size_t len = strlen(text);
    size_t clamped = CLAMP(byte_offset, 0, (int)len);
    return g_utf8_pointer_to_offset(text, text + clamped);
}

static void
loong_im_context_get_preedit_string(WPEInputMethodContext *context, char **text, GList **underlines, guint *cursor_offset)
{
    LoongIMContext *self = LOONG_IM_CONTEXT(context);
    const char *preedit = self->preedit.text ? self->preedit.text : "";
    if (text)
        *text = g_strdup(preedit);
    guint begin, end;
    if (self->preedit.cursor_begin == -1 && self->preedit.cursor_end == -1) {
        begin = end = g_utf8_strlen(preedit, -1);
    } else {
        begin = char_offset(preedit, self->preedit.cursor_begin);
        end = char_offset(preedit, self->preedit.cursor_end);
    }
    if (underlines) {
        *underlines = NULL;
        if (begin != end)
            *underlines = g_list_prepend(NULL, wpe_input_method_underline_new(MIN(begin, end), MAX(begin, end)));
    }
    if (cursor_offset)
        *cursor_offset = begin;
}

static void
loong_im_context_focus_in(WPEInputMethodContext *context)
{
    LoongIMContext *self = LOONG_IM_CONTEXT(context);
    WPEDisplay *display = wpe_input_method_context_get_display(context);
    if (!display || !LOONG_IS_DISPLAY(display) || !wpe_input_method_context_get_view(context))
        return;
    IMState *state = im_state(LOONG_DISPLAY(display));
    if (state->current == self)
        return;
    state->current = self;
    if (state->text_input && state->surface)
        enable(self, state);
}

static void
loong_im_context_focus_out(WPEInputMethodContext *context)
{
    LoongIMContext *self = LOONG_IM_CONTEXT(context);
    IMState *state = state_for(self);
    if (!state)
        return;
    if (state->surface)
        disable(self, state);
    state->current = NULL;
}

static void
loong_im_context_set_cursor_area(WPEInputMethodContext *context, int x, int y, int width, int height)
{
    LoongIMContext *self = LOONG_IM_CONTEXT(context);
    if (self->cursor.x == x && self->cursor.y == y && self->cursor.width == width && self->cursor.height == height)
        return;
    self->cursor.x = x;
    self->cursor.y = y;
    self->cursor.width = width;
    self->cursor.height = height;
    IMState *state = state_for(self);
    if (state) {
        send_cursor_rect(self, state);
        commit_state(self, state);
    }
}

static void
loong_im_context_set_surrounding(WPEInputMethodContext *context, const char *text, guint length, guint cursor, guint selection)
{
    LoongIMContext *self = LOONG_IM_CONTEXT(context);
    g_free(self->surrounding.text);
    self->surrounding.text = g_strndup(text, length);
    self->surrounding.cursor = cursor;
    self->surrounding.anchor = selection;
    IMState *state = state_for(self);
    if (state) {
        send_surrounding(self, state);
        commit_state(self, state);
    }
}

static void
loong_im_context_reset(WPEInputMethodContext *context)
{
    update_state(LOONG_IM_CONTEXT(context), ZWP_TEXT_INPUT_V3_CHANGE_CAUSE_OTHER);
}

static void
purpose_or_hints_changed(LoongIMContext *self)
{
    IMState *state = state_for(self);
    if (!state)
        return;
    send_content_type(self, state);
    commit_state(self, state);
}

static void
loong_im_context_constructed(GObject *object)
{
    G_OBJECT_CLASS(loong_im_context_parent_class)->constructed(object);
    g_signal_connect(object, "notify::input-purpose", G_CALLBACK(purpose_or_hints_changed), NULL);
    g_signal_connect(object, "notify::input-hints", G_CALLBACK(purpose_or_hints_changed), NULL);
}

static void
loong_im_context_dispose(GObject *object)
{
    LoongIMContext *self = LOONG_IM_CONTEXT(object);
    loong_im_context_focus_out(WPE_INPUT_METHOD_CONTEXT(self));
    WPEDisplay *display = wpe_input_method_context_get_display(WPE_INPUT_METHOD_CONTEXT(self));
    if (display && LOONG_IS_DISPLAY(display)) {
        IMState *state = im_state(LOONG_DISPLAY(display));
        if (state->current == self)
            state->current = NULL;
    }
    g_clear_pointer(&self->pending_preedit.text, g_free);
    g_clear_pointer(&self->preedit.text, g_free);
    g_clear_pointer(&self->pending_commit, g_free);
    g_clear_pointer(&self->surrounding.text, g_free);
    G_OBJECT_CLASS(loong_im_context_parent_class)->dispose(object);
}

static void
loong_im_context_init(LoongIMContext *self)
{
    self->cause = ZWP_TEXT_INPUT_V3_CHANGE_CAUSE_INPUT_METHOD;
}

static void
loong_im_context_class_init(LoongIMContextClass *klass)
{
    GObjectClass *object_class = G_OBJECT_CLASS(klass);
    object_class->constructed = loong_im_context_constructed;
    object_class->dispose = loong_im_context_dispose;
    WPEInputMethodContextClass *im_class = WPE_INPUT_METHOD_CONTEXT_CLASS(klass);
    im_class->get_preedit_string = loong_im_context_get_preedit_string;
    im_class->focus_in = loong_im_context_focus_in;
    im_class->focus_out = loong_im_context_focus_out;
    im_class->set_cursor_area = loong_im_context_set_cursor_area;
    im_class->set_surrounding = loong_im_context_set_surrounding;
    im_class->reset = loong_im_context_reset;
}

void
loong_im_surface_destroyed(LoongDisplay *display, struct wl_surface *surface)
{
    IMState *state = g_object_get_data(G_OBJECT(display), "loong-im-state");
    if (state && state->surface == surface)
        state->surface = NULL;
}

WPEInputMethodContext *
loong_im_context_new(LoongDisplay *display, WPEView *view)
{
    if (!display->text_input)
        return NULL;
    im_state(display);
    return g_object_new(LOONG_TYPE_IM_CONTEXT, "view", view, NULL);
}
