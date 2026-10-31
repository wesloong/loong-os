/*
 * loong-bridge-core：JS bridge 的白名单、参数校验、GVariant → JSON、URL 校验与配置解析。
 * 见 loong-bridge-core.h。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include "loong-bridge-core.h"

#include <math.h>
#include <stdlib.h>
#include <string.h>

G_DEFINE_QUARK(loong-bridge-error-quark, loong_bridge_error)

const char *
loong_bridge_error_name(int code)
{
    switch (code) {
    case LOONG_BRIDGE_ERR_INVALID_ARGS:
        return LOONG_BRIDGE_ERROR_INVALID_ARGS;
    case LOONG_BRIDGE_ERR_NOT_ALLOWED:
        return LOONG_BRIDGE_ERROR_NOT_ALLOWED;
    case LOONG_BRIDGE_ERR_UNAVAILABLE:
        return LOONG_BRIDGE_ERROR_UNAVAILABLE;
    default:
        return LOONG_BRIDGE_ERROR_FAILED;
    }
}

/* ───────────────────────── 白名单 ─────────────────────────
 * 只有这里列出的接口与方法可以从页面调用。新增条目前先确认：
 *   1. loong-sysd 内部做了权限复核（access.rs）与参数校验；
 *   2. 对普通用户也无害，或者只有系统应用拿得到 bridge（本来就是如此）。
 * 不暴露 org.freedesktop.DBus.Properties.Set、内省、Agent 接口与任何其他服务。 */

static const LoongBridgeInterface interfaces[] = {
    { "Backlight", "org.loong.System1.Backlight", "/org/loong/System1/Backlight" },
    { "Power", "org.loong.System1.Power", "/org/loong/System1/Power" },
    { "Wifi", "org.loong.System1.Wifi", "/org/loong/System1/Wifi" },
};

static const LoongBridgeMethod methods[] = {
    { "Backlight", "SetBrightness", "db", "()", -1 },
    { "Power", "Refresh", "", "()", -1 },
    { "Wifi", "GetNetworks", "", "(a(suisbb))", -1 },
    { "Wifi", "SetEnabled", "b", "()", -1 },
    { "Wifi", "Scan", "", "()", -1 },
    /* Connect 要等到连上或失败才返回（iwd 的握手与 DHCP 可能要十几秒）。 */
    { "Wifi", "Connect", "ss", "()", 60000 },
    { "Wifi", "Disconnect", "", "()", -1 },
    { "Wifi", "Forget", "s", "()", -1 },
};

const LoongBridgeInterface *
loong_bridge_find_interface(const char *name)
{
    if (!name)
        return NULL;
    for (gsize i = 0; i < G_N_ELEMENTS(interfaces); i++) {
        if (strcmp(interfaces[i].name, name) == 0)
            return &interfaces[i];
    }
    return NULL;
}

const LoongBridgeInterface *
loong_bridge_find_interface_by_dbus_name(const char *interface)
{
    if (!interface)
        return NULL;
    for (gsize i = 0; i < G_N_ELEMENTS(interfaces); i++) {
        if (strcmp(interfaces[i].interface, interface) == 0)
            return &interfaces[i];
    }
    return NULL;
}

const LoongBridgeMethod *
loong_bridge_find_method(const char *iface, const char *method)
{
    if (!iface || !method)
        return NULL;
    for (gsize i = 0; i < G_N_ELEMENTS(methods); i++) {
        if (strcmp(methods[i].iface, iface) == 0 && strcmp(methods[i].method, method) == 0)
            return &methods[i];
    }
    return NULL;
}

/* ───────────────────────── 参数校验 ───────────────────────── */

/* JS 字符串 → 新分配的 UTF-8 C 字符串；不是字符串、太长、含 NUL 时返回 NULL。 */
static char *
js_string(JSCValue *value, gsize max_len)
{
    if (!value || !jsc_value_is_string(value))
        return NULL;
    g_autoptr(GBytes) bytes = jsc_value_to_string_as_bytes(value);
    gsize len = 0;
    const char *data = bytes ? g_bytes_get_data(bytes, &len) : NULL;
    if (!data)
        return g_strdup("");
    /* 有的实现会把结尾的 NUL 计入长度。 */
    if (len > 0 && data[len - 1] == '\0')
        len--;
    if (len > max_len || memchr(data, '\0', len) || !g_utf8_validate_len(data, len, NULL))
        return NULL;
    return g_strndup(data, len);
}

char *
loong_bridge_get_string_property(JSCValue *object, const char *name, gsize max_len)
{
    if (!object || !jsc_value_is_object(object) || !jsc_value_object_has_property(object, name))
        return NULL;
    g_autoptr(JSCValue) value = jsc_value_object_get_property(object, name);
    return js_string(value, max_len);
}

GVariant *
loong_bridge_build_args(const LoongBridgeMethod *method, JSCValue *args, GError **error)
{
    gsize want = strlen(method->in_sig);
    gsize have = 0;
    gboolean empty = !args || jsc_value_is_undefined(args) || jsc_value_is_null(args);
    if (!empty) {
        if (!jsc_value_is_array(args)) {
            g_set_error(error, LOONG_BRIDGE_ERROR, LOONG_BRIDGE_ERR_INVALID_ARGS, "args 必须是数组");
            return NULL;
        }
        g_autoptr(JSCValue) length = jsc_value_object_get_property(args, "length");
        double n = jsc_value_to_double(length);
        have = (n >= 0 && n < 64) ? (gsize)n : 64;
    }
    if (have != want) {
        g_set_error(error, LOONG_BRIDGE_ERROR, LOONG_BRIDGE_ERR_INVALID_ARGS, "%s.%s 需要 %zu 个参数，实际 %zu 个",
                    method->iface, method->method, want, have);
        return NULL;
    }

    GVariantBuilder builder;
    g_variant_builder_init(&builder, G_VARIANT_TYPE_TUPLE);
    for (gsize i = 0; i < want; i++) {
        g_autoptr(JSCValue) v = jsc_value_object_get_property_at_index(args, (guint)i);
        GVariant *item = NULL;
        const char *expect = "";
        switch (method->in_sig[i]) {
        case 'd':
            expect = "有限的数字";
            if (jsc_value_is_number(v)) {
                double d = jsc_value_to_double(v);
                if (isfinite(d))
                    item = g_variant_new_double(d);
            }
            break;
        case 'b':
            expect = "布尔值";
            if (jsc_value_is_boolean(v))
                item = g_variant_new_boolean(jsc_value_to_boolean(v));
            break;
        case 's': {
            expect = "字符串（不超过 256 字节）";
            g_autofree char *s = js_string(v, LOONG_BRIDGE_MAX_STRING);
            if (s)
                item = g_variant_new_string(s);
            break;
        }
        default:
            g_assert_not_reached();
        }
        if (!item) {
            g_variant_builder_clear(&builder);
            g_set_error(error, LOONG_BRIDGE_ERROR, LOONG_BRIDGE_ERR_INVALID_ARGS, "%s.%s 的第 %zu 个参数应为%s",
                        method->iface, method->method, i + 1, expect);
            return NULL;
        }
        g_variant_builder_add_value(&builder, item);
    }
    return g_variant_builder_end(&builder);
}

/* ───────────────────────── GVariant → JSON ───────────────────────── */

void
loong_bridge_json_string(GString *out, const char *s)
{
    g_string_append_c(out, '"');
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        switch (*p) {
        case '"':
            g_string_append(out, "\\\"");
            break;
        case '\\':
            g_string_append(out, "\\\\");
            break;
        case '\n':
            g_string_append(out, "\\n");
            break;
        case '\r':
            g_string_append(out, "\\r");
            break;
        case '\t':
            g_string_append(out, "\\t");
            break;
        default:
            if (*p < 0x20)
                g_string_append_printf(out, "\\u%04x", *p);
            else
                g_string_append_c(out, (char)*p);
        }
    }
    g_string_append_c(out, '"');
}

static void
json_double(GString *out, double d)
{
    if (!isfinite(d)) {
        g_string_append(out, "null");
        return;
    }
    char buf[G_ASCII_DTOSTR_BUF_SIZE];
    g_string_append(out, g_ascii_dtostr(buf, sizeof buf, d));
}

void
loong_bridge_json_variant(GString *out, GVariant *value)
{
    switch (g_variant_classify(value)) {
    case G_VARIANT_CLASS_BOOLEAN:
        g_string_append(out, g_variant_get_boolean(value) ? "true" : "false");
        break;
    case G_VARIANT_CLASS_BYTE:
        g_string_append_printf(out, "%u", g_variant_get_byte(value));
        break;
    case G_VARIANT_CLASS_INT16:
        g_string_append_printf(out, "%d", g_variant_get_int16(value));
        break;
    case G_VARIANT_CLASS_UINT16:
        g_string_append_printf(out, "%u", g_variant_get_uint16(value));
        break;
    case G_VARIANT_CLASS_INT32:
        g_string_append_printf(out, "%" G_GINT32_FORMAT, g_variant_get_int32(value));
        break;
    case G_VARIANT_CLASS_UINT32:
        g_string_append_printf(out, "%" G_GUINT32_FORMAT, g_variant_get_uint32(value));
        break;
    case G_VARIANT_CLASS_INT64:
        g_string_append_printf(out, "%" G_GINT64_FORMAT, g_variant_get_int64(value));
        break;
    case G_VARIANT_CLASS_UINT64:
        g_string_append_printf(out, "%" G_GUINT64_FORMAT, g_variant_get_uint64(value));
        break;
    case G_VARIANT_CLASS_DOUBLE:
        json_double(out, g_variant_get_double(value));
        break;
    case G_VARIANT_CLASS_STRING:
    case G_VARIANT_CLASS_OBJECT_PATH:
    case G_VARIANT_CLASS_SIGNATURE:
        loong_bridge_json_string(out, g_variant_get_string(value, NULL));
        break;
    case G_VARIANT_CLASS_VARIANT: {
        g_autoptr(GVariant) inner = g_variant_get_variant(value);
        loong_bridge_json_variant(out, inner);
        break;
    }
    case G_VARIANT_CLASS_MAYBE: {
        g_autoptr(GVariant) inner = g_variant_get_maybe(value);
        if (inner)
            loong_bridge_json_variant(out, inner);
        else
            g_string_append(out, "null");
        break;
    }
    case G_VARIANT_CLASS_ARRAY: {
        const GVariantType *elem = g_variant_type_element(g_variant_get_type(value));
        gboolean object = g_variant_type_is_dict_entry(elem) &&
                          (g_variant_type_equal(g_variant_type_key(elem), G_VARIANT_TYPE_STRING) ||
                           g_variant_type_equal(g_variant_type_key(elem), G_VARIANT_TYPE_OBJECT_PATH));
        g_string_append_c(out, object ? '{' : '[');
        gsize n = g_variant_n_children(value);
        for (gsize i = 0; i < n; i++) {
            g_autoptr(GVariant) child = g_variant_get_child_value(value, i);
            if (i)
                g_string_append_c(out, ',');
            if (object) {
                g_autoptr(GVariant) k = g_variant_get_child_value(child, 0);
                g_autoptr(GVariant) v = g_variant_get_child_value(child, 1);
                loong_bridge_json_string(out, g_variant_get_string(k, NULL));
                g_string_append_c(out, ':');
                loong_bridge_json_variant(out, v);
            } else {
                loong_bridge_json_variant(out, child);
            }
        }
        g_string_append_c(out, object ? '}' : ']');
        break;
    }
    case G_VARIANT_CLASS_TUPLE:
    case G_VARIANT_CLASS_DICT_ENTRY: {
        g_string_append_c(out, '[');
        gsize n = g_variant_n_children(value);
        for (gsize i = 0; i < n; i++) {
            g_autoptr(GVariant) child = g_variant_get_child_value(value, i);
            if (i)
                g_string_append_c(out, ',');
            loong_bridge_json_variant(out, child);
        }
        g_string_append_c(out, ']');
        break;
    }
    case G_VARIANT_CLASS_HANDLE:
    default:
        g_string_append(out, "null");
        break;
    }
}

/* ───────────────────────── URL 校验 ───────────────────────── */

gboolean
loong_bridge_url_trusted(const char *url, const char *root_dir)
{
    if (!url || !root_dir || !g_path_is_absolute(root_dir))
        return FALSE;
    g_autoptr(GUri) uri = g_uri_parse(url, G_URI_FLAGS_NONE, NULL);
    if (!uri || g_strcmp0(g_uri_get_scheme(uri), "file") != 0)
        return FALSE;
    const char *host = g_uri_get_host(uri);
    if (host && *host && g_ascii_strcasecmp(host, "localhost") != 0)
        return FALSE;
    /* G_URI_FLAGS_NONE：路径已经过 %-解码。 */
    const char *path = g_uri_get_path(uri);
    if (!path || !g_path_is_absolute(path))
        return FALSE;
    /* realpath 同时消除 `..`、`.` 与符号链接；文件不存在时失败。 */
    char *real = realpath(path, NULL);
    if (!real)
        return FALSE;
    gsize n = strlen(root_dir);
    while (n > 1 && root_dir[n - 1] == '/')
        n--;
    gboolean ok = strncmp(real, root_dir, n) == 0 && real[n] == '/';
    free(real);
    return ok;
}

/* ───────────────────────── os-release ───────────────────────── */

GHashTable *
loong_bridge_parse_os_release(const char *text)
{
    GHashTable *table = g_hash_table_new_full(g_str_hash, g_str_equal, g_free, g_free);
    if (!text)
        return table;
    g_auto(GStrv) lines = g_strsplit(text, "\n", -1);
    for (char **l = lines; *l; l++) {
        char *line = g_strstrip(*l);
        if (!*line || *line == '#')
            continue;
        char *eq = strchr(line, '=');
        if (!eq || eq == line)
            continue;
        *eq = '\0';
        const char *key = line;
        gboolean valid_key = TRUE;
        for (const char *k = key; *k; k++) {
            if (!g_ascii_isalnum(*k) && *k != '_')
                valid_key = FALSE;
        }
        if (!valid_key)
            continue;
        const char *v = eq + 1;
        GString *value = g_string_new(NULL);
        if (*v == '"' || *v == '\'') {
            char quote = *v++;
            for (; *v && *v != quote; v++) {
                if (quote == '"' && *v == '\\' && v[1])
                    v++;
                g_string_append_c(value, *v);
            }
        } else {
            g_string_append(value, v);
        }
        char *s = g_string_free(value, FALSE);
        if (!g_utf8_validate(s, -1, NULL)) {
            char *valid = g_utf8_make_valid(s, -1);
            g_free(s);
            s = valid;
        }
        g_hash_table_replace(table, g_strdup(key), s);
    }
    return table;
}

/* ───────────────────────── TOML 子集 ───────────────────────── */

static gboolean
toml_bare_key(const char *k)
{
    if (!*k)
        return FALSE;
    for (; *k; k++) {
        if (!g_ascii_isalnum(*k) && *k != '_' && *k != '-')
            return FALSE;
    }
    return TRUE;
}

/* 解析一个值；成功返回浮动引用的 GVariant。 */
static GVariant *
toml_value(const char *v)
{
    if (*v == '"') {
        GString *s = g_string_new(NULL);
        for (v++; *v && *v != '"'; v++) {
            if (*v == '\\' && v[1]) {
                v++;
                switch (*v) {
                case 'n':
                    g_string_append_c(s, '\n');
                    break;
                case 't':
                    g_string_append_c(s, '\t');
                    break;
                case 'r':
                    g_string_append_c(s, '\r');
                    break;
                case 'u': {
                    char hex[5] = { 0 };
                    if (strlen(v + 1) >= 4) {
                        memcpy(hex, v + 1, 4);
                        g_string_append_unichar(s, (gunichar)strtoul(hex, NULL, 16));
                        v += 4;
                    }
                    break;
                }
                default:
                    g_string_append_c(s, *v);
                }
                continue;
            }
            g_string_append_c(s, *v);
        }
        if (*v != '"' || !g_utf8_validate(s->str, -1, NULL)) {
            g_string_free(s, TRUE);
            return NULL;
        }
        g_autofree char *str = g_string_free(s, FALSE);
        return g_variant_new_string(str);
    }
    if (*v == '\'') {
        const char *end = strchr(v + 1, '\'');
        if (!end)
            return NULL;
        g_autofree char *str = g_strndup(v + 1, end - v - 1);
        return g_utf8_validate(str, -1, NULL) ? g_variant_new_string(str) : NULL;
    }
    /* 去掉行尾注释与空白。 */
    g_autofree char *tok = g_strdup(v);
    char *hash = strchr(tok, '#');
    if (hash)
        *hash = '\0';
    g_strstrip(tok);
    if (strcmp(tok, "true") == 0)
        return g_variant_new_boolean(TRUE);
    if (strcmp(tok, "false") == 0)
        return g_variant_new_boolean(FALSE);
    /* 数字可以带下划线分隔（1_000）。 */
    g_autofree char *num = g_strdup(tok);
    char *w = num;
    for (const char *r = tok; *r; r++) {
        if (*r != '_')
            *w++ = *r;
    }
    *w = '\0';
    if (!*num)
        return NULL;
    gint64 i;
    if (g_ascii_string_to_signed(num, 10, G_MININT64, G_MAXINT64, &i, NULL))
        return g_variant_new_int64(i);
    char *end = NULL;
    double d = g_ascii_strtod(num, &end);
    if (end && *end == '\0' && isfinite(d))
        return g_variant_new_double(d);
    return NULL;
}

GVariant *
loong_bridge_parse_toml(const char *text)
{
    GVariantDict root;
    g_variant_dict_init(&root, NULL);
    /* 表名 → GVariantDict*（按出现顺序记录名字，最后放回根上）。 */
    g_autoptr(GHashTable) tables = g_hash_table_new_full(g_str_hash, g_str_equal, g_free,
                                                         (GDestroyNotify)g_variant_dict_unref);
    GVariantDict *current = NULL; /* NULL = 根 */
    gboolean skipping = FALSE;    /* 不支持的 [[数组表]] */

    g_auto(GStrv) lines = g_strsplit(text ? text : "", "\n", -1);
    for (char **l = lines; *l; l++) {
        char *line = g_strstrip(*l);
        if (!*line || *line == '#')
            continue;
        if (*line == '[') {
            skipping = line[1] == '[';
            char *close = strchr(line, ']');
            if (skipping || !close)
                continue;
            *close = '\0';
            g_autofree char *name = g_strstrip(g_strdup(line + 1));
            if (!toml_bare_key(name)) {
                skipping = TRUE;
                continue;
            }
            current = g_hash_table_lookup(tables, name);
            if (!current) {
                current = g_variant_dict_new(NULL);
                g_hash_table_insert(tables, g_strdup(name), current);
            }
            continue;
        }
        if (skipping)
            continue;
        char *eq = strchr(line, '=');
        if (!eq)
            continue;
        *eq = '\0';
        char *key = g_strstrip(line);
        char *val = g_strstrip(eq + 1);
        if (!toml_bare_key(key))
            continue;
        GVariant *v = toml_value(val);
        if (!v)
            continue;
        g_variant_dict_insert_value(current ? current : &root, key, v);
    }

    GHashTableIter it;
    gpointer name, dict;
    g_hash_table_iter_init(&it, tables);
    while (g_hash_table_iter_next(&it, &name, &dict))
        g_variant_dict_insert_value(&root, name, g_variant_dict_end(dict));
    return g_variant_dict_end(&root);
}

/* ───────────────────────── 外观 ───────────────────────── */

gboolean
loong_appearance_valid(const char *theme)
{
    return g_strcmp0(theme, "light") == 0 || g_strcmp0(theme, "dark") == 0;
}

const char *
loong_appearance_parse(const char *text)
{
    g_autoptr(GVariant) doc = g_variant_ref_sink(loong_bridge_parse_toml(text));
    const char *theme = NULL;
    if (g_variant_lookup(doc, "theme", "&s", &theme) && g_strcmp0(theme, "dark") == 0)
        return "dark";
    return "light";
}

char *
loong_appearance_format(const char *theme)
{
    return g_strdup_printf("# Loong OS 外观：light（宣纸）或 dark（夜墨）。\n"
                           "# 由“设置”写入；loong-shell 与所有 Web 应用读取，见 docs/api/loong-js-bridge.md。\n"
                           "theme = \"%s\"\n",
                           loong_appearance_valid(theme) ? theme : "light");
}
