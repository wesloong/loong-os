#!/usr/bin/env python3
"""在 QEMU 中运行 Loong OS 镜像，并通过 QMP 截图、模拟触摸（在开发 chroot 内运行）。

  tools/qemu.py start  <镜像> [--arch x86_64] [--size 1280x800] [--mem 2048]
  tools/qemu.py wait   <正则> [--timeout 秒]        等待串口日志出现某内容
  tools/qemu.py shot   <文件.png>                   截图
  tools/qemu.py tap    <x> <y>                       点按（逻辑坐标 = 屏幕像素）
  tools/qemu.py swipe  <x1> <y1> <x2> <y2> [毫秒]    滑动
  tools/qemu.py stop

运行时文件放在 build/qemu/（QMP socket、串口日志、OVMF 变量）。
"""

import argparse
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time

RUN = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "build", "qemu")
QMP = os.path.join(RUN, "qmp.sock")
SERIAL = os.path.join(RUN, "serial.log")
STATE = os.path.join(RUN, "state.json")


def start(args):
    os.makedirs(RUN, exist_ok=True)
    for f in (QMP, SERIAL):
        if os.path.exists(f):
            os.remove(f)
    w, h = (int(v) for v in args.size.split("x"))
    vars_file = os.path.join(RUN, "OVMF_VARS.fd")
    shutil.copy("/usr/share/OVMF/OVMF_VARS.fd", vars_file)
    cmd = [
        f"qemu-system-{args.arch}",
        "-machine", "q35", "-accel", "tcg,thread=multi", "-cpu", "max",
        "-smp", "4", "-m", str(args.mem),
        "-drive", "if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE.fd",
        "-drive", f"if=pflash,format=raw,file={vars_file}",
        "-drive", f"file={args.image},format=raw,if=virtio",
        "-device", f"virtio-gpu-pci,xres={w},yres={h}",
        "-device", "virtio-tablet-pci",
        "-device", "virtio-keyboard-pci",
        "-display", "none",
        "-serial", f"file:{SERIAL}",
        "-qmp", f"unix:{QMP},server=on,wait=off",
        "-daemonize", "-pidfile", os.path.join(RUN, "qemu.pid"),
    ]
    subprocess.run(cmd, check=True)
    with open(STATE, "w") as f:
        json.dump({"w": w, "h": h}, f)
    print("QEMU 已启动，串口日志:", SERIAL)


class Qmp:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(QMP)
        self.f = self.s.makefile("rw")
        self._read()  # greeting
        self.cmd("qmp_capabilities")

    def _read(self):
        while True:
            msg = json.loads(self.f.readline())
            if "event" not in msg:
                return msg

    def cmd(self, name, **arguments):
        self.f.write(json.dumps({"execute": name, "arguments": arguments}) + "\n")
        self.f.flush()
        r = self._read()
        if "error" in r:
            raise RuntimeError(r["error"])
        return r.get("return")


def screen():
    with open(STATE) as f:
        st = json.load(f)
    return st["w"], st["h"]


def abs_events(x, y):
    w, h = screen()
    return [
        {"type": "abs", "data": {"axis": "x", "value": int(x * 32767 / (w - 1))}},
        {"type": "abs", "data": {"axis": "y", "value": int(y * 32767 / (h - 1))}},
    ]


def btn(down):
    return {"type": "btn", "data": {"down": down, "button": "left"}}


def tap(args):
    q = Qmp()
    q.cmd("input-send-event", events=abs_events(args.x, args.y))
    q.cmd("input-send-event", events=[btn(True)])
    time.sleep(0.06)
    q.cmd("input-send-event", events=[btn(False)])


def swipe(args):
    q = Qmp()
    q.cmd("input-send-event", events=abs_events(args.x1, args.y1))
    q.cmd("input-send-event", events=[btn(True)])
    steps = max(2, int(args.ms / 8))
    for i in range(1, steps + 1):
        t = i / steps
        q.cmd("input-send-event", events=abs_events(args.x1 + (args.x2 - args.x1) * t, args.y1 + (args.y2 - args.y1) * t))
        time.sleep(args.ms / 1000 / steps)
    q.cmd("input-send-event", events=[btn(False)])


def shot(args):
    q = Qmp()
    q.cmd("screendump", filename=os.path.abspath(args.file), format="png")
    print("截图:", args.file)


def wait(args):
    deadline = time.time() + args.timeout
    pat = re.compile(args.pattern)
    while time.time() < deadline:
        if os.path.exists(SERIAL):
            with open(SERIAL, errors="replace") as f:
                if pat.search(f.read()):
                    print("已出现:", args.pattern)
                    return
        time.sleep(1)
    print("超时：未等到", args.pattern, file=sys.stderr)
    sys.exit(1)


def stop(_args):
    try:
        Qmp().cmd("quit")
    except (OSError, RuntimeError):
        pass


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("start")
    s.add_argument("image")
    s.add_argument("--arch", default="x86_64")
    s.add_argument("--size", default="1280x800")
    s.add_argument("--mem", type=int, default=2048)
    s.set_defaults(fn=start)
    s = sub.add_parser("wait")
    s.add_argument("pattern")
    s.add_argument("--timeout", type=int, default=300)
    s.set_defaults(fn=wait)
    s = sub.add_parser("shot")
    s.add_argument("file")
    s.set_defaults(fn=shot)
    s = sub.add_parser("tap")
    s.add_argument("x", type=float)
    s.add_argument("y", type=float)
    s.set_defaults(fn=tap)
    s = sub.add_parser("swipe")
    for a in ("x1", "y1", "x2", "y2"):
        s.add_argument(a, type=float)
    s.add_argument("ms", type=float, nargs="?", default=200)
    s.set_defaults(fn=swipe)
    s = sub.add_parser("stop")
    s.set_defaults(fn=stop)
    args = p.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
