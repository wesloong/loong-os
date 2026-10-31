//! 触摸输入：先交给系统手势识别，没被消费的再转发给应用。
//!
//! 转发时按窗口此刻在屏幕上的摆放（[`WindowPlacement`]，可能正随卡片缩放）把触点换算成窗口
//! 坐标，交给手指下面的那个表面（根表面、子表面或弹出菜单）。按下时选中的表面会一直收到这根
//! 手指的移动和抬起（隐式抓取），手指移出它的范围也一样，坐标始终相对这个表面。

use smithay::{
    backend::input::InputTime,
    desktop::{Window, WindowSurfaceType},
    input::touch::{DownEvent, MotionEvent, UpEvent},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{
    shell::{TouchRoute, geometry::WindowPlacement},
    state::LoongState,
};

/// 触摸焦点：表面，及其原点的窗口坐标（Smithay 用触点减去它得到表面局部坐标）。
type Focus = Option<(WlSurface, Point<f64, Logical>)>;

/// 屏幕上的点 `(x, y)` 换算成窗口坐标，并找出它下面的表面。
fn surface_at(
    window: &Window,
    placement: WindowPlacement,
    x: f64,
    y: f64,
) -> (Point<f64, Logical>, Focus) {
    let geo = window.geometry().loc;
    let p: Point<f64, Logical> = placement
        .to_window((x, y), (geo.x as f64, geo.y as f64))
        .into();
    let under = window
        .surface_under(p, WindowSurfaceType::ALL)
        .map(|(surface, loc)| (surface, loc.to_f64()));
    (p, under)
}

impl LoongState {
    pub fn touch_down(&mut self, slot: u32, x: f64, y: f64, time_us: u64) {
        let route = self.shell.touch_down(slot, x, y, self.now);
        let TouchRoute::Client(window, placement) = route else {
            return;
        };
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        let (location, focus) = surface_at(&window, placement, x, y);
        // 手指下面没有接收输入的表面：这根手指不交给应用。
        if focus.is_none() {
            return;
        }
        self.client_touches.retain(|(s, _)| *s != slot);
        self.client_touches.push((slot, window));
        let event = DownEvent {
            slot: Some(slot).into(),
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_micros(time_us),
        };
        touch.down(self, focus, &event);
        touch.frame(self);
    }

    pub fn touch_motion(&mut self, slot: u32, x: f64, y: f64, time_us: u64) {
        // 系统手势照常跟踪每根手指；是否交给应用只看这根手指按下时是否交给了应用。
        self.shell.touch_motion(slot, x, y, self.now);
        let Some(window) = self.client_window(slot) else {
            return;
        };
        // 按窗口此刻的摆放换算：多指操作时，窗口可能正被另一根手指拖着缩放。
        let Some((front, placement)) = self.shell.front_window(self.now) else {
            return;
        };
        let Some(touch) = self.seat.get_touch().filter(|_| front == window) else {
            return;
        };
        let (location, focus) = surface_at(&window, placement, x, y);
        let event = MotionEvent {
            slot: Some(slot).into(),
            location,
            time: InputTime::from_micros(time_us),
        };
        touch.motion(self, focus, &event);
        touch.frame(self);
    }

    pub fn touch_up(&mut self, slot: u32, time_us: u64) {
        self.shell.touch_up(slot, self.now);
        let Some(i) = self.client_touches.iter().position(|(s, _)| *s == slot) else {
            return;
        };
        self.client_touches.remove(i);
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        let event = UpEvent {
            slot: Some(slot).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_micros(time_us),
        };
        touch.up(self, &event);
        touch.frame(self);
    }

    /// 这根手指按下时交给了哪个窗口。
    fn client_window(&self, slot: u32) -> Option<Window> {
        self.client_touches
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, w)| w.clone())
    }
}
