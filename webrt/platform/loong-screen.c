/*
 * LoongScreen：一个 wl_output。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include "loong-platform-private.h"

G_DEFINE_FINAL_TYPE(LoongScreen, loong_screen, WPE_TYPE_SCREEN)

static void
loong_screen_invalidate(WPEScreen *screen)
{
    LoongScreen *self = LOONG_SCREEN(screen);
    if (self->wl_output) {
        if (wl_output_get_version(self->wl_output) >= WL_OUTPUT_RELEASE_SINCE_VERSION)
            wl_output_release(self->wl_output);
        else
            wl_output_destroy(self->wl_output);
        self->wl_output = NULL;
    }
    WPE_SCREEN_CLASS(loong_screen_parent_class)->invalidate(screen);
}

static void
loong_screen_dispose(GObject *object)
{
    loong_screen_invalidate(WPE_SCREEN(object));
    G_OBJECT_CLASS(loong_screen_parent_class)->dispose(object);
}

static void
loong_screen_init(LoongScreen *self)
{
    self->pending.scale = 1;
}

static void
loong_screen_class_init(LoongScreenClass *klass)
{
    G_OBJECT_CLASS(klass)->dispose = loong_screen_dispose;
    WPE_SCREEN_CLASS(klass)->invalidate = loong_screen_invalidate;
}

static void
output_geometry(void *data, struct wl_output *output, int32_t x, int32_t y, int32_t physical_width,
                int32_t physical_height, int32_t subpixel, const char *make, const char *model, int32_t transform)
{
    LoongScreen *self = data;
    self->pending.x = x;
    self->pending.y = y;
    switch (transform) {
    case WL_OUTPUT_TRANSFORM_90:
    case WL_OUTPUT_TRANSFORM_270:
    case WL_OUTPUT_TRANSFORM_FLIPPED_90:
    case WL_OUTPUT_TRANSFORM_FLIPPED_270:
        wpe_screen_set_physical_size(WPE_SCREEN(self), physical_height, physical_width);
        break;
    default:
        wpe_screen_set_physical_size(WPE_SCREEN(self), physical_width, physical_height);
        break;
    }
}

static void
output_mode(void *data, struct wl_output *output, uint32_t flags, int32_t width, int32_t height, int32_t refresh)
{
    LoongScreen *self = data;
    if (!(flags & WL_OUTPUT_MODE_CURRENT))
        return;
    self->pending.width = width;
    self->pending.height = height;
    wpe_screen_set_refresh_rate(WPE_SCREEN(self), refresh);
}

static void
output_done(void *data, struct wl_output *output)
{
    LoongScreen *self = data;
    int scale = MAX(self->pending.scale, 1);
    wpe_screen_set_position(WPE_SCREEN(self), self->pending.x / scale, self->pending.y / scale);
    wpe_screen_set_size(WPE_SCREEN(self), self->pending.width / scale, self->pending.height / scale);
    wpe_screen_set_scale(WPE_SCREEN(self), scale);
}

static void
output_scale(void *data, struct wl_output *output, int32_t factor)
{
    LoongScreen *self = data;
    self->pending.scale = factor;
}

static void
output_name(void *data, struct wl_output *output, const char *name)
{
}

static void
output_description(void *data, struct wl_output *output, const char *description)
{
}

static const struct wl_output_listener output_listener = {
    .geometry = output_geometry,
    .mode = output_mode,
    .done = output_done,
    .scale = output_scale,
    .name = output_name,
    .description = output_description,
};

LoongScreen *
loong_screen_new(guint32 name, struct wl_output *output)
{
    LoongScreen *self = g_object_new(LOONG_TYPE_SCREEN, "id", name, NULL);
    self->name = name;
    self->wl_output = output;
    wl_output_add_listener(output, &output_listener, self);
    return self;
}
