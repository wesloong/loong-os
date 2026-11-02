#!/bin/sh
# 端到端：状态栏的真实电量 / Wi‑Fi 与控制中心。假 sysfs（背光 + 电池）→ 私有总线上的
# loong-sysd → 无屏幕 loong-shell（--realtime，LOONG_SYSD_BUS 指向这条总线）。
#
#   tools/dev-chroot.sh run crates/loong-shell/tests/control-center-headless.sh [输出目录]
#
# 先构建（在 chroot 里）：cargo build --release -p loong-shell -p loong-sysd
#
# 时间线（脚本时间，毫秒）：
#   1500   截图：主屏，状态栏 57%（放电）
#   ≈2400  外部：插上充电器、电量 80%（改假 sysfs，调 Power.Refresh）
#   4500   截图：状态栏 80%，石绿电量 + 闪电
#   5000   从右上角下拉控制中心（录 APNG，直到 7000）
#   7200   截图：控制中心（宣纸）
#   8000   向左拖亮度滑块 → 假 sysfs 的 brightness 跟着变（brightness.log）
#   9800   截图：拖动之后
#   ≈10400 外部：SetBrightness(0.9)（相当于设置里改）→ 滑块用弹簧跟过去
#   12000  截图
#   12800  点「夜墨」→ 写 appearance.toml → 外观文件的监视把整个系统界面换成夜墨
#   16000  截图：控制中心（夜墨）
#   17000  上滑收起
#   19500  截图：夜墨主屏，状态栏 80% 充电
#
# 输出：frame-*.png、anim.png（下拉手势）、brightness.log（开机秒数 值）、timeline.log、
# appearance.toml、各进程日志。

set -eu

SRC=${SRC:-$(cd "$(dirname "$0")/../../.." && pwd)}
OUT=${1:-$SRC/out/control-center}
TARGET=${CARGO_TARGET_DIR:-$SRC/build/target-alpine}
SHELL_BIN=${SHELL_BIN:-$TARGET/release/loong-shell}
SYSD_BIN=${SYSD_BIN:-$TARGET/release/loong-sysd}
for b in "$SHELL_BIN" "$SYSD_BIN"; do
	[ -x "$b" ] || { echo "缺少 $b（cargo build --release -p loong-shell -p loong-sysd）" >&2; exit 1; }
done
command -v dbus-daemon > /dev/null || { echo "缺少 dbus-daemon（apk add dbus）" >&2; exit 1; }

WORK=$(mktemp -d /tmp/loong-cc-XXXXXX)
rm -rf "$OUT"
mkdir -p "$OUT"

# —— 假 sysfs：raw 背光 0–1000（当前 300），放电中的电池 57%（28.5 Wh / 6 W），拔掉的充电器 ——
S=$WORK/sysfs
BAT=$S/class/power_supply/BAT0
mkdir -p "$S/class/backlight/panel" "$BAT" "$S/class/power_supply/AC"
echo raw > "$S/class/backlight/panel/type"
echo 1000 > "$S/class/backlight/panel/max_brightness"
echo 300 > "$S/class/backlight/panel/brightness"
for kv in type=Battery present=1 status=Discharging capacity=57 energy_now=28500000 energy_full=50000000 power_now=6000000; do
	echo "${kv#*=}" > "$BAT/${kv%%=*}"
done
echo Mains > "$S/class/power_supply/AC/type"
echo 0 > "$S/class/power_supply/AC/online"

cat > "$WORK/script.json" <<'EOF'
{
  "fps": 30,
  "duration_ms": 20000,
  "clock": "9:41",
  "date": "九月廿一 · 星期五",
  "follow_appearance": true,
  "events": [
    { "at": 5000, "swipe": { "from": [1100, 4], "to": [1100, 400], "duration": 450 } },
    { "at": 8000, "swipe": { "from": [1100, 198], "to": [980, 198], "duration": 700, "hold": 150 } },
    { "at": 12800, "tap": [1160, 88] },
    { "at": 17000, "swipe": { "from": [1000, 330], "to": [1000, 200], "duration": 120 } }
  ],
  "screenshots": [1500, 4500, 5300, 7200, 9800, 12000, 16000, 19500],
  "record": { "every": 2, "scale": 1, "from_ms": 4900, "to_ms": 6600, "crop": [700, 0, 580, 400] }
}
EOF

export HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/home/.config" XDG_RUNTIME_DIR="$WORK/run"
mkdir -p "$HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
export LANG=C.UTF-8 LIBGL_ALWAYS_SOFTWARE=1 RUST_LOG="${RUST_LOG:-info}"
unset LOONG_THEME LOONG_REDUCED_MOTION DBUS_SESSION_BUS_ADDRESS

# —— 私有总线代替系统总线 ——
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
PIDS=$!
export LOONG_SYSD_BUS="unix:path=$BUS"
i=0
while [ ! -S "$BUS" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done

cleanup() {
	# shellcheck disable=SC2086
	kill $PIDS 2>/dev/null || true
	rm -rf "$WORK"
}
trap cleanup EXIT

now() { cut -d' ' -f1 /proc/uptime; }
call() {
	dbus-send --bus="$LOONG_SYSD_BUS" --print-reply --dest=org.loong.System1 "$@" >> "$OUT/dbus-send.log" 2>&1
}

LOONG_SYSFS_ROOT="$S" "$SYSD_BIN" > "$OUT/loong-sysd.log" 2>&1 &
PIDS="$PIDS $!"
# 每 50 ms 看一次 brightness 文件，只记变化。
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

echo "$(now) 启动 loong-shell" > "$OUT/timeline.log"
# 外部事件跟着 loong-shell 的帧时间走（启动时要先画山水，耗时不定）：等到某张截图写出来再动手。
after() {
	while [ ! -e "$OUT/$1" ]; do sleep 0.05; done
	sleep "$2"
}
(
	after frame-01500.png 0.9
	# 插上充电器、电量 80%：改假 sysfs，再让 loong-sysd 立即重读（容器里收不到 uevent）。
	echo Charging > "$BAT/status"
	echo 80 > "$BAT/capacity"
	echo 40000000 > "$BAT/energy_now"
	echo 1 > "$S/class/power_supply/AC/online"
	echo "$(now) 外部：充电，80%" >> "$OUT/timeline.log"
	call /org/loong/System1/Power org.loong.System1.Power.Refresh
	after frame-09800.png 0.6
	echo "$(now) 外部：SetBrightness(0.9)" >> "$OUT/timeline.log"
	call /org/loong/System1/Backlight org.loong.System1.Backlight.SetBrightness double:0.9 boolean:true
) &
PIDS="$PIDS $!"

"$SHELL_BIN" --headless --realtime --size 1280x800 --script "$WORK/script.json" --out "$OUT" \
	> "$OUT/loong-shell.log" 2>&1 || { echo "loong-shell 失败，见 $OUT/loong-shell.log" >&2; tail -20 "$OUT/loong-shell.log" >&2; }
echo "$(now) 结束" >> "$OUT/timeline.log"

cp "$XDG_CONFIG_HOME/loong/appearance.toml" "$OUT/" 2>/dev/null || true
echo "appearance.toml："
cat "$OUT/appearance.toml" 2>/dev/null || echo "（未写入）"
echo "brightness 文件（开机秒数 值）："
cat "$OUT/brightness.log"
echo "时间线："
cat "$OUT/timeline.log"
echo "截图：$OUT"
