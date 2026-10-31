/*
 * loong-bridge：系统应用的 JS bridge（window.loong）。
 *
 * 只给以 --bridge=system 启动、且起始页位于受信任目录（LOONG_WEB_ROOT，默认
 * /usr/share/loong/web）下的应用使用。页面通过 WebKit 的带回复脚本消息调用，
 * loong-webapp（UI 进程）按白名单转发到 loong-sysd（D-Bus org.loong.System1），
 * 并把属性变化等信号转成页面事件。接口与安全模型见 docs/api/loong-js-bridge.md。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#pragma once

#include <wpe/webkit.h>

#include "loong-appearance.h"

G_BEGIN_DECLS

typedef struct _LoongBridge LoongBridge;

/*
 * 为 `view` 安装 bridge：注入 JS、注册消息处理器、连接 loong-sysd 所在的总线
 * （默认系统总线；开发时可用环境变量 LOONG_SYSD_BUS=session 或 D-Bus 地址）。
 * `root_dir` 是受信任目录的真实路径（realpath）。必须在加载页面之前调用。
 */
LoongBridge *loong_bridge_new(WebKitWebView *view, const char *root_dir, LoongAppearance *appearance);

void loong_bridge_free(LoongBridge *bridge);

G_END_DECLS
