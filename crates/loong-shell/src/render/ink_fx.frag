// 「墨韵」水墨印记像素着色器：点墨（墨滴）与笔迹（带飞白的笔段）。
//
// 由 GlesRenderer::compile_custom_pixel_shader 编译：Smithay 会在前面加上 `#version 100`
// （调试变体再加 `#define DEBUG_FLAGS`），所以这里不能写 #version。
// 只在一个包住印记的小矩形里运行；噪声为无三角函数的哈希 + value noise（fbm 3 个八度）。
// 输出预乘颜色。

precision highp float;

uniform float alpha;
uniform vec2 size;         // 元素尺寸（物理像素）
varying vec2 v_coords;     // 0..1

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec4 seg;      // 起点 x、y，终点 x、y（局部物理像素；墨滴两点相同）
uniform vec4 shape;    // 起点半径、终点半径（物理像素）、起点处的笔画累计长度（点）、缩放（物理像素/点）
uniform vec4 params;   // 种类（0 墨滴 / 1 笔迹）、浓度、干湿（0 湿 – 1 干）、种子
uniform vec4 ink;      // 墨色（预乘）
uniform vec2 caps;     // 笔段起点、终点是否画圆头（起笔 / 收笔）

float hash12(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

float vnoise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    vec2 u = f * f * (3.0 - 2.0 * f);
    float a = hash12(i);
    float b = hash12(i + vec2(1.0, 0.0));
    float c = hash12(i + vec2(0.0, 1.0));
    float d = hash12(i + vec2(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

float fbm(vec2 p) {
    float s = 0.5 * vnoise(p);
    s += 0.25 * vnoise(p * 2.03 + vec2(17.0, 9.0));
    s += 0.125 * vnoise(p * 4.01 + vec2(31.0, 23.0));
    return s / 0.875;
}

// 墨滴落在宣纸上：花瓣状不规则的轮廓，边缘顺着纸纤维洇开（细密的放射状毛边），
// 墨色由中心向外渐淡，最外缘一圈细细的“水线”，纸纹让墨色微微不匀。
float drop(vec2 p) {
    float k = shape.w;
    float seed = params.w;
    vec2 d = (p - seg.xy) / k;              // 点
    float rr = max(shape.x / k, 0.5);
    float dist = length(d);
    vec2 dir = d / max(dist, 0.001);
    // 在单位圆上取噪声得到轮廓（不用 atan，没有接缝）：大的花瓣起伏 + 细密的纤维。
    float lobe = fbm(dir * 1.7 + seed);
    float fiber = vnoise(dir * 13.0 + seed * 1.3) * 0.6 + vnoise(d * 0.32 + seed) * 0.4;
    float edge_r = rr * (0.8 + 0.34 * lobe);
    float t = dist / max(edge_r, 0.5);
    float body = 1.0 - smoothstep(0.8, 1.0 + 0.14 * fiber, t);
    float wash = 0.5 + 0.5 * (1.0 - smoothstep(0.0, 0.85, t));
    float line = (t - 0.93) / 0.055;
    float ring = exp(-line * line) * 0.42;
    float grain = 0.93 + 0.14 * vnoise(d * 0.55 + seed * 5.1);
    float halo = smoothstep(0.85, 1.0, t) * (1.0 - smoothstep(1.0, 1.32, t)) * 0.16 * fiber;
    return body * (wash * grain + ring) + halo;
}

// 笔迹的一段：笔段两端平切，相邻笔段首尾相接、不重叠；整条笔画的起点（起笔）和终点（收笔）
// 画圆头。边缘顺纸纤维参差、湿笔向外微洇；笔肚里有顺笔势的鬃毛纹，干笔时裂成飞白；
// 墨色沿笔画浓淡起伏。纹理按整条笔画的累计长度取样，连续不断。
float stroke(vec2 p) {
    float k = shape.w;
    float seed = params.w;
    float dry = params.z;
    vec2 a = seg.xy / k;
    vec2 q = p / k;
    vec2 ab = seg.zw / k - a;
    float len = max(length(ab), 0.001);
    vec2 dir = ab / len;
    vec2 nrm = vec2(-dir.y, dir.x);
    float h = dot(q - a, dir);
    float before = step(h, 0.0);
    float after = step(len, h);
    if (before * (1.0 - caps.x) + after * (1.0 - caps.y) > 0.5) {
        return 0.0;
    }
    float hc = clamp(h, 0.0, len);
    float across = dot(q - a, nrm);
    float r = mix(shape.x, shape.y, hc / len) / k;
    float dist = mix(abs(across), length(q - (a + dir * hc)), max(before, after));
    float s = shape.z + hc;
    float wob = vnoise(vec2(s * 0.06, seed));
    float rag = vnoise(vec2(s * 0.32, sign(across) * 5.0 + seed));
    float rr = max(r * (0.84 + 0.22 * wob + 0.14 * (rag - 0.5)), 0.3);
    float u = dist / rr;
    float side = across / rr;
    float body = 1.0 - smoothstep(0.74, 1.0, u);
    float bristle = vnoise(vec2(s * 0.016 + seed, side * 2.4 + seed * 1.7)) * 0.65
        + vnoise(vec2(s * 0.045 - seed, side * 5.0)) * 0.35;
    float gaps = smoothstep(dry * 0.5 - 0.06, dry * 0.5 + 0.08, bristle);
    float tone = mix(0.82 + 0.3 * bristle, gaps, dry);
    float conc = 0.84 + 0.32 * vnoise(vec2(s * 0.012, seed * 0.7));
    float rim = smoothstep(0.5, 0.95, u) * (1.0 - dry) * 0.25;
    float bleed = (1.0 - smoothstep(1.0, 1.4, u)) * (1.0 - dry) * 0.16 * rag;
    return body * tone * conc * (1.0 + rim) + bleed;
}

void main() {
    vec2 p = v_coords * size;
    float cov = params.x < 0.5 ? drop(p) : stroke(p);
    vec4 color = ink * (clamp(cov * params.y, 0.0, 1.0) * alpha);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
