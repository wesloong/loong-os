#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""从 ICU 完整数据包（icudt<版本>l.dat）裁出 Loong OS 用的中英文数据。

用法: build-icu-data.py <完整的 icudt*.dat> <输出 .dat>
需要 ICU 工具（Alpine 的 icu 包）：icupkg、derb、genrb。

保留：
- 语言区域：en（en_001、en_GB、en_US、en_US_POSIX）与 zh（zh_CN、zh_Hans、zh_Hans_CN），
  覆盖主数据（日期、数字格式）以及 curr / lang / region / unit / zone / rbnf / coll / brkitr
  各子树。网页请求 zh-TW、zh-HK 时，JavaScriptCore 按 BCP 47 截断回退到 zh（简体格式）。
- 断行与分词：.brk 规则（含中日文的 line_*_cj），中日文词典 cjdict.dict（中文按词选择、
  Intl.Segmenter）以及分词引擎要读的 brkitr/ja.res；泰、老、高棉、缅文词典不要（这些文字
  回退为按字形簇断开）。
- 排序：root（UCA）与 zh（拼音、笔画等），localeCompare('zh') 按拼音排序。
- 转换器：只留 gb18030（UTF-8、UTF-16、Latin-1 是 ICU 内建的算法转换器，不占数据）。
  WebKit 解码网页的 GBK/GB18030/Big5 等用自己的 TextCodecCJK，不依赖 ICU 转换器。
- 其余与语言无关的数据（归一化、时区、复数规则、单位、Unicode 字符名等）全部保留，
  与 Alpine 的 icu-data-en 一致。
- 不要：音译规则 translit/（1.1 MB；WebKit 和 JavaScriptCore 都不用 ICU 的音译）、其他语言。

各语言区域的数据引用各子树的 pool.res（共享字符串池，完整数据就是这样编译的），所以池要保留；
从源数据用 ICU 数据过滤器重新编译可以去掉池，但要多编一遍 ICU 数据，省下的不到 1 MB。

各子树的 res_index.res（可用的语言区域列表）按实际保留的内容重新生成，
这样 Intl.*.supportedLocalesOf() 不会报告其实没有数据的语言。
"""

import re
import subprocess
import sys
import tempfile
from pathlib import Path

LOCALES = {"en", "en_001", "en_GB", "en_US", "en_US_POSIX", "zh", "zh_CN", "zh_Hans", "zh_Hans_CN"}
TREES = {"", "brkitr", "coll", "curr", "lang", "rbnf", "region", "unit", "zone"}
CONVERTERS = {"gb18030-2022.cnv"}
DICTS = {"cjdict.dict"}
# brkitr/ja.res 必须带上：ICU 的中日文分词引擎（CjkBreakEngine）初始化时要读其中的 extensions，
# 读不到就整个不用词典，中文退化成一整串算一个“词”。
EXTRA = {"brkitr/ja.res"}
# 被引用、但有意不带的项：泰、老、高棉、缅文词典（brkitr/root.res 引用；ICU 找不到词典时按字形簇断开），
# 以及日文按文节断行的规则（brkitr/ja.res 引用，只有 ja@lw=phrase 用到）
ALLOWED_MISSING = {
    "brkitr/thaidict.dict", "brkitr/laodict.dict", "brkitr/khmerdict.dict", "brkitr/burmesedict.dict",
    "brkitr/line_phrase_cj.brk", "brkitr/line_loose_phrase_cj.brk", "brkitr/line_normal_phrase_cj.brk",
}
# 语言区域名：2–3 个小写字母的语言，后接 _脚本 / _地区 / _变体（root、pool、res_index 等不匹配）
LOCALE_RE = re.compile(r"^[a-z]{2,3}(_[A-Za-z0-9]*)*$")


def run(*cmd, cwd=None, check=True) -> str:
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    if check and r.returncode != 0:
        sys.exit(f"{' '.join(cmd)} 失败:\n{r.stdout}{r.stderr}")
    return r.stdout + r.stderr


def keep(item: str) -> bool:
    if item in EXTRA:
        return True
    tree, _, name = item.rpartition("/")
    if tree not in TREES:
        return False
    stem, _, ext = name.rpartition(".")
    if ext == "cnv":
        return name in CONVERTERS
    if ext == "dict":
        return name in DICTS
    if ext == "brk":
        # line_*phrase_cj 只给日文的按文节断行（ja@lw=phrase）用
        return "phrase" not in stem
    if ext == "res" and stem != "res_index" and LOCALE_RE.match(stem):
        return stem in LOCALES
    return True


def regen_res_index(work: Path, tree: str, kept: set[str]) -> None:
    """按保留的语言区域重写 <tree>/res_index.res（derb 反编译 → 过滤 → genrb 编译）。"""
    d = work / tree if tree else work
    present = {Path(i).stem for i in kept if (i.rpartition("/")[0] == tree and i.endswith(".res"))}
    text = run("derb", "-c", "./res_index.res", cwd=d)
    out, section = [], None
    for line in text.splitlines():
        m = re.match(r"^\s*(AliasLocales|InstalledLocales)\s*\{", line)
        if m:
            section = m.group(1)
        elif section and re.match(r"^\s*\}\s*$", line):
            section = None
        elif section:
            loc = re.match(r"^\s*([A-Za-z0-9_]+)\s*\{", line)
            if loc and loc.group(1) not in present:
                continue
        out.append(line)
    # derb 不输出 nofallback 标记；没有它 genrb 会把 “res_index” 当成语言区域名，
    # 给它加上并不存在的父级 “res”
    text = re.sub(r"^res_index\s*\{", "res_index:table(nofallback){", "\n".join(out), count=1, flags=re.M)
    src = d / "res_index.txt"
    src.write_text(text + "\n", encoding="utf-8")
    run("genrb", "-q", "-s", str(d), "-d", str(d), "res_index.txt")
    src.unlink()


def main() -> None:
    full, out = Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve()
    items = [line.strip() for line in run("icupkg", "-l", str(full)).splitlines()
             if line.strip() and not line.startswith("Item ")]
    kept = sorted(i for i in items if keep(i))
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp) / "items"
        work.mkdir()
        lst = Path(tmp) / "keep.txt"
        lst.write_text("\n".join(kept) + "\n")
        run("icupkg", "-x", str(lst), "-d", str(work), str(full))
        for tree in sorted({i.rpartition("/")[0] for i in kept if i.endswith("res_index.res")}):
            regen_res_index(work, tree, set(kept))
        # 包名（目录前缀）取自输出文件名，必须是 icudt<版本>l
        run("icupkg", "-tl", "--ignore-deps", "-a", str(lst), "-s", str(work), "new", str(out))
    # icupkg -l 会报告每个被引用、但包里没有的项（此时退出码非 0）
    log = run("icupkg", "-l", str(out), check=False)
    missing = {m for m in re.findall(r"depends on missing item (\S+)", log)}
    # icupkg 用 --ignore-deps 只是放过预期内的缺失；其他缺失说明过滤规则漏了东西
    unexpected = missing - ALLOWED_MISSING
    if unexpected:
        sys.exit("缺少被引用的数据: " + " ".join(sorted(unexpected)))
    print(f"{out}: {out.stat().st_size / 1048576:.2f} MiB，{len(kept)}/{len(items)} 项，"
          f"有意不带: {' '.join(sorted(missing)) or '无'}")


if __name__ == "__main__":
    main()
