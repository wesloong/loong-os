/*
 * Loong OS 的 libGL.so.1 桩：导出 GLX 1.4 的全部入口，但一律报告“没有 GLX”。
 *
 * 为什么需要它：Alpine 的 gst-plugins-base 带 GLX 平台编译，libgstgl-1.0.so.0 直接链接
 * libGL.so.1（导入 glXChooseFBConfig 等 16 个 GLX 函数），而 libWPEWebKit 又链接 libgstgl。
 * musl 在加载时解析全部符号，所以只要系统里装着 WebKit，就必须有一个导出这些符号的 libGL.so.1。
 * Alpine 的 mesa-gl 能满足，但它要依赖完整的 mesa 包（LLVM、X11/xcb）。
 *
 * Loong OS 只有 Wayland：GStreamer GL 在 Wayland 上走 EGL，从不调用 GLX；libepoxy 也只在
 * libGL 已加载时用 glXGetCurrentContext() 判断“当前是不是 GLX 上下文”，这里返回 NULL 即可。
 * 因此这些函数只需返回失败值（NULL / False / 0 / GLX_NO_EXTENSION），不依赖任何 X11 头文件。
 * 不导出 gl* 函数：桌面 OpenGL 不在 Loong OS 的支持范围内（mesa-loong 以 -Dopengl=false 构建）。
 */

#include <stddef.h>

#define STUB __attribute__((visibility("default")))

typedef int Bool;
typedef unsigned long XID;
typedef void *GLXContext;
typedef void *GLXFBConfig;
typedef void (*proc_t)(void);

enum { GLX_NO_EXTENSION = 3 };

/* GLX 1.0 – 1.2 */
STUB void *glXChooseVisual(void *dpy, int screen, int *attrib) { return NULL; }
STUB GLXContext glXCreateContext(void *dpy, void *vis, GLXContext share, Bool direct) { return NULL; }
STUB void glXDestroyContext(void *dpy, GLXContext ctx) {}
STUB Bool glXMakeCurrent(void *dpy, XID drawable, GLXContext ctx) { return 0; }
STUB void glXCopyContext(void *dpy, GLXContext src, GLXContext dst, unsigned long mask) {}
STUB void glXSwapBuffers(void *dpy, XID drawable) {}
STUB XID glXCreateGLXPixmap(void *dpy, void *vis, XID pixmap) { return 0; }
STUB void glXDestroyGLXPixmap(void *dpy, XID pixmap) {}
STUB Bool glXQueryExtension(void *dpy, int *error_base, int *event_base) { return 0; }
STUB Bool glXQueryVersion(void *dpy, int *major, int *minor) { return 0; }
STUB Bool glXIsDirect(void *dpy, GLXContext ctx) { return 0; }
STUB int glXGetConfig(void *dpy, void *vis, int attrib, int *value) { return GLX_NO_EXTENSION; }
STUB GLXContext glXGetCurrentContext(void) { return NULL; }
STUB XID glXGetCurrentDrawable(void) { return 0; }
STUB void glXWaitGL(void) {}
STUB void glXWaitX(void) {}
STUB void glXUseXFont(XID font, int first, int count, int list_base) {}
STUB const char *glXQueryExtensionsString(void *dpy, int screen) { return ""; }
STUB const char *glXQueryServerString(void *dpy, int screen, int name) { return ""; }
STUB const char *glXGetClientString(void *dpy, int name) { return ""; }
STUB void *glXGetCurrentDisplay(void) { return NULL; }

/* GLX 1.3 */
STUB GLXFBConfig *glXChooseFBConfig(void *dpy, int screen, const int *attrib, int *n)
{
	if (n)
		*n = 0;
	return NULL;
}
STUB int glXGetFBConfigAttrib(void *dpy, GLXFBConfig config, int attrib, int *value) { return GLX_NO_EXTENSION; }
STUB GLXFBConfig *glXGetFBConfigs(void *dpy, int screen, int *n)
{
	if (n)
		*n = 0;
	return NULL;
}
STUB void *glXGetVisualFromFBConfig(void *dpy, GLXFBConfig config) { return NULL; }
STUB XID glXCreateWindow(void *dpy, GLXFBConfig config, XID win, const int *attrib) { return 0; }
STUB void glXDestroyWindow(void *dpy, XID win) {}
STUB XID glXCreatePixmap(void *dpy, GLXFBConfig config, XID pixmap, const int *attrib) { return 0; }
STUB void glXDestroyPixmap(void *dpy, XID pixmap) {}
STUB XID glXCreatePbuffer(void *dpy, GLXFBConfig config, const int *attrib) { return 0; }
STUB void glXDestroyPbuffer(void *dpy, XID pbuf) {}
STUB void glXQueryDrawable(void *dpy, XID draw, int attrib, unsigned int *value)
{
	if (value)
		*value = 0;
}
STUB GLXContext glXCreateNewContext(void *dpy, GLXFBConfig config, int type, GLXContext share, Bool direct) { return NULL; }
STUB Bool glXMakeContextCurrent(void *dpy, XID draw, XID read, GLXContext ctx) { return 0; }
STUB XID glXGetCurrentReadDrawable(void) { return 0; }
STUB int glXQueryContext(void *dpy, GLXContext ctx, int attrib, int *value) { return GLX_NO_EXTENSION; }
STUB void glXSelectEvent(void *dpy, XID draw, unsigned long mask) {}
STUB void glXGetSelectedEvent(void *dpy, XID draw, unsigned long *mask)
{
	if (mask)
		*mask = 0;
}

/* GLX 1.4 与 GLX_ARB_get_proc_address：没有任何扩展函数 */
STUB proc_t glXGetProcAddress(const unsigned char *name) { return NULL; }
STUB proc_t glXGetProcAddressARB(const unsigned char *name) { return NULL; }
