//! 输入法引擎：在 Rime 会话之上提供面向屏幕键盘的接口。
//!
//! - 字母、空格、退格、回车、数字选词、翻页 → [`ImeEngine::process_key`]；
//! - 标点、数字、符号等“直接文字” → [`ImeEngine::insert_text`]（组字中会先把
//!   组字内容上屏，再输入这段文字，与 iPad 拼音键盘一致）；
//! - 点按候选栏 → [`ImeEngine::select_candidate`]；
//! - 中/英切换 → [`ImeEngine::set_ascii_mode`]。
//!
//! 每次操作后，[`ImeEngine::state`] 是最新的组字状态（预编辑、本页候选、高亮），
//! 待上屏的文字用 [`ImeEngine::take_commit`] 取出。

use crate::config::ImeConfig;
use crate::error::ImeError;
use crate::rime::{Candidate, Preedit, Rime, Session, keysym};

/// 输入法能处理的按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImeKey {
    /// 可打印字符（字母、数字、ASCII 标点）。
    Char(char),
    /// 空格。
    Space,
    /// 退格。
    Backspace,
    /// 回车。
    Enter,
    /// Esc。
    Escape,
    /// 向前翻页。
    PageUp,
    /// 向后翻页。
    PageDown,
    /// 方向键。
    Left,
    /// 方向键。
    Right,
    /// 方向键。
    Up,
    /// 方向键。
    Down,
    /// Tab。
    Tab,
    /// 向后删除。
    Delete,
}

impl ImeKey {
    /// 对应的 X11 keysym（librime 使用 keysym）。
    pub fn keysym(self) -> u32 {
        match self {
            ImeKey::Char(c) if c.is_ascii() => c as u32,
            // 非 ASCII 字符：X11 的 Unicode keysym。
            ImeKey::Char(c) => 0x0100_0000 | c as u32,
            ImeKey::Space => keysym::SPACE,
            ImeKey::Backspace => keysym::BACKSPACE,
            ImeKey::Enter => keysym::RETURN,
            ImeKey::Escape => keysym::ESCAPE,
            ImeKey::PageUp => keysym::PAGE_UP,
            ImeKey::PageDown => keysym::PAGE_DOWN,
            ImeKey::Left => keysym::LEFT,
            ImeKey::Right => keysym::RIGHT,
            ImeKey::Up => keysym::UP,
            ImeKey::Down => keysym::DOWN,
            ImeKey::Tab => keysym::TAB,
            ImeKey::Delete => keysym::DELETE,
        }
    }
}

/// 按键处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    /// 输入法处理了（可能更新了组字状态，或产生了待上屏文字）。
    Handled,
    /// 输入法没处理：宿主应把它当作普通按键发给应用（例如没有组字时的退格、回车）。
    Forward(ImeKey),
}

/// 组字状态快照（每次操作后刷新）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImeState {
    /// 预编辑文字（例如 `ni hao`）；不在组字时为 `None`。
    pub preedit: Option<Preedit>,
    /// 原始输入（例如 `nihao`）。
    pub raw_input: String,
    /// 本页候选。
    pub candidates: Vec<Candidate>,
    /// 页码（从 0 开始）。
    pub page_no: usize,
    /// 每页候选数。
    pub page_size: usize,
    /// 是否最后一页。
    pub is_last_page: bool,
    /// 高亮候选在本页中的下标（按空格会选它）。
    pub highlighted: usize,
    /// 现在确认会上屏的文字（例如 `你好`）。
    pub commit_preview: Option<String>,
    /// 西文模式（英文）。
    pub ascii_mode: bool,
}

impl ImeState {
    /// 是否正在组字。
    pub fn is_composing(&self) -> bool {
        !self.raw_input.is_empty() || self.preedit.is_some()
    }
}

/// 中文拼音输入引擎（一个 Rime 会话）。
pub struct ImeEngine {
    session: Session,
    state: ImeState,
    commit: String,
    revision: u64,
}

impl std::fmt::Debug for ImeEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImeEngine")
            .field("session", &self.session)
            .field("state", &self.state)
            .finish()
    }
}

impl ImeEngine {
    /// 启动 librime（每个进程一次，必要时部署）并创建一个输入会话。
    ///
    /// 首次运行需要部署，会阻塞约 1 秒；合成器应在后台线程调用。
    pub fn new(config: &ImeConfig) -> Result<ImeEngine, ImeError> {
        let rime = Rime::start(config)?;
        ImeEngine::with_rime(rime)
    }

    /// 在已启动的 librime 上创建一个输入会话。
    pub fn with_rime(rime: &'static Rime) -> Result<ImeEngine, ImeError> {
        let session = rime.create_session()?;
        let mut engine = ImeEngine {
            session,
            state: ImeState::default(),
            commit: String::new(),
            revision: 0,
        };
        engine.refresh();
        Ok(engine)
    }

    /// 底层 Rime 会话（高级用法）。
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// 当前组字状态。
    pub fn state(&self) -> &ImeState {
        &self.state
    }

    /// 是否正在组字。
    pub fn is_composing(&self) -> bool {
        self.state.is_composing()
    }

    /// 组字状态的版本号：[`ImeEngine::state`] 每变化一次加一（用于判断是否要重画候选栏）。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// 取出待上屏的文字（取出后清空）。
    pub fn take_commit(&mut self) -> Option<String> {
        if self.commit.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.commit))
        }
    }

    /// 处理一个按键。
    ///
    /// librime 没处理的可打印字符和空格直接作为文字上屏；其余没处理的键返回
    /// [`KeyOutcome::Forward`]，由宿主按普通按键发给应用。
    pub fn process_key(&mut self, key: ImeKey) -> KeyOutcome {
        if let ImeKey::Char(c) = key
            && !c.is_ascii_graphic()
        {
            // 非 ASCII 字符（全角标点、长按得到的变体等）按文字处理。
            let mut buf = [0u8; 4];
            self.insert_text(c.encode_utf8(&mut buf));
            return KeyOutcome::Handled;
        }
        let handled = self.session.process_key(key.keysym(), 0);
        self.collect();
        if handled {
            return KeyOutcome::Handled;
        }
        match key {
            ImeKey::Char(c) => {
                self.commit.push(c);
                KeyOutcome::Handled
            }
            ImeKey::Space => {
                self.commit.push(' ');
                KeyOutcome::Handled
            }
            other => KeyOutcome::Forward(other),
        }
    }

    /// 以原始 keysym 处理按键（实体键盘）。返回 `false` 时宿主应把原按键转发给应用。
    pub fn process_keysym(&mut self, keysym: u32, modifiers: u32) -> bool {
        let handled = self.session.process_key(keysym, modifiers);
        self.collect();
        handled
    }

    /// 依次输入一串字符（测试与调试用），等价于逐个 [`ImeEngine::process_key`]。
    pub fn type_str(&mut self, text: &str) {
        for c in text.chars() {
            let key = if c == ' ' {
                ImeKey::Space
            } else {
                ImeKey::Char(c)
            };
            self.process_key(key);
        }
    }

    /// 直接输入一段文字（标点、数字、符号）。
    ///
    /// 组字中时先把组字内容按当前候选转换上屏（例如 `nihao` + `，` → `你好，`）。
    /// 例外：中文组字中输入 `'` 是拼音分隔符（`xi'an` → 西安），交给 librime。
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.is_composing() {
            if text == "'" && !self.state.ascii_mode {
                self.session.process_key('\'' as u32, 0);
                self.collect();
                return;
            }
            self.session.commit_composition();
            self.collect();
        }
        self.commit.push_str(text);
    }

    /// 选择本页第 `index` 个候选（点按候选栏）。
    pub fn select_candidate(&mut self, index: usize) -> bool {
        let ok = self.session.select_candidate_on_current_page(index);
        self.collect();
        ok
    }

    /// 选择完整候选列表中的第 `index` 个候选（展开的候选面板）。
    pub fn select_candidate_global(&mut self, index: usize) -> bool {
        let ok = self.session.select_candidate(index);
        self.collect();
        ok
    }

    /// 完整候选列表中从 `start` 开始的最多 `max` 个候选（不受分页限制）。
    pub fn candidates(&self, start: usize, max: usize) -> Vec<Candidate> {
        if !self.is_composing() {
            return Vec::new();
        }
        self.session.candidates(start, max)
    }

    /// 向后翻页。返回是否翻页成功。
    pub fn next_page(&mut self) -> bool {
        let ok = self.session.change_page(false);
        self.collect();
        ok
    }

    /// 向前翻页。返回是否翻页成功。
    pub fn prev_page(&mut self) -> bool {
        let ok = self.session.change_page(true);
        self.collect();
        ok
    }

    /// 西文模式（英文）是否开启。
    pub fn ascii_mode(&self) -> bool {
        self.state.ascii_mode
    }

    /// 切换中/英。组字中切到英文时，把已输入的字母原样上屏（与常见拼音输入法一致）。
    pub fn set_ascii_mode(&mut self, ascii: bool) {
        if ascii == self.state.ascii_mode {
            return;
        }
        if ascii && self.is_composing() {
            let raw = self.session.input();
            self.session.clear_composition();
            self.commit.push_str(&raw);
        }
        self.session.set_option("ascii_mode", ascii);
        self.collect();
    }

    /// 切换中/英，返回切换后是否为西文模式。
    pub fn toggle_ascii_mode(&mut self) -> bool {
        let ascii = !self.state.ascii_mode;
        self.set_ascii_mode(ascii);
        self.state.ascii_mode
    }

    /// 把组字内容按当前候选转换后上屏（例如输入框失去焦点前）。
    pub fn commit_composition(&mut self) {
        if self.is_composing() {
            self.session.commit_composition();
            self.collect();
        }
    }

    /// 丢弃组字内容（不上屏）。
    pub fn clear(&mut self) {
        self.session.clear_composition();
        self.collect();
    }

    /// 取出 librime 产生的上屏文字，并刷新状态快照。
    fn collect(&mut self) {
        if let Some(text) = self.session.take_commit() {
            self.commit.push_str(&text);
        }
        self.refresh();
    }

    fn refresh(&mut self) {
        let ctx = self.session.context();
        let ascii_mode = self.session.option("ascii_mode");
        let raw_input = self.session.input();
        let state = ImeState {
            preedit: ctx.preedit,
            raw_input,
            candidates: ctx.menu.candidates,
            page_no: ctx.menu.page_no,
            page_size: ctx.menu.page_size,
            is_last_page: ctx.menu.is_last_page,
            highlighted: ctx.menu.highlighted,
            commit_preview: ctx.commit_preview,
            ascii_mode,
        };
        if state != self.state {
            self.state = state;
            self.revision += 1;
        }
    }
}
