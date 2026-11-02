#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""把思源黑体的简体中文可变字体（Noto Sans SC VF，CFF2）裁成 Loong OS 用的子集。

字体本身为 SIL OFL 1.1（见上游 LICENSE）；本脚本只做裁剪和改名。

用法: build-font.py <NotoSansSC-VF.otf> <Unihan_OtherMappings.txt> <输出 .otf>

- 字符集：GB2312 全部字符（6763 个汉字 + 682 个符号）∪《通用规范汉字表》8105 字
  （Unihan 的 kTGH 字段）∪ 拉丁、标点与常用符号（见 RANGES）。约 1.07 万个字形。
- 字重：可变字体只保留 400–700（常规到粗体，默认 400）；界面和网页用到的 400/500/600/700
  都是真实字重，不靠合成加粗。比两个静态字重（常规 + 粗体）还小。
- 改名为 “Noto Sans CJK SC”：与 Alpine 的 font-noto-cjk 同名，loong-ui（cosmic-text 按
  这个名字回退中文）、LoongKit 的字体栈和 fontconfig 的默认规则都不用改。
"""

import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

FAMILY = "Noto Sans CJK SC"
PS_FAMILY = "NotoSansCJKSC"
SRC_FAMILY = "Noto Sans SC"
SRC_PS_FAMILY = "NotoSansSC"

# 汉字以外收录的区段（闭区间）
RANGES = [
    (0x0020, 0x007E),  # 基本拉丁
    (0x00A0, 0x017F),  # 拉丁补充、扩展 A
    (0x01CD, 0x01DC),  # 拼音声调字母（ǎ ǐ ǒ ǔ ǖ ǘ ǚ ǜ）
    (0x02B0, 0x02FF),  # 修饰字母（ˇ ˉ ˊ ˋ ˙）
    (0x0370, 0x03FF),  # 希腊字母
    (0x0400, 0x045F),  # 西里尔字母
    (0x2000, 0x206F),  # 通用标点（— … ‘ ’ “ ” ‰ ※）
    (0x2070, 0x209F),  # 上标、下标
    (0x20A0, 0x20CF),  # 货币符号
    (0x2100, 0x218F),  # 类字母符号（℃ № ™）、数字形式（Ⅰ–Ⅻ）
    (0x2190, 0x21FF),  # 箭头
    (0x2200, 0x22FF),  # 数学运算符
    (0x2460, 0x24FF),  # 带圈字母数字（① ⑴ ⒈）
    (0x2500, 0x257F),  # 制表符
    (0x25A0, 0x25FF),  # 几何图形（■ □ ▲ ○ ●）
    (0x2600, 0x26FF),  # 杂项符号（★ ☆）
    (0x2E80, 0x2EFF),  # 中日韩部首补充
    (0x2F00, 0x2FDF),  # 康熙部首（PDF 复制出的文字里常见，外形与汉字相同）
    (0x3000, 0x303F),  # 中日韩符号和标点（、。〈〉《》「」【】〔〕）
    (0x3040, 0x30FF),  # 平假名、片假名（GB2312 也收录）
    (0x3100, 0x312F),  # 注音符号
    (0x31A0, 0x31BF),  # 注音符号扩展
    (0x3200, 0x32FF),  # 带圈中日韩字母和月份（㈠ ㊣）
    (0x3300, 0x33FF),  # 中日韩兼容（㎡ ㎏）
    (0xFE10, 0xFE1F),  # 竖排标点
    (0xFE30, 0xFE4F),  # 中日韩兼容形式
    (0xFE50, 0xFE6F),  # 小写变体
    (0xFF00, 0xFFEF),  # 半角及全角字符
]


def gb2312() -> set[int]:
    """GB2312 的全部字符（一、二级汉字 6763 个，以及全角符号、假名、希腊与俄文字母等）。"""
    cps: set[int] = set()
    for hi in range(0xA1, 0xF8):
        for lo in range(0xA1, 0xFF):
            try:
                cps.add(ord(bytes([hi, lo]).decode("gb2312")))
            except UnicodeDecodeError:
                continue
    return cps


def tgh(other_mappings: Path) -> set[int]:
    """《通用规范汉字表》（2013）的 8105 个字：Unihan 的 kTGH 字段。"""
    cps: set[int] = set()
    for line in other_mappings.read_text(encoding="utf-8").splitlines():
        fields = line.split("\t")
        if len(fields) >= 3 and fields[1] == "kTGH":
            cps.add(int(fields[0][2:], 16))
    if len(cps) != 8105:
        sys.exit(f"kTGH 应有 8105 个字，实际 {len(cps)} 个")
    return cps


def rename(font: TTFont) -> None:
    name = font["name"]
    for rec in name.names:
        s = rec.toUnicode()
        t = s.replace(SRC_FAMILY, FAMILY).replace(SRC_PS_FAMILY, PS_FAMILY)
        if rec.nameID == 5:
            t = s + "; Loong OS subset"
        if t != s:
            rec.string = t
    # 默认实例现在是常规体（原字体的默认实例是 Thin）
    for plat, enc, lang in ((3, 1, 0x409), (3, 1, 0x804)):
        name.setName("Regular", 2, plat, enc, lang)
        name.setName(FAMILY, 4, plat, enc, lang)
    name.setName(f"{PS_FAMILY}-Regular", 6, 3, 1, 0x409)
    name.setName(f"2.004;ADBO;{PS_FAMILY}-Regular;Loong", 3, 3, 1, 0x409)
    # 16/17（排版家族名/子家族名）与 1/2 相同时不需要
    name.names = [r for r in name.names if r.nameID not in (16, 17)]
    font["OS/2"].usWeightClass = 400
    font["OS/2"].fsSelection = (font["OS/2"].fsSelection & ~0b1100001) | 0b1000000
    font["head"].macStyle = 0


def main() -> None:
    src, mappings, out = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
    cps = gb2312() | tgh(mappings)
    for a, b in RANGES:
        cps.update(range(a, b + 1))

    font = TTFont(src)
    # 字重轴裁到 400–700：原默认值 100 落在范围外，默认实例随之变成 400。
    # 先裁轴再取子集（反过来时 fontTools 在去掉提示的 CFF2 上裁轴会出错）
    font = instancer.instantiateVariableFont(font, {"wght": (400, 700)})
    opts = subset.Options()
    # 版式特性用 fontTools 的默认集合（含竖排 vert/vrt2、标点挤压 halt/chws、locl）
    opts.name_IDs = ["*"]
    opts.name_languages = ["*"]
    opts.notdef_outline = True
    opts.glyph_names = False
    opts.drop_tables += ["DSIG"]
    sub = subset.Subsetter(opts)
    sub.populate(unicodes=sorted(cps))
    sub.subset(font)
    rename(font)
    font.save(out)

    check = TTFont(out)
    cmap = check.getBestCmap()
    han = [c for c in cps if 0x3400 <= c <= 0x9FFF or c >= 0x20000]
    missing = sorted(c for c in han if c not in cmap)
    axes = [(a.axisTag, a.minValue, a.defaultValue, a.maxValue) for a in check["fvar"].axes]
    print(
        f"{out}: {out.stat().st_size / 1048576:.2f} MiB, {len(cmap)} 个码位, "
        f"{check['maxp'].numGlyphs} 个字形, 汉字 {len(han) - len(missing)}/{len(han)}, 轴 {axes}"
    )
    if missing:
        print("源字体缺少的汉字: " + "".join(chr(c) for c in missing))


if __name__ == "__main__":
    main()
