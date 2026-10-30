#!/usr/bin/env python3
"""LoongKit「墨韵」程序纹理：生成内联 SVG（feTurbulence 噪声）并写回 loongkit.css。

    python3 tools/loongkit-textures.py           # 改写 webrt/loongkit/loongkit.css 中的纹理令牌
    python3 tools/loongkit-textures.py --check   # 只检查是否最新（CI 用），过期时返回 1

纹理都是静态图（mask 或背景），浏览器栅格化一次后缓存，动画只动 transform / opacity。
调参后用 tools/web-shot.mjs 截图检查，亮色与暗色都要看。

令牌（行首缩进与名字不变，只替换 url(...)）：
  --loong-paper-texture  宣纸：云絮（低频明暗）+ 弯曲的纤维 + 细颗粒（暗色不加纤维）
  --loong-sheet-texture  素笺：只有云絮与颗粒，更干净，用于卡片
  --loong-silk-texture   绢：经纬两向的细丝，用于导航栏等半透明材质
  --loong-seal-texture   印泥：零星透白的颗粒 + 大块的轻重不匀（mask）
  --loong-brush-stroke   一笔横画：圆钝起笔、毛边、收笔飞白（mask，拉伸使用）
  --loong-ink-drop       点墨：中心略淡、边缘积墨、外缘被纤维扯出毛边（mask）
"""
import re
import sys
from pathlib import Path

CSS = Path(__file__).resolve().parent.parent / "webrt/loongkit/loongkit.css"


def enc(svg: str) -> str:
    s = re.sub(r"\n\s*", "", svg.strip()).replace("#", "%23")
    return f'url("data:image/svg+xml,{s}")'


def paper(light: bool, cloud: float, fibers: float, grain: float, fibers2: float = 0.0, size: int = 320) -> str:
    """纸面纹理：各层都是可平铺（stitchTiles）的噪声，按不透明度叠加。"""
    ink = "0 0 0 0 .36  0 0 0 0 .28  0 0 0 0 .17" if light else "0 0 0 0 .9  0 0 0 0 .85  0 0 0 0 .72"
    fib = "0 0 0 0 1  0 0 0 0 1  0 0 0 0 .98" if light else "0 0 0 0 .9  0 0 0 0 .85  0 0 0 0 .72"
    gr = "0 0 0 0 .3  0 0 0 0 .24  0 0 0 0 .14" if light else "0 0 0 0 0  0 0 0 0 0  0 0 0 0 0"
    defs = [f"<svg xmlns='http://www.w3.org/2000/svg' width='{size}' height='{size}'>"]
    rects = []
    if cloud:  # 云絮：低频 fbm 映射成暖褐（亮）/ 月白（暗）的斑驳
        defs.append(
            "<filter id='c' x='0' y='0' width='1' height='1'><feTurbulence type='fractalNoise' baseFrequency='.007' "
            f"numOctaves='3' seed='11' stitchTiles='stitch'/><feColorMatrix values='{ink}  3 0 0 0 -1.35'/></filter>"
        )
        rects.append(f"<rect width='{size}' height='{size}' filter='url(#c)' opacity='{cloud}'/>")
    # 纤维：各向异性噪声（细长条纹）经低频噪声位移弯曲，再取阈值只留最亮的几根。
    for fid, freq, seed, wseed, op in (("f", ".012 .38", 4, 8, fibers), ("h", ".38 .012", 6, 3, fibers2)):
        if not op:
            continue
        defs.append(
            f"<filter id='{fid}' x='0' y='0' width='1' height='1'><feTurbulence type='fractalNoise' baseFrequency='{freq}' "
            f"numOctaves='2' seed='{seed}' stitchTiles='stitch' result='s'/><feTurbulence type='fractalNoise' "
            f"baseFrequency='.01' numOctaves='2' seed='{wseed}' stitchTiles='stitch' result='w'/><feDisplacementMap in='s' "
            f"in2='w' scale='40' xChannelSelector='R' yChannelSelector='G'/><feColorMatrix values='{fib}  16 0 0 0 -10.6'/></filter>"
        )
        rects.append(f"<rect width='{size}' height='{size}' filter='url(#{fid})' opacity='{op}'/>")
    if grain:  # 细颗粒：纸的“牙口”
        defs.append(
            "<filter id='g' x='0' y='0' width='1' height='1'><feTurbulence type='fractalNoise' baseFrequency='.85' "
            f"numOctaves='2' seed='2' stitchTiles='stitch'/><feColorMatrix values='{gr}  -2.4 0 0 0 1.35'/></filter>"
        )
        rects.append(f"<rect width='{size}' height='{size}' filter='url(#g)' opacity='{grain}'/>")
    return enc("".join(defs) + "".join(rects) + "</svg>")


SEAL = enc(
    """<svg xmlns='http://www.w3.org/2000/svg' width='64' height='64'>
<filter id='s' x='0' y='0' width='1' height='1'>
<feTurbulence type='fractalNoise' baseFrequency='.7' numOctaves='2' seed='5' stitchTiles='stitch' result='a'/>
<feColorMatrix in='a' values='0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  0 0 0 -30 22.4' result='s0'/>
<feComponentTransfer in='s0' result='speck'><feFuncA type='linear' slope='.45' intercept='.55'/></feComponentTransfer>
<feTurbulence type='fractalNoise' baseFrequency='.06' numOctaves='2' seed='9' stitchTiles='stitch' result='b'/>
<feColorMatrix in='b' values='0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  0 0 0 1.6 .25' result='patch'/>
<feComposite in='speck' in2='patch' operator='arithmetic' k1='1'/>
</filter><rect width='64' height='64' filter='url(#s)'/></svg>"""
)

# 笔画：路径用水平渐变的 R 通道编码“行笔进度”（0 → 1），滤镜据此只在收笔段（后 16%）
# 用条纹噪声挖出飞白：alpha = 4.2·(1 − R) + 4.4·噪声 − 2.75。
BRUSH = enc(
    """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 400 40' preserveAspectRatio='none'>
<linearGradient id='p'><stop offset='0' stop-color='#000'/><stop offset='.84' stop-color='#000'/><stop offset='1' stop-color='#f00'/></linearGradient>
<filter id='b' x='-3%' y='-40%' width='106%' height='180%'>
<feTurbulence type='fractalNoise' baseFrequency='.0035 .42' numOctaves='3' seed='8' result='n'/>
<feColorMatrix in='SourceGraphic' values='0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  -1 0 0 1 0' result='wet'/>
<feComposite in='wet' in2='n' operator='arithmetic' k2='4.2' k3='4.4' k4='-2.75' result='m'/>
<feComposite in='m' in2='SourceGraphic' operator='in' result='dry'/>
<feTurbulence type='fractalNoise' baseFrequency='.05 .35' numOctaves='2' seed='3' result='e'/>
<feDisplacementMap in='dry' in2='e' scale='3.5' xChannelSelector='R' yChannelSelector='G' result='o'/>
<feColorMatrix in='o' values='0 0 0 0 1  0 0 0 0 1  0 0 0 0 1  0 0 0 1 0'/>
</filter>
<path filter='url(#b)' fill='url(#p)' d='M6 20.5C6 9.5 13 5 25 5.2 70 6 200 7 300 8.2c40 .5 70 2 86 4.5 6 1 8 4.2 8 7.8s-2 6.6-8 7.6c-16 2.6-46 4.2-86 4.8C200 34 70 35 25 35.4 13 35.6 6 31 6 20.5Z'/>
</svg>"""
)

# 点墨：径向渐变给出“中心淡、边缘积墨”，两级位移（低频外形 + 高频纤维毛边）。
DROP = enc(
    """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 200 200'>
<radialGradient id='g'><stop offset='0' stop-color='#fff' stop-opacity='.66'/><stop offset='.52' stop-color='#fff' stop-opacity='.52'/>
<stop offset='.8' stop-color='#fff' stop-opacity='.86'/><stop offset='.9' stop-color='#fff' stop-opacity='.3'/><stop offset='1' stop-color='#fff' stop-opacity='0'/></radialGradient>
<filter id='f' x='-25%' y='-25%' width='150%' height='150%'>
<feTurbulence type='fractalNoise' baseFrequency='.018' numOctaves='3' seed='12' result='w'/>
<feDisplacementMap in='SourceGraphic' in2='w' scale='22' xChannelSelector='R' yChannelSelector='G' result='b'/>
<feTurbulence type='fractalNoise' baseFrequency='.12' numOctaves='2' seed='4' result='fine'/>
<feDisplacementMap in='b' in2='fine' scale='6' xChannelSelector='R' yChannelSelector='G'/>
</filter><circle cx='100' cy='100' r='76' fill='url(#g)' filter='url(#f)'/></svg>"""
)

FIXED = {"--loong-seal-texture": SEAL, "--loong-brush-stroke": BRUSH, "--loong-ink-drop": DROP}
PAPER_NAMES = ("--loong-paper-texture", "--loong-sheet-texture", "--loong-silk-texture")
LIGHT = {
    "--loong-paper-texture": paper(True, cloud=0.07, fibers=0.24, grain=0.04),
    "--loong-sheet-texture": paper(True, cloud=0.05, fibers=0, grain=0.035, size=256),
    "--loong-silk-texture": paper(True, cloud=0.04, fibers=0.18, fibers2=0.18, grain=0.03),
}
DARK = {
    "--loong-paper-texture": paper(False, cloud=0.035, fibers=0, grain=0.07),
    "--loong-sheet-texture": paper(False, cloud=0.025, fibers=0, grain=0.06, size=256),
    "--loong-silk-texture": paper(False, cloud=0.02, fibers=0.01, fibers2=0.01, grain=0.05),
}


def render(css: str) -> str:
    """第一组纸面纹理是亮色，之后的（媒体查询与 data-theme 两份）是暗色。"""
    out, seen = [], 0
    for line in css.split("\n"):
        m = re.match(r'^(\s*)(--loong-[a-z-]+): url\("data:image/svg\+xml,.*\);$', line)
        if not m:
            out.append(line)
            continue
        indent, name = m.groups()
        if name in FIXED:
            out.append(f"{indent}{name}: {FIXED[name]};")
        elif name == PAPER_NAMES[0]:
            table = LIGHT if seen == 0 else DARK
            seen += 1
            out += [f"{indent}{k}: {table[k]};" for k in PAPER_NAMES]
        elif name not in PAPER_NAMES:
            out.append(line)
    return "\n".join(out)


def main() -> int:
    old = CSS.read_text(encoding="utf-8")
    new = render(old)
    if "--check" in sys.argv:
        if new != old:
            print(f"{CSS} 中的纹理不是最新，请运行 python3 tools/loongkit-textures.py", file=sys.stderr)
            return 1
        return 0
    CSS.write_text(new, encoding="utf-8")
    print(f"已更新 {CSS}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
