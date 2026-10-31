/*
 * loong-bridge-core：JS bridge 中与 WebKit、D-Bus 连接无关的部分（白名单、参数校验、
 * GVariant → JSON、URL 校验、配置文件解析），全部是纯函数，便于单元测试
 * （webrt/tests/test-bridge-core.c）。
 *
 * 接口说明见 docs/api/loong-js-bridge.md。
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#pragma once

#include <glib.h>
#include <jsc/jsc.h>

G_BEGIN_DECLS

/* loong-sysd 的总线名与对象路径前缀。 */
#define LOONG_SYSD_BUS_NAME "org.loong.System1"
#define LOONG_SYSD_PATH_PREFIX "/org/loong/System1/"

/* bridge 自己产生的错误名（loong-sysd 与总线的错误名原样转交给页面）。 */
#define LOONG_BRIDGE_ERROR_INVALID_ARGS "org.loong.Bridge.Error.InvalidArgs"
#define LOONG_BRIDGE_ERROR_NOT_ALLOWED "org.loong.Bridge.Error.NotAllowed"
#define LOONG_BRIDGE_ERROR_UNAVAILABLE "org.loong.Bridge.Error.Unavailable"
#define LOONG_BRIDGE_ERROR_FAILED "org.loong.Bridge.Error.Failed"

/* 字符串参数的最大长度（字节）。SSID ≤ 32 字节、WPA 密码 ≤ 64 字节，256 足够宽松。 */
#define LOONG_BRIDGE_MAX_STRING 256

typedef enum {
    LOONG_BRIDGE_ERR_INVALID_ARGS,
    LOONG_BRIDGE_ERR_NOT_ALLOWED,
    LOONG_BRIDGE_ERR_UNAVAILABLE,
    LOONG_BRIDGE_ERR_FAILED,
} LoongBridgeErrorCode;

#define LOONG_BRIDGE_ERROR (loong_bridge_error_quark())
GQuark loong_bridge_error_quark(void);

/* LoongBridgeErrorCode → 错误名。 */
const char *loong_bridge_error_name(int code);

/* 一个可以被页面调用的接口（JS 侧用短名：Backlight / Power / Wifi）。 */
typedef struct {
    const char *name;      /* 短名 */
    const char *interface; /* D-Bus 接口名 */
    const char *path;      /* 对象路径 */
} LoongBridgeInterface;

/* 白名单中的一个方法。 */
typedef struct {
    const char *iface;  /* 接口短名 */
    const char *method; /* D-Bus 方法名 */
    const char *in_sig; /* 参数签名（不含括号），逐个字符对应 JS 数组的元素："d" "b" "s" */
    const char *out_sig; /* 返回值签名（含括号），例如 "(a(suisbb))" */
    int timeout_ms;      /* -1 = GDBus 默认（25 秒） */
} LoongBridgeMethod;

/* 按短名查接口；不在白名单中返回 NULL。 */
const LoongBridgeInterface *loong_bridge_find_interface(const char *name);
/* 按 D-Bus 接口名查接口（转发信号用）；不在白名单中返回 NULL。 */
const LoongBridgeInterface *loong_bridge_find_interface_by_dbus_name(const char *interface);
/* 查方法；接口或方法不在白名单中返回 NULL。 */
const LoongBridgeMethod *loong_bridge_find_method(const char *iface, const char *method);

/*
 * 按方法签名把 JS 参数数组转换成 D-Bus 参数（浮动引用的元组）。
 * `args` 可以是 NULL / undefined（等同于空数组）。逐个校验类型：
 *   d：有限的 number；b：boolean；s：string（UTF-8、不含 NUL、≤ LOONG_BRIDGE_MAX_STRING 字节）。
 * 个数必须与签名一致。失败时返回 NULL 并设置 LOONG_BRIDGE_ERROR_INVALID_ARGS。
 */
GVariant *loong_bridge_build_args(const LoongBridgeMethod *method, JSCValue *args, GError **error);

/* 读取 JS 对象的字符串属性（必须是 string 且 ≤ max_len 字节，否则返回 NULL）。 */
char *loong_bridge_get_string_property(JSCValue *object, const char *name, gsize max_len);

/* JSON：追加一个字符串字面量（含引号与转义）。`s` 应为合法 UTF-8。 */
void loong_bridge_json_string(GString *out, const char *s);
/* JSON：把任意 GVariant 追加为 JSON 值。字典（a{s*} / a{o*}）→ 对象；其他数组、元组 → 数组；
 * 变体 → 内部值；数字 → number（NaN/无穷大 → null）；对象路径、签名 → 字符串。 */
void loong_bridge_json_variant(GString *out, GVariant *value);

/*
 * URL 是否位于受信任的目录 `root_dir`（已经 realpath 过的绝对路径，例如 /usr/share/loong/web）之下：
 * 必须是 file://（主机为空或 localhost），路径解码、解析符号链接与 `..` 后仍在 root_dir 内，
 * 且文件存在。查询串与片段（?mock=1、#wifi）不影响判断。
 */
gboolean loong_bridge_url_trusted(const char *url, const char *root_dir);

/* 解析 os-release（KEY=value，值可带单/双引号与反斜杠转义）。键、值都是新分配的字符串。 */
GHashTable *loong_bridge_parse_os_release(const char *text);

/*
 * 解析 TOML 的一个小子集（device.toml、appearance.toml 用到的部分）：
 * `[表]`、`键 = "字符串" | 整数 | 小数 | true/false`、`#` 注释。
 * 返回 a{sv}：顶层键直接放在根上，每个表是一个嵌套的 a{sv}。不认识的行忽略。
 */
GVariant *loong_bridge_parse_toml(const char *text);

/* 外观：只接受 "light" / "dark"。 */
gboolean loong_appearance_valid(const char *theme);
/* 从 appearance.toml 的内容中读取 theme；缺失或无效时返回 "light"（静态字符串）。 */
const char *loong_appearance_parse(const char *text);
/* 生成 appearance.toml 的内容（新分配）。 */
char *loong_appearance_format(const char *theme);

G_END_DECLS
