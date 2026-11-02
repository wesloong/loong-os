#!/bin/sh
# PWA 安装 → 主屏 → 启动 → 移除的端到端检查：无屏幕 loong-shell + 真实的 loong-browser 与 loong-webapp。
#
#   tools/dev-chroot.sh run sh webrt/tests/pwa-headless.sh [脚本.json] [输出目录]
#
# 默认脚本 webrt/tests/pwa/install.json（坐标按 1280×800；“分享 → 添加到主屏幕”做两遍，WebKit 偶尔丢掉
# 刚加载完时的第一次点按，同样的内容再装一遍不算变化）：点 Dock 的浏览器打开测试 PWA「墨记」
# （webrt/browser/tests/pages/pwa/，本地 HTTP 服务）→ 分享 → 添加到主屏幕 → 回主屏（展卷到第二页、
# 新图标落印，录成 APNG）→ 点图标以 loong-webapp 打开（没有浏览器界面）→ 回主屏 → 长按 → 移除应用 →
# 确认 → 收墨。结束后检查：浏览器写了 manifest 与图标、loong-shell 发现了新应用并启动了 loong-webapp、
# 卸载后注册表目录与缓存目录都已删除、home.toml 里也没有它了。
#
# 需要先构建：cargo build -p loong-shell；meson setup build/webrt webrt && ninja -C build/webrt。
# 环境变量：LOONG_SHELL（默认 $CARGO_TARGET_DIR/debug/loong-shell）、BUILD（默认 build/webrt）、PORT（默认 8765）。
set -eu

here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../.." && pwd)
script=${1:-$here/pwa/install.json}
out=${2:-out/pwa}
LOONG_SHELL=${LOONG_SHELL:-${CARGO_TARGET_DIR:-$src/build/target-alpine}/debug/loong-shell}
BUILD=${BUILD:-build/webrt}
PORT=${PORT:-8765}
mkdir -p "$out"
out=$(cd "$out" && pwd)

export XDG_RUNTIME_DIR=$(mktemp -d /tmp/loong-pwa-test.XXXXXX)
chmod 700 "$XDG_RUNTIME_DIR"
export HOME=$XDG_RUNTIME_DIR/home
export XDG_DATA_HOME=$HOME/.local/share XDG_CACHE_HOME=$HOME/.cache XDG_CONFIG_HOME=$HOME/.config
mkdir -p "$HOME"
python3 "$src/webrt/browser/tests/serve.py" "$PORT" "$src/webrt/browser/tests/pages" > "$out/http.log" 2>&1 &
server_pid=$!
trap 'kill $server_pid 2>/dev/null; rm -rf "$XDG_RUNTIME_DIR"' EXIT INT TERM

wpe="env WEBKIT_SKIA_ENABLE_CPU_RENDERING=1 LIBGL_ALWAYS_SOFTWARE=1 LANG=C.UTF-8 WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1"
browser="$wpe G_MESSAGES_DEBUG=${BROWSER_DEBUG:-} $src/$BUILD/browser/loong-browser http://127.0.0.1:$PORT/pwa/"
# 主屏上已安装应用的启动命令用 loong-webapp（loong-shell 读 LOONG_WEBAPP），它的输出也在 shell.log 里。
export LOONG_WEBAPP="$wpe LOONG_WEBAPP_CONSOLE=1 $src/$BUILD/loong-webapp"
RUST_LOG=${RUST_LOG:-info,loong_shell::shell=debug} LIBGL_ALWAYS_SOFTWARE=1 "$LOONG_SHELL" --headless --realtime --size 1280x800 \
	--script "$script" --out "$out" --app "browser=$browser > $out/browser.log 2>&1" > "$out/shell.log" 2>&1
pkill -f "$BUILD/browser/loong-browser" 2>/dev/null || true
pkill -f "$BUILD/loong-webapp" 2>/dev/null || true

# ── 检查 ──
fail=0
check() { # 说明 命令…
	what=$1
	shift
	if "$@" > /dev/null 2>&1; then echo "通过：$what"; else echo "失败：$what"; fail=1; fi
}
id=127-0-0-1-$PORT-pwa
check "浏览器写入了 manifest 与图标" grep -q "已添加到主屏幕：.*$id/manifest.json（含图标）" "$out/browser.log"
check "loong-shell 发现了新应用" grep -q "新应用：墨记 · 随手笔记（$id）" "$out/shell.log"
check "点图标启动了 loong-webapp（带作用域、主题色与背景色）" \
	grep -q "启动: .*loong-webapp --id '$id' --name '墨记 · 随手笔记' --scope 'http://127.0.0.1:$PORT/pwa/' --theme-color '#3f7d68' --background-color '#f6f1e4'" "$out/shell.log"
check "loong-webapp 的窗口对应到这个应用" grep -q "app_id=Some(\"loong.webapp._$(echo "$id" | tr - _)\") → Some(\"$id\")" "$out/shell.log"
check "确认后卸载" grep -q "已卸载 $id" "$out/shell.log"
check "注册表目录已删除" test ! -e "$XDG_DATA_HOME/loong/webapps/$id"
check "缓存目录已删除" test ! -e "$XDG_CACHE_HOME/loong/webapps/$id"
check "home.toml 里没有它" sh -c "! grep -q '$id' '$XDG_CONFIG_HOME/loong/home.toml'"
exit $fail
