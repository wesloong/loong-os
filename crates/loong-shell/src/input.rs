//! 触摸输入：先交给系统手势识别，没被消费的再转发给应用。

use smithay::{
    backend::input::InputTime,
    input::touch::{DownEvent, MotionEvent, UpEvent},
    utils::SERIAL_COUNTER,
};

use crate::{shell::TouchRoute, state::LoongState};

impl LoongState {
    pub fn touch_down(&mut self, slot: u32, x: f64, y: f64, time_us: u64) {
        let route = self.shell.touch_down(slot, x, y, self.now);
        if let TouchRoute::Client(window, origin) = route {
            let (Some(touch), Some(toplevel)) = (self.seat.get_touch(), window.toplevel()) else {
                return;
            };
            let surface = toplevel.wl_surface().clone();
            let event = DownEvent {
                slot: Some(slot).into(),
                location: (x, y).into(),
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::from_micros(time_us),
            };
            touch.down(self, Some((surface, origin.into())), &event);
            touch.frame(self);
        }
    }

    pub fn touch_motion(&mut self, slot: u32, x: f64, y: f64, time_us: u64) {
        let route = self.shell.touch_motion(slot, x, y, self.now);
        if let TouchRoute::Client(window, origin) = route {
            let (Some(touch), Some(toplevel)) = (self.seat.get_touch(), window.toplevel()) else {
                return;
            };
            let surface = toplevel.wl_surface().clone();
            let event = MotionEvent {
                slot: Some(slot).into(),
                location: (x, y).into(),
                time: InputTime::from_micros(time_us),
            };
            touch.motion(self, Some((surface, origin.into())), &event);
            touch.frame(self);
        }
    }

    pub fn touch_up(&mut self, slot: u32, time_us: u64) {
        let route = self.shell.touch_up(slot, self.now);
        if let TouchRoute::Client(..) = route {
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
    }
}
