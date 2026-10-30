#!/bin/sh
# 在 x86_64 开发 chroot 里交叉编译 Loong OS 的 Rust 组件（目标 aarch64-alpine-linux-musl）。
#
#   tools/dev-chroot.sh run tools/build-aarch64.sh                  构建 loong-shell（release）
#   tools/dev-chroot.sh run tools/build-aarch64.sh test -p loong-motion
#                                                                   其他 cargo 子命令；测试经 qemu-user 运行
#   tools/dev-chroot.sh run tools/build-aarch64.sh sysroot          只准备 C sysroot 和 Rust 标准库
#
# 产物：$CARGO_TARGET_DIR/aarch64-alpine-linux-musl/release/loong-shell
#      （即 build/target-alpine/aarch64-alpine-linux-musl/release/loong-shell，mkimage.sh 会从这里取）
#
# 做法：
#   1. C sysroot：apk --root build/sysroot-aarch64 --arch aarch64 安装 musl-dev、gcc（crt*.o、libgcc、
#      libgcc_s）以及 loong-shell 要链接的库。不运行安装脚本，sysroot 只用来编译和链接。
#   2. Rust 标准库：Alpine 只提供宿主架构的标准库；rustup 的预编译标准库与 Alpine 的 rustc 版本串
#      不同，不能混用。所以用 rust-src 包现场编译一份（cargo -Zbuild-std，RUSTC_BOOTSTRAP=1 允许稳定版
#      cargo 用这个功能），放进 build/rust-sysroot-aarch64，之后的构建用 --sysroot 指向它。
#      不在每次构建时直接用 -Zbuild-std：那样 build.rs 里的 rustc 探测（io-lifetimes、rustix 等
#      用来判断编译器特性）找不到目标标准库，会误判而编译失败。
#      标准库只在 rustc 版本变化时重新编译（约几分钟）。
#   3. 链接：宿主机的 clang + lld（--target=aarch64-alpine-linux-musl --sysroot=<C sysroot>）。
#   4. C 依赖：pkg-config 指向 C sysroot。
#   5. 运行测试：qemu-aarch64 -L <C sysroot>（不依赖 binfmt）。
#
# 也可以完全在 aarch64 chroot 里（qemu-user 模拟）本地编译，但比交叉编译慢一个数量级。

set -eu

TARGET_ARCH=${TARGET_ARCH:-aarch64}
TARGET=$TARGET_ARCH-alpine-linux-musl
SRC=${SRC:-$(cd "$(dirname "$0")/.." && pwd)}
SYSROOT=${SYSROOT:-$SRC/build/sysroot-$TARGET_ARCH}
RUST_SYSROOT=${RUST_SYSROOT:-$SRC/build/rust-sysroot-$TARGET_ARCH}
JOBS=${JOBS:-2}

# loong-shell 链接的库（readelf -d 可查）：libseat、libudev、libinput、libxkbcommon、libgbm、libgcc_s、musl。
# 不装 mesa-dev：它会拖入 rusticl/LLVM（数百 MB），而我们只需要 libgbm.so。
SYSROOT_PKGS="musl-dev gcc libseat-dev eudev-dev libinput-dev libxkbcommon-dev mesa-gbm"

die() { echo "build-aarch64: $*" >&2; exit 1; }

[ -f /etc/alpine-release ] || die "请在开发 chroot 里运行：tools/dev-chroot.sh run tools/build-aarch64.sh"
[ -d /usr/lib/rustlib/src/rust/library ] || apk add -q --no-progress rust-src
command -v ld.lld >/dev/null || apk add -q --no-progress lld
command -v "qemu-$TARGET_ARCH" >/dev/null || apk add -q --no-progress "qemu-$TARGET_ARCH"

prepare_sysroot() {
	stamp="$SYSROOT/.loong-sysroot"
	if [ "$(cat "$stamp" 2>/dev/null)" = "$SYSROOT_PKGS" ]; then
		return 0
	fi
	echo ">>> 准备 $TARGET_ARCH C sysroot: $SYSROOT"
	initdb=""
	[ -e "$SYSROOT/lib/apk/db/installed" ] || initdb=--initdb
	mkdir -p "$SYSROOT"
	# 用目标架构的 Alpine 公钥校验（开发 chroot 的 /etc/apk/keys 只有宿主架构的公钥）。
	# shellcheck disable=SC2086
	apk add --root "$SYSROOT" --arch "$TARGET_ARCH" $initdb -U --scripts=no --no-progress -q \
		--keys-dir "/usr/share/apk/keys/$TARGET_ARCH" --repositories-file /etc/apk/repositories $SYSROOT_PKGS
	# mesa-gbm 只带 libgbm.so.1，补上链接时用的 libgbm.so（gbm-sys 直接 -lgbm）。
	ln -sf libgbm.so.1 "$SYSROOT/usr/lib/libgbm.so"
	echo "$SYSROOT_PKGS" > "$stamp"
}

ENV_TARGET=$(echo "$TARGET" | tr 'a-z-' 'A-Z_')
ENV_TARGET_LC=$(echo "$TARGET" | tr '-' '_')
LINK_FLAGS="-C link-arg=--target=$TARGET -C link-arg=--sysroot=$SYSROOT -C link-arg=-fuse-ld=lld"
export "CARGO_TARGET_${ENV_TARGET}_LINKER=clang"
export "CARGO_TARGET_${ENV_TARGET}_RUNNER=qemu-$TARGET_ARCH -L $SYSROOT"
export "CC_${ENV_TARGET_LC}=clang --target=$TARGET --sysroot=$SYSROOT" "AR_${ENV_TARGET_LC}=llvm-ar"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
export PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/pkgconfig:$SYSROOT/usr/share/pkgconfig"

prepare_rust_sysroot() {
	want="$(rustc --version) $TARGET"
	stamp="$RUST_SYSROOT/.loong-rust-sysroot"
	if [ "$(cat "$stamp" 2>/dev/null)" = "$want" ]; then
		return 0
	fi
	echo ">>> 编译 $TARGET 的 Rust 标准库: $RUST_SYSROOT"
	work="$RUST_SYSROOT.build"
	rm -rf "$work"
	mkdir -p "$work/src"
	# 一个空的二进制 crate，release + thin LTO：这样标准库的 rlib 里带有 LTO 需要的 bitcode，
	# 与官方预编译的标准库一致。
	cat > "$work/Cargo.toml" <<-'EOF'
		[package]
		name = "loong-rust-sysroot"
		version = "0.0.0"
		edition = "2024"

		[workspace]

		[profile.release]
		lto = "thin"
		codegen-units = 1
	EOF
	echo 'fn main() {}' > "$work/src/main.rs"
	# 与 rustc 自身的构建（bootstrap）一致：标准库按 panic=unwind 编译，但 panic_abort 和
	# compiler_builtins 必须用 panic=abort，否则 panic = "abort" 的程序无法链接。
	cat > "$work/rustc-wrapper" <<-'EOF'
		#!/bin/sh
		case " $* " in
			*" --crate-name panic_abort "*|*" --crate-name compiler_builtins "*) exec "$@" -C panic=abort ;;
		esac
		exec "$@"
	EOF
	chmod 755 "$work/rustc-wrapper"
	(
		cd "$work"
		export RUSTC_BOOTSTRAP=1 CARGO_TARGET_DIR="$work/target" RUSTC_WRAPPER="$work/rustc-wrapper"
		export "CARGO_TARGET_${ENV_TARGET}_RUSTFLAGS=$LINK_FLAGS"
		cargo build --release --target "$TARGET" -j"$JOBS" -Zbuild-std=std,panic_abort,test
	)
	rm -rf "$RUST_SYSROOT"
	mkdir -p "$RUST_SYSROOT/lib/rustlib/$TARGET/lib"
	cp "$work/target/$TARGET/release/deps/"*.rlib "$RUST_SYSROOT/lib/rustlib/$TARGET/lib/"
	echo "$want" > "$stamp"
	rm -rf "$work"
	# cargo 的指纹不包含 sysroot 的内容：换了标准库后，旧的中间产物必须清掉。
	rm -rf "${CARGO_TARGET_DIR:-$SRC/target}/$TARGET"
}

prepare_sysroot
prepare_rust_sysroot
[ "${1:-}" = sysroot ] && exit 0
[ $# -gt 0 ] || set -- build --release -p loong-shell

export "CARGO_TARGET_${ENV_TARGET}_RUSTFLAGS=--sysroot=$RUST_SYSROOT $LINK_FLAGS"
# 文档测试由 rustdoc 编译，它不读 RUSTFLAGS（使用 --target 时 RUSTDOCFLAGS 只作用于目标架构）
export RUSTDOCFLAGS="--sysroot=$RUST_SYSROOT $LINK_FLAGS"
cmd=$1
shift
exec cargo "$cmd" --target "$TARGET" -j"$JOBS" "$@"
