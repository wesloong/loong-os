//! 逻辑点（pt）下的矩形。

/// 矩形（逻辑点，原点在键盘左上角，y 向下）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// 左边。
    pub x: f64,
    /// 上边。
    pub y: f64,
    /// 宽。
    pub w: f64,
    /// 高。
    pub h: f64,
}

impl Rect {
    /// 新建矩形。
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect { x, y, w, h }
    }

    /// 由左上角与右下角构造。
    pub fn from_edges(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        Rect::new(left, top, right - left, bottom - top)
    }

    /// 右边。
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    /// 下边。
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    /// 中心点。
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// 面积。
    pub fn area(&self) -> f64 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    /// 点是否在矩形内（左上闭、右下开，相邻矩形不会同时命中）。
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// `other` 是否完全在矩形内。
    pub fn contains_rect(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// 两个矩形是否有面积大于 0 的重叠。
    pub fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    /// 点到矩形的距离（点在矩形内时为 0）。
    pub fn distance_to(&self, x: f64, y: f64) -> f64 {
        let dx = (self.x - x).max(0.0).max(x - self.right());
        let dy = (self.y - y).max(0.0).max(y - self.bottom());
        dx.hypot(dy)
    }

    /// 四边各向外扩 `d`（负数为内缩）。
    pub fn inflate(&self, d: f64) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(10.0, 0.0, 10.0, 10.0);
        assert!(a.contains(0.0, 0.0));
        assert!(!a.contains(10.0, 5.0));
        assert!(b.contains(10.0, 5.0));
        assert!(!a.overlaps(&b), "相邻不算重叠");
    }

    #[test]
    fn distance() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(r.distance_to(5.0, 5.0), 0.0);
        assert_eq!(r.distance_to(13.0, 5.0), 3.0);
        assert_eq!(r.distance_to(13.0, 14.0), 5.0);
    }
}
