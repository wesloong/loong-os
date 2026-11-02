#!/bin/sh
# 已安装 Web 应用的三种小笺（截图用）：按 docs/api/loong-webapps.md 的格式直接写注册表，装三个应用——
#   墨记：maskable PNG（铺满小笺）；山居笔记：any SVG（缩到 70% 放在宣纸上）；听雨：没有图标（名称首字，
#   颜料取自 theme_color）——再用无屏幕 loong-shell 截主屏第二页与长按菜单，亮色、暗色各一套。
#
#   tools/dev-chroot.sh run sh webrt/tests/pwa/icons.sh [输出目录]
#
# 环境变量 LOONG_SHELL 同 webrt/tests/pwa-headless.sh。
set -eu

here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../../.." && pwd)
out=${1:-out/pwa-icons}
LOONG_SHELL=${LOONG_SHELL:-${CARGO_TARGET_DIR:-$src/build/target-alpine}/debug/loong-shell}
pages=$src/webrt/browser/tests/pages
mkdir -p "$out"

export XDG_RUNTIME_DIR=$(mktemp -d /tmp/loong-pwa-icons.XXXXXX)
chmod 700 "$XDG_RUNTIME_DIR"
trap 'rm -rf "$XDG_RUNTIME_DIR"' EXIT INT TERM
export HOME=$XDG_RUNTIME_DIR/home
export XDG_DATA_HOME=$HOME/.local/share XDG_CACHE_HOME=$HOME/.cache XDG_CONFIG_HOME=$HOME/.config
reg=$XDG_DATA_HOME/loong/webapps

install_app() { # id manifest [图标文件 目标名]
	mkdir -p "$reg/$1"
	[ $# -ge 4 ] && cp "$3" "$reg/$1/$4"
	printf '%s\n' "$2" > "$reg/$1/manifest.json"
}
install_app moji '{
  "name": "墨记 · 随手笔记", "short_name": "墨记",
  "start_url": "http://127.0.0.1:8765/pwa/?source=pwa", "scope": "http://127.0.0.1:8765/pwa/",
  "display": "standalone", "theme_color": "#3f7d68", "background_color": "#f6f1e4",
  "loong_icon": { "file": "icon.png", "purpose": "maskable" }
}' "$pages/pwa/icon-192.png" icon.png
install_app shanju '{
  "name": "山居笔记", "short_name": "山居",
  "start_url": "http://127.0.0.1:8765/index.html?source=pwa", "scope": "http://127.0.0.1:8765/",
  "display": "standalone", "theme_color": "#2e5e74", "background_color": "#f3eee2",
  "loong_icon": { "file": "icon.svg", "purpose": "any" }
}' "$pages/icon.svg" icon.svg
install_app tingyu '{
  "name": "听雨", "start_url": "https://tingyu.example/", "display": "standalone",
  "theme_color": "#9d2b3c"
}'

for theme in light dark; do
	sed "s/\"theme\": \"light\"/\"theme\": \"$theme\"/" "$here/icons.json" > "$XDG_RUNTIME_DIR/icons-$theme.json"
	mkdir -p "$out/$theme"
	LIBGL_ALWAYS_SOFTWARE=1 "$LOONG_SHELL" --headless --size 1280x800 \
		--script "$XDG_RUNTIME_DIR/icons-$theme.json" --out "$out/$theme" > "$out/$theme/shell.log" 2>&1
	grep -q "PWA 注册表：.*，3 个应用" "$out/$theme/shell.log" || { echo "失败：没有读到 3 个应用（$theme）"; exit 1; }
done
echo "截图在 $out/light 与 $out/dark"
