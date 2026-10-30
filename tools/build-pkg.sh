#!/bin/sh
# 在开发 chroot 中构建 system/packages/<包名> 下的 APK 包。
#
# 用法: tools/build-pkg.sh <包名> [abuild 参数...]
# 产物: build/packages/packages/<arch>/*.apk（已签名，可作为本地 apk 仓库；
#       abuild 用 APKBUILD 的上级目录名 packages 作为仓库名）
# 中间文件: build/abuild/<包名>/（不会弄脏源码目录）
#
# 环境变量: JOBS（并行编译数，默认 3）

set -eu

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
pkg=${1:?用法: tools/build-pkg.sh <包名>}
shift
[ -f "$REPO_ROOT/system/packages/$pkg/APKBUILD" ] || { echo "没有这个包: $pkg" >&2; exit 1; }

exec "$REPO_ROOT/tools/dev-chroot.sh" run /bin/sh -c '
set -eu
pkg=$1; shift
key=$(ls /root/.abuild/*.rsa 2>/dev/null | head -1 || true)
if [ -z "$key" ]; then
	PACKAGER="Loong OS Developers <dev@loong-os.local>" abuild-keygen -n >/dev/null 2>&1
	key=$(ls /root/.abuild/*.rsa | head -1)
fi
cp -n "$key.pub" /etc/apk/keys/ 2>/dev/null || true
cat > /root/.abuild/abuild.conf <<EOF
PACKAGER="Loong OS Developers <dev@loong-os.local>"
PACKAGER_PRIVKEY="$key"
EOF
export JOBS='"${JOBS:-3}"'
export SRCDEST=$SRC/build/distfiles
export REPODEST=$SRC/build/packages
export srcdir=$SRC/build/abuild/$pkg/src
export pkgbasedir=$SRC/build/abuild/$pkg/pkg
mkdir -p "$srcdir" "$pkgbasedir" "$SRCDEST" "$REPODEST"
cd $SRC/system/packages/$pkg
abuild -F -r "$@"
' build-pkg "$pkg" "$@"
