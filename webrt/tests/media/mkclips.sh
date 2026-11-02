#!/bin/sh
# 生成媒体检查页（index.html）用的测试片段。在开发 chroot 里运行（用 chroot 里完整的发行版 GStreamer 编码，
# 需要 gstreamer-tools、gst-plugins-bad（fdkaacenc）与 gst-plugins-ugly（x264enc）；镜像里只有解码用的最小插件集，
# 见 system/packages/gst-plugins-loong 与 ffmpeg-loong）：
#
#   tools/dev-chroot.sh run sh webrt/tests/media/mkclips.sh build/media-test
#
# 输出目录里放 index.html 与片段；QEMU 里怎么用见 docs/媒体格式.md。
set -eu
out=${1:?用法：mkclips.sh <输出目录>}
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
cp "$here/index.html" "$out/"
cd "$out"

# 画面：320×240@15 的测试图 + 时间码；声音：48 kHz 立体声正弦波（每个缓冲 1024 个采样）。60 秒。
# 管道描述交给 eval 再分词：gst-launch 把每个参数当作一个整体，引号里的 "Sans 28" 才能带空格。
launch() { eval "gst-launch-1.0 -q -e $*"; }
size='video/x-raw,width=320,height=240,framerate=15/1 ! timeoverlay font-desc="Sans 28" ! videoconvert'
a_fmt='audio/x-raw,rate=48000,channels=2 ! audioconvert'
vp9='vp9enc deadline=1 cpu-used=8 keyframe-max-dist=15'

# WebM：VP9 + Opus；WebM：VP8 + Vorbis；Ogg：Theora + Vorbis
launch "videotestsrc num-buffers=900 pattern=ball ! $size ! $vp9 ! queue ! mux." \
	"audiotestsrc num-buffers=2813 freq=440 volume=0.2 ! $a_fmt ! opusenc ! queue ! mux." \
	"webmmux name=mux ! filesink location=vp9-opus.webm"
launch "videotestsrc num-buffers=900 pattern=smpte ! $size ! vp8enc deadline=1 keyframe-max-dist=15 ! queue ! mux." \
	"audiotestsrc num-buffers=2813 freq=550 volume=0.2 ! $a_fmt ! vorbisenc ! queue ! mux." \
	"webmmux name=mux ! filesink location=vp8-vorbis.webm"
launch "videotestsrc num-buffers=900 pattern=ball background-color=0xff604020 ! $size ! theoraenc ! queue ! mux." \
	"audiotestsrc num-buffers=2813 freq=550 volume=0.2 ! $a_fmt ! vorbisenc ! queue ! mux." \
	"oggmux name=mux ! filesink location=theora-vorbis.ogv"
# MP4：H.264 High（x264enc，gst-plugins-ugly；CABAC、B 帧，与网站上的 MP4 相同）+ AAC-LC（fdkaacenc，gst-plugins-bad）
h264='x264enc speed-preset=faster bitrate=400 key-int-max=15 ! video/x-h264,profile=high ! h264parse'
launch "videotestsrc num-buffers=900 pattern=ball background-color=0xff402060 ! $size ! $h264 ! queue ! mux." \
	"audiotestsrc num-buffers=2813 freq=660 volume=0.2 ! $a_fmt ! fdkaacenc bitrate=128000 ! queue ! mux." \
	"mp4mux name=mux faststart=true ! filesink location=h264-aac.mp4"
# 纯音频：MP3（带 ID3v2 标签）、Ogg Opus、M4A（AAC-LC）
launch "audiotestsrc num-buffers=2813 freq=330 volume=0.2 ! $a_fmt ! lamemp3enc ! id3v2mux ! filesink location=tone.mp3"
launch "audiotestsrc num-buffers=2813 freq=440 volume=0.2 ! $a_fmt ! opusenc ! oggmux ! filesink location=tone.opus"
launch "audiotestsrc num-buffers=2813 freq=770 volume=0.2 ! $a_fmt ! fdkaacenc bitrate=128000 ! mp4mux faststart=true ! filesink location=tone.m4a"

# MSE 用的 WebM（VP9 + Opus，10 秒；streamable：没有 Cues、Segment 长度未知，与网站的分片一样）与
# Web Audio 解码用的 WAV（1 秒）。file:// 页面不能 fetch 本地文件，所以写成 base64 放进 media-data.js。
launch "videotestsrc num-buffers=150 pattern=ball background-color=0xff204060 ! $size ! $vp9 ! queue ! mux." \
	"audiotestsrc num-buffers=469 freq=440 volume=0.2 ! $a_fmt ! opusenc ! queue ! mux." \
	"webmmux name=mux streamable=true ! filesink location=mse.webm"
launch "audiotestsrc num-buffers=43 freq=880 volume=0.2 ! audio/x-raw,rate=44100,channels=1,format=S16LE ! wavenc ! filesink location=beep.wav"
# MSE 用的分片 MP4（fMP4：moov 带 mvex，之后每秒一个 moof + mdat；H.264 High + AAC-LC，40 秒：TCG 下画面
# 落后页面十几秒，片段太短时截图赶不上）与 decodeAudioData 用的 M4A（AAC-LC，1 秒）
launch "videotestsrc num-buffers=600 pattern=ball background-color=0xff602040 ! $size ! $h264 ! queue ! mux." \
	"audiotestsrc num-buffers=1875 freq=660 volume=0.2 ! $a_fmt ! fdkaacenc bitrate=128000 ! queue ! mux." \
	"mp4mux name=mux fragment-duration=1000 streamable=true ! filesink location=mse.mp4"
launch "audiotestsrc num-buffers=47 freq=990 volume=0.2 ! audio/x-raw,rate=48000,channels=1 ! audioconvert ! fdkaacenc bitrate=128000 ! mp4mux ! filesink location=beep.m4a"
{
	printf 'window.MEDIA_DATA = {\n  mseWebm: "'
	base64 -w0 mse.webm
	printf '",\n  mseMp4: "'
	base64 -w0 mse.mp4
	printf '",\n  beepWav: "'
	base64 -w0 beep.wav
	printf '",\n  beepM4a: "'
	base64 -w0 beep.m4a
	printf '"\n};\n'
} > media-data.js
rm -f mse.webm mse.mp4 beep.wav beep.m4a
ls -l
