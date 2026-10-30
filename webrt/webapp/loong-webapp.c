/*
 * loong-webapp：Loong OS 的 PWA 运行时（每个 Web 应用一个进程）。
 *
 *   loong-webapp --id <应用id> [--name <名称>] [--scope <URL 前缀>] <起始 URL>
 *
 * - 基于 WPE WebKit 2.54 的 WPEPlatform API（Wayland），无需 libwpe/Cog。
 * - 每个应用独立的数据与缓存目录（Cookie、localStorage、IndexedDB、Service Worker 互不相通）：
 *     $XDG_DATA_HOME/loong/webapps/<id>/   $XDG_CACHE_HOME/loong/webapps/<id>/
 * - Wayland app_id 为 loong.webapp.<id>，系统界面据此把窗口对应到主屏图标、绘制启动闪屏。
 * - 超出应用作用域（scope，默认是起始 URL 的源）的页面跳转、window.open 交给系统浏览器打开。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#include <stdio.h>
#include <string.h>
#include <wpe/webkit.h>
#include <wpe/wpe-platform.h>

typedef struct {
    char *id;
    char *name;
    char *scope;
    char *start_url;
    GApplication *app;
} LoongWebApp;

/* 应用 id 只允许 [a-z0-9-]，用于目录名和 Wayland app_id。 */
static gboolean
valid_app_id(const char *id)
{
    if (!id || !*id || strlen(id) > 64)
        return FALSE;
    for (const char *p = id; *p; p++) {
        if (!g_ascii_islower(*p) && !g_ascii_isdigit(*p) && *p != '-')
            return FALSE;
    }
    return TRUE;
}

/* 默认作用域：起始 URL 的源（scheme://host[:port]/）。 */
static char *
default_scope(const char *url)
{
    g_autoptr(GUri) uri = g_uri_parse(url, G_URI_FLAGS_NONE, NULL);
    if (!uri)
        return g_strdup(url);
    g_autoptr(GUri) origin = g_uri_build(G_URI_FLAGS_NONE, g_uri_get_scheme(uri), NULL, g_uri_get_host(uri),
                                         g_uri_get_port(uri), "/", NULL, NULL);
    return g_uri_to_string(origin);
}

static gboolean
in_scope(LoongWebApp *wa, const char *url)
{
    return url && (g_str_has_prefix(url, wa->scope) || g_str_has_prefix(url, "about:") ||
                   g_str_has_prefix(url, "data:") || g_str_has_prefix(url, "blob:"));
}

/* 交给系统浏览器打开。 */
static void
open_in_browser(const char *url)
{
    const char *browser = g_getenv("LOONG_BROWSER");
    g_autoptr(GError) error = NULL;
    const char *argv[] = { browser ? browser : "loong-browser", url, NULL };
    if (!g_spawn_async(NULL, (char **)argv, NULL, G_SPAWN_SEARCH_PATH, NULL, NULL, NULL, &error))
        g_warning("无法用浏览器打开 %s: %s", url, error->message);
}

static gboolean
on_decide_policy(WebKitWebView *view, WebKitPolicyDecision *decision, WebKitPolicyDecisionType type, LoongWebApp *wa)
{
    if (type != WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION)
        return FALSE;
    WebKitNavigationAction *action =
        webkit_navigation_policy_decision_get_navigation_action(WEBKIT_NAVIGATION_POLICY_DECISION(decision));
    const char *url = webkit_uri_request_get_uri(webkit_navigation_action_get_request(action));
    if (in_scope(wa, url))
        return FALSE;
    /* 只拦截用户发起的跳转；重定向等交给 WebKit 默认处理。 */
    if (webkit_navigation_action_is_user_gesture(action)) {
        open_in_browser(url);
        webkit_policy_decision_ignore(decision);
        return TRUE;
    }
    return FALSE;
}

/* window.open / target=_blank：PWA 中不开新窗口，交给浏览器。 */
static WebKitWebView *
on_create(WebKitWebView *view, WebKitNavigationAction *action, LoongWebApp *wa)
{
    const char *url = webkit_uri_request_get_uri(webkit_navigation_action_get_request(action));
    if (in_scope(wa, url))
        webkit_web_view_load_uri(view, url);
    else
        open_in_browser(url);
    return NULL;
}

static gboolean
on_permission_request(WebKitWebView *view, WebKitPermissionRequest *request, LoongWebApp *wa)
{
    /* v1：通知权限先允许（转发到系统通知中心是 M2 的工作），其余一律拒绝，
     * 之后改为由系统界面弹出 Loong 风格的授权对话框。 */
    if (WEBKIT_IS_NOTIFICATION_PERMISSION_REQUEST(request))
        webkit_permission_request_allow(request);
    else
        webkit_permission_request_deny(request);
    return TRUE;
}

static void
on_title_changed(WebKitWebView *view, GParamSpec *pspec, LoongWebApp *wa)
{
    WPEView *wpe_view = webkit_web_view_get_wpe_view(view);
    const char *title = webkit_web_view_get_title(view);
    if (wpe_view && wpe_view_get_toplevel(wpe_view))
        wpe_toplevel_set_title(wpe_view_get_toplevel(wpe_view), title && *title ? title : wa->name);
}

static void
on_close(WebKitWebView *view, LoongWebApp *wa)
{
    g_application_release(wa->app);
}

static void
on_activate(GApplication *app, LoongWebApp *wa)
{
    g_autofree char *data_dir = g_build_filename(g_get_user_data_dir(), "loong", "webapps", wa->id, NULL);
    g_autofree char *cache_dir = g_build_filename(g_get_user_cache_dir(), "loong", "webapps", wa->id, NULL);
    g_mkdir_with_parents(data_dir, 0700);
    g_mkdir_with_parents(cache_dir, 0700);
    WebKitNetworkSession *session = webkit_network_session_new(data_dir, cache_dir);

    WebKitSettings *settings = webkit_settings_new_with_settings(
        "enable-developer-extras", FALSE,
        "enable-smooth-scrolling", TRUE,
        "enable-webgl", TRUE,
        "enable-media", TRUE,
        "hardware-acceleration-policy", WEBKIT_HARDWARE_ACCELERATION_POLICY_ALWAYS,
        NULL);

    WebKitWebView *view = WEBKIT_WEB_VIEW(g_object_new(WEBKIT_TYPE_WEB_VIEW,
        "network-session", session,
        "settings", settings,
        NULL));
    g_object_unref(settings);
    g_object_unref(session);

    g_signal_connect(view, "decide-policy", G_CALLBACK(on_decide_policy), wa);
    g_signal_connect(view, "create", G_CALLBACK(on_create), wa);
    g_signal_connect(view, "permission-request", G_CALLBACK(on_permission_request), wa);
    g_signal_connect(view, "notify::title", G_CALLBACK(on_title_changed), wa);
    g_signal_connect(view, "close", G_CALLBACK(on_close), wa);

    WPEView *wpe_view = webkit_web_view_get_wpe_view(view);
    if (wpe_view && wpe_view_get_toplevel(wpe_view)) {
        WPEToplevel *toplevel = wpe_view_get_toplevel(wpe_view);
        wpe_toplevel_set_title(toplevel, wa->name);
        /* 平板上应用总是全屏（系统界面也会强制）。 */
        wpe_toplevel_fullscreen(toplevel);
    }

    g_application_hold(app);
    webkit_web_view_load_uri(view, wa->start_url);
}

int
main(int argc, char **argv)
{
    LoongWebApp wa = { 0 };
    g_autofree char *scope = NULL;
    g_auto(GStrv) rest = NULL;
    GOptionEntry entries[] = {
        { "id", 0, 0, G_OPTION_ARG_STRING, &wa.id, "应用 id（[a-z0-9-]）", "ID" },
        { "name", 0, 0, G_OPTION_ARG_STRING, &wa.name, "应用名称", "NAME" },
        { "scope", 0, 0, G_OPTION_ARG_STRING, &scope, "作用域 URL 前缀（默认：起始 URL 的源）", "URL" },
        { G_OPTION_REMAINING, 0, 0, G_OPTION_ARG_STRING_ARRAY, &rest, NULL, "URL" },
        { NULL }
    };
    g_autoptr(GOptionContext) ctx = g_option_context_new("<起始 URL> - Loong OS Web 应用运行时");
    g_option_context_add_main_entries(ctx, entries, NULL);
    g_autoptr(GError) error = NULL;
    if (!g_option_context_parse(ctx, &argc, &argv, &error)) {
        fprintf(stderr, "%s\n", error->message);
        return 2;
    }
    if (!valid_app_id(wa.id) || !rest || !rest[0]) {
        fprintf(stderr, "用法: loong-webapp --id <应用id> [--name 名称] [--scope URL] <起始 URL>\n");
        return 2;
    }
    wa.start_url = rest[0];
    wa.scope = scope ? g_strdup(scope) : default_scope(wa.start_url);
    if (!wa.name)
        wa.name = g_strdup(wa.id);

    /* app_id 形如 loong.webapp.<id>（GApplication id 不允许 '-'，替换为 '_'）。 */
    g_autofree char *safe_id = g_strdelimit(g_strdup(wa.id), "-", '_');
    g_autofree char *app_id = g_strconcat("loong.webapp.", safe_id, NULL);
    g_set_prgname(app_id);
    g_set_application_name(wa.name);

    /* NON_UNIQUE：不依赖 D-Bus 会话总线。 */
    GApplication *app = g_application_new(app_id, G_APPLICATION_NON_UNIQUE);
    wa.app = app;
    g_signal_connect(app, "activate", G_CALLBACK(on_activate), &wa);
    int status = g_application_run(app, 0, NULL);
    g_object_unref(app);
    g_free(wa.scope);
    return status;
}
