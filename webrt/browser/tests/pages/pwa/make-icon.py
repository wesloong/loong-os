#!/usr/bin/env python3
"""生成测试 PWA「墨记」的 maskable 图标（纯 Python，不依赖第三方库）。

    python3 make-icon.py icon-192.png 192

满版石绿底（maskable：内容在中心 80% 的安全区内），月白的一轮月与两重远山。4× 超采样抗锯齿。
"""
import math
import struct
import sys
import zlib


def png(path, w, h, rows):
    def chunk(kind, data):
        c = struct.pack(">I", len(data)) + kind + data
        return c + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + bytes(r) for r in rows)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(raw, 9)))
        f.write(chunk(b"IEND", b""))


def mix(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


def color_at(x, y):
    """x, y ∈ [0, 1)：这一点的颜色（不透明）。"""
    top, bottom = (0x2B, 0x63, 0x52), (0x3F, 0x7D, 0x68)
    c = mix(top, bottom, y)
    moon = (0xF3, 0xEE, 0xE2)
    if math.hypot(x - 0.64, y - 0.34) < 0.13:
        c = moon
    # 远山：两道正弦起伏的山脊，下面是山体。
    far = 0.62 + 0.06 * math.sin(x * 7.0 + 1.0) + 0.03 * math.sin(x * 17.0)
    near = 0.72 + 0.08 * math.sin(x * 5.0 + 3.4) + 0.025 * math.sin(x * 23.0 + 1.0)
    if y > far:
        c = mix(c, moon, 0.45)
    if y > near:
        c = mix(c, moon, 0.85)
    return c


def main():
    path, size = sys.argv[1], int(sys.argv[2])
    ss = 4
    rows = []
    for py in range(size):
        row = []
        for px in range(size):
            acc = [0.0, 0.0, 0.0]
            for sy in range(ss):
                for sx in range(ss):
                    c = color_at((px + (sx + 0.5) / ss) / size, (py + (sy + 0.5) / ss) / size)
                    for i in range(3):
                        acc[i] += c[i]
            row += [round(v / (ss * ss)) for v in acc] + [255]
        rows.append(row)
    png(path, size, size, rows)


if __name__ == "__main__":
    main()
