#!/bin/sh
# 在 loong-shell 的无屏幕模式里运行 loong-browser：点按主屏的浏览器图标启动它，然后按脚本回放触摸
# （loong-shell 把触摸发给窗口的根表面，正好检验 loong 平台“自己做子表面命中测试”的那条路径）。
#
#   tools/dev-chroot.sh run sh webrt/browser/tests/shell-run.sh 脚本.json 输出目录 [浏览器参数…]
#
# 脚本格式见 crates/loong-shell/src/script.rs（tap / swipe / screenshots，按真实时间回放）。
# 环境变量：LOONG_SHELL（默认 /src/build/target-alpine/debug/loong-shell，即主仓库的构建）、
#           BUILD（默认 build/webrt）、PORT（本地 HTTP 服务端口，默认 8765，提供 tests/pages/）。
set -eu

script=$1
out=$2
shift 2
LOONG_SHELL=${LOONG_SHELL:-/src/build/target-alpine/debug/loong-shell}
BUILD=${BUILD:-build/webrt}
PORT=${PORT:-8765}
mkdir -p "$out"

export XDG_RUNTIME_DIR=$(mktemp -d /tmp/loong-shell-test.XXXXXX)
chmod 700 "$XDG_RUNTIME_DIR"
export HOME=$XDG_RUNTIME_DIR/home XDG_DATA_HOME=$XDG_RUNTIME_DIR/home/.local/share XDG_CACHE_HOME=$XDG_RUNTIME_DIR/home/.cache
mkdir -p "$HOME"
python3 "$(dirname "$0")/serve.py" "$PORT" "$(dirname "$0")/pages" > "$out/http.log" 2>&1 &
server_pid=$!
trap 'kill $server_pid 2>/dev/null; rm -rf "$XDG_RUNTIME_DIR"' EXIT INT TERM

browser="env WEBKIT_SKIA_ENABLE_CPU_RENDERING=1 LIBGL_ALWAYS_SOFTWARE=1 LANG=C.UTF-8 WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1"
browser="$browser G_MESSAGES_DEBUG=loong-webrt $(pwd)/$BUILD/browser/loong-browser $*"
LIBGL_ALWAYS_SOFTWARE=1 "$LOONG_SHELL" --headless --realtime --size 1280x800 --script "$script" --out "$out" \
	--app "browser=$browser > $out/browser.log 2>&1" > "$out/shell.log" 2>&1
pkill -f "$BUILD/browser/loong-browser" 2>/dev/null || true
