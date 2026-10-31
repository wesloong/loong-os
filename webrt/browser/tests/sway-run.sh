#!/bin/sh
# 在无屏幕的 sway（wlroots headless + pixman，支持子表面）里运行 loong-browser，
# 按场景文件操作并截图。用于开发 chroot（先构建 build/webrt）：
#
#   tools/dev-chroot.sh run sh webrt/browser/tests/sway-run.sh 场景文件 输出目录 [浏览器参数…]
#
# 场景文件每行一条命令（# 开头为注释）：
#   sleep 秒                 等待
#   shot 文件名.png          截图（grim）
#   click X Y               指针移到 (X, Y) 并点按左键
#   drag X1 Y1 X2 Y2        按住左键拖动
#   move X Y                指针移到 (X, Y)
#   wheel N                 滚轮 N 格（正数向下）
#   type 文本               用虚拟键盘输入（ASCII）
#   key 键名                按一个键，例如 Return、Escape、BackSpace
#   preedit 文本 / commit 文本  模拟输入法（text-input-v3 ← input-method-v2）：预编辑 / 提交
#   imestate                在 vinput.log 里记下输入法是否被激活
#   run 命令…               在合成器环境里执行任意命令（例如再启动一次浏览器）
# 指针与键盘来自 loong-vinput（常驻的虚拟设备，见 loong-vinput.c）。
#
# 环境变量：SIZE（默认 1280x800）、SCALE（默认 1）、BUILD（默认 build/webrt）、APP（代替浏览器运行的程序）。
set -eu

scenario=$1
out=$2
shift 2
SIZE=${SIZE:-1280x800}
SCALE=${SCALE:-1}
BUILD=${BUILD:-build/webrt}
mkdir -p "$out"

export XDG_RUNTIME_DIR=$(mktemp -d /tmp/loong-browser-test.XXXXXX)
chmod 700 "$XDG_RUNTIME_DIR"
export HOME=$XDG_RUNTIME_DIR/home XDG_DATA_HOME=$XDG_RUNTIME_DIR/home/.local/share XDG_CACHE_HOME=$XDG_RUNTIME_DIR/home/.cache
mkdir -p "$HOME"
export WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 WLR_HEADLESS_OUTPUTS=1

cat > "$XDG_RUNTIME_DIR/sway.conf" <<EOF
output HEADLESS-1 mode $SIZE scale $SCALE bg #808080 solid_color
default_border none
for_window [app_id=".*"] fullscreen enable
seat seat0 hide_cursor 1
EOF
sway -c "$XDG_RUNTIME_DIR/sway.conf" > "$out/sway.log" 2>&1 &
sway_pid=$!
browser_pid=
vinput_pid=
server_pid=
cleanup() {
	exec 3>&- 2>/dev/null || true
	kill "$browser_pid" "$vinput_pid" $server_pid 2>/dev/null || true
	sleep 0.3
	kill "$sway_pid" 2>/dev/null || true
	wait 2>/dev/null || true
	rm -rf "$XDG_RUNTIME_DIR"
}
trap cleanup EXIT INT TERM
for _ in $(seq 50); do
	ls "$XDG_RUNTIME_DIR"/wayland-[0-9] >/dev/null 2>&1 && ls "$XDG_RUNTIME_DIR"/sway-ipc.* >/dev/null 2>&1 && break
	sleep 0.1
done
export WAYLAND_DISPLAY=$(basename "$(ls "$XDG_RUNTIME_DIR"/wayland-[0-9] | head -1)")
export SWAYSOCK=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.* | head -1)

# 输出的逻辑尺寸（虚拟指针的坐标范围）
w=${SIZE%x*}
h=${SIZE#*x}
mkfifo "$XDG_RUNTIME_DIR/vinput"
lw=$(awk "BEGIN { printf \"%d\", $w / $SCALE }")
lh=$(awk "BEGIN { printf \"%d\", $h / $SCALE }")
"$BUILD/browser/loong-vinput" "$lw" "$lh" < "$XDG_RUNTIME_DIR/vinput" > "$out/vinput.log" 2>&1 &
vinput_pid=$!
exec 3> "$XDG_RUNTIME_DIR/vinput"

# 本地 HTTP 服务（http://127.0.0.1:$PORT/），默认提供 tests/pages/
PORT=${PORT:-8765}
server_pid=
if [ "${SERVE:-$(dirname "$0")/pages}" != none ]; then
	python3 "$(dirname "$0")/serve.py" "$PORT" "${SERVE:-$(dirname "$0")/pages}" > "$out/http.log" 2>&1 &
	server_pid=$!
fi

export WEBKIT_SKIA_ENABLE_CPU_RENDERING=1 LIBGL_ALWAYS_SOFTWARE=1 LANG=C.UTF-8
# chroot 里 / 不是挂载点，bubblewrap 沙箱起不来（镜像里不需要这个变量）。
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
# APP 可以换成别的程序（例如对照测试 WPE 自带平台上的 loong-webapp）
${APP:-$BUILD/browser/loong-browser} "$@" > "$out/browser.log" 2>&1 &
browser_pid=$!

set -f
while IFS= read -r line || [ -n "$line" ]; do
	set -- $line
	[ $# -eq 0 ] && continue
	case "$1" in
	\#*) ;;
	sleep) sleep "$2" ;;
	shot) grim "$out/$2" ;;
	click | drag | move | wheel | type | key | preedit | commit | imestate) echo "$line" >&3 ;;
	run) shift; "$@" ;;
	*) echo "未知命令: $line" >&2 ;;
	esac
done < "$scenario"
kill -0 "$browser_pid" 2>/dev/null && echo "浏览器仍在运行" || echo "浏览器已退出" >&2
