//! iPad 式屏幕键盘模型：布局、命中测试与按键状态，不涉及绘制。
//!
//! - [`layout`]：由键盘宽高（逻辑点）算出每个键的位置与触摸区域（[`KeyboardLayout`]）；
//! - [`Keyboard`]：触摸状态机（按下/滑动/抬起、Shift、换层、长按备选、退格连删、
//!   按键高亮与升起动画），输出 [`KeyAction`]；
//! - 键帽内容由 [`Keyboard::label`] 给出，宿主（loong-shell）负责栅格化。

mod geometry;
mod keymap;
pub mod layout;
mod model;

pub use geometry::Rect;
pub use keymap::{KeyKind, Layer};
pub use layout::{
    DEFAULT_BOTTOM_INSET, Hit, Key, KeyId, KeyboardLayout, LayoutParams, Metrics, SizeClass,
    TOUCH_SLOP,
};
pub use model::{
    AlternatesPopup, KEY_RELEASE_SPRING, KeyAction, KeyLabel, KeySymbol, Keyboard,
    LONG_PRESS_DELAY, PRESENT_SPRING, REPEAT_DELAY, REPEAT_INTERVAL, SHIFT_DOUBLE_TAP, ShiftState,
};
