#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

// Loong OS 图层着色器：在 Smithay 默认纹理着色器基础上增加
//   - 连续圆角遮罩（超椭圆 n=4，与 loong-ui::shape 一致）
//   - 颜色乘子 tint（纯色块 = 1×1 白色纹理 × tint）

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

uniform vec2 size;       // 目标矩形尺寸（物理像素）
uniform vec4 clip;       // 裁剪矩形（相对目标矩形左上角，物理像素）：x, y, w, h
uniform float radius;    // 裁剪矩形的名义圆角半径（物理像素）
uniform vec4 color_mul;  // 预乘颜色乘子

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

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif

    color = color * color_mul * alpha * corner_mask(v_coords * size - clip.xy);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
