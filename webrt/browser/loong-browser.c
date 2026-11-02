/*
 * loong-browser：Loong OS 的系统浏览器。
 *
 *   loong-browser [--theme light|dark] [--search-url URL] [--windowed WxH] [URL...]
 *
 * - 运行在自写的 loong WPEPlatform 后端上（webrt/platform/）：一个窗口里同时有两类 WebView，
 *   各占一个 wl_subsurface——
 *     界面（chrome）：本地页面 webrt/browser/ui/index.html（LoongKit「墨韵」），在最上层；
 *     网页内容：每个标签页一个 WebView，只有当前标签页的子表面挂在窗口上。
 * - 界面与本进程通过 script message handler “loong” 通信：界面发命令（前往、后退、标签页……），
 *   本进程把状态（标题、地址、进度、能否后退、theme-color、图标）推给界面。
 * - 界面平时只占顶部的工具栏；弹出菜单、对话框、<select> 时临时铺满窗口（透明背景），
 *   盖在网页内容之上。
 * - 数据目录 $XDG_DATA_HOME/loong/browser/，缓存 $XDG_CACHE_HOME/loong/browser/。
 * - “添加到主屏幕”：读取网页的 W3C manifest，规范化后写入
 *   $XDG_DATA_HOME/loong/webapps/<id>/manifest.json（系统界面据此生成图标，见 ADR 0003）。
 * - 单实例：第二次启动时把网址交给已运行的浏览器（$XDG_RUNTIME_DIR/loong-browser.sock），
 *   在新标签页中打开，不依赖 D-Bus。
 * - 系统状态栏叠在界面顶部的 24pt 留白上：经 Loong 私有协议 loong_app_v1 告诉 loong-shell
 *   用哪种墨——平时按界面的外观（宣纸 → 浓墨，夜墨 → 月白）；网页全屏时顶部是网页，
 *   交给合成器按网页的 theme-color 决定（没有时合成器取样或按系统外观）。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#include <errno.h>
#include <gio/gio.h>
#include <gio/gunixsocketaddress.h>
#include <glib/gstdio.h>
#include <locale.h>
#include <stdio.h>
#include <string.h>
#include <wpe/webkit.h>
#include <wpe/wpe-platform.h>

#include "loong-platform.h"
#include "loong-appearance.h"

#define APP_ID "loong.browser"
#define DEFAULT_SEARCH_URL "https://www.bing.com/search?q=%s"
#define CHROME_LAYER 1
#define CONTENT_LAYER 0
#define STATUS_BAR_HEIGHT 24     /* 系统状态栏叠在应用之上（设计规范 §8.2） */
#define DEFAULT_CHROME_HEIGHT 76 /* 状态栏 24 + 工具栏 52 */
#define PAPER_LIGHT 0xf3eee2
#define PAPER_DARK 0x15140f

typedef struct _Browser Browser;

typedef struct {
    Browser *browser;
    guint id;
    WebKitWebView *web_view;
    LoongView *view;
    char *favicon;
    char *manifest;
    gboolean error_page;
} Tab;

struct _Browser {
    GApplication *app;
    WPEDisplay *display;
    WPEToplevel *toplevel;
    WebKitNetworkSession *session;
    WebKitSettings *content_settings;
    WebKitUserContentManager *content_ucm;
    WebKitWebView *chrome;
    LoongView *chrome_view;
    GPtrArray *tabs; /* Tab*，按标签栏顺序 */
    Tab *active;
    guint next_tab_id;
    int chrome_height;
    gboolean expanded;
    gboolean fullscreen; /* 网页全屏（视频等）：内容铺满窗口，界面隐藏 */
    gboolean quitting;
    gboolean chrome_ready;
    guint push_idle;

    char *ui_dir;
    char *chrome_url;
    char *start_url;
    char *search_url;
    gboolean dark;
    LoongAppearance *appearance; /* 跟随系统外观（没有用 --theme / LOONG_THEME 固定时） */
    gboolean windowed;
    int windowed_width, windowed_height;
    char **initial_urls;

    /* 界面里正在显示的模态（同一时刻最多一个） */
    guint modal_id;
    WebKitScriptDialog *dialog;
    WebKitOptionMenu *menu;

    GSocketService *socket_service;
    char *socket_path;
};

static void push_state(Browser *b);
static Tab *tab_new(Browser *b, WebKitWebView *related, const char *url, gboolean activate);
static void tab_activate(Browser *b, Tab *tab);
static void update_status_bar(Browser *b);

/* ───────────────────────── 小工具 ───────────────────────── */

static void
json_string(GString *s, const char *value)
{
    if (!value) {
        g_string_append(s, "null");
        return;
    }
    g_string_append_c(s, '"');
    for (const unsigned char *p = (const unsigned char *)value; *p; p++) {
        switch (*p) {
        case '"':
            g_string_append(s, "\\\"");
            break;
        case '\\':
            g_string_append(s, "\\\\");
            break;
        case '\n':
            g_string_append(s, "\\n");
            break;
        case '\r':
            g_string_append(s, "\\r");
            break;
        case '\t':
            g_string_append(s, "\\t");
            break;
        case '<': /* 避免在任何上下文里出现 </script> */
            g_string_append(s, "\\u003c");
            break;
        default:
            if (*p < 0x20)
                g_string_append_printf(s, "\\u%04x", *p);
            else
                g_string_append_c(s, *p);
        }
    }
    g_string_append_c(s, '"');
}

static void
chrome_call(Browser *b, const char *function, const char *json)
{
    if (!b->chrome_ready)
        return;
    g_autofree char *script = g_strdup_printf("window.loongChrome && loongChrome.%s(%s)", function, json ? json : "");
    webkit_web_view_evaluate_javascript(b->chrome, script, -1, NULL, NULL, NULL, NULL, NULL);
}

static char *
js_string(JSCValue *object, const char *name)
{
    g_autoptr(JSCValue) value = jsc_value_object_get_property(object, name);
    return value && jsc_value_is_string(value) ? jsc_value_to_string(value) : NULL;
}

static gint64
js_int(JSCValue *object, const char *name, gint64 fallback)
{
    g_autoptr(JSCValue) value = jsc_value_object_get_property(object, name);
    return value && jsc_value_is_number(value) ? (gint64)jsc_value_to_double(value) : fallback;
}

static gboolean
js_bool(JSCValue *object, const char *name)
{
    g_autoptr(JSCValue) value = jsc_value_object_get_property(object, name);
    return value && jsc_value_to_boolean(value);
}

static gboolean
is_ipv4(const char *host)
{
    int dots = 0;
    for (const char *p = host; *p; p++) {
        if (*p == '.')
            dots++;
        else if (!g_ascii_isdigit(*p))
            return FALSE;
    }
    return dots == 3;
}

/*
 * 地址栏输入 → 网址：
 *   有协议（https://、file:、about: …）的原样使用；
 *   像主机名的（含“.”或是 localhost，没有空格）补协议：本机与 IP 用 http，其余 https；
 *   其余作为搜索词交给搜索引擎（search_url 中的 %s 替换为转义后的关键词）。
 */
static char *
resolve_input(const char *input, const char *search_url)
{
    g_autofree char *text = g_strstrip(g_strdup(input ? input : ""));
    if (!*text)
        return NULL;
    static const char *const schemes[] = { "http:", "https:", "file:", "about:", "data:", "blob:", NULL };
    for (int i = 0; schemes[i]; i++) {
        if (g_ascii_strncasecmp(text, schemes[i], strlen(schemes[i])) == 0)
            return g_steal_pointer(&text);
    }
    gboolean has_space = strpbrk(text, " \t") != NULL;
    if (!has_space && strstr(text, "://"))
        return g_steal_pointer(&text);
    if (!has_space) {
        /* 主机部分：到第一个 / ? # 为止，再去掉端口。 */
        size_t host_len = strcspn(text, "/?#");
        g_autofree char *hostport = g_strndup(text, host_len);
        char *colon = strrchr(hostport, ':');
        gboolean has_port = FALSE;
        if (colon && colon[1] && strspn(colon + 1, "0123456789") == strlen(colon + 1)) {
            *colon = '\0';
            has_port = TRUE;
        }
        const char *host = hostport;
        gboolean local = !g_ascii_strcasecmp(host, "localhost") || is_ipv4(host);
        gboolean dotted = strchr(host, '.') && host[0] != '.' && host[strlen(host) - 1] != '.';
        if (local || (dotted && !strchr(host, '_')) || (has_port && *host))
            return g_strconcat(local || has_port ? "http://" : "https://", text, NULL);
    }
    g_autofree char *escaped = g_uri_escape_string(text, NULL, TRUE);
    const char *mark = strstr(search_url, "%s");
    if (!mark)
        return g_strconcat(search_url, escaped, NULL);
    return g_strdup_printf("%.*s%s%s", (int)(mark - search_url), search_url, escaped, mark + 2);
}

/* ───────────────────────── 布局 ───────────────────────── */

/* WebKitColor → 0xAARRGGBB（没有或全透明时为 0）。 */
static guint32
color_argb(const WebKitColor *c)
{
    if (c->alpha <= 0)
        return 0;
    guint32 a = (guint32)(CLAMP(c->alpha, 0, 1) * 255 + 0.5);
    guint32 r = (guint32)(CLAMP(c->red, 0, 1) * 255 + 0.5);
    guint32 g = (guint32)(CLAMP(c->green, 0, 1) * 255 + 0.5);
    guint32 bl = (guint32)(CLAMP(c->blue, 0, 1) * 255 + 0.5);
    return a << 24 | r << 16 | g << 8 | bl;
}

/* 状态栏的墨（loong_app_v1）：平时窗口顶部是界面（宣纸 / 夜墨，随外观），直接指定样式；
 * 网页全屏时顶部是网页，改回 auto，主题色用网页的 theme-color（没有时为 0，合成器取样或按系统外观）。 */
static void
update_status_bar(Browser *b)
{
    if (!b->toplevel)
        return;
    LoongToplevel *toplevel = LOONG_TOPLEVEL(b->toplevel);
    if (b->fullscreen && b->active) {
        WebKitColor color;
        guint32 argb = webkit_web_view_get_theme_color(b->active->web_view, &color) ? color_argb(&color) : 0;
        loong_toplevel_set_status_bar_style(toplevel, LOONG_STATUS_BAR_AUTO);
        loong_toplevel_set_theme_color(toplevel, argb);
    } else {
        loong_toplevel_set_status_bar_style(toplevel, b->dark ? LOONG_STATUS_BAR_LIGHT_CONTENT : LOONG_STATUS_BAR_DARK_CONTENT);
        loong_toplevel_set_theme_color(toplevel, 0xff000000 | (b->dark ? PAPER_DARK : PAPER_LIGHT));
    }
}

static void
layout(Browser *b)
{
    int width, height;
    wpe_toplevel_get_size(b->toplevel, &width, &height);
    int chrome_h = CLAMP(b->chrome_height, 1, height);
    loong_view_set_geometry(b->chrome_view, 0, 0, width, b->expanded ? height : chrome_h);
    wpe_view_set_visible(WPE_VIEW(b->chrome_view), !b->fullscreen);
    for (guint i = 0; i < b->tabs->len; i++) {
        Tab *tab = g_ptr_array_index(b->tabs, i);
        if (b->fullscreen && tab == b->active)
            loong_view_set_geometry(tab->view, 0, 0, width, height);
        else
            loong_view_set_geometry(tab->view, 0, chrome_h, width, MAX(1, height - chrome_h));
    }
    update_status_bar(b);
}

static gboolean
on_enter_fullscreen(WebKitWebView *web_view, Browser *b)
{
    if (!b->active || b->active->web_view != web_view)
        return TRUE; /* 后台标签页不许全屏 */
    b->fullscreen = TRUE;
    layout(b);
    return FALSE; /* 继续默认处理：WebKit 让窗口全屏（平板上本来就是） */
}

static gboolean
on_leave_fullscreen(WebKitWebView *web_view, Browser *b)
{
    b->fullscreen = FALSE;
    layout(b);
    return FALSE;
}

static void
on_size_changed(LoongToplevel *toplevel, int width, int height, Browser *b)
{
    layout(b);
}

/* ───────────────────────── 状态推送 ───────────────────────── */

static Tab *
find_tab(Browser *b, guint id)
{
    for (guint i = 0; i < b->tabs->len; i++) {
        Tab *tab = g_ptr_array_index(b->tabs, i);
        if (tab->id == id)
            return tab;
    }
    return NULL;
}

static Tab *
tab_for_view(Browser *b, WebKitWebView *web_view)
{
    for (guint i = 0; i < b->tabs->len; i++) {
        Tab *tab = g_ptr_array_index(b->tabs, i);
        if (tab->web_view == web_view)
            return tab;
    }
    return NULL;
}

static gboolean
tab_is_start(Tab *tab)
{
    const char *uri = webkit_web_view_get_uri(tab->web_view);
    return !uri || !*uri || !strcmp(uri, "about:blank") || !strcmp(uri, tab->browser->start_url);
}

static void
append_tab_json(GString *s, Tab *tab)
{
    WebKitWebView *v = tab->web_view;
    gboolean start = tab_is_start(tab);
    const char *title = webkit_web_view_get_title(v);
    g_string_append_printf(s, "{\"id\":%u,\"title\":", tab->id);
    json_string(s, start ? "新标签页" : (title && *title ? title : NULL));
    g_string_append(s, ",\"url\":");
    json_string(s, start ? "" : webkit_web_view_get_uri(v));
    g_string_append(s, ",\"favicon\":");
    json_string(s, start ? NULL : tab->favicon);
    g_string_append_printf(s, ",\"loading\":%s,\"progress\":%.3f,\"start\":%s,\"error\":%s}",
                           webkit_web_view_is_loading(v) ? "true" : "false", webkit_web_view_get_estimated_load_progress(v),
                           start ? "true" : "false", tab->error_page ? "true" : "false");
}

static gboolean
push_state_idle(gpointer data)
{
    Browser *b = data;
    b->push_idle = 0;
    /* 当前标签页或它的 theme-color 变了：网页全屏时状态栏跟着变。 */
    update_status_bar(b);
    if (!b->chrome_ready || !b->active)
        return G_SOURCE_REMOVE;
    WebKitWebView *v = b->active->web_view;
    GString *s = g_string_new("{\"tabs\":[");
    for (guint i = 0; i < b->tabs->len; i++) {
        if (i)
            g_string_append_c(s, ',');
        append_tab_json(s, g_ptr_array_index(b->tabs, i));
    }
    g_string_append_printf(s, "],\"active\":%u,\"canGoBack\":%s,\"canGoForward\":%s", b->active->id,
                           webkit_web_view_can_go_back(v) ? "true" : "false", webkit_web_view_can_go_forward(v) ? "true" : "false");
    GTlsCertificateFlags errors = 0;
    gboolean https = webkit_web_view_get_tls_info(v, NULL, &errors);
    g_string_append_printf(s, ",\"secure\":%s", https && !errors ? "true" : "false");
    WebKitColor color;
    if (!tab_is_start(b->active) && webkit_web_view_get_theme_color(v, &color) && color.alpha > 0) {
        g_string_append_printf(s, ",\"themeColor\":\"rgb(%d %d %d)\"", (int)(color.red * 255), (int)(color.green * 255),
                               (int)(color.blue * 255));
    } else {
        g_string_append(s, ",\"themeColor\":null");
    }
    g_string_append_printf(s, ",\"canInstall\":%s,\"statusBar\":%d}", tab_is_start(b->active) || b->active->error_page ? "false" : "true",
                           b->windowed ? 0 : STATUS_BAR_HEIGHT);
    chrome_call(b, "update", s->str);
    g_string_free(s, TRUE);

    /* 窗口标题跟随当前标签页。 */
    const char *title = webkit_web_view_get_title(v);
    wpe_toplevel_set_title(b->toplevel, title && *title && !tab_is_start(b->active) ? title : "浏览器");
    return G_SOURCE_REMOVE;
}

static void
push_state(Browser *b)
{
    if (!b->push_idle)
        b->push_idle = g_idle_add(push_state_idle, b);
}

static void
toast(Browser *b, const char *kind, const char *text)
{
    GString *s = g_string_new("{\"kind\":");
    json_string(s, kind);
    g_string_append(s, ",\"text\":");
    json_string(s, text);
    g_string_append_c(s, '}');
    chrome_call(b, "toast", s->str);
    g_string_free(s, TRUE);
}

/* ───────────────────────── 页面信息（图标等，在隔离的 JS world 中读取） ───────────────────────── */

#define PAGE_INFO_WORLD "loong-browser"

static void
page_info_ready(GObject *object, GAsyncResult *result, gpointer data)
{
    g_autoptr(GError) error = NULL;
    g_autoptr(JSCValue) value = webkit_web_view_evaluate_javascript_finish(WEBKIT_WEB_VIEW(object), result, &error);
    Browser *b = data;
    Tab *tab = tab_for_view(b, WEBKIT_WEB_VIEW(object));
    if (!tab || !value || !jsc_value_is_object(value))
        return;
    g_free(tab->favicon);
    tab->favicon = js_string(value, "icon");
    g_free(tab->manifest);
    tab->manifest = js_string(value, "manifest");
    /* 只接受 http(s) / data 图标，别的（例如 javascript:）一律丢弃。 */
    if (tab->favicon && !g_str_has_prefix(tab->favicon, "https://") && !g_str_has_prefix(tab->favicon, "http://") &&
        !g_str_has_prefix(tab->favicon, "data:image/"))
        g_clear_pointer(&tab->favicon, g_free);
    push_state(b);
}

static void
query_page_info(Tab *tab)
{
    static const char script[] =
        "(() => {"
        " const pick = (sel) => { const l = document.querySelector(sel); return l && l.href ? l.href : null; };"
        " const icon = pick('link[rel~=\"icon\"][sizes=\"32x32\"]') || pick('link[rel~=\"icon\"]') ||"
        "   pick('link[rel=\"apple-touch-icon\"]') || (location.protocol.startsWith('http') ? location.origin + '/favicon.ico' : null);"
        " return { icon, manifest: pick('link[rel~=\"manifest\"]') };"
        "})()";
    webkit_web_view_evaluate_javascript(tab->web_view, script, -1, PAGE_INFO_WORLD, NULL, NULL, page_info_ready, tab->browser);
}

/* ───────────────────────── 添加到主屏幕（PWA 安装） ───────────────────────── */

/* 在页面的隔离 world 中读取并规范化 manifest（相对地址按 manifest 地址解析），没有 manifest 时由页面信息合成。 */
static const char install_script[] =
    "const abs = (u, base) => { try { return new URL(u, base).href; } catch (e) { return null; } };"
    "const link = document.querySelector('link[rel~=\"manifest\"]');"
    "let m = {}, base = location.href;"
    "if (link && link.href) {"
    "  try { const r = await fetch(link.href, { credentials: link.crossOrigin === 'use-credentials' ? 'include' : 'same-origin' });"
    "        if (r.ok) { m = JSON.parse(await r.text()); base = link.href; } } catch (e) { m = {}; }"
    "}"
    "if (typeof m !== 'object' || m === null || Array.isArray(m)) m = {};"
    "const meta = (n) => { const e = document.querySelector('meta[name=\"' + n + '\"]'); return e ? e.content : null; };"
    "const out = {};"
    "out.name = String(m.name || m.short_name || meta('application-name') || document.title || location.hostname).slice(0, 80);"
    "out.short_name = String(m.short_name || out.name).slice(0, 40);"
    "out.start_url = abs(m.start_url || location.href, base) || location.href;"
    "if (new URL(out.start_url).origin !== location.origin) out.start_url = location.href;"
    "let scope = m.scope ? abs(m.scope, base) : null;"
    "if (!scope || !out.start_url.startsWith(scope)) scope = new URL('.', out.start_url).href;"
    "out.scope = scope;"
    "out.id = m.id ? abs(m.id, out.start_url) : out.start_url.split('#')[0];"
    "out.display = ['fullscreen','standalone','minimal-ui','browser'].includes(m.display) ? m.display : 'standalone';"
    "if (typeof m.theme_color === 'string') out.theme_color = m.theme_color; else if (meta('theme-color')) out.theme_color = meta('theme-color');"
    "if (typeof m.background_color === 'string') out.background_color = m.background_color;"
    "if (typeof m.description === 'string') out.description = m.description.slice(0, 300);"
    "if (typeof m.lang === 'string') out.lang = m.lang;"
    "out.icons = [];"
    "if (Array.isArray(m.icons)) for (const i of m.icons.slice(0, 16)) {"
    "  if (!i || typeof i.src !== 'string') continue;"
    "  const src = abs(i.src, base); if (!src || !/^(https?:|data:image\\/)/.test(src)) continue;"
    "  const e = { src }; for (const k of ['sizes', 'type', 'purpose']) if (typeof i[k] === 'string') e[k] = i[k];"
    "  out.icons.push(e);"
    "}"
    "if (!out.icons.length) for (const l of document.querySelectorAll('link[rel~=\"icon\"], link[rel=\"apple-touch-icon\"]')) {"
    "  if (l.href && /^https?:/.test(l.href)) out.icons.push(l.sizes && l.sizes.value ? { src: l.href, sizes: l.sizes.value } : { src: l.href });"
    "}"
    "const u = new URL(out.id);"
    "const slug = (u.host + u.pathname.replace(/\\/index\\.html?$/, '/')).toLowerCase().replace(/^www\\./, '').replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');"
    "return { slug: slug || 'app', manifest: JSON.stringify(out, null, 2), name: out.name };";

/* 应用 id 只允许 [a-z0-9-]，最长 64（与 loong-webapp 的规则一致）。 */
static char *
sanitize_app_id(const char *slug)
{
    GString *s = g_string_new(NULL);
    for (const char *p = slug ? slug : ""; *p && s->len < 64; p++) {
        char c = g_ascii_tolower(*p);
        if (g_ascii_isalnum(c))
            g_string_append_c(s, c);
        else if (s->len && s->str[s->len - 1] != '-')
            g_string_append_c(s, '-');
    }
    while (s->len && s->str[s->len - 1] == '-')
        g_string_truncate(s, s->len - 1);
    if (!s->len)
        g_string_append(s, "app");
    return g_string_free(s, FALSE);
}

static void
install_ready(GObject *object, GAsyncResult *result, gpointer data)
{
    Browser *b = data;
    g_autoptr(GError) error = NULL;
    g_autoptr(JSCValue) value = webkit_web_view_call_async_javascript_function_finish(WEBKIT_WEB_VIEW(object), result, &error);
    if (!value || !jsc_value_is_object(value)) {
        g_warning("读取网页 manifest 失败: %s", error ? error->message : "未知错误");
        toast(b, "error", "无法添加到主屏幕");
        return;
    }
    g_autofree char *slug = js_string(value, "slug");
    g_autofree char *manifest = js_string(value, "manifest");
    g_autofree char *name = js_string(value, "name");
    if (!manifest || strlen(manifest) > 64 * 1024) {
        toast(b, "error", "无法添加到主屏幕");
        return;
    }
    g_autofree char *id = sanitize_app_id(slug);
    g_autofree char *dir = g_build_filename(g_get_user_data_dir(), "loong", "webapps", id, NULL);
    g_autofree char *path = g_build_filename(dir, "manifest.json", NULL);
    if (g_mkdir_with_parents(dir, 0700) < 0 || !g_file_set_contents(path, manifest, -1, &error)) {
        g_warning("写入 %s 失败: %s", path, error ? error->message : g_strerror(errno));
        toast(b, "error", "无法添加到主屏幕");
        return;
    }
    g_message("已添加到主屏幕：%s → %s", name ? name : id, path);
    toast(b, "installed", name ? name : id);
}

static void
install_active(Browser *b)
{
    if (!b->active || tab_is_start(b->active))
        return;
    webkit_web_view_call_async_javascript_function(b->active->web_view, install_script, -1, NULL, PAGE_INFO_WORLD, NULL, NULL,
                                                   install_ready, b);
}

/* ───────────────────────── 模态：对话框与 <select> 菜单（由界面绘制） ───────────────────────── */

static void
modal_dismiss(Browser *b)
{
    if (b->dialog) {
        webkit_script_dialog_close(b->dialog);
        g_clear_pointer(&b->dialog, webkit_script_dialog_unref);
    }
    if (b->menu) {
        WebKitOptionMenu *menu = g_steal_pointer(&b->menu);
        g_signal_handlers_disconnect_by_data(menu, b);
        webkit_option_menu_close(menu);
        g_object_unref(menu);
    }
    chrome_call(b, "closeModal", NULL);
}

static gboolean
on_script_dialog(WebKitWebView *web_view, WebKitScriptDialog *dialog, Browser *b)
{
    Tab *tab = tab_for_view(b, web_view);
    if (!tab || tab != b->active || !b->chrome_ready)
        return FALSE; /* 后台标签页的对话框：按默认处理（直接关闭） */
    modal_dismiss(b);
    b->dialog = webkit_script_dialog_ref(dialog);
    b->modal_id++;
    static const char *const types[] = { "alert", "confirm", "prompt", "beforeunload" };
    WebKitScriptDialogType type = webkit_script_dialog_get_dialog_type(dialog);
    GString *s = g_string_new(NULL);
    g_string_append_printf(s, "{\"id\":%u,\"type\":\"%s\",\"message\":", b->modal_id, types[CLAMP((int)type, 0, 3)]);
    json_string(s, webkit_script_dialog_get_message(dialog));
    g_string_append(s, ",\"defaultText\":");
    json_string(s, type == WEBKIT_SCRIPT_DIALOG_PROMPT ? webkit_script_dialog_prompt_get_default_text(dialog) : NULL);
    g_string_append(s, ",\"origin\":");
    g_autoptr(GUri) uri = g_uri_parse(webkit_web_view_get_uri(web_view) ? webkit_web_view_get_uri(web_view) : "", G_URI_FLAGS_NONE, NULL);
    json_string(s, uri && g_uri_get_host(uri) ? g_uri_get_host(uri) : "此网页");
    g_string_append_c(s, '}');
    chrome_call(b, "dialog", s->str);
    g_string_free(s, TRUE);
    loong_view_focus(b->chrome_view);
    return TRUE;
}

/* WebKit 自己关闭了菜单（例如页面跳转）：界面上的菜单也收起。 */
static void
on_menu_closed(WebKitOptionMenu *menu, Browser *b)
{
    g_signal_handlers_disconnect_by_data(menu, b);
    if (b->menu != menu)
        return;
    g_clear_object(&b->menu);
    chrome_call(b, "closeModal", NULL);
}

static gboolean
on_show_option_menu(WebKitWebView *web_view, WebKitOptionMenu *menu, WebKitRectangle *rect, Browser *b)
{
    Tab *tab = tab_for_view(b, web_view);
    if (!tab || tab != b->active || !b->chrome_ready)
        return FALSE;
    modal_dismiss(b);
    b->menu = g_object_ref(menu);
    b->modal_id++;
    GString *s = g_string_new(NULL);
    int window_width, window_height;
    wpe_toplevel_get_size(b->toplevel, &window_width, &window_height);
    g_string_append_printf(s, "{\"id\":%u,\"x\":%d,\"y\":%d,\"width\":%d,\"height\":%d,\"windowWidth\":%d,\"windowHeight\":%d,\"items\":[",
                           b->modal_id, rect->x, rect->y + b->chrome_height, rect->width, rect->height, window_width, window_height);
    guint n = webkit_option_menu_get_n_items(menu);
    for (guint i = 0; i < n; i++) {
        WebKitOptionMenuItem *item = webkit_option_menu_get_item(menu, i);
        if (i)
            g_string_append_c(s, ',');
        g_string_append(s, "{\"label\":");
        json_string(s, webkit_option_menu_item_get_label(item));
        g_string_append_printf(s, ",\"enabled\":%s,\"selected\":%s,\"group\":%s,\"child\":%s}",
                               webkit_option_menu_item_is_enabled(item) ? "true" : "false",
                               webkit_option_menu_item_is_selected(item) ? "true" : "false",
                               webkit_option_menu_item_is_group_label(item) ? "true" : "false",
                               webkit_option_menu_item_is_group_child(item) ? "true" : "false");
    }
    g_string_append(s, "]}");
    chrome_call(b, "menu", s->str);
    g_string_free(s, TRUE);
    g_signal_connect(menu, "close", G_CALLBACK(on_menu_closed), b);
    return TRUE;
}

/* ───────────────────────── 标签页 ───────────────────────── */

static void
on_tab_notify(WebKitWebView *web_view, GParamSpec *pspec, Browser *b)
{
    push_state(b);
}

static void
on_back_forward_changed(WebKitBackForwardList *list, WebKitBackForwardListItem *added, gpointer removed, Browser *b)
{
    push_state(b);
}

static void
on_load_changed(WebKitWebView *web_view, WebKitLoadEvent event, Browser *b)
{
    Tab *tab = tab_for_view(b, web_view);
    if (!tab)
        return;
    if (event == WEBKIT_LOAD_STARTED) {
        tab->error_page = FALSE;
        g_clear_pointer(&tab->favicon, g_free);
        g_clear_pointer(&tab->manifest, g_free);
    } else if (event == WEBKIT_LOAD_COMMITTED || event == WEBKIT_LOAD_FINISHED) {
        if (!tab_is_start(tab) && !tab->error_page)
            query_page_info(tab);
    }
    push_state(b);
}

static char *
html_escape(const char *text)
{
    return g_markup_escape_text(text ? text : "", -1);
}

static void
show_error_page(Tab *tab, const char *uri, const char *title, const char *message)
{
    Browser *b = tab->browser;
    g_autofree char *path = g_build_filename(b->ui_dir, "error.html", NULL);
    g_autofree char *tmpl = NULL;
    if (!g_file_get_contents(path, &tmpl, NULL, NULL))
        tmpl = g_strdup("<!doctype html><meta charset=utf-8><h1>{{TITLE}}</h1><p>{{MESSAGE}}</p><p>{{URL}}</p>");
    g_autofree char *t = html_escape(title), *m = html_escape(message), *u = html_escape(uri);
    g_auto(GStrv) a = g_strsplit(tmpl, "{{TITLE}}", -1);
    g_autofree char *s1 = g_strjoinv(t, a);
    g_auto(GStrv) c = g_strsplit(s1, "{{MESSAGE}}", -1);
    g_autofree char *s2 = g_strjoinv(m, c);
    g_auto(GStrv) d = g_strsplit(s2, "{{URL}}", -1);
    g_autofree char *html = g_strjoinv(u, d);
    tab->error_page = TRUE;
    webkit_web_view_load_alternate_html(tab->web_view, html, uri, NULL);
}

static gboolean
on_load_failed(WebKitWebView *web_view, WebKitLoadEvent event, const char *uri, GError *error, Browser *b)
{
    Tab *tab = tab_for_view(b, web_view);
    if (!tab)
        return FALSE;
    if (g_error_matches(error, WEBKIT_NETWORK_ERROR, WEBKIT_NETWORK_ERROR_CANCELLED) ||
        g_error_matches(error, WEBKIT_POLICY_ERROR, WEBKIT_POLICY_ERROR_FRAME_LOAD_INTERRUPTED_BY_POLICY_CHANGE) ||
        g_error_matches(error, WEBKIT_MEDIA_ERROR, WEBKIT_MEDIA_ERROR_WILL_HANDLE_LOAD))
        return FALSE;
    g_autofree char *fallback = g_strdup_printf("网页无法载入（%s）。", error->message);
    const char *message = fallback;
    if (g_error_matches(error, WEBKIT_NETWORK_ERROR, WEBKIT_NETWORK_ERROR_FILE_DOES_NOT_EXIST))
        message = "找不到这个文件。";
    else if (g_error_matches(error, WEBKIT_NETWORK_ERROR, WEBKIT_NETWORK_ERROR_UNKNOWN_PROTOCOL))
        message = "不支持这种网址。";
    else if (error->domain == G_RESOLVER_ERROR || g_error_matches(error, G_IO_ERROR, G_IO_ERROR_HOST_NOT_FOUND))
        message = "找不到服务器。请检查网址是否正确，或者网络是否已连接。";
    else if (g_error_matches(error, G_IO_ERROR, G_IO_ERROR_CONNECTION_REFUSED))
        message = "服务器拒绝了连接。";
    else if (g_error_matches(error, G_IO_ERROR, G_IO_ERROR_TIMED_OUT))
        message = "连接超时。";
    show_error_page(tab, uri, "无法打开此网页", message);
    return TRUE;
}

static gboolean
on_load_failed_tls(WebKitWebView *web_view, const char *uri, GTlsCertificate *certificate, GTlsCertificateFlags errors, Browser *b)
{
    Tab *tab = tab_for_view(b, web_view);
    if (!tab)
        return FALSE;
    show_error_page(tab, uri, "连接不安全", "这个网站的证书无效，浏览器已阻止访问，以免信息被窃取或篡改。");
    return TRUE;
}

static void
on_web_process_terminated(WebKitWebView *web_view, WebKitWebProcessTerminationReason reason, Browser *b)
{
    Tab *tab = tab_for_view(b, web_view);
    if (tab && reason != WEBKIT_WEB_PROCESS_TERMINATED_BY_API)
        show_error_page(tab, webkit_web_view_get_uri(web_view), "网页已停止运行",
                        reason == WEBKIT_WEB_PROCESS_EXCEEDED_MEMORY_LIMIT ? "网页占用的内存太多。" : "网页意外退出了。");
}

static gboolean
on_decide_policy(WebKitWebView *web_view, WebKitPolicyDecision *decision, WebKitPolicyDecisionType type, Browser *b)
{
    if (type != WEBKIT_POLICY_DECISION_TYPE_RESPONSE)
        return FALSE;
    WebKitResponsePolicyDecision *response = WEBKIT_RESPONSE_POLICY_DECISION(decision);
    if (webkit_response_policy_decision_is_mime_type_supported(response))
        return FALSE;
    /* v1 不做下载管理：无法显示的内容直接忽略，并提示用户。 */
    if (webkit_response_policy_decision_is_main_frame_main_resource(response))
        toast(b, "error", "暂不支持下载文件");
    webkit_policy_decision_ignore(decision);
    return TRUE;
}

static gboolean
on_permission_request(WebKitWebView *web_view, WebKitPermissionRequest *request, Browser *b)
{
    /* 授权对话框之后由系统界面提供；在那之前一律拒绝。 */
    webkit_permission_request_deny(request);
    return TRUE;
}

static WebKitWebView *
on_create(WebKitWebView *web_view, WebKitNavigationAction *action, Browser *b)
{
    Tab *tab = tab_new(b, web_view, NULL, FALSE);
    return tab->web_view;
}

static void
on_ready_to_show(WebKitWebView *web_view, Browser *b)
{
    Tab *tab = tab_for_view(b, web_view);
    if (tab)
        tab_activate(b, tab);
}

static void tab_close(Browser *b, Tab *tab);

static void
on_close(WebKitWebView *web_view, Browser *b)
{
    if (b->quitting)
        return;
    Tab *tab = tab_for_view(b, web_view);
    if (tab)
        tab_close(b, tab);
}

static Tab *
tab_new(Browser *b, WebKitWebView *related, const char *url, gboolean activate)
{
    Tab *tab = g_new0(Tab, 1);
    tab->browser = b;
    tab->id = ++b->next_tab_id;
    if (related) {
        tab->web_view = WEBKIT_WEB_VIEW(g_object_new(WEBKIT_TYPE_WEB_VIEW, "related-view", related, "settings", b->content_settings,
                                                     "user-content-manager", b->content_ucm, NULL));
    } else {
        tab->web_view = WEBKIT_WEB_VIEW(g_object_new(WEBKIT_TYPE_WEB_VIEW, "display", b->display, "network-session", b->session,
                                                     "settings", b->content_settings, "user-content-manager", b->content_ucm, NULL));
    }
    WPEView *view = webkit_web_view_get_wpe_view(tab->web_view);
    tab->view = LOONG_VIEW(view);
    loong_view_set_layer(tab->view, CONTENT_LAYER);
    wpe_view_set_visible(view, FALSE);
    wpe_view_set_toplevel(view, b->toplevel);

    g_signal_connect(tab->web_view, "notify::title", G_CALLBACK(on_tab_notify), b);
    g_signal_connect(tab->web_view, "notify::uri", G_CALLBACK(on_tab_notify), b);
    g_signal_connect(tab->web_view, "notify::estimated-load-progress", G_CALLBACK(on_tab_notify), b);
    g_signal_connect(tab->web_view, "notify::is-loading", G_CALLBACK(on_tab_notify), b);
    g_signal_connect(tab->web_view, "notify::theme-color", G_CALLBACK(on_tab_notify), b);
    g_signal_connect(webkit_web_view_get_back_forward_list(tab->web_view), "changed", G_CALLBACK(on_back_forward_changed), b);
    g_signal_connect(tab->web_view, "load-changed", G_CALLBACK(on_load_changed), b);
    g_signal_connect(tab->web_view, "load-failed", G_CALLBACK(on_load_failed), b);
    g_signal_connect(tab->web_view, "load-failed-with-tls-errors", G_CALLBACK(on_load_failed_tls), b);
    g_signal_connect(tab->web_view, "web-process-terminated", G_CALLBACK(on_web_process_terminated), b);
    g_signal_connect(tab->web_view, "decide-policy", G_CALLBACK(on_decide_policy), b);
    g_signal_connect(tab->web_view, "permission-request", G_CALLBACK(on_permission_request), b);
    g_signal_connect(tab->web_view, "create", G_CALLBACK(on_create), b);
    g_signal_connect(tab->web_view, "ready-to-show", G_CALLBACK(on_ready_to_show), b);
    g_signal_connect(tab->web_view, "close", G_CALLBACK(on_close), b);
    g_signal_connect(tab->web_view, "script-dialog", G_CALLBACK(on_script_dialog), b);
    g_signal_connect(tab->web_view, "show-option-menu", G_CALLBACK(on_show_option_menu), b);
    g_signal_connect(tab->web_view, "enter-fullscreen", G_CALLBACK(on_enter_fullscreen), b);
    g_signal_connect(tab->web_view, "leave-fullscreen", G_CALLBACK(on_leave_fullscreen), b);

    /* 新标签页排在当前标签页之后。 */
    guint index = b->tabs->len;
    if (b->active) {
        for (guint i = 0; i < b->tabs->len; i++) {
            if (g_ptr_array_index(b->tabs, i) == b->active)
                index = i + 1;
        }
    }
    g_ptr_array_insert(b->tabs, index, tab);
    layout(b);
    if (url)
        webkit_web_view_load_uri(tab->web_view, url);
    if (activate)
        tab_activate(b, tab);
    push_state(b);
    return tab;
}

static void
tab_activate(Browser *b, Tab *tab)
{
    if (b->active == tab)
        return;
    modal_dismiss(b);
    Tab *old = b->active;
    b->active = tab;
    wpe_view_set_visible(WPE_VIEW(tab->view), TRUE);
    if (old)
        wpe_view_set_visible(WPE_VIEW(old->view), FALSE);
    if (!tab_is_start(tab))
        loong_view_focus(tab->view);
    push_state(b);
}

static void
tab_free(Tab *tab)
{
    g_signal_handlers_disconnect_by_data(tab->web_view, tab->browser);
    g_signal_handlers_disconnect_by_data(webkit_web_view_get_back_forward_list(tab->web_view), tab->browser);
    /* 不要 terminate_web_process：window.open 打开的标签页与打开它的页面共用一个 Web 进程。 */
    wpe_view_set_toplevel(WPE_VIEW(tab->view), NULL);
    g_object_unref(tab->web_view);
    g_free(tab->favicon);
    g_free(tab->manifest);
    g_free(tab);
}

static void
tab_close(Browser *b, Tab *tab)
{
    guint index = 0;
    for (guint i = 0; i < b->tabs->len; i++) {
        if (g_ptr_array_index(b->tabs, i) == tab)
            index = i;
    }
    if (b->tabs->len == 1) {
        /* 至少保留一个标签页：最后一个关掉时换成新标签页。 */
        tab_new(b, NULL, b->start_url, TRUE);
        index = 0;
        for (guint i = 0; i < b->tabs->len; i++) {
            if (g_ptr_array_index(b->tabs, i) == tab)
                index = i;
        }
    } else if (b->active == tab) {
        Tab *next = g_ptr_array_index(b->tabs, index + 1 < b->tabs->len ? index + 1 : index - 1);
        tab_activate(b, next);
    }
    g_ptr_array_remove_index(b->tabs, index);
    tab_free(tab);
    layout(b);
    push_state(b);
}

static void
navigate(Browser *b, const char *text)
{
    g_autofree char *url = resolve_input(text, b->search_url);
    if (!url || !b->active)
        return;
    webkit_web_view_load_uri(b->active->web_view, url);
    loong_view_focus(b->active->view);
}

/* ───────────────────────── 界面（chrome） ───────────────────────── */

static void
on_chrome_message(WebKitUserContentManager *ucm, JSCValue *message, Browser *b)
{
    if (!jsc_value_is_object(message))
        return;
    g_autofree char *cmd = js_string(message, "cmd");
    if (!cmd)
        return;
    Tab *tab = b->active;
    if (!strcmp(cmd, "ready")) {
        b->chrome_ready = TRUE;
        push_state(b);
    } else if (!strcmp(cmd, "layout")) {
        int height = (int)js_int(message, "height", DEFAULT_CHROME_HEIGHT);
        gboolean expanded = js_bool(message, "expanded");
        if (height != b->chrome_height || expanded != b->expanded) {
            b->chrome_height = MAX(height, 1);
            b->expanded = expanded;
            layout(b);
        }
    } else if (!strcmp(cmd, "navigate")) {
        g_autofree char *text = js_string(message, "text");
        navigate(b, text);
    } else if (!strcmp(cmd, "back") && tab) {
        webkit_web_view_go_back(tab->web_view);
    } else if (!strcmp(cmd, "forward") && tab) {
        webkit_web_view_go_forward(tab->web_view);
    } else if (!strcmp(cmd, "reload") && tab) {
        webkit_web_view_reload(tab->web_view);
    } else if (!strcmp(cmd, "stop") && tab) {
        webkit_web_view_stop_loading(tab->web_view);
    } else if (!strcmp(cmd, "newTab")) {
        g_autofree char *text = js_string(message, "text");
        g_autofree char *url = text ? resolve_input(text, b->search_url) : NULL;
        tab_new(b, NULL, url ? url : b->start_url, TRUE);
    } else if (!strcmp(cmd, "closeTab")) {
        Tab *t = find_tab(b, (guint)js_int(message, "id", 0));
        if (t)
            tab_close(b, t);
    } else if (!strcmp(cmd, "switchTab")) {
        Tab *t = find_tab(b, (guint)js_int(message, "id", 0));
        if (t)
            tab_activate(b, t);
    } else if (!strcmp(cmd, "focusContent") && tab) {
        loong_view_focus(tab->view);
    } else if (!strcmp(cmd, "install")) {
        install_active(b);
    } else if (!strcmp(cmd, "dialogResult")) {
        if (b->dialog && (guint)js_int(message, "id", 0) == b->modal_id) {
            gboolean ok = js_bool(message, "ok");
            WebKitScriptDialogType type = webkit_script_dialog_get_dialog_type(b->dialog);
            if (type == WEBKIT_SCRIPT_DIALOG_CONFIRM || type == WEBKIT_SCRIPT_DIALOG_BEFORE_UNLOAD_CONFIRM)
                webkit_script_dialog_confirm_set_confirmed(b->dialog, ok);
            else if (type == WEBKIT_SCRIPT_DIALOG_PROMPT && ok) {
                g_autofree char *text = js_string(message, "text");
                webkit_script_dialog_prompt_set_text(b->dialog, text ? text : "");
            }
            webkit_script_dialog_close(b->dialog);
            g_clear_pointer(&b->dialog, webkit_script_dialog_unref);
            if (tab)
                loong_view_focus(tab->view);
        }
    } else if (!strcmp(cmd, "menuResult")) {
        if (b->menu && (guint)js_int(message, "id", 0) == b->modal_id) {
            WebKitOptionMenu *menu = g_steal_pointer(&b->menu);
            g_signal_handlers_disconnect_by_data(menu, b);
            gint64 index = js_int(message, "index", -1);
            if (index >= 0 && index < webkit_option_menu_get_n_items(menu))
                webkit_option_menu_activate_item(menu, (guint)index);
            webkit_option_menu_close(menu);
            g_object_unref(menu);
        }
    }
}

static gboolean
on_chrome_decide_policy(WebKitWebView *web_view, WebKitPolicyDecision *decision, WebKitPolicyDecisionType type, Browser *b)
{
    if (type == WEBKIT_POLICY_DECISION_TYPE_RESPONSE)
        return FALSE;
    WebKitNavigationAction *action =
        webkit_navigation_policy_decision_get_navigation_action(WEBKIT_NAVIGATION_POLICY_DECISION(decision));
    const char *uri = webkit_uri_request_get_uri(webkit_navigation_action_get_request(action));
    /* 界面只能停留在自己的页面上；其他链接在当前标签页打开。 */
    if (uri && g_str_has_prefix(uri, b->chrome_url))
        return FALSE;
    webkit_policy_decision_ignore(decision);
    if (uri && b->active && (g_str_has_prefix(uri, "http://") || g_str_has_prefix(uri, "https://")))
        webkit_web_view_load_uri(b->active->web_view, uri);
    return TRUE;
}

static void
on_chrome_terminated(WebKitWebView *web_view, WebKitWebProcessTerminationReason reason, Browser *b)
{
    g_warning("浏览器界面的 Web 进程退出了（%d），重新载入", reason);
    b->chrome_ready = FALSE;
    webkit_web_view_load_uri(b->chrome, b->chrome_url);
}

static void
on_chrome_close(WebKitWebView *web_view, Browser *b)
{
    b->quitting = TRUE;
    g_application_quit(b->app);
}

static void
on_window_closed(WPEView *view, Browser *b)
{
    b->quitting = TRUE;
    g_application_quit(b->app);
}

static void
create_chrome(Browser *b)
{
    WebKitUserContentManager *ucm = webkit_user_content_manager_new();
    webkit_user_content_manager_register_script_message_handler(ucm, "loong", NULL);
    g_signal_connect(ucm, "script-message-received::loong", G_CALLBACK(on_chrome_message), b);
    WebKitSettings *settings = webkit_settings_new_with_settings("enable-developer-extras", FALSE, "enable-webgl", FALSE,
                                                                 "enable-media", FALSE, "enable-smooth-scrolling", TRUE, NULL);
    webkit_settings_set_enable_write_console_messages_to_stdout(settings, g_getenv("LOONG_BROWSER_DEBUG") != NULL);
    b->chrome = WEBKIT_WEB_VIEW(g_object_new(WEBKIT_TYPE_WEB_VIEW, "display", b->display, "network-session", b->session,
                                             "settings", settings, "user-content-manager", ucm, NULL));
    g_object_unref(settings);
    g_object_unref(ucm);
    WebKitColor transparent = { 0, 0, 0, 0 };
    webkit_web_view_set_background_color(b->chrome, &transparent);
    b->chrome_view = LOONG_VIEW(webkit_web_view_get_wpe_view(b->chrome));
    loong_view_set_layer(b->chrome_view, CHROME_LAYER);
    wpe_view_set_toplevel(WPE_VIEW(b->chrome_view), b->toplevel);
    g_signal_connect(b->chrome, "decide-policy", G_CALLBACK(on_chrome_decide_policy), b);
    g_signal_connect(b->chrome, "web-process-terminated", G_CALLBACK(on_chrome_terminated), b);
    g_signal_connect(b->chrome, "close", G_CALLBACK(on_chrome_close), b);
    /* 合成器要求关闭窗口（xdg_toplevel.close → 所有 view 的 closed）：整个浏览器退出。 */
    g_signal_connect(b->chrome_view, "closed", G_CALLBACK(on_window_closed), b);
    webkit_web_view_load_uri(b->chrome, b->chrome_url);
}

/* ───────────────────────── 单实例 ───────────────────────── */

static void on_socket_line(GObject *stream, GAsyncResult *result, gpointer data);

typedef struct {
    Browser *browser;
    GSocketConnection *connection;
    GDataInputStream *input;
} Client;

static void
read_next_line(Client *client)
{
    g_data_input_stream_read_line_async(client->input, G_PRIORITY_DEFAULT, NULL, (GAsyncReadyCallback)on_socket_line, client);
}

static void
on_socket_line(GObject *stream, GAsyncResult *result, gpointer data)
{
    Client *client = data;
    gsize length;
    g_autofree char *line = g_data_input_stream_read_line_finish(G_DATA_INPUT_STREAM(stream), result, &length, NULL);
    if (!line) {
        g_object_unref(client->input);
        g_object_unref(client->connection);
        g_free(client);
        return;
    }
    if (*line && length < 8192 && g_utf8_validate(line, -1, NULL)) {
        g_autofree char *url = resolve_input(line, client->browser->search_url);
        if (url)
            tab_new(client->browser, NULL, url, TRUE);
    }
    read_next_line(client);
}

static gboolean
on_incoming(GSocketService *service, GSocketConnection *connection, GObject *source, Browser *b)
{
    Client *client = g_new0(Client, 1);
    client->browser = b;
    client->connection = g_object_ref(connection);
    client->input = g_data_input_stream_new(g_io_stream_get_input_stream(G_IO_STREAM(connection)));
    read_next_line(client);
    return TRUE;
}

/* 已有实例在运行：把网址交给它，返回 TRUE。 */
static gboolean
forward_to_running_instance(const char *path, char **urls)
{
    g_autoptr(GSocketClient) client = g_socket_client_new();
    g_autoptr(GSocketAddress) address = g_unix_socket_address_new(path);
    g_autoptr(GSocketConnection) connection = g_socket_client_connect(client, G_SOCKET_CONNECTABLE(address), NULL, NULL);
    if (!connection)
        return FALSE;
    GOutputStream *out = g_io_stream_get_output_stream(G_IO_STREAM(connection));
    for (char **u = urls; u && *u; u++) {
        g_autofree char *line = g_strconcat(*u, "\n", NULL);
        g_output_stream_write_all(out, line, strlen(line), NULL, NULL, NULL);
    }
    if (!urls || !urls[0])
        g_output_stream_write_all(out, "\n", 1, NULL, NULL, NULL);
    g_io_stream_close(G_IO_STREAM(connection), NULL, NULL);
    return TRUE;
}

static void
listen_for_instances(Browser *b)
{
    g_autoptr(GError) error = NULL;
    g_unlink(b->socket_path);
    g_autoptr(GSocketAddress) address = g_unix_socket_address_new(b->socket_path);
    b->socket_service = g_socket_service_new();
    if (!g_socket_listener_add_address(G_SOCKET_LISTENER(b->socket_service), address, G_SOCKET_TYPE_STREAM, G_SOCKET_PROTOCOL_DEFAULT,
                                       NULL, NULL, &error)) {
        g_warning("无法监听 %s: %s", b->socket_path, error->message);
        g_clear_object(&b->socket_service);
        return;
    }
    g_chmod(b->socket_path, 0600);
    g_signal_connect(b->socket_service, "incoming", G_CALLBACK(on_incoming), b);
    g_socket_service_start(b->socket_service);
}

/* ───────────────────────── 启动 ───────────────────────── */

static char *
find_ui_dir(const char *argv0)
{
    const char *env = g_getenv("LOONG_BROWSER_UI");
    if (env && *env)
        return g_canonicalize_filename(env, NULL);
    /* 开发时直接用源码树：从 build/webrt/browser/loong-browser 往上找 webrt/browser/ui。 */
    g_autofree char *exe = g_file_read_link("/proc/self/exe", NULL);
    g_autofree char *dir = exe ? g_path_get_dirname(exe) : NULL;
    for (int depth = 0; dir && depth < 5; depth++) {
        g_autofree char *dev = g_build_filename(dir, "webrt", "browser", "ui", NULL);
        g_autofree char *dev_index = g_build_filename(dev, "index.html", NULL);
        if (g_file_test(dev_index, G_FILE_TEST_EXISTS))
            return g_canonicalize_filename(dev, NULL);
        char *parent = g_path_get_dirname(dir);
        g_free(dir);
        dir = parent;
    }
    /* 安装位置：编译时的前缀；镜像里固定在 /usr/share（mkimage.sh 直接拷贝，不走 meson install）。 */
    static const char *const installed[] = { LOONG_BROWSER_UI_DIR, "/usr/share/loong/web/browser/ui" };
    for (guint i = 0; i < G_N_ELEMENTS(installed); i++) {
        g_autofree char *index = g_build_filename(installed[i], "index.html", NULL);
        if (g_file_test(index, G_FILE_TEST_EXISTS))
            return g_strdup(installed[i]);
    }
    return g_strdup(LOONG_BROWSER_UI_DIR);
}

static void
on_display_disconnected(WPEDisplay *display, GError *error, Browser *b)
{
    g_warning("与合成器的连接已断开：%s", error ? error->message : "");
    g_application_quit(b->app);
}


/* 外观（宣纸/夜墨）→ WPE 的 dark-mode 设置（界面与网页的 prefers-color-scheme）与窗口底色。 */
static void
apply_theme(Browser *b)
{
    WPESettings *wpe_settings = wpe_display_get_settings(b->display);
    wpe_settings_set_boolean(wpe_settings, WPE_SETTING_DARK_MODE, b->dark, WPE_SETTINGS_SOURCE_APPLICATION, NULL);
    if (b->toplevel)
        loong_toplevel_set_background(LOONG_TOPLEVEL(b->toplevel), b->dark ? PAPER_DARK : PAPER_LIGHT);
    update_status_bar(b);
}

static void
on_appearance_changed(LoongAppearance *appearance, G_GNUC_UNUSED GParamSpec *pspec, Browser *b)
{
    b->dark = g_strcmp0(loong_appearance_get_theme(appearance), "dark") == 0;
    apply_theme(b);
}

static void
on_activate(GApplication *app, Browser *b)
{
    if (b->display)
        return;
    g_autoptr(GError) error = NULL;
    b->display = loong_display_new();
    if (!wpe_display_connect(b->display, &error)) {
        g_printerr("loong-browser: %s\n", error->message);
        g_application_quit(app);
        return;
    }
    g_signal_connect(b->display, "disconnected", G_CALLBACK(on_display_disconnected), b);
    WPESettings *wpe_settings = wpe_display_get_settings(b->display);
    /* 所有 view 放进同一个窗口，由本程序决定位置。 */
    wpe_settings_set_boolean(wpe_settings, WPE_SETTING_CREATE_VIEWS_WITH_A_TOPLEVEL, FALSE, WPE_SETTINGS_SOURCE_APPLICATION, NULL);
    b->toplevel = wpe_display_create_toplevel(b->display, 0);
    loong_toplevel_set_app_id(LOONG_TOPLEVEL(b->toplevel), APP_ID);
    if (b->appearance) {
        b->dark = g_strcmp0(loong_appearance_get_theme(b->appearance), "dark") == 0;
        g_signal_connect(b->appearance, "notify::theme", G_CALLBACK(on_appearance_changed), b);
    }
    apply_theme(b);
    wpe_toplevel_set_title(b->toplevel, "浏览器");
    if (b->windowed)
        wpe_toplevel_resize(b->toplevel, b->windowed_width, b->windowed_height);
    else
        wpe_toplevel_fullscreen(b->toplevel); /* 平板上应用总是全屏 */
    g_signal_connect(b->toplevel, "size-changed", G_CALLBACK(on_size_changed), b);

    g_autofree char *data_dir = g_build_filename(g_get_user_data_dir(), "loong", "browser", NULL);
    g_autofree char *cache_dir = g_build_filename(g_get_user_cache_dir(), "loong", "browser", NULL);
    g_mkdir_with_parents(data_dir, 0700);
    g_mkdir_with_parents(cache_dir, 0700);
    b->session = webkit_network_session_new(data_dir, cache_dir);
    webkit_website_data_manager_set_favicons_enabled(webkit_network_session_get_website_data_manager(b->session), FALSE);

    b->content_settings = webkit_settings_new_with_settings("enable-developer-extras", FALSE, "enable-smooth-scrolling", TRUE,
                                                            "enable-webgl", TRUE, "enable-media", TRUE,
                                                            "javascript-can-open-windows-automatically", FALSE, NULL);
    /* LOONG_BROWSER_DEBUG=1：把网页与界面的控制台消息打印到标准输出。 */
    webkit_settings_set_enable_write_console_messages_to_stdout(b->content_settings, g_getenv("LOONG_BROWSER_DEBUG") != NULL);
    webkit_settings_set_user_agent_with_application_details(b->content_settings, "LoongBrowser", "0.1");
    b->content_ucm = webkit_user_content_manager_new();
    /* 网页内容在工具栏下方：LoongKit 页面不再为状态栏留白。 */
    WebKitUserStyleSheet *sheet = webkit_user_style_sheet_new(":root { --loong-safe-area-top: 0px !important; }",
                                                              WEBKIT_USER_CONTENT_INJECT_TOP_FRAME, WEBKIT_USER_STYLE_LEVEL_USER, NULL, NULL);
    webkit_user_content_manager_add_style_sheet(b->content_ucm, sheet);
    webkit_user_style_sheet_unref(sheet);

    b->chrome_height = DEFAULT_CHROME_HEIGHT;
    create_chrome(b);
    if (b->initial_urls && b->initial_urls[0]) {
        for (char **u = b->initial_urls; *u; u++) {
            g_autofree char *url = resolve_input(*u, b->search_url);
            if (url)
                tab_new(b, NULL, url, u == b->initial_urls);
        }
    }
    if (!b->tabs->len)
        tab_new(b, NULL, b->start_url, TRUE);
    layout(b);
    listen_for_instances(b);
    g_application_hold(app);
}

int
main(G_GNUC_UNUSED int argc, char **argv)
{
    Browser browser = { 0 };
    Browser *b = &browser;
    g_autofree char *theme = NULL;
    g_autofree char *windowed = NULL;
    g_autofree char *search = NULL;
    GOptionEntry entries[] = {
        { "theme", 0, 0, G_OPTION_ARG_STRING, &theme, "外观：light（宣纸，默认）或 dark（夜墨）", "THEME" },
        { "search-url", 0, 0, G_OPTION_ARG_STRING, &search, "搜索网址，%s 处替换为关键词", "URL" },
        { "windowed", 0, 0, G_OPTION_ARG_STRING, &windowed, "不全屏，按指定大小开窗（调试用）", "WxH" },
        { G_OPTION_REMAINING, 0, 0, G_OPTION_ARG_STRING_ARRAY, &b->initial_urls, NULL, "URL" },
        { NULL }
    };
    g_autoptr(GOptionContext) ctx = g_option_context_new("[网址…] - Loong OS 浏览器");
    g_option_context_add_main_entries(ctx, entries, NULL);
    g_autoptr(GError) error = NULL;
    setlocale(LC_ALL, "");
    g_auto(GStrv) args = g_strdupv(argv);
    if (!g_option_context_parse_strv(ctx, &args, &error)) {
        fprintf(stderr, "%s\n", error->message);
        return 2;
    }

    /* 配置文件 $XDG_CONFIG_HOME/loong/browser.ini：[browser] search-url=…（命令行优先） */
    g_autofree char *config = g_build_filename(g_get_user_config_dir(), "loong", "browser.ini", NULL);
    g_autoptr(GKeyFile) keyfile = g_key_file_new();
    g_autofree char *config_search = NULL;
    if (g_key_file_load_from_file(keyfile, config, G_KEY_FILE_NONE, NULL))
        config_search = g_key_file_get_string(keyfile, "browser", "search-url", NULL);
    b->search_url = g_strdup(search ? search : config_search ? config_search : DEFAULT_SEARCH_URL);

    if (!theme)
        theme = g_strdup(g_getenv("LOONG_THEME"));
    /* 没有用命令行或环境变量固定外观时，跟随系统外观文件（“设置”写入，见 docs/api/loong-js-bridge.md） */
    if (theme)
        b->dark = !g_ascii_strcasecmp(theme, "dark");
    else
        b->appearance = loong_appearance_new();
    if (windowed && sscanf(windowed, "%dx%d", &b->windowed_width, &b->windowed_height) == 2 && b->windowed_width > 0 &&
        b->windowed_height > 0)
        b->windowed = TRUE;

    b->socket_path = g_build_filename(g_get_user_runtime_dir(), "loong-browser.sock", NULL);
    if (!g_getenv("LOONG_BROWSER_NEW_INSTANCE") && forward_to_running_instance(b->socket_path, b->initial_urls))
        return 0;

    b->ui_dir = find_ui_dir(argv[0]);
    g_autofree char *index = g_build_filename(b->ui_dir, "index.html", NULL);
    g_autofree char *start = g_build_filename(b->ui_dir, "start.html", NULL);
    b->chrome_url = g_filename_to_uri(index, NULL, NULL);
    b->start_url = g_filename_to_uri(start, NULL, NULL);
    b->tabs = g_ptr_array_new();

    g_set_prgname(APP_ID);
    g_set_application_name("浏览器");
    GApplication *app = g_application_new(APP_ID, G_APPLICATION_NON_UNIQUE);
    b->app = app;
    g_signal_connect(app, "activate", G_CALLBACK(on_activate), b);
    int status = g_application_run(app, 0, NULL);

    if (b->socket_service) {
        g_socket_service_stop(b->socket_service);
        g_unlink(b->socket_path);
    }
    g_object_unref(app);
    return status;
}
