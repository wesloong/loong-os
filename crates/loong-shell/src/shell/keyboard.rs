//! 屏幕键盘与拼音输入（M4）在系统界面中的状态。只描述“状态与画什么”，绘制见 `render::keyboard`，
//! 与应用之间的协议（text-input-v3、合成按键、窗口尺寸）见 `crate::ime`。
//!
//! 输入法 [`InputMethod`]（loong-ime：键盘模型 + 拼音引擎）运行在合成器进程内，本模块把它接进系统界面：
//!
//! - **激活**：应用的输入框 enable / disable → [`ScreenKeyboard::activate`] / [`ScreenKeyboard::deactivate`]；
//! - **可见性**：输入框激活、前台应用停稳、没有系统手势时才显示（[`ScreenKeyboard::set_allowed`]）。
//!   用户按收起键后键盘收起、输入框仍处于激活状态：再点按一下应用（不是滑动），键盘重新弹出；
//! - **触摸**：键区交给 `InputMethod`；候选栏（点选、横向惯性滚动、越界翻页、翻页箭头）在这里处理；
//! - **输出**：`ImeEvent` 按顺序转成 [`TextOp`]（commit_string / preedit_string / 合成按键 / done）；
//! - **窗口避让**：键盘升起到位后，[`ScreenKeyboard::window_inset`] 给出前台窗口应让出的高度。
//!
//! 坐标：除特别说明外为屏幕逻辑点；`InputMethod` 与 [`KeyboardLayout`] 使用键盘坐标（原点在键盘左上角）。

use loong_ime::{
    ContentPurpose, Engine, ImeEvent, ImeKey, InputMethod,
    keyboard::{Hit, KeyId, KeyKind, Keyboard, KeyboardLayout, LayoutParams},
};
use loong_motion::{Decay, Spring, SpringAnimation, VelocityTracker};

use super::{
    DrawItem, Scene, TAP_SLOP, TexKey,
    geometry::RectF,
    gesture::{rubber_clamp, rubber_clamp_inverse},
    pin,
};

/// 键盘背板上缘向上投出的墨影高度（点）：背板纹理比键盘高这么多。
pub const PANEL_SHADOW: f64 = 14.0;
/// 放大小笺、备选条四周墨影的外扩（点）。
pub const POPUP_SHADOW: f64 = 12.0;
/// 键盘升起到这个进度时让应用窗口让出高度（之后的几帧里 WebKit 重新排版）。
const INSET_AT: f64 = 0.98;
/// 收起后点按应用重新弹出键盘的延迟（秒）：这次点按如果让输入框失去焦点，应用会先发
/// disable，键盘就不必弹出又收回。
const RESHOW_DELAY: f64 = 0.2;
/// 候选栏越界拖动超过这个距离（点）松手即翻页。
const PAGE_PULL: f64 = 44.0;
/// 候选左右留白（字号的倍数）。
const CELL_PAD_EM: f64 = 0.75;
/// 拼音（预编辑）字号相对候选字号。
const PREEDIT_EM: f64 = 0.66;
/// 拼音左右留白（点）。
const PREEDIT_PAD: f64 = 14.0;

/// 发给应用的一条输入消息（按顺序发送）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextOp {
    /// `zwp_text_input_v3.commit_string`。
    Commit(String),
    /// `zwp_text_input_v3.preedit_string`（文字、光标字节偏移）；`None` 清除预编辑。
    Preedit(Option<(String, usize)>),
    /// 合成的 `wl_keyboard` 按键（按下再抬起），例如没有组字时的退格、回车。
    Key(ImeKey),
    /// `zwp_text_input_v3.done`：让前面的 commit_string / preedit_string 生效。
    Done,
}

/// 候选栏中的一格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    /// 本页第 i 个候选。
    Candidate(usize),
    /// 上一页（“‹”）。
    PrevPage,
    /// 下一页（“›”）。
    NextPage,
}

/// 候选栏中的一格：在候选条（可横向滚动）中的位置与文字。
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateCell {
    pub kind: CellKind,
    /// 左边（候选条坐标：0 为候选条起点，未减去滚动位置）。
    pub x: f64,
    pub w: f64,
    pub text: String,
}

/// 文字宽度的估算（em）：霞鹜文楷的汉字与全角标点为 1em，拉丁字母、数字约 0.52em。
/// 候选格子留有足够的边距，绘制时文字在格子中居中，估算的误差看不出来；这样排版与命中测试
/// 不依赖字体，测试可复现。
pub fn text_width_em(text: &str) -> f64 {
    text.chars()
        .map(|c| match c as u32 {
            0..=0x7F => {
                if c == ' ' {
                    0.3
                } else {
                    0.52
                }
            }
            0x2E80.. => 1.0,
            _ => 0.7,
        })
        .sum()
}

/// 候选栏：拼音（固定在左侧）+ 可横向滚动的候选条。
#[derive(Debug)]
struct CandidateBar {
    preedit: Option<String>,
    /// 拼音所占的宽度（候选条从这里开始）。
    preedit_w: f64,
    cells: Vec<CandidateCell>,
    content_w: f64,
    highlighted: Option<usize>,
    scroll: SpringAnimation<f64>,
    tracker: VelocityTracker<f64>,
}

impl CandidateBar {
    fn new() -> Self {
        CandidateBar {
            preedit: None,
            preedit_w: 0.0,
            cells: Vec::new(),
            content_w: 0.0,
            highlighted: None,
            scroll: scroll_anim(0.0, 0.0, 0.0, 0.0),
            tracker: VelocityTracker::new(),
        }
    }
}

fn scroll_anim(from: f64, to: f64, velocity: f64, now: f64) -> SpringAnimation<f64> {
    SpringAnimation::new(Spring::SMOOTH, from, to, velocity, now).with_rest_thresholds(0.05, 0.5)
}

/// 候选栏上的触摸。
#[derive(Debug, Clone, Copy)]
struct BarTouch {
    slot: u32,
    start: (f64, f64),
    /// 按下时的滚动位置（已去掉橡皮筋）。
    base: f64,
    dragging: bool,
}

/// 应用上的触摸（判断是不是一次点按，用于收起后重新弹出键盘）。
#[derive(Debug, Clone, Copy)]
struct AppTouch {
    slot: u32,
    start: (f64, f64),
    moved: bool,
}

/// 系统界面中的屏幕键盘。
#[derive(Debug)]
pub struct ScreenKeyboard {
    im: InputMethod,
    w: f64,
    h: f64,
    /// 应用的输入框处于激活状态（text-input enable）。
    active: bool,
    purpose: ContentPurpose,
    /// 系统允许显示（前台应用停稳、没有系统手势、未锁屏）。
    allowed: bool,
    /// 用户按了收起键（输入框仍激活）。
    dismissed: bool,
    reshow_at: Option<f64>,
    /// 前台窗口应让出的高度。
    inset: f64,
    key_slots: Vec<u32>,
    bar_touch: Option<BarTouch>,
    app_touch: Option<AppTouch>,
    bar: CandidateBar,
    /// 引擎组字状态的版本（判断候选栏是否要重排）。
    engine_rev: Option<u64>,
    /// 候选栏内容的版本：每次重排加一，绘制端据此重画候选纹理。
    cand_version: u64,
    events: Vec<ImeEvent>,
}

impl ScreenKeyboard {
    /// 新建（隐藏状态，没有拼音引擎时只能输入英文）。
    pub fn new(w: f64, h: f64, now: f64) -> Self {
        let keyboard = Keyboard::new(LayoutParams::for_screen(w, h), true, now);
        ScreenKeyboard {
            im: InputMethod::new(keyboard, None),
            w,
            h,
            active: false,
            purpose: ContentPurpose::Normal,
            allowed: false,
            dismissed: false,
            reshow_at: None,
            inset: 0.0,
            key_slots: Vec::new(),
            bar_touch: None,
            app_touch: None,
            bar: CandidateBar::new(),
            engine_rev: None,
            cand_version: 0,
            events: Vec::new(),
        }
    }

    /// 装上拼音引擎（librime 在后台启动完成后）。
    pub fn set_engine(&mut self, engine: Box<dyn Engine>) {
        self.im.set_boxed_engine(Some(engine));
        self.refresh_bar(true);
    }

    /// 是否已有拼音引擎（中文可用）。
    #[allow(dead_code, reason = "测试与状态显示使用")]
    pub fn has_engine(&self) -> bool {
        self.im.engine().is_some()
    }

    /// 输入法（键盘模型、预编辑、候选）。
    pub fn input_method(&self) -> &InputMethod {
        &self.im
    }

    /// 当前布局。
    pub fn layout(&self) -> &KeyboardLayout {
        self.im.keyboard().layout()
    }

    /// 键盘总高度（候选栏 + 键区 + 底部安全区）。
    pub fn height(&self) -> f64 {
        self.layout().params.height
    }

    /// 输入框是否激活。
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// 输入框用途。
    #[allow(dead_code, reason = "测试使用")]
    pub fn purpose(&self) -> ContentPurpose {
        self.purpose
    }

    /// 是否处于（或正在进入）显示状态。
    pub fn is_shown(&self) -> bool {
        self.im.keyboard().is_shown()
    }

    /// 升起进度 0..1。
    pub fn presentation(&self, now: f64) -> f64 {
        self.im.keyboard().presentation(now).clamp(0.0, 1.0)
    }

    /// 键盘上边缘在屏幕上的 y。
    pub fn top(&self, now: f64) -> f64 {
        self.h - self.height() * self.presentation(now)
    }

    /// 前台应用窗口应让出的高度（升起到位后为键盘高度，收起开始时立即为 0）。
    pub fn window_inset(&self) -> f64 {
        self.inset
    }

    /// 候选栏内容的版本（每次重排加一）。
    pub fn candidate_version(&self) -> u64 {
        self.cand_version
    }

    /// 候选栏左侧的拼音（预编辑）及其所占宽度。
    pub fn preedit_cell(&self) -> Option<(&str, f64)> {
        self.bar.preedit.as_deref().map(|t| (t, self.bar.preedit_w))
    }

    /// 候选条的格子。
    pub fn candidate_cells(&self) -> &[CandidateCell] {
        &self.bar.cells
    }

    /// 高亮的候选（按空格会选它）。
    pub fn highlighted_candidate(&self) -> Option<usize> {
        self.bar.highlighted
    }

    /// 候选字号（点）。
    pub fn candidate_font_size(&self) -> f64 {
        self.layout().metrics.candidate_font_size
    }

    /// 拼音字号（点）。
    pub fn preedit_font_size(&self) -> f64 {
        (self.candidate_font_size() * PREEDIT_EM).round()
    }

    // ───────────────────────── 激活与可见性 ─────────────────────────

    /// 输入框获得焦点（text-input enable）。`purpose` 为 `None` 表示应用没有发送内容类型：
    /// 已激活时（WPE 会连发两次 enable）保持原来的用途，否则按普通文本处理。
    pub fn activate(&mut self, purpose: Option<ContentPurpose>, now: f64) {
        let purpose = purpose.unwrap_or(if self.active {
            self.purpose
        } else {
            ContentPurpose::Normal
        });
        // 同一个输入框再次 enable：不重置层与组字，只在用户收起过键盘时重新弹出。
        if !(self.active && purpose == self.purpose) {
            let events = self.im.activate(purpose, now);
            self.handle(events);
        }
        self.active = true;
        self.purpose = purpose;
        self.dismissed = false;
        self.reshow_at = None;
        self.sync_visibility(now);
    }

    /// 输入框失去焦点（text-input disable、焦点离开、应用退出）：丢弃组字，收起键盘。
    pub fn deactivate(&mut self, now: f64) {
        self.reshow_at = None;
        self.dismissed = false;
        if !self.active {
            return;
        }
        self.active = false;
        let events = self.im.deactivate(now);
        self.handle(events);
        self.end_touches(now);
        self.inset = 0.0;
    }

    /// 系统是否允许显示键盘（前台应用停稳、没有系统手势、未锁屏）。
    pub fn set_allowed(&mut self, allowed: bool, now: f64) {
        if allowed != self.allowed {
            self.allowed = allowed;
            self.sync_visibility(now);
        }
    }

    fn sync_visibility(&mut self, now: f64) {
        let want = self.active && self.allowed && !self.dismissed;
        if want == self.is_shown() {
            return;
        }
        if want {
            self.im.keyboard_mut().show(now);
        } else {
            self.im.keyboard_mut().hide(now);
            self.end_touches(now);
            // 收起一开始就把高度还给应用（先恢复窗口，再播放收起动画）。
            self.inset = 0.0;
        }
    }

    fn end_touches(&mut self, now: f64) {
        self.im.touch_cancel(now);
        self.key_slots.clear();
        self.bar_touch = None;
    }

    /// 每帧调用：长按与退格连删的定时、收起后重新弹出、窗口避让。
    pub fn update(&mut self, now: f64) {
        if self.im.keyboard().next_deadline().is_some_and(|d| d <= now) {
            let events = self.im.tick(now);
            self.handle(events);
        }
        if let Some(t) = self.reshow_at
            && now >= t
        {
            self.reshow_at = None;
            if self.active && self.dismissed {
                self.dismissed = false;
                self.sync_visibility(now);
            }
        }
        if self.is_shown() {
            if self.presentation(now) >= INSET_AT {
                self.inset = self.height();
            }
        } else {
            self.inset = 0.0;
        }
    }

    /// 没有动画、触摸、待发送的消息时为真（画面静止时不重绘）。
    pub fn is_idle(&self, now: f64) -> bool {
        !self.im.keyboard().is_animating(now)
            && self.key_slots.is_empty()
            && self.bar_touch.is_none()
            && self.bar.scroll.is_done(now)
            && self.events.is_empty()
            && self.reshow_at.is_none()
    }

    // ───────────────────────── 触摸 ─────────────────────────

    /// 点 `(x, y)` 是否落在（显示中的）键盘上。
    pub fn contains(&self, x: f64, y: f64, now: f64) -> bool {
        self.is_shown() && (0.0..self.w).contains(&x) && y >= self.top(now) && y < self.h
    }

    /// 这根手指是否在键盘上。
    pub fn owns(&self, slot: u32) -> bool {
        self.key_slots.contains(&slot) || self.bar_touch.is_some_and(|t| t.slot == slot)
    }

    /// 手指按在键盘上（调用前用 [`ScreenKeyboard::contains`] 判断）。返回点墨的半径。
    pub fn touch_down(&mut self, slot: u32, x: f64, y: f64, now: f64) -> f64 {
        let top = self.top(now);
        let (kx, ky) = (x, y - top);
        let m = self.layout().metrics;
        match self.layout().hit_test(kx, ky) {
            Some(Hit::CandidateBar) => {
                if self.bar_touch.is_some() {
                    // 候选栏同时只跟一根手指。
                    return 0.0;
                }
                let shown = self.bar.scroll.value(now);
                pin(&mut self.bar.scroll, shown, now);
                self.bar.tracker.reset();
                self.bar.tracker.add(now, x);
                let base = self.unrubber(shown);
                self.bar_touch = Some(BarTouch {
                    slot,
                    start: (x, y),
                    base,
                    dragging: false,
                });
                m.candidate_bar_height * 0.42
            }
            _ => {
                let events = self.im.touch_down(slot, kx, ky, now);
                self.key_slots.push(slot);
                self.handle(events);
                m.key_height * 0.42
            }
        }
    }

    /// 手指移动。返回是否刚开始拖动（宿主据此让墨滴淡去）。
    pub fn touch_motion(&mut self, slot: u32, x: f64, y: f64, now: f64) -> bool {
        if self.key_slots.contains(&slot) {
            let top = self.top(now);
            let events = self.im.touch_motion(slot, x, y - top, now);
            self.handle(events);
            return false;
        }
        let Some(mut t) = self.bar_touch.filter(|t| t.slot == slot) else {
            return false;
        };
        self.bar.tracker.add(now, x);
        let started = !t.dragging && (x - t.start.0).abs() > TAP_SLOP;
        if started {
            t.dragging = true;
            // 从越过起手阈值的位置开始跟手，不跳。
            t.start.0 = x;
        }
        if t.dragging {
            let shown = self.rubber(t.base - (x - t.start.0));
            pin(&mut self.bar.scroll, shown, now);
        }
        self.bar_touch = Some(t);
        started
    }

    /// 手指抬起。
    pub fn touch_up(&mut self, slot: u32, now: f64) {
        if let Some(i) = self.key_slots.iter().position(|&s| s == slot) {
            self.key_slots.remove(i);
            let events = self.im.touch_up(slot, now);
            self.handle(events);
            return;
        }
        let Some(t) = self.bar_touch.filter(|t| t.slot == slot) else {
            return;
        };
        self.bar_touch = None;
        if !t.dragging {
            let strip_x = t.start.0 - self.bar.preedit_w + self.bar.scroll.value(now);
            if let Some(kind) = self.cell_at(strip_x) {
                self.tap_cell(kind, now);
            }
            return;
        }
        self.release_scroll(now);
    }

    /// 应用窗口上的手指按下 / 移动 / 抬起：收起键盘后点按应用，键盘重新弹出。
    pub fn app_touch_down(&mut self, slot: u32, x: f64, y: f64) {
        self.app_touch = Some(AppTouch {
            slot,
            start: (x, y),
            moved: false,
        });
    }

    pub fn app_touch_motion(&mut self, slot: u32, x: f64, y: f64) {
        if let Some(t) = &mut self.app_touch
            && t.slot == slot
            && (x - t.start.0).hypot(y - t.start.1) > TAP_SLOP
        {
            t.moved = true;
        }
    }

    pub fn app_touch_up(&mut self, slot: u32, now: f64) {
        if let Some(t) = self.app_touch.take()
            && t.slot == slot
            && !t.moved
            && self.active
            && self.dismissed
        {
            self.reshow_at = Some(now + RESHOW_DELAY);
        }
    }

    /// 候选条最大滚动位置。
    fn max_scroll(&self) -> f64 {
        (self.bar.content_w - (self.w - self.bar.preedit_w)).max(0.0)
    }

    fn rubber(&self, raw: f64) -> f64 {
        let d = (self.w - self.bar.preedit_w).max(1.0);
        rubber_clamp(raw, 0.0, self.max_scroll(), d)
    }

    fn unrubber(&self, shown: f64) -> f64 {
        let d = (self.w - self.bar.preedit_w).max(1.0);
        rubber_clamp_inverse(shown, 0.0, self.max_scroll(), d)
    }

    /// 候选条拖动松手：越界够远就翻页，否则惯性滚动（`Decay::NORMAL` 投影）后停在范围内。
    fn release_scroll(&mut self, now: f64) {
        let shown = self.bar.scroll.value(now);
        let max = self.max_scroll();
        let has = |k: CellKind| self.bar.cells.iter().any(|c| c.kind == k);
        if shown > max + PAGE_PULL && has(CellKind::NextPage) {
            self.change_page(true);
            return;
        }
        if shown < -PAGE_PULL && has(CellKind::PrevPage) {
            self.change_page(false);
            return;
        }
        let v = -self.bar.tracker.velocity_at(now);
        let target = (shown + Decay::NORMAL.project(v)).clamp(0.0, max);
        self.bar.scroll = scroll_anim(shown, target, v, now);
    }

    /// 候选条坐标 `x` 处的格子。
    fn cell_at(&self, x: f64) -> Option<CellKind> {
        self.bar
            .cells
            .iter()
            .find(|c| x >= c.x && x < c.x + c.w)
            .map(|c| c.kind)
    }

    /// 点按候选栏中的一格。
    pub fn tap_cell(&mut self, kind: CellKind, _now: f64) {
        match kind {
            CellKind::Candidate(i) => {
                let events = self.im.select_candidate(i);
                self.handle(events);
            }
            CellKind::NextPage => self.change_page(true),
            CellKind::PrevPage => self.change_page(false),
        }
    }

    fn change_page(&mut self, forward: bool) {
        let events = self.im.change_page(forward);
        self.handle(events);
    }

    // ───────────────────────── 输出 ─────────────────────────

    fn handle(&mut self, events: Vec<ImeEvent>) {
        for e in events {
            if e == ImeEvent::Hide {
                // 收起键：输入框仍激活，键盘模型已开始收起。
                self.dismissed = true;
                self.key_slots.clear();
                self.bar_touch = None;
                self.inset = 0.0;
            }
            self.events.push(e);
        }
        self.refresh_bar(false);
    }

    /// 取出要发给应用的消息：`commit_string` / `preedit_string` 攒在一起，以一个 `done` 结束；
    /// 合成按键之前先发 `done`，保证应用先插入文字、再处理按键（例如先上屏 “a” 再退格）。
    pub fn take_ops(&mut self) -> Vec<TextOp> {
        let mut ops = Vec::new();
        let mut pending = false;
        let mut preedit_dirty = false;
        let preedit = || {
            self.im
                .preedit()
                .filter(|p| !p.text.is_empty())
                .map(|p| (p.text.clone(), p.cursor))
        };
        let flush = |ops: &mut Vec<TextOp>, pending: &mut bool, preedit_dirty: &mut bool| {
            if *preedit_dirty {
                ops.push(TextOp::Preedit(preedit()));
            }
            if *pending {
                ops.push(TextOp::Done);
            }
            *pending = false;
            *preedit_dirty = false;
        };
        for e in std::mem::take(&mut self.events) {
            match e {
                ImeEvent::Commit(text) => {
                    ops.push(TextOp::Commit(text));
                    pending = true;
                }
                ImeEvent::CompositionChanged => {
                    preedit_dirty = true;
                    pending = true;
                }
                ImeEvent::Forward(key) => {
                    flush(&mut ops, &mut pending, &mut preedit_dirty);
                    ops.push(TextOp::Key(key));
                }
                ImeEvent::Hide => {}
            }
        }
        flush(&mut ops, &mut pending, &mut preedit_dirty);
        ops
    }

    /// 组字状态变了：重排候选栏。
    fn refresh_bar(&mut self, force: bool) {
        let rev = self.im.engine().map(|e| e.revision());
        if rev == self.engine_rev && !force {
            return;
        }
        self.engine_rev = rev;
        let font = self.candidate_font_size();
        let pad = font * CELL_PAD_EM;
        let bar_h = self.layout().metrics.candidate_bar_height;
        let state = self.im.engine().map(|e| e.state().clone());
        let bar = &mut self.bar;
        bar.preedit = self
            .im
            .preedit()
            .map(|p| p.text.clone())
            .filter(|t| !t.is_empty());
        bar.preedit_w = bar.preedit.as_deref().map_or(0.0, |t| {
            (text_width_em(t) * font * PREEDIT_EM + 2.0 * PREEDIT_PAD).ceil()
        });
        bar.cells.clear();
        bar.highlighted = None;
        let mut x = 0.0;
        let mut push = |cells: &mut Vec<CandidateCell>, kind, text: String, em: f64| {
            let w = (em * font + 2.0 * pad).max(bar_h).ceil();
            cells.push(CandidateCell { kind, x, w, text });
            x += w;
        };
        if let Some(state) = state.filter(|s| !s.candidates.is_empty()) {
            if state.page_no > 0 {
                push(&mut bar.cells, CellKind::PrevPage, "‹".into(), 0.6);
            }
            for (i, c) in state.candidates.iter().enumerate() {
                push(
                    &mut bar.cells,
                    CellKind::Candidate(i),
                    c.text.clone(),
                    text_width_em(&c.text),
                );
            }
            if !state.is_last_page {
                push(&mut bar.cells, CellKind::NextPage, "›".into(), 0.6);
            }
            bar.highlighted = Some(state.highlighted.min(state.candidates.len() - 1));
        }
        bar.content_w = x;
        bar.scroll = scroll_anim(0.0, 0.0, 0.0, 0.0);
        self.cand_version += 1;
    }

    // ───────────────────────── 画面 ─────────────────────────

    /// 键 `id` 在屏幕上的矩形。
    pub fn key_rect(&self, id: KeyId, now: f64) -> RectF {
        let r = self.layout().key(id).rect;
        RectF::new(r.x, r.y + self.top(now), r.w, r.h)
    }

    /// 正按着的字符键（显示放大小笺）。长按备选条打开时没有。
    pub fn preview_key(&self) -> Option<KeyId> {
        let kb = self.im.keyboard();
        if kb.popup().is_some() {
            return None;
        }
        let layout = kb.layout();
        (0..layout.keys.len()).rev().find(|&id| {
            kb.is_pressed(id)
                && matches!(layout.keys[id].kind, KeyKind::Letter(_) | KeyKind::Char(_))
        })
    }

    /// 放大小笺的键与它在屏幕上的矩形（不含墨影）。
    pub fn preview(&self, now: f64) -> Option<(KeyId, RectF)> {
        let id = self.preview_key()?;
        Some((id, self.preview_rect(id, now)))
    }

    /// 键 `id` 的放大小笺：比键宽 30%、高 10%，浮在键的正上方。
    pub fn preview_rect(&self, id: KeyId, now: f64) -> RectF {
        let k = self.key_rect(id, now);
        let m = self.layout().metrics;
        let (w, h) = ((k.w * 1.3).max(k.h).round(), (k.h * 1.1).round());
        let x = (k.x + k.w / 2.0 - w / 2.0).clamp(4.0, (self.w - w - 4.0).max(4.0));
        let y = k.y - h - (m.row_gap * 0.5).round();
        RectF::new(x, y, w, h)
    }

    /// 放大小笺的尺寸（点，不含墨影）：所有字符键大小相同，绘制端据此生成一张小笺底。
    pub fn preview_size(&self) -> (f64, f64) {
        let id = self
            .layout()
            .keys
            .iter()
            .position(|k| matches!(k.kind, KeyKind::Letter(_)))
            .unwrap_or(0);
        let r = self.preview_rect(id, 0.0);
        (r.w, r.h)
    }

    /// 生成键盘的画面（键盘在应用卡片之上，Home 指示条、点墨之下）。
    pub fn push_scene(&self, scene: &mut Scene, ink: [f32; 3], now: f64) {
        let p = self.presentation(now);
        if p <= 0.001 {
            return;
        }
        let top = self.top(now);
        let height = self.height();
        let layout = self.layout();
        let m = layout.metrics;
        let texture = |key, rect, clip| DrawItem::Texture {
            key,
            rect,
            alpha: 1.0,
            radius: 0.0,
            clip,
            reveal: None,
        };
        scene.push(texture(
            TexKey::KeyboardPanel,
            RectF::new(0.0, top - PANEL_SHADOW, self.w, height + PANEL_SHADOW),
            None,
        ));
        // 按下：键帽着墨（底色变深，暗色外观变亮），由弹簧淡去。
        let kb = self.im.keyboard();
        for id in 0..layout.keys.len() {
            let hl = kb.highlight(id, now);
            if hl > 0.003 {
                scene.push(DrawItem::Solid {
                    rect: self.key_rect(id, now),
                    color: [ink[0], ink[1], ink[2], (0.13 * hl) as f32],
                    radius: m.corner_radius,
                });
            }
        }
        scene.push(texture(
            TexKey::KeyboardGlyphs,
            RectF::new(0.0, top, self.w, height),
            None,
        ));

        // 候选栏：拼音固定在左侧，候选条在其右侧横向滚动。
        let bar_h = m.candidate_bar_height;
        if let Some((_, w)) = self.preedit_cell() {
            scene.push(texture(
                TexKey::CandidatePreedit,
                RectF::new(0.0, top, w, bar_h),
                None,
            ));
        }
        let left = self.bar.preedit_w;
        let strip = RectF::new(left, top, self.w - left, bar_h);
        let scroll = self.bar.scroll.value(now);
        for (i, cell) in self.bar.cells.iter().enumerate() {
            let x = left + cell.x - scroll;
            if x + cell.w < left || x > self.w {
                continue;
            }
            if Some(cell.kind) == self.bar.highlighted.map(CellKind::Candidate) {
                // 高亮候选：下方一笔浓墨（笔触代替色块）。
                let pad = cell.w.min(self.candidate_font_size() * 2.0) * 0.18;
                scene.push(texture(
                    TexKey::CandidateMark,
                    RectF::new(x + pad, top + bar_h - 9.0, cell.w - 2.0 * pad, 6.0),
                    Some(strip),
                ));
            }
            scene.push(texture(
                TexKey::CandidateCell(i),
                RectF::new(x, top, cell.w, bar_h),
                Some(strip),
            ));
        }

        // 放大小笺（按下的字符键）与长按备选条：在键盘之上。
        if let Some((_, r)) = self.preview(now) {
            let s = POPUP_SHADOW;
            scene.push(texture(
                TexKey::KeyPreview,
                RectF::new(r.x - s, r.y - s, r.w + 2.0 * s, r.h + 2.0 * s),
                None,
            ));
        }
        if let Some(popup) = kb.popup() {
            let f = popup.frame;
            let s = POPUP_SHADOW;
            scene.push(texture(
                TexKey::KeyAlternates,
                RectF::new(f.x - s, f.y + top - s, f.w + 2.0 * s, f.h + 2.0 * s),
                None,
            ));
        }
    }

    // ───────────────────────── 脚本与测试 ─────────────────────────

    /// 打出字符 `ch` 要点按的键在屏幕上的中心（当前层上找不到时为 `None`）。
    /// `' '` 为空格，`'\n'` 为换行，`'\u{8}'` 为退格。
    pub fn key_center(&self, ch: char, now: f64) -> Option<(f64, f64)> {
        let layout = self.layout();
        let text = ch.to_string();
        let id = layout.keys.iter().position(|k| match k.kind {
            KeyKind::Letter(c) => c == ch.to_ascii_lowercase(),
            KeyKind::Char(s) => s == text,
            KeyKind::Space => ch == ' ',
            KeyKind::Return => ch == '\n',
            KeyKind::Backspace => ch == '\u{8}',
            _ => false,
        })?;
        Some(self.key_rect(id, now).center())
    }

    /// 某种功能键在屏幕上的中心。
    pub fn kind_center(&self, kind: KeyKind, now: f64) -> Option<(f64, f64)> {
        let id = self.layout().find(kind)?;
        Some(self.key_rect(id, now).center())
    }

    /// 候选栏第 `index` 个候选在屏幕上的中心（不在可见范围内时为 `None`）。
    pub fn candidate_center(&self, index: usize, now: f64) -> Option<(f64, f64)> {
        let cell = self
            .bar
            .cells
            .iter()
            .find(|c| c.kind == CellKind::Candidate(index))?;
        let x = self.bar.preedit_w + cell.x + cell.w / 2.0 - self.bar.scroll.value(now);
        let bar_h = self.layout().metrics.candidate_bar_height;
        (x >= self.bar.preedit_w && x < self.w).then(|| (x, self.top(now) + bar_h / 2.0))
    }
}

#[cfg(test)]
mod tests;
