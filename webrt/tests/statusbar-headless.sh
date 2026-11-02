#!/bin/sh
# 自适应状态栏的端到端检查：无屏幕 loong-shell + 真实的 WPE WebKit 客户端（loong-webapp、loong-browser）。
#
#   tools/dev-chroot.sh run sh webrt/tests/statusbar-headless.sh dark|light [输出目录]
#
# 先构建（都在 chroot 里）：
#   cargo build -p loong-shell
#   meson setup build/webrt webrt && ninja -C build/webrt
#
# 系统外观为第一个参数（脚本的 theme，同时写入 appearance.toml，Web 应用也跟随它）。依次打开：
#   备忘录 notes      loong-webapp，LoongKit 宣纸页面（data-theme=light，meta theme-color #f3eee2）
#   日历 calendar     loong-webapp，LoongKit 夜墨页面（data-theme=dark，meta theme-color #15140f）
#   相册 photos       loong-webapp，没有 theme-color 的普通网页，底色与系统外观相反 → loong-shell 取样
#   浏览器 browser    loong-browser --theme 与系统外观相反（界面指定状态栏样式），再点按网页里的按钮
#                     进入网页全屏（深色网页，状态栏改按网页的 theme-color）
# 之后经多任务打开备忘录、再经多任务打开日历（亮 → 暗），只录顶部一条（anim.png，全分辨率）。
# loong-shell 日志里的“状态栏：…（…）”记下每次换墨的依据。
#
# 环境变量：SHELL_BIN（默认 $CARGO_TARGET_DIR/debug/loong-shell）、BUILD（默认 build/webrt）、
#           PORT（本地 HTTP 服务端口，默认 8766）。
set -eu

THEME=${1:?用法：statusbar-headless.sh dark|light [输出目录]}
case "$THEME" in
	dark) OPPOSITE=light; PLAIN=plain-light.html ;;
	light) OPPOSITE=dark; PLAIN=plain-dark.html ;;
	*) echo "外观只能是 dark 或 light" >&2; exit 2 ;;
esac
SRC=${SRC:-$(cd "$(dirname "$0")/../.." && pwd)}
OUT=${2:-$SRC/out/statusbar-$THEME}
TARGET=${CARGO_TARGET_DIR:-$SRC/build/target-alpine}
SHELL_BIN=${SHELL_BIN:-$TARGET/debug/loong-shell}
BUILD=${BUILD:-$SRC/build/webrt}
PORT=${PORT:-8766}
PAGES=$SRC/webrt/tests/statusbar
for b in "$SHELL_BIN" "$BUILD/loong-webapp" "$BUILD/browser/loong-browser"; do
	[ -x "$b" ] || { echo "缺少 $b（构建方法见脚本开头）" >&2; exit 1; }
done

WORK=$(mktemp -d /tmp/loong-statusbar-XXXXXX)
rm -rf "$OUT"
mkdir -p "$OUT"
export HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/home/.config" XDG_RUNTIME_DIR="$WORK/run"
export XDG_DATA_HOME="$WORK/home/.local/share" XDG_CACHE_HOME="$WORK/home/.cache"
mkdir -p "$XDG_CONFIG_HOME/loong" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
printf 'theme = "%s"\n' "$THEME" > "$XDG_CONFIG_HOME/loong/appearance.toml"
export LANG=C.UTF-8 LIBGL_ALWAYS_SOFTWARE=1 WEBKIT_SKIA_ENABLE_CPU_RENDERING=1
export RUST_LOG="${RUST_LOG:-info}" G_MESSAGES_DEBUG=loong-webrt LOONG_BROWSER_NEW_INSTANCE=1
# 只用于开发 chroot：bwrap 在 chroot 里起不来（见 settings-headless.sh）。
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
unset DBUS_SESSION_BUS_ADDRESS LOONG_THEME

python3 "$SRC/webrt/browser/tests/serve.py" "$PORT" "$PAGES" > "$OUT/http.log" 2>&1 &
SERVER=$!
cleanup() {
	kill "$SERVER" 2>/dev/null || true
	pkill -f "$BUILD/loong-webapp" 2>/dev/null || true
	pkill -f "$BUILD/browser/loong-browser" 2>/dev/null || true
	rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

# 时间（毫秒）：每个应用打开 7.5 秒后截图（WebKit 冷启动较慢），再上滑回主屏。
cat > "$WORK/script.json" <<EOF
{
  "fps": 30,
  "duration_ms": 46200,
  "clock": "9:41",
  "theme": "$THEME",
  "events": [
    { "at": 300, "launch": "notes" },
    { "at": 8000, "swipe": { "from": [640, 795], "to": [640, 430], "duration": 170 } },
    { "at": 9300, "launch": "calendar" },
    { "at": 17000, "swipe": { "from": [640, 795], "to": [640, 430], "duration": 170 } },
    { "at": 18300, "launch": "photos" },
    { "at": 26000, "swipe": { "from": [640, 795], "to": [640, 430], "duration": 170 } },
    { "at": 27300, "launch": "browser" },
    { "at": 35000, "tap": [640, 438] },
    { "at": 37200, "swipe": { "from": [640, 795], "to": [640, 600], "duration": 250, "hold": 400 } },
    { "at": 39200, "launch": "notes" },
    { "at": 41700, "swipe": { "from": [640, 795], "to": [640, 600], "duration": 250, "hold": 400 } },
    { "at": 43700, "launch": "calendar" }
  ],
  "screenshots": [7800, 9200, 16800, 25800, 34800, 37000, 38800, 41500, 43300, 46000],
  "record": { "every": 2, "scale": 1, "from_ms": 38800, "to_ms": 46000, "crop": [0, 0, 1280, 72] }
}
EOF

webapp() { echo "$BUILD/loong-webapp --id $1 --name $2 file://$PAGES/$3"; }
"$SHELL_BIN" --headless --realtime --size 1280x800 --script "$WORK/script.json" --out "$OUT" \
	--app "notes=$(webapp notes 备忘录 light.html) > $OUT/notes.log 2>&1" \
	--app "calendar=$(webapp calendar 日历 dark.html) > $OUT/calendar.log 2>&1" \
	--app "photos=$(webapp photos 相册 $PLAIN) > $OUT/photos.log 2>&1" \
	--app "browser=$BUILD/browser/loong-browser --theme $OPPOSITE http://127.0.0.1:$PORT/fullscreen.html > $OUT/browser.log 2>&1" \
	> "$OUT/loong-shell.log" 2>&1 || true

echo "系统外观 $THEME；状态栏换墨（时间为日志时间）："
sed 's/\x1b\[[0-9;]*m//g' "$OUT/loong-shell.log" | grep -E "状态栏|启动:" || true
echo "客户端（loong_app_v1）："
grep -h "loong_app_v1\|loong 平台已连接" "$OUT"/*.log 2>/dev/null | sed 's/^/  /' || true
echo "截图与录像（anim.png）：$OUT"
