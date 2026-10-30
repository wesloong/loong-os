//! 确定性噪声：value noise 与 fbm（分形叠加）。纸纹、山脊、飞白、墨晕都用它。

/// 整数哈希 → [0, 1)。
#[inline]
pub fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (y as u32).wrapping_mul(0xd816_3841)
        ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0x00ff_ffff) as f32 / 16_777_216.0
}

#[inline]
fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// 二维 value noise，范围 [0, 1)，周期为整数格。
pub fn value(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (smooth(x - xi as f32), smooth(y - yi as f32));
    let a = hash(xi, yi, seed);
    let b = hash(xi + 1, yi, seed);
    let c = hash(xi, yi + 1, seed);
    let d = hash(xi + 1, yi + 1, seed);
    let top = a + (b - a) * fx;
    let bottom = c + (d - c) * fx;
    top + (bottom - top) * fy
}

/// 分形噪声：`octaves` 层 value noise 叠加（频率 ×2、幅度 ×0.5），归一化到 [0, 1)。
pub fn fbm(x: f32, y: f32, seed: u32, octaves: u32) -> f32 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
    for o in 0..octaves.max(1) {
        sum += amp * value(x * freq, y * freq, seed.wrapping_add(o * 1013));
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_in_range() {
        for i in 0..1000 {
            let (x, y) = (i as f32 * 0.37, i as f32 * 0.91);
            let v = fbm(x, y, 5, 4);
            assert_eq!(v, fbm(x, y, 5, 4));
            assert!((0.0..1.0).contains(&v));
        }
        assert_ne!(value(1.5, 2.5, 1), value(1.5, 2.5, 2));
    }

    #[test]
    fn continuous() {
        let a = value(3.999, 7.0, 9);
        let b = value(4.001, 7.0, 9);
        assert!((a - b).abs() < 0.01);
    }
}
