//! 栅格工具：浮点灰度场（墨量、遮罩、噪声）、模糊、重采样，以及把墨/颜料按遮罩
//! 叠加到预乘 alpha 位图上的混合函数。

use tiny_skia::{FillRule, Paint, Path, Pixmap, Transform};

/// 单通道浮点场，行优先。
#[derive(Clone)]
pub struct Field {
    pub w: usize,
    pub h: usize,
    pub data: Vec<f32>,
}

impl Field {
    pub fn new(w: usize, h: usize) -> Field {
        Field {
            w: w.max(1),
            h: h.max(1),
            data: vec![0.0; w.max(1) * h.max(1)],
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.w + x]
    }

    #[inline]
    pub fn at(&mut self, x: usize, y: usize) -> &mut f32 {
        &mut self.data[y * self.w + x]
    }

    /// 双线性采样（坐标为像素中心坐标系：像素 i 的中心在 i + 0.5），超出边界取边缘值。
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let fx = (x - 0.5).clamp(0.0, (self.w - 1) as f32);
        let fy = (y - 0.5).clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let a = self.data[y0 * self.w + x0];
        let b = self.data[y0 * self.w + x1];
        let c = self.data[y1 * self.w + x0];
        let d = self.data[y1 * self.w + x1];
        let top = a + (b - a) * tx;
        let bot = c + (d - c) * tx;
        top + (bot - top) * ty
    }

    /// 由函数逐像素生成（参数为像素中心坐标）。
    pub fn from_fn(w: usize, h: usize, mut f: impl FnMut(f32, f32) -> f32) -> Field {
        let mut out = Field::new(w, h);
        for y in 0..out.h {
            for x in 0..out.w {
                out.data[y * out.w + x] = f(x as f32 + 0.5, y as f32 + 0.5);
            }
        }
        out
    }

    /// 放大/缩小到 `w × h`（双线性）。
    pub fn resized(&self, w: usize, h: usize) -> Field {
        let (sx, sy) = (self.w as f32 / w as f32, self.h as f32 / h as f32);
        let mut out = Field::new(w, h);
        // 每行的 y 插值系数相同，先算好每列的 x 下标与系数。
        let cols: Vec<(usize, usize, f32)> = (0..w)
            .map(|x| {
                let fx = ((x as f32 + 0.5) * sx - 0.5).clamp(0.0, (self.w - 1) as f32);
                let x0 = fx as usize;
                (x0, (x0 + 1).min(self.w - 1), fx - x0 as f32)
            })
            .collect();
        for y in 0..h {
            let fy = ((y as f32 + 0.5) * sy - 0.5).clamp(0.0, (self.h - 1) as f32);
            let y0 = fy as usize;
            let y1 = (y0 + 1).min(self.h - 1);
            let ty = fy - y0 as f32;
            let r0 = &self.data[y0 * self.w..(y0 + 1) * self.w];
            let r1 = &self.data[y1 * self.w..(y1 + 1) * self.w];
            let row = &mut out.data[y * w..(y + 1) * w];
            for (o, &(x0, x1, tx)) in row.iter_mut().zip(&cols) {
                let top = r0[x0] + (r0[x1] - r0[x0]) * tx;
                let bot = r1[x0] + (r1[x1] - r1[x0]) * tx;
                *o = top + (bot - top) * ty;
            }
        }
        out
    }

    /// 近似高斯模糊（三次盒式模糊），`radius` 为像素。
    pub fn blur(&mut self, radius: f32) {
        let r = radius.round() as usize;
        if r == 0 {
            return;
        }
        for _ in 0..3 {
            box_blur(&mut self.data, self.w, self.h, r);
        }
    }

    pub fn max_value(&self) -> f32 {
        self.data.iter().copied().fold(0.0, f32::max)
    }
}

fn box_blur(buf: &mut [f32], w: usize, h: usize, r: usize) {
    let mut tmp = vec![0.0f32; buf.len()];
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        let row = &buf[y * w..(y + 1) * w];
        let mut acc: f32 = (0..=r).map(|i| row[i.min(w - 1)]).sum::<f32>() + row[0] * r as f32;
        let out = &mut tmp[y * w..(y + 1) * w];
        for x in 0..w {
            out[x] = acc * norm;
            acc += row[(x + r + 1).min(w - 1)] - row[x.saturating_sub(r)];
        }
    }
    let mut acc = vec![0.0f32; w];
    for (x, a) in acc.iter_mut().enumerate() {
        *a = (0..=r).map(|i| tmp[i.min(h - 1) * w + x]).sum::<f32>() + tmp[x] * r as f32;
    }
    for y in 0..h {
        let add = (y + r + 1).min(h - 1) * w;
        let sub = y.saturating_sub(r) * w;
        let out = &mut buf[y * w..(y + 1) * w];
        for x in 0..w {
            out[x] = acc[x] * norm;
            acc[x] += tmp[add + x] - tmp[sub + x];
        }
    }
}

#[inline]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
pub fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// 0xRRGGBB → 线性无关的 [0,1] sRGB 分量（这里直接在 sRGB 空间混合，和传统颜料调色的观感接近）。
#[inline]
pub fn rgb(c: u32) -> [f32; 3] {
    [
        ((c >> 16) & 0xff) as f32 / 255.0,
        ((c >> 8) & 0xff) as f32 / 255.0,
        (c & 0xff) as f32 / 255.0,
    ]
}

#[inline]
pub fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
}

/// 把颜色 `color` 以不透明度 `alpha` 叠加（source-over）到预乘位图的第 `i` 个像素。
#[inline]
pub fn blend_px(data: &mut [u8], i: usize, color: [f32; 3], alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let a = alpha.min(1.0);
    let px = &mut data[i * 4..i * 4 + 4];
    let inv = 1.0 - a;
    px[0] = (color[0] * a * 255.0 + px[0] as f32 * inv + 0.5) as u8;
    px[1] = (color[1] * a * 255.0 + px[1] as f32 * inv + 0.5) as u8;
    px[2] = (color[2] * a * 255.0 + px[2] as f32 * inv + 0.5) as u8;
    px[3] = (a * 255.0 + px[3] as f32 * inv + 0.5) as u8;
}

/// 按遮罩把单色叠加到整张位图（遮罩与位图同尺寸）。
pub fn blend_field(pm: &mut Pixmap, field: &Field, color: [f32; 3], opacity: f32) {
    debug_assert_eq!(field.w, pm.width() as usize);
    let data = pm.data_mut();
    for (i, &a) in field.data.iter().enumerate() {
        blend_px(data, i, color, a * opacity);
    }
}

/// 把不透明的 RGB 场写入位图（每个像素 alpha = 1）。
pub fn write_opaque(pm: &mut Pixmap, f: impl Fn(usize, usize) -> [f32; 3]) {
    let w = pm.width() as usize;
    for (i, px) in pm.data_mut().chunks_exact_mut(4).enumerate() {
        let c = f(i % w, i / w);
        px[0] = (c[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        px[1] = (c[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        px[2] = (c[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        px[3] = 255;
    }
}

/// 栅格化路径为覆盖度场（抗锯齿）。
pub fn path_mask(path: &Path, w: usize, h: usize, transform: Transform) -> Field {
    let mut pm = crate::pixmap(w as u32, h as u32);
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color_rgba8(255, 255, 255, 255);
    pm.fill_path(path, &paint, FillRule::Winding, transform, None);
    let mut f = Field::new(w, h);
    for (o, px) in f.data.iter_mut().zip(pm.data().chunks_exact(4)) {
        *o = px[3] as f32 / 255.0;
    }
    f
}

/// 位图放大到 `w × h`（双线性，预乘空间），用于低分辨率绘制后放大。
///
/// 可分离的定点双线性：先横向插值每一行源图（16 位中间结果），再按行并行纵向插值。
/// 比通用的变换 + 采样管线快一个数量级，开机时要放大上千万像素。
pub fn upscale(src: &Pixmap, w: u32, h: u32) -> Pixmap {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (w, h) = (w.max(1) as usize, h.max(1) as usize);
    // 每个目标坐标对应的两个源下标与 0..=256 的权重。
    let taps = |dst: usize, srcn: usize| -> Vec<(usize, usize, u32)> {
        let s = srcn as f32 / dst as f32;
        (0..dst)
            .map(|i| {
                let f = ((i as f32 + 0.5) * s - 0.5).clamp(0.0, (srcn - 1) as f32);
                let i0 = f as usize;
                let i1 = (i0 + 1).min(srcn - 1);
                (i0, i1, ((f - i0 as f32) * 256.0).round() as u32)
            })
            .collect()
    };
    let cols = taps(w, sw);
    let rows = taps(h, sh);
    // 横向：sh 行 × w 列 × 4 通道，值域 0..=255·256。
    let sdata = src.data();
    let mut mid = vec![0u16; sh * w * 4];
    for y in 0..sh {
        let srow = &sdata[y * sw * 4..(y + 1) * sw * 4];
        let mrow = &mut mid[y * w * 4..(y + 1) * w * 4];
        for (x, &(x0, x1, t)) in cols.iter().enumerate() {
            let (a, b) = (&srow[x0 * 4..x0 * 4 + 4], &srow[x1 * 4..x1 * 4 + 4]);
            for c in 0..4 {
                mrow[x * 4 + c] = (a[c] as u32 * (256 - t) + b[c] as u32 * t) as u16;
            }
        }
    }
    let mut out = crate::pixmap(w as u32, h as u32);
    let mid = &mid;
    let rows = &rows;
    super::landscape::par_rows(out.data_mut(), w, |y, row| {
        let (y0, y1, t) = rows[y];
        let (r0, r1) = (
            &mid[y0 * w * 4..(y0 + 1) * w * 4],
            &mid[y1 * w * 4..(y1 + 1) * w * 4],
        );
        for (o, (&a, &b)) in row.iter_mut().zip(r0.iter().zip(r1.iter())) {
            *o = ((a as u32 * (256 - t) + b as u32 * t + 32768) >> 16) as u8;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_keeps_flat_fields_flat() {
        let mut f = Field::from_fn(40, 30, |_, _| 0.5);
        f.blur(3.0);
        assert!(f.data.iter().all(|v| (v - 0.5).abs() < 1e-4));
    }

    #[test]
    fn resize_preserves_gradients() {
        let f = Field::from_fn(10, 10, |x, _| x / 10.0);
        let g = f.resized(40, 40);
        assert!(g.get(0, 0) < g.get(20, 0) && g.get(20, 0) < g.get(39, 0));
    }
}
