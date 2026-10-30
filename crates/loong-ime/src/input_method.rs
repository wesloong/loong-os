//! 输入法整体：把屏幕键盘（[`Keyboard`]）与拼音引擎（[`ImeEngine`]）连起来，
//! 产生交给宿主（合成器）的 [`ImeEvent`]。
//!
//! 宿主每处理完一批事件，按 `zwp_text_input_v3` 的顺序发给应用：
//! `commit_string`（[`ImeEvent::Commit`]）→ `preedit_string`（[`InputMethod::preedit`]）→ `done`；
//! [`ImeEvent::Forward`] 的键用合成的 `wl_keyboard` 按键发给焦点应用（例如没有组字时的退格）。
//!
//! 引擎是可选的：librime 还在后台启动、或数据缺失时，键盘照常工作，只能输入英文。

use crate::engine::{ImeEngine, ImeKey, KeyOutcome};
use crate::keyboard::{KeyAction, Keyboard, Layer, ShiftState};
use crate::rime::{Candidate, Preedit};

/// 输入框的内容用途（取值与 `zwp_text_input_v3.content_purpose` 相同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ContentPurpose {
    /// 普通文本。
    #[default]
    Normal,
    /// 只允许字母。
    Alpha,
    /// 只允许数字。
    Digits,
    /// 数字（可含小数点、正负号）。
    Number,
    /// 电话号码。
    Phone,
    /// 网址。
    Url,
    /// 电子邮件。
    Email,
    /// 人名。
    Name,
    /// 密码。
    Password,
    /// PIN 码。
    Pin,
    /// 日期。
    Date,
    /// 时间。
    Time,
    /// 日期时间。
    Datetime,
    /// 终端。
    Terminal,
}

impl ContentPurpose {
    /// 由 `zwp_text_input_v3.content_purpose` 的数值转换；未知值当作普通文本。
    pub fn from_text_input_v3(value: u32) -> ContentPurpose {
        use ContentPurpose::*;
        match value {
            1 => Alpha,
            2 => Digits,
            3 => Number,
            4 => Phone,
            5 => Url,
            6 => Email,
            7 => Name,
            8 => Password,
            9 => Pin,
            10 => Date,
            11 => Time,
            12 => Datetime,
            13 => Terminal,
            _ => Normal,
        }
    }

    /// 激活时是否默认用中文（网址、邮箱、密码等默认英文）。
    pub fn allows_chinese(self) -> bool {
        matches!(self, ContentPurpose::Normal | ContentPurpose::Name)
    }

    /// 敏感内容（密码、PIN）：不能切到中文，保证 Rime 的用户词库不会记下密码。
    pub fn is_sensitive(self) -> bool {
        matches!(self, ContentPurpose::Password | ContentPurpose::Pin)
    }

    /// 激活时默认显示的层。
    pub fn initial_layer(self) -> Layer {
        use ContentPurpose::*;
        match self {
            Digits | Number | Phone | Pin | Date | Time | Datetime => Layer::Numbers,
            _ => Layer::Letters,
        }
    }
}

/// 交给宿主的事件（按顺序处理）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImeEvent {
    /// 上屏文字：`zwp_text_input_v3.commit_string`。
    Commit(String),
    /// 输入法不处理的按键：作为普通按键发给焦点应用（合成 `wl_keyboard` 按键）。
    Forward(ImeKey),
    /// 预编辑或候选变了：重画候选栏，并发送 `preedit_string`。
    CompositionChanged,
    /// 用户按了收起键盘。
    Hide,
}

/// 屏幕键盘 + 拼音引擎。
#[derive(Debug)]
pub struct InputMethod {
    keyboard: Keyboard,
    engine: Option<ImeEngine>,
    /// 用户最近一次在普通输入框里选择的中/英模式。
    prefer_chinese: bool,
    purpose: ContentPurpose,
}

impl InputMethod {
    /// 新建。`engine` 为 `None` 时只能输入英文（之后可用 [`InputMethod::set_engine`] 补上）。
    pub fn new(keyboard: Keyboard, engine: Option<ImeEngine>) -> InputMethod {
        let mut im = InputMethod {
            keyboard,
            engine,
            prefer_chinese: true,
            purpose: ContentPurpose::Normal,
        };
        im.sync_mode();
        im
    }

    /// 键盘模型（布局、键帽、高亮、升起进度）。
    pub fn keyboard(&self) -> &Keyboard {
        &self.keyboard
    }

    /// 键盘模型（可变，例如屏幕旋转时 `set_params`）。
    pub fn keyboard_mut(&mut self) -> &mut Keyboard {
        &mut self.keyboard
    }

    /// 拼音引擎。
    pub fn engine(&self) -> Option<&ImeEngine> {
        self.engine.as_ref()
    }

    /// 装上（或去掉）拼音引擎，例如 librime 在后台启动完成后。
    pub fn set_engine(&mut self, engine: Option<ImeEngine>) {
        self.engine = engine;
        self.sync_mode();
    }

    /// 当前是否中文模式。
    pub fn is_chinese(&self) -> bool {
        self.keyboard.chinese()
    }

    /// 预编辑文字（发给应用的 `preedit_string`，也显示在候选栏左侧）。
    pub fn preedit(&self) -> Option<&Preedit> {
        self.engine
            .as_ref()
            .and_then(|e| e.state().preedit.as_ref())
    }

    /// 本页候选。
    pub fn candidates(&self) -> &[Candidate] {
        self.engine
            .as_ref()
            .map_or(&[][..], |e| e.state().candidates.as_slice())
    }

    /// 输入框获得焦点（`zwp_text_input_v3.enable`）：按用途选择中/英与初始层，升起键盘。
    pub fn activate(&mut self, purpose: ContentPurpose, now: f64) -> Vec<ImeEvent> {
        self.run(|im| {
            if let Some(e) = &mut im.engine {
                e.clear();
            }
            im.purpose = purpose;
            im.sync_mode();
            im.keyboard.set_layer(purpose.initial_layer());
            im.keyboard.set_shift(ShiftState::Off);
            im.keyboard.show(now);
            None
        })
    }

    /// 输入框失去焦点（`disable` / 焦点离开）：丢弃组字，收起键盘。
    pub fn deactivate(&mut self, now: f64) -> Vec<ImeEvent> {
        self.run(|im| {
            if let Some(e) = &mut im.engine {
                e.clear();
            }
            im.keyboard.hide(now);
            None
        })
    }

    /// 手指按下（键盘坐标）。落在候选栏的触摸由宿主自己处理。
    pub fn touch_down(&mut self, slot: u32, x: f64, y: f64, now: f64) -> Vec<ImeEvent> {
        let actions = self.keyboard.touch_down(slot, x, y, now);
        self.apply_all(actions, now)
    }

    /// 手指移动。
    pub fn touch_motion(&mut self, slot: u32, x: f64, y: f64, now: f64) -> Vec<ImeEvent> {
        let actions = self.keyboard.touch_motion(slot, x, y, now);
        self.apply_all(actions, now)
    }

    /// 手指抬起。
    pub fn touch_up(&mut self, slot: u32, now: f64) -> Vec<ImeEvent> {
        let actions = self.keyboard.touch_up(slot, now);
        self.apply_all(actions, now)
    }

    /// 取消所有触摸。
    pub fn touch_cancel(&mut self, now: f64) {
        self.keyboard.touch_cancel(now);
    }

    /// 定时驱动（长按、退格连删），见 [`Keyboard::next_deadline`]。
    pub fn tick(&mut self, now: f64) -> Vec<ImeEvent> {
        let actions = self.keyboard.tick(now);
        self.apply_all(actions, now)
    }

    /// 点按候选栏中本页第 `index` 个候选。
    pub fn select_candidate(&mut self, index: usize) -> Vec<ImeEvent> {
        self.run(|im| {
            if let Some(e) = &mut im.engine {
                e.select_candidate(index);
            }
            None
        })
    }

    /// 候选翻页（`forward` 为 `true` 时向后）。
    pub fn change_page(&mut self, forward: bool) -> Vec<ImeEvent> {
        self.run(|im| {
            if let Some(e) = &mut im.engine {
                if forward {
                    e.next_page();
                } else {
                    e.prev_page();
                }
            }
            None
        })
    }

    /// 执行一组键盘动作。
    pub fn apply_all(&mut self, actions: Vec<KeyAction>, now: f64) -> Vec<ImeEvent> {
        let mut out = Vec::new();
        for action in actions {
            out.extend(self.apply(action, now));
        }
        out
    }

    /// 执行一个键盘动作。
    pub fn apply(&mut self, action: KeyAction, now: f64) -> Vec<ImeEvent> {
        self.run(|im| match action {
            KeyAction::Char(c) => im.key(ImeKey::Char(c), Some(c.to_string())),
            KeyAction::Text(s) => {
                match &mut im.engine {
                    Some(e) => e.insert_text(&s),
                    None => return Some(ImeEvent::Commit(s)),
                }
                None
            }
            KeyAction::Space => im.key(ImeKey::Space, Some(" ".into())),
            KeyAction::Backspace => im.key(ImeKey::Backspace, None),
            KeyAction::Return => im.key(ImeKey::Enter, None),
            KeyAction::ToggleLanguage => {
                let chinese = !im.keyboard.chinese();
                if chinese && im.purpose.is_sensitive() {
                    return None;
                }
                if let Some(e) = &mut im.engine {
                    e.set_ascii_mode(!chinese);
                    im.keyboard.set_chinese(chinese);
                    if im.purpose.allows_chinese() {
                        im.prefer_chinese = chinese;
                    }
                }
                None
            }
            KeyAction::Hide => {
                // 收起前把组字内容上屏，避免打了一半的字丢失。
                if let Some(e) = &mut im.engine {
                    e.commit_composition();
                }
                im.keyboard.hide(now);
                Some(ImeEvent::Hide)
            }
        })
    }

    /// 把按键交给引擎；没有引擎时 `fallback` 作为文字上屏，否则转发。
    fn key(&mut self, key: ImeKey, fallback: Option<String>) -> Option<ImeEvent> {
        match (&mut self.engine, fallback) {
            (Some(e), _) => match e.process_key(key) {
                KeyOutcome::Handled => None,
                KeyOutcome::Forward(k) => Some(ImeEvent::Forward(k)),
            },
            (None, Some(text)) => Some(ImeEvent::Commit(text)),
            (None, None) => Some(ImeEvent::Forward(key)),
        }
    }

    /// 执行 `f`，然后按顺序收集事件：上屏文字 → `f` 产生的事件 → 组字变化。
    fn run(&mut self, f: impl FnOnce(&mut Self) -> Option<ImeEvent>) -> Vec<ImeEvent> {
        let before = self.engine.as_ref().map(ImeEngine::revision);
        let event = f(self);
        let mut out = Vec::new();
        if let Some(text) = self.engine.as_mut().and_then(ImeEngine::take_commit) {
            out.push(ImeEvent::Commit(text));
        }
        out.extend(event);
        if self.engine.as_ref().map(ImeEngine::revision) != before {
            out.push(ImeEvent::CompositionChanged);
        }
        out
    }

    fn sync_mode(&mut self) {
        let chinese = self.engine.is_some() && self.prefer_chinese && self.purpose.allows_chinese();
        if let Some(e) = &mut self.engine {
            e.set_ascii_mode(!chinese);
        }
        self.keyboard.set_chinese(chinese);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::LayoutParams;

    /// 没有引擎（librime 未就绪）时键盘照常输入英文。
    #[test]
    fn works_without_engine() {
        let kb = Keyboard::new(LayoutParams::for_screen(1280.0, 800.0), true, 0.0);
        let mut im = InputMethod::new(kb, None);
        assert!(!im.is_chinese(), "没有引擎时不能是中文模式");
        let ev = im.activate(ContentPurpose::Normal, 0.0);
        assert!(ev.is_empty());
        assert_eq!(
            im.apply(KeyAction::Char('h'), 0.0),
            vec![ImeEvent::Commit("h".into())]
        );
        assert_eq!(
            im.apply(KeyAction::Backspace, 0.0),
            vec![ImeEvent::Forward(ImeKey::Backspace)]
        );
        assert_eq!(im.apply(KeyAction::Hide, 0.0), vec![ImeEvent::Hide]);
        assert!(!im.keyboard().is_shown());
    }

    #[test]
    fn purpose_picks_layer() {
        let kb = Keyboard::new(LayoutParams::for_screen(1280.0, 800.0), false, 0.0);
        let mut im = InputMethod::new(kb, None);
        im.activate(ContentPurpose::from_text_input_v3(4), 0.0);
        assert_eq!(im.keyboard().layer(), Layer::Numbers);
        assert!(im.keyboard().is_shown());
        im.activate(ContentPurpose::Url, 1.0);
        assert_eq!(im.keyboard().layer(), Layer::Letters);
        assert!(!ContentPurpose::Password.allows_chinese());
    }
}
