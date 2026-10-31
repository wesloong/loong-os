/*
 * loong-bridge-core 的单元测试：白名单、参数校验、GVariant → JSON、URL 校验、配置解析。
 *
 *   meson test -C build/webrt
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include <glib/gstdio.h>
#include <math.h>
#include <string.h>
#include <unistd.h>

#include "loong-bridge-core.h"

static JSCContext *ctx;

static JSCValue *
js(const char *code)
{
    JSCValue *v = jsc_context_evaluate(ctx, code, -1);
    g_assert_nonnull(v);
    return v;
}

static void
test_allowlist(void)
{
    g_assert_nonnull(loong_bridge_find_method("Backlight", "SetBrightness"));
    g_assert_nonnull(loong_bridge_find_method("Wifi", "Connect"));
    g_assert_cmpint(loong_bridge_find_method("Wifi", "Connect")->timeout_ms, >, 25000);
    /* 不在白名单中的一律拒绝：大小写、其他接口、Properties.Set、Agent、其他服务。 */
    g_assert_null(loong_bridge_find_method("Backlight", "setBrightness"));
    g_assert_null(loong_bridge_find_method("Backlight", "Set"));
    g_assert_null(loong_bridge_find_method("Properties", "Set"));
    g_assert_null(loong_bridge_find_method("Agent", "RequestPassphrase"));
    g_assert_null(loong_bridge_find_method("Update1", "Apply"));
    g_assert_null(loong_bridge_find_method(NULL, "Scan"));
    g_assert_null(loong_bridge_find_interface("Agent"));
    g_assert_null(loong_bridge_find_interface("org.loong.System1.Wifi"));
    g_assert_cmpstr(loong_bridge_find_interface("Wifi")->path, ==, "/org/loong/System1/Wifi");
    g_assert_cmpstr(loong_bridge_find_interface_by_dbus_name("org.loong.System1.Power")->name, ==, "Power");
    g_assert_null(loong_bridge_find_interface_by_dbus_name("org.loong.System1.Update1"));
    g_assert_null(loong_bridge_find_interface_by_dbus_name("net.connman.iwd.Agent"));
}

/* 用给定的 JS 参数调用 build_args；成功时返回打印出来的 GVariant。 */
static char *
build(const char *iface, const char *method, const char *args_js, GError **error)
{
    const LoongBridgeMethod *m = loong_bridge_find_method(iface, method);
    g_assert_nonnull(m);
    g_autoptr(JSCValue) args = args_js ? js(args_js) : NULL;
    g_autoptr(GVariant) v = loong_bridge_build_args(m, args, error);
    if (!v)
        return NULL;
    g_variant_ref_sink(v);
    return g_variant_print(v, TRUE);
}

static void
expect_invalid(const char *iface, const char *method, const char *args_js)
{
    g_autoptr(GError) error = NULL;
    g_autofree char *s = build(iface, method, args_js, &error);
    if (s)
        g_error("%s.%s(%s) 应该被拒绝，却得到 %s", iface, method, args_js, s);
    g_assert_error(error, LOONG_BRIDGE_ERROR, LOONG_BRIDGE_ERR_INVALID_ARGS);
    g_assert_cmpstr(loong_bridge_error_name(error->code), ==, LOONG_BRIDGE_ERROR_INVALID_ARGS);
}

static void
test_build_args(void)
{
    g_autoptr(GError) error = NULL;
    g_autofree char *a = build("Backlight", "SetBrightness", "[0.5, true]", &error);
    g_assert_no_error(error);
    g_assert_cmpstr(a, ==, "(0.5, true)");
    g_autofree char *b = build("Wifi", "Connect", "['茶寮-5G', 'correct horse']", &error);
    g_assert_no_error(error);
    g_assert_cmpstr(b, ==, "('茶寮-5G', 'correct horse')");
    g_autofree char *c = build("Wifi", "Scan", NULL, &error);
    g_assert_no_error(error);
    g_assert_cmpstr(c, ==, "()");
    g_autofree char *d = build("Wifi", "Scan", "[]", &error);
    g_assert_no_error(error);
    g_assert_cmpstr(d, ==, "()");
    g_autofree char *e = build("Wifi", "Scan", "undefined", &error);
    g_assert_no_error(error);
    g_autofree char *f = build("Wifi", "Connect", "['Cafe', '']", &error);
    g_assert_no_error(error);
    g_assert_cmpstr(f, ==, "('Cafe', '')");

    /* 类型不对 */
    expect_invalid("Backlight", "SetBrightness", "['0.5', true]");
    expect_invalid("Backlight", "SetBrightness", "[0.5, 1]");
    expect_invalid("Backlight", "SetBrightness", "[NaN, true]");
    expect_invalid("Backlight", "SetBrightness", "[Infinity, true]");
    expect_invalid("Backlight", "SetBrightness", "[null, true]");
    expect_invalid("Backlight", "SetBrightness", "[{valueOf() { return 0.5; }}, true]");
    expect_invalid("Wifi", "SetEnabled", "['true']");
    expect_invalid("Wifi", "Forget", "[42]");
    /* 个数不对、不是数组 */
    expect_invalid("Backlight", "SetBrightness", "[0.5]");
    expect_invalid("Backlight", "SetBrightness", "[0.5, true, 1]");
    expect_invalid("Backlight", "SetBrightness", NULL);
    expect_invalid("Wifi", "Scan", "[1]");
    expect_invalid("Wifi", "Scan", "({length: 0})");
    expect_invalid("Wifi", "Forget", "'Home'");
    expect_invalid("Wifi", "Forget", "new Array(1000000)");
    /* 字符串：含 NUL、过长 */
    expect_invalid("Wifi", "Forget", "['Ho\\0me']");
    expect_invalid("Wifi", "Connect", "['Home', 'x'.repeat(257)]");
    g_autofree char *g = build("Wifi", "Connect", "['Home', 'x'.repeat(256)]", &error);
    g_assert_no_error(error);
    g_assert_nonnull(g);
    /* 孤立的代理项：要么拒绝，要么转换成合法 UTF-8。 */
    g_autofree char *h = build("Wifi", "Forget", "['\\uD800']", &error);
    g_clear_error(&error);
    if (h)
        g_assert_true(g_utf8_validate(h, -1, NULL));
}

static void
test_get_string_property(void)
{
    g_autoptr(JSCValue) obj = js("({op: 'call', n: 1, long: 'abcdef'})");
    g_autofree char *op = loong_bridge_get_string_property(obj, "op", 32);
    g_assert_cmpstr(op, ==, "call");
    g_assert_null(loong_bridge_get_string_property(obj, "n", 32));
    g_assert_null(loong_bridge_get_string_property(obj, "missing", 32));
    g_assert_null(loong_bridge_get_string_property(obj, "long", 3));
    g_autoptr(JSCValue) str = js("'call'");
    g_assert_null(loong_bridge_get_string_property(str, "length", 32));
}

static char *
to_json(GVariant *v)
{
    g_autoptr(GVariant) owned = g_variant_ref_sink(v);
    GString *s = g_string_new(NULL);
    loong_bridge_json_variant(s, owned);
    return g_string_free(s, FALSE);
}

static void
test_json(void)
{
    g_autofree char *a = to_json(g_variant_new_parsed(
        "{'Brightness': <0.25>, 'Available': <true>, 'Device': <'intel\"backlight'>, 'MaxRaw': <uint32 1000>}"));
    /* a{sv} → 对象（字典的顺序就是 GVariant 中的顺序）。 */
    g_assert_true(strstr(a, "\"Brightness\":0.25"));
    g_assert_true(strstr(a, "\"Available\":true"));
    g_assert_true(strstr(a, "\"Device\":\"intel\\\"backlight\""));
    g_assert_true(strstr(a, "\"MaxRaw\":1000"));
    g_assert_true(a[0] == '{' && a[strlen(a) - 1] == '}');

    g_autofree char *b = to_json(
        g_variant_new_parsed("([('Home', uint32 75, -45, 'psk', false, true), ('茶\\n寮', 33, -70, 'open', true, false)],)"));
    g_assert_cmpstr(b, ==, "[[[\"Home\",75,-45,\"psk\",false,true],[\"茶\\n寮\",33,-70,\"open\",true,false]]]");

    g_autofree char *c = to_json(g_variant_new_parsed("(int64 -3600, uint64 18446744073709551615, @mi nothing, "
                                                      "objectpath '/org/loong', nan, inf, <<byte 7>>)"));
    g_assert_cmpstr(c, ==, "[-3600,18446744073709551615,null,\"/org/loong\",null,null,7]");

    g_autofree char *d = to_json(g_variant_new_string("tab\there \x01 \\ end"));
    g_assert_cmpstr(d, ==, "\"tab\\there \\u0001 \\\\ end\"");

    g_autofree char *e = to_json(g_variant_new_parsed("@as []"));
    g_assert_cmpstr(e, ==, "[]");

    /* 输出一定能被 JSON 解析器读回来。 */
    for (const char *const *json = (const char *const[]){ a, b, c, d, e, NULL }; *json; json++) {
        g_autoptr(JSCValue) v = jsc_value_new_from_json(ctx, *json);
        g_assert_nonnull(v);
        g_assert_false(jsc_value_is_undefined(v));
    }
}

static void
test_json_parses(void)
{
    const char *samples[] = {
        "{'Percentage': <78.5>, 'State': <'charging'>, 'TimeToFull': <int64 2520>}",
        "('a\"b\\\\c', [1.5, 2e100, -0.0], {'k': <@as ['x', 'y']>})",
    };
    for (gsize i = 0; i < G_N_ELEMENTS(samples); i++) {
        g_autofree char *json = to_json(g_variant_new_parsed(samples[i]));
        g_autoptr(JSCValue) v = jsc_value_new_from_json(ctx, json);
        g_assert_nonnull(v);
        g_assert_false(jsc_value_is_undefined(v));
        g_autoptr(JSCException) exc = jsc_context_get_exception(ctx);
        g_assert_null(exc);
    }
}

/* ───────────────────────── URL 校验 ───────────────────────── */

static void
test_url_trusted(void)
{
    g_autoptr(GError) error = NULL;
    g_autofree char *tmp = g_dir_make_tmp("loong-bridge-XXXXXX", &error);
    g_assert_no_error(error);
    g_autofree char *root = g_build_filename(tmp, "web", NULL);
    g_autofree char *app = g_build_filename(root, "apps", "settings", NULL);
    g_autofree char *other = g_build_filename(tmp, "webx", NULL); /* 前缀相同的兄弟目录 */
    g_assert_cmpint(g_mkdir_with_parents(app, 0755), ==, 0);
    g_assert_cmpint(g_mkdir_with_parents(other, 0755), ==, 0);
    g_autofree char *index = g_build_filename(app, "index.html", NULL);
    g_autofree char *evil = g_build_filename(other, "evil.html", NULL);
    g_autofree char *outside = g_build_filename(tmp, "outside.html", NULL);
    g_autofree char *link = g_build_filename(root, "link.html", NULL);
    g_assert_true(g_file_set_contents(index, "x", -1, NULL));
    g_assert_true(g_file_set_contents(evil, "x", -1, NULL));
    g_assert_true(g_file_set_contents(outside, "x", -1, NULL));
    g_assert_cmpint(symlink(outside, link), ==, 0);

    g_autofree char *u_index = g_strconcat("file://", index, NULL);
    g_autofree char *u_index_q = g_strconcat("file://", index, "?mock=1#wifi", NULL);
    g_autofree char *u_localhost = g_strconcat("file://localhost", index, NULL);
    g_autofree char *u_dotdot = g_strconcat("file://", root, "/apps/../../outside.html", NULL);
    g_autofree char *u_encoded = g_strconcat("file://", root, "/apps/%2e%2e/%2E%2E/outside.html", NULL);
    g_autofree char *u_sibling = g_strconcat("file://", evil, NULL);
    g_autofree char *u_link = g_strconcat("file://", link, NULL);
    g_autofree char *u_missing = g_strconcat("file://", root, "/missing.html", NULL);
    g_autofree char *u_host = g_strconcat("file://example.com", index, NULL);
    g_autofree char *u_root = g_strconcat("file://", root, NULL);
    g_autofree char *root_slash = g_strconcat(root, "/", NULL);

    g_assert_true(loong_bridge_url_trusted(u_index, root));
    g_assert_true(loong_bridge_url_trusted(u_index_q, root));
    g_assert_true(loong_bridge_url_trusted(u_localhost, root));
    g_assert_true(loong_bridge_url_trusted(u_index, root_slash));
    g_assert_false(loong_bridge_url_trusted(u_dotdot, root));
    g_assert_false(loong_bridge_url_trusted(u_encoded, root));
    g_assert_false(loong_bridge_url_trusted(u_sibling, root));
    g_assert_false(loong_bridge_url_trusted(u_link, root));
    g_assert_false(loong_bridge_url_trusted(u_missing, root));
    g_assert_false(loong_bridge_url_trusted(u_host, root));
    g_assert_false(loong_bridge_url_trusted(u_root, root));
    g_assert_false(loong_bridge_url_trusted("https://example.com/usr/share/loong/web/x.html", root));
    g_assert_false(loong_bridge_url_trusted("about:blank", root));
    g_assert_false(loong_bridge_url_trusted("data:text/html,<p>x", root));
    g_assert_false(loong_bridge_url_trusted(NULL, root));
    g_assert_false(loong_bridge_url_trusted(u_index, "relative/root"));

    g_unlink(link);
    g_unlink(index);
    g_unlink(evil);
    g_unlink(outside);
    g_rmdir(app);
    g_autofree char *apps = g_path_get_dirname(app);
    g_rmdir(apps);
    g_rmdir(root);
    g_rmdir(other);
    g_rmdir(tmp);
}

/* ───────────────────────── 配置解析 ───────────────────────── */

static void
test_os_release(void)
{
    g_autoptr(GHashTable) t = loong_bridge_parse_os_release("# 注释\n"
                                                            "NAME=\"Loong OS\"\n"
                                                            "PRETTY_NAME='Loong OS 0.1.0'\n"
                                                            "VERSION_ID=0.1.0\n"
                                                            "QUOTED=\"a \\\"b\\\" \\$c\"\n"
                                                            "bad key=x\n"
                                                            "  ID=loong  \n");
    g_assert_cmpstr(g_hash_table_lookup(t, "NAME"), ==, "Loong OS");
    g_assert_cmpstr(g_hash_table_lookup(t, "PRETTY_NAME"), ==, "Loong OS 0.1.0");
    g_assert_cmpstr(g_hash_table_lookup(t, "VERSION_ID"), ==, "0.1.0");
    g_assert_cmpstr(g_hash_table_lookup(t, "QUOTED"), ==, "a \"b\" $c");
    g_assert_cmpstr(g_hash_table_lookup(t, "ID"), ==, "loong");
    g_assert_null(g_hash_table_lookup(t, "bad key"));
    g_autoptr(GHashTable) empty = loong_bridge_parse_os_release(NULL);
    g_assert_cmpuint(g_hash_table_size(empty), ==, 0);
}

static void
test_toml(void)
{
    g_autoptr(GVariant) doc = g_variant_ref_sink(loong_bridge_parse_toml("# 小米平板 5\n"
                                                                         "name = \"小米平板 5\"  # 注释\n"
                                                                         "size_class = 'regular'\n"
                                                                         "\n"
                                                                         "[screen]\n"
                                                                         "width = 2_560\n"
                                                                         "height = 1600\n"
                                                                         "diagonal_inch = 11.0 # 英寸\n"
                                                                         "hdr = false\n"
                                                                         "label = \"a\\\"b\\u9f99\"\n"
                                                                         "[[bogus]]\n"
                                                                         "skip = 1\n"
                                                                         "[screen]\n"
                                                                         "refresh_hz = 120\n"
                                                                         "broken = \"unterminated\n"
                                                                         "x y = 1\n"));
    const char *name;
    g_assert_true(g_variant_lookup(doc, "name", "&s", &name));
    g_assert_cmpstr(name, ==, "小米平板 5");
    g_assert_true(g_variant_lookup(doc, "size_class", "&s", &name));
    g_assert_cmpstr(name, ==, "regular");
    g_assert_false(g_variant_lookup(doc, "skip", "x", NULL));
    g_autoptr(GVariant) screen = g_variant_lookup_value(doc, "screen", G_VARIANT_TYPE_VARDICT);
    g_assert_nonnull(screen);
    gint64 w, hz;
    double diag;
    gboolean hdr;
    const char *label;
    g_assert_true(g_variant_lookup(screen, "width", "x", &w));
    g_assert_cmpint(w, ==, 2560);
    g_assert_true(g_variant_lookup(screen, "refresh_hz", "x", &hz));
    g_assert_cmpint(hz, ==, 120);
    g_assert_true(g_variant_lookup(screen, "diagonal_inch", "d", &diag));
    g_assert_cmpfloat(diag, ==, 11.0);
    g_assert_true(g_variant_lookup(screen, "hdr", "b", &hdr));
    g_assert_false(hdr);
    g_assert_true(g_variant_lookup(screen, "label", "&s", &label));
    g_assert_cmpstr(label, ==, "a\"b龙");
    g_assert_false(g_variant_lookup(screen, "broken", "&s", &label));

    g_autoptr(GVariant) empty = g_variant_ref_sink(loong_bridge_parse_toml(NULL));
    g_assert_cmpuint(g_variant_n_children(empty), ==, 0);
}

static void
test_appearance(void)
{
    g_assert_cmpstr(loong_appearance_parse("theme = \"dark\"\n"), ==, "dark");
    g_assert_cmpstr(loong_appearance_parse("# x\ntheme = 'dark' # 夜墨\n"), ==, "dark");
    g_assert_cmpstr(loong_appearance_parse("theme = \"light\""), ==, "light");
    g_assert_cmpstr(loong_appearance_parse("theme = \"sepia\""), ==, "light");
    g_assert_cmpstr(loong_appearance_parse("theme = dark"), ==, "light");
    g_assert_cmpstr(loong_appearance_parse(""), ==, "light");
    g_assert_cmpstr(loong_appearance_parse(NULL), ==, "light");
    g_assert_true(loong_appearance_valid("dark"));
    g_assert_false(loong_appearance_valid("auto"));
    g_assert_false(loong_appearance_valid(NULL));
    for (const char *const *t = (const char *const[]){ "light", "dark", NULL }; *t; t++) {
        g_autofree char *text = loong_appearance_format(*t);
        g_assert_cmpstr(loong_appearance_parse(text), ==, *t);
    }
    g_autofree char *bogus = loong_appearance_format("\"; rm -rf /");
    g_assert_cmpstr(loong_appearance_parse(bogus), ==, "light");
}

int
main(int argc, char **argv)
{
    g_test_init(&argc, &argv, NULL);
    ctx = jsc_context_new();
    g_test_add_func("/bridge/allowlist", test_allowlist);
    g_test_add_func("/bridge/build-args", test_build_args);
    g_test_add_func("/bridge/string-property", test_get_string_property);
    g_test_add_func("/bridge/json", test_json);
    g_test_add_func("/bridge/json-parses", test_json_parses);
    g_test_add_func("/bridge/url-trusted", test_url_trusted);
    g_test_add_func("/bridge/os-release", test_os_release);
    g_test_add_func("/bridge/toml", test_toml);
    g_test_add_func("/bridge/appearance", test_appearance);
    int status = g_test_run();
    g_object_unref(ctx);
    return status;
}
