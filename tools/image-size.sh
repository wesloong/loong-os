#!/bin/sh
# 系统镜像体积报告：rootfs 里每个 apk 包、每个目录占多少，最大的文件，只读根压缩后的大小。
# 在开发 chroot 内、mkimage.sh 之后运行：
#
#   tools/dev-chroot.sh run tools/image-size.sh qemu-x86_64
#   tools/dev-chroot.sh run tools/image-size.sh qemu-x86_64 --top 40 --compressed 15
#
# 选项：
#   --top N          每个表列出前 N 项（默认 25）
#   --compressed N   为最大的 N 个包各生成一个只读根（参数与 mkimage 相同），估算压缩后的大小。
#                    单独压缩没有跨包去重，估算值略偏大；所有包加起来也不等于镜像大小。
#   --root DIR       直接分析一个 rootfs 目录（默认 build/image/<机型>/rootfs）
#   --opts="参数"    估算压缩后大小时改用这组 mkfs 参数（默认用镜像自己的，见 work/rootfs.env）
#   --only 包,包     只估算这些包（与 --compressed 一起用）
#
# 读取的文件：build/image/<机型>/rootfs（含 /lib/apk/db/installed）、work/root.<fs>，
# 以及 mkimage 记下的压缩参数 work/rootfs.env。大小一律按文件内容的字节数（硬链接只算一次），
# 不按磁盘块；“MiB”为 1048576 字节。

set -eu
PROFILE=${1:?用法: tools/image-size.sh <机型> [--top N] [--compressed N] [--root DIR]}
shift
SRC=${SRC:-$(cd "$(dirname "$0")/.." && pwd)}
exec python3 -I - "$SRC" "$PROFILE" "$@" <<'PY'
import argparse
import os
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile

src, profile = sys.argv[1], sys.argv[2]
ap = argparse.ArgumentParser(prog="image-size.sh")
ap.add_argument("--top", type=int, default=25)
ap.add_argument("--compressed", type=int, default=0)
ap.add_argument("--root")
ap.add_argument("--opts")
ap.add_argument("--only", default="")
args = ap.parse_args(sys.argv[3:])

out = os.path.join(src, "build", "image", profile)
root = os.path.abspath(args.root or os.path.join(out, "rootfs"))
work = os.path.join(out, "work")
if not os.path.isdir(root):
    sys.exit(f"没有 {root}：先运行 system/mkimage/mkimage.sh {profile}")
MiB = 1048576


def mib(n):
    return f"{n / MiB:8.2f}"


# ── 遍历 rootfs：每个普通文件的大小（硬链接按 inode 只算一次）──
files = {}  # 相对路径 → 大小（重复的硬链接记 0）
seen_inodes = set()
for dirpath, dirnames, filenames in os.walk(root):
    for name in filenames:
        p = os.path.join(dirpath, name)
        st = os.lstat(p)
        if not stat.S_ISREG(st.st_mode):
            continue
        rel = os.path.relpath(p, root)
        key = (st.st_dev, st.st_ino)
        files[rel] = 0 if key in seen_inodes else st.st_size
        seen_inodes.add(key)
total = sum(files.values())

# ── apk 数据库：文件归属 ──
# mkimage 把 /var 移到了 /usr/share/loong/var-skel（首次启动时复制到 data 分区）
def on_disk(path):
    if path == "var" or path.startswith("var/"):
        return "usr/share/loong/var-skel" + path[3:]
    return path


pkgs = {}  # 包名 → {ver, declared, files}
owner = {}
db = os.path.join(root, "lib/apk/db/installed")
if os.path.exists(db):
    cur = None
    d = ""
    with open(db, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\n")
            if line.startswith("P:"):
                cur = {"name": line[2:], "ver": "", "declared": 0, "files": []}
                pkgs[cur["name"]] = cur
            elif cur is None:
                continue
            elif line.startswith("V:"):
                cur["ver"] = line[2:]
            elif line.startswith("I:"):
                cur["declared"] = int(line[2:])
            elif line.startswith("F:"):
                d = line[2:]
            elif line.startswith("R:"):
                rel = on_disk(f"{d}/{line[2:]}" if d else line[2:])
                cur["files"].append(rel)
                owner.setdefault(rel, cur["name"])
for p in pkgs.values():
    p["size"] = sum(files.get(r, 0) for r in p["files"] if owner.get(r) == p["name"])
    p["present"] = sum(1 for r in p["files"] if r in files)
unowned = {r: s for r, s in files.items() if r not in owner}

# ── 只读根镜像 ──
env = {}
env_file = os.path.join(work, "rootfs.env")
if os.path.exists(env_file):
    with open(env_file) as f:
        for line in f:
            k, _, v = line.strip().partition("=")
            if k:
                env[k] = shlex.split(v)[0] if v else ""
fs = env.get("ROOT_FS", "erofs")
img = os.path.join(work, f"root.{fs}")
img_size = os.path.getsize(img) if os.path.exists(img) else None

print(f"# {profile} 的 rootfs：{root}")
print(f"文件内容合计 {total / MiB:.1f} MiB，{len(files)} 个文件，{len(pkgs)} 个 apk 包")
if img_size is not None:
    opts = env.get("ROOT_FS_OPTS", "")
    print(f"只读根 {img}（{fs} {opts}）：{img_size / MiB:.1f} MiB，压缩比 {img_size / total:.1%}")
else:
    print("（还没有只读根镜像：mkimage 没有跑完？）")

# ── 按软件包 ──
print(f"\n## 按 apk 包（rootfs 中实际存在的文件；声明 = 包的 installed-size；前 {args.top} 个）")
print(f"{'MiB':>8} {'声明MiB':>8} {'占比':>6}  包")
ranked = sorted(pkgs.values(), key=lambda p: -p["size"])
for p in ranked[: args.top]:
    print(f"{mib(p['size'])} {mib(p['declared'])} {p['size'] / total:6.1%}  {p['name']}-{p['ver']}")
rest = ranked[args.top :]
print(f"{mib(sum(p['size'] for p in rest))} {'':8} {sum(p['size'] for p in rest) / total:6.1%}  其余 {len(rest)} 个包")
print(f"{mib(sum(unowned.values()))} {'':8} {sum(unowned.values()) / total:6.1%}  不属于任何包（Loong OS 组件、构建时生成的文件）")

# ── 不属于任何包的文件，按目录汇总 ──
print("\n## 不属于任何包的文件（按前两级目录）")
groups = {}
for r, s in unowned.items():
    parts = r.split("/")
    k = "/".join(parts[:3]) if parts[0] == "usr" else "/".join(parts[:2])
    groups[k] = groups.get(k, 0) + s
for k, s in sorted(groups.items(), key=lambda kv: -kv[1])[: args.top]:
    if s:
        print(f"{mib(s)}  /{k}")

# ── 按目录 ──
print(f"\n## 按目录（前 {args.top} 个；/usr/lib、/usr/share 展开到第三级）")
dirs = {}
for r, s in files.items():
    parts = r.split("/")
    depth = 3 if parts[:2] in (["usr", "lib"], ["usr", "share"]) else 2
    k = "/".join(parts[: min(depth, len(parts) - 1)]) or "."
    dirs[k] = dirs.get(k, 0) + s
for k, s in sorted(dirs.items(), key=lambda kv: -kv[1])[: args.top]:
    print(f"{mib(s)}  /{k}")

# ── 最大的文件 ──
print(f"\n## 最大的文件（前 {args.top} 个）")
for r, s in sorted(files.items(), key=lambda kv: -kv[1])[: args.top]:
    print(f"{mib(s)}  /{r}  [{owner.get(r, '-')}]")

# ── 每个包单独压缩的估算 ──
if args.compressed:
    opts = args.opts or env.get("ROOT_FS_OPTS", "-zlz4hc,12" if fs == "erofs" else "-comp lz4 -Xhc")
    print(f"\n## 压缩后估算（每个包单独生成 {fs} {opts}，前 {args.compressed} 个）")
    print(f"{'原MiB':>8} {'压缩MiB':>8} {'比例':>6}  包")
    tmp = tempfile.mkdtemp(prefix="size-", dir=out)
    try:
        only = [n for n in args.only.split(",") if n]
        for p in [q for q in ranked if q["name"] in only] if only else ranked[: args.compressed]:
            stage = os.path.join(tmp, "stage")
            for r in p["files"]:
                if owner.get(r) != p["name"] or r not in files:
                    continue
                dst = os.path.join(stage, r)
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                os.link(os.path.join(root, r), dst)
            os.makedirs(stage, exist_ok=True)
            pimg = os.path.join(tmp, "p.img")
            if fs == "erofs":
                cmd = ["mkfs.erofs", *shlex.split(opts), "-T0", "--quiet", pimg, stage]
            else:
                cmd = ["mksquashfs", stage, pimg, "-noappend", "-quiet", "-no-progress", *shlex.split(opts)]
            subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)
            c = os.path.getsize(pimg)
            print(f"{mib(p['size'])} {mib(c)} {c / max(p['size'], 1):6.1%}  {p['name']}")
            shutil.rmtree(stage)
            os.unlink(pimg)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
PY
