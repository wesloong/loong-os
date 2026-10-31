/*
 * loong-bridge：系统应用的 JS bridge。见 loong-bridge.h 与 docs/api/loong-js-bridge.md。
 *
 *   页面 JS ──postMessage──▶ on_message（UI 进程）──白名单 + 参数校验──▶ GDBus（异步）──▶ loong-sysd
 *      ▲                                                                                      │
 *      └──── Promise（{ok, value | error}）◀──── 回复 ◀──────────────────────────────────────┘
 *      └──── loong[Symbol.for('loong.bridge.dispatch')](json) ◀── PropertiesChanged 等信号 ──┘
 *
 * 所有 D-Bus 调用都是异步的，文件读写放在线程里，UI 线程从不阻塞。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include "loong-bridge.h"

#include <gio/gio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/utsname.h>

#include "loong-bridge-core.h"

#define HANDLER_NAME "loong"
#define SHIM_RESOURCE "/org/loong/webapp/loong-bridge.js"
#define DEVICE_FILE "/usr/share/loong/device.toml"

struct _LoongBridge {
    WebKitWebView *view; /* 不持有引用：bridge 随 view 一起销毁 */
    WebKitUserContentManager *ucm;
    char *root;        /* 受信任目录（realpath） */
    char *trusted_uri; /* 最近一次校验通过的页面地址（缓存） */
    LoongAppearance *appearance;
    gulong appearance_handler;
    gulong message_handler;
    GCancellable *cancellable;

    GDBusConnection *bus;
    gboolean connecting;
    GQueue waiting; /* 等总线连上的 Request */
    guint signal_id;
    guint watch_id;
    gulong closed_handler;
};

/* 一次页面请求。回复之前一直持有 reply 与 JS 上下文。 */
typedef struct {
    LoongBridge *bridge;
    WebKitScriptMessageReply *reply;
    JSCContext *ctx;
    const LoongBridgeInterface *iface;
    const LoongBridgeMethod *method; /* NULL = Properties.GetAll */
    GVariant *params;
} Request;

static Request *
request_new(LoongBridge *self, WebKitScriptMessageReply *reply, JSCContext *ctx)
{
    Request *r = g_new0(Request, 1);
    r->bridge = self;
    r->reply = webkit_script_message_reply_ref(reply);
    r->ctx = g_object_ref(ctx);
    return r;
}

static void
request_free(Request *r)
{
    webkit_script_message_reply_unref(r->reply);
    g_object_unref(r->ctx);
    g_clear_pointer(&r->params, g_variant_unref);
    g_free(r);
}

/* ───────────────────────── 回复 ───────────────────────── */

static void
reply_json(WebKitScriptMessageReply *reply, JSCContext *ctx, const char *json)
{
    g_autoptr(JSCValue) value = jsc_value_new_from_json(ctx, json);
    if (value)
        webkit_script_message_reply_return_value(reply, value);
    else
        webkit_script_message_reply_return_error_message(reply, "loong-bridge: 无法构造返回值");
}

static void
reply_value(Request *r, GVariant *value)
{
    g_autoptr(GString) json = g_string_new("{\"ok\":true,\"value\":");
    if (value)
        loong_bridge_json_variant(json, value);
    else
        g_string_append(json, "null");
    g_string_append_c(json, '}');
    reply_json(r->reply, r->ctx, json->str);
}

static void
reply_error(WebKitScriptMessageReply *reply, JSCContext *ctx, const char *name, const char *message)
{
    g_autoptr(GString) json = g_string_new("{\"ok\":false,\"error\":{\"name\":");
    loong_bridge_json_string(json, name);
    g_string_append(json, ",\"message\":");
    loong_bridge_json_string(json, message);
    g_string_append(json, "}}");
    reply_json(reply, ctx, json->str);
}

/* GError → {name, message}：D-Bus 错误保留原错误名（org.loong.System1.Error.* 等）。 */
static void
reply_gerror(Request *r, GError *error)
{
    if (error->domain == LOONG_BRIDGE_ERROR) {
        reply_error(r->reply, r->ctx, loong_bridge_error_name(error->code), error->message);
        return;
    }
    g_autofree char *remote = g_dbus_error_get_remote_error(error);
    if (remote) {
        g_dbus_error_strip_remote_error(error);
        reply_error(r->reply, r->ctx, remote, error->message);
    } else if (g_error_matches(error, G_IO_ERROR, G_IO_ERROR_TIMED_OUT)) {
        reply_error(r->reply, r->ctx, "org.freedesktop.DBus.Error.NoReply", "loong-sysd 没有及时回应");
    } else {
        reply_error(r->reply, r->ctx, LOONG_BRIDGE_ERROR_FAILED, error->message);
    }
}

/* ───────────────────────── 页面校验与事件 ───────────────────────── */

/* 当前页面是否仍在受信任目录中（页面可能已经跳走）。 */
static gboolean
page_trusted(LoongBridge *self)
{
    const char *uri = webkit_web_view_get_uri(self->view);
    if (!uri)
        return FALSE;
    if (self->trusted_uri && strcmp(self->trusted_uri, uri) == 0)
        return TRUE;
    if (!loong_bridge_url_trusted(uri, self->root))
        return FALSE;
    g_free(self->trusted_uri);
    self->trusted_uri = g_strdup(uri);
    return TRUE;
}

/* 把事件（JSON）投递给页面。JSON 作为参数传入，不拼接进脚本。 */
static void
dispatch_event(LoongBridge *self, const char *json)
{
    if (!page_trusted(self))
        return;
    GVariantDict args;
    g_variant_dict_init(&args, NULL);
    g_variant_dict_insert(&args, "json", "s", json);
    webkit_web_view_call_async_javascript_function(
        self->view, "const f = window.loong && window.loong[Symbol.for('loong.bridge.dispatch')]; if (f) f(json);", -1,
        g_variant_dict_end(&args), NULL, NULL, NULL, NULL, NULL);
}

static void
on_signal(GDBusConnection *bus, const char *sender, const char *path, const char *interface, const char *signal,
          GVariant *params, gpointer data)
{
    LoongBridge *self = data;
    g_autoptr(GString) json = g_string_new("{\"iface\":");

    if (g_strcmp0(interface, "org.freedesktop.DBus.Properties") == 0 && g_strcmp0(signal, "PropertiesChanged") == 0) {
        if (!g_variant_is_of_type(params, G_VARIANT_TYPE("(sa{sv}as)")))
            return;
        const char *changed_iface;
        g_autoptr(GVariant) changed = NULL;
        g_autoptr(GVariant) invalidated = NULL;
        g_variant_get(params, "(&s@a{sv}@as)", &changed_iface, &changed, &invalidated);
        const LoongBridgeInterface *i = loong_bridge_find_interface_by_dbus_name(changed_iface);
        if (!i || g_strcmp0(i->path, path) != 0)
            return;
        loong_bridge_json_string(json, i->name);
        g_string_append(json, ",\"signal\":\"PropertiesChanged\",\"args\":");
        loong_bridge_json_variant(json, changed);
        g_string_append(json, ",\"invalidated\":");
        loong_bridge_json_variant(json, invalidated);
        g_string_append_c(json, '}');
        dispatch_event(self, json->str);
        return;
    }

    /* 其他信号：只转发白名单中的两个。 */
    const LoongBridgeInterface *i = loong_bridge_find_interface_by_dbus_name(interface);
    if (!i || g_strcmp0(i->path, path) != 0)
        return;
    loong_bridge_json_string(json, i->name);
    if (strcmp(i->name, "Wifi") == 0 && g_strcmp0(signal, "NetworksChanged") == 0) {
        g_string_append(json, ",\"signal\":\"NetworksChanged\",\"args\":{}}");
    } else if (strcmp(i->name, "Power") == 0 && g_strcmp0(signal, "LowBattery") == 0 &&
               g_variant_is_of_type(params, G_VARIANT_TYPE("(sd)"))) {
        const char *level;
        double percentage;
        g_variant_get(params, "(&sd)", &level, &percentage);
        g_string_append(json, ",\"signal\":\"LowBattery\",\"args\":{\"level\":");
        loong_bridge_json_string(json, level);
        g_autoptr(GVariant) pct = g_variant_ref_sink(g_variant_new_double(percentage));
        g_string_append(json, ",\"percentage\":");
        loong_bridge_json_variant(json, pct);
        g_string_append(json, "}}");
    } else {
        return;
    }
    dispatch_event(self, json->str);
}

static void
on_name_appeared(GDBusConnection *bus, const char *name, const char *owner, gpointer data)
{
    dispatch_event(data, "{\"iface\":\"System\",\"signal\":\"ServiceChanged\",\"args\":{\"running\":true}}");
}

static void
on_name_vanished(GDBusConnection *bus, const char *name, gpointer data)
{
    dispatch_event(data, "{\"iface\":\"System\",\"signal\":\"ServiceChanged\",\"args\":{\"running\":false}}");
}

static void
on_theme(LoongAppearance *appearance, GParamSpec *pspec, LoongBridge *self)
{
    g_autoptr(GString) json = g_string_new("{\"iface\":\"Appearance\",\"signal\":\"Changed\",\"args\":{\"theme\":");
    loong_bridge_json_string(json, loong_appearance_get_theme(appearance));
    g_string_append(json, "}}");
    dispatch_event(self, json->str);
}

/* ───────────────────────── D-Bus ───────────────────────── */

static void
on_call_done(GObject *source, GAsyncResult *result, gpointer data)
{
    Request *r = data;
    g_autoptr(GError) error = NULL;
    g_autoptr(GVariant) ret = g_dbus_connection_call_finish(G_DBUS_CONNECTION(source), result, &error);
    if (g_error_matches(error, G_IO_ERROR, G_IO_ERROR_CANCELLED)) {
        /* bridge 已销毁：页面也没了，不再回复。 */
        request_free(r);
        return;
    }
    if (!ret) {
        reply_gerror(r, error);
    } else if (!r->method) {
        g_autoptr(GVariant) props = g_variant_get_child_value(ret, 0); /* (a{sv}) → a{sv} */
        reply_value(r, props);
    } else {
        /* 没有返回值的方法给 null；其余是返回值元组（JS 侧取 [0]）。 */
        reply_value(r, g_variant_n_children(ret) ? ret : NULL);
    }
    request_free(r);
}

static void
request_send(LoongBridge *self, Request *r)
{
    if (r->method) {
        g_dbus_connection_call(self->bus, LOONG_SYSD_BUS_NAME, r->iface->path, r->iface->interface, r->method->method,
                               r->params, G_VARIANT_TYPE(r->method->out_sig), G_DBUS_CALL_FLAGS_NONE,
                               r->method->timeout_ms, self->cancellable, on_call_done, r);
    } else {
        g_dbus_connection_call(self->bus, LOONG_SYSD_BUS_NAME, r->iface->path, "org.freedesktop.DBus.Properties",
                               "GetAll", g_variant_new("(s)", r->iface->interface), G_VARIANT_TYPE("(a{sv})"),
                               G_DBUS_CALL_FLAGS_NONE, -1, self->cancellable, on_call_done, r);
    }
}

static void ensure_bus(LoongBridge *self);

static void
drop_bus(LoongBridge *self)
{
    if (!self->bus)
        return;
    g_dbus_connection_signal_unsubscribe(self->bus, self->signal_id);
    g_bus_unwatch_name(self->watch_id);
    g_signal_handler_disconnect(self->bus, self->closed_handler);
    self->signal_id = self->watch_id = 0;
    self->closed_handler = 0;
    g_clear_object(&self->bus);
}

static void
on_bus_closed(GDBusConnection *bus, gboolean remote_peer_vanished, GError *error, LoongBridge *self)
{
    g_warning("loong-bridge: 与总线的连接已断开%s%s，下次调用时重新连接", error ? "：" : "", error ? error->message : "");
    drop_bus(self);
    on_name_vanished(NULL, LOONG_SYSD_BUS_NAME, self);
}

static void
bus_ready(LoongBridge *self, GDBusConnection *bus, GError *error)
{
    self->connecting = FALSE;
    Request *r;
    if (!bus) {
        g_warning("loong-bridge: 无法连接 loong-sysd 所在的总线：%s", error->message);
        while ((r = g_queue_pop_head(&self->waiting))) {
            reply_error(r->reply, r->ctx, LOONG_BRIDGE_ERROR_UNAVAILABLE, "无法连接系统服务（D-Bus）");
            request_free(r);
        }
        return;
    }
    self->bus = bus;
    /* g_bus_get() 返回的共享连接默认断开时退出进程；Web 应用不应因此退出。 */
    g_dbus_connection_set_exit_on_close(bus, FALSE);
    self->closed_handler = g_signal_connect(bus, "closed", G_CALLBACK(on_bus_closed), self);
    /* 先订阅信号、再处理排队的 GetAll，保证不漏掉中间的变化。 */
    self->signal_id = g_dbus_connection_signal_subscribe(bus, LOONG_SYSD_BUS_NAME, NULL, NULL, NULL, NULL,
                                                         G_DBUS_SIGNAL_FLAGS_NONE, on_signal, self, NULL);
    self->watch_id = g_bus_watch_name_on_connection(bus, LOONG_SYSD_BUS_NAME, G_BUS_NAME_WATCHER_FLAGS_NONE,
                                                    on_name_appeared, on_name_vanished, self, NULL);
    while ((r = g_queue_pop_head(&self->waiting)))
        request_send(self, r);
}

static void
on_bus_get(GObject *source, GAsyncResult *result, gpointer data)
{
    g_autoptr(GError) error = NULL;
    GDBusConnection *bus = g_bus_get_finish(result, &error);
    if (g_error_matches(error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
        return;
    bus_ready(data, bus, error);
}

static void
on_bus_address(GObject *source, GAsyncResult *result, gpointer data)
{
    g_autoptr(GError) error = NULL;
    GDBusConnection *bus = g_dbus_connection_new_for_address_finish(result, &error);
    if (g_error_matches(error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
        return;
    bus_ready(data, bus, error);
}

/* 连接 loong-sysd 所在的总线：默认系统总线；LOONG_SYSD_BUS=session|system|<地址>（与 loong-sysd 相同）。 */
static void
ensure_bus(LoongBridge *self)
{
    if (self->bus || self->connecting)
        return;
    self->connecting = TRUE;
    const char *spec = g_getenv("LOONG_SYSD_BUS");
    if (!spec || !*spec || strcmp(spec, "system") == 0)
        g_bus_get(G_BUS_TYPE_SYSTEM, self->cancellable, on_bus_get, self);
    else if (strcmp(spec, "session") == 0)
        g_bus_get(G_BUS_TYPE_SESSION, self->cancellable, on_bus_get, self);
    else
        g_dbus_connection_new_for_address(spec,
                                          G_DBUS_CONNECTION_FLAGS_AUTHENTICATION_CLIENT |
                                              G_DBUS_CONNECTION_FLAGS_MESSAGE_BUS_CONNECTION,
                                          NULL, self->cancellable, on_bus_address, self);
}

static void
submit(LoongBridge *self, Request *r)
{
    if (self->bus) {
        request_send(self, r);
    } else {
        g_queue_push_tail(&self->waiting, r);
        ensure_bus(self);
    }
}

/* ───────────────────────── 本机信息（在线程里读取） ───────────────────────── */

static void
dict_insert_statvfs(GVariantDict *dict, const char *key, const char *path)
{
    struct statvfs s;
    if (statvfs(path, &s) != 0)
        return;
    GVariantDict d;
    g_variant_dict_init(&d, NULL);
    guint64 unit = s.f_frsize ? s.f_frsize : s.f_bsize;
    g_variant_dict_insert(&d, "total", "t", (guint64)s.f_blocks * unit);
    g_variant_dict_insert(&d, "free", "t", (guint64)s.f_bfree * unit);
    g_variant_dict_insert(&d, "available", "t", (guint64)s.f_bavail * unit);
    g_variant_dict_insert_value(dict, key, g_variant_dict_end(&d));
}

/* /proc/cpuinfo 中第一个出现的 `key : value`。 */
static char *
cpuinfo_field(const char *text, const char *key)
{
    g_auto(GStrv) lines = g_strsplit(text ? text : "", "\n", -1);
    for (char **l = lines; *l; l++) {
        char *colon = strchr(*l, ':');
        if (!colon)
            continue;
        g_autofree char *k = g_strstrip(g_strndup(*l, colon - *l));
        if (g_ascii_strcasecmp(k, key) == 0) {
            char *v = g_strstrip(g_strdup(colon + 1));
            if (*v && g_utf8_validate(v, -1, NULL))
                return v;
            g_free(v);
        }
    }
    return NULL;
}

/* 读取一个短的文本文件（sysfs、device-tree），去掉结尾的 NUL 与空白。 */
static char *
read_short(const char *path)
{
    g_autofree char *text = NULL;
    gsize len = 0;
    if (!g_file_get_contents(path, &text, &len, NULL))
        return NULL;
    char *s = g_strstrip(g_strndup(text, strnlen(text, len)));
    if (!*s || !g_utf8_validate(s, -1, NULL)) {
        g_free(s);
        return NULL;
    }
    return s;
}

static GVariant *
collect_device_info(void)
{
    GVariantDict info;
    g_variant_dict_init(&info, NULL);

    /* 系统：/etc/os-release（开发时可用 LOONG_OS_RELEASE 指向别的文件）。 */
    g_autofree char *os_text = NULL;
    const char *os_path = g_getenv("LOONG_OS_RELEASE");
    if (!(os_path && g_file_get_contents(os_path, &os_text, NULL, NULL)) &&
        !g_file_get_contents("/etc/os-release", &os_text, NULL, NULL))
        g_file_get_contents("/usr/lib/os-release", &os_text, NULL, NULL);
    g_autoptr(GHashTable) os = loong_bridge_parse_os_release(os_text);
    static const char *const os_keys[][2] = {
        { "NAME", "name" },           { "PRETTY_NAME", "prettyName" }, { "ID", "id" },
        { "VERSION", "version" },     { "VERSION_ID", "versionId" },   { "IMAGE_VERSION", "imageVersion" },
        { "BUILD_ID", "buildId" },    { "HOME_URL", "homeUrl" },
    };
    GVariantDict osd;
    g_variant_dict_init(&osd, NULL);
    for (gsize i = 0; i < G_N_ELEMENTS(os_keys); i++) {
        const char *v = g_hash_table_lookup(os, os_keys[i][0]);
        if (v)
            g_variant_dict_insert(&osd, os_keys[i][1], "s", v);
    }
    g_variant_dict_insert_value(&info, "os", g_variant_dict_end(&osd));

    /* 机型：/usr/share/loong/device.toml（与 loong-shell 相同；开发时可用 LOONG_DEVICE_FILE）。 */
    const char *dev_path = g_getenv("LOONG_DEVICE_FILE");
    g_autofree char *dev_text = NULL;
    if (g_file_get_contents(dev_path && *dev_path ? dev_path : DEVICE_FILE, &dev_text, NULL, NULL))
        g_variant_dict_insert_value(&info, "device", loong_bridge_parse_toml(dev_text));

    g_variant_dict_insert(&info, "hostname", "s", g_get_host_name());

    struct utsname u;
    if (uname(&u) == 0) {
        GVariantDict k;
        g_variant_dict_init(&k, NULL);
        g_variant_dict_insert(&k, "name", "s", u.sysname);
        g_variant_dict_insert(&k, "release", "s", u.release);
        g_variant_dict_insert(&k, "machine", "s", u.machine);
        g_variant_dict_insert_value(&info, "kernel", g_variant_dict_end(&k));
    }

    GVariantDict cpu;
    g_variant_dict_init(&cpu, NULL);
    g_variant_dict_insert(&cpu, "count", "u", g_get_num_processors());
    g_autofree char *cpuinfo = NULL;
    g_file_get_contents("/proc/cpuinfo", &cpuinfo, NULL, NULL);
    g_autofree char *model = cpuinfo_field(cpuinfo, "model name");
    if (!model)
        model = cpuinfo_field(cpuinfo, "Hardware");
    if (model)
        g_variant_dict_insert(&cpu, "model", "s", model);
    /* ARM SoC：/sys/devices/soc0/{family,machine}，例如 Snapdragon / SM8150。 */
    g_autofree char *soc_family = read_short("/sys/devices/soc0/family");
    g_autofree char *soc_machine = read_short("/sys/devices/soc0/machine");
    if (soc_machine) {
        g_autofree char *soc = soc_family ? g_strdup_printf("%s %s", soc_family, soc_machine) : g_strdup(soc_machine);
        g_variant_dict_insert(&cpu, "soc", "s", soc);
    }
    g_variant_dict_insert_value(&info, "cpu", g_variant_dict_end(&cpu));

    /* 主板型号（设备树，例如 "Xiaomi Pad 5"）。 */
    g_autofree char *board = read_short("/proc/device-tree/model");
    if (board)
        g_variant_dict_insert(&info, "board", "s", board);

    g_autofree char *meminfo = NULL;
    if (g_file_get_contents("/proc/meminfo", &meminfo, NULL, NULL)) {
        g_autofree char *total = cpuinfo_field(meminfo, "MemTotal");
        if (total) {
            GVariantDict m;
            g_variant_dict_init(&m, NULL);
            g_variant_dict_insert(&m, "total", "t", g_ascii_strtoull(total, NULL, 10) * 1024);
            g_variant_dict_insert_value(&info, "memory", g_variant_dict_end(&m));
        }
    }

    /* 存储：用户数据所在的文件系统（$HOME，设备上是 data 分区）与系统分区（/，只读 erofs）。 */
    GVariantDict storage;
    g_variant_dict_init(&storage, NULL);
    const char *home = g_get_home_dir();
    dict_insert_statvfs(&storage, "data", home);
    dict_insert_statvfs(&storage, "system", "/");
    struct stat hs, rs;
    gboolean separate = stat(home, &hs) == 0 && stat("/", &rs) == 0 && hs.st_dev != rs.st_dev;
    g_variant_dict_insert(&storage, "separate", "b", separate);
    g_variant_dict_insert_value(&info, "storage", g_variant_dict_end(&storage));

    g_autofree char *webkit =
        g_strdup_printf("%u.%u.%u", webkit_get_major_version(), webkit_get_minor_version(), webkit_get_micro_version());
    g_variant_dict_insert(&info, "webkit", "s", webkit);

    return g_variant_dict_end(&info);
}

static void
device_info_thread(GTask *task, gpointer source, gpointer data, GCancellable *cancellable)
{
    g_task_return_pointer(task, g_variant_ref_sink(collect_device_info()), (GDestroyNotify)g_variant_unref);
}

static void
on_device_info(GObject *source, GAsyncResult *result, gpointer data)
{
    Request *r = data;
    g_autoptr(GError) error = NULL;
    g_autoptr(GVariant) info = g_task_propagate_pointer(G_TASK(result), &error);
    if (info)
        reply_value(r, info);
    else if (!g_error_matches(error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
        reply_gerror(r, error);
    request_free(r);
}

static void
on_theme_written(GObject *source, GAsyncResult *result, gpointer data)
{
    Request *r = data;
    g_autoptr(GError) error = NULL;
    if (loong_appearance_set_theme_finish(LOONG_APPEARANCE(source), result, &error)) {
        reply_value(r, NULL);
    } else if (!g_error_matches(error, G_IO_ERROR, G_IO_ERROR_CANCELLED)) {
        reply_gerror(r, error);
    }
    request_free(r);
}

/* ───────────────────────── 消息入口 ───────────────────────── */

static gboolean
on_message(WebKitUserContentManager *ucm, JSCValue *message, WebKitScriptMessageReply *reply, LoongBridge *self)
{
    JSCContext *ctx = jsc_value_get_context(message);
    if (!page_trusted(self)) {
        g_warning("loong-bridge: 拒绝来自 %s 的调用", webkit_web_view_get_uri(self->view));
        reply_error(reply, ctx, LOONG_BRIDGE_ERROR_NOT_ALLOWED, "当前页面无权使用系统接口");
        return TRUE;
    }
    if (!jsc_value_is_object(message)) {
        reply_error(reply, ctx, LOONG_BRIDGE_ERROR_INVALID_ARGS, "消息必须是对象");
        return TRUE;
    }
    g_autofree char *op = loong_bridge_get_string_property(message, "op", 32);

    if (g_strcmp0(op, "call") == 0 || g_strcmp0(op, "getAll") == 0) {
        g_autofree char *iface_name = loong_bridge_get_string_property(message, "iface", 32);
        const LoongBridgeInterface *iface = loong_bridge_find_interface(iface_name);
        if (!iface) {
            reply_error(reply, ctx, LOONG_BRIDGE_ERROR_NOT_ALLOWED, "不允许访问这个接口");
            return TRUE;
        }
        const LoongBridgeMethod *method = NULL;
        GVariant *params = NULL;
        if (strcmp(op, "call") == 0) {
            g_autofree char *method_name = loong_bridge_get_string_property(message, "method", 64);
            method = loong_bridge_find_method(iface_name, method_name);
            if (!method) {
                reply_error(reply, ctx, LOONG_BRIDGE_ERROR_NOT_ALLOWED, "不允许调用这个方法");
                return TRUE;
            }
            g_autoptr(JSCValue) args = jsc_value_object_get_property(message, "args");
            g_autoptr(GError) error = NULL;
            params = loong_bridge_build_args(method, args, &error);
            if (!params) {
                reply_error(reply, ctx, loong_bridge_error_name(error->code), error->message);
                return TRUE;
            }
        }
        Request *r = request_new(self, reply, ctx);
        r->iface = iface;
        r->method = method;
        r->params = params ? g_variant_ref_sink(params) : NULL;
        submit(self, r);
    } else if (g_strcmp0(op, "appearance.get") == 0) {
        g_autoptr(GString) json = g_string_new("{\"ok\":true,\"value\":{\"theme\":");
        loong_bridge_json_string(json, loong_appearance_get_theme(self->appearance));
        g_string_append(json, "}}");
        reply_json(reply, ctx, json->str);
    } else if (g_strcmp0(op, "appearance.set") == 0) {
        g_autofree char *theme = loong_bridge_get_string_property(message, "theme", 16);
        if (!loong_appearance_valid(theme)) {
            reply_error(reply, ctx, LOONG_BRIDGE_ERROR_INVALID_ARGS, "外观只能是 light 或 dark");
            return TRUE;
        }
        loong_appearance_set_theme_async(self->appearance, theme, self->cancellable, on_theme_written,
                                         request_new(self, reply, ctx));
    } else if (g_strcmp0(op, "device.info") == 0) {
        g_autoptr(GTask) task = g_task_new(NULL, self->cancellable, on_device_info, request_new(self, reply, ctx));
        g_task_run_in_thread(task, device_info_thread);
    } else {
        reply_error(reply, ctx, LOONG_BRIDGE_ERROR_INVALID_ARGS, "未知的操作");
    }
    return TRUE;
}

/* ───────────────────────── 创建与销毁 ───────────────────────── */

LoongBridge *
loong_bridge_new(WebKitWebView *view, const char *root_dir, LoongAppearance *appearance)
{
    g_autoptr(GBytes) shim = g_resources_lookup_data(SHIM_RESOURCE, G_RESOURCE_LOOKUP_FLAGS_NONE, NULL);
    g_return_val_if_fail(shim != NULL, NULL);

    LoongBridge *self = g_new0(LoongBridge, 1);
    self->view = view;
    self->root = g_strdup(root_dir);
    self->appearance = g_object_ref(appearance);
    self->cancellable = g_cancellable_new();
    g_queue_init(&self->waiting);
    self->ucm = g_object_ref(webkit_web_view_get_user_content_manager(view));

    /* JS 只注入顶层框架、只注入受信任目录下的页面（消息处理时还会再校验一次）。 */
    gsize len = 0;
    const char *data = g_bytes_get_data(shim, &len);
    g_autofree char *source = g_strndup(data, len);
    g_autofree char *pattern = g_strconcat("file://", root_dir, "/*", NULL);
    const char *const allow[] = { pattern, NULL };
    WebKitUserScript *script = webkit_user_script_new(source, WEBKIT_USER_CONTENT_INJECT_TOP_FRAME,
                                                      WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START, allow, NULL);
    webkit_user_content_manager_add_script(self->ucm, script);
    webkit_user_script_unref(script);

    self->message_handler = g_signal_connect(self->ucm, "script-message-with-reply-received::" HANDLER_NAME,
                                             G_CALLBACK(on_message), self);
    if (!webkit_user_content_manager_register_script_message_handler_with_reply(self->ucm, HANDLER_NAME, NULL))
        g_warning("loong-bridge: 无法注册消息处理器 %s", HANDLER_NAME);
    self->appearance_handler = g_signal_connect(appearance, "notify::theme", G_CALLBACK(on_theme), self);

    ensure_bus(self);
    return self;
}

void
loong_bridge_free(LoongBridge *self)
{
    if (!self)
        return;
    g_cancellable_cancel(self->cancellable);
    Request *r;
    while ((r = g_queue_pop_head(&self->waiting)))
        request_free(r);
    drop_bus(self);
    webkit_user_content_manager_unregister_script_message_handler(self->ucm, HANDLER_NAME, NULL);
    g_signal_handler_disconnect(self->ucm, self->message_handler);
    g_signal_handler_disconnect(self->appearance, self->appearance_handler);
    g_object_unref(self->ucm);
    g_object_unref(self->appearance);
    g_object_unref(self->cancellable);
    g_free(self->trusted_uri);
    g_free(self->root);
    g_free(self);
}
