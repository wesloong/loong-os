//! 确定性噪声：value noise 与 fbm（分形叠加）、可平铺噪声、一维噪声与伪随机数。
//! 纸纹、山脊、飞白、墨晕都用它。

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

/// 向下取整到整数（不调用 libm 的 `floorf`：基线 x86‑64 没有对应指令，逐像素调用很慢）。
#[inline]
fn ifloor(x: f32) -> i32 {
    let i = x as i32;
    i - ((x < i as f32) as i32)
}

/// 二维 value noise，范围 [0, 1)，周期为整数格。
#[inline]
pub fn value(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (ifloor(x), ifloor(y));
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

/// 可平铺的 value noise：在 x 方向以 `period_x` 格、y 方向以 `period_y` 格为周期。
pub fn value_periodic(x: f32, y: f32, period_x: i32, period_y: i32, seed: u32) -> f32 {
    let (xi, yi) = (ifloor(x), ifloor(y));
    let (fx, fy) = (smooth(x - xi as f32), smooth(y - yi as f32));
    let px = period_x.max(1);
    let py = period_y.max(1);
    let (x0, x1) = (xi.rem_euclid(px), (xi + 1).rem_euclid(px));
    let (y0, y1) = (yi.rem_euclid(py), (yi + 1).rem_euclid(py));
    let a = hash(x0, y0, seed);
    let b = hash(x1, y0, seed);
    let c = hash(x0, y1, seed);
    let d = hash(x1, y1, seed);
    let top = a + (b - a) * fx;
    let bottom = c + (d - c) * fx;
    top + (bottom - top) * fy
}

/// 可平铺的分形噪声（每一层频率翻倍，周期也随之翻倍，保证整体仍可平铺）。
pub fn fbm_periodic(x: f32, y: f32, period: i32, seed: u32, octaves: u32) -> f32 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
    let mut p = period;
    for o in 0..octaves.max(1) {
        sum += amp * value_periodic(x * freq, y * freq, p, p, seed.wrapping_add(o * 1013));
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
        p *= 2;
    }
    sum / norm
}

/// 一维 value noise（山脊线、笔压起伏用），范围 [0, 1)。
pub fn value1(x: f32, seed: u32) -> f32 {
    let xi = ifloor(x);
    let f = smooth(x - xi as f32);
    let a = hash(xi, 0x51ed, seed);
    let b = hash(xi + 1, 0x51ed, seed);
    a + (b - a) * f
}

/// 一维分形噪声，范围 [0, 1)。
pub fn fbm1(x: f32, seed: u32, octaves: u32) -> f32 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
    for o in 0..octaves.max(1) {
        sum += amp * value1(x * freq, seed.wrapping_add(o * 7919));
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

/// 确定性伪随机数发生器（SplitMix64）：布置山峰、纤维、苔点等。
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// 由种子与用途标签构造（不同用途互不相关）。
    pub fn new(seed: u32, salt: u32) -> Rng {
        Rng(((seed as u64) << 32 | salt as u64) ^ 0x9e37_79b9_7f4a_7c15)
    }

    /// 下一个 64 位随机数。
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// [0, 1) 均匀分布。
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// [lo, hi) 均匀分布。
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.f32()
    }

    /// 近似正态分布（均值 0，标准差 1）。
    pub fn normal(&mut self) -> f32 {
        (0..4).map(|_| self.f32()).sum::<f32>() * 1.732 - 3.464
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periodic_noise_tiles() {
        for i in 0..50 {
            let (x, y) = (i as f32 * 0.31, i as f32 * 0.77);
            let a = value_periodic(x, y, 8, 8, 3);
            let b = value_periodic(x + 8.0, y - 16.0, 8, 8, 3);
            assert!((a - b).abs() < 1e-5);
            let f = fbm_periodic(x, y, 4, 3, 3);
            assert!((f - fbm_periodic(x + 4.0, y + 4.0, 4, 3, 3)).abs() < 1e-5);
        }
    }

    #[test]
    fn rng_is_deterministic_and_uniform() {
        let mut a = Rng::new(9, 1);
        let mut b = Rng::new(9, 1);
        let mut sum = 0.0;
        for _ in 0..10_000 {
            let v = a.f32();
            assert_eq!(v, b.f32());
            assert!((0.0..1.0).contains(&v));
            sum += v;
        }
        assert!((sum / 10_000.0 - 0.5).abs() < 0.02);
        assert_ne!(Rng::new(9, 1).next_u64(), Rng::new(9, 2).next_u64());
    }

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
