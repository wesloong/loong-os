/*
 * loong-appearance：系统外观（浅色「宣纸」/ 深色「夜墨」）。
 *
 * 外观保存在 $XDG_CONFIG_HOME/loong/appearance.toml（默认 ~/.config/loong/appearance.toml）：
 *
 *     theme = "dark"    # 或 "light"；文件不存在或无效时为 "light"
 *
 * 每个 loong-webapp 进程都读取并监视这个文件，把它同步到 WPE 的 dark-mode 设置，
 * 页面里的 prefers-color-scheme 随之切换；“设置”通过 JS bridge 写入它。
 * 约定见 docs/api/loong-js-bridge.md。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#pragma once

#include <gio/gio.h>

G_BEGIN_DECLS

#define LOONG_TYPE_APPEARANCE (loong_appearance_get_type())
G_DECLARE_FINAL_TYPE(LoongAppearance, loong_appearance, LOONG, APPEARANCE, GObject)

/* 读取外观文件并开始监视。属性 "theme" 变化时发 notify::theme。 */
LoongAppearance *loong_appearance_new(void);

/* 当前外观："light" 或 "dark"。 */
const char *loong_appearance_get_theme(LoongAppearance *self);

/* 外观文件路径（新分配）。 */
char *loong_appearance_path(void);

/* 异步写入外观文件（原子替换），在线程里完成，不阻塞 UI。 */
void loong_appearance_set_theme_async(LoongAppearance *self, const char *theme, GCancellable *cancellable,
                                      GAsyncReadyCallback callback, gpointer user_data);
gboolean loong_appearance_set_theme_finish(LoongAppearance *self, GAsyncResult *result, GError **error);

G_END_DECLS
