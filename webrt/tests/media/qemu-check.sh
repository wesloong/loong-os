#!/bin/sh
# 在 QEMU 镜像里打开媒体检查页（index.html），播放中连续截图，取出页面的控制台记录。
# 在开发 chroot 里运行；多个代理共用机器时整个过程包在 flock 里：
#
#   tools/dev-chroot.sh run sh webrt/tests/media/mkclips.sh build/media-test
#   tools/dev-chroot.sh run flock /var/lock/loong-qemu sh webrt/tests/media/qemu-check.sh \
#       build/image/qemu-x86_64/loong-qemu-x86_64.img build/media-test out/media-qemu
#
# 镜像要带 WebKit（WITH_WPE=1）与 DEV_IMAGE（串口 root 免密，QEMU 机型默认如此）。原镜像不改：
# 启动的是一个 qcow2 覆盖层。片段打成 tar 作为第二块磁盘（/dev/vdb）传进去，解到 /home/loong/media，
# 由 busybox-extras 的 httpd 在 127.0.0.1:8080 提供（真实网站的媒体都经 HTTP 由 WebKit 自己的 source 读取；
# file:// 时 Web 进程的沙箱看不到 /home，见 index.html 开头）。开发 chroot 里要有 busybox-extras。
# 页面出现后点按一下页面空白处：有声音的媒体与 AudioContext 要有用户手势才开始（WPE 默认的自动播放策略
# 是 ALLOW_WITHOUT_SOUND）。
# 环境变量：QUERY（页面的地址参数，例如 ?only=webm,ogg）、SHOTS（点按后截几张图，默认 6）、
#           GAP（每张之间的秒数，默认 8）。
set -eu
IMG=${1:?用法：qemu-check.sh <镜像> <片段目录> <输出目录>}
CLIPS=${2:?}
OUT=${3:?}
QUERY=${QUERY:-}
SHOTS=${SHOTS:-6}
GAP=${GAP:-8}
Q="python3 $SRC/tools/qemu.py"

mkdir -p "$OUT"
rm -f "$OUT"/*.png "$OUT"/media.log "$OUT"/states.txt
# 片段 + httpd（busybox-extras，动态链接 musl，镜像里能直接运行）+ 媒体类型表
STAGE="$OUT/stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -a "$CLIPS"/. "$STAGE"/
cp /bin/busybox-extras "$STAGE"/
printf '%s\n' .webm:video/webm .ogv:video/ogg .opus:audio/ogg .mp3:audio/mpeg .js:text/javascript \
	.html:"text/html; charset=utf-8" > "$STAGE"/httpd.conf
tar -C "$STAGE" -cf "$OUT/media.tar" .
rm -rf "$STAGE"
rm -f "$OUT/disk.qcow2"
qemu-img create -q -f qcow2 -b "$(realpath "$IMG")" -F raw "$OUT/disk.qcow2"
$Q start "$OUT/disk.qcow2" --disk "$OUT/media.tar"
trap '$Q stop >/dev/null 2>&1 || true; rm -f "$OUT/disk.qcow2" "$OUT/media.tar"' EXIT

$Q wait "login:" --timeout 900
sleep 2
$Q send root
sleep 3
# 等 loong-shell 的 Wayland socket，再给锁屏一点时间画出来
$Q exec 'n=0; until ls /run/user/1000/wayland-[0-9] >/dev/null 2>&1 || [ $n -ge 300 ]; do sleep 1; n=$((n+1)); done; ls /run/user/1000' --timeout 400
sleep 25
$Q shot "$OUT/0-lock.png"
# 上滑解锁（TCG 下刚开机事件处理慢，滑两次）
$Q swipe 640 790 640 150 600
sleep 8
$Q swipe 640 790 640 150 600
sleep 8
$Q shot "$OUT/1-home.png"

$Q exec 'mkdir -p /home/loong/media && tar -xf /dev/vdb -C /home/loong/media && chown -R loong:loong /home/loong/media && ls /home/loong/media'
$Q exec 'ip link set lo up; /home/loong/media/busybox-extras httpd -p 127.0.0.1:8080 -h /home/loong/media -c /home/loong/media/httpd.conf && sleep 1 && wget -q -O /dev/null http://127.0.0.1:8080/index.html && echo httpd ok'
# 以 loong 用户、会话的环境启动 loong-webapp（loong-shell 把不是从图标启动的窗口直接打开）
$Q exec "WL=\$(cd /run/user/1000 && ls -d wayland-[0-9] | head -1); su loong -s /bin/sh -c \"cd /home/loong && env XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=\$WL HOME=/home/loong LANG=C.UTF-8 LIBGL_ALWAYS_SOFTWARE=1 WEBKIT_SKIA_ENABLE_CPU_RENDERING=1 LOONG_WEBAPP_CONSOLE=1 GST_DEBUG=2 setsid loong-webapp --id media --name 媒体 'http://127.0.0.1:8080/index.html$QUERY' > /home/loong/media.log 2>&1 &\"; echo started"
# 等页面开始输出（第一行 MEDIA）
$Q exec 'n=0; until grep -q "MEDIA {" /home/loong/media.log 2>/dev/null || [ $n -ge 600 ]; do sleep 1; n=$((n+1)); done; echo "waited $n s"' --timeout 700
sleep 8
$Q tap 1150 640
# TCG 下画面比页面里的时间落后十几秒，所以连续截几张；每张记下当时页面最后输出的状态
: > "$OUT/states.txt"
i=0
while [ $i -lt "$SHOTS" ]; do
	sleep "$GAP"
	i=$((i + 1))
	$Q shot "$OUT/media-$i.png"
	$Q exec 'grep "MEDIA {" /home/loong/media.log | tail -1' | sed "s/^.*MEDIA /[$i] /" | tee -a "$OUT/states.txt"
done
# 网页进程还在吗；完整日志（GStreamer 警告、页面事件）
$Q exec 'ps -o pid,user,args | grep -i "[W]PE\|[l]oong-webapp"; cat /home/loong/media.log' --timeout 120 > "$OUT/media.log" || true
grep -E "TYPES|EVENT|crash|Crash|terminated|ERROR|CRITICAL" "$OUT/media.log" | cut -c1-300 | head -40 || true
echo "截图与日志：$OUT"
