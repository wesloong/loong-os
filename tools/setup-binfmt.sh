#!/bin/sh
# 在宿主机上注册 qemu-user 的 binfmt_misc，让 chroot 里可以直接运行其他架构（如 aarch64）的程序。
# 跨架构构建镜像（mkimage.sh 会 chroot 进目标 rootfs 运行 apk 和安装脚本）需要它。
#
# 用法（在宿主机上以 root 运行，不是在 chroot 里）:
#   tools/setup-binfmt.sh [架构...]          注册（默认 aarch64）。已注册时什么也不做，可重复运行
#   tools/setup-binfmt.sh status             查看注册状态
#   tools/setup-binfmt.sh remove [架构...]   注销本脚本注册的项
#
# 解释器使用 Alpine 的 qemu-<架构>（static-pie，静态链接）：先装进开发 chroot（apk 会校验签名），
# 再复制到 <主仓库>/build/qemu-user/。注册时带两个标志：
#   F（fix-binary）     注册时内核就打开解释器文件，之后在任何 chroot 里都能用，chroot 里不需要放 qemu；
#   P（preserve-argv0） 保留原始 argv[0]，busybox 等多入口程序依赖它。
# 只影响对应架构的 ELF 文件（宿主机原本就无法运行它们），不会改变宿主机自身程序的行为。
# binfmt_misc 的注册不会在重启后保留，重启后需要重新运行本脚本。

set -eu

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
MAIN_ROOT=$(git -C "$REPO_ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)
MAIN_ROOT=${MAIN_ROOT%/.git}
[ -n "$MAIN_ROOT" ] && [ -d "$MAIN_ROOT" ] || MAIN_ROOT=$REPO_ROOT
CHROOT=${CHROOT:-$MAIN_ROOT/build/chroot}
QEMU_DIR=${QEMU_DIR:-$MAIN_ROOT/build/qemu-user}
BINFMT=/proc/sys/fs/binfmt_misc
FLAGS=FP

die() { echo "setup-binfmt: $*" >&2; exit 1; }
info() { echo "setup-binfmt: $*"; }

# 各架构的 ELF 头魔数与掩码（十六进制，取自 QEMU 的 scripts/qemu-binfmt-conf.sh）。
# 与 binfmt_misc 状态文件里的显示格式相同，便于比较；注册时转成 \xNN 形式。
magic_of() {
	case "$1" in
		aarch64) echo 7f454c460201010000000000000000000200b700 ;;
		riscv64) echo 7f454c460201010000000000000000000200f300 ;;
		*) return 1 ;;
	esac
}
MASK=ffffffffffffff00fffffffffffffffffeffffff
escape() { printf '%s\n' "$1" | sed 's/../\\x&/g'; }

ensure_mounted() {
	[ -e "$BINFMT/register" ] && return 0
	grep -qw binfmt_misc /proc/filesystems || die "内核不支持 binfmt_misc"
	info "挂载 binfmt_misc 到 $BINFMT"
	mount -t binfmt_misc binfmt_misc "$BINFMT"
}

# 已有启用中的项处理这个架构（不论是谁注册的）时，输出该项的名字。
existing_entry() {
	hex=$(magic_of "$1")
	for e in "$BINFMT"/*; do
		case "${e##*/}" in register|status) continue ;; esac
		[ -f "$e" ] || continue
		grep -qx enabled "$e" || continue
		if grep -q "^magic $hex" "$e"; then
			echo "${e##*/}"
			return 0
		fi
	done
	return 1
}

is_static() {
	if command -v file >/dev/null 2>&1; then
		file -L "$1" | grep -q 'static'
	elif command -v readelf >/dev/null 2>&1; then
		! readelf -l "$1" | grep -q 'INTERP'
	else
		info "警告：没有 file/readelf，无法确认 $1 是静态链接的"
	fi
}

interpreter() {
	arch=$1
	dst="$QEMU_DIR/qemu-$arch-static"
	src="$CHROOT/usr/bin/qemu-$arch"
	if [ ! -x "$src" ]; then
		[ -f "$CHROOT/etc/alpine-release" ] || die "找不到开发 chroot（$CHROOT），请先运行 tools/dev-chroot.sh init"
		info "在开发 chroot 里安装 qemu-$arch"
		"$REPO_ROOT/tools/dev-chroot.sh" run apk add -q --no-progress "qemu-$arch" >&2
	fi
	is_static "$src" || die "$src 不是静态链接的，不能用作 F 标志的 binfmt 解释器"
	mkdir -p "$QEMU_DIR"
	if ! cmp -s "$src" "$dst" 2>/dev/null; then
		cp "$src" "$dst.tmp"
		mv "$dst.tmp" "$dst"
	fi
	echo "$dst"
}

do_register() {
	host=$(uname -m)
	ensure_mounted
	for arch in "$@"; do
		magic=$(magic_of "$arch") || die "不支持的架构: $arch"
		if [ "$arch" = "$host" ]; then
			info "$arch 就是宿主机架构，不需要注册"
			continue
		fi
		if name=$(existing_entry "$arch"); then
			info "$arch 已注册（$BINFMT/$name，解释器 $(sed -n 's/^interpreter //p' "$BINFMT/$name")，标志 $(sed -n 's/^flags: //p' "$BINFMT/$name")）"
			continue
		fi
		[ -e "$BINFMT/qemu-$arch" ] && die "$BINFMT/qemu-$arch 已存在但处于禁用状态，请先手动处理"
		interp=$(interpreter "$arch")
		printf '%s' ":qemu-$arch:M::$(escape "$magic"):$(escape "$MASK"):$interp:$FLAGS" > "$BINFMT/register"
		info "已注册 $arch → $interp（标志 $FLAGS）"
	done
}

do_status() {
	if [ ! -e "$BINFMT/register" ]; then
		echo "binfmt_misc 未挂载"
		return 0
	fi
	echo "binfmt_misc: $(cat "$BINFMT/status")"
	for e in "$BINFMT"/*; do
		case "${e##*/}" in register|status) continue ;; esac
		[ -f "$e" ] || continue
		printf '%s: %s，%s，%s\n' "${e##*/}" "$(head -1 "$e")" \
			"$(sed -n 's/^interpreter //p' "$e")" "$(sed -n 's/^flags: /标志 /p' "$e")"
	done
}

do_remove() {
	for arch in "$@"; do
		e="$BINFMT/qemu-$arch"
		if [ -e "$e" ] && grep -q "^interpreter $QEMU_DIR/" "$e"; then
			echo -1 > "$e"
			info "已注销 qemu-$arch"
		else
			info "qemu-$arch 不是本脚本注册的（或未注册），不处理"
		fi
	done
}

[ "$(id -u)" = 0 ] || die "需要 root 权限"
cmd=${1:-register}
case "$cmd" in
	status|remove|register|help|-h|--help) [ $# -gt 0 ] && shift ;;
	*) cmd=register ;;
esac
[ $# -gt 0 ] || set -- aarch64
case "$cmd" in
	status) do_status ;;
	remove) do_remove "$@" ;;
	register) do_register "$@" ;;
	*) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//' ;;
esac
