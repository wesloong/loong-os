//! 逻辑坐标（点）下的几何类型。

/// 矩形（逻辑点）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectF {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        RectF { x, y, w, h }
    }

    /// 以中心点和尺寸构造。
    pub fn centered(cx: f64, cy: f64, w: f64, h: f64) -> Self {
        RectF::new(cx - w / 2.0, cy - h / 2.0, w, h)
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// 以 `(ox, oy)` 为中心缩放 `s` 倍。
    pub fn scaled_about(&self, ox: f64, oy: f64, s: f64) -> RectF {
        RectF::new(
            ox + (self.x - ox) * s,
            oy + (self.y - oy) * s,
            self.w * s,
            self.h * s,
        )
    }

    /// 按比例缩放 `(w, h)` 使其铺满 `self`（aspect fill，可能超出），居中。
    pub fn fill(&self, w: f64, h: f64) -> RectF {
        if w <= 0.0 || h <= 0.0 {
            return *self;
        }
        let s = (self.w / w).max(self.h / h);
        let (cx, cy) = self.center();
        RectF::centered(cx, cy, w * s, h * s)
    }
}

/// 线性插值。
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// 平滑阶梯函数。
pub fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_covers_and_keeps_center() {
        let r = RectF::new(0.0, 0.0, 100.0, 100.0).fill(200.0, 100.0);
        assert_eq!(r, RectF::new(-50.0, 0.0, 200.0, 100.0));
    }

    #[test]
    fn scale_about_center() {
        let r = RectF::new(0.0, 0.0, 100.0, 50.0).scaled_about(50.0, 25.0, 0.5);
        assert_eq!(r, RectF::new(25.0, 12.5, 50.0, 25.0));
    }
}
