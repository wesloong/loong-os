# Loong OS：在 x86_64 开发 chroot 里交叉编译 aarch64 Alpine（musl）程序的 CMake 工具链文件。
#
#   cmake -DCMAKE_TOOLCHAIN_FILE=tools/cross/aarch64-alpine.cmake -DLOONG_SYSROOT=<sysroot> …
#   （或者用环境变量：CMAKE_TOOLCHAIN_FILE、LOONG_SYSROOT；tools/cross-build-pkg.sh 就是这样用的）
#
# 编译器：宿主机的 clang/clang++（--target=aarch64-alpine-linux-musl），链接器 lld；
# ar/ranlib/strip/objcopy 用 Alpine 的 binutils-aarch64（aarch64-alpine-linux-musl-*）。
# sysroot：一个 aarch64 的 Alpine 根目录，用 apk --root <sysroot> --arch aarch64 装好 musl-dev、gcc
# （crt*.o、libgcc）、libstdc++-dev 和要链接的 -dev 包（不运行安装脚本，见 tools/cross-build-pkg.sh）。
#
# 头文件、库、CMake 包配置只在 sysroot 里找；程序（构建时在宿主机上运行的 Perl/Python/Ruby、gperf、
# glib-compile-resources、wayland-scanner 等）只在宿主机上找。pkg-config 也只看 sysroot 里的 .pc 文件，
# -I/-L 路径加上 sysroot 前缀。

set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR aarch64)
set(LOONG_CROSS_TRIPLE aarch64-alpine-linux-musl)

# sysroot：-D 参数优先，其次是环境变量。记进缓存，ninja 触发的重新配置也能拿到；
# try_compile 的子项目只继承 CMAKE_TRY_COMPILE_PLATFORM_VARIABLES 里列出的变量。
if (NOT LOONG_SYSROOT)
    set(LOONG_SYSROOT "$ENV{LOONG_SYSROOT}" CACHE PATH "目标架构（aarch64）的 Alpine sysroot")
endif ()
if (NOT LOONG_SYSROOT OR NOT EXISTS "${LOONG_SYSROOT}/usr/include/stdio.h")
    message(FATAL_ERROR "LOONG_SYSROOT（${LOONG_SYSROOT}）不是 aarch64 的 sysroot：先运行 tools/cross-build-pkg.sh 准备")
endif ()
list(APPEND CMAKE_TRY_COMPILE_PLATFORM_VARIABLES LOONG_SYSROOT)

set(CMAKE_SYSROOT "${LOONG_SYSROOT}")
set(CMAKE_C_COMPILER clang)
set(CMAKE_CXX_COMPILER clang++)
set(CMAKE_C_COMPILER_TARGET ${LOONG_CROSS_TRIPLE})
set(CMAKE_CXX_COMPILER_TARGET ${LOONG_CROSS_TRIPLE})
set(CMAKE_ASM_COMPILER_TARGET ${LOONG_CROSS_TRIPLE})
set(CMAKE_AR /usr/bin/${LOONG_CROSS_TRIPLE}-ar CACHE FILEPATH "")
set(CMAKE_RANLIB /usr/bin/${LOONG_CROSS_TRIPLE}-ranlib CACHE FILEPATH "")
set(CMAKE_STRIP /usr/bin/${LOONG_CROSS_TRIPLE}-strip CACHE FILEPATH "")
set(CMAKE_OBJCOPY /usr/bin/${LOONG_CROSS_TRIPLE}-objcopy CACHE FILEPATH "")
set(CMAKE_NM /usr/bin/${LOONG_CROSS_TRIPLE}-nm CACHE FILEPATH "")
# 不指定时 clang 会去找 aarch64-alpine-linux-musl-ld（binutils-aarch64 的 ld.bfd）；统一用 lld
set(CMAKE_EXE_LINKER_FLAGS_INIT "-fuse-ld=lld")
set(CMAKE_SHARED_LINKER_FLAGS_INIT "-fuse-ld=lld")
set(CMAKE_MODULE_LINKER_FLAGS_INIT "-fuse-ld=lld")

set(CMAKE_FIND_ROOT_PATH "${LOONG_SYSROOT}")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)

# pkg-config（宿主机的 pkgconf）：PKG_CONFIG_LIBDIR 替换默认搜索路径，宿主机的 .pc 不会混进来
set(ENV{PKG_CONFIG_LIBDIR} "${LOONG_SYSROOT}/usr/lib/pkgconfig:${LOONG_SYSROOT}/usr/share/pkgconfig")
set(ENV{PKG_CONFIG_SYSROOT_DIR} "${LOONG_SYSROOT}")
set(ENV{PKG_CONFIG_PATH} "")
set(PKG_CONFIG_EXECUTABLE /usr/bin/pkg-config CACHE FILEPATH "")

# 构建时运行的工具一律用宿主机的（sysroot 里 glib-dev、wayland-dev 带的是 aarch64 程序）
set(GLIB_COMPILE_RESOURCES_EXECUTABLE /usr/bin/glib-compile-resources CACHE FILEPATH "")
set(WAYLAND_SCANNER /usr/bin/wayland-scanner CACHE FILEPATH "")
