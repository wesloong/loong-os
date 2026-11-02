#!/usr/bin/env python3
"""在 QEMU 中测量开机与启动应用的耗时（在开发 chroot 内运行，借用 tools/qemu.py）。

  tools/qemu-bench.py <镜像> [--runs N] [--app 设置=837,732] [--out 目录] [--arch x86_64]

每一轮：启动虚拟机 → 锁屏出现 → 上滑解锁 → 点 Dock 上的应用 → 应用画面稳定 → 关机。
计时靠截图：每 0.25 秒截一次屏（缩成 160×100 的亮度图），画面停止变化后，回头找第一张
与最终画面几乎相同的截图，它的时刻就是“画面就绪”的时刻。锁屏要求画面是亮的（跳过黑屏的
固件与内核阶段），应用要求最终画面不是打开时的启动画面（图标 + 宣纸底）。

  开机到锁屏   从 QEMU 启动（含 UEFI 固件、内核、initramfs、OpenRC、loong-shell）到锁屏画面就绪
  启动应用     从点按图标到应用画面就绪（第一次启动：文件都还没进页缓存，读只读根的开销都算在内）

沙箱里是 TCG 软件模拟，绝对值比真机慢很多，只适合比较同一台机器上的不同镜像（例如不同的压缩参数）。
多个代理共用机器时，整个过程要包在 flock /var/lock/loong-qemu 里。
"""

import argparse
import json
import os
import statistics
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import qemu  # noqa: E402

GRID_W, GRID_H = 160, 100


def signature(q, path):
    """截图并缩成 GRID_W×GRID_H 的亮度（取绿色通道）列表。"""
    q.cmd("screendump", filename=path, format="ppm")
    with open(path, "rb") as f:
        data = f.read()
    # P6 头：P6\n<w> <h>\n<max>\n
    parts = data.split(b"\n", 3)
    w, h = (int(v) for v in parts[1].split())
    px = parts[3]
    sx, sy = w // GRID_W, h // GRID_H
    sig = []
    for gy in range(GRID_H):
        row = px[(gy * sy) * w * 3 : (gy * sy + 1) * w * 3]
        sig.extend(row[1 : GRID_W * sx * 3 : sx * 3])
    return sig


def tap(q, x, y):
    q.cmd("input-send-event", events=qemu.abs_events(x, y))
    q.cmd("input-send-event", events=[qemu.btn(True)])
    time.sleep(0.06)
    q.cmd("input-send-event", events=[qemu.btn(False)])


def swipe(q, x1, y1, x2, y2, ms=200):
    # QMP 同一时间只接受一个连接：用同一个连接发输入事件（不调用 qemu.swipe）
    q.cmd("input-send-event", events=qemu.abs_events(x1, y1))
    q.cmd("input-send-event", events=[qemu.btn(True)])
    steps = max(2, int(ms / 8))
    for i in range(1, steps + 1):
        t = i / steps
        q.cmd("input-send-event", events=qemu.abs_events(x1 + (x2 - x1) * t, y1 + (y2 - y1) * t))
        time.sleep(ms / 1000 / steps)
    q.cmd("input-send-event", events=[qemu.btn(False)])


def diff(a, b):
    return sum(abs(x - y) for x, y in zip(a, b)) / len(a)


def is_splash(sig):
    """应用还没画出内容：启动画面（纯色宣纸底，中间一个放大的图标）或空白的窗口。
    除去状态栏、底部横条和中间的图标，其余都是同一种颜色。"""
    vals = [
        sig[gy * GRID_W + gx]
        for gy in range(8, 95)
        for gx in range(GRID_W)
        if not (55 <= gx <= 105 and 30 <= gy <= 70)
    ]
    return statistics.pstdev(vals) < 4


def wait_ready(q, tmp, t0, timeout, stable_s, bright=None, differ_from=None):
    """截图直到画面连续 stable_s 秒不变，返回第一张与最终画面相同的截图时刻。
    bright：要求平均亮度大于它（跳过黑屏）；differ_from：最终画面必须与它不同（跳过应用的启动画面）。"""
    frames = []
    deadline = t0 + timeout
    while time.time() < deadline:
        t = time.time()
        s = signature(q, tmp)
        frames.append((t, s))
        last = frames[-1][1]
        if (bright is not None and sum(last) / len(last) < bright) or (
            differ_from is not None and diff(last, differ_from) < 1.0
        ):
            time.sleep(0.25)
            continue
        # 最近 stable_s 秒的截图都与最后一张相同
        recent = [f for f in frames if f[0] >= t - stable_s]
        if frames[0][0] <= t - stable_s and all(diff(f[1], last) < 1.0 for f in recent):
            for ft, fs in frames:
                if diff(fs, last) < 1.0 and (bright is None or sum(fs) / len(fs) >= bright):
                    return ft - t0, last
        time.sleep(0.25)
    raise TimeoutError("画面没有稳定下来")


def run_once(args, idx):
    tmp = os.path.join(qemu.RUN, "bench.ppm")
    qemu.stop(None)
    time.sleep(1)
    # 每一轮都是“第一次开机”：在写时复制的覆盖层上运行，原镜像不变
    overlay = os.path.join(qemu.RUN, "bench.qcow2")
    if os.path.exists(overlay):
        os.remove(overlay)
    fmt = "qcow2" if args.image.endswith(".qcow2") else "raw"
    subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-b", os.path.abspath(args.image), "-F", fmt, overlay], check=True)
    t0 = time.time()
    qemu.start(argparse.Namespace(image=overlay, arch=args.arch, size="1280x800", mem=args.mem, smp=args.smp, disk=[]))
    q = None
    while q is None:
        try:
            q = qemu.Qmp()
        except OSError:
            if time.time() > t0 + 30:
                raise
            time.sleep(0.1)
    result = {}
    # 锁屏是浅色宣纸（夜墨主题时请改阈值）；UEFI 与内核控制台阶段是黑屏
    result["boot_to_lock_s"], _ = wait_ready(q, tmp, t0, 600, 4.0, bright=100)
    q.cmd("screendump", filename=os.path.join(args.out, f"run{idx}-lock.png"), format="png")
    # 刚开机时系统还忙（TCG 下事件处理慢），一次上滑不一定解锁：滑完看画面是否变了，没变就再滑
    time.sleep(5)
    lock = signature(q, tmp)
    for _ in range(4):
        swipe(q, 640, 790, 640, 150, ms=600)
        _, home = wait_ready(q, tmp, time.time(), 120, 2.0)
        if diff(home, lock) > 5:
            break
    else:
        raise RuntimeError("上滑没有解锁")
    for name, (x, y) in args.app:
        # 点按后画面没有变化（TCG 下刚解锁时偶尔丢掉点按），就再点一次；从最后一次点按开始计时
        time.sleep(3)
        for _ in range(4):
            t1 = time.time()
            tap(q, x, y)
            while time.time() < t1 + 8 and diff(signature(q, tmp), home) < 1.0:
                time.sleep(0.2)
            if time.time() < t1 + 8:
                break
        else:
            raise RuntimeError(f"点按 {name} 没有反应")
        # 先等打开动画结束、画面停住；停住的若是启动画面（图标 + 宣纸底），再等网页内容出来
        # （WebKit 的窗口出来后、网页画出来之前还有一段空白的纸色画面，同样不算）
        ready, last = wait_ready(q, tmp, t1, 300, 1.5)
        for _ in range(5):
            if not is_splash(last):
                break
            ready, last = wait_ready(q, tmp, t1, 300, 4.0, differ_from=last)
        result[f"launch_{name}_s"] = ready
        q.cmd("screendump", filename=os.path.join(args.out, f"run{idx}-{name}.png"), format="png")
        # 回主屏，再启动下一个
        for _ in range(4):
            swipe(q, 640, 790, 640, 250, ms=600)
            _, now = wait_ready(q, tmp, time.time(), 120, 2.0)
            if diff(now, home) < 3:
                break
    q.cmd("quit")
    time.sleep(2)
    os.remove(overlay)
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("image")
    p.add_argument("--runs", type=int, default=3)
    p.add_argument("--app", action="append", default=[], help="名称=x,y（Dock 图标坐标），可多次；默认 设置=837,732")
    p.add_argument("--out", default="out/bench")
    p.add_argument("--arch", default="x86_64")
    p.add_argument("--mem", type=int, default=2048)
    p.add_argument("--smp", type=int, default=4)
    args = p.parse_args()
    apps = args.app or ["设置=837,732"]
    args.app = [(a.split("=")[0], tuple(float(v) for v in a.split("=")[1].split(","))) for a in apps]
    # QEMU 以 -daemonize 运行，工作目录是 /：截图路径必须是绝对路径
    args.out = os.path.abspath(args.out)
    os.makedirs(args.out, exist_ok=True)
    results = []
    for i in range(args.runs):
        r = run_once(args, i)
        print(json.dumps(r, ensure_ascii=False), flush=True)
        results.append(r)
    summary = {k: round(statistics.median(r[k] for r in results), 1) for k in results[0]}
    print("中位数:", json.dumps(summary, ensure_ascii=False))
    with open(os.path.join(args.out, "bench.json"), "w") as f:
        json.dump({"image": args.image, "runs": results, "median": summary}, f, ensure_ascii=False, indent=1)


if __name__ == "__main__":
    main()
