//! 屏幕键盘的状态机：触摸 → 按键动作（[`KeyAction`]）。只管状态，不画任何东西。
//!
//! 行为参考 iPad：
//!
//! - **字符键在抬起时输出**，抬起时手指下的键为准（按住滑到相邻键可以纠正）；
//! - **快速连打**：新手指按下时，其他还按着的字符键立即输出（不等它们抬起），
//!   保证双手交替打字时顺序正确；
//! - **Shift** 在按下时生效：单击 → 下一个字母大写，双击（0.35 秒内）→ 大写锁定；
//!   按住 Shift 打的字母都大写，松开后恢复；
//! - **换层键**（123 / #+= / ABC）在按下时切换；如果同一根手指滑到某个字符上再抬起，
//!   输出该字符后自动回到原来的层；
//! - **退格**按下立即删除一次，按住 0.5 秒后每 0.1 秒连删；
//! - **长按**（0.5 秒且移动不超过 8pt）有备选的键弹出备选条，横向滑动选择，抬起输出。
//!
//! 时间统一用秒（`f64`，通常是帧时间戳），与 `loong-motion` 一致。宿主在
//! [`Keyboard::next_deadline`] 时刻（或每帧）调用 [`Keyboard::tick`] 驱动长按与连删。
//!
//! 按键反馈：按下时高亮**立即**变为 1（不做渐入，避免增加延迟）；抬起后用
//! `Spring::INTERACTIVE` 渐隐（约 0.25 秒停稳）。键盘升起/收起用 `Spring::SMOOTH`。

use loong_motion::{Spring, SpringAnimation};

use super::geometry::Rect;
use super::keymap::{KeyKind, Layer, alternates};
use super::layout::{Hit, KeyId, KeyboardLayout, LayoutParams, TOUCH_SLOP};

/// 长按触发时间（秒），设计规范 §9.2。
pub const LONG_PRESS_DELAY: f64 = 0.5;
/// 退格连删：开始前的等待（秒）。
pub const REPEAT_DELAY: f64 = 0.5;
/// 退格连删：间隔（秒）。
pub const REPEAT_INTERVAL: f64 = 0.1;
/// 双击 Shift 进入大写锁定的最大间隔（秒）。
pub const SHIFT_DOUBLE_TAP: f64 = 0.35;
/// 按键高亮渐隐的弹簧。
pub const KEY_RELEASE_SPRING: Spring = Spring::INTERACTIVE;
/// 键盘升起/收起的弹簧。
pub const PRESENT_SPRING: Spring = Spring::SMOOTH;

/// Shift 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ShiftState {
    /// 小写。
    #[default]
    Off,
    /// 下一个字母大写。
    Once,
    /// 大写锁定。
    Locked,
}

/// 键盘产生的动作，交给输入法引擎（见 `InputMethod`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// 字母（已按 Shift 转换大小写）。中文模式下送入拼音组字。
    Char(char),
    /// 直接输入的文字（标点、数字、符号、长按备选）。
    Text(String),
    /// 空格。
    Space,
    /// 退格。
    Backspace,
    /// 换行 / 确认。
    Return,
    /// 中/英切换。
    ToggleLanguage,
    /// 收起键盘。
    Hide,
}

/// 键帽上的符号（由宿主映射到图标，例如 Lucide）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeySymbol {
    /// Shift（未激活）。
    Shift,
    /// Shift（下一个字母大写）。
    ShiftOnce,
    /// 大写锁定。
    CapsLock,
    /// 退格。
    Backspace,
    /// 收起键盘。
    HideKeyboard,
}

/// 键帽内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyLabel {
    /// 文字。
    Text(String),
    /// 文字 + 角标（长按可得的第一个备选，例如 “，” 上方的 “！”）。
    TextWithHint(String, String),
    /// 符号。
    Symbol(KeySymbol),
    /// 中/英键：宿主画 “中/英”，并突出当前模式。
    Language {
        /// 当前是否为中文。
        chinese: bool,
    },
}

/// 长按弹出的备选条。
#[derive(Debug, Clone, PartialEq)]
pub struct AlternatesPopup {
    /// 被长按的键。
    pub key: KeyId,
    /// 背板（可能超出键盘上边，宿主应画在键盘之上的图层）。
    pub frame: Rect,
    /// 每个备选的位置与文字。
    pub cells: Vec<(Rect, String)>,
    /// 当前选中的备选。
    pub selected: usize,
}

impl AlternatesPopup {
    fn select_at(&mut self, x: f64) {
        if let (Some(first), n) = (self.cells.first(), self.cells.len()) {
            let w = first.0.w.max(1.0);
            let i = ((x - first.0.x) / w).floor();
            self.selected = (i.max(0.0) as usize).min(n - 1);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    /// 普通按下：抬起时输出当前键。
    Pressing,
    /// Shift 按住中。`typed`：按住期间打过字母。
    Shift { typed: bool },
    /// 退格连删中。
    Repeat { next: f64 },
    /// 长按备选条打开。
    Popup(AlternatesPopup),
    /// 已经输出过（快速连打提前上屏、手指滑出退格键等），抬起时什么都不做。
    Done,
}

#[derive(Debug, Clone)]
struct Touch {
    slot: u32,
    key: Option<KeyId>,
    /// 当前键上的按下位置与时间（换键时重置），用于判断长按。
    anchor: (f64, f64),
    anchor_time: f64,
    pos: (f64, f64),
    phase: Phase,
    /// 由换层键按下切换了层：如果在字符键上抬起，输出后回到这一层。
    return_layer: Option<Layer>,
}

/// 屏幕键盘模型。
#[derive(Debug, Clone)]
pub struct Keyboard {
    params: LayoutParams,
    layer: Layer,
    chinese: bool,
    shift: ShiftState,
    layout: KeyboardLayout,
    touches: Vec<Touch>,
    last_shift_down: Option<f64>,
    highlight: Vec<SpringAnimation<f64>>,
    presentation: SpringAnimation<f64>,
    revision: u64,
}

impl Keyboard {
    /// 新建键盘（字母层、隐藏状态）。
    pub fn new(params: LayoutParams, chinese: bool, now: f64) -> Keyboard {
        let layout = KeyboardLayout::compute(&params, Layer::Letters, chinese);
        let highlight =
            vec![SpringAnimation::at_rest(KEY_RELEASE_SPRING, 0.0, now); layout.keys.len()];
        Keyboard {
            params,
            layer: Layer::Letters,
            chinese,
            shift: ShiftState::Off,
            layout,
            touches: Vec::new(),
            last_shift_down: None,
            highlight,
            presentation: SpringAnimation::at_rest(PRESENT_SPRING, 0.0, now),
            revision: 0,
        }
    }

    // ---- 状态 ----

    /// 当前布局。
    pub fn layout(&self) -> &KeyboardLayout {
        &self.layout
    }

    /// 布局或键帽内容的版本号：变化时宿主需要重新栅格化键帽纹理
    /// （按下高亮不改变它，它每帧由 [`Keyboard::highlight`] 给出）。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// 当前层。
    pub fn layer(&self) -> Layer {
        self.layer
    }

    /// 是否中文模式。
    pub fn chinese(&self) -> bool {
        self.chinese
    }

    /// Shift 状态。
    pub fn shift(&self) -> ShiftState {
        self.shift
    }

    /// 修改尺寸（例如屏幕旋转）。正在进行的触摸被取消。
    pub fn set_params(&mut self, params: LayoutParams, now: f64) {
        if params == self.params {
            return;
        }
        self.params = params;
        self.cancel_touches(now);
        self.relayout();
    }

    /// 切换中/英文模式（与引擎的 ascii_mode 保持一致）。
    pub fn set_chinese(&mut self, chinese: bool) {
        if chinese != self.chinese {
            self.chinese = chinese;
            self.relayout();
        }
    }

    /// 切换层。离开字母层时，一次性的 Shift 失效。
    pub fn set_layer(&mut self, layer: Layer) {
        if layer == self.layer {
            return;
        }
        if layer != Layer::Letters && self.shift == ShiftState::Once {
            self.shift = ShiftState::Off;
        }
        self.layer = layer;
        self.relayout();
    }

    /// 设置 Shift（例如句首自动大写）。
    pub fn set_shift(&mut self, shift: ShiftState) {
        if shift != self.shift {
            self.shift = shift;
            self.revision += 1;
        }
    }

    fn relayout(&mut self) {
        self.layout = KeyboardLayout::compute(&self.params, self.layer, self.chinese);
        // 各层网格相同，键数不变；尺寸类变化时才会不同。
        let idle = SpringAnimation::at_rest(KEY_RELEASE_SPRING, 0.0, 0.0);
        self.highlight.resize(self.layout.keys.len(), idle);
        self.revision += 1;
    }

    /// 键帽内容（随层、Shift、中英模式变化）。
    pub fn label(&self, id: KeyId) -> KeyLabel {
        let upper = self.shift != ShiftState::Off;
        match self.layout.keys[id].kind {
            KeyKind::Letter(c) => KeyLabel::Text(if upper {
                c.to_ascii_uppercase().to_string()
            } else {
                c.to_string()
            }),
            KeyKind::Char(s) => {
                let alts = alternates(KeyKind::Char(s), self.chinese);
                match (self.layer, alts.first()) {
                    // 字母层的标点键在角上提示第一个备选（iPad 的 “,!” “.?”）。
                    (Layer::Letters, Some(hint)) => {
                        KeyLabel::TextWithHint(s.to_owned(), (*hint).to_owned())
                    }
                    _ => KeyLabel::Text(s.to_owned()),
                }
            }
            KeyKind::Shift => KeyLabel::Symbol(match self.shift {
                ShiftState::Off => KeySymbol::Shift,
                ShiftState::Once => KeySymbol::ShiftOnce,
                ShiftState::Locked => KeySymbol::CapsLock,
            }),
            KeyKind::Backspace => KeyLabel::Symbol(KeySymbol::Backspace),
            KeyKind::Hide => KeyLabel::Symbol(KeySymbol::HideKeyboard),
            KeyKind::Return => KeyLabel::Text(if self.chinese { "换行" } else { "return" }.into()),
            KeyKind::Space => KeyLabel::Text(if self.chinese { "空格" } else { "space" }.into()),
            KeyKind::Layer(Layer::Numbers) => KeyLabel::Text("123".into()),
            KeyKind::Layer(Layer::Symbols) => KeyLabel::Text("#+=".into()),
            KeyKind::Layer(Layer::Letters) => {
                KeyLabel::Text(if self.chinese { "拼音" } else { "ABC" }.into())
            }
            KeyKind::Language => KeyLabel::Language {
                chinese: self.chinese,
            },
        }
    }

    /// 键是否正被按住。
    pub fn is_pressed(&self, id: KeyId) -> bool {
        self.touches
            .iter()
            .any(|t| t.key == Some(id) && t.phase != Phase::Done)
    }

    /// 键的按下高亮（0–1），用于绘制按下态（底色变化）。
    pub fn highlight(&self, id: KeyId, now: f64) -> f64 {
        self.highlight
            .get(id)
            .map_or(0.0, |h| h.value(now).clamp(0.0, 1.0))
    }

    /// 当前打开的长按备选条。
    pub fn popup(&self) -> Option<&AlternatesPopup> {
        self.touches.iter().find_map(|t| match &t.phase {
            Phase::Popup(p) => Some(p),
            _ => None,
        })
    }

    /// 是否还有动画（按键高亮渐隐、键盘升起/收起）需要逐帧重绘。
    pub fn is_animating(&self, now: f64) -> bool {
        !self.presentation.is_done(now) || self.highlight.iter().any(|h| !h.is_done(now))
    }

    // ---- 升起 / 收起 ----

    /// 升起键盘。
    pub fn show(&mut self, now: f64) {
        self.presentation.retarget(1.0, now);
    }

    /// 收起键盘（取消所有触摸）。
    pub fn hide(&mut self, now: f64) {
        self.cancel_touches(now);
        self.presentation.retarget(0.0, now);
    }

    /// 是否处于（或正在进入）显示状态。
    pub fn is_shown(&self) -> bool {
        self.presentation.target() > 0.5
    }

    /// 升起进度：0 = 完全收起，1 = 完全升起。宿主把键盘图层下移
    /// `(1 - p) × 键盘高度`。
    pub fn presentation(&self, now: f64) -> f64 {
        self.presentation.value(now)
    }

    // ---- 触摸 ----

    /// 手指按下。返回需要立即执行的动作（例如退格、快速连打时提前上屏的字符）。
    ///
    /// 落在候选栏里的触摸不处理（宿主先用 [`KeyboardLayout::hit_test`] 分流）。
    pub fn touch_down(&mut self, slot: u32, x: f64, y: f64, now: f64) -> Vec<KeyAction> {
        let mut out = Vec::new();
        if self.touches.iter().any(|t| t.slot == slot) {
            out.extend(self.touch_up(slot, now));
        }
        let key = match self.layout.hit_test(x, y) {
            Some(Hit::Key(id)) => Some(id),
            _ => None,
        };
        let kind = key.map(|id| self.layout.keys[id].kind);

        // 快速连打：其他按着的字符键立即输出。
        if kind.is_some_and(|k| k.types_text()) {
            for i in 0..self.touches.len() {
                let t = &self.touches[i];
                if t.phase == Phase::Pressing
                    && t.key
                        .is_some_and(|id| self.layout.keys[id].kind.types_text())
                {
                    out.extend(self.release(i, now));
                    self.touches[i].phase = Phase::Done;
                    if let Some(id) = self.touches[i].key {
                        self.release_highlight(id, now);
                    }
                }
            }
        }

        let mut touch = Touch {
            slot,
            key,
            anchor: (x, y),
            anchor_time: now,
            pos: (x, y),
            phase: Phase::Pressing,
            return_layer: None,
        };
        match kind {
            Some(KeyKind::Shift) => {
                self.shift_down(now);
                touch.phase = Phase::Shift { typed: false };
            }
            Some(KeyKind::Layer(layer)) => {
                touch.return_layer = Some(self.layer);
                self.set_layer(layer);
            }
            Some(KeyKind::Backspace) => {
                out.push(KeyAction::Backspace);
                touch.phase = Phase::Repeat {
                    next: now + REPEAT_DELAY,
                };
            }
            Some(_) => {}
            None => touch.phase = Phase::Done,
        }
        if let Some(id) = key {
            self.press_highlight(id, now);
        }
        self.touches.push(touch);
        out
    }

    /// 手指移动：滑到别的键上时改为那个键；备选条打开时选择备选。
    pub fn touch_motion(&mut self, slot: u32, x: f64, y: f64, now: f64) -> Vec<KeyAction> {
        let Some(i) = self.touches.iter().position(|t| t.slot == slot) else {
            return Vec::new();
        };
        self.touches[i].pos = (x, y);
        let current = self.touches[i].key;
        // 还在当前键（含容差）内就不换键，避免在边界上来回跳。
        let stay =
            current.is_some_and(|id| self.layout.keys[id].hit.inflate(TOUCH_SLOP).contains(x, y));
        if let Phase::Popup(popup) = &mut self.touches[i].phase {
            let before = popup.selected;
            popup.select_at(x);
            if popup.selected != before {
                self.revision += 1;
            }
            return Vec::new();
        }
        if stay {
            return Vec::new();
        }
        match self.touches[i].phase {
            Phase::Pressing => {
                let next = match self.layout.hit_test(x, y) {
                    Some(Hit::Key(id)) => Some(id),
                    _ => None,
                };
                if next != current {
                    let t = &mut self.touches[i];
                    t.key = next;
                    t.anchor = (x, y);
                    t.anchor_time = now;
                    if let Some(old) = current {
                        self.release_highlight(old, now);
                    }
                    if let Some(new) = next {
                        self.press_highlight(new, now);
                    }
                }
            }
            Phase::Repeat { .. } => {
                // 手指滑出退格键：停止连删。
                self.touches[i].phase = Phase::Done;
                if let Some(old) = current {
                    self.release_highlight(old, now);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// 手指抬起：输出当前键对应的动作。
    pub fn touch_up(&mut self, slot: u32, now: f64) -> Vec<KeyAction> {
        let Some(i) = self.touches.iter().position(|t| t.slot == slot) else {
            return Vec::new();
        };
        let out = self.release(i, now);
        let touch = self.touches.remove(i);
        if let Some(id) = touch.key {
            self.release_highlight(id, now);
        }
        if matches!(touch.phase, Phase::Popup(_)) {
            self.revision += 1; // 备选条关闭
        }
        out
    }

    /// 取消所有触摸（例如系统手势接管、键盘收起），不输出任何动作。
    pub fn touch_cancel(&mut self, now: f64) {
        self.cancel_touches(now);
    }

    fn cancel_touches(&mut self, now: f64) {
        for t in std::mem::take(&mut self.touches) {
            if let Some(id) = t.key {
                self.release_highlight(id, now);
            }
        }
        self.revision += 1;
    }

    /// 驱动长按与连删。返回到期产生的动作。
    pub fn tick(&mut self, now: f64) -> Vec<KeyAction> {
        let mut out = Vec::new();
        for i in 0..self.touches.len() {
            let t = &self.touches[i];
            match t.phase {
                Phase::Pressing => {
                    let Some(id) = t.key else { continue };
                    let moved = (t.pos.0 - t.anchor.0).hypot(t.pos.1 - t.anchor.1);
                    if now - t.anchor_time >= LONG_PRESS_DELAY && moved <= TOUCH_SLOP {
                        if let Some(popup) = self.make_popup(id, t.pos.0) {
                            self.touches[i].phase = Phase::Popup(popup);
                            self.revision += 1;
                        }
                    }
                }
                Phase::Repeat { mut next } => {
                    // 帧率很低时一次最多补 5 次，避免一下删掉一大片。
                    let mut n = 0;
                    while next <= now && n < 5 {
                        out.push(KeyAction::Backspace);
                        next += REPEAT_INTERVAL;
                        n += 1;
                    }
                    if next <= now {
                        next = now + REPEAT_INTERVAL;
                    }
                    self.touches[i].phase = Phase::Repeat { next };
                }
                _ => {}
            }
        }
        out
    }

    /// 下一次需要调用 [`Keyboard::tick`] 的时刻（没有待触发的长按/连删时为 `None`）。
    pub fn next_deadline(&self) -> Option<f64> {
        self.touches
            .iter()
            .filter_map(|t| match t.phase {
                Phase::Pressing => t
                    .key
                    .filter(|&id| self.has_alternates(id))
                    .map(|_| t.anchor_time + LONG_PRESS_DELAY),
                Phase::Repeat { next } => Some(next),
                _ => None,
            })
            .min_by(f64::total_cmp)
    }

    // ---- 内部 ----

    fn shift_down(&mut self, now: f64) {
        let double = self
            .last_shift_down
            .is_some_and(|t| now - t <= SHIFT_DOUBLE_TAP);
        self.shift = match self.shift {
            ShiftState::Off => ShiftState::Once,
            ShiftState::Once if double => ShiftState::Locked,
            ShiftState::Once | ShiftState::Locked => ShiftState::Off,
        };
        // 刚进入锁定时清掉记录，避免第三下又被当成双击。
        self.last_shift_down = (self.shift != ShiftState::Locked).then_some(now);
        self.revision += 1;
    }

    /// 第 `i` 个触摸抬起（或被快速连打提前结束）时输出的动作。
    fn release(&mut self, i: usize, _now: f64) -> Vec<KeyAction> {
        let t = &self.touches[i];
        let return_layer = t.return_layer;
        let action = match (&t.phase, t.key) {
            (Phase::Pressing, Some(id)) => match self.layout.keys[id].kind {
                KeyKind::Letter(c) => Some(KeyAction::Char(self.apply_shift(c))),
                KeyKind::Char(s) => Some(KeyAction::Text(s.to_owned())),
                KeyKind::Space => Some(KeyAction::Space),
                KeyKind::Return => Some(KeyAction::Return),
                KeyKind::Language => Some(KeyAction::ToggleLanguage),
                KeyKind::Hide => Some(KeyAction::Hide),
                KeyKind::Layer(_) | KeyKind::Shift | KeyKind::Backspace => None,
            },
            (Phase::Popup(p), _) => p
                .cells
                .get(p.selected)
                .map(|(_, s)| KeyAction::Text(s.clone())),
            (Phase::Shift { typed: true }, _) if self.shift == ShiftState::Once => {
                // 按住 Shift 打过字母：松开后恢复小写。
                self.shift = ShiftState::Off;
                self.revision += 1;
                None
            }
            _ => None,
        };
        let typed_letter = matches!(
            (&self.touches[i].phase, self.touches[i].key),
            (Phase::Pressing | Phase::Popup(_), Some(id))
                if matches!(self.layout.keys[id].kind, KeyKind::Letter(_))
        );
        if typed_letter {
            self.after_letter();
        }
        // 从换层键滑到字符上抬起：输出后回到原来的层。
        if let (Some(prev), Some(KeyAction::Char(_) | KeyAction::Text(_) | KeyAction::Space)) =
            (return_layer, &action)
        {
            self.set_layer(prev);
        }
        action.into_iter().collect()
    }

    fn apply_shift(&self, c: char) -> char {
        if self.shift == ShiftState::Off {
            c
        } else {
            c.to_ascii_uppercase()
        }
    }

    /// 输出一个字母之后：一次性的 Shift 失效（按住 Shift 时等松开再说）。
    fn after_letter(&mut self) {
        let mut held = false;
        for t in &mut self.touches {
            if let Phase::Shift { typed } = &mut t.phase {
                *typed = true;
                held = true;
            }
        }
        if !held && self.shift == ShiftState::Once {
            self.shift = ShiftState::Off;
            self.revision += 1;
        }
    }

    fn has_alternates(&self, id: KeyId) -> bool {
        !alternates(self.layout.keys[id].kind, self.chinese).is_empty()
    }

    fn make_popup(&self, id: KeyId, finger_x: f64) -> Option<AlternatesPopup> {
        let kind = self.layout.keys[id].kind;
        let alts = alternates(kind, self.chinese);
        if alts.is_empty() {
            return None;
        }
        let upper = matches!(kind, KeyKind::Letter(_)) && self.shift != ShiftState::Off;
        let key = &self.layout.keys[id].rect;
        let m = &self.layout.metrics;
        let cell_w = (m.key_width * 0.7).clamp(44.0, 64.0).round();
        let n = alts.len() as f64;
        let total = cell_w * n;
        let (min_x, max_x) = (m.edge_padding, self.layout.params.width - m.edge_padding);
        // 第一个备选在键的正上方，向右排开；靠近右边缘时整体左移。
        let x0 = (key.center().0 - cell_w / 2.0)
            .min(max_x - total)
            .max(min_x);
        let y = key.y - m.row_gap - key.h;
        let cells: Vec<(Rect, String)> = alts
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let text = if upper {
                    s.to_uppercase()
                } else {
                    (*s).to_owned()
                };
                (Rect::new(x0 + i as f64 * cell_w, y, cell_w, key.h), text)
            })
            .collect();
        let mut popup = AlternatesPopup {
            key: id,
            frame: Rect::new(x0, y, total, key.h),
            cells,
            selected: 0,
        };
        popup.select_at(finger_x);
        Some(popup)
    }

    fn press_highlight(&mut self, id: KeyId, now: f64) {
        if let Some(h) = self.highlight.get_mut(id) {
            // 立即点亮：按键反馈不能有延迟。
            h.retarget(1.0, now);
            h.jump_to(1.0, 0.0, now);
        }
    }

    fn release_highlight(&mut self, id: KeyId, now: f64) {
        if self
            .touches
            .iter()
            .any(|t| t.key == Some(id) && t.phase != Phase::Done)
        {
            return; // 还有别的手指按着它
        }
        if let Some(h) = self.highlight.get_mut(id) {
            h.retarget(0.0, now);
        }
    }
}
