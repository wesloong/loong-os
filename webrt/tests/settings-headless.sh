#!/bin/sh
# 在开发 chroot 里端到端地跑一遍“设置”：假 sysfs（背光 + 电池）→ 私有总线上的 loong-sysd →
# 无屏幕的 loong-shell → loong-webapp（WPE WebKit，--bridge=system）→ 设置页面。
# 按脚本点按、拖动亮度滑块并截图，同时记录假 sysfs 中 brightness 文件的变化。
#
#   tools/dev-chroot.sh run webrt/tests/settings-headless.sh [输出目录]
#
# 先构建（都在 chroot 里）：
#   cargo build --release -p loong-shell -p loong-sysd
#   meson setup build/webrt-dev webrt -Dweb_root=$SRC/webrt && ninja -C build/webrt-dev
# 受信任目录指向源码树的 webrt/，这样不用把页面装到 /usr/share/loong/web 也能拿到 bridge。
#
# 环境变量 FAKE_IWD=1：再起一个假 iwd（build/webrt-dev/fake-iwd），走一遍 Wi‑Fi 扫描、
# 输入密码、经 Agent 连接（脚本换成 Wi‑Fi 的那一套）。否则 Wi‑Fi 页面显示“不可用”。

set -eu

SRC=${SRC:-$(cd "$(dirname "$0")/../.." && pwd)}
OUT=${1:-$SRC/out/settings-headless}
TARGET=${CARGO_TARGET_DIR:-$SRC/build/target-alpine}
SHELL_BIN=${SHELL_BIN:-$TARGET/release/loong-shell}
SYSD_BIN=${SYSD_BIN:-$TARGET/release/loong-sysd}
WEBAPP_BIN=${WEBAPP_BIN:-$SRC/build/webrt-dev/loong-webapp}
FAKE_IWD_BIN=${FAKE_IWD_BIN:-$SRC/build/webrt-dev/fake-iwd}
FAKE_IWD=${FAKE_IWD:-0}
for b in "$SHELL_BIN" "$SYSD_BIN" "$WEBAPP_BIN"; do
	[ -x "$b" ] || { echo "缺少 $b（构建方法见脚本开头）" >&2; exit 1; }
done

WORK=$(mktemp -d /tmp/loong-settings-XXXXXX)
rm -rf "$OUT"
mkdir -p "$OUT"

# —— 假 sysfs：raw 背光 0–1000（当前 300），放电中的电池 57%（28.5 Wh / 6 W ≈ 4 小时 45 分），拔掉的充电器 ——
S=$WORK/sysfs
mkdir -p "$S/class/backlight/panel" "$S/class/power_supply/BAT0" "$S/class/power_supply/AC"
echo raw > "$S/class/backlight/panel/type"
echo 1000 > "$S/class/backlight/panel/max_brightness"
echo 300 > "$S/class/backlight/panel/brightness"
for kv in type=Battery present=1 status=Discharging capacity=57 energy_now=28500000 energy_full=50000000 power_now=6000000; do
	echo "${kv#*=}" > "$S/class/power_supply/BAT0/${kv%%=*}"
done
echo Mains > "$S/class/power_supply/AC/type"
echo 0 > "$S/class/power_supply/AC/online"

cat > "$WORK/os-release" <<'EOF'
NAME="Loong OS"
PRETTY_NAME="Loong OS 0.1.0"
ID=loong
ID_LIKE=alpine
VERSION_ID=0.1.0
IMAGE_VERSION=0.1.0
EOF

# —— 触摸脚本（逻辑点，毫秒；--realtime 下按真实时间推进） ——
if [ "$FAKE_IWD" = 1 ]; then
	# Wi‑Fi：扫描列表 → 点已保存的“听雨轩”加入 → 它的详情页 → 忽略此网络（确认弹窗）→ 回到列表 →
	# 点新的加密网络“茶寮-5G”弹出密码 sheet。
	# （无屏幕脚本只有点按与滑动，没有键盘事件；输入密码经 Agent 连接的流程由 loong-sysd 的集成测试覆盖。）
	START="#wifi"
	cat > "$WORK/script.json" <<'EOF'
{
  "fps": 30,
  "duration_ms": 23500,
  "clock": "9:41",
  "events": [
    { "at": 300, "launch": "settings" },
    { "at": 10000, "tap": [700, 545] },
    { "at": 13500, "tap": [1105, 383] },
    { "at": 16000, "tap": [810, 526] },
    { "at": 17500, "tap": [720, 456] },
    { "at": 20500, "tap": [700, 494] }
  ],
  "screenshots": [9500, 10600, 13000, 15500, 17000, 20000, 23000]
}
EOF
else
	START="#display"
	cat > "$WORK/script.json" <<'EOF'
{
  "fps": 30,
  "duration_ms": 25000,
  "clock": "9:41",
  "events": [
    { "at": 300, "launch": "settings" },
    { "at": 9500, "swipe": { "from": [842, 410], "to": [640, 410], "duration": 900, "hold": 200 } },
    { "at": 14500, "tap": [903, 175] },
    { "at": 17000, "tap": [170, 354] },
    { "at": 19500, "tap": [170, 248] },
    { "at": 22000, "tap": [170, 170] }
  ],
  "screenshots": [9000, 12000, 14000, 16500, 19000, 21500, 24800]
}
EOF
fi

export HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/home/.config" XDG_RUNTIME_DIR="$WORK/run"
mkdir -p "$HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
export LANG=C.UTF-8 LIBGL_ALWAYS_SOFTWARE=1 WEBKIT_SKIA_ENABLE_CPU_RENDERING=1
export LOONG_SYSFS_ROOT="$S" RUST_LOG="${RUST_LOG:-info}"
export LOONG_OS_RELEASE="$WORK/os-release" LOONG_DEVICE_FILE="$SRC/system/profiles/qemu-x86_64.device.toml"
export LOONG_WEBAPP_CONSOLE=1
# 只用于开发 chroot：chroot 的根不是挂载点，bwrap 做不了 `mount --make-rslave /`，WebKit 的沙箱起不来。
# 设备上保留沙箱（WebContent 进程里没有系统总线的套接字）。
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
URL="file://$SRC/webrt/apps/settings/index.html$START"

# —— 私有总线代替系统总线 ——
# 不设 DBUS_SESSION_BUS_ADDRESS（设备上的会话里也没有会话总线）：否则 WebKit 的沙箱要用 bwrap
# 启动 xdg-dbus-proxy，而 bwrap 在 chroot 里起不来；也避免激活无障碍总线（at-spi）。
# loong-sysd、假 iwd 与 bridge 都用 LOONG_SYSD_BUS 指向这条总线。
unset DBUS_SESSION_BUS_ADDRESS
BUS="$WORK/bus"
sed "s|@BUS@|$BUS|" > "$WORK/bus.conf" <<'EOF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path=@BUS@</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
dbus-daemon --config-file="$WORK/bus.conf" --nofork --nopidfile --nosyslog > "$OUT/dbus.log" 2>&1 &
BUS_PID=$!
export LOONG_SYSD_BUS="unix:path=$BUS"
i=0
while [ ! -S "$BUS" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done

PIDS=$BUS_PID
cleanup() {
	# shellcheck disable=SC2086
	kill $PIDS 2>/dev/null || true
	pkill -f "loong-webapp --id settings" 2>/dev/null || true
	rm -rf "$WORK"
}
trap cleanup EXIT

now() { cut -d' ' -f1 /proc/uptime; }

"$SYSD_BIN" > "$OUT/loong-sysd.log" 2>&1 &
PIDS="$PIDS $!"
if [ "$FAKE_IWD" = 1 ]; then
	[ -x "$FAKE_IWD_BIN" ] || { echo "缺少 $FAKE_IWD_BIN" >&2; exit 1; }
	"$FAKE_IWD_BIN" --address "$LOONG_SYSD_BUS" > "$OUT/fake-iwd.log" 2>&1 &
	PIDS="$PIDS $!"
fi
# 每 50 ms 看一次 brightness 文件，只记变化（时间是开机以来的秒数）。
(
	last=
	while :; do
		v=$(cat "$S/class/backlight/panel/brightness")
		[ "$v" != "$last" ] && echo "$(now) $v" && last=$v
		sleep 0.05
	done
) > "$OUT/brightness.log" &
PIDS="$PIDS $!"
sleep 1

echo "$(now) 开始（脚本时间 0）" > "$OUT/timeline.log"
if [ "$FAKE_IWD" != 1 ]; then
	# 别处改亮度（相当于控制中心）：滑块应当用弹簧跟过去。
	(
		sleep 12.5
		echo "$(now) 外部 SetBrightness(0.9)" >> "$OUT/timeline.log"
		dbus-send --bus="$LOONG_SYSD_BUS" --print-reply --dest=org.loong.System1 /org/loong/System1/Backlight \
			org.loong.System1.Backlight.SetBrightness double:0.9 boolean:true > "$OUT/external-set.log" 2>&1
	) &
	PIDS="$PIDS $!"
fi
"$SHELL_BIN" --headless --realtime --size 1280x800 --script "$WORK/script.json" --out "$OUT" \
	--app "settings=$WEBAPP_BIN --id settings --name 设置 --bridge=system $URL" > "$OUT/loong-shell.log" 2>&1 || true
echo "$(now) 结束" >> "$OUT/timeline.log"

cp "$XDG_CONFIG_HOME/loong/appearance.toml" "$OUT/" 2>/dev/null || true
echo "appearance.toml："
cat "$OUT/appearance.toml" 2>/dev/null || echo "（未写入）"
echo "brightness 文件（开机秒数 值）："
cat "$OUT/brightness.log"
echo "时间线："
cat "$OUT/timeline.log"
echo "截图：$OUT"
