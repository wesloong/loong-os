#!/bin/sh
# 交叉编译 Web 运行时（webrt/：loong-webapp、loong-browser）的 aarch64 版本，供 aarch64 镜像（WITH_WPE=1）使用。
#
# 用法（在宿主机上）: tools/cross-build-webrt.sh [test]
#   test  编译后经 qemu-user 运行单元测试（meson test）
#
# 产物: build/webrt-<架构>/（mkimage.sh 构建这个架构的镜像时从这里取 loong-webapp 与 browser/loong-browser）
# sysroot: build/cross/webrt-<架构>/sysroot
#
# 需要本地仓库（build/packages）里有该架构的 wpewebkit-dev：先运行 tools/cross-build-pkg.sh wpewebkit。
# 环境变量：ARCH（默认 aarch64）、JOBS（默认 2）。

set -eu

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
ARCH=${ARCH:-aarch64}
[ -f "$REPO_ROOT/tools/cross/$ARCH-alpine.meson" ] || { echo "没有 $ARCH 的 meson 交叉编译配置 tools/cross/$ARCH-alpine.meson" >&2; exit 1; }

exec "$REPO_ROOT/tools/dev-chroot.sh" run /bin/sh -c '
set -eu
arch=$1 jobs=$2; shift 2
W=$SRC/build/cross/webrt-$arch
SYSROOT=$W/sysroot
OUT=$SRC/build/webrt-$arch
ls "$SRC/build/packages/packages/$arch"/wpewebkit-dev-*.apk >/dev/null 2>&1 ||
	{ echo "cross-build-webrt: 本地仓库里没有 $arch 的 wpewebkit-dev：先运行 tools/cross-build-pkg.sh wpewebkit" >&2; exit 1; }

# 构建时在宿主机上运行的：meson、wayland-scanner（wayland-dev）、glib-compile-resources（glib-dev）
missing=""
for p in meson wayland-dev glib-dev binutils-$arch qemu-$arch; do
	apk info -e "$p" >/dev/null || missing="$missing $p"
done
# shellcheck disable=SC2086
[ -z "$missing" ] || flock /var/lock/loong-apk apk add -q --no-progress $missing

# webrt 链接的库（见 webrt/meson.build、webrt/platform/meson.build）；wpewebkit-dev 来自本地仓库
LOCAL_REPO=$SRC/build/packages/packages "$SRC/tools/cross/mksysroot.sh" "$SYSROOT" "$arch" \
	musl-dev gcc wpewebkit-dev glib-dev wayland-dev wayland-protocols libxkbcommon-dev libdrm-dev

printf "[constants]\nsysroot = %s\n" "'\''$SYSROOT'\''" > "$W/sysroot.ini"
if [ -f "$OUT/build.ninja" ]; then
	meson setup --reconfigure "$OUT" "$SRC/webrt" >/dev/null
else
	meson setup --cross-file "$W/sysroot.ini" --cross-file "$SRC/tools/cross/$arch-alpine.meson" "$OUT" "$SRC/webrt"
fi
ninja -C "$OUT" -j"$jobs"
file "$OUT/loong-webapp" "$OUT/browser/loong-browser"
[ "${1:-}" != test ] || meson test -C "$OUT" --print-errorlogs
' cross-build-webrt "$ARCH" "${JOBS:-2}" "$@"
