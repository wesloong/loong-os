use std::f64::consts::{PI, TAU};

use crate::{Animatable, sub, zero};

/// 弹簧参数（质量固定为 1）。
///
/// - `response`：弹簧“速度感”，单位秒。数值上等于无阻尼时的振荡周期；越小越快。
/// - `damping_fraction`：阻尼比 ζ。`1.0` 为临界阻尼（不回弹、最快停稳）；
///   `< 1.0` 会回弹；`> 1.0` 更“黏”。
///
/// 与 SwiftUI `spring(response:dampingFraction:)` 的参数含义一致，方便设计稿直接对照。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    /// 响应时间（秒），必须大于 0。
    pub response: f64,
    /// 阻尼比，必须 ≥ 0。
    pub damping_fraction: f64,
}

impl Spring {
    /// 跟手阶段：手指拖动时让元素紧紧跟随，几乎无延迟。
    pub const INTERACTIVE: Spring = Spring::new(0.15, 0.86);
    /// 利落：按钮、开关、小面板。
    pub const SNAPPY: Spring = Spring::new(0.3, 0.85);
    /// 平滑：默认的大面积转场，无回弹。
    pub const SMOOTH: Spring = Spring::new(0.5, 1.0);
    /// 弹跳：图标落位、提示出现等需要“活力”的场合。
    pub const BOUNCY: Spring = Spring::new(0.5, 0.7);
    /// 打开应用：图标放大成窗口。
    pub const APP_OPEN: Spring = Spring::new(0.42, 0.9);
    /// 回到主屏：窗口缩回图标，保留一点点回弹。
    pub const APP_CLOSE: Spring = Spring::new(0.38, 0.82);

    /// 创建弹簧参数。
    pub const fn new(response: f64, damping_fraction: f64) -> Self {
        Spring {
            response,
            damping_fraction,
        }
    }

    /// 无阻尼角频率 ω₀ = 2π / response。
    #[inline]
    pub fn omega0(&self) -> f64 {
        TAU / self.response.max(1e-6)
    }

    /// 劲度系数 k = ω₀²（质量为 1）。
    #[inline]
    pub fn stiffness(&self) -> f64 {
        let w0 = self.omega0();
        w0 * w0
    }

    /// 阻尼系数 c = 4π·ζ / response（质量为 1）。
    #[inline]
    pub fn damping(&self) -> f64 {
        4.0 * PI * self.damping_fraction / self.response.max(1e-6)
    }

    /// 从初始位移 `x0`（相对目标）和初速度 `v0` 出发，经过 `t` 秒后的（位移, 速度）。
    ///
    /// 使用解析解，任意时刻求值都是精确的，与帧率无关。
    pub fn solve(&self, x0: f64, v0: f64, t: f64) -> (f64, f64) {
        if t <= 0.0 {
            return (x0, v0);
        }
        let w0 = self.omega0();
        let z = self.damping_fraction.max(0.0);
        const EPS: f64 = 1e-6;

        if z < 1.0 - EPS {
            // 欠阻尼：衰减振荡。
            let wd = w0 * (1.0 - z * z).sqrt();
            let decay = (-z * w0 * t).exp();
            let a = x0;
            let b = (v0 + z * w0 * x0) / wd;
            let (s, c) = (wd * t).sin_cos();
            let x = decay * (a * c + b * s);
            let v = decay * (-z * w0 * (a * c + b * s) + wd * (b * c - a * s));
            (x, v)
        } else if z <= 1.0 + EPS {
            // 临界阻尼：最快且不越过目标（除非初速度很大）。
            let decay = (-w0 * t).exp();
            let b = v0 + w0 * x0;
            let x = decay * (x0 + b * t);
            let v = decay * (v0 - w0 * b * t);
            (x, v)
        } else {
            // 过阻尼：两个指数项叠加。
            let s = (z * z - 1.0).sqrt();
            let r1 = -w0 * (z - s);
            let r2 = -w0 * (z + s);
            let c2 = (v0 - r1 * x0) / (r2 - r1);
            let c1 = x0 - c2;
            let (e1, e2) = ((r1 * t).exp(), (r2 * t).exp());
            (c1 * e1 + c2 * e2, c1 * r1 * e1 + c2 * r2 * e2)
        }
    }

    /// 估算从 (`x0`, `v0`) 运动到静止（位移 ≤ `rest_delta` 且速度 ≤ `rest_velocity`）
    /// 所需时间（秒）。最长返回 `max_duration`。
    pub fn settling_duration(
        &self,
        x0: f64,
        v0: f64,
        rest_delta: f64,
        rest_velocity: f64,
        max_duration: f64,
    ) -> f64 {
        // 以 1ms 步长查找最后一次“未静止”的时刻。解析解求值很便宜。
        const STEP: f64 = 0.001;
        let mut last_moving = 0.0;
        let mut t = 0.0;
        while t <= max_duration {
            let (x, v) = self.solve(x0, v0, t);
            if x.abs() > rest_delta || v.abs() > rest_velocity {
                last_moving = t;
            } else if t - last_moving > self.response {
                // 已经连续静止超过一个周期，不会再动了。
                break;
            }
            t += STEP;
        }
        (last_moving + STEP).min(max_duration)
    }
}

impl Default for Spring {
    fn default() -> Self {
        Spring::SMOOTH
    }
}

/// 一段弹簧动画：从当前状态朝 `target` 运动。
///
/// 内部只记录“起点状态 + 起始时间”，任意时刻用解析解求值，所以：
/// - 不需要逐帧推进，掉帧也不会累计误差；
/// - [`retarget`](Self::retarget) 时以当前位置和速度为新起点，运动是连续的。
#[derive(Debug, Clone, Copy)]
pub struct SpringAnimation<V: Animatable = f64> {
    spring: Spring,
    target: V,
    /// 起始时刻相对目标的位移。
    start_displacement: V,
    start_velocity: V,
    start_time: f64,
    rest_delta: f64,
    rest_velocity: f64,
}

impl<V: Animatable> SpringAnimation<V> {
    /// 在时刻 `now` 从 `from`（带初速度 `velocity`）开始朝 `to` 运动。
    pub fn new(spring: Spring, from: V, to: V, velocity: V, now: f64) -> Self {
        SpringAnimation {
            spring,
            target: to,
            start_displacement: sub(from, to),
            start_velocity: velocity,
            start_time: now,
            rest_delta: 0.001,
            rest_velocity: 0.01,
        }
    }

    /// 静止在 `value` 的“动画”（已完成状态）。
    pub fn at_rest(spring: Spring, value: V, now: f64) -> Self {
        Self::new(spring, value, value, zero(), now)
    }

    /// 设置静止判定阈值（单位与动画值相同）。例如像素动画可用 `(0.1, 1.0)`。
    pub fn with_rest_thresholds(mut self, delta: f64, velocity: f64) -> Self {
        self.rest_delta = delta;
        self.rest_velocity = velocity;
        self
    }

    /// 弹簧参数。
    pub fn spring(&self) -> Spring {
        self.spring
    }

    /// 目标值。
    pub fn target(&self) -> V {
        self.target
    }

    fn state(&self, now: f64) -> (V, V) {
        let t = now - self.start_time;
        let mut xs = [0.0; 16];
        let mut vs = [0.0; 16];
        debug_assert!(V::DIM <= 16, "最多支持 16 个分量");
        for i in 0..V::DIM {
            let (x, v) = self.spring.solve(
                self.start_displacement.get(i),
                self.start_velocity.get(i),
                t,
            );
            xs[i] = x;
            vs[i] = v;
        }
        let value = V::from_fn(|i| self.target.get(i) + xs[i]);
        let velocity = V::from_fn(|i| vs[i]);
        (value, velocity)
    }

    /// `now` 时刻的值。动画结束后返回精确的目标值。
    pub fn value(&self, now: f64) -> V {
        if self.is_done(now) {
            return self.target;
        }
        self.state(now).0
    }

    /// `now` 时刻的速度（单位/秒）。
    pub fn velocity(&self, now: f64) -> V {
        if self.is_done(now) {
            return zero();
        }
        self.state(now).1
    }

    /// 所有分量是否都已静止。
    pub fn is_done(&self, now: f64) -> bool {
        let (value, velocity) = self.state(now);
        (0..V::DIM).all(|i| {
            (value.get(i) - self.target.get(i)).abs() <= self.rest_delta
                && velocity.get(i).abs() <= self.rest_velocity
        })
    }

    /// 在 `now` 时刻改为朝 `to` 运动，保持位置与速度连续。
    pub fn retarget(&mut self, to: V, now: f64) {
        let (value, velocity) = self.state(now);
        *self = SpringAnimation {
            target: to,
            start_displacement: sub(value, to),
            start_velocity: velocity,
            start_time: now,
            ..*self
        };
    }

    /// 在 `now` 时刻更换弹簧参数（例如手指松开后从“跟手”切换到“回弹”），保持连续。
    pub fn set_spring(&mut self, spring: Spring, now: f64) {
        let target = self.target;
        self.retarget(target, now);
        self.spring = spring;
    }

    /// 在 `now` 时刻把值直接设为 `value`、速度设为 `velocity`（例如手指正在拖动时）。
    pub fn jump_to(&mut self, value: V, velocity: V, now: f64) {
        self.start_displacement = sub(value, self.target);
        self.start_velocity = velocity;
        self.start_time = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用 RK4 数值积分作为“标准答案”，验证解析解。
    fn rk4(spring: Spring, x0: f64, v0: f64, t_end: f64) -> (f64, f64) {
        let (k, c) = (spring.stiffness(), spring.damping());
        let accel = |x: f64, v: f64| -k * x - c * v;
        let dt = 1e-5;
        let (mut x, mut v) = (x0, v0);
        let steps = (t_end / dt).round() as usize;
        for _ in 0..steps {
            let (k1x, k1v) = (v, accel(x, v));
            let (k2x, k2v) = (
                v + 0.5 * dt * k1v,
                accel(x + 0.5 * dt * k1x, v + 0.5 * dt * k1v),
            );
            let (k3x, k3v) = (
                v + 0.5 * dt * k2v,
                accel(x + 0.5 * dt * k2x, v + 0.5 * dt * k2v),
            );
            let (k4x, k4v) = (v + dt * k3v, accel(x + dt * k3x, v + dt * k3v));
            x += dt / 6.0 * (k1x + 2.0 * k2x + 2.0 * k3x + k4x);
            v += dt / 6.0 * (k1v + 2.0 * k2v + 2.0 * k3v + k4v);
        }
        (x, v)
    }

    #[test]
    fn analytic_matches_numeric_for_all_damping_regimes() {
        for &zeta in &[0.0, 0.3, 0.7, 1.0, 1.5, 3.0] {
            let spring = Spring::new(0.4, zeta);
            for &(x0, v0) in &[(1.0, 0.0), (-200.0, 1500.0), (0.0, -3.0)] {
                for &t in &[0.016, 0.1, 0.25, 0.6] {
                    let (xa, va) = spring.solve(x0, v0, t);
                    let (xn, vn) = rk4(spring, x0, v0, t);
                    let scale = x0.abs().max(v0.abs()).max(1.0);
                    assert!(
                        (xa - xn).abs() < 1e-6 * scale,
                        "ζ={zeta} t={t}: x {xa} vs {xn}"
                    );
                    assert!(
                        (va - vn).abs() < 1e-5 * scale,
                        "ζ={zeta} t={t}: v {va} vs {vn}"
                    );
                }
            }
        }
    }

    #[test]
    fn response_is_the_undamped_period() {
        let spring = Spring::new(0.5, 0.0);
        let (x, v) = spring.solve(1.0, 0.0, 0.5);
        assert!((x - 1.0).abs() < 1e-9 && v.abs() < 1e-9);
        let (x, _) = spring.solve(1.0, 0.0, 0.25);
        assert!((x + 1.0).abs() < 1e-9);
    }

    #[test]
    fn critically_damped_does_not_overshoot_from_rest() {
        let spring = Spring::SMOOTH;
        let mut t = 0.0;
        while t < 2.0 {
            let (x, _) = spring.solve(100.0, 0.0, t);
            assert!(x >= -1e-9, "t={t} 越过目标: {x}");
            t += 0.001;
        }
    }

    #[test]
    fn bouncy_overshoots() {
        let spring = Spring::BOUNCY;
        let min = (0..2000)
            .map(|i| spring.solve(1.0, 0.0, i as f64 * 0.001).0)
            .fold(f64::INFINITY, f64::min);
        // ζ=0.7 的理论超调量 = e^(−ζπ/√(1−ζ²)) ≈ 4.6%。
        let expected = (-0.7 * PI / (1.0f64 - 0.49).sqrt()).exp();
        assert!((min + expected).abs() < 1e-3, "回弹 {min}，理论 {expected}");
    }

    #[test]
    fn animation_reaches_target_and_reports_done() {
        let anim = SpringAnimation::new(Spring::SNAPPY, 0.0, 100.0, 0.0, 10.0)
            .with_rest_thresholds(0.1, 1.0);
        assert!(!anim.is_done(10.0));
        assert!(anim.value(10.05) > 0.0);
        assert!(anim.is_done(12.0));
        assert_eq!(anim.value(12.0), 100.0);
        assert_eq!(anim.velocity(12.0), 0.0);
    }

    #[test]
    fn retarget_keeps_position_and_velocity_continuous() {
        let mut anim =
            SpringAnimation::new(Spring::BOUNCY, [0.0, 0.0], [300.0, -50.0], [0.0, 0.0], 0.0);
        let t = 0.12;
        let (before_x, before_v) = (anim.value(t), anim.velocity(t));
        anim.retarget([-80.0, 40.0], t);
        let (after_x, after_v) = (anim.value(t), anim.velocity(t));
        for i in 0..2 {
            assert!((before_x[i] - after_x[i]).abs() < 1e-9);
            assert!((before_v[i] - after_v[i]).abs() < 1e-9);
        }
        assert!(anim.is_done(5.0));
        assert_eq!(anim.value(5.0), [-80.0, 40.0]);
    }

    #[test]
    fn set_spring_is_continuous() {
        let mut anim = SpringAnimation::new(Spring::INTERACTIVE, 0.0, 1.0, 5.0, 0.0);
        let (x, v) = (anim.value(0.05), anim.velocity(0.05));
        anim.set_spring(Spring::BOUNCY, 0.05);
        assert!((anim.value(0.05) - x).abs() < 1e-12);
        assert!((anim.velocity(0.05) - v).abs() < 1e-12);
        assert_eq!(anim.spring(), Spring::BOUNCY);
    }

    #[test]
    fn frame_rate_independent() {
        // 60Hz 与 120Hz 在相同时间点取样，结果完全一致（解析解不累计误差）。
        let anim = SpringAnimation::new(Spring::APP_OPEN, 0.0, 1.0, 0.0, 0.0);
        for frame in 0..30 {
            let t60 = frame as f64 / 60.0;
            let t120 = (frame * 2) as f64 / 120.0;
            assert_eq!(anim.value(t60), anim.value(t120));
        }
    }

    #[test]
    fn settling_duration_is_reasonable() {
        let d = Spring::SMOOTH.settling_duration(1.0, 0.0, 0.001, 0.01, 10.0);
        assert!(d > 0.4 && d < 1.5, "SMOOTH 停稳时间 {d}");
        let d_fast = Spring::SNAPPY.settling_duration(1.0, 0.0, 0.001, 0.01, 10.0);
        assert!(d_fast < d);
    }
}
