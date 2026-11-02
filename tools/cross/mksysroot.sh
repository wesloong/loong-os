#!/bin/sh
# 准备交叉编译用的目标架构 sysroot。在开发 chroot 里运行，由 tools/cross-build-pkg.sh、tools/cross-build-webrt.sh 调用。
#
# 用法: tools/cross/mksysroot.sh <sysroot 目录> <架构> <包...>
#   LOCAL_REPO=<仓库目录>  另加一个本地仓库（例如 $SRC/build/packages/packages），从中安装目标架构的包；
#                          设置后每次都运行 apk add -u，本地仓库里重新打的包会被升级进来。
#
# apk --root <sysroot> --arch <架构> 安装，不运行安装脚本（sysroot 只用来编译和链接，不需要模拟运行任何程序）。
# 公钥：目标架构的 Alpine 公钥 + 开发 chroot 的 /etc/apk/keys（含本地仓库的签名公钥）。
# 包列表与上次相同（且没有 LOCAL_REPO）时什么也不做。

set -eu
SYSROOT=${1:?用法: mksysroot.sh <sysroot> <架构> <包...>}
arch=${2:?缺少架构}
shift 2
pkgs="$*"
stamp=$SYSROOT/.loong-sysroot
if [ -z "${LOCAL_REPO:-}" ] && [ "$(cat "$stamp" 2>/dev/null)" = "$pkgs" ]; then
	exit 0
fi
echo ">>> 准备 $arch sysroot: $SYSROOT"
mkdir -p "$SYSROOT/etc/apk/keys"
cp /usr/share/apk/keys/"$arch"/*.pub /etc/apk/keys/*.pub "$SYSROOT/etc/apk/keys/"
initdb=""
[ -e "$SYSROOT/lib/apk/db/installed" ] || initdb=--initdb
repo=""
[ -z "${LOCAL_REPO:-}" ] || repo="--repository $LOCAL_REPO"
# shellcheck disable=SC2086
apk add --root "$SYSROOT" --arch "$arch" $initdb -U -u --scripts=no --no-progress -q \
	--repositories-file /etc/apk/repositories $repo $pkgs
# 指向绝对路径的符号链接（例如 libfoo.so -> /usr/lib/libfoo.so.1）在 sysroot 里会指到开发 chroot 自己的文件上，
# 改成相对链接
find "$SYSROOT/usr/lib" "$SYSROOT/lib" -type l 2>/dev/null | while read -r l; do
	t=$(readlink "$l")
	case "$t" in /*) ;; *) continue ;; esac
	ln -sfn "$(python3 -c 'import os, sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))' \
		"$SYSROOT$t" "$(dirname "$l")")" "$l"
done
echo "$pkgs" > "$stamp"
