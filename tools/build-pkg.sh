#!/bin/sh
# 在开发 chroot 中构建 system/packages/<包名> 下的 APK 包。
#
# 用法: tools/build-pkg.sh <包名> [abuild 参数...]
# 产物: build/packages/packages/<arch>/*.apk（已签名，可作为本地 apk 仓库；不分架构的包同时放进 x86_64 与 aarch64；
#       abuild 用 APKBUILD 的上级目录名 packages 作为仓库名）
# 中间文件: build/abuild/<包名>/（不会弄脏源码目录；跨架构时为 build/abuild/<包名>-<架构>/）
#
# 环境变量:
#   JOBS  并行编译数，默认 3
#   ARCH  目标架构，默认与开发 chroot 相同。不同时（例如在 x86_64 上构建 aarch64 的包），在目标架构的
#         Alpine 构建 chroot（build/chroot-<架构>/）里经 qemu-user 本地编译：先在宿主机运行一次
#         tools/setup-binfmt.sh；构建 chroot 第一次使用时自动创建（Alpine 基础系统 + alpine-sdk），之后复用。
#         编译器等全部经 qemu-user 模拟运行，比同架构编译慢一个数量级。签名密钥与开发 chroot 共用。
#         WPE WebKit 这样的大包这样编译要好几天，改用 tools/cross-build-pkg.sh 交叉编译（编译器原生运行）。

set -eu

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
pkg=${1:?用法: tools/build-pkg.sh <包名>}
shift
[ -f "$REPO_ROOT/system/packages/$pkg/APKBUILD" ] || { echo "没有这个包: $pkg" >&2; exit 1; }
ARCH=${ARCH:-$(uname -m)}

# 在构建 chroot 里运行 abuild。参数：包名 中间文件目录名 [abuild 参数...]
ABUILD_SH='
set -eu
pkg=$1 work=$2; shift 2
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
export srcdir=$SRC/build/abuild/$work/src
export pkgbasedir=$SRC/build/abuild/$work/pkg
mkdir -p "$srcdir" "$pkgbasedir" "$SRCDEST" "$REPODEST"
cd $SRC/system/packages/$pkg
abuild -F -r "$@"
# 不分架构的包（字体、数据）：abuild 只放进本机架构的仓库目录。复制一份到另一个架构的目录并重建索引，
# 跨架构构建的镜像（aarch64 的 qemu-aarch64、xiaomi-nabu）才装得上。
if [ $# -eq 0 ] && grep -q "^arch=\"noarch\"" APKBUILD; then
	host=$(apk --print-arch)
	repo=$REPODEST/$(basename "$(dirname "$PWD")")
	for a in x86_64 aarch64; do
		[ "$a" != "$host" ] || continue
		mkdir -p "$repo/$a"
		for f in $(abuild -F listpkg); do cp "$repo/$host/$f" "$repo/$a/"; done
		(cd "$repo/$a" && rm -f APKINDEX.tar.gz && apk index -q --rewrite-arch "$a" -o APKINDEX.tar.gz *.apk && abuild-sign -q APKINDEX.tar.gz)
		echo ">>> 已复制到 $repo/$a"
	done
fi
'

if [ "$ARCH" = "$(uname -m)" ]; then
	exec "$REPO_ROOT/tools/dev-chroot.sh" run /bin/sh -c "$ABUILD_SH" build-pkg "$pkg" "$pkg" "$@"
fi

# 跨架构：在开发 chroot 里准备目标架构的构建 chroot，挂载仓库后 chroot 进去运行 ABUILD_SH。
exec "$REPO_ROOT/tools/dev-chroot.sh" run /bin/sh -c '
set -eu
arch=$1 abuild_sh=$2; shift 2
R=$SRC/build/chroot-$arch
die() { echo "build-pkg: $*" >&2; exit 1; }

# 签名密钥用开发 chroot 的：本地仓库的包和索引都由它签名，mkimage 信任开发 chroot 的 /etc/apk/keys
key=$(ls /root/.abuild/*.rsa 2>/dev/null | head -1 || true)
if [ -z "$key" ]; then
	PACKAGER="Loong OS Developers <dev@loong-os.local>" abuild-keygen -n >/dev/null 2>&1
	key=$(ls /root/.abuild/*.rsa | head -1)
fi
cp -n "$key.pub" /etc/apk/keys/ 2>/dev/null || true

# 与 mkimage.sh 相同的两步：先不运行安装脚本装一个最小系统（apk 3 在嵌套 chroot 里无法为安装脚本
# 创建命名空间），再 chroot 进去由目标系统自己的 apk 安装其余软件包
if [ ! -e "$R/lib/apk/db/installed" ]; then
	echo ">>> 创建 $arch 构建 chroot: $R"
	mkdir -p "$R/etc/apk/keys"
	cp /usr/share/apk/keys/"$arch"/*.pub "$R/etc/apk/keys/"
	cp /etc/apk/repositories "$R/etc/apk/repositories"
	apk add --root "$R" --initdb --arch "$arch" --no-cache --no-progress -q --scripts=no \
		--repositories-file "$R/etc/apk/repositories" alpine-baselayout busybox apk-tools alpine-keys
	chroot "$R" /bin/busybox true 2>/dev/null ||
		die "无法在 chroot 中运行 $arch 程序：请先在宿主机上运行 tools/setup-binfmt.sh"
	chroot "$R" /bin/busybox --install -s
fi

cleanup() {
	for m in "$SRC" /dev /proc; do
		umount "$R$m" 2>/dev/null || true
	done
}
trap cleanup EXIT
mkdir -p "$R/proc" "$R/dev" "$R$SRC"
mount -t proc proc "$R/proc"
mount --bind /dev "$R/dev"
mount --bind "$SRC" "$R$SRC"
cp -L /etc/resolv.conf "$R/etc/resolv.conf"
# 开发 chroot 的证书包（含代理 CA），供 abuild 下载源码
cp /etc/ssl/certs/ca-certificates.crt "$R/etc/ssl/loong-host-ca.crt" 2>/dev/null || true

if [ ! -x "$R/usr/bin/abuild" ]; then
	chroot "$R" /sbin/apk fix --no-cache --no-progress -q --reinstall alpine-baselayout busybox
	chroot "$R" /sbin/apk add --no-cache --no-progress -q alpine-sdk
fi
# abuild -r 用 apk 安装构建依赖，需要本地的仓库索引
chroot "$R" /sbin/apk update -q
mkdir -p "$R/root/.abuild"
cp "$key" "$key.pub" "$R/root/.abuild/"
cp "$key.pub" "$R/etc/apk/keys/"

chroot "$R" /usr/bin/env -i \
	HOME=/root TERM="${TERM:-xterm}" LANG=C.UTF-8 \
	PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
	HTTPS_PROXY="${HTTPS_PROXY:-}" https_proxy="${HTTPS_PROXY:-}" \
	NO_PROXY="${NO_PROXY:-}" no_proxy="${NO_PROXY:-}" \
	SSL_CERT_FILE=/etc/ssl/loong-host-ca.crt CURL_CA_BUNDLE=/etc/ssl/loong-host-ca.crt \
	SRC="$SRC" \
	/bin/sh -c "$abuild_sh" build-pkg "$@"
' build-pkg-cross "$ARCH" "$ABUILD_SH" "$pkg" "$pkg-$ARCH" "$@"
