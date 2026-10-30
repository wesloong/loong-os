//! # loong-ime
//!
//! Loong OS 的输入法：**librime 中文拼音引擎** + **iPad 式屏幕键盘模型**。
//! 输入法运行在合成器（loong-shell）进程内，键盘由 loong-shell 绘制成图层纹理，
//! 通过 `zwp_text_input_v3` 把文字交给应用（WPE WebKit）。架构与集成步骤见
//! `docs/api/loong-ime.md`。
//!
//! - [`rime`]：librime C API 的最小安全封装（手写 FFI，经 pkg-config 链接）；
//! - [`ImeEngine`]：面向屏幕键盘的拼音引擎（按键、预编辑、候选分页、上屏、中/英）；
//! - [`keyboard`]：键盘布局、命中测试与触摸状态机，输出 [`keyboard::KeyAction`]；
//! - [`InputMethod`]：把键盘与引擎连起来，输出交给合成器的 [`ImeEvent`]。
//!
//! ```no_run
//! use loong_ime::{ContentPurpose, ImeConfig, ImeEngine, ImeEvent, InputMethod};
//! use loong_ime::keyboard::{Keyboard, LayoutParams};
//!
//! // librime 启动（首次运行会部署，阻塞）——合成器里放到后台线程。
//! let engine = ImeEngine::new(&ImeConfig::default()).ok();
//! let keyboard = Keyboard::new(LayoutParams::for_screen(1280.0, 800.0), true, 0.0);
//! let mut im = InputMethod::new(keyboard, engine);
//!
//! im.activate(ContentPurpose::Normal, 0.0); // 输入框获得焦点
//! for event in im.touch_down(0, 300.0, 120.0, 0.1).into_iter().chain(im.touch_up(0, 0.15)) {
//!     match event {
//!         ImeEvent::Commit(text) => { /* text_input.commit_string(text) */ }
//!         ImeEvent::Forward(key) => { /* 合成 wl_keyboard 按键 */ }
//!         ImeEvent::CompositionChanged => { /* 重画候选栏；preedit_string(im.preedit()) */ }
//!         ImeEvent::Hide => { /* 键盘收起动画 */ }
//!     }
//! }
//! ```

#![warn(missing_docs)]

mod config;
mod engine;
mod error;
mod input_method;
pub mod keyboard;
pub mod rime;

pub use config::{
    DEFAULT_PAGE_SIZE, DEFAULT_SCHEMA, DEFAULT_SHARED_DATA_DIR, ImeConfig, LogLevel,
    default_user_data_dir,
};
pub use engine::{ImeEngine, ImeKey, ImeState, KeyOutcome};
pub use error::ImeError;
pub use input_method::{ContentPurpose, ImeEvent, InputMethod};
pub use rime::{Candidate, Preedit, Rime};
