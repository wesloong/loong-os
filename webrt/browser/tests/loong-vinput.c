/*
 * loong-vinput：测试用的常驻虚拟输入设备（wlroots 合成器：zwlr_virtual_pointer_v1 +
 * zwp_virtual_keyboard_v1）。wtype / wlrctl 每次调用都新建、销毁设备，客户端常常来不及
 * 绑定 wl_pointer / wl_keyboard 就错过了事件；这个工具在整个测试期间保持设备存在。
 *
 *   loong-vinput 宽 高 < 命令
 *
 * 从标准输入逐行读命令：
 *   move X Y            指针移到输出坐标 (X, Y)
 *   down / up           左键按下 / 松开
 *   click X Y           移动并点按
 *   drag X1 Y1 X2 Y2    按住左键从 (X1,Y1) 拖到 (X2,Y2)
 *   wheel N             滚轮 N 格（正数向下）
 *   type 文本           逐字输入（ASCII，按美式键盘布局）
 *   key 键名            按一个键（xkb 键名，例如 Return、BackSpace、Escape）
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#define _GNU_SOURCE
#include <linux/input-event-codes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>

#include "virtual-keyboard-unstable-v1-client-protocol.h"
#include "wlr-virtual-pointer-unstable-v1-client-protocol.h"

static struct wl_seat *seat;
static struct zwlr_virtual_pointer_manager_v1 *pointer_manager;
static struct zwp_virtual_keyboard_manager_v1 *keyboard_manager;
static struct zwlr_virtual_pointer_v1 *pointer;
static struct zwp_virtual_keyboard_v1 *keyboard;
static struct xkb_keymap *keymap;
static unsigned width, height;
static unsigned shift_mask;

static void
global(void *data, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version)
{
    if (!strcmp(interface, wl_seat_interface.name) && !seat)
        seat = wl_registry_bind(registry, name, &wl_seat_interface, 1);
    else if (!strcmp(interface, zwlr_virtual_pointer_manager_v1_interface.name))
        pointer_manager = wl_registry_bind(registry, name, &zwlr_virtual_pointer_manager_v1_interface, 1);
    else if (!strcmp(interface, zwp_virtual_keyboard_manager_v1_interface.name))
        keyboard_manager = wl_registry_bind(registry, name, &zwp_virtual_keyboard_manager_v1_interface, 1);
}

static void
global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
}

static const struct wl_registry_listener registry_listener = { global, global_remove };

static uint32_t
now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void
upload_keymap(void)
{
    struct xkb_context *context = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
    struct xkb_rule_names names = { .layout = "us" };
    keymap = xkb_keymap_new_from_names(context, &names, XKB_KEYMAP_COMPILE_NO_FLAGS);
    char *text = xkb_keymap_get_as_string(keymap, XKB_KEYMAP_FORMAT_TEXT_V1);
    size_t size = strlen(text) + 1;
    int fd = memfd_create("loong-vinput-keymap", MFD_CLOEXEC);
    if (fd < 0 || ftruncate(fd, size) < 0)
        exit(1);
    void *map = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    memcpy(map, text, size);
    munmap(map, size);
    zwp_virtual_keyboard_v1_keymap(keyboard, WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1, fd, size);
    close(fd);
    free(text);
    shift_mask = 1u << xkb_keymap_mod_get_index(keymap, XKB_MOD_NAME_SHIFT);
    xkb_context_unref(context);
}

/* 找到产生 keysym 的键码（evdev，= xkb 键码 - 8）以及是否需要 Shift。 */
static int
find_key(xkb_keysym_t sym, int *shift)
{
    for (xkb_keycode_t code = xkb_keymap_min_keycode(keymap); code <= xkb_keymap_max_keycode(keymap); code++) {
        for (int level = 0; level < 2; level++) {
            const xkb_keysym_t *syms;
            int n = xkb_keymap_key_get_syms_by_level(keymap, code, 0, level, &syms);
            if (n == 1 && syms[0] == sym) {
                *shift = level;
                return code - 8;
            }
        }
    }
    return -1;
}

static void
press_key(xkb_keysym_t sym, struct wl_display *display)
{
    int shift = 0;
    int code = find_key(sym, &shift);
    if (code < 0) {
        fprintf(stderr, "loong-vinput: 键盘布局里没有 keysym 0x%x\n", sym);
        return;
    }
    if (shift)
        zwp_virtual_keyboard_v1_modifiers(keyboard, shift_mask, 0, 0, 0);
    zwp_virtual_keyboard_v1_key(keyboard, now_ms(), code, WL_KEYBOARD_KEY_STATE_PRESSED);
    wl_display_flush(display);
    usleep(15000);
    zwp_virtual_keyboard_v1_key(keyboard, now_ms(), code, WL_KEYBOARD_KEY_STATE_RELEASED);
    if (shift)
        zwp_virtual_keyboard_v1_modifiers(keyboard, 0, 0, 0, 0);
    wl_display_flush(display);
    usleep(25000);
}

static void
move_to(double x, double y)
{
    zwlr_virtual_pointer_v1_motion_absolute(pointer, now_ms(), (uint32_t)x, (uint32_t)y, width, height);
    zwlr_virtual_pointer_v1_frame(pointer);
}

static void
button(int pressed)
{
    zwlr_virtual_pointer_v1_button(pointer, now_ms(), BTN_LEFT, pressed ? WL_POINTER_BUTTON_STATE_PRESSED : WL_POINTER_BUTTON_STATE_RELEASED);
    zwlr_virtual_pointer_v1_frame(pointer);
}

int
main(int argc, char **argv)
{
    if (argc < 3) {
        fprintf(stderr, "用法: loong-vinput 宽 高 < 命令\n");
        return 2;
    }
    width = atoi(argv[1]);
    height = atoi(argv[2]);
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) {
        fprintf(stderr, "loong-vinput: 无法连接合成器\n");
        return 1;
    }
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, NULL);
    wl_display_roundtrip(display);
    if (!seat || !pointer_manager || !keyboard_manager) {
        fprintf(stderr, "loong-vinput: 合成器不支持虚拟指针 / 虚拟键盘\n");
        return 1;
    }
    pointer = zwlr_virtual_pointer_manager_v1_create_virtual_pointer(pointer_manager, seat);
    keyboard = zwp_virtual_keyboard_manager_v1_create_virtual_keyboard(keyboard_manager, seat);
    upload_keymap();
    wl_display_roundtrip(display);

    char line[4096];
    while (fgets(line, sizeof line, stdin)) {
        line[strcspn(line, "\n")] = '\0';
        double x, y, x2, y2;
        int n;
        if (sscanf(line, "click %lf %lf", &x, &y) == 2) {
            move_to(x, y);
            wl_display_flush(display);
            usleep(30000);
            button(1);
            wl_display_flush(display);
            usleep(60000);
            button(0);
        } else if (sscanf(line, "drag %lf %lf %lf %lf", &x, &y, &x2, &y2) == 4) {
            move_to(x, y);
            button(1);
            for (int i = 1; i <= 20; i++) {
                wl_display_flush(display);
                usleep(16000);
                move_to(x + (x2 - x) * i / 20, y + (y2 - y) * i / 20);
            }
            wl_display_flush(display);
            usleep(16000);
            button(0);
        } else if (sscanf(line, "move %lf %lf", &x, &y) == 2) {
            move_to(x, y);
        } else if (!strcmp(line, "down")) {
            button(1);
        } else if (!strcmp(line, "up")) {
            button(0);
        } else if (sscanf(line, "wheel %d", &n) == 1) {
            for (int i = 0; i < abs(n); i++) {
                zwlr_virtual_pointer_v1_axis_source(pointer, WL_POINTER_AXIS_SOURCE_WHEEL);
                zwlr_virtual_pointer_v1_axis_discrete(pointer, now_ms(), WL_POINTER_AXIS_VERTICAL_SCROLL,
                                                      wl_fixed_from_int(n > 0 ? 15 : -15), n > 0 ? 1 : -1);
                zwlr_virtual_pointer_v1_frame(pointer);
                wl_display_flush(display);
                usleep(30000);
            }
        } else if (!strncmp(line, "type ", 5)) {
            for (const char *p = line + 5; *p; p++)
                press_key(xkb_utf32_to_keysym((unsigned char)*p), display);
        } else if (!strncmp(line, "key ", 4)) {
            xkb_keysym_t sym = xkb_keysym_from_name(line + 4, XKB_KEYSYM_CASE_INSENSITIVE);
            if (sym != XKB_KEY_NoSymbol)
                press_key(sym, display);
        } else if (*line) {
            fprintf(stderr, "loong-vinput: 未知命令 %s\n", line);
        }
        wl_display_flush(display);
        wl_display_roundtrip(display);
    }
    zwlr_virtual_pointer_v1_destroy(pointer);
    zwp_virtual_keyboard_v1_destroy(keyboard);
    wl_display_roundtrip(display);
    wl_display_disconnect(display);
    return 0;
}
