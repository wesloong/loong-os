//! 自动化测试脚本：用虚拟时钟回放触摸操作，并在指定时刻截图 / 录制动画。
//!
//! ```json
//! {
//!   "fps": 60,
//!   "duration_ms": 2500,
//!   "events": [
//!     { "at": 300, "tap": [640, 330] },
//!     { "at": 1400, "swipe": { "from": [640, 798], "to": [640, 420], "duration": 180 } }
//!   ],
//!   "screenshots": [0, 450, 1450, 2400],
//!   "record": { "every": 2, "scale": 0.5 }
//! }
//! ```
//! 坐标单位为逻辑点，时间单位为毫秒。
//!
//! 屏幕键盘（键的位置在运行时按当前布局求出，脚本不写坐标）：
//!
//! ```json
//! { "at": 1200, "text_input": "normal" }
//! { "at": 2000, "type": "nihao", "interval": 250 }
//! { "at": 3500, "candidate": 0 }
//! { "at": 4000, "key": "lang" }
//! ```
//!
//! - `text_input`：模拟应用的输入框获得焦点（内容用途，如 `normal`、`url`、`number`），`off` 为失去焦点。
//!   真实客户端（WPE WebKit）经 text-input-v3 激活键盘，不需要它；
//! - `type`：依次点按这些字符所在的键（`" "` 空格，`"\n"` 换行，`"\b"` 退格），间隔 `interval` 毫秒；
//! - `candidate`：点按候选栏本页第 i 个候选；
//! - `key`：点按功能键：`shift` / `lang` / `123` / `symbols` / `abc` / `hide` / `space` / `return` / `backspace`。

use loong_ime::{
    ContentPurpose,
    keyboard::{KeyKind, Layer},
};
use serde::Deserialize;

use crate::shell::keyboard::ScreenKeyboard;

fn default_fps() -> u32 {
    60
}
fn default_every() -> u32 {
    2
}
fn default_record_scale() -> f64 {
    0.5
}

/// 脚本。
#[derive(Debug, Deserialize)]
pub struct Script {
    #[serde(default = "default_fps")]
    pub fps: u32,
    pub duration_ms: u64,
    /// 固定状态栏时间，保证截图可复现。
    #[serde(default)]
    pub clock: Option<String>,
    /// 固定锁屏日期文字。
    #[serde(default)]
    pub date: Option<String>,
    /// 开始时是否处于锁屏。
    #[serde(default)]
    pub locked: bool,
    /// 外观（`"light"` / `"dark"`），覆盖设备描述与环境变量，保证截图可复现。
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub events: Vec<Event>,
    #[serde(default)]
    pub screenshots: Vec<u64>,
    #[serde(default)]
    pub record: Option<Record>,
}

/// 一个操作。
#[derive(Debug, Deserialize)]
pub struct Event {
    pub at: u64,
    #[serde(default)]
    pub tap: Option<[f64; 2]>,
    #[serde(default)]
    pub swipe: Option<Swipe>,
    /// 直接打开某个应用（按 id）。
    #[serde(default)]
    pub launch: Option<String>,
    /// 模拟输入框获得焦点（内容用途，如 `"normal"`、`"url"`、`"number"`），`"off"` 为失去焦点。
    #[serde(default)]
    pub text_input: Option<String>,
    /// 在屏幕键盘上依次点按这些字符所在的键。
    #[serde(default, rename = "type")]
    pub type_text: Option<String>,
    /// `type` 中相邻两次点按的间隔（毫秒）。
    #[serde(default = "default_type_interval")]
    pub interval: u64,
    /// 点按候选栏本页第 i 个候选。
    #[serde(default)]
    pub candidate: Option<usize>,
    /// 点按屏幕键盘的功能键。
    #[serde(default)]
    pub key: Option<String>,
}

fn default_type_interval() -> u64 {
    200
}

/// 屏幕键盘上要点按的目标（位置在运行时按当前布局求出）。
#[derive(Debug, Clone, PartialEq)]
pub enum KeyTarget {
    /// 打出这个字符的键。
    Char(char),
    /// 功能键（shift / lang / 123 / symbols / abc / hide / space / return / backspace）。
    Named(String),
    /// 候选栏本页第 i 个候选。
    Candidate(usize),
}

/// 滑动。
#[derive(Debug, Deserialize)]
pub struct Swipe {
    pub from: [f64; 2],
    pub to: [f64; 2],
    /// 滑动用时（毫秒）。
    pub duration: u64,
    /// 到达终点后停留多久再抬起（毫秒）。停留会让松手速度归零。
    #[serde(default)]
    pub hold: u64,
}

/// 录制设置。
#[derive(Debug, Deserialize)]
pub struct Record {
    /// 每隔几帧录一帧。
    #[serde(default = "default_every")]
    pub every: u32,
    /// 录制分辨率相对屏幕的比例（0.5 = 一半）。
    #[serde(default = "default_record_scale")]
    pub scale: f64,
}

/// 展开后的底层动作。
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Down {
        slot: u32,
        x: f64,
        y: f64,
    },
    Move {
        slot: u32,
        x: f64,
        y: f64,
    },
    Up {
        slot: u32,
    },
    Launch(String),
    /// 模拟输入框获得焦点（内容用途）/ 失去焦点（`None`）。
    TextInput(Option<String>),
    /// 手指按在屏幕键盘的某个目标上。
    KeyDown {
        slot: u32,
        target: KeyTarget,
    },
}

impl Script {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(text)?)
    }

    /// 无脚本时：只渲染主屏一帧。
    pub fn still() -> Self {
        Script {
            fps: 60,
            duration_ms: 0,
            clock: None,
            date: None,
            locked: false,
            theme: None,
            events: vec![],
            screenshots: vec![0],
            record: None,
        }
    }

    /// 展开为按时间排序的底层动作（时间单位：秒）。
    pub fn expand(&self) -> Vec<(f64, Action)> {
        let mut out = Vec::new();
        for (i, e) in self.events.iter().enumerate() {
            let t0 = e.at as f64 / 1000.0;
            let slot = i as u32;
            if let Some([x, y]) = e.tap {
                out.push((t0, Action::Down { slot, x, y }));
                out.push((t0 + 0.06, Action::Up { slot }));
            }
            if let Some(s) = &e.swipe {
                let dur = s.duration as f64 / 1000.0;
                out.push((
                    t0,
                    Action::Down {
                        slot,
                        x: s.from[0],
                        y: s.from[1],
                    },
                ));
                // 以 120Hz 采样手指位置（与真实触摸屏相当）。
                let steps = ((dur * 120.0).ceil() as usize).max(1);
                for k in 1..=steps {
                    let f = k as f64 / steps as f64;
                    let x = s.from[0] + (s.to[0] - s.from[0]) * f;
                    let y = s.from[1] + (s.to[1] - s.from[1]) * f;
                    out.push((t0 + dur * f, Action::Move { slot, x, y }));
                }
                out.push((
                    t0 + dur + s.hold as f64 / 1000.0 + 0.001,
                    Action::Up { slot },
                ));
            }
            if let Some(app) = &e.launch {
                out.push((t0, Action::Launch(app.clone())));
            }
            if let Some(purpose) = &e.text_input {
                let purpose = (purpose != "off").then(|| purpose.clone());
                out.push((t0, Action::TextInput(purpose)));
            }
            let mut tap_key = |t: f64, target: KeyTarget| {
                out.push((t, Action::KeyDown { slot, target }));
                out.push((t + 0.06, Action::Up { slot }));
            };
            if let Some(text) = &e.type_text {
                let step = e.interval.max(80) as f64 / 1000.0;
                for (k, c) in text.chars().enumerate() {
                    tap_key(t0 + k as f64 * step, KeyTarget::Char(c));
                }
            }
            if let Some(i) = e.candidate {
                tap_key(t0, KeyTarget::Candidate(i));
            }
            if let Some(name) = &e.key {
                tap_key(t0, KeyTarget::Named(name.clone()));
            }
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out
    }
}

/// 输入框用途的名称（与 `zwp_text_input_v3.content_purpose` 的枚举名相同，小写）。
pub fn content_purpose(name: &str) -> Option<ContentPurpose> {
    use ContentPurpose::*;
    Some(match name {
        "normal" => Normal,
        "alpha" => Alpha,
        "digits" => Digits,
        "number" => Number,
        "phone" => Phone,
        "url" => Url,
        "email" => Email,
        "name" => Name,
        "password" => Password,
        "pin" => Pin,
        "date" => Date,
        "time" => Time,
        "datetime" => Datetime,
        "terminal" => Terminal,
        _ => return None,
    })
}

/// 屏幕键盘上某个目标此刻在屏幕上的位置（键盘没显示、当前层上没有时为 `None`）。
pub fn key_position(kb: &ScreenKeyboard, target: &KeyTarget, now: f64) -> Option<(f64, f64)> {
    if !kb.is_shown() {
        return None;
    }
    match target {
        KeyTarget::Char(c) => kb.key_center(*c, now),
        KeyTarget::Candidate(i) => kb.candidate_center(*i, now),
        KeyTarget::Named(name) => {
            let kind = match name.as_str() {
                "shift" => KeyKind::Shift,
                "lang" => KeyKind::Language,
                "123" => KeyKind::Layer(Layer::Numbers),
                "symbols" => KeyKind::Layer(Layer::Symbols),
                "abc" => KeyKind::Layer(Layer::Letters),
                "hide" => KeyKind::Hide,
                "space" => KeyKind::Space,
                "return" => KeyKind::Return,
                "backspace" => KeyKind::Backspace,
                _ => return None,
            };
            kb.kind_center(kind, now)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swipe_expands_to_down_moves_up() {
        let s = Script::parse(
            r#"{"duration_ms": 1000, "events": [{"at": 100, "swipe": {"from": [0, 100], "to": [0, 0], "duration": 100}}]}"#,
        )
        .unwrap();
        let a = s.expand();
        assert_eq!(
            a.first().unwrap().1,
            Action::Down {
                slot: 0,
                x: 0.0,
                y: 100.0
            }
        );
        assert_eq!(a.last().unwrap().1, Action::Up { slot: 0 });
        assert_eq!(a.len(), 2 + 12);
        assert!(a.windows(2).all(|w| w[0].0 <= w[1].0));
    }

    #[test]
    fn keyboard_events_expand_to_key_taps() {
        let s = Script::parse(
            r#"{"duration_ms": 3000, "events": [
                {"at": 100, "text_input": "url"},
                {"at": 500, "type": "ni\b", "interval": 300},
                {"at": 2000, "candidate": 1},
                {"at": 2500, "key": "lang"},
                {"at": 2900, "text_input": "off"}
            ]}"#,
        )
        .unwrap();
        let a = s.expand();
        assert_eq!(a[0], (0.1, Action::TextInput(Some("url".into()))));
        let downs: Vec<(f64, KeyTarget)> = a
            .iter()
            .filter_map(|(t, act)| match act {
                Action::KeyDown { target, .. } => Some((*t, target.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            downs,
            vec![
                (0.5, KeyTarget::Char('n')),
                (0.8, KeyTarget::Char('i')),
                (1.1, KeyTarget::Char('\u{8}')),
                (2.0, KeyTarget::Candidate(1)),
                (2.5, KeyTarget::Named("lang".into())),
            ]
        );
        // 每次按下 60ms 后抬起。
        let ups = a
            .iter()
            .filter(|(_, act)| matches!(act, Action::Up { .. }))
            .count();
        assert_eq!(ups, 5);
        assert_eq!(a.last().unwrap().1, Action::TextInput(None));
    }
}
