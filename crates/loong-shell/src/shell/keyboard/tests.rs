//! 屏幕键盘的状态测试：激活与可见性、布局尺寸、候选栏命中与滚动、上屏流程。
//! 用一个查表的假拼音引擎代替 librime（不依赖系统数据，结果确定）。

use loong_ime::{Candidate, ImeState, KeyOutcome, Preedit};

use super::*;
use crate::shell::{Shell, TouchRoute};

const FRAME: f64 = 1.0 / 120.0;

/// 查表的假拼音引擎：小写字母组字，每页 `page_size` 个候选。
#[derive(Debug)]
struct FakeEngine {
    raw: String,
    page: usize,
    page_size: usize,
    ascii: bool,
    commit: String,
    state: ImeState,
    revision: u64,
}

const TABLE: &[(&str, &[&str])] = &[
    ("ni", &["你", "呢", "泥", "尼", "拟"]),
    ("nihao", &["你好", "拟好", "妳好", "逆号", "你", "呢"]),
    ("zhongguo", &["中国", "中过"]),
];

impl FakeEngine {
    fn new(page_size: usize) -> Self {
        let mut e = FakeEngine {
            raw: String::new(),
            page: 0,
            page_size,
            ascii: false,
            commit: String::new(),
            state: ImeState::default(),
            revision: 0,
        };
        e.refresh();
        e
    }

    fn all(&self) -> Vec<String> {
        if self.raw.is_empty() {
            return Vec::new();
        }
        TABLE
            .iter()
            .find(|(k, _)| *k == self.raw)
            .map(|(_, v)| v.iter().map(|s| s.to_string()).collect())
            .unwrap_or_else(|| vec![self.raw.clone()])
    }

    fn refresh(&mut self) {
        let all = self.all();
        let start = self.page * self.page_size;
        let state = ImeState {
            preedit: (!self.raw.is_empty()).then(|| Preedit {
                text: self.raw.clone(),
                cursor: self.raw.len(),
                sel_start: 0,
                sel_end: self.raw.len(),
            }),
            raw_input: self.raw.clone(),
            candidates: all
                .iter()
                .skip(start)
                .take(self.page_size)
                .map(|t| Candidate {
                    text: t.clone(),
                    comment: None,
                })
                .collect(),
            page_no: self.page,
            page_size: self.page_size,
            is_last_page: start + self.page_size >= all.len(),
            highlighted: 0,
            commit_preview: all.first().cloned(),
            ascii_mode: self.ascii,
        };
        if state != self.state {
            self.state = state;
            self.revision += 1;
        }
    }

    fn select(&mut self, i: usize) -> bool {
        let Some(c) = self.state.candidates.get(i) else {
            return false;
        };
        self.commit.push_str(&c.text);
        self.raw.clear();
        self.page = 0;
        self.refresh();
        true
    }
}

impl Engine for FakeEngine {
    fn state(&self) -> &ImeState {
        &self.state
    }
    fn revision(&self) -> u64 {
        self.revision
    }
    fn take_commit(&mut self) -> Option<String> {
        (!self.commit.is_empty()).then(|| std::mem::take(&mut self.commit))
    }
    fn process_key(&mut self, key: ImeKey) -> KeyOutcome {
        let composing = !self.raw.is_empty();
        match key {
            ImeKey::Char(c) if !self.ascii && c.is_ascii_lowercase() => {
                self.raw.push(c);
                self.page = 0;
            }
            ImeKey::Char(c) => self.commit.push(c),
            ImeKey::Space if composing => {
                self.select(0);
            }
            ImeKey::Space => self.commit.push(' '),
            ImeKey::Backspace if composing => {
                self.raw.pop();
            }
            ImeKey::Enter if composing => {
                self.commit.push_str(&std::mem::take(&mut self.raw));
            }
            other => return KeyOutcome::Forward(other),
        }
        self.refresh();
        KeyOutcome::Handled
    }
    fn insert_text(&mut self, text: &str) {
        if !self.raw.is_empty() {
            self.select(0);
        }
        self.commit.push_str(text);
    }
    fn select_candidate(&mut self, index: usize) -> bool {
        self.select(index)
    }
    fn next_page(&mut self) -> bool {
        if self.state.is_last_page {
            return false;
        }
        self.page += 1;
        self.refresh();
        true
    }
    fn prev_page(&mut self) -> bool {
        if self.page == 0 {
            return false;
        }
        self.page -= 1;
        self.refresh();
        true
    }
    fn set_ascii_mode(&mut self, ascii: bool) {
        if ascii && !self.raw.is_empty() {
            self.commit.push_str(&std::mem::take(&mut self.raw));
        }
        self.ascii = ascii;
        self.refresh();
    }
    fn commit_composition(&mut self) {
        if !self.raw.is_empty() {
            self.select(0);
        }
    }
    fn clear(&mut self) {
        self.raw.clear();
        self.page = 0;
        self.refresh();
    }
}

/// 只有键盘（不经过 Shell 的手势路由）。
struct Kb {
    kb: ScreenKeyboard,
    now: f64,
    slot: u32,
}

impl Kb {
    fn new(w: f64, h: f64, page_size: Option<usize>) -> Self {
        let mut kb = ScreenKeyboard::new(w, h, 0.0);
        if let Some(n) = page_size {
            kb.set_engine(Box::new(FakeEngine::new(n)));
        }
        Kb {
            kb,
            now: 0.0,
            slot: 0,
        }
    }

    fn wait(&mut self, secs: f64) {
        let end = self.now + secs;
        while self.now < end - 1e-9 {
            self.now = (self.now + FRAME).min(end);
            self.kb.update(self.now);
        }
    }

    /// 输入框获得焦点，系统允许显示，等键盘升起到位。
    fn focus(&mut self) {
        self.kb.activate(Some(ContentPurpose::Normal), self.now);
        self.kb.set_allowed(true, self.now);
        self.wait(1.0);
    }

    fn tap(&mut self, x: f64, y: f64) {
        self.slot += 1;
        assert!(self.kb.contains(x, y, self.now), "({x}, {y}) 不在键盘上");
        self.kb.touch_down(self.slot, x, y, self.now);
        self.wait(0.05);
        self.kb.touch_up(self.slot, self.now);
        self.wait(0.05);
    }

    fn type_str(&mut self, text: &str) {
        for c in text.chars() {
            let (x, y) = self.kb.key_center(c, self.now).expect("键存在");
            self.tap(x, y);
        }
    }

    fn drag(&mut self, from: (f64, f64), to: (f64, f64), dur: f64) {
        self.slot += 1;
        self.kb.touch_down(self.slot, from.0, from.1, self.now);
        let steps = (dur * 120.0).ceil() as usize;
        for k in 1..=steps {
            let f = k as f64 / steps as f64;
            self.now += dur / steps as f64;
            let x = from.0 + (to.0 - from.0) * f;
            let y = from.1 + (to.1 - from.1) * f;
            self.kb.touch_motion(self.slot, x, y, self.now);
            self.kb.update(self.now);
        }
        self.kb.touch_up(self.slot, self.now);
    }
}

#[test]
fn shows_only_when_active_and_allowed() {
    let mut t = Kb::new(1280.0, 800.0, None);
    assert!(!t.kb.is_shown() && t.kb.is_idle(0.0));
    // 激活但系统不允许（例如应用还在打开）：不显示。
    t.kb.activate(Some(ContentPurpose::Normal), 0.0);
    t.wait(0.5);
    assert!(t.kb.is_active() && !t.kb.is_shown());
    assert_eq!(t.kb.presentation(t.now), 0.0);
    // 允许后升起：先不让出窗口高度，到位后才让出。
    t.kb.set_allowed(true, t.now);
    assert!(t.kb.is_shown());
    t.wait(0.1);
    let p = t.kb.presentation(t.now);
    assert!(p > 0.0 && p < 0.9, "{p}");
    assert_eq!(t.kb.window_inset(), 0.0);
    t.wait(1.0);
    assert_eq!(t.kb.presentation(t.now), 1.0);
    assert_eq!(t.kb.window_inset(), t.kb.height());
    assert_eq!(t.kb.top(t.now), 800.0 - t.kb.height());
    assert!(t.kb.is_idle(t.now));
    // 系统手势（不允许）：立即还给应用全部高度，然后收起。
    t.kb.set_allowed(false, t.now);
    assert_eq!(t.kb.window_inset(), 0.0);
    assert!(!t.kb.is_shown() && t.kb.is_active());
    t.kb.set_allowed(true, t.now);
    t.wait(1.0);
    // 失去焦点：收起。
    t.kb.deactivate(t.now);
    assert!(!t.kb.is_shown() && !t.kb.is_active());
    assert_eq!(t.kb.window_inset(), 0.0);
    t.wait(1.0);
    assert_eq!(t.kb.presentation(t.now), 0.0);
    assert!(t.kb.is_idle(t.now));
}

#[test]
fn second_enable_keeps_state_and_purpose() {
    let mut t = Kb::new(1280.0, 800.0, Some(9));
    t.kb.activate(Some(ContentPurpose::Phone), 0.0);
    t.kb.set_allowed(true, 0.0);
    t.wait(1.0);
    assert_eq!(
        t.kb.input_method().keyboard().layer(),
        loong_ime::keyboard::Layer::Numbers
    );
    t.kb.take_ops();
    // WPE 连发两次 enable，第二次不带内容类型：保持电话号码的数字层，动画不重播。
    t.kb.activate(None, t.now);
    assert_eq!(t.kb.purpose(), ContentPurpose::Phone);
    assert_eq!(t.kb.presentation(t.now), 1.0);
    assert!(t.kb.is_idle(t.now));
    // 新输入框（普通文本）：回到字母层，默认中文。
    t.kb.deactivate(t.now);
    t.kb.activate(None, t.now);
    assert_eq!(t.kb.purpose(), ContentPurpose::Normal);
    assert!(t.kb.input_method().is_chinese());
    // 密码框：英文。
    t.kb.activate(Some(ContentPurpose::Password), t.now);
    assert!(!t.kb.input_method().is_chinese());
}

#[test]
fn layout_fits_landscape_and_portrait() {
    for (w, h, height, bar) in [(1280.0, 800.0, 396.0, 50.0), (800.0, 1280.0, 332.0, 40.0)] {
        let mut t = Kb::new(w, h, None);
        t.focus();
        assert_eq!(t.kb.height(), height, "{w}x{h}");
        let layout = t.kb.layout();
        assert_eq!(layout.candidate_bar.h, bar);
        assert_eq!(layout.params.width, w);
        // 键盘不超过屏高的一半，所有键都在屏幕内、在底部手势区以上。
        assert!(t.kb.height() <= h * 0.5);
        for id in 0..layout.keys.len() {
            let r = t.kb.key_rect(id, t.now);
            assert!(r.x >= 0.0 && r.x + r.w <= w, "{id}: {r:?}");
            assert!(r.y >= h - height && r.y + r.h <= h - 20.0, "{id}: {r:?}");
        }
        // 放大小笺在键的上方、屏幕之内。
        let (pw, ph) = t.kb.preview_size();
        assert!(pw > layout.metrics.key_width && ph > layout.metrics.key_height);
        let q = t.kb.layout().find(KeyKind::Letter('q')).unwrap();
        let r = t.kb.preview_rect(q, t.now);
        assert!(r.x >= 0.0 && r.y + r.h <= t.kb.key_rect(q, t.now).y);
    }
}

#[test]
fn english_without_engine_commits_and_forwards_in_order() {
    let mut t = Kb::new(1280.0, 800.0, None);
    t.focus();
    t.type_str("hi");
    assert_eq!(
        t.kb.take_ops(),
        vec![
            TextOp::Commit("h".into()),
            TextOp::Commit("i".into()),
            TextOp::Done
        ]
    );
    // 上屏 “a” 之后退格：先 done 让 “a” 生效，再发合成的退格键。
    t.type_str("a\u{8}");
    assert_eq!(
        t.kb.take_ops(),
        vec![
            TextOp::Commit("a".into()),
            TextOp::Done,
            TextOp::Key(ImeKey::Backspace)
        ]
    );
    t.type_str("\n");
    assert_eq!(t.kb.take_ops(), vec![TextOp::Key(ImeKey::Enter)]);
}

#[test]
fn pinyin_preedit_then_candidate_commit() {
    let mut t = Kb::new(1280.0, 800.0, Some(9));
    t.focus();
    assert!(t.kb.input_method().is_chinese());
    t.type_str("ni");
    assert_eq!(
        t.kb.take_ops(),
        vec![TextOp::Preedit(Some(("ni".into(), 2))), TextOp::Done]
    );
    t.type_str("hao");
    assert_eq!(
        t.kb.take_ops(),
        vec![TextOp::Preedit(Some(("nihao".into(), 5))), TextOp::Done]
    );
    // 候选栏：拼音在左，候选从左到右，第一个高亮。
    let (pinyin, pw) = t.kb.preedit_cell().unwrap();
    assert_eq!(pinyin, "nihao");
    assert!(pw > 0.0);
    let cells = t.kb.candidate_cells();
    assert_eq!(cells[0].text, "你好");
    assert_eq!(cells.len(), 6, "6 个候选，一页放得下，没有翻页箭头");
    assert!(cells.windows(2).all(|c| c[0].x + c[0].w <= c[1].x + 1e-9));
    assert_eq!(t.kb.highlighted_candidate(), Some(0));
    // 点 “你好”：上屏并清除预编辑，一个 done。
    let (x, y) = t.kb.candidate_center(0, t.now).unwrap();
    assert!(y < t.kb.top(t.now) + t.kb.layout().candidate_bar.h);
    t.tap(x, y);
    assert_eq!(
        t.kb.take_ops(),
        vec![
            TextOp::Commit("你好".into()),
            TextOp::Preedit(None),
            TextOp::Done
        ]
    );
    assert!(t.kb.candidate_cells().is_empty() && t.kb.preedit_cell().is_none());
    // 空格选高亮候选；标点先把组字上屏。
    t.type_str("ni ");
    let ops = t.kb.take_ops();
    assert!(ops.contains(&TextOp::Commit("你".into())), "{ops:?}");
    // 一批消息里只发最终的预编辑：中间的 “n”“ni” 不必逐个发送。
    t.type_str("ni，");
    assert_eq!(
        t.kb.take_ops(),
        vec![
            TextOp::Commit("你，".into()),
            TextOp::Preedit(None),
            TextOp::Done
        ]
    );
}

#[test]
fn candidate_bar_hit_testing_and_paging() {
    // 每页 2 个候选：nihao 有 3 页。
    let mut t = Kb::new(1280.0, 800.0, Some(2));
    t.focus();
    t.type_str("nihao");
    t.kb.take_ops();
    let kinds: Vec<CellKind> = t.kb.candidate_cells().iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        vec![
            CellKind::Candidate(0),
            CellKind::Candidate(1),
            CellKind::NextPage
        ]
    );
    // 点 “›” 翻到第 2 页：前面多一个 “‹”。
    let left = t.kb.preedit_cell().unwrap().1;
    let next = t.kb.candidate_cells()[2].clone();
    let bar_y = t.kb.top(t.now) + 20.0;
    t.tap(left + next.x + next.w / 2.0, bar_y);
    let texts: Vec<&str> =
        t.kb.candidate_cells()
            .iter()
            .map(|c| c.text.as_str())
            .collect();
    assert_eq!(texts, vec!["‹", "妳好", "逆号", "›"]);
    // 翻页不上屏，只更新预编辑（内容没变时也没关系）。
    assert!(
        !t.kb
            .take_ops()
            .iter()
            .any(|op| matches!(op, TextOp::Commit(_)))
    );
    // 点第 2 页的第 1 个候选 → “妳好”。
    let (x, y) = t.kb.candidate_center(0, t.now).unwrap();
    t.tap(x, y);
    assert_eq!(t.kb.take_ops()[0], TextOp::Commit("妳好".into()));
    // 键区里的点不会选到候选；候选栏格子以外（拼音上）点按没有效果。
    t.type_str("nihao");
    t.kb.take_ops();
    t.tap(5.0, bar_y);
    assert!(t.kb.take_ops().is_empty());
}

#[test]
fn candidate_strip_scrolls_and_pulls_to_next_page() {
    // 窄屏、每页 9 个：候选条比屏幕宽，可以横向滚动。
    let mut t = Kb::new(800.0, 1280.0, Some(5));
    t.focus();
    t.type_str("ni");
    t.kb.take_ops();
    // 拼音以外的宽度不够放下 5 个候选 + “›”：用 ni 的 5 个单字 + 箭头，内容宽度大于可视宽度时可滚动。
    let cells = t.kb.candidate_cells().to_vec();
    let content: f64 = cells.iter().map(|c| c.w).sum();
    let y = t.kb.top(t.now) + 20.0;
    let visible = 800.0 - t.kb.preedit_cell().unwrap().1;
    if content > visible {
        t.drag((700.0, y), (300.0, y), 0.2);
        t.wait(1.0);
        let max = content - visible;
        let s = t.kb.bar.scroll.value(t.now);
        assert!(s > 0.0 && s <= max + 1e-6, "滚动停在范围内：{s} / {max}");
    }
    // 一页放得下时：向左拖过末端（橡皮筋）松手 → 翻到下一页（这里 ni 只有 1 页，不翻）。
    let before = t.kb.candidate_version();
    t.drag((700.0, y), (100.0, y), 0.15);
    t.wait(1.0);
    assert_eq!(t.kb.candidate_version(), before, "最后一页拖过头不翻页");
    assert!(t.kb.bar.scroll.value(t.now) <= t.kb.max_scroll() + 1e-6);

    // nihao 每页 2 个：拖过末端翻到第 2 页。
    let mut t = Kb::new(1280.0, 800.0, Some(2));
    t.focus();
    t.type_str("nihao");
    t.kb.take_ops();
    let y = t.kb.top(t.now) + 20.0;
    t.drag((900.0, y), (500.0, y), 0.15);
    let texts: Vec<&str> =
        t.kb.candidate_cells()
            .iter()
            .map(|c| c.text.as_str())
            .collect();
    assert_eq!(texts, vec!["‹", "妳好", "逆号", "›"]);
    // 滑动不是点按：没有选中候选。
    assert!(
        !t.kb
            .take_ops()
            .iter()
            .any(|op| matches!(op, TextOp::Commit(_)))
    );
}

#[test]
fn hide_key_dismisses_and_tap_on_app_brings_it_back() {
    let mut t = Kb::new(1280.0, 800.0, Some(9));
    t.focus();
    t.type_str("ni");
    t.kb.take_ops();
    let (x, y) = t.kb.kind_center(KeyKind::Hide, t.now).expect("有收起键");
    t.tap(x, y);
    // 收起前把组字上屏。
    assert_eq!(
        t.kb.take_ops(),
        vec![
            TextOp::Commit("你".into()),
            TextOp::Preedit(None),
            TextOp::Done
        ]
    );
    assert!(!t.kb.is_shown() && t.kb.is_active());
    assert_eq!(t.kb.window_inset(), 0.0);
    t.wait(1.0);
    // 在应用上滑动：不弹出。
    t.kb.app_touch_down(7, 400.0, 300.0);
    t.kb.app_touch_motion(7, 400.0, 200.0);
    t.kb.app_touch_up(7, t.now);
    t.wait(0.5);
    assert!(!t.kb.is_shown());
    // 点按：稍等片刻后弹出（这次点按若让输入框失去焦点，应用会先发 disable）。
    t.kb.app_touch_down(8, 400.0, 300.0);
    t.kb.app_touch_up(8, t.now);
    assert!(!t.kb.is_shown() && !t.kb.is_idle(t.now));
    t.wait(0.3);
    assert!(t.kb.is_shown());
    // 点按后马上 disable：不弹出。
    t.tap(x, y);
    t.kb.take_ops();
    t.kb.app_touch_down(9, 400.0, 300.0);
    t.kb.app_touch_up(9, t.now);
    t.kb.deactivate(t.now + 0.05);
    t.wait(0.5);
    assert!(!t.kb.is_shown());
}

#[test]
fn long_press_and_backspace_repeat_are_driven_by_update() {
    let mut t = Kb::new(1280.0, 800.0, None);
    t.focus();
    let (x, y) = t.kb.key_center('\u{8}', t.now).unwrap();
    t.kb.touch_down(1, x, y, t.now);
    assert!(!t.kb.is_idle(t.now));
    t.wait(1.0);
    t.kb.touch_up(1, t.now);
    let deletes =
        t.kb.take_ops()
            .iter()
            .filter(|op| **op == TextOp::Key(ImeKey::Backspace))
            .count();
    // 按下 1 次 + 0.5 秒后每 0.1 秒 1 次。
    assert!((5..=7).contains(&deletes), "{deletes}");
    // 长按 “，” 弹出备选条，放大小笺随之隐藏。
    let (x, y) = t.kb.key_center(',', t.now).unwrap();
    t.kb.touch_down(2, x, y, t.now);
    assert!(t.kb.preview(t.now).is_some());
    t.wait(0.6);
    assert!(t.kb.input_method().keyboard().popup().is_some());
    assert!(t.kb.preview(t.now).is_none());
    t.kb.touch_up(2, t.now);
    assert!(matches!(&t.kb.take_ops()[0], TextOp::Commit(_)));
}

// ───────────────────────── 经过 Shell 的路由 ─────────────────────────

fn shell_with_keyboard() -> (Shell, f64) {
    let mut shell = Shell::new(1280.0, 800.0, 18.0);
    let notes = shell.apps.iter().position(|a| a.id == "notes").unwrap();
    shell.launch(notes, 0.0);
    let mut now = 0.0;
    let step = |shell: &mut Shell, secs: f64, now: &mut f64| {
        let end = *now + secs;
        while *now < end {
            *now += FRAME;
            shell.update(*now);
        }
    };
    step(&mut shell, 1.0, &mut now);
    shell.keyboard.activate(Some(ContentPurpose::Normal), now);
    step(&mut shell, 1.0, &mut now);
    assert!(shell.keyboard.is_shown());
    assert_eq!(shell.keyboard.presentation(now), 1.0);
    (shell, now)
}

#[test]
fn keyboard_waits_for_the_app_to_settle() {
    let mut shell = Shell::new(1280.0, 800.0, 18.0);
    let notes = shell.apps.iter().position(|a| a.id == "notes").unwrap();
    // 应用一打开输入框就获得焦点：打开动画结束后键盘才升起。
    shell.launch(notes, 0.0);
    shell.keyboard.activate(None, 0.0);
    shell.update(0.1);
    assert!(!shell.keyboard.is_shown());
    let mut now = 0.1;
    while now < 1.5 {
        now += FRAME;
        shell.update(now);
    }
    assert!(shell.keyboard.is_shown());
}

#[test]
fn keys_are_routed_to_the_keyboard_not_the_app() {
    let (mut shell, now) = shell_with_keyboard();
    let (x, y) = shell.keyboard.key_center('a', now).unwrap();
    assert!(matches!(shell.touch_down(0, x, y, now), TouchRoute::Shell));
    // 第二根手指（快速连打）同样交给键盘。
    let (x2, y2) = shell.keyboard.key_center('s', now).unwrap();
    assert!(matches!(
        shell.touch_down(1, x2, y2, now + 0.02),
        TouchRoute::Shell
    ));
    shell.touch_up(0, now + 0.05);
    shell.touch_up(1, now + 0.06);
    assert_eq!(
        shell.keyboard.take_ops(),
        vec![
            TextOp::Commit("a".into()),
            TextOp::Commit("s".into()),
            TextOp::Done
        ]
    );
    // 键盘以上的区域照常交给应用（演示应用没有窗口，路由为 Shell，但不会产生输入）。
    shell.touch_down(2, 640.0, 100.0, now + 0.1);
    shell.touch_up(2, now + 0.15);
    assert!(shell.keyboard.take_ops().is_empty());
}

#[test]
fn bottom_edge_swipe_beats_the_keyboard() {
    let (mut shell, mut now) = shell_with_keyboard();
    // 键盘底部 20pt 是 Home 指示条与系统手势区：上滑回主屏，键盘随之收起。
    shell.touch_down(0, 640.0, 795.0, now);
    assert!(!shell.keyboard.is_shown(), "系统手势开始时键盘收起");
    assert_eq!(shell.keyboard.window_inset(), 0.0);
    for k in 1..=15 {
        now += 0.01;
        shell.touch_motion(0, 640.0, 795.0 - 20.0 * k as f64, now);
        shell.update(now);
    }
    shell.touch_up(0, now);
    assert!(shell.keyboard.take_ops().is_empty(), "没有误触键");
    let end = now + 1.5;
    while now < end {
        now += FRAME;
        shell.update(now);
    }
    assert!(!shell.app_in_front());
    assert!(!shell.keyboard.is_shown());
    assert!(shell.is_idle(now));
}
