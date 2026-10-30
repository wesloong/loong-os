#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

// Loong OS 图层着色器：在 Smithay 默认纹理着色器基础上增加
//   - 连续圆角遮罩（超椭圆 n=4，与 loong-ui::shape 一致）
//   - 颜色乘子 tint（纯色块 = 1×1 白色纹理 × tint）
//   - 「墨韵」水墨遮罩（可选，reveal_mode.x > 0 时才计算）：
//       1 晕染：带噪声起伏的圆形前沿向外扩散，前沿内侧积一圈墨（应用开启 / 收墨）；
//       2 墨散：从下往上像墨入清水般化开，散开处被噪声摇曳、上浮成烟缕（锁屏上滑）。
//     晕染在屏幕坐标中计算，多个元素（窗口的各个子表面）可以共用同一个遮罩。
// 噪声：无三角函数的哈希 + value noise，fbm 只用 3 个八度。

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2 size;         // 目标矩形尺寸（物理像素）
uniform vec4 clip;         // 裁剪矩形（相对目标矩形左上角，物理像素）：x, y, w, h
uniform float radius;      // 裁剪矩形的名义圆角半径（物理像素）
uniform vec4 color_mul;    // 预乘颜色乘子
uniform vec2 origin;       // 目标矩形左上角的屏幕坐标（物理像素）
uniform vec4 reveal;       // 晕染：中心 x、y（屏幕物理像素）、半径、前沿宽度（物理像素）
uniform vec4 reveal_mode;  // 模式（0 无 / 1 晕染 / 2 墨散）、种子、墨散进度、缩放（物理像素/点）
uniform vec4 reveal_ink;   // 前沿积墨的颜色（预乘），a 为积墨强度

// 连续圆角裁剪：p 为相对裁剪矩形左上角的坐标。
float corner_mask(vec2 p) {
    vec2 half_size = clip.zw * 0.5;
    float a = min(radius * 1.28, min(half_size.x, half_size.y));
    if (a < 0.5) {
        vec2 inside = step(vec2(0.0), p) * step(p, clip.zw);
        return inside.x * inside.y;
    }
    vec2 q = abs(p - half_size) - (half_size - vec2(a));
    float d;
    if (q.x > 0.0 && q.y > 0.0) {
        vec2 n = q / a;
        n = n * n;
        d = (sqrt(sqrt(n.x * n.x + n.y * n.y)) - 1.0) * a;
    } else {
        d = max(q.x, q.y) - a;
    }
    return clamp(0.5 - d, 0.0, 1.0);
}

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

void main() {
    vec2 local = v_coords * size;
    vec2 tc = v_coords;
    float mask = 1.0;
    float front = 0.0;

    if (reveal_mode.x > 0.5) {
        float k = reveal_mode.w;
        float seed = reveal_mode.y;
        if (reveal_mode.x < 1.5) {
            // 晕染：到中心的距离被两层噪声扰动——大尺度的不规则轮廓（像墨在纸上洇开的花瓣状边缘）
            // + 细碎的纸纤维。单位换算成“点”，不同缩放的屏幕上纹理一致。
            vec2 sp = (origin + local) / k;
            float r = reveal.z / k;
            float soft = max(reveal.w / k, 1.0);
            float n = fbm(sp * 0.014 + seed);
            float fiber = vnoise(sp * 0.11 + seed * 3.7) * 0.6 + vnoise(sp * 0.31 - seed) * 0.4;
            float dist = length(sp - reveal.xy / k)
                + (n - 0.5) * soft * 3.2
                + (fiber - 0.5) * soft * 0.7;
            mask = 1.0 - smoothstep(r - soft * 0.18, r, dist);
            // 潮线：前沿内侧窄窄一圈积墨，越靠前沿越浓。
            front = mask * smoothstep(r - soft * 0.9, r - soft * 0.05, dist);
        } else {
            // 墨散：底部（锁屏被拉走的一端）先散，越往上越晚；手指一动底边就开始化开。
            // 散开处的画面被噪声左右摇曳、微微上浮，像墨在清水里化成烟缕；丝缕随锁屏一起移动。
            float u = reveal_mode.z;
            vec2 q = local / k;
            float n = fbm(vec2(q.x * 0.012, q.y * 0.01 - u * 1.6) + seed);
            float t = (1.0 - v_coords.y) * 0.6 + n * 0.55;
            float edge = 1.35 * pow(u, 0.75) - 0.08;
            mask = smoothstep(edge - 0.03, edge + 0.22, t);
            float zone = (1.0 - mask) * 0.8 + u * 0.2;
            float sway = fbm(vec2(q.x * 0.02 + u * 0.8, q.y * 0.025) + seed * 1.3 + 5.0) - 0.5;
            float rise = fbm(vec2(q.x * 0.06, q.y * 0.02 - u) + seed * 2.1);
            vec2 disp = vec2(sway * 28.0, rise * 22.0) * zone * k;
            tc = clamp(v_coords + disp / size, 0.0, 1.0);
            front = mask * (1.0 - smoothstep(0.0, 0.1, t - edge));
        }
    }

    vec4 color = texture2D(tex, tc);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif

    color = color * color_mul * alpha * corner_mask(local - clip.xy);
    // 前沿积墨：颜色向墨色靠拢（预乘），再乘遮罩。
    color.rgb = mix(color.rgb, reveal_ink.rgb * color.a, front * reveal_ink.a);
    color *= mask;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
