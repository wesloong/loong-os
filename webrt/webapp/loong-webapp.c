/*
 * loong-webapp：Loong OS 的 PWA 运行时（每个 Web 应用一个进程）。
 *
 *   loong-webapp --id <应用id> [--name <名称>] [--scope <URL 前缀>] [--bridge=system] <起始 URL>
 *
 * - 基于 WPE WebKit 2.54 的 WPEPlatform API（Wayland），无需 libwpe/Cog。
 * - 每个应用独立的数据与缓存目录（Cookie、localStorage、IndexedDB、Service Worker 互不相通）：
 *     $XDG_DATA_HOME/loong/webapps/<id>/   $XDG_CACHE_HOME/loong/webapps/<id>/
 * - Wayland app_id 为 loong.webapp.<id>，系统界面据此把窗口对应到主屏图标、绘制启动闪屏。
 * - 超出应用作用域（scope，默认是起始 URL 的源）的页面跳转、window.open 交给系统浏览器打开。
 * - 系统外观（$XDG_CONFIG_HOME/loong/appearance.toml）同步到 WPE 的 dark-mode 设置，
 *   页面的 prefers-color-scheme 随系统切换（loong-appearance.c）。
 * - 页面的主题色（<meta name="theme-color">，随 prefers-color-scheme 变化）经 Loong 私有协议
 *   loong_app_v1 告诉 loong-shell，状态栏据此选浓墨或月白（loong-app-surface.c，
 *   docs/api/loong-app-protocol.md）。页面没有主题色时不发颜色：loong-shell 先读窗口顶部的
 *   像素（共享内存缓冲），读不到再按系统外观——也就是退回纸色。
 * - --bridge=system：给系统应用（设置等）提供 window.loong（loong-bridge.c）。只有起始页位于
 *   受信任目录（LOONG_WEB_ROOT，默认 /usr/share/loong/web）下时才允许，否则拒绝启动；
 *   此时作用域收紧到受信任目录，任何框架都不能跳转到目录以外。见 docs/api/loong-js-bridge.md。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#include <stdio.h>
#include <stdlib.h>
#include <locale.h>
#include <string.h>
#include <wpe/webkit.h>
#include <wpe/wpe-platform.h>
#include <wpe/wayland/wpe-wayland.h>

#include "loong-app-surface.h"
#include "loong-appearance.h"
#include "loong-bridge-core.h"
#include "loong-bridge.h"

/* 受信任目录：只读根分区上的系统 Web 应用（meson 选项 web_root）。 */
#ifndef LOONG_WEB_ROOT
#define LOONG_WEB_ROOT "/usr/share/loong/web"
#endif

typedef struct {
    char *id;
    char *name;
    char *scope;
    char *start_url;
    char *bridge_root; /* 非 NULL：启用了 bridge，值为受信任目录的真实路径 */
    GApplication *app;
    WebKitWebView *view;
    LoongAppearance *appearance;
    LoongBridge *bridge;
    LoongAppSurface *app_surface; /* loong_app_v1（合成器不是 loong-shell 时为 NULL） */
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
    if (wa->bridge_root) {
        /* 系统应用：任何框架（含 iframe、重定向）只能停留在受信任目录内，
         * 这样能拿到 window.webkit.messageHandlers.loong 的永远只有系统自带的页面。 */
        if (loong_bridge_url_trusted(url, wa->bridge_root) || g_strcmp0(url, "about:blank") == 0 ||
            g_strcmp0(url, "about:srcdoc") == 0)
            return FALSE;
        if (webkit_navigation_action_is_user_gesture(action) &&
            (g_str_has_prefix(url, "https://") || g_str_has_prefix(url, "http://")))
            open_in_browser(url);
        else
            g_warning("已阻止系统应用跳转到 %s", url);
        webkit_policy_decision_ignore(decision);
        return TRUE;
    }
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
    if (wa->bridge_root ? loong_bridge_url_trusted(url, wa->bridge_root) : in_scope(wa, url))
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

/* 系统外观 → WPE 的 dark-mode 设置（页面的 prefers-color-scheme）与加载前的底色。 */
static void
apply_appearance(LoongWebApp *wa)
{
    gboolean dark = g_strcmp0(loong_appearance_get_theme(wa->appearance), "dark") == 0;
    WPEDisplay *display = webkit_web_view_get_display(wa->view);
    WPESettings *settings = display ? wpe_display_get_settings(display) : NULL;
    g_autoptr(GError) error = NULL;
    if (settings &&
        !wpe_settings_set_boolean(settings, WPE_SETTING_DARK_MODE, dark, WPE_SETTINGS_SOURCE_APPLICATION, &error))
        g_warning("无法设置深色模式：%s", error->message);
    /* 宣纸 / 夜墨（与 LoongKit 的 --loong-color-paper 一致），避免页面绘制前闪白。 */
    WebKitColor paper = dark ? (WebKitColor){ 0x15 / 255.0, 0x14 / 255.0, 0x0f / 255.0, 1 }
                             : (WebKitColor){ 0xf3 / 255.0, 0xee / 255.0, 0xe2 / 255.0, 1 };
    webkit_web_view_set_background_color(wa->view, &paper);
}

static void
on_theme_changed(LoongAppearance *appearance, GParamSpec *pspec, LoongWebApp *wa)
{
    apply_appearance(wa);
}

/* 页面主题色 → loong-shell（状态栏的墨）。没有主题色时发 0（未设置）：loong-shell 读得到窗口
 * 顶部的像素就按像素，读不到（GPU 缓冲）就按系统外观，与页面加载前的纸色底一致。 */
static void
update_theme_color(LoongWebApp *wa)
{
    WebKitColor color;
    guint32 argb = 0;
    if (webkit_web_view_get_theme_color(wa->view, &color) && color.alpha > 0) {
        guint32 a = (guint32)(CLAMP(color.alpha, 0, 1) * 255 + 0.5);
        guint32 r = (guint32)(CLAMP(color.red, 0, 1) * 255 + 0.5);
        guint32 g = (guint32)(CLAMP(color.green, 0, 1) * 255 + 0.5);
        guint32 b = (guint32)(CLAMP(color.blue, 0, 1) * 255 + 0.5);
        argb = a << 24 | r << 16 | g << 8 | b;
    }
    loong_app_surface_set_theme_color(wa->app_surface, argb);
}

static void
on_theme_color_changed(WebKitWebView *view, GParamSpec *pspec, LoongWebApp *wa)
{
    update_theme_color(wa);
}

/* WPE 自带的 Wayland 平台：用公开接口取 wl_display 与窗口的 wl_surface，建立 loong_app_v1。
 * 其他平台（headless、DRM）没有 Wayland 窗口，什么也不做。 */
static void
setup_app_surface(LoongWebApp *wa, WPEToplevel *toplevel)
{
    WPEDisplay *display = webkit_web_view_get_display(wa->view);
    if (!WPE_IS_DISPLAY_WAYLAND(display) || !WPE_IS_TOPLEVEL_WAYLAND(toplevel))
        return;
    struct wl_display *wl_display = wpe_display_wayland_get_wl_display(WPE_DISPLAY_WAYLAND(display));
    struct wl_surface *surface = wpe_toplevel_wayland_get_wl_surface(WPE_TOPLEVEL_WAYLAND(toplevel));
    if (!wl_display || !surface)
        return;
    wa->app_surface = loong_app_surface_new(wl_display, surface);
    if (!wa->app_surface)
        return;
    g_signal_connect(wa->view, "notify::theme-color", G_CALLBACK(on_theme_color_changed), wa);
    update_theme_color(wa);
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
        /* 调试：LOONG_WEBAPP_CONSOLE=1 时把页面的 console 输出写到标准输出。 */
        "enable-write-console-messages-to-stdout", g_strcmp0(g_getenv("LOONG_WEBAPP_CONSOLE"), "1") == 0,
        NULL);

    WebKitUserContentManager *ucm = webkit_user_content_manager_new();
    WebKitWebView *view = WEBKIT_WEB_VIEW(g_object_new(WEBKIT_TYPE_WEB_VIEW,
        "network-session", session,
        "settings", settings,
        "user-content-manager", ucm,
        NULL));
    g_object_unref(ucm);
    g_object_unref(settings);
    g_object_unref(session);
    wa->view = view;

    wa->appearance = loong_appearance_new();
    g_signal_connect(wa->appearance, "notify::theme", G_CALLBACK(on_theme_changed), wa);
    apply_appearance(wa);
    if (wa->bridge_root)
        wa->bridge = loong_bridge_new(view, wa->bridge_root, wa->appearance);

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
        setup_app_surface(wa, toplevel);
    }

    g_application_hold(app);
    webkit_web_view_load_uri(view, wa->start_url);
}

int
main(G_GNUC_UNUSED int argc, char **argv)
{
    LoongWebApp wa = { 0 };
    g_autofree char *scope = NULL;
    g_autofree char *bridge = NULL;
    g_auto(GStrv) rest = NULL;
    GOptionEntry entries[] = {
        { "id", 0, 0, G_OPTION_ARG_STRING, &wa.id, "应用 id（[a-z0-9-]）", "ID" },
        { "name", 0, 0, G_OPTION_ARG_STRING, &wa.name, "应用名称", "NAME" },
        { "scope", 0, 0, G_OPTION_ARG_STRING, &scope, "作用域 URL 前缀（默认：起始 URL 的源）", "URL" },
        { "bridge", 0, 0, G_OPTION_ARG_STRING, &bridge,
          "提供系统接口 window.loong（只能是 system；起始页须位于 " LOONG_WEB_ROOT " 下）", "system" },
        { G_OPTION_REMAINING, 0, 0, G_OPTION_ARG_STRING_ARRAY, &rest, NULL, "URL" },
        { NULL }
    };
    g_autoptr(GOptionContext) ctx = g_option_context_new("<起始 URL> - Loong OS Web 应用运行时");
    g_option_context_add_main_entries(ctx, entries, NULL);
    g_autoptr(GError) error = NULL;
    /* 参数一律按 UTF-8 解析（应用名常是中文）。g_option_context_parse 会按当前
     * locale 转码，而系统里不一定设置了 UTF-8 locale（musl 默认 C/ASCII）。 */
    setlocale(LC_ALL, "");
    g_auto(GStrv) args = g_strdupv(argv);
    if (!g_option_context_parse_strv(ctx, &args, &error)) {
        fprintf(stderr, "%s\n", error->message);
        return 2;
    }
    if (!valid_app_id(wa.id) || !rest || !rest[0]) {
        fprintf(stderr, "用法: loong-webapp --id <应用id> [--name 名称] [--scope URL] [--bridge=system] <起始 URL>\n");
        return 2;
    }
    wa.start_url = rest[0];
    if (bridge) {
        /* bridge 只给只读根分区上的系统应用：参数不对、起始页不在受信任目录内时拒绝启动，
         * 而不是悄悄降级成普通应用（配置错误应当立即暴露）。 */
        char *root = realpath(LOONG_WEB_ROOT, NULL);
        if (strcmp(bridge, "system") != 0 || !root || !loong_bridge_url_trusted(wa.start_url, root)) {
            fprintf(stderr, "loong-webapp: 拒绝启用 --bridge=%s：起始页 %s 不在 %s 下（或 bridge 名称不对）\n", bridge,
                    wa.start_url, LOONG_WEB_ROOT);
            free(root);
            return 2;
        }
        wa.bridge_root = g_strdup(root);
        free(root);
        /* 作用域收紧到受信任目录。 */
        g_free(scope);
        scope = g_strconcat("file://", wa.bridge_root, "/", NULL);
    }
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
    loong_app_surface_free(wa.app_surface);
    loong_bridge_free(wa.bridge);
    g_clear_object(&wa.appearance);
    g_object_unref(app);
    g_free(wa.scope);
    g_free(wa.bridge_root);
    return status;
}
