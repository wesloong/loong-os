#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""把 Inter 的可变字体改成 Loong OS 用的样子。

字体本身为 SIL OFL 1.1（见 Alpine font-inter 包里的许可证）；本脚本只做裁轴和改名。

用法: build-font.py <InterVariable.ttf> <输出 .ttf>

- 族名从 “Inter Variable” 改成 “Inter”：loong-ui（cosmic-text 只认字体里的名字）和 LoongKit 的
  字体栈都请求 “Inter”。Alpine 的 font-inter 里叫 “Inter” 的只有 11 MB 的静态字体合集
  Inter.ttc；只装可变字体时 “Inter” 匹配不到，拉丁字母实际用的是思源黑体里的。
- 光学尺寸轴（opsz 14–32）固定在 14（正文）：860 KB → 620 KB。字重轴 100–900 保留
  （网页常用 300 等细字重）。
"""

import sys
from pathlib import Path

from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

SRC_FAMILY = "Inter Variable"
FAMILY = "Inter"


def main() -> None:
    src, out = Path(sys.argv[1]), Path(sys.argv[2])
    font = instancer.instantiateVariableFont(TTFont(src), {"opsz": 14})
    for rec in font["name"].names:
        s = rec.toUnicode()
        if rec.nameID in (1, 4, 16) and SRC_FAMILY in s:
            rec.string = s.replace(SRC_FAMILY, FAMILY)
        elif rec.nameID == 5:
            rec.string = s + "; Loong OS (opsz 14)"
    font.save(out)

    check = TTFont(out)
    names = {r.nameID: r.toUnicode() for r in check["name"].names if r.platformID == 3}
    axes = [(a.axisTag, a.minValue, a.defaultValue, a.maxValue) for a in check["fvar"].axes]
    print(f"{out}: {out.stat().st_size / 1024:.0f} KiB, 族名 {names.get(1)!r}, 轴 {axes}")
    if names.get(1) != FAMILY:
        sys.exit("改名失败")


if __name__ == "__main__":
    main()
