#!/bin/sh
# 中断后继续编译 WPE WebKit（不重新解包、不从头编译），完成后打包成 APK。
# 用法: tools/resume-wpewebkit.sh  （日志追加到 build/logs/wpewebkit.log）
set -eu
REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
exec "$REPO_ROOT/tools/dev-chroot.sh" run /bin/sh -c '
set -eu
cd "$SRC/system/packages/wpewebkit"
. ./APKBUILD
apk add -q --no-progress --virtual .makedepends-wpewebkit $makedepends $depends
export srcdir="$SRC/build/abuild/wpewebkit/src" pkgbasedir="$SRC/build/abuild/wpewebkit/pkg"
export REPODEST="$SRC/build/packages" SRCDEST="$SRC/build/distfiles"
ninja -C "$srcdir/wpewebkit-$pkgver/build" -j"${JOBS:-3}"
echo ">>> 编译完成，开始打包"
abuild -F -K rootpkg
abuild -F index 2>/dev/null || true
echo ">>> wpewebkit 打包完成"
'
