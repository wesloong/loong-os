#!/bin/sh
# Loong OS 镜像构建器。在开发 chroot 内运行：
#
#   tools/dev-chroot.sh run system/mkimage/mkimage.sh qemu-x86_64
#   tools/dev-chroot.sh run system/mkimage/mkimage.sh qemu-aarch64
#   tools/dev-chroot.sh run system/mkimage/mkimage.sh xiaomi-nabu
#
# 流程：apk 安装 rootfs → 叠加配置 → 裁剪 → 只读根（erofs，内核不支持时用 squashfs）→ initramfs
#       → 按启动后端（BOOT_BACKEND）打包：
#   uefi-sdboot  UKI（每个 slot 一个）→ ESP（systemd-boot）→ GPT 磁盘（esp | root_a | root_b | data）
#   android-abl  boot_a.img / boot_b.img（mkbootimg）→ super.img（逻辑分区 system_a | system_b）
#                → userdata.img，以及刷机脚本 flash.sh
# 全程不需要 loop 设备（FAT 用 mtools 写入，分区用 dd 写到偏移处）。
#
# 目标架构与开发 chroot 不同时（例如在 x86_64 上构建 aarch64），先在宿主机运行一次 tools/setup-binfmt.sh；
# loong-shell 用 tools/build-aarch64.sh 交叉编译。
#
# 环境变量：
#   LOONG_VERSION        系统版本（默认 0.1.0-dev），写入 /etc/os-release 与 UKI
#   UPDATE_BUNDLE=1      同时生成签名的 OTA 更新包 update-<版本>-<机型>.loongup（tools/mkupdate.sh）
#   LOONG_UPDATE_PUBKEY  镜像信任的 OTA 公钥（正式发布用；私钥不必在构建机上）
#   LOONG_UPDATE_KEY     开发用的签名私钥（默认 build/keys/loong-dev.pem，不存在时自动生成，不进 git）
#
# 产物：$SRC/build/image/<机型>/

set -eu

PROFILE=${1:?用法: mkimage.sh <机型>}
SRC=${SRC:-/src}
PROFILE_CONF="$SRC/system/profiles/$PROFILE.conf"
[ -f "$PROFILE_CONF" ] || { echo "没有这个机型: $PROFILE" >&2; exit 1; }

# 机型配置的默认值（各项含义见 system/profiles/*.conf）
BOOT_BACKEND=uefi-sdboot
ROOT_FS=erofs
SERIAL_TTY=ttyS0
EXTRA_REPOS=""
EXTRA_KEYS=""
DEVICE_SERVICES=""
ROOT_PART=root_
DATA_PART=data
SUPER_PART=""
DTB=""
APPEND_DTB=0
SESSION_ENV=""
# shellcheck disable=SC1090
. "$PROFILE_CONF"

OUT="$SRC/build/image/$PROFILE"
ROOT="$OUT/rootfs"
WORK="$OUT/work"
TOOLS="$WORK/tools"
VERSION=${LOONG_VERSION:-0.1.0-dev}
ALPINE_BRANCH=${ALPINE_BRANCH:-v3.24}
MIRROR=${MIRROR:-http://dl-cdn.alpinelinux.org/alpine}
HOST_ARCH=$(apk --print-arch)

log() { printf '\033[1;36m==> %s\033[0m\n' "$*"; }
die() { echo "mkimage: $*" >&2; exit 1; }

case "$BOOT_BACKEND" in
	uefi-sdboot|android-abl) ;;
	*) die "未知的启动后端: $BOOT_BACKEND" ;;
esac
case "$ROOT_FS" in
	erofs|squashfs) ;;
	*) die "不支持的根文件系统: $ROOT_FS" ;;
esac

# loong-shell：与开发 chroot 同架构时是普通的 cargo 构建，否则是 tools/build-aarch64.sh 的交叉编译产物
if [ "$ARCH" = "$HOST_ARCH" ]; then
	SHELL_BIN=${SHELL_BIN:-$SRC/build/target-alpine/release/loong-shell}
	SHELL_HINT="cargo build --release -p loong-shell -p loong-sysd -p loong-update"
else
	SHELL_BIN=${SHELL_BIN:-$SRC/build/target-alpine/$ARCH-alpine-linux-musl/release/loong-shell}
	SHELL_HINT="tools/build-$ARCH.sh"
fi
[ -x "$SHELL_BIN" ] || die "缺少 $SHELL_BIN，请先运行 $SHELL_HINT"
# loong-sysd、loong-update 与 loong-shell 由同一次（交叉）构建产出，放在同一目录
SYSD_BIN=${SYSD_BIN:-$(dirname "$SHELL_BIN")/loong-sysd}
UPDATE_BIN=${UPDATE_BIN:-$(dirname "$SHELL_BIN")/loong-update}
for b in "$SYSD_BIN" "$UPDATE_BIN"; do
	[ -x "$b" ] || die "缺少 $b，请先运行 $SHELL_HINT"
done

# OTA 签名公钥：镜像只信任它签署的更新包。没有指定时使用开发密钥（build/keys/，不进 git），
# 第一次构建时生成；之后同一台开发机构建的镜像与更新包都用这一对密钥。
KEY_DIR="$SRC/build/keys"
UPDATE_KEY=${LOONG_UPDATE_KEY:-$KEY_DIR/loong-dev.pem}
UPDATE_PUBKEY=${LOONG_UPDATE_PUBKEY:-${UPDATE_KEY%.pem}.pub}
if [ ! -f "$UPDATE_PUBKEY" ]; then
	[ -z "${LOONG_UPDATE_PUBKEY:-}${LOONG_UPDATE_KEY:-}" ] || die "找不到 OTA 公钥 $UPDATE_PUBKEY"
	log "生成开发用的 OTA 签名密钥 $UPDATE_KEY"
	mkdir -p "$KEY_DIR"
	chmod 700 "$KEY_DIR"
	(umask 077 && openssl genpkey -algorithm ed25519 -out "$UPDATE_KEY")
	openssl pkey -in "$UPDATE_KEY" -pubout -out "$UPDATE_PUBKEY"
fi

# ───────────────────────── 1. rootfs ─────────────────────────
PACKAGES="
	alpine-base busybox-openrc
	eudev udev-init-scripts udev-init-scripts-openrc
	seatd seatd-openrc dbus dbus-openrc
	kmod e2fsprogs e2fsprogs-extra dosfstools
	mesa-egl mesa-gles mesa-gbm libinput-libs libxkbcommon xkeyboard-config libseat
	fontconfig font-inter font-noto-cjk
	foot
	librime
	$KERNEL_PKG
	$DEVICE_PKGS
"

log "安装 rootfs（$PROFILE_NAME，$ARCH）"
rm -rf "$OUT"
mkdir -p "$ROOT/etc/apk/keys" "$WORK"
# 公钥：目标架构的 Alpine 公钥（开发 chroot 的 /etc/apk/keys 只有宿主架构的），本地软件包的公钥，
# 以及机型额外仓库的公钥（随仓库存放在 system/keys/）。
cp /usr/share/apk/keys/"$ARCH"/*.pub /etc/apk/keys/*.pub "$ROOT/etc/apk/keys/"
for k in $EXTRA_KEYS; do
	cp "$SRC/$k" "$ROOT/etc/apk/keys/"
done
printf '%s\n' "$MIRROR/$ALPINE_BRANCH/main" "$MIRROR/$ALPINE_BRANCH/community" > "$ROOT/etc/apk/repositories"
[ -z "$EXTRA_REPOS" ] || printf '%s\n' "$EXTRA_REPOS" >> "$ROOT/etc/apk/repositories"

# apk 3 在嵌套 chroot 里无法为安装脚本创建命名空间，所以分两步：
#   1) 不运行脚本，装一个最小的引导系统（busybox + apk）
#   2) chroot 进目标系统，由目标系统自己的 apk 安装其余软件包（此时脚本能正常运行）
# 跨架构构建时，第 2 步的程序经 qemu-user binfmt 运行。
apk add --root "$ROOT" --initdb --arch "$ARCH" --no-cache --no-progress -q --scripts=no \
	--repositories-file "$ROOT/etc/apk/repositories" alpine-baselayout busybox apk-tools alpine-keys
chroot "$ROOT" /bin/busybox true 2>/dev/null ||
	die "无法在 chroot 中运行 $ARCH 程序：请先在宿主机上运行 tools/setup-binfmt.sh"
chroot "$ROOT" /bin/busybox --install -s

cleanup_mounts() {
	for m in mnt/loong-packages dev proc; do
		umount "$ROOT/$m" 2>/dev/null || true
	done
}
trap cleanup_mounts EXIT
mount -t proc proc "$ROOT/proc"
mount --bind /dev "$ROOT/dev"
cp -L /etc/resolv.conf "$ROOT/etc/resolv.conf"

LOCAL_REPO="$SRC/build/packages"
extra_repo=""
if [ -d "$LOCAL_REPO/packages/$ARCH" ]; then
	mkdir -p "$ROOT/mnt/loong-packages"
	mount --bind "$LOCAL_REPO" "$ROOT/mnt/loong-packages"
	extra_repo="--repository /mnt/loong-packages/packages"
	[ -n "${WITH_WPE:-}" ] && PACKAGES="$PACKAGES wpewebkit wpewebkit-minibrowser"
	# 「墨韵」展示字体（霞鹜文楷屏幕版）：tools/build-pkg.sh font-lxgw-wenkai-screen
	ls "$LOCAL_REPO/packages/$ARCH"/font-lxgw-wenkai-screen-*.apk >/dev/null 2>&1 && \
		PACKAGES="$PACKAGES font-lxgw-wenkai-screen"
	# 拼音输入法的 Rime 数据（loong_pinyin，词典已预编译，约 8 MiB）：tools/build-pkg.sh loong-rime-data。
	# 没有它时屏幕键盘只能输入英文（librime 本身在上面的基础包里）。
	ls "$LOCAL_REPO/packages/$ARCH"/loong-rime-data-*.apk >/dev/null 2>&1 && \
		PACKAGES="$PACKAGES loong-rime-data"
fi
# 重新安装引导包以运行它们的脚本，再安装其余软件包。
chroot "$ROOT" /sbin/apk fix --no-cache --no-progress -q --reinstall alpine-baselayout busybox
# shellcheck disable=SC2086
chroot "$ROOT" /sbin/apk add --no-cache --no-progress -q $extra_repo $PACKAGES
cleanup_mounts
trap - EXIT
rm -f "$ROOT/etc/resolv.conf"

# ───────────────────────── 2. Loong OS 组件 ─────────────────────────
log "安装 Loong OS 组件"
install -Dm755 "$SHELL_BIN" "$ROOT/usr/bin/loong-shell"
install -Dm755 "$SYSD_BIN" "$ROOT/usr/bin/loong-sysd"
install -Dm755 "$UPDATE_BIN" "$ROOT/usr/bin/loong-update"
install -Dm644 "$UPDATE_PUBKEY" "$ROOT/usr/share/loong/update-keys/$(basename "${UPDATE_PUBKEY%.pub}").pub"
install -Dm644 "$SRC/system/profiles/$PROFILE.device.toml" "$ROOT/usr/share/loong/device.toml"
# Web 运行时（WPE WebKit 构建完成后可用：WITH_WPE=1）
if [ -n "${WITH_WPE:-}" ]; then
	WEBAPP_BIN="$SRC/build/webrt/loong-webapp"
	[ -x "$WEBAPP_BIN" ] || { echo "缺少 $WEBAPP_BIN，请先构建 webrt（见 README）" >&2; exit 1; }
	install -Dm755 "$WEBAPP_BIN" "$ROOT/usr/bin/loong-webapp"
	# /usr/share/loong/web 是 JS bridge 的受信任目录（只读根分区）：系统 Web 应用（apps/，例如设置）
	# 以 --bridge=system 启动时，起始页必须在这里面。见 docs/api/loong-js-bridge.md。
	mkdir -p "$ROOT/usr/share/loong/web"
	cp -a "$SRC/webrt/loongkit" "$SRC/webrt/demo" "$SRC/webrt/apps" "$ROOT/usr/share/loong/web/"
	# 系统浏览器：程序 + 界面（界面与 LoongKit 的相对位置同源码树：web/browser/ui ↔ web/loongkit）
	BROWSER_BIN="$SRC/build/webrt/browser/loong-browser"
	[ -x "$BROWSER_BIN" ] || { echo "缺少 $BROWSER_BIN，请先构建 webrt（见 README）" >&2; exit 1; }
	install -Dm755 "$BROWSER_BIN" "$ROOT/usr/bin/loong-browser"
	mkdir -p "$ROOT/usr/share/loong/web/browser"
	cp -a "$SRC/webrt/browser/ui" "$ROOT/usr/share/loong/web/browser/"
	[ -f "$SRC/system/profiles/$PROFILE.apps.toml" ] && \
		install -Dm644 "$SRC/system/profiles/$PROFILE.apps.toml" "$ROOT/usr/share/loong/apps.toml"
fi
cp -a "$SRC/system/rootfs/." "$ROOT/"
# 机型专属文件（音频 UCM 配置等），相当于 loong-device-<机型> 包的内容
[ -d "$SRC/system/devices/$PROFILE/rootfs" ] && cp -a "$SRC/system/devices/$PROFILE/rootfs/." "$ROOT/"
chmod 755 "$ROOT"/etc/init.d/loong-*
if [ -n "${SESSION_ENV:-}" ]; then
	printf 'LOONG_SESSION_ENV="%s"\n' "$SESSION_ENV" >> "$ROOT/etc/conf.d/loong-session"
fi
mkdir -p "$ROOT/etc/loong"
# loong-update 的机型配置（loong-boot-ok 也通过 loong-update 确认启动）。说明见 docs/api/loong-update.md
cat > "$ROOT/etc/loong/update.conf" <<EOF
# 由 mkimage.sh 生成
PROFILE=$PROFILE
ARCH=$ARCH
BOOT_BACKEND=$BOOT_BACKEND
ROOT_PART=$ROOT_PART
SUPER_PART=$SUPER_PART
ESP_PART=esp
BOOT_PART=boot_
BOOT_TRIES=3
EOF

cat > "$ROOT/etc/os-release" <<EOF
NAME="Loong OS"
PRETTY_NAME="Loong OS $VERSION"
ID=loong
ID_LIKE=alpine
VERSION_ID=$VERSION
IMAGE_VERSION=$VERSION
HOME_URL="https://github.com/wesloong/loong-os"
EOF
echo loong > "$ROOT/etc/hostname"

# 用户：loong（uid 1000），通过 seatd 访问显示与输入设备。直接写文件，跨架构构建也适用。
echo "loong:x:1000:1000:Loong:/home/loong:/bin/sh" >> "$ROOT/etc/passwd"
echo "loong:x:1000:" >> "$ROOT/etc/group"
echo "loong:!::0:::::" >> "$ROOT/etc/shadow"
for g in seat video input audio; do
	if grep -q "^$g:" "$ROOT/etc/group"; then
		sed -i -E "s/^($g:[^:]*:[^:]*:)(.*)$/\1\2,loong/; s/:,loong$/:loong/" "$ROOT/etc/group"
	else
		echo "$g:x:$(awk -F: 'BEGIN{m=900} $3>m && $3<1000{m=$3} END{print m+1}' "$ROOT/etc/group"):loong" >> "$ROOT/etc/group"
	fi
done
if [ "${DEV_IMAGE:-0}" = 1 ]; then
	# 开发镜像：root 免密，串口可登录。
	sed -i 's/^root:[^:]*:/root::/' "$ROOT/etc/shadow"
	grep -q "^$SERIAL_TTY:" "$ROOT/etc/inittab" ||
		echo "$SERIAL_TTY::respawn:/sbin/getty -L 115200 $SERIAL_TTY vt100" >> "$ROOT/etc/inittab"
fi
# 屏幕属于系统界面：不在虚拟终端上启动文字登录。
sed -i -E 's/^(tty[0-9]+::respawn)/#\1/' "$ROOT/etc/inittab"

# OpenRC 服务（直接建符号链接，等价于 rc-update add）
enable() { mkdir -p "$ROOT/etc/runlevels/$2"; ln -sf "/etc/init.d/$1" "$ROOT/etc/runlevels/$2/$1"; }
for s in devfs dmesg sysfs procfs udev udev-trigger udev-settle; do enable "$s" sysinit; done
for s in modules hostname bootmisc sysctl seedrng loong-growfs; do enable "$s" boot; done
for s in dbus seatd loong-session loong-boot-ok; do enable "$s" default; done
enable loong-sysd default
for s in killprocs savecache; do enable "$s" shutdown; done
# 机型服务，格式为“服务名:运行级别”
for e in $DEVICE_SERVICES; do
	[ -e "$ROOT/etc/init.d/${e%%:*}" ] || die "机型服务 ${e%%:*} 不存在（缺少对应的 -openrc 包？）"
	enable "${e%%:*}" "${e#*:}"
done

# 只读根：会被写入的文件改为指向可写位置
ln -sf /run/resolv.conf "$ROOT/etc/resolv.conf"
ln -sf /var/lib/dbus/machine-id "$ROOT/etc/machine-id"
cat > "$ROOT/etc/fstab" <<'EOF'
# 根文件系统（erofs，只读）与 /data 由 initramfs 挂载。
EOF

# udev 硬件数据库（构建时生成，运行时根目录只读）
chroot "$ROOT" /bin/udevadm hwdb --update 2>/dev/null || chroot "$ROOT" /sbin/udevadm hwdb --update || true

# ───────────────────────── 3. 裁剪 ─────────────────────────
log "裁剪 rootfs"
KVER=$(ls "$ROOT/lib/modules" | head -1)
cp "$ROOT$KERNEL_IMAGE" "$WORK/vmlinuz"
if [ -n "$DTB" ]; then
	# 内核包把 dtb 装在 /boot/dtbs/ 或 /usr/share/dtb/ 下（随发行版和版本而异）
	dtb_path=$(find "$ROOT/boot" "$ROOT/usr/share" -path "*/$DTB.dtb" 2>/dev/null | head -1)
	[ -n "$dtb_path" ] || die "内核包里找不到 $DTB.dtb"
	cp "$dtb_path" "$WORK/dtb"
fi
rm -rf "$ROOT/boot"/* "$ROOT/usr/share/man" "$ROOT/usr/share/doc" "$ROOT/usr/share/info" \
	"$ROOT/usr/share/fonts/noto/NotoSerifCJK-"* "$ROOT/var/cache/apk"/*
# Inter 只保留可变字体
find "$ROOT/usr/share/fonts/inter" -type f ! -name 'Inter*Variable*' -delete 2>/dev/null || true

# /var 的初始内容：首次启动时由 initramfs 复制到 data 分区
mkdir -p "$ROOT/usr/share/loong"
mv "$ROOT/var" "$ROOT/usr/share/loong/var-skel"
mkdir -p "$ROOT/var" "$ROOT/data" "$ROOT/home" "$ROOT/root" "$ROOT/tmp"

# ───────────────────────── 4. 只读根 ─────────────────────────
log "生成 $ROOT_FS 根文件系统"
ROOTIMG="$WORK/root.$ROOT_FS"
case "$ROOT_FS" in
	erofs) mkfs.erofs -zlz4hc,12 -T0 "$ROOTIMG" "$ROOT" >/dev/null ;;
	# 内核没有 erofs 时的替代（例如 pmOS 的 sm8150 内核）：同样只读、lz4hc 压缩，时间戳固定以便复现
	squashfs) mksquashfs "$ROOT" "$ROOTIMG" -noappend -quiet -no-progress -comp lz4 -Xhc \
		-mkfs-time 0 -all-time 0 >/dev/null ;;
esac
root_bytes=$(stat -c %s "$ROOTIMG")
# 根分区（slot）留 50% 余量给后续 OTA 更新，按 MiB 对齐
ROOT_MB=$(( (root_bytes * 3 / 2) / 1048576 + 16 ))

# ───────────────────────── 5. initramfs ─────────────────────────
# initramfs 与启动程序要用目标架构的文件：装进一个临时目录（不运行安装脚本）再取出。
log "准备目标架构的启动文件"
TOOLS_PKGS="busybox-static"
[ "$BOOT_BACKEND" = uefi-sdboot ] && TOOLS_PKGS="$TOOLS_PKGS systemd-boot systemd-efistub"
[ -n "$SUPER_PART" ] && TOOLS_PKGS="$TOOLS_PKGS device-mapper-static"
# shellcheck disable=SC2086
apk add --root "$TOOLS" --initdb --arch "$ARCH" --no-cache --no-progress -q --scripts=no \
	--keys-dir "$ROOT/etc/apk/keys" --repositories-file "$ROOT/etc/apk/repositories" $TOOLS_PKGS

log "生成 initramfs（内核 $KVER）"
IRD="$WORK/initramfs"
mkdir -p "$IRD/bin" "$IRD/etc/loong" "$IRD/lib/modules/$KVER"
cp "$TOOLS/bin/busybox.static" "$IRD/bin/busybox"
install -m755 "$SRC/system/initramfs/init" "$IRD/init"
cat > "$IRD/etc/loong/initramfs.conf" <<EOF
ROOT_PART=$ROOT_PART
ROOT_FS=$ROOT_FS
DATA_PART=$DATA_PART
SUPER_PART=$SUPER_PART
EOF
if [ -n "$SUPER_PART" ]; then
	install -m755 "$SRC/system/initramfs/dynpart" "$IRD/bin/loong-dynpart"
	cp "$TOOLS/usr/sbin/dmsetup.static" "$IRD/bin/dmsetup"
fi
echo "$INITRAMFS_MODULES" | tr ' ' '\n' > "$IRD/etc/loong/initramfs-modules"
# 复制所需模块及其依赖（解压为 .ko，busybox modprobe 可直接加载）
MODDIR="$ROOT/lib/modules/$KVER"
need_mods=""
add_mod() {
	# 内核里根本没有的模块（既不是内建也不是 .ko）直接报错，免得做出一个启动不了的镜像
	path=$(modinfo -b "$ROOT" -k "$KVER" -F filename "$1" 2>/dev/null | head -1) ||
		die "内核 $KVER 不支持 $1（既不是内建，也没有模块）"
	case "$path" in ""|"(builtin)"|*builtin*) return 0 ;; esac
	case " $need_mods " in *" $path "*) return 0 ;; esac
	need_mods="$need_mods $path"
	for dep in $(modinfo -b "$ROOT" -k "$KVER" -F depends "$1" 2>/dev/null | tr ',' ' '); do
		add_mod "$dep"
	done
}
for m in $INITRAMFS_MODULES; do add_mod "$m"; done
for path in $need_mods; do
	rel=${path#"$MODDIR/"}
	mkdir -p "$IRD/lib/modules/$KVER/$(dirname "$rel")"
	case "$path" in
		*.gz) gzip -dc "$path" > "$IRD/lib/modules/$KVER/${rel%.gz}" ;;
		*.xz) xz -dc "$path" > "$IRD/lib/modules/$KVER/${rel%.xz}" ;;
		*.zst) zstd -qdc "$path" > "$IRD/lib/modules/$KVER/${rel%.zst}" ;;
		*) cp "$path" "$IRD/lib/modules/$KVER/$rel" ;;
	esac
done
cp "$MODDIR"/modules.builtin* "$MODDIR/modules.order" "$IRD/lib/modules/$KVER/" 2>/dev/null || true
depmod -b "$IRD" "$KVER"
(cd "$IRD" && find . | cpio -o -H newc --quiet | gzip -9) > "$WORK/initramfs.img"

# ───────────────────────── 6. data ─────────────────────────
log "生成 data 分区"
truncate -s "${DATA_MB}M" "$WORK/data.img"
# 块大小固定 4096：UFS 等存储的逻辑扇区是 4096 字节，1024 字节块的 ext4 在上面无法挂载
mke2fs -q -t ext4 -b 4096 -L data -F "$WORK/data.img"

# ───────────────────────── OTA 更新包的载荷 ─────────────────────────
# 把更新包要用的文件（硬链接，不占额外空间）和元数据放进 $OUT/update/，由 tools/mkupdate.sh 打包签名。
# 参数：种类[@slot]=文件（同 loong-update pack）
stage_update() {
	mkdir -p "$OUT/update"
	items=""
	for spec in "$@"; do
		what=${spec%%=*}
		name=$(echo "$what" | tr '@' '-')
		ln -f "${spec#*=}" "$OUT/update/$name"
		items="$items $what=$name"
	done
	cat > "$OUT/update/bundle.env" <<-EOF
		VERSION=$VERSION
		PROFILE=$PROFILE
		ARCH=$ARCH
		BOOT_BACKEND=$BOOT_BACKEND
		ROOT_FS=$ROOT_FS
		ITEMS="$items"
	EOF
}

# ───────────────────────── 7a. uefi-sdboot ─────────────────────────
build_uefi_sdboot() {
	IMG="$OUT/loong-$PROFILE.img"
	EFI_DIR="$TOOLS/usr/lib/systemd/boot/efi"

	log "生成 UKI"
	# systemd-boot 优先启动版本号更高的 UKI；用尽启动次数的 UKI 会被排到最后（自动回退）。
	# 初始镜像中 slot A 是当前系统，slot B 作为“旧版本”备份（IMAGE_VERSION=0）。
	# OTA 时 loong-update 把新版本写入另一个 slot，新 UKI 版本更高，排在前面。
	build_uki() { # <slot> <IMAGE_VERSION> <输出>
		sed "s/^IMAGE_VERSION=.*/IMAGE_VERSION=$2/" "$ROOT/etc/os-release" > "$WORK/os-release-$1"
		ukify build \
			--linux "$WORK/vmlinuz" \
			--initrd "$WORK/initramfs.img" \
			--cmdline "$CMDLINE loong.slot=$1" \
			--os-release "@$WORK/os-release-$1" \
			--stub "$EFI_DIR/linux$EFI_ARCH.efi.stub" \
			--efi-arch "$EFI_ARCH" \
			--output "$3" >/dev/null
	}
	build_uki a "$VERSION" "$WORK/loong-a.efi"
	build_uki b 0 "$WORK/loong-b.efi"
	if [ "${UPDATE_BUNDLE:-0}" = 1 ]; then
		# 更新包里两个 slot 的 UKI 都带真实版本
		build_uki b "$VERSION" "$WORK/update-loong-b.efi"
		stage_update rootfs="$ROOTIMG" uki@a="$WORK/loong-a.efi" uki@b="$WORK/update-loong-b.efi"
	fi

	log "生成 ESP（systemd-boot）"
	ESP="$WORK/esp.img"
	mkfs.vfat -C -n ESP "$ESP" $((ESP_MB * 1024)) >/dev/null
	EFI_UPPER=$(echo "$EFI_ARCH" | tr a-z A-Z)
	mmd -i "$ESP" ::/EFI ::/EFI/BOOT ::/EFI/systemd ::/EFI/Linux ::/loader
	mcopy -i "$ESP" "$EFI_DIR/systemd-boot$EFI_ARCH.efi" "::/EFI/BOOT/BOOT$EFI_UPPER.EFI"
	mcopy -i "$ESP" "$EFI_DIR/systemd-boot$EFI_ARCH.efi" "::/EFI/systemd/systemd-boot$EFI_ARCH.efi"
	# slot A 是当前系统，带 3 次启动计数；成功启动后由 loong-boot-ok 去掉计数。
	mcopy -i "$ESP" "$WORK/loong-a.efi" "::/EFI/Linux/loong-a+3.efi"
	mcopy -i "$ESP" "$WORK/loong-b.efi" "::/EFI/Linux/loong-b.efi"
	printf 'timeout 0\ndefault loong-*\nconsole-mode keep\neditor no\n' > "$WORK/loader.conf"
	mcopy -i "$ESP" "$WORK/loader.conf" ::/loader/loader.conf

	log "组装磁盘镜像"
	TOTAL_MB=$(( 1 + ESP_MB + ROOT_MB * 2 + DATA_MB + 1 ))
	rm -f "$IMG"
	truncate -s "${TOTAL_MB}M" "$IMG"
	sfdisk -q "$IMG" <<-EOF
		label: gpt
		size=${ESP_MB}MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="esp"
		size=${ROOT_MB}MiB, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="${ROOT_PART}a"
		size=${ROOT_MB}MiB, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="${ROOT_PART}b"
		size=${DATA_MB}MiB, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="$DATA_PART"
	EOF
	# 分区按 1MiB 对齐（sfdisk 默认），起始扇区换算成 MiB 偏移写入。
	put() { dd if="$2" of="$IMG" bs=1M seek=$(($1 / 2048)) conv=notrunc status=none; }
	# shellcheck disable=SC2046
	set -- $(sfdisk -d "$IMG" | sed -nE 's/.*start= *([0-9]+).*/\1/p')
	put "$1" "$ESP"
	put "$2" "$ROOTIMG"
	put "$3" "$ROOTIMG"
	put "$4" "$WORK/data.img"

	log "完成：$IMG"
	printf '  rootfs (%s): %s MiB\n  initramfs:      %s KiB\n  UKI:            %s MiB\n  磁盘镜像:       %s MiB\n' \
		"$ROOT_FS" $((root_bytes / 1048576)) $(($(stat -c %s "$WORK/initramfs.img") / 1024)) \
		$(($(stat -c %s "$WORK/loong-a.efi") / 1048576)) "$TOTAL_MB"
}

# ───────────────────────── 7b. android-abl ─────────────────────────
# Android 引导程序（如高通 ABL）从 boot_a / boot_b 加载 boot.img；根文件系统放在 super 分区的
# 逻辑分区 system_a / system_b 里（由 initramfs 的 loong-dynpart 映射），/data 是 userdata 分区。
build_android_abl() {
	kernel="$WORK/vmlinuz"
	if [ "$APPEND_DTB" = 1 ]; then
		cat "$WORK/vmlinuz" "$WORK/dtb" > "$WORK/vmlinuz-dtb"
		kernel="$WORK/vmlinuz-dtb"
	fi

	log "生成 boot_a.img / boot_b.img"
	for slot in a b; do
		mkbootimg \
			--header_version "$BOOTIMG_HEADER_VERSION" \
			--kernel "$kernel" \
			--ramdisk "$WORK/initramfs.img" \
			--base "$BOOTIMG_BASE" \
			--kernel_offset "$BOOTIMG_KERNEL_OFFSET" \
			--ramdisk_offset "$BOOTIMG_RAMDISK_OFFSET" \
			--second_offset "$BOOTIMG_SECOND_OFFSET" \
			--tags_offset "$BOOTIMG_TAGS_OFFSET" \
			--pagesize "$BOOTIMG_PAGESIZE" \
			--cmdline "$CMDLINE loong.slot=$slot" \
			--output "$OUT/boot_$slot.img"
		[ "$(stat -c %s "$OUT/boot_$slot.img")" -le "$BOOTIMG_MAX_BYTES" ] || die "boot_$slot.img 超过 boot 分区大小"
	done
	# 自检：解包核对内核、initramfs 与 cmdline（boot.img v0 的 cmdline 最长 512 字节，超长会被截断）
	unpack_bootimg --boot_img "$OUT/boot_b.img" --out "$WORK/unpacked" > "$WORK/unpacked.txt"
	cmp -s "$WORK/unpacked/kernel" "$kernel" || die "boot.img 自检失败：内核不一致"
	cmp -s "$WORK/unpacked/ramdisk" "$WORK/initramfs.img" || die "boot.img 自检失败：initramfs 不一致"
	grep -q "^command line args: $CMDLINE loong.slot=b\$" "$WORK/unpacked.txt" || die "boot.img 自检失败：cmdline 不一致"
	rm -rf "$WORK/unpacked" "$WORK/unpacked.txt"
	if [ "${UPDATE_BUNDLE:-0}" = 1 ]; then
		stage_update rootfs="$ROOTIMG" boot@a="$OUT/boot_a.img" boot@b="$OUT/boot_b.img"
	fi

	log "生成 super.img（${ROOT_PART}a | ${ROOT_PART}b）"
	cp "$ROOTIMG" "$OUT/system.img"
	slot_bytes=$((ROOT_MB * 1048576))
	[ $((slot_bytes * 2 + 16 * 1048576)) -le "$SUPER_SIZE" ] || die "super 分区放不下两个 ${ROOT_MB} MiB 的 slot"
	# 两个 slot 写入同一个系统；元数据 2 个槽，对应 A/B。
	lpmake --metadata-size 65536 --metadata-slots 2 --super-name "$SUPER_PART" \
		--device "$SUPER_PART:$SUPER_SIZE" --group "loong:$((slot_bytes * 2))" \
		--partition "${ROOT_PART}a:none:$slot_bytes:loong" --image "${ROOT_PART}a=$ROOTIMG" \
		--partition "${ROOT_PART}b:none:$slot_bytes:loong" --image "${ROOT_PART}b=$ROOTIMG" \
		--sparse --output "$OUT/super.img" 2> "$WORK/lpmake.log" || { cat "$WORK/lpmake.log" >&2; die "lpmake 失败"; }

	log "生成 $DATA_PART.img"
	img2simg "$WORK/data.img" "$OUT/$DATA_PART.img"

	# 万一 ABL 因 AVB 校验拒绝启动，可刷入这个关闭校验的 vbmeta（见 docs/devices/）
	avbtool make_vbmeta_image --flags 2 --padding_size 4096 --output "$OUT/vbmeta-disabled.img"

	cat > "$OUT/flash.sh" <<-EOF
		#!/bin/sh
		# 用 fastboot 把 Loong OS 刷入 $PROFILE_NAME。会覆盖 super（Android 系统）与 userdata（全部用户数据）！
		# 用法：平板进入 fastboot 模式并连接电脑，然后在本目录运行 sh flash.sh
		set -eu
		cd "\$(dirname "\$0")"
		product=\$(fastboot getvar product 2>&1 | sed -n 's/^product: *//p')
		[ "\$product" = "$FASTBOOT_PRODUCT" ] || { echo "连接的设备是 '\$product'，不是 $FASTBOOT_PRODUCT"; exit 1; }
		size=\$(fastboot getvar partition-size:$SUPER_PART 2>&1 | sed -n 's/^partition-size:$SUPER_PART: *//p')
		[ "\$((size))" -eq $SUPER_SIZE ] || { echo "$SUPER_PART 分区大小是 '\$size'，镜像按 $SUPER_SIZE 字节生成，不刷"; exit 1; }
		fastboot erase dtbo_a
		fastboot erase dtbo_b
		fastboot flash boot_a boot_a.img
		fastboot flash boot_b boot_b.img
		fastboot flash $SUPER_PART super.img
		fastboot flash $DATA_PART $DATA_PART.img
		fastboot --set-active=a
		fastboot reboot
	EOF
	chmod 755 "$OUT/flash.sh"

	log "完成：$OUT"
	printf '  rootfs (%s): %s MiB（每个 slot %s MiB）\n  initramfs:      %s KiB\n  boot.img:       %s MiB\n  super.img:      %s MiB（sparse）\n' \
		"$ROOT_FS" $((root_bytes / 1048576)) "$ROOT_MB" $(($(stat -c %s "$WORK/initramfs.img") / 1024)) \
		$(($(stat -c %s "$OUT/boot_a.img") / 1048576)) $(($(stat -c %s "$OUT/super.img") / 1048576))
}

case "$BOOT_BACKEND" in
	uefi-sdboot) build_uefi_sdboot ;;
	android-abl) build_android_abl ;;
esac

if [ "${UPDATE_BUNDLE:-0}" = 1 ]; then
	log "生成 OTA 更新包"
	LOONG_UPDATE_KEY="$UPDATE_KEY" "$SRC/tools/mkupdate.sh" "$PROFILE"
fi
