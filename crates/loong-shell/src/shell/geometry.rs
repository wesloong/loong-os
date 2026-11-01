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

/// 应用窗口在屏幕上的摆放：窗口的几何区域（xdg geometry）画在 `rect`，整个窗口（含子表面）
/// 按 `scale = rect.w / 屏幕宽` 等比缩放（打开、回主屏、多任务时窗口随卡片缩放）。
///
/// “窗口坐标”以窗口根表面的左上角为原点（Smithay `Window` 的 (0, 0)），几何区域从
/// `geometry_loc` 开始。渲染与触摸共用这一个换算，保证手指落在画面上的哪个表面就交给哪个表面。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPlacement {
    pub rect: RectF,
    pub scale: f64,
}

impl WindowPlacement {
    pub fn new(rect: RectF, screen_w: f64) -> Self {
        WindowPlacement {
            rect,
            scale: rect.w / screen_w.max(1.0),
        }
    }

    /// 窗口坐标原点（根表面左上角）在屏幕上的位置。
    pub fn origin(&self, geometry_loc: (f64, f64)) -> (f64, f64) {
        (
            self.rect.x - geometry_loc.0 * self.scale,
            self.rect.y - geometry_loc.1 * self.scale,
        )
    }

    /// 屏幕上的点 → 窗口坐标。
    pub fn to_window(self, (x, y): (f64, f64), geometry_loc: (f64, f64)) -> (f64, f64) {
        let (ox, oy) = self.origin(geometry_loc);
        let k = self.scale.max(1e-6);
        ((x - ox) / k, (y - oy) / k)
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

    #[test]
    fn window_placement_maps_screen_to_window() {
        // 全屏：屏幕坐标 = 窗口坐标（几何区域从根表面原点开始时）。
        let full = WindowPlacement::new(RectF::new(0.0, 0.0, 1280.0, 800.0), 1280.0);
        assert_eq!(full.to_window((100.0, 30.0), (0.0, 0.0)), (100.0, 30.0));
        // 卡片缩到一半、移到 (320, 200)：屏幕上卡片内的点按比例放大回窗口坐标。
        let half = WindowPlacement::new(RectF::new(320.0, 200.0, 640.0, 400.0), 1280.0);
        assert_eq!(half.scale, 0.5);
        assert_eq!(half.to_window((320.0, 200.0), (0.0, 0.0)), (0.0, 0.0));
        assert_eq!(half.to_window((640.0, 400.0), (0.0, 0.0)), (640.0, 400.0));
        // 子表面 (490, 308) 处（窗口坐标）的一点，画在屏幕 (565, 354)。
        assert_eq!(half.to_window((565.0, 354.0), (0.0, 0.0)), (490.0, 308.0));
        // 几何区域不从根表面原点开始（例如客户端画了阴影）：几何区域的左上角对齐卡片。
        let geo = (10.0, 20.0);
        assert_eq!(half.origin(geo), (315.0, 190.0));
        assert_eq!(half.to_window((320.0, 200.0), geo), (10.0, 20.0));
    }
}
