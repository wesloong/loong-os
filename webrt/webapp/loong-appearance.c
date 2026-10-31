/*
 * loong-appearance：读取、监视、写入系统外观文件。见 loong-appearance.h。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include "loong-appearance.h"

#include <gio/gio.h>

#include "loong-bridge-core.h"

struct _LoongAppearance {
    GObject parent;
    char *path;
    const char *theme; /* 静态字符串 "light" / "dark" */
    GFileMonitor *monitor;
};

G_DEFINE_FINAL_TYPE(LoongAppearance, loong_appearance, G_TYPE_OBJECT)

enum { PROP_0, PROP_THEME, N_PROPS };
static GParamSpec *props[N_PROPS];

char *
loong_appearance_path(void)
{
    return g_build_filename(g_get_user_config_dir(), "loong", "appearance.toml", NULL);
}

static void
reload(LoongAppearance *self)
{
    g_autofree char *text = NULL;
    /* 文件不存在 = 默认外观（浅色）。 */
    g_file_get_contents(self->path, &text, NULL, NULL);
    const char *theme = loong_appearance_parse(text);
    if (g_strcmp0(theme, self->theme) != 0) {
        self->theme = theme;
        g_object_notify_by_pspec(G_OBJECT(self), props[PROP_THEME]);
    }
}

static void
on_changed(GFileMonitor *monitor, GFile *file, GFile *other, GFileMonitorEvent event, LoongAppearance *self)
{
    switch (event) {
    case G_FILE_MONITOR_EVENT_CHANGES_DONE_HINT:
    case G_FILE_MONITOR_EVENT_CREATED:
    case G_FILE_MONITOR_EVENT_DELETED:
    case G_FILE_MONITOR_EVENT_MOVED_IN:
    case G_FILE_MONITOR_EVENT_RENAMED:
        reload(self);
        break;
    default:
        break;
    }
}

static void
loong_appearance_finalize(GObject *object)
{
    LoongAppearance *self = LOONG_APPEARANCE(object);
    if (self->monitor) {
        g_file_monitor_cancel(self->monitor);
        g_object_unref(self->monitor);
    }
    g_free(self->path);
    G_OBJECT_CLASS(loong_appearance_parent_class)->finalize(object);
}

static void
loong_appearance_get_property(GObject *object, guint id, GValue *value, GParamSpec *pspec)
{
    LoongAppearance *self = LOONG_APPEARANCE(object);
    switch (id) {
    case PROP_THEME:
        g_value_set_string(value, self->theme);
        break;
    default:
        G_OBJECT_WARN_INVALID_PROPERTY_ID(object, id, pspec);
    }
}

static void
loong_appearance_class_init(LoongAppearanceClass *klass)
{
    GObjectClass *object_class = G_OBJECT_CLASS(klass);
    object_class->finalize = loong_appearance_finalize;
    object_class->get_property = loong_appearance_get_property;
    props[PROP_THEME] = g_param_spec_string("theme", NULL, NULL, "light", G_PARAM_READABLE | G_PARAM_STATIC_STRINGS);
    g_object_class_install_properties(object_class, N_PROPS, props);
}

static void
loong_appearance_init(LoongAppearance *self)
{
    self->theme = "light";
    self->path = loong_appearance_path();
}

LoongAppearance *
loong_appearance_new(void)
{
    LoongAppearance *self = g_object_new(LOONG_TYPE_APPEARANCE, NULL);
    /* 先建好目录：监视一个不存在的目录只能靠轮询。 */
    g_autofree char *dir = g_path_get_dirname(self->path);
    g_mkdir_with_parents(dir, 0700);
    reload(self);

    g_autoptr(GFile) file = g_file_new_for_path(self->path);
    g_autoptr(GError) error = NULL;
    /* 写入方用“写临时文件 + rename”原子替换，WATCH_MOVES 让 rename 也能报告出来。 */
    self->monitor = g_file_monitor_file(file, G_FILE_MONITOR_WATCH_MOVES, NULL, &error);
    if (self->monitor) {
        g_file_monitor_set_rate_limit(self->monitor, 100);
        g_signal_connect(self->monitor, "changed", G_CALLBACK(on_changed), self);
    } else {
        g_warning("无法监视 %s：%s（外观只在启动时读取）", self->path, error->message);
    }
    return self;
}

const char *
loong_appearance_get_theme(LoongAppearance *self)
{
    g_return_val_if_fail(LOONG_IS_APPEARANCE(self), "light");
    return self->theme;
}

static void
write_in_thread(GTask *task, gpointer source, gpointer data, GCancellable *cancellable)
{
    LoongAppearance *self = source;
    g_autofree char *contents = loong_appearance_format(data);
    g_autofree char *dir = g_path_get_dirname(self->path);
    GError *error = NULL;
    if (g_mkdir_with_parents(dir, 0700) != 0) {
        g_task_return_new_error(task, G_IO_ERROR, G_IO_ERROR_FAILED, "无法创建 %s", dir);
        return;
    }
    if (!g_file_set_contents_full(self->path, contents, -1, G_FILE_SET_CONTENTS_CONSISTENT, 0644, &error)) {
        g_task_return_error(task, error);
        return;
    }
    g_task_return_boolean(task, TRUE);
}

static void
on_written(GObject *source, GAsyncResult *result, gpointer data)
{
    GTask *outer = data;
    GError *error = NULL;
    if (g_task_propagate_boolean(G_TASK(result), &error)) {
        /* 不等文件监视：本进程立即生效（监视随后到来时 reload 发现没有变化）。 */
        reload(LOONG_APPEARANCE(source));
        g_task_return_boolean(outer, TRUE);
    } else {
        g_task_return_error(outer, error);
    }
    g_object_unref(outer);
}

void
loong_appearance_set_theme_async(LoongAppearance *self, const char *theme, GCancellable *cancellable,
                                 GAsyncReadyCallback callback, gpointer user_data)
{
    g_return_if_fail(LOONG_IS_APPEARANCE(self));
    GTask *outer = g_task_new(self, cancellable, callback, user_data);
    if (!loong_appearance_valid(theme)) {
        g_task_return_new_error(outer, LOONG_BRIDGE_ERROR, LOONG_BRIDGE_ERR_INVALID_ARGS,
                                "外观只能是 light 或 dark");
        g_object_unref(outer);
        return;
    }
    g_autoptr(GTask) task = g_task_new(self, cancellable, on_written, outer);
    g_task_set_task_data(task, g_strdup(theme), g_free);
    g_task_run_in_thread(task, write_in_thread);
}

gboolean
loong_appearance_set_theme_finish(LoongAppearance *self, GAsyncResult *result, GError **error)
{
    return g_task_propagate_boolean(G_TASK(result), error);
}
