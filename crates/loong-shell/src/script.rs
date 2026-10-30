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

use serde::Deserialize;

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
    Down { slot: u32, x: f64, y: f64 },
    Move { slot: u32, x: f64, y: f64 },
    Up { slot: u32 },
    Launch(String),
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
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out
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
}
