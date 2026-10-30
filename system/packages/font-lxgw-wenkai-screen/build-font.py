#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""把 lxgw-wenkai-screen-webfont（npm 包）按 unicode-range 切分的 woff2 子集
重新拼回一个 TTF，并裁剪到系统需要的字符集。

字体本身为 SIL OFL 1.1（见包内 OFL.txt）；本脚本只做格式转换与裁剪。

用法: build-font.py <npm 包解压目录 package/> <输出 .ttf>

字符集：webfont 收录的全部码位（约 1.4 万个：1.2 万个常用汉字，覆盖 GB2312
除 33 个罕用字外的全部汉字；拉丁字母、数字、标点、全角与中日韩符号）。
缺的字由 fontconfig 回退到思源黑体。
"""

import re
import sys
from pathlib import Path

from fontTools import subset
from fontTools.merge import Merger, Options as MergeOptions
from fontTools.ttLib import TTFont

FAMILY = "LXGW WenKai Screen"
STYLE = "Regular"
PS_NAME = "LXGWWenKaiScreen-Regular"


def subset_files(pkg: Path) -> list[Path]:
    """按 CSS 中出现的顺序列出 Regular（LXGW WenKai Screen，非 R 版）的子集文件。"""
    css = (pkg / "lxgwwenkaiscreen.css").read_text(encoding="utf-8")
    names = re.findall(r"url\('\./files/(lxgwwenkaiscreen-subset-\d+\.woff2)'\)", css)
    if not names:
        sys.exit("CSS 中没有找到子集文件")
    seen = []
    for n in names:
        if n not in seen:
            seen.append(n)
    return [pkg / "files" / n for n in seen]


def gb2312_hanzi() -> set[int]:
    """GB2312 一、二级汉字（6763 个），用于检查覆盖率。"""
    cps: set[int] = set()
    for hi in range(0xB0, 0xF8):
        for lo in range(0xA1, 0xFF):
            try:
                cps.add(ord(bytes([hi, lo]).decode("gb2312")))
            except UnicodeDecodeError:
                continue
    return cps


def set_names(font: TTFont) -> None:
    name = font["name"]
    keep = []
    for rec in name.names:
        if rec.nameID in (0, 7, 8, 9, 11, 12, 13, 14):
            keep.append(rec)
    name.names = keep
    for platform, enc, lang in ((3, 1, 0x409), (1, 0, 0)):
        name.setName(FAMILY, 1, platform, enc, lang)
        name.setName(STYLE, 2, platform, enc, lang)
        name.setName(f"{FAMILY} {STYLE}", 4, platform, enc, lang)
        name.setName(PS_NAME, 6, platform, enc, lang)
        name.setName(f"{FAMILY}: {STYLE}", 3, platform, enc, lang)
        name.setName("Version 1.250.2 (lxgw-wenkai-screen-webfont 1.7.0)", 5, platform, enc, lang)
    name.setName("霞鹜文楷 屏幕阅读版", 1, 3, 1, 0x804)
    name.setName("常规", 2, 3, 1, 0x804)
    font["OS/2"].usWeightClass = 400
    font["OS/2"].fsSelection = (font["OS/2"].fsSelection & ~0b1100001) | 0b1000000
    font["head"].macStyle = 0


def main() -> None:
    pkg, out = Path(sys.argv[1]), Path(sys.argv[2])
    files = subset_files(pkg)
    # 子集是同一字体按码位切出来的，表结构一致，可以直接合并。
    # woff2 → 内存中的 TTF（Merger 需要文件路径或 TTFont，这里先统一转成 sfnt）。
    work = out.parent / (out.stem + ".parts")
    work.mkdir(parents=True, exist_ok=True)
    parts = []
    for f in files:
        font = TTFont(f)
        font.flavor = None
        # 竖排表在合并器中没有对应逻辑，界面只用横排，去掉。
        for tag in ("vhea", "vmtx", "VORG"):
            if tag in font:
                del font[tag]
        p = work / (f.stem + ".ttf")
        font.save(p)
        parts.append(str(p))
    opts = MergeOptions(drop_tables=["vhea", "vmtx", "VORG", "DSIG"])
    merged = Merger(options=opts).merge(parts)

    opts = subset.Options()
    opts.layout_features = ["*"]
    opts.name_IDs = ["*"]
    opts.name_languages = ["*"]
    opts.notdef_outline = True
    opts.glyph_names = False
    opts.hinting = False
    opts.drop_tables += ["vhea", "vmtx", "VORG", "DSIG"]
    sub = subset.Subsetter(opts)
    # 收录 webfont 中的全部码位（约 1.4 万个，含 1.2 万汉字），合并后的重复字形由裁剪去掉。
    sub.populate(unicodes=sorted(merged.getBestCmap().keys()))
    sub.subset(merged)
    set_names(merged)
    merged.flavor = None
    merged.save(out)
    for p in parts:
        Path(p).unlink()
    work.rmdir()

    check = TTFont(out)
    cmap = check.getBestCmap()
    missing_gb = sorted(c for c in gb2312_hanzi() if c not in cmap)
    print(
        f"{out}: {out.stat().st_size / 1e6:.1f} MB, {len(cmap)} 个码位, "
        f"{check['maxp'].numGlyphs} 个字形, GB2312 汉字缺 {len(missing_gb)} 个: "
        + "".join(chr(c) for c in missing_gb)
    )


if __name__ == "__main__":
    main()
