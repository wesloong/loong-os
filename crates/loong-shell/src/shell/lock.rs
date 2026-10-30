//! 锁屏：时间、日期，上滑解锁。
//!
//! 锁屏是覆盖在最上层的一张“纸”。手指上滑时它 1:1 跟手上移；往下拉时按橡皮筋阻尼；
//! 松手后用速度投影判断是否解锁——解锁则带着手指速度弹走，否则弹回。

use loong_motion::{
    Decay, RUBBER_BAND_COEFFICIENT, Spring, SpringAnimation, VelocityTracker, rubber_band, snap,
};

struct Drag {
    slot: u32,
    start_y: f64,
    start_offset: f64,
}

/// 锁屏状态。
pub struct LockScreen {
    h: f64,
    /// 锁屏层的垂直偏移（点）：0 表示盖满屏幕，-h 表示完全移出。
    offset: SpringAnimation<f64>,
    locked: bool,
    drag: Option<Drag>,
    tracker: VelocityTracker<f64>,
}

impl LockScreen {
    pub fn new(h: f64, locked: bool) -> Self {
        let offset = if locked { 0.0 } else { -h };
        LockScreen {
            h,
            offset: SpringAnimation::at_rest(Spring::SMOOTH, offset, 0.0)
                .with_rest_thresholds(0.1, 1.0),
            locked,
            drag: None,
            tracker: VelocityTracker::new(),
        }
    }

    /// 锁定（例如按电源键）：锁屏从上方落下。
    pub fn lock(&mut self, now: f64) {
        self.locked = true;
        self.drag = None;
        self.offset.set_spring(Spring::SNAPPY, now);
        self.offset.retarget(0.0, now);
    }

    /// 是否处于锁定状态（锁定时所有触摸都由锁屏处理）。
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// 是否正在拖动锁屏。
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// 当前偏移（点）。
    pub fn offset(&self, now: f64) -> f64 {
        self.offset.value(now)
    }

    /// 解锁进度：0 = 锁屏盖满，1 = 完全移开。
    pub fn progress(&self, now: f64) -> f64 {
        (-self.offset(now) / self.h).clamp(0.0, 1.0)
    }

    /// 是否需要绘制。
    pub fn is_visible(&self, now: f64) -> bool {
        self.locked || self.offset(now) > -self.h + 0.5
    }

    pub fn is_idle(&self, now: f64) -> bool {
        self.drag.is_none() && self.offset.is_done(now)
    }

    pub fn touch_down(&mut self, slot: u32, y: f64, now: f64) {
        if self.drag.is_some() {
            return;
        }
        // 接住正在进行的动画，从当前位置继续跟手。
        let current = self.offset(now);
        self.offset.jump_to(current, 0.0, now);
        self.tracker.reset();
        self.tracker.add(now, y);
        self.drag = Some(Drag {
            slot,
            start_y: y,
            start_offset: current,
        });
    }

    pub fn touch_motion(&mut self, slot: u32, y: f64, now: f64) {
        let Some(drag) = &self.drag else { return };
        if drag.slot != slot {
            return;
        }
        self.tracker.add(now, y);
        let raw = drag.start_offset + (y - drag.start_y);
        // 往下拉（偏移为正）时按橡皮筋阻尼，最多移出一屏。
        let offset = if raw > 0.0 {
            rubber_band(raw, self.h, RUBBER_BAND_COEFFICIENT)
        } else {
            raw.max(-self.h)
        };
        self.offset.jump_to(offset, 0.0, now);
    }

    pub fn touch_up(&mut self, slot: u32, now: f64) {
        if self.drag.as_ref().is_none_or(|d| d.slot != slot) {
            return;
        }
        self.drag = None;
        let v = self.tracker.velocity_at(now);
        let current = self.offset(now);
        // 与回主屏手势相同：参考距离 D = 0.25 × 屏幕高度，FAST 投影。
        let reference = self.h * 0.25;
        if snap::should_complete(-current / reference, -v / reference, Decay::FAST) {
            self.locked = false;
            self.offset = SpringAnimation::new(Spring::SMOOTH, current, -self.h, v, now)
                .with_rest_thresholds(0.1, 1.0);
        } else {
            self.offset = SpringAnimation::new(Spring::SNAPPY, current, 0.0, v, now)
                .with_rest_thresholds(0.1, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swipe(lock: &mut LockScreen, from: f64, to: f64, t0: f64, dur: f64) -> f64 {
        lock.touch_down(0, from, t0);
        let steps = 12;
        for i in 1..=steps {
            let f = i as f64 / steps as f64;
            lock.touch_motion(0, from + (to - from) * f, t0 + dur * f);
        }
        let t_up = t0 + dur + 0.001;
        lock.touch_up(0, t_up);
        t_up
    }

    #[test]
    fn quick_flick_unlocks() {
        let mut lock = LockScreen::new(800.0, true);
        let t = swipe(&mut lock, 700.0, 600.0, 0.0, 0.08);
        assert!(!lock.is_locked());
        assert!(lock.is_visible(t));
        assert!(!lock.is_visible(t + 3.0));
        assert!((lock.progress(t + 3.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn short_slow_drag_bounces_back() {
        let mut lock = LockScreen::new(800.0, true);
        lock.touch_down(0, 700.0, 0.0);
        lock.touch_motion(0, 660.0, 0.5);
        lock.touch_up(0, 0.6);
        assert!(lock.is_locked());
        assert!(lock.offset(3.0).abs() < 0.2);
    }

    #[test]
    fn pulling_down_is_rubber_banded() {
        let mut lock = LockScreen::new(800.0, true);
        lock.touch_down(0, 100.0, 0.0);
        lock.touch_motion(0, 500.0, 0.2);
        let off = lock.offset(0.2);
        assert!(off > 0.0 && off < 400.0 * RUBBER_BAND_COEFFICIENT + 1.0);
    }

    #[test]
    fn lock_again() {
        let mut lock = LockScreen::new(800.0, false);
        assert!(!lock.is_visible(0.0));
        lock.lock(1.0);
        assert!(lock.is_locked() && lock.is_visible(1.0));
        assert!(lock.offset(3.0).abs() < 0.2);
    }
}
