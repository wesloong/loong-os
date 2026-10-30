#!/bin/sh
# Loong OS 开发环境：基于 Alpine 的构建 chroot。
#
# 用法:
#   tools/dev-chroot.sh init            创建 chroot 并安装开发依赖
#   tools/dev-chroot.sh run <命令...>   在 chroot 内执行命令（工作目录为仓库挂载点 $SRC）
#   tools/dev-chroot.sh shell           进入交互 shell
#   tools/dev-chroot.sh umount          卸载 chroot 的挂载点
#
# 环境变量:
#   CHROOT         chroot 目录（默认 <主仓库>/build/chroot，所有 git worktree 共用一个 chroot）
#   SRC_MOUNT      仓库在 chroot 内的挂载点（主仓库为 /src，worktree 为 /wt/<目录名>）
#   ALPINE_BRANCH  Alpine 版本（默认 v3.24）
#   MIRROR         Alpine 镜像（默认 http://dl-cdn.alpinelinux.org/alpine，包本身有签名校验）
#
# 需要 root（或 CAP_SYS_ADMIN）权限，用于 bind mount。

set -eu

ALPINE_BRANCH=${ALPINE_BRANCH:-v3.24}
MIRROR=${MIRROR:-http://dl-cdn.alpinelinux.org/alpine}
REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
# git worktree 与主仓库共用 .git；chroot 与依赖缓存放在主仓库，各 worktree 的构建产物各自独立。
MAIN_ROOT=$(git -C "$REPO_ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)
MAIN_ROOT=${MAIN_ROOT%/.git}
[ -n "$MAIN_ROOT" ] && [ -d "$MAIN_ROOT" ] || MAIN_ROOT=$REPO_ROOT
CHROOT=${CHROOT:-$MAIN_ROOT/build/chroot}
if [ "$REPO_ROOT" = "$MAIN_ROOT" ]; then
	SRC_MOUNT=${SRC_MOUNT:-/src}
else
	SRC_MOUNT=${SRC_MOUNT:-/wt/$(basename "$REPO_ROOT")}
fi
ARCH=$(uname -m)

# 开发所需的包。按用途分组，便于维护。
PKGS_BASE="alpine-base alpine-sdk bash git curl ca-certificates ccache"
PKGS_TOOLCHAIN="clang lld cmake samurai meson pkgconf rust cargo rustfmt rust-clippy python3 perl ruby ruby-getoptlong"
PKGS_SHELL="wayland-dev wayland-protocols libxkbcommon-dev libinput-dev eudev-dev libseat-dev
  mesa-dev libdrm-dev pixman-dev mesa-dri-gallium mesa-egl mesa-gles mesa-gbm seatd
  font-inter font-noto-cjk weston weston-clients foot
  librime-dev rime-plum-data"
PKGS_IMAGE="erofs-utils e2fsprogs mtools dosfstools sfdisk ukify py3-pefile systemd-boot systemd-efistub
  busybox-static kmod zstd xz cpio file squashfs-tools qemu-system-x86_64 qemu-hw-display-virtio-gpu qemu-hw-display-virtio-gpu-pci ovmf"
# 跨架构（aarch64）：交叉编译用的标准库源码、qemu-user（binfmt，见 tools/setup-binfmt.sh）、
# aarch64 虚拟机（自带 edk2 固件），以及 Android 启动镜像工具（boot.img、super、sparse、vbmeta）
PKGS_CROSS="rust-src qemu-aarch64 qemu-system-aarch64
  android-tools-mkbootimg android-tools-unpack_bootimg android-tools-lpmake android-tools-lpdump
  android-tools-img2simg android-tools-simg2img android-tools-avbtool"

die() { echo "dev-chroot: $*" >&2; exit 1; }

[ "$(id -u)" = 0 ] || die "需要 root 权限"

fetch_apk_static() {
	# 从 Alpine main 仓库下载 apk-tools-static，取出 apk.static。
	tmp=$(mktemp -d)
	idx="$MIRROR/$ALPINE_BRANCH/main/$ARCH/APKINDEX.tar.gz"
	ver=$(curl -fsS "$idx" | tar -xzO APKINDEX \
		| awk '/^P:apk-tools-static$/{f=1} f&&/^V:/{print substr($0,3); exit}')
	[ -n "$ver" ] || die "找不到 apk-tools-static"
	curl -fsS -o "$tmp/apk.apk" "$MIRROR/$ALPINE_BRANCH/main/$ARCH/apk-tools-static-$ver.apk"
	tar -xzf "$tmp/apk.apk" -C "$tmp" sbin/apk.static 2>/dev/null || true
	[ -x "$tmp/sbin/apk.static" ] || die "解压 apk.static 失败"
	echo "$tmp/sbin/apk.static"
}

is_mounted() { grep -q " $1 " /proc/mounts; }

do_mount() {
	mkdir -p "$CHROOT/proc" "$CHROOT/sys" "$CHROOT/dev" "$CHROOT/src"
	is_mounted "$CHROOT/proc" || mount -t proc proc "$CHROOT/proc"
	is_mounted "$CHROOT/sys" || mount --rbind /sys "$CHROOT/sys"
	is_mounted "$CHROOT/dev" || mount --rbind /dev "$CHROOT/dev"
	is_mounted "$CHROOT/src" || mount --bind "$MAIN_ROOT" "$CHROOT/src"
	if [ "$SRC_MOUNT" != /src ]; then
		mkdir -p "$CHROOT$SRC_MOUNT"
		is_mounted "$CHROOT$SRC_MOUNT" || mount --bind "$REPO_ROOT" "$CHROOT$SRC_MOUNT"
	fi
	# 网络与证书：复用宿主机的 DNS 与代理 CA。
	cp -L /etc/resolv.conf "$CHROOT/etc/resolv.conf"
	if [ -f /root/.ccr/ca-bundle.crt ]; then
		mkdir -p "$CHROOT/usr/local/share/ca-certificates"
		cp /root/.ccr/ca-bundle.crt "$CHROOT/usr/local/share/ca-certificates/host-proxy.crt"
	fi
}

do_umount() {
	# worktree 只卸载自己的挂载点：chroot 的 /src、/dev、/proc 由所有 worktree（和后台构建）共用。
	if [ "$SRC_MOUNT" != /src ]; then
		if is_mounted "$CHROOT$SRC_MOUNT"; then
			umount "$CHROOT$SRC_MOUNT" 2>/dev/null || umount -l "$CHROOT$SRC_MOUNT"
		fi
		return 0
	fi
	if pgrep -f "chroot $CHROOT" >/dev/null 2>&1; then
		die "chroot 中仍有进程在运行（例如后台编译），不卸载"
	fi
	for m in src dev sys proc; do
		if is_mounted "$CHROOT/$m"; then
			umount -R "$CHROOT/$m" 2>/dev/null || umount -lR "$CHROOT/$m"
		fi
	done
}

chroot_exec() {
	do_mount
	env -i \
		HOME=/root TERM="${TERM:-xterm}" LANG=C.UTF-8 \
		PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
		HTTPS_PROXY="${HTTPS_PROXY:-}" https_proxy="${HTTPS_PROXY:-}" \
		NO_PROXY="${NO_PROXY:-}" no_proxy="${NO_PROXY:-}" \
		SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
		SRC="$SRC_MOUNT" \
		CARGO_HOME=/src/build/cargo-home \
		CARGO_TARGET_DIR="$SRC_MOUNT/build/target-alpine" \
		CCACHE_DIR=/src/build/ccache \
		chroot "$CHROOT" /bin/sh -c 'cd "$SRC" && exec "$@"' sh "$@"
}

do_init() {
	if [ -f "$CHROOT/etc/alpine-release" ]; then
		echo "chroot 已存在: $CHROOT"
	else
		mkdir -p "$CHROOT/etc/apk"
		apk_static=$(fetch_apk_static)
		printf '%s\n' "$MIRROR/$ALPINE_BRANCH/main" "$MIRROR/$ALPINE_BRANCH/community" \
			> "$CHROOT/etc/apk/repositories"
		# 首次引导：先装 alpine-keys，之后所有包都做签名校验。
		"$apk_static" --root "$CHROOT" --initdb --arch "$ARCH" \
			--repositories-file "$CHROOT/etc/apk/repositories" \
			--allow-untrusted -U add alpine-keys
		"$apk_static" --root "$CHROOT" --arch "$ARCH" \
			--repositories-file "$CHROOT/etc/apk/repositories" \
			--keys-dir "$CHROOT/etc/apk/keys" add alpine-base
		rm -rf "$(dirname "$(dirname "$apk_static")")"
	fi
	# shellcheck disable=SC2086
	chroot_exec apk add --no-progress $PKGS_BASE $PKGS_TOOLCHAIN $PKGS_SHELL $PKGS_IMAGE $PKGS_CROSS
	chroot_exec update-ca-certificates >/dev/null 2>&1 || true
	echo "完成: $CHROOT ($(cat "$CHROOT/etc/alpine-release"))"
}

cmd=${1:-help}
[ $# -gt 0 ] && shift
case "$cmd" in
	init) do_init ;;
	run) [ $# -gt 0 ] || die "run 需要命令"; chroot_exec "$@" ;;
	shell) chroot_exec /bin/bash -l ;;
	umount) do_umount ;;
	*) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//' ;;
esac
