//! 把弹簧曲线转换为 CSS `linear()` 缓动函数。
//!
//! Web 应用（设置、浏览器界面等）用 CSS 过渡/动画即可获得与系统一致的弹簧手感，
//! 而且动画可以在 WebKit 的合成线程执行，不占用 JS 主线程。

use crate::Spring;

/// 生成的 CSS 缓动。
#[derive(Debug, Clone, PartialEq)]
pub struct CssEasing {
    /// 动画时长（毫秒），即弹簧从 0 运动到静止于 1 所需的时间。
    pub duration_ms: u32,
    /// 形如 `linear(0, 0.42 10%, …, 1)` 的缓动函数。
    pub easing: String,
}

/// 从静止的 0 运动到 1 的弹簧曲线，用折线近似，最大误差不超过 `tolerance`（如 0.002）。
pub fn spring_easing(spring: Spring, tolerance: f64) -> CssEasing {
    let duration = spring.settling_duration(-1.0, 0.0, 0.001, 0.01, 5.0);
    const SAMPLES: usize = 400;
    let points: Vec<(f64, f64)> = (0..=SAMPLES)
        .map(|i| {
            let p = i as f64 / SAMPLES as f64;
            let (x, _) = spring.solve(-1.0, 0.0, p * duration);
            (p, 1.0 + x)
        })
        .collect();
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    simplify(&points, 0, points.len() - 1, tolerance, &mut keep);

    let last = points.len() - 1;
    let parts: Vec<String> = points
        .iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(i, &(p, y))| {
            if i == 0 {
                "0".to_string()
            } else if i == last {
                "1".to_string()
            } else {
                format!("{} {}%", trim(y, 4), trim(p * 100.0, 2))
            }
        })
        .collect();
    CssEasing {
        duration_ms: (duration * 1000.0).round() as u32,
        easing: format!("linear({})", parts.join(", ")),
    }
}

/// Ramer–Douglas–Peucker 折线简化（按纵向误差）。
fn simplify(pts: &[(f64, f64)], a: usize, b: usize, tol: f64, keep: &mut [bool]) {
    if b <= a + 1 {
        return;
    }
    let (x0, y0) = pts[a];
    let (x1, y1) = pts[b];
    let mut worst = (a, 0.0);
    for (i, &(x, y)) in pts.iter().enumerate().take(b).skip(a + 1) {
        let y_line = y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        let err = (y - y_line).abs();
        if err > worst.1 {
            worst = (i, err);
        }
    }
    if worst.1 > tol {
        keep[worst.0] = true;
        simplify(pts, a, worst.0, tol, keep);
        simplify(pts, worst.0, b, tol, keep);
    }
}

fn trim(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(easing: &str) -> Vec<(f64, f64)> {
        let inner = easing.trim_start_matches("linear(").trim_end_matches(')');
        let parts: Vec<&str> = inner.split(", ").collect();
        let n = parts.len();
        parts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut it = p.split(' ');
                let y: f64 = it.next().unwrap().parse().unwrap();
                let x = match it.next() {
                    Some(pct) => pct.trim_end_matches('%').parse::<f64>().unwrap() / 100.0,
                    None => i as f64 / (n - 1) as f64,
                };
                (x, y)
            })
            .collect()
    }

    #[test]
    fn easing_is_compact_and_accurate() {
        for spring in [Spring::SMOOTH, Spring::SNAPPY, Spring::BOUNCY] {
            let e = spring_easing(spring, 0.002);
            assert!(e.easing.starts_with("linear(0, ") && e.easing.ends_with(", 1)"));
            let pts = parse(&e.easing);
            assert!(pts.len() < 60, "{} 个点太多", pts.len());
            let dur = e.duration_ms as f64 / 1000.0;
            // 折线在任意位置与真实弹簧的误差都应很小。
            for i in 0..=200 {
                let p = i as f64 / 200.0;
                let w = pts.windows(2).find(|w| p >= w[0].0 && p <= w[1].0).unwrap();
                let y_lin =
                    w[0].1 + (w[1].1 - w[0].1) * (p - w[0].0) / (w[1].0 - w[0].0).max(1e-12);
                let y_true = 1.0 + spring.solve(-1.0, 0.0, p * dur).0;
                assert!(
                    (y_lin - y_true).abs() < 0.004,
                    "{spring:?} p={p}: {y_lin} vs {y_true}"
                );
            }
        }
    }

    #[test]
    fn bouncy_overshoots_in_css_too() {
        let e = spring_easing(Spring::BOUNCY, 0.002);
        // ζ=0.7 理论超调约 4.6%。
        assert!(parse(&e.easing).iter().any(|&(_, y)| y > 1.04));
    }
}
