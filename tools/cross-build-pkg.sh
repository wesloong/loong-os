#!/bin/sh
# 在 x86_64 开发 chroot 里交叉编译 system/packages/<包名>，产出 aarch64 的 APK。
# 编译器（clang/lld）原生运行，不经 qemu-user：WPE WebKit 这样的大包在 aarch64 构建 chroot 里模拟编译要好几天
# （tools/build-pkg.sh 的 ARCH=aarch64），交叉编译与本机编译 x86_64 版本差不多快。
#
# 用法（在宿主机上，与 tools/build-pkg.sh 相同）:
#   tools/cross-build-pkg.sh <包名>                     从头构建：fetch verify unpack prepare build rootpkg index
#   tools/cross-build-pkg.sh <包名> build rootpkg index  中断后继续（不重新解包，ninja 接着编译），然后打包
#   tools/cross-build-pkg.sh <包名> rootpkg index        只重新打包（已编译完）
#   tools/cross-build-pkg.sh <包名> sysroot              只准备 sysroot
#   tools/cross-build-pkg.sh <包名> clean                删除中间文件和 sysroot（APK 保留）
#
# 产物: build/packages/packages/<架构>/（与 tools/build-pkg.sh 同一个本地仓库，同一把签名密钥）
# 中间文件: build/cross/<包名>-<架构>/src、pkg；sysroot: build/cross/<包名>-<架构>/sysroot
#
# 环境变量:
#   ARCH  目标架构，默认 aarch64（工具链文件 tools/cross/<架构>-alpine.cmake）
#   JOBS  并行编译数，默认 2
#
# 做法（abuild 自带的交叉编译模式，即 Alpine bootstrap 用的那套）：
#   1. sysroot：apk --root <sysroot> --arch aarch64 安装 musl-dev、gcc（crt*.o、libgcc）、libstdc++-dev 和
#      APKBUILD 的 makedepends_host（要链接的 -dev 包），不运行安装脚本；不需要模拟运行任何程序。
#      APKBUILD 的 makedepends_build（构建时运行的工具）装在开发 chroot 里，另外装 binutils-<架构>
#      （aarch64-alpine-linux-musl-ar/strip/objcopy；abuild 用 <三元组>-strip 去符号）。
#   2. abuild 以 CHOST=<架构>、CBUILDROOT=<sysroot> 运行（-d：依赖已由上一步装好）。abuild 据此把 CARCH 设为
#      aarch64、CFLAGS 加上 --sysroot、按 sysroot 里装的包推算 so:/pc: 依赖、用 <三元组>-strip 去符号。
#      默认编译参数取自目标架构的 abuild（ABUILD_DEFCONF；x86_64 的另有只适用于 x86 的 -fno-plt）。
#   3. CMake：环境变量 CMAKE_TOOLCHAIN_FILE 指向 tools/cross/<架构>-alpine.cmake（clang --target、sysroot、
#      pkg-config、只在宿主机上找程序）。APKBUILD 要支持交叉编译：分出 makedepends_build / makedepends_host，
#      CBUILD 与 CHOST 不同时补上 CMake 探测不到的选项（见 system/packages/wpewebkit/APKBUILD）。
# 已支持的包：wpewebkit。

set -eu

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
pkg=${1:?用法: tools/cross-build-pkg.sh <包名> [abuild 阶段...|sysroot|clean]}
shift
[ -f "$REPO_ROOT/system/packages/$pkg/APKBUILD" ] || { echo "没有这个包: $pkg" >&2; exit 1; }
ARCH=${ARCH:-aarch64}
[ -f "$REPO_ROOT/tools/cross/$ARCH-alpine.cmake" ] || { echo "没有 $ARCH 的工具链文件 tools/cross/$ARCH-alpine.cmake" >&2; exit 1; }
[ "$ARCH" != "$(uname -m)" ] || { echo "目标架构与开发 chroot 相同，用 tools/build-pkg.sh" >&2; exit 1; }

exec "$REPO_ROOT/tools/dev-chroot.sh" run /bin/sh -c '
set -eu
pkg=$1 arch=$2 jobs=$3; shift 3
[ $# -gt 0 ] || set -- fetch verify unpack prepare build rootpkg index
W=$SRC/build/cross/$pkg-$arch
SYSROOT=$W/sysroot
die() { echo "cross-build-pkg: $*" >&2; exit 1; }
log() { echo ">>> cross-build-pkg: $*"; }

if [ "$1" = clean ]; then
	rm -rf "$W"
	log "已删除 $W"
	exit 0
fi

cd "$SRC/system/packages/$pkg"
makedepends_build=$(. ./APKBUILD; echo ${makedepends_build:-})
makedepends_host=$(. ./APKBUILD; echo ${makedepends_host:-})
source_urls=$(. ./APKBUILD; echo $source)
[ -n "$makedepends_host" ] || die "$pkg 的 APKBUILD 没有分出 makedepends_build / makedepends_host，不支持交叉编译"

# ── 构建机上的工具 ──
missing=""
for p in $makedepends_build binutils-$arch; do
	apk info -e "$p" >/dev/null || missing="$missing $p"
done
if [ -n "$missing" ]; then
	log "在开发 chroot 里安装:$missing"
	# shellcheck disable=SC2086
	flock /var/lock/loong-apk apk add -q --no-progress $missing
fi

# ── sysroot：C 库与启动文件、libgcc、libstdc++ 头文件，加上 makedepends_host ──
# shellcheck disable=SC2086
"$SRC/tools/cross/mksysroot.sh" "$SYSROOT" "$arch" musl-dev gcc libstdc++-dev $makedepends_host
[ "$1" = sysroot ] && { log "sysroot 已就绪: $SYSROOT（$(du -sh "$SYSROOT" | cut -f1)）"; exit 0; }

# ── 签名密钥（与 tools/build-pkg.sh 相同：本地仓库的包和索引都用开发 chroot 的密钥） ──
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

# ── 目标架构 abuild 的默认编译参数 ──
defconf=$W/abuild-default.conf
if [ ! -s "$defconf" ]; then
	rm -rf "$W/abuild-apk"
	mkdir -p "$W/abuild-apk"
	apk fetch -q --arch "$arch" --keys-dir "/usr/share/apk/keys/$arch" --repositories-file /etc/apk/repositories \
		-o "$W/abuild-apk" abuild
	tar -xzf "$W"/abuild-apk/abuild-*.apk -C "$W/abuild-apk" usr/share/abuild/default.conf 2>/dev/null
	cp "$W/abuild-apk/usr/share/abuild/default.conf" "$defconf"
	rm -rf "$W/abuild-apk"
fi

# ── 源码：主仓库已经下载过的直接硬链接过来（worktree 的 build/ 是独立的） ──
export SRCDEST=$SRC/build/distfiles
mkdir -p "$SRCDEST"
for s in $source_urls; do
	f=${s##*/}
	f=${f%%::*}
	case "$s" in *://*) ;; *) continue ;; esac
	[ -e "$SRCDEST/$f" ] || [ ! -e "/src/build/distfiles/$f" ] || ln "/src/build/distfiles/$f" "$SRCDEST/$f" 2>/dev/null ||
		cp "/src/build/distfiles/$f" "$SRCDEST/$f"
done

export CHOST=$arch CBUILDROOT=$SYSROOT ABUILD_DEFCONF=$defconf
export CMAKE_TOOLCHAIN_FILE=$SRC/tools/cross/$arch-alpine.cmake LOONG_SYSROOT=$SYSROOT
export JOBS=$jobs REPODEST=$SRC/build/packages srcdir=$W/src pkgbasedir=$W/pkg
# ccache 对交叉编译没有帮助（与 x86_64 的结果不能共用），还会占满共用的缓存目录
export CCACHE_DISABLE=1
mkdir -p "$srcdir" "$pkgbasedir" "$REPODEST"
log "abuild $* （$arch，JOBS=$jobs，开始于 $(date "+%F %T")）"
start=$(date +%s)
abuild -F -d "$@"
end=$(date +%s)
log "完成于 $(date "+%F %T")，用时 $(( (end - start) / 3600 )) 小时 $(( (end - start) / 60 % 60 )) 分"
' cross-build-pkg "$pkg" "$ARCH" "${JOBS:-2}" "$@"
