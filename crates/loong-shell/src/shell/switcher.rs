//! 多任务界面：卡片布局、横向滚动与吸附、手势判定。
//!
//! 这里只放与 GPU、Wayland 无关的纯计算，便于单元测试；状态与画面见 `shell/mod.rs`。
//!
//! 卡片按最近使用排成一行：最旧的在最左，最近的在最右（与设计规范 8.6 一致）。
//! 吸附位置让相邻两张卡片并排完整显示，较新的一张在右侧；只有一张卡片时居中。

use loong_motion::{Decay, RUBBER_BAND_COEFFICIENT, rubber_band, rubber_band_inverse, snap};

use super::gesture::{rubber_clamp, rubber_clamp_inverse};

/// 卡片相对屏幕的缩放。
pub const CARD_SCALE: f64 = 0.45;
/// 卡片间距（点）。
pub const CARD_GAP: f64 = 32.0;
/// 卡片圆角（`panel` 令牌）。
pub const CARD_RADIUS: f64 = 22.0;
/// 卡片上方标签中的应用图标边长。
pub const LABEL_ICON: f64 = 24.0;
/// 卡片上方标签（图标 + 名称）位图的逻辑高度，图标在其中垂直居中。
pub const LABEL_HEIGHT: f64 = 32.0;
/// 标签图标底边到卡片顶边的距离。
pub const LABEL_GAP: f64 = 8.0;
/// 卡片阴影（`shadowHigh`）：模糊向四周扩展的距离。
pub const SHADOW_SPREAD: f64 = 48.0;
/// 卡片阴影向下的偏移。
pub const SHADOW_OFFSET_Y: f64 = 16.0;

/// 停顿进入多任务：速度低于该值（点/秒）视为停顿。
pub const PAUSE_SPEED: f64 = 100.0;
/// 停顿进入多任务：停顿需要持续的时间（秒）。
pub const PAUSE_DURATION: f64 = 0.1;

/// 底边上滑中的“停顿”检测（设计规范 9.1 / 9.2）：上移距离达到 `min_distance` 后，
/// 手指速度持续低于 [`PAUSE_SPEED`] 达到 [`PAUSE_DURATION`]，即进入多任务。
///
/// 手指静止时触摸屏通常不再上报事件，所以除了每次触摸移动，还要每帧调用一次。
#[derive(Debug, Clone, Copy, Default)]
pub struct PauseDetector {
    slow_since: Option<f64>,
}

impl PauseDetector {
    /// 送入 `now` 时刻的上移距离与手指速度，返回是否满足停顿条件。
    pub fn update(&mut self, now: f64, distance: f64, speed: f64, min_distance: f64) -> bool {
        if distance < min_distance || speed >= PAUSE_SPEED {
            self.slow_since = None;
            return false;
        }
        let since = *self.slow_since.get_or_insert(now);
        now - since >= PAUSE_DURATION
    }
}

/// 标签位图左上角的 y：卡片顶边在 `card_y`、相对标准卡片的缩放为 `k`。
pub fn label_top(card_y: f64, k: f64) -> f64 {
    card_y - (LABEL_GAP + LABEL_ICON + (LABEL_HEIGHT - LABEL_ICON) / 2.0) * k
}

/// 卡片上下拖动：手指位移 → 卡片位移。向上 1:1 跟手；向下按橡皮筋阻尼（d = 卡片高度）。
pub fn card_drag_offset(dy: f64, card_h: f64) -> f64 {
    if dy < 0.0 {
        dy
    } else {
        rubber_band(dy, card_h, RUBBER_BAND_COEFFICIENT)
    }
}

/// [`card_drag_offset`] 的反函数：手指按到正在回位的卡片时，由卡片位移还原手指位移。
pub fn card_drag_inverse(offset: f64, card_h: f64) -> f64 {
    if offset < 0.0 {
        offset
    } else {
        rubber_band_inverse(offset, card_h, RUBBER_BAND_COEFFICIENT)
    }
}

/// 上滑关闭卡片的判定：`lift` 为卡片上移的距离（点，向上为正），`velocity` 为向上的速度
/// （点/秒）。按设计规范 9.1，`should_complete(lift / 卡片高度, …, FAST)`。
pub fn should_dismiss(lift: f64, velocity: f64, card_h: f64) -> bool {
    card_h > 0.0 && snap::should_complete(lift / card_h, velocity / card_h, Decay::FAST)
}

/// 多任务界面的几何布局。
///
/// 滚动位置 `scroll` 是“内容坐标”中对准屏幕中线的位置；第 i 张卡片的中心在内容坐标
/// `i × stride` 处。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SwitcherLayout {
    pub w: f64,
    pub h: f64,
    pub card_w: f64,
    pub card_h: f64,
    /// 相邻卡片中心的距离。
    pub stride: f64,
    /// 卡片中心的 y。
    pub center_y: f64,
}

impl SwitcherLayout {
    pub fn new(w: f64, h: f64) -> Self {
        let card_w = (w * CARD_SCALE).round();
        let card_h = (h * CARD_SCALE).round();
        // 卡片和上方的标签作为一个整体在屏幕上垂直居中。
        let center_y = (h / 2.0 + (LABEL_GAP + LABEL_ICON) / 2.0).round();
        SwitcherLayout {
            w,
            h,
            card_w,
            card_h,
            stride: card_w + CARD_GAP,
            center_y,
        }
    }

    /// `n` 张卡片时的滚动范围。
    pub fn scroll_range(&self, n: usize) -> (f64, f64) {
        if n <= 1 {
            (0.0, 0.0)
        } else {
            (self.stride / 2.0, (n as f64 - 1.5) * self.stride)
        }
    }

    /// 吸附位置：每个位置让第 k、k+1 张卡片并排显示。
    pub fn snap_points(&self, n: usize) -> Vec<f64> {
        if n <= 1 {
            vec![0.0]
        } else {
            (0..n - 1).map(|k| (k as f64 + 0.5) * self.stride).collect()
        }
    }

    /// 进入多任务时的滚动位置：最近的应用（最后一张）在右侧。
    pub fn initial_scroll(&self, n: usize) -> f64 {
        self.scroll_range(n).1
    }

    /// 第 `i` 张卡片在滚动位置 `scroll` 时的矩形 [x, y, w, h, 圆角]。
    pub fn slot(&self, i: usize, scroll: f64) -> [f64; 5] {
        let cx = self.w / 2.0 + i as f64 * self.stride - scroll;
        [
            cx - self.card_w / 2.0,
            self.center_y - self.card_h / 2.0,
            self.card_w,
            self.card_h,
            CARD_RADIUS,
        ]
    }

    /// 拖动时：手指对应的滚动位置 → 显示的滚动位置（越界部分按橡皮筋，d = 屏幕宽度）。
    pub fn rubber_scroll(&self, raw: f64, n: usize) -> f64 {
        let (lo, hi) = self.scroll_range(n);
        rubber_clamp(raw, lo, hi, self.w)
    }

    /// [`rubber_scroll`](Self::rubber_scroll) 的反函数：手指按到正在回弹的列表时还原手指位置。
    pub fn unrubber_scroll(&self, shown: f64, n: usize) -> f64 {
        let (lo, hi) = self.scroll_range(n);
        rubber_clamp_inverse(shown, lo, hi, self.w)
    }

    /// 是否在滚动范围内（允许微小误差）。
    pub fn in_range(&self, scroll: f64, n: usize) -> bool {
        let (lo, hi) = self.scroll_range(n);
        scroll >= lo - 0.5 && scroll <= hi + 0.5
    }

    /// 松手后的滚动目标：用 `FAST` 把速度投影成预计落点，再吸附到最近的卡片位置。
    /// 越界松手时，投影落点也会被吸附到边界上的位置。
    pub fn release_target(&self, scroll: f64, velocity: f64, n: usize) -> f64 {
        snap::nearest_snap_point(scroll, velocity, Decay::FAST, &self.snap_points(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> SwitcherLayout {
        SwitcherLayout::new(1280.0, 800.0)
    }

    #[test]
    fn two_cards_fit_side_by_side_with_newest_on_the_right() {
        let l = layout();
        for (w, h) in [(1280.0, 800.0), (800.0, 1280.0), (1024.0, 768.0)] {
            let l = SwitcherLayout::new(w, h);
            let s = l.initial_scroll(2);
            let (a, b) = (l.slot(0, s), l.slot(1, s));
            assert!(a[0] >= 0.0 && b[0] + b[2] <= w, "{w}x{h}");
            assert!(a[0] + a[2] < b[0], "两张卡片之间有间距");
            assert!((a[0] + b[0] + b[2] - w).abs() < 1e-9, "整体水平居中");
            // 标签在状态栏下方。
            assert!(label_top(a[1], 1.0) > 24.0);
            assert!(a[1] + a[3] < h - 20.0);
        }
        // 一张卡片时居中。
        let c = l.slot(0, l.initial_scroll(1));
        assert!((c[0] + c[2] / 2.0 - 640.0).abs() < 1e-9);
        // 多张卡片时，最近的一张在右侧，并且能看到更早一张的边缘。
        let s = l.initial_scroll(4);
        assert!(l.slot(3, s)[0] > 640.0);
        let older = l.slot(1, s);
        assert!(older[0] + older[2] > 0.0 && older[0] < 0.0);
    }

    #[test]
    fn scroll_snapping_uses_projection_and_stays_in_range() {
        let l = layout();
        let n = 5;
        let pts = l.snap_points(n);
        assert_eq!(pts.len(), 4);
        let (lo, hi) = l.scroll_range(n);
        assert_eq!((pts[0], pts[3]), (lo, hi));
        // 慢慢拖过不到一半：回到原位；过半：去下一张。
        let s = l.stride;
        assert_eq!(l.release_target(pts[1] + 0.4 * s, 0.0, n), pts[1]);
        assert_eq!(l.release_target(pts[1] + 0.6 * s, 0.0, n), pts[2]);
        // 只拖了一点，但甩得快：去下一张。
        assert_eq!(l.release_target(pts[1] + 0.1 * s, 3000.0, n), pts[2]);
        assert_eq!(l.release_target(pts[1] - 0.1 * s, -3000.0, n), pts[0]);
        // 大力甩：吸附到边界，不会越界。
        assert_eq!(l.release_target(pts[1], 1e6, n), hi);
        assert_eq!(l.release_target(lo - 100.0, -1e6, n), lo);
        // 越界后松手（无速度）：回到边界。
        assert_eq!(l.release_target(hi + 80.0, 0.0, n), hi);
    }

    #[test]
    fn rubber_band_scroll_is_invertible() {
        let l = layout();
        let n = 3;
        let (lo, hi) = l.scroll_range(n);
        assert_eq!(l.rubber_scroll(lo + 10.0, n), lo + 10.0);
        let over = l.rubber_scroll(hi + 300.0, n);
        assert!(over > hi && over < hi + 300.0 * 0.56);
        for raw in [lo - 400.0, lo - 1.0, (lo + hi) / 2.0, hi + 50.0, hi + 900.0] {
            let shown = l.rubber_scroll(raw, n);
            assert!((l.unrubber_scroll(shown, n) - raw).abs() < 1e-6, "{raw}");
        }
        // 只有一张卡片时，任意方向拖动都是橡皮筋。
        assert!(l.rubber_scroll(200.0, 1).abs() < 200.0 * 0.56);
    }

    #[test]
    fn pause_detection() {
        let min = 100.0;
        // 移动距离不够：再慢也不进入。
        let mut p = PauseDetector::default();
        assert!(!p.update(0.0, 50.0, 0.0, min));
        assert!(!p.update(0.5, 50.0, 0.0, min));
        // 距离够了，速度低于阈值持续 100ms 才进入。
        let mut p = PauseDetector::default();
        assert!(!p.update(1.0, 150.0, 40.0, min));
        assert!(!p.update(1.05, 150.0, 60.0, min));
        assert!(p.update(1.1, 150.0, 20.0, min));
        // 中途又快速移动：重新计时。
        let mut p = PauseDetector::default();
        assert!(!p.update(2.0, 150.0, 50.0, min));
        assert!(!p.update(2.06, 160.0, 400.0, min));
        assert!(!p.update(2.12, 170.0, 50.0, min));
        assert!(!p.update(2.2, 170.0, 50.0, min));
        assert!(p.update(2.23, 170.0, 50.0, min));
        // 一直快速移动（甩动）：不进入。
        let mut p = PauseDetector::default();
        for i in 0..30 {
            assert!(!p.update(i as f64 / 60.0, 100.0 + i as f64 * 20.0, 1200.0, min));
        }
    }

    #[test]
    fn dismiss_decision() {
        let h = 360.0;
        // 慢慢上推不到一半：不关闭；超过一半：关闭。
        assert!(!should_dismiss(120.0, 0.0, h));
        assert!(should_dismiss(200.0, 0.0, h));
        // 轻轻上甩：关闭。
        assert!(should_dismiss(60.0, 1800.0, h));
        // 上推很多但往回拉：不关闭。
        assert!(!should_dismiss(250.0, -1500.0, h));
        // 向下拖：不关闭。
        assert!(!should_dismiss(-40.0, -300.0, h));
    }

    #[test]
    fn card_drag_mapping() {
        let h = 360.0;
        assert_eq!(card_drag_offset(-120.0, h), -120.0);
        let down = card_drag_offset(200.0, h);
        assert!(down > 0.0 && down < 200.0 * 0.56);
        for dy in [-300.0, -1.0, 0.0, 30.0, 500.0] {
            let off = card_drag_offset(dy, h);
            assert!((card_drag_inverse(off, h) - dy).abs() < 1e-6);
        }
    }
}
