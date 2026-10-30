use crate::{Animatable, zero};

/// 惯性衰减（滚动松手后的减速）。
///
/// 速度按 `rate_per_ms` 每毫秒衰减一次：v(t) = v₀ · rate^(t·1000)。
/// 这是连续形式，与 UIScrollView 的 `decelerationRate` 数值含义相同。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decay {
    /// 每毫秒保留的速度比例，取值 (0, 1)。
    pub rate_per_ms: f64,
}

impl Decay {
    /// 普通滚动列表。
    pub const NORMAL: Decay = Decay { rate_per_ms: 0.998 };
    /// 快速停止，用于分页、多任务卡片等“落点比滑行重要”的场合。
    pub const FAST: Decay = Decay { rate_per_ms: 0.99 };

    /// 创建衰减参数。
    pub const fn new(rate_per_ms: f64) -> Self {
        Decay { rate_per_ms }
    }

    /// 连续衰减常数 λ = 1000·ln(rate)（负数，单位 1/秒）。
    #[inline]
    fn lambda(&self) -> f64 {
        1000.0 * self.rate_per_ms.clamp(1e-9, 1.0 - 1e-9).ln()
    }

    /// 初速度 `v0` 下经过 `t` 秒的（位移, 速度）。
    pub fn solve(&self, v0: f64, t: f64) -> (f64, f64) {
        if t <= 0.0 {
            return (0.0, v0);
        }
        let l = self.lambda();
        let e = (l * t).exp();
        (v0 * (e - 1.0) / l, v0 * e)
    }

    /// 以初速度 `v0` 惯性滑行，最终会移动的总距离（落点预测）。
    ///
    /// 手势松手时，用 “当前位置 + project(速度)” 判断用户“想去哪里”，
    /// 而不是只看松手位置。与 Apple 在 WWDC18《Designing Fluid Interfaces》中
    /// 给出的离散公式相差不到 1%（NORMAL 约 0.1%，FAST 约 0.5%）。
    pub fn project(&self, v0: f64) -> f64 {
        -v0 / self.lambda()
    }

    /// 从速度 `v0` 衰减到 `v_min` 所需时间（秒）。
    pub fn duration_to(&self, v0: f64, v_min: f64) -> f64 {
        if v0.abs() <= v_min || v_min <= 0.0 {
            return 0.0;
        }
        (v_min / v0.abs()).ln() / self.lambda()
    }
}

impl Default for Decay {
    fn default() -> Self {
        Decay::NORMAL
    }
}

/// 一段惯性衰减动画。
#[derive(Debug, Clone, Copy)]
pub struct DecayAnimation<V: Animatable = f64> {
    decay: Decay,
    from: V,
    velocity: V,
    start_time: f64,
    rest_velocity: f64,
}

impl<V: Animatable> DecayAnimation<V> {
    /// 在 `now` 时刻从 `from` 以速度 `velocity` 开始滑行。
    pub fn new(decay: Decay, from: V, velocity: V, now: f64) -> Self {
        DecayAnimation {
            decay,
            from,
            velocity,
            start_time: now,
            rest_velocity: 1.0,
        }
    }

    /// 速度低于该值（单位/秒）即视为停止。
    pub fn with_rest_velocity(mut self, v: f64) -> Self {
        self.rest_velocity = v;
        self
    }

    /// 最终停下的位置。
    pub fn target(&self) -> V {
        V::from_fn(|i| self.from.get(i) + self.decay.project(self.velocity.get(i)))
    }

    /// `now` 时刻的值。
    pub fn value(&self, now: f64) -> V {
        let t = now - self.start_time;
        V::from_fn(|i| self.from.get(i) + self.decay.solve(self.velocity.get(i), t).0)
    }

    /// `now` 时刻的速度。
    pub fn velocity(&self, now: f64) -> V {
        if self.is_done(now) {
            return zero();
        }
        let t = now - self.start_time;
        V::from_fn(|i| self.decay.solve(self.velocity.get(i), t).1)
    }

    /// 是否已停止。
    pub fn is_done(&self, now: f64) -> bool {
        let t = now - self.start_time;
        (0..V::DIM).all(|i| self.decay.solve(self.velocity.get(i), t).1.abs() <= self.rest_velocity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn velocity_decays_by_rate_each_millisecond() {
        let d = Decay::NORMAL;
        let (_, v) = d.solve(1000.0, 0.001);
        assert!((v - 998.0).abs() < 1e-9);
        let (_, v) = d.solve(1000.0, 0.010);
        assert!((v - 1000.0 * 0.998f64.powi(10)).abs() < 1e-9);
    }

    #[test]
    fn position_converges_to_projection() {
        for d in [Decay::NORMAL, Decay::FAST] {
            let (x, _) = d.solve(2000.0, 30.0);
            assert!((x - d.project(2000.0)).abs() < 1e-6);
        }
    }

    #[test]
    fn projection_matches_apple_discrete_formula() {
        // Apple: (v / 1000) * rate / (1 - rate)
        for rate in [0.998, 0.99] {
            let apple = (1500.0 / 1000.0) * rate / (1.0 - rate);
            let ours = Decay::new(rate).project(1500.0);
            assert!(
                ((ours - apple) / apple).abs() < 0.006,
                "rate {rate}: {ours} vs {apple}"
            );
        }
    }

    #[test]
    fn animation_stops() {
        let a = DecayAnimation::new(Decay::FAST, 0.0, -800.0, 1.0);
        assert!(!a.is_done(1.0));
        let t_end = 1.0 + Decay::FAST.duration_to(800.0, 1.0) + 0.001;
        assert!(a.is_done(t_end));
        assert!((a.value(t_end) - a.target()).abs() < 0.2);
        assert!(a.target() < 0.0);
    }
}
