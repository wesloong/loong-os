/*
 * fake-iwd：开发用的假 iwd（net.connman.iwd），只实现 loong-sysd 用到的部分，
 * 让没有无线网卡的开发环境也能把“设置 → bridge → loong-sysd → iwd”整条链路跑通。
 *
 *   fake-iwd --address <D-Bus 地址>      （webrt/tests/settings-headless.sh 用 FAKE_IWD=1 启动它）
 *
 * 一块网卡（/net/connman/iwd/0/4），五个网络；已保存“竹林小筑”（已连接）与“听雨轩”。
 * 新的加密网络连接时和真 iwd 一样回调已注册 Agent 的 RequestPassphrase，密码是 loong1234。
 * 每次状态变化都在设备对象上发 PropertiesChanged，loong-sysd 收到 iwd 的任何信号都会重新读取。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include <gio/gio.h>
#include <stdlib.h>
#include <string.h>

#define IWD "net.connman.iwd"
#define ADAPTER_PATH "/net/connman/iwd/0"
#define DEVICE_PATH "/net/connman/iwd/0/4"
#define PASSWORD "loong1234"

typedef struct {
    const char *ssid;
    const char *type; /* open / psk / 8021x */
    int dbm;
    gboolean known;
    gboolean connected;
    char *path;       /* Network 对象 */
    char *known_path; /* KnownNetwork 对象 */
} Net;

static Net nets[] = {
    { "竹林小筑", "psk", -41, TRUE, TRUE, NULL, NULL },
    { "茶寮-5G", "psk", -47, FALSE, FALSE, NULL, NULL },
    { "听雨轩", "psk", -59, TRUE, FALSE, NULL, NULL },
    { "Loong-Guest", "open", -64, FALSE, FALSE, NULL, NULL },
    { "山居书屋", "psk", -76, FALSE, FALSE, NULL, NULL },
};

static GDBusConnection *bus;
static gboolean adapter_powered = TRUE;
static gboolean device_powered = TRUE;
static gboolean scanning = FALSE;
static const char *station_state = "connected";
static char *agent_owner;
static char *agent_path;

static const char xml[] =
    "<node>"
    " <interface name='org.freedesktop.DBus.ObjectManager'>"
    "  <method name='GetManagedObjects'><arg type='a{oa{sa{sv}}}' direction='out'/></method>"
    " </interface>"
    " <interface name='net.connman.iwd.AgentManager'>"
    "  <method name='RegisterAgent'><arg type='o' direction='in'/></method>"
    "  <method name='UnregisterAgent'><arg type='o' direction='in'/></method>"
    " </interface>"
    " <interface name='net.connman.iwd.Adapter'>"
    "  <property name='Powered' type='b' access='readwrite'/>"
    " </interface>"
    " <interface name='net.connman.iwd.Device'>"
    "  <property name='Name' type='s' access='read'/>"
    "  <property name='Powered' type='b' access='readwrite'/>"
    "  <property name='Adapter' type='o' access='read'/>"
    "  <property name='Mode' type='s' access='read'/>"
    " </interface>"
    " <interface name='net.connman.iwd.Station'>"
    "  <method name='Scan'/>"
    "  <method name='Disconnect'/>"
    "  <method name='GetOrderedNetworks'><arg type='a(on)' direction='out'/></method>"
    "  <property name='State' type='s' access='read'/>"
    "  <property name='Scanning' type='b' access='read'/>"
    " </interface>"
    " <interface name='net.connman.iwd.Network'>"
    "  <method name='Connect'/>"
    "  <property name='Name' type='s' access='read'/>"
    "  <property name='Type' type='s' access='read'/>"
    "  <property name='Connected' type='b' access='read'/>"
    " </interface>"
    " <interface name='net.connman.iwd.KnownNetwork'>"
    "  <method name='Forget'/>"
    "  <property name='Name' type='s' access='read'/>"
    "  <property name='Type' type='s' access='read'/>"
    " </interface>"
    "</node>";

static GDBusNodeInfo *info;

static Net *
net_by_path(const char *path)
{
    for (gsize i = 0; i < G_N_ELEMENTS(nets); i++) {
        if (g_strcmp0(nets[i].path, path) == 0 || g_strcmp0(nets[i].known_path, path) == 0)
            return &nets[i];
    }
    return NULL;
}

static Net *
connected_net(void)
{
    for (gsize i = 0; i < G_N_ELEMENTS(nets); i++) {
        if (nets[i].connected)
            return &nets[i];
    }
    return NULL;
}

/* 任意一个 iwd 信号都会让 loong-sysd 重新读取全部状态。 */
static void
emit_changed(void)
{
    GVariantBuilder b;
    g_variant_builder_init(&b, G_VARIANT_TYPE("a{sv}"));
    g_variant_builder_add(&b, "{sv}", "State", g_variant_new_string(station_state));
    g_variant_builder_add(&b, "{sv}", "Scanning", g_variant_new_boolean(scanning));
    g_dbus_connection_emit_signal(bus, NULL, DEVICE_PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged",
                                  g_variant_new("(sa{sv}as)", "net.connman.iwd.Station", &b, NULL), NULL);
}

static void
add_iface(GVariantBuilder *ifaces, const char *name, GVariantBuilder *props)
{
    g_variant_builder_add(ifaces, "{sa{sv}}", name, props);
}

static GVariant *
managed_objects(void)
{
    GVariantBuilder objs, ifaces, p;
    g_variant_builder_init(&objs, G_VARIANT_TYPE("a{oa{sa{sv}}}"));

    g_variant_builder_init(&ifaces, G_VARIANT_TYPE("a{sa{sv}}"));
    g_variant_builder_init(&p, G_VARIANT_TYPE("a{sv}"));
    add_iface(&ifaces, "net.connman.iwd.AgentManager", &p);
    g_variant_builder_add(&objs, "{oa{sa{sv}}}", "/net/connman/iwd", &ifaces);

    g_variant_builder_init(&ifaces, G_VARIANT_TYPE("a{sa{sv}}"));
    g_variant_builder_init(&p, G_VARIANT_TYPE("a{sv}"));
    g_variant_builder_add(&p, "{sv}", "Powered", g_variant_new_boolean(adapter_powered));
    add_iface(&ifaces, "net.connman.iwd.Adapter", &p);
    g_variant_builder_add(&objs, "{oa{sa{sv}}}", ADAPTER_PATH, &ifaces);

    g_variant_builder_init(&ifaces, G_VARIANT_TYPE("a{sa{sv}}"));
    g_variant_builder_init(&p, G_VARIANT_TYPE("a{sv}"));
    g_variant_builder_add(&p, "{sv}", "Name", g_variant_new_string("wlan0"));
    g_variant_builder_add(&p, "{sv}", "Powered", g_variant_new_boolean(device_powered));
    g_variant_builder_add(&p, "{sv}", "Adapter", g_variant_new_object_path(ADAPTER_PATH));
    g_variant_builder_add(&p, "{sv}", "Mode", g_variant_new_string("station"));
    add_iface(&ifaces, "net.connman.iwd.Device", &p);
    if (device_powered) {
        /* 和真 iwd 一样：设备关闭时 Station 接口消失。 */
        g_variant_builder_init(&p, G_VARIANT_TYPE("a{sv}"));
        g_variant_builder_add(&p, "{sv}", "State", g_variant_new_string(station_state));
        g_variant_builder_add(&p, "{sv}", "Scanning", g_variant_new_boolean(scanning));
        Net *c = connected_net();
        if (c)
            g_variant_builder_add(&p, "{sv}", "ConnectedNetwork", g_variant_new_object_path(c->path));
        add_iface(&ifaces, "net.connman.iwd.Station", &p);
    }
    g_variant_builder_add(&objs, "{oa{sa{sv}}}", DEVICE_PATH, &ifaces);

    for (gsize i = 0; i < G_N_ELEMENTS(nets); i++) {
        Net *n = &nets[i];
        if (device_powered) {
            g_variant_builder_init(&ifaces, G_VARIANT_TYPE("a{sa{sv}}"));
            g_variant_builder_init(&p, G_VARIANT_TYPE("a{sv}"));
            g_variant_builder_add(&p, "{sv}", "Name", g_variant_new_string(n->ssid));
            g_variant_builder_add(&p, "{sv}", "Type", g_variant_new_string(n->type));
            g_variant_builder_add(&p, "{sv}", "Connected", g_variant_new_boolean(n->connected));
            g_variant_builder_add(&p, "{sv}", "Device", g_variant_new_object_path(DEVICE_PATH));
            if (n->known)
                g_variant_builder_add(&p, "{sv}", "KnownNetwork", g_variant_new_object_path(n->known_path));
            add_iface(&ifaces, "net.connman.iwd.Network", &p);
            g_variant_builder_add(&objs, "{oa{sa{sv}}}", n->path, &ifaces);
        }
        if (n->known) {
            g_variant_builder_init(&ifaces, G_VARIANT_TYPE("a{sa{sv}}"));
            g_variant_builder_init(&p, G_VARIANT_TYPE("a{sv}"));
            g_variant_builder_add(&p, "{sv}", "Name", g_variant_new_string(n->ssid));
            g_variant_builder_add(&p, "{sv}", "Type", g_variant_new_string(n->type));
            add_iface(&ifaces, "net.connman.iwd.KnownNetwork", &p);
            g_variant_builder_add(&objs, "{oa{sa{sv}}}", n->known_path, &ifaces);
        }
    }
    return g_variant_new("(a{oa{sa{sv}}})", &objs);
}

static int
by_signal(gconstpointer a, gconstpointer b)
{
    return (*(Net *const *)b)->dbm - (*(Net *const *)a)->dbm;
}

static gboolean
scan_done(gpointer data)
{
    /* 信号强度随机晃动一点，像真的扫描。 */
    for (gsize i = 0; i < G_N_ELEMENTS(nets); i++)
        nets[i].dbm = CLAMP(nets[i].dbm + g_random_int_range(-2, 3), -88, -32);
    scanning = FALSE;
    emit_changed();
    g_message("扫描完成");
    return G_SOURCE_REMOVE;
}

static void
set_connected(Net *n)
{
    for (gsize i = 0; i < G_N_ELEMENTS(nets); i++)
        nets[i].connected = FALSE;
    if (n) {
        n->connected = TRUE;
        n->known = TRUE;
        station_state = "connected";
        g_message("已连接 %s", n->ssid);
    } else {
        station_state = "disconnected";
    }
    emit_changed();
}

typedef struct {
    GDBusMethodInvocation *inv;
    Net *net;
} Pending;

static gboolean
finish_connect(gpointer data)
{
    Pending *p = data;
    set_connected(p->net);
    g_dbus_method_invocation_return_value(p->inv, NULL);
    g_free(p);
    return G_SOURCE_REMOVE;
}

static void
on_passphrase(GObject *source, GAsyncResult *result, gpointer data)
{
    Pending *p = data;
    g_autoptr(GError) error = NULL;
    g_autoptr(GVariant) reply = g_dbus_connection_call_finish(bus, result, &error);
    const char *pass = NULL;
    if (reply)
        g_variant_get(reply, "(&s)", &pass);
    g_message("Agent 交回密码：%s", reply ? (strcmp(pass, PASSWORD) == 0 ? "正确" : "错误") : error->message);
    if (!reply || strcmp(pass, PASSWORD) != 0) {
        station_state = connected_net() ? "connected" : "disconnected";
        emit_changed();
        g_dbus_method_invocation_return_dbus_error(p->inv, "net.connman.iwd.Failed", "Operation failed");
        g_free(p);
        return;
    }
    g_timeout_add(600, finish_connect, p);
}

static void
method_call(GDBusConnection *c, const char *sender, const char *path, const char *iface, const char *method,
            GVariant *params, GDBusMethodInvocation *inv, gpointer data)
{
    if (strcmp(method, "GetManagedObjects") == 0) {
        g_dbus_method_invocation_return_value(inv, managed_objects());
    } else if (strcmp(method, "RegisterAgent") == 0) {
        g_free(agent_owner);
        g_free(agent_path);
        agent_owner = g_strdup(sender);
        g_variant_get(params, "(o)", &agent_path);
        g_message("Agent 已注册：%s %s", agent_owner, agent_path);
        g_dbus_method_invocation_return_value(inv, NULL);
    } else if (strcmp(method, "UnregisterAgent") == 0) {
        g_clear_pointer(&agent_owner, g_free);
        g_clear_pointer(&agent_path, g_free);
        g_dbus_method_invocation_return_value(inv, NULL);
    } else if (strcmp(method, "Scan") == 0) {
        if (scanning) {
            g_dbus_method_invocation_return_dbus_error(inv, "net.connman.iwd.Busy", "Operation already in progress");
            return;
        }
        scanning = TRUE;
        emit_changed();
        g_timeout_add(1500, scan_done, NULL);
        g_dbus_method_invocation_return_value(inv, NULL);
    } else if (strcmp(method, "GetOrderedNetworks") == 0) {
        Net *sorted[G_N_ELEMENTS(nets)];
        for (gsize i = 0; i < G_N_ELEMENTS(nets); i++)
            sorted[i] = &nets[i];
        qsort(sorted, G_N_ELEMENTS(nets), sizeof sorted[0], by_signal);
        GVariantBuilder b;
        g_variant_builder_init(&b, G_VARIANT_TYPE("a(on)"));
        for (gsize i = 0; i < G_N_ELEMENTS(nets); i++)
            g_variant_builder_add(&b, "(on)", sorted[i]->path, (gint16)(sorted[i]->dbm * 100));
        g_dbus_method_invocation_return_value(inv, g_variant_new("(a(on))", &b));
    } else if (strcmp(method, "Disconnect") == 0) {
        set_connected(NULL);
        g_dbus_method_invocation_return_value(inv, NULL);
    } else if (strcmp(method, "Connect") == 0) {
        Net *n = net_by_path(path);
        station_state = "connecting";
        emit_changed();
        Pending *p = g_new0(Pending, 1);
        p->inv = inv;
        p->net = n;
        if (strcmp(n->type, "open") == 0 || n->known) {
            g_timeout_add(900, finish_connect, p);
        } else if (!agent_owner) {
            g_dbus_method_invocation_return_dbus_error(inv, "net.connman.iwd.Failed", "No agent");
            g_free(p);
        } else {
            g_dbus_connection_call(bus, agent_owner, agent_path, "net.connman.iwd.Agent", "RequestPassphrase",
                                   g_variant_new("(o)", n->path), G_VARIANT_TYPE("(s)"), G_DBUS_CALL_FLAGS_NONE, 30000,
                                   NULL, on_passphrase, p);
        }
    } else if (strcmp(method, "Forget") == 0) {
        Net *n = net_by_path(path);
        if (!n || !n->known) {
            g_dbus_method_invocation_return_dbus_error(inv, "net.connman.iwd.NotFound", "Not found");
            return;
        }
        n->known = FALSE;
        g_message("已忘记 %s", n->ssid);
        if (n->connected)
            set_connected(NULL);
        else
            emit_changed();
        g_dbus_method_invocation_return_value(inv, NULL);
    } else {
        g_dbus_method_invocation_return_dbus_error(inv, "org.freedesktop.DBus.Error.UnknownMethod", method);
    }
}

static GVariant *
get_property(GDBusConnection *c, const char *sender, const char *path, const char *iface, const char *prop,
             GError **error, gpointer data)
{
    /* 直接从 GetManagedObjects 的结果里找，保证两边一致。 */
    g_autoptr(GVariant) all = g_variant_ref_sink(managed_objects());
    g_autoptr(GVariant) objs = g_variant_get_child_value(all, 0);
    g_autoptr(GVariant) ifaces = g_variant_lookup_value(objs, path, NULL);
    g_autoptr(GVariant) props = ifaces ? g_variant_lookup_value(ifaces, iface, NULL) : NULL;
    GVariant *v = props ? g_variant_lookup_value(props, prop, NULL) : NULL;
    if (!v)
        g_set_error(error, G_DBUS_ERROR, G_DBUS_ERROR_INVALID_ARGS, "没有属性 %s", prop);
    return v;
}

static gboolean
set_property(GDBusConnection *c, const char *sender, const char *path, const char *iface, const char *prop,
             GVariant *value, GError **error, gpointer data)
{
    gboolean on = g_variant_get_boolean(value);
    if (strcmp(iface, "net.connman.iwd.Adapter") == 0) {
        adapter_powered = on;
    } else {
        device_powered = on;
        if (!on)
            set_connected(NULL);
        station_state = on ? (connected_net() ? "connected" : "disconnected") : "disconnected";
    }
    g_message("%s.Powered = %d", iface, on);
    emit_changed();
    return TRUE;
}

static const GDBusInterfaceVTable vtable = { method_call, get_property, set_property, { 0 } };

static void
register_object(const char *path, const char *iface)
{
    g_autoptr(GError) error = NULL;
    if (!g_dbus_connection_register_object(bus, path, g_dbus_node_info_lookup_interface(info, iface), &vtable, NULL,
                                           NULL, &error))
        g_error("注册 %s %s 失败：%s", path, iface, error->message);
}

static char *
hex(const char *s)
{
    GString *out = g_string_new(NULL);
    for (const unsigned char *p = (const unsigned char *)s; *p; p++)
        g_string_append_printf(out, "%02x", *p);
    return g_string_free(out, FALSE);
}

int
main(int argc, char **argv)
{
    const char *address = argc == 3 && strcmp(argv[1], "--address") == 0 ? argv[2] : NULL;
    if (!address) {
        g_printerr("用法: fake-iwd --address <D-Bus 地址>\n");
        return 2;
    }
    g_autoptr(GError) error = NULL;
    bus = g_dbus_connection_new_for_address_sync(
        address, G_DBUS_CONNECTION_FLAGS_AUTHENTICATION_CLIENT | G_DBUS_CONNECTION_FLAGS_MESSAGE_BUS_CONNECTION, NULL,
        NULL, &error);
    if (!bus)
        g_error("连接 %s 失败：%s", address, error->message);
    info = g_dbus_node_info_new_for_xml(xml, &error);
    g_assert_no_error(error);

    register_object("/", "org.freedesktop.DBus.ObjectManager");
    register_object("/net/connman/iwd", "net.connman.iwd.AgentManager");
    register_object(ADAPTER_PATH, "net.connman.iwd.Adapter");
    register_object(DEVICE_PATH, "net.connman.iwd.Device");
    register_object(DEVICE_PATH, "net.connman.iwd.Station");
    for (gsize i = 0; i < G_N_ELEMENTS(nets); i++) {
        g_autofree char *h = hex(nets[i].ssid);
        nets[i].path = g_strdup_printf("%s/%s_%s", DEVICE_PATH, h, nets[i].type);
        nets[i].known_path = g_strdup_printf("/net/connman/iwd/%s_%s", h, nets[i].type);
        register_object(nets[i].path, "net.connman.iwd.Network");
        register_object(nets[i].known_path, "net.connman.iwd.KnownNetwork");
    }
    g_bus_own_name_on_connection(bus, IWD, G_BUS_NAME_OWNER_FLAGS_NONE, NULL, NULL, NULL, NULL);
    g_message("假 iwd 已启动（%s），密码 %s", address, PASSWORD);
    g_main_loop_run(g_main_loop_new(NULL, FALSE));
    return 0;
}
