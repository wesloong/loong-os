#!/bin/sh
# Loong OS 镜像构建器。在开发 chroot 内运行：
#
#   tools/dev-chroot.sh run /src/system/mkimage/mkimage.sh qemu-x86_64
#
# 流程：apk 安装 rootfs → 叠加配置 → 裁剪 → erofs（只读、压缩）→ initramfs
#       → UKI（每个 slot 一个）→ ESP（systemd-boot）→ GPT 磁盘（esp | root_a | root_b | data）
# 全程不需要 loop 设备（FAT 用 mtools 写入，分区用 dd 写到偏移处）。
#
# 产物：/src/build/image/<机型>/loong-<机型>.img

set -eu

PROFILE=${1:?用法: mkimage.sh <机型>}
SRC=/src
PROFILE_CONF="$SRC/system/profiles/$PROFILE.conf"
[ -f "$PROFILE_CONF" ] || { echo "没有这个机型: $PROFILE" >&2; exit 1; }
# shellcheck disable=SC1090
. "$PROFILE_CONF"

OUT="$SRC/build/image/$PROFILE"
ROOT="$OUT/rootfs"
WORK="$OUT/work"
IMG="$OUT/loong-$PROFILE.img"
VERSION=${LOONG_VERSION:-0.1.0-dev}
ALPINE_BRANCH=${ALPINE_BRANCH:-v3.24}
MIRROR=${MIRROR:-http://dl-cdn.alpinelinux.org/alpine}

log() { printf '\033[1;36m==> %s\033[0m\n' "$*"; }

# ───────────────────────── 1. rootfs ─────────────────────────
PACKAGES="
	alpine-base busybox-openrc
	eudev udev-init-scripts udev-init-scripts-openrc
	seatd seatd-openrc dbus dbus-openrc
	kmod e2fsprogs dosfstools
	mesa-egl mesa-gles mesa-gbm libinput-libs libxkbcommon xkeyboard-config libseat
	fontconfig font-inter font-noto-cjk
	foot
	$KERNEL_PKG
	$DEVICE_PKGS
"

log "安装 rootfs（$PROFILE_NAME）"
rm -rf "$OUT"
mkdir -p "$ROOT/etc/apk" "$WORK"
cp -a /etc/apk/keys "$ROOT/etc/apk/"
printf '%s\n' "$MIRROR/$ALPINE_BRANCH/main" "$MIRROR/$ALPINE_BRANCH/community" > "$ROOT/etc/apk/repositories"

# apk 3 在嵌套 chroot 里无法为安装脚本创建命名空间，所以分两步：
#   1) 不运行脚本，装一个最小的引导系统（busybox + apk）
#   2) chroot 进目标系统，由目标系统自己的 apk 安装其余软件包（此时脚本能正常运行）
# 跨架构构建（如在 x86_64 上构建 aarch64）需要 qemu-user binfmt。
apk add --root "$ROOT" --initdb --arch "$ARCH" --no-cache --no-progress -q --scripts=no \
	--repositories-file "$ROOT/etc/apk/repositories" alpine-baselayout busybox apk-tools alpine-keys
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
SHELL_BIN="$SRC/build/target-alpine/release/loong-shell"
[ -x "$SHELL_BIN" ] || { echo "缺少 $SHELL_BIN，请先运行 cargo build --release -p loong-shell" >&2; exit 1; }
install -Dm755 "$SHELL_BIN" "$ROOT/usr/bin/loong-shell"
install -Dm644 "$SRC/system/profiles/$PROFILE.device.toml" "$ROOT/usr/share/loong/device.toml"
# Web 运行时（WPE WebKit 构建完成后可用：WITH_WPE=1）
if [ -n "${WITH_WPE:-}" ]; then
	WEBAPP_BIN="$SRC/build/webrt/loong-webapp"
	[ -x "$WEBAPP_BIN" ] || { echo "缺少 $WEBAPP_BIN，请先构建 webrt（见 README）" >&2; exit 1; }
	install -Dm755 "$WEBAPP_BIN" "$ROOT/usr/bin/loong-webapp"
	mkdir -p "$ROOT/usr/share/loong/web"
	cp -a "$SRC/webrt/loongkit" "$SRC/webrt/demo" "$ROOT/usr/share/loong/web/"
	[ -f "$SRC/system/profiles/$PROFILE.apps.toml" ] && \
		install -Dm644 "$SRC/system/profiles/$PROFILE.apps.toml" "$ROOT/usr/share/loong/apps.toml"
fi
cp -a "$SRC/system/rootfs/." "$ROOT/"
chmod 755 "$ROOT/etc/init.d/loong-session" "$ROOT/etc/init.d/loong-boot-ok"
if [ -n "${SESSION_ENV:-}" ]; then
	printf 'LOONG_SESSION_ENV="%s"\n' "$SESSION_ENV" >> "$ROOT/etc/conf.d/loong-session"
fi

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
	grep -q '^ttyS0' "$ROOT/etc/inittab" || echo 'ttyS0::respawn:/sbin/getty -L 115200 ttyS0 vt100' >> "$ROOT/etc/inittab"
fi
# 屏幕属于系统界面：不在虚拟终端上启动文字登录。
sed -i -E 's/^(tty[0-9]+::respawn)/#\1/' "$ROOT/etc/inittab"

# OpenRC 服务（直接建符号链接，等价于 rc-update add）
enable() { mkdir -p "$ROOT/etc/runlevels/$2"; ln -sf "/etc/init.d/$1" "$ROOT/etc/runlevels/$2/$1"; }
for s in devfs dmesg sysfs procfs udev udev-trigger udev-settle; do enable "$s" sysinit; done
for s in modules hostname bootmisc sysctl seedrng; do enable "$s" boot; done
for s in dbus seatd loong-session loong-boot-ok; do enable "$s" default; done
for s in killprocs savecache; do enable "$s" shutdown; done

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
rm -rf "$ROOT/boot"/* "$ROOT/usr/share/man" "$ROOT/usr/share/doc" "$ROOT/usr/share/info" \
	"$ROOT/usr/share/fonts/noto/NotoSerifCJK-"* "$ROOT/var/cache/apk"/*
# Inter 只保留可变字体
find "$ROOT/usr/share/fonts/inter" -type f ! -name 'Inter*Variable*' -delete 2>/dev/null || true

# /var 的初始内容：首次启动时由 initramfs 复制到 data 分区
mkdir -p "$ROOT/usr/share/loong"
mv "$ROOT/var" "$ROOT/usr/share/loong/var-skel"
mkdir -p "$ROOT/var" "$ROOT/data" "$ROOT/home" "$ROOT/root" "$ROOT/tmp"

# ───────────────────────── 4. erofs ─────────────────────────
log "生成 erofs 根文件系统"
mkfs.erofs -zlz4hc,12 -T0 "$WORK/root.erofs" "$ROOT" >/dev/null

# ───────────────────────── 5. initramfs ─────────────────────────
log "生成 initramfs（内核 $KVER）"
IRD="$WORK/initramfs"
mkdir -p "$IRD/bin" "$IRD/etc/loong" "$IRD/lib/modules/$KVER"
cp /bin/busybox.static "$IRD/bin/busybox"
install -m755 "$SRC/system/initramfs/init" "$IRD/init"
echo "$INITRAMFS_MODULES" | tr ' ' '\n' > "$IRD/etc/loong/initramfs-modules"
# 复制所需模块及其依赖（解压为 .ko，busybox modprobe 可直接加载）
MODDIR="$ROOT/lib/modules/$KVER"
need_mods=""
add_mod() {
	path=$(modinfo -b "$ROOT" -k "$KVER" -F filename "$1" 2>/dev/null | head -1) || return 0
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

# ───────────────────────── 6. UKI ─────────────────────────
log "生成 UKI"
# systemd-boot 优先启动版本号更高的 UKI；用尽启动次数的 UKI 会被排到最后（自动回退）。
# 初始镜像中 slot A 是当前系统，slot B 作为“旧版本”备份。OTA 时把新版本写入另一个 slot 即可。
for slot in a b; do
	if [ "$slot" = a ]; then slot_version=$VERSION; else slot_version=0; fi
	sed "s/^IMAGE_VERSION=.*/IMAGE_VERSION=$slot_version/" "$ROOT/etc/os-release" > "$WORK/os-release-$slot"
	ukify build \
		--linux "$WORK/vmlinuz" \
		--initrd "$WORK/initramfs.img" \
		--cmdline "$CMDLINE loong.slot=$slot" \
		--os-release "@$WORK/os-release-$slot" \
		--stub "/usr/lib/systemd/boot/efi/linux$EFI_ARCH.efi.stub" \
		--output "$WORK/loong-$slot.efi" >/dev/null
done

# ───────────────────────── 7. ESP ─────────────────────────
log "生成 ESP（systemd-boot）"
ESP="$WORK/esp.img"
mkfs.vfat -C -n ESP "$ESP" $((ESP_MB * 1024)) >/dev/null
EFI_UPPER=$(echo "$EFI_ARCH" | tr a-z A-Z)
mmd -i "$ESP" ::/EFI ::/EFI/BOOT ::/EFI/systemd ::/EFI/Linux ::/loader
mcopy -i "$ESP" "/usr/lib/systemd/boot/efi/systemd-boot$EFI_ARCH.efi" "::/EFI/BOOT/BOOT$EFI_UPPER.EFI"
mcopy -i "$ESP" "/usr/lib/systemd/boot/efi/systemd-boot$EFI_ARCH.efi" "::/EFI/systemd/systemd-boot$EFI_ARCH.efi"
# slot A 是当前系统，带 3 次启动计数；成功启动后由 loong-boot-ok 去掉计数。
mcopy -i "$ESP" "$WORK/loong-a.efi" "::/EFI/Linux/loong-a+3.efi"
mcopy -i "$ESP" "$WORK/loong-b.efi" "::/EFI/Linux/loong-b.efi"
printf 'timeout 0\ndefault loong-*\nconsole-mode keep\neditor no\n' > "$WORK/loader.conf"
mcopy -i "$ESP" "$WORK/loader.conf" ::/loader/loader.conf

# ───────────────────────── 8. data ─────────────────────────
log "生成 data 分区"
truncate -s "${DATA_MB}M" "$WORK/data.img"
mke2fs -q -t ext4 -L data -F "$WORK/data.img"

# ───────────────────────── 9. GPT 磁盘 ─────────────────────────
log "组装磁盘镜像"
root_bytes=$(stat -c %s "$WORK/root.erofs")
# root 分区留 50% 余量给后续 OTA 更新，按 MiB 对齐
ROOT_MB=$(( (root_bytes * 3 / 2) / 1048576 + 16 ))
TOTAL_MB=$(( 1 + ESP_MB + ROOT_MB * 2 + DATA_MB + 1 ))
rm -f "$IMG"
truncate -s "${TOTAL_MB}M" "$IMG"
sfdisk -q "$IMG" <<EOF
label: gpt
size=${ESP_MB}MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="esp"
size=${ROOT_MB}MiB, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="root_a"
size=${ROOT_MB}MiB, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="root_b"
size=${DATA_MB}MiB, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="data"
EOF
# 分区按 1MiB 对齐（sfdisk 默认），起始扇区换算成 MiB 偏移写入。
put() { dd if="$2" of="$IMG" bs=1M seek=$(($1 / 2048)) conv=notrunc status=none; }
starts=$(sfdisk -d "$IMG" | sed -nE 's/.*start= *([0-9]+).*/\1/p')
set -- $starts
put "$1" "$ESP"
put "$2" "$WORK/root.erofs"
put "$3" "$WORK/root.erofs"
put "$4" "$WORK/data.img"

log "完成：$IMG"
printf '  rootfs (erofs): %s MiB\n  initramfs:      %s KiB\n  UKI:            %s MiB\n  磁盘镜像:       %s MiB\n' \
	$((root_bytes / 1048576)) $(($(stat -c %s "$WORK/initramfs.img") / 1024)) \
	$(($(stat -c %s "$WORK/loong-a.efi") / 1048576)) "$TOTAL_MB"
