#!/bin/sh
# 把 mkimage.sh 准备好的载荷打包成签名的 OTA 更新包（在开发 chroot 内运行）。
#
#   UPDATE_BUNDLE=1 LOONG_VERSION=0.1.1 tools/dev-chroot.sh run system/mkimage/mkimage.sh qemu-x86_64
#       构建镜像时顺便生成更新包（mkimage 最后会调用本脚本）
#   tools/dev-chroot.sh run tools/mkupdate.sh qemu-x86_64
#       用同一批载荷重新打包（例如换签名密钥、改 min_version）
#
# 载荷与元数据在 build/image/<机型>/update/（UPDATE_BUNDLE=1 时由 mkimage.sh 生成）。
# 产物：build/image/<机型>/update-<版本>-<机型>.loongup
#
# 环境变量：
#   LOONG_UPDATE_KEY          签名私钥（默认 build/keys/loong-dev.pem，即开发镜像信任的那把）
#   LOONG_UPDATE_MIN_VERSION  设备至少要运行这个版本才能安装（默认不限制）
#   LOONG_UPDATE              loong-update 程序（默认用宿主架构的 release 构建，没有就编译；
#                             更新包格式与架构无关，aarch64 的包也用宿主机的程序打）

set -eu

PROFILE=${1:?用法: mkupdate.sh <机型>}
SRC=${SRC:-$(cd "$(dirname "$0")/.." && pwd)}
OUT="$SRC/build/image/$PROFILE"
STAGE="$OUT/update"

die() { echo "mkupdate: $*" >&2; exit 1; }

[ -f "$STAGE/bundle.env" ] || die "没有 $STAGE/bundle.env：请用 UPDATE_BUNDLE=1 运行 mkimage.sh $PROFILE"
# shellcheck disable=SC1091
. "$STAGE/bundle.env"

KEY=${LOONG_UPDATE_KEY:-$SRC/build/keys/loong-dev.pem}
[ -f "$KEY" ] || die "没有签名私钥 $KEY"
TOOL=${LOONG_UPDATE:-${CARGO_TARGET_DIR:-$SRC/build/target-alpine}/release/loong-update}
if [ ! -x "$TOOL" ]; then
	echo ">>> 编译宿主机的 loong-update"
	(cd "$SRC" && cargo build --release -j"${JOBS:-2}" -p loong-update)
fi

BUNDLE="$OUT/update-$VERSION-$PROFILE.loongup"
set --
for item in $ITEMS; do
	set -- "$@" "${item%%=*}=$STAGE/${item#*=}"
done
"$TOOL" pack --key "$KEY" --version "$VERSION" --profile "$PROFILE" --arch "$ARCH" \
	--backend "$BOOT_BACKEND" --root-fs "$ROOT_FS" \
	${LOONG_UPDATE_MIN_VERSION:+--min-version "$LOONG_UPDATE_MIN_VERSION"} \
	-o "$BUNDLE" "$@"
# 打包后再完整校验一遍（验签 + 所有载荷的哈希）
PUB="${KEY%.pem}.pub"
[ ! -f "$PUB" ] || "$TOOL" verify --key "$PUB" "$BUNDLE" > /dev/null
echo "更新包: $BUNDLE（$(($(stat -c %s "$BUNDLE") / 1048576)) MiB）"
