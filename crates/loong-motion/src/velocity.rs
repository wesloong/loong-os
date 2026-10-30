use crate::{Animatable, zero};

const CAPACITY: usize = 32;

/// 根据触点轨迹估算速度（单位/秒）。
///
/// 对最近 `horizon`（默认 100ms）内的采样做最小二乘直线拟合，抗抖动。
/// 如果手指在松开前停住超过 `stop_threshold`（默认 40ms），速度视为 0——
/// 用户“按住再松开”时不应触发惯性。
#[derive(Debug, Clone)]
pub struct VelocityTracker<V: Animatable = f64> {
    samples: [(f64, V); CAPACITY],
    len: usize,
    head: usize,
    horizon: f64,
    stop_threshold: f64,
}

impl<V: Animatable> Default for VelocityTracker<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Animatable> VelocityTracker<V> {
    /// 使用默认参数创建。
    pub fn new() -> Self {
        VelocityTracker {
            samples: [(0.0, zero()); CAPACITY],
            len: 0,
            head: 0,
            horizon: 0.1,
            stop_threshold: 0.04,
        }
    }

    /// 清空采样（新手势开始时调用）。
    pub fn reset(&mut self) {
        self.len = 0;
        self.head = 0;
    }

    /// 记录 `time` 时刻的位置。时间必须单调不减。
    pub fn add(&mut self, time: f64, position: V) {
        if let Some((last_t, _)) = self.last() {
            if time < last_t {
                // 时间倒退通常意味着新手势，重新开始。
                self.reset();
            }
        }
        self.samples[self.head] = (time, position);
        self.head = (self.head + 1) % CAPACITY;
        self.len = (self.len + 1).min(CAPACITY);
    }

    fn last(&self) -> Option<(f64, V)> {
        (self.len > 0).then(|| self.samples[(self.head + CAPACITY - 1) % CAPACITY])
    }

    /// 以最后一个采样时刻为“现在”估算速度。
    pub fn velocity(&self) -> V {
        match self.last() {
            Some((t, _)) => self.velocity_at(t),
            None => zero(),
        }
    }

    /// 在 `now` 时刻（例如手指抬起时）估算速度。
    pub fn velocity_at(&self, now: f64) -> V {
        let Some((last_t, _)) = self.last() else {
            return zero();
        };
        if now - last_t > self.stop_threshold {
            return zero();
        }
        // 取时间窗口内的采样（从新到旧）。
        let window: Vec<(f64, V)> = (0..self.len)
            .map(|i| self.samples[(self.head + CAPACITY - 1 - i) % CAPACITY])
            .take_while(|(t, _)| last_t - t <= self.horizon)
            .collect();
        if window.len() < 2 {
            return zero();
        }
        let n = window.len() as f64;
        let mean_t = window.iter().map(|(t, _)| t).sum::<f64>() / n;
        let var_t: f64 = window.iter().map(|(t, _)| (t - mean_t).powi(2)).sum();
        if var_t <= f64::EPSILON {
            return zero();
        }
        V::from_fn(|i| {
            let mean_x = window.iter().map(|(_, p)| p.get(i)).sum::<f64>() / n;
            let cov: f64 = window
                .iter()
                .map(|(t, p)| (t - mean_t) * (p.get(i) - mean_x))
                .sum();
            cov / var_t
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_velocity_is_exact() {
        let mut vt = VelocityTracker::<[f64; 2]>::new();
        for i in 0..20 {
            let t = i as f64 / 120.0;
            vt.add(t, [300.0 * t, -1200.0 * t + 5.0]);
        }
        let v = vt.velocity();
        assert!((v[0] - 300.0).abs() < 1e-6);
        assert!((v[1] + 1200.0).abs() < 1e-6);
    }

    #[test]
    fn noise_is_smoothed() {
        let mut vt = VelocityTracker::<f64>::new();
        for i in 0..12 {
            let t = i as f64 / 120.0;
            let jitter = if i % 2 == 0 { 0.8 } else { -0.8 };
            vt.add(t, 1000.0 * t + jitter);
        }
        assert!((vt.velocity() - 1000.0).abs() < 60.0);
    }

    #[test]
    fn pause_before_release_means_zero_velocity() {
        let mut vt = VelocityTracker::<f64>::new();
        for i in 0..10 {
            vt.add(i as f64 * 0.008, i as f64 * 10.0);
        }
        assert!(vt.velocity_at(0.072 + 0.01) > 0.0);
        assert_eq!(vt.velocity_at(0.072 + 0.1), 0.0);
    }

    #[test]
    fn only_recent_samples_count() {
        let mut vt = VelocityTracker::<f64>::new();
        // 先慢后快：速度应反映最近 100ms。
        for i in 0..30 {
            vt.add(i as f64 * 0.01, i as f64 * 1.0);
        }
        let base = 29.0;
        for i in 1..=10 {
            vt.add(0.29 + i as f64 * 0.01, base + i as f64 * 20.0);
        }
        assert!(vt.velocity() > 1500.0);
    }

    #[test]
    fn single_sample_is_zero() {
        let mut vt = VelocityTracker::<f64>::new();
        vt.add(1.0, 5.0);
        assert_eq!(vt.velocity(), 0.0);
    }
}
