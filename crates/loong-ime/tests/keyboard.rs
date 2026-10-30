//! 屏幕键盘模型测试：布局几何、命中测试、触摸状态机。不需要 librime 数据。

use loong_ime::keyboard::{
    Hit, KeyAction, KeyKind, KeyLabel, KeySymbol, Keyboard, KeyboardLayout, Layer, LayoutParams,
    ShiftState, SizeClass, TOUCH_SLOP,
};

/// 常见平板逻辑尺寸（横竖屏）。
const SCREENS: [(f64, f64); 8] = [
    (1280.0, 800.0),
    (800.0, 1280.0),
    (1194.0, 834.0),
    (834.0, 1194.0),
    (1024.0, 768.0),
    (768.0, 1024.0),
    (1366.0, 1024.0),
    (600.0, 960.0),
];

fn all_layouts() -> Vec<KeyboardLayout> {
    let mut out = Vec::new();
    for (w, h) in SCREENS {
        let params = LayoutParams::for_screen(w, h);
        for layer in [Layer::Letters, Layer::Numbers, Layer::Symbols] {
            for chinese in [false, true] {
                out.push(KeyboardLayout::compute(&params, layer, chinese));
            }
        }
    }
    out
}

fn key_center(layout: &KeyboardLayout, kind: KeyKind) -> (f64, f64) {
    let id = layout
        .find(kind)
        .unwrap_or_else(|| panic!("找不到 {kind:?}"));
    layout.key(id).rect.center()
}

// ---------------------------------------------------------------- 布局

#[test]
fn preferred_sizes() {
    let land = LayoutParams::for_screen(1280.0, 800.0);
    let port = LayoutParams::for_screen(800.0, 1280.0);
    assert_eq!(land.size_class, SizeClass::Regular);
    assert_eq!((land.width, land.height), (1280.0, 396.0));
    assert_eq!((port.width, port.height), (800.0, 332.0));
    // 键盘约占横屏高度的一半、竖屏的四分之一（与 iPad 接近）。
    assert!((0.45..0.52).contains(&(land.height / 800.0)));
    assert!((0.24..0.28).contains(&(port.height / 1280.0)));
    assert_eq!(SizeClass::for_screen(390.0, 844.0), SizeClass::Compact);
}

#[test]
fn keys_inside_bounds_without_overlap() {
    for layout in all_layouts() {
        let p = layout.params;
        let ctx = format!(
            "{}×{} {:?} 中文={}",
            p.width, p.height, layout.layer, layout.chinese
        );
        assert_eq!(layout.keys.len(), 37, "{ctx}");
        assert!(layout.candidate_bar.bottom() <= layout.key_area.y, "{ctx}");
        assert!(
            (layout.key_area.bottom() - (p.height - p.bottom_inset)).abs() < 1e-9,
            "{ctx}：键区止于底部安全区之上"
        );
        for (i, a) in layout.keys.iter().enumerate() {
            assert!(
                layout.key_area.contains_rect(&a.rect),
                "{ctx}：{:?} {:?} 超出键区 {:?}",
                a.kind,
                a.rect,
                layout.key_area
            );
            assert!(a.hit.contains_rect(&a.rect), "{ctx}：触摸区域应包含键帽");
            // 触控目标 ≥ 44pt 高，字母键 ≥ 44pt 宽（设计规范 §2.2）。
            assert!(a.rect.h >= 44.0, "{ctx}：键高 {}", a.rect.h);
            if matches!(a.kind, KeyKind::Letter(_) | KeyKind::Char(_)) {
                assert!(a.rect.w >= 44.0, "{ctx}：键宽 {}", a.rect.w);
            }
            for b in &layout.keys[i + 1..] {
                assert!(
                    !a.rect.overlaps(&b.rect),
                    "{ctx}：{:?} 与 {:?} 重叠",
                    a.kind,
                    b.kind
                );
                assert!(!a.hit.overlaps(&b.hit), "{ctx}：触摸区域重叠");
                // 相邻键之间留出间距。
                let gap_x = (b.rect.x - a.rect.right()).max(a.rect.x - b.rect.right());
                let gap_y = (b.rect.y - a.rect.bottom()).max(a.rect.y - b.rect.bottom());
                assert!(
                    gap_x.max(gap_y) >= 5.0,
                    "{ctx}：{:?} 与 {:?} 太近",
                    a.kind,
                    b.kind
                );
            }
        }
        // 触摸区域无缝铺满键区。
        let total: f64 = layout.keys.iter().map(|k| k.hit.area()).sum();
        assert!(
            (total - layout.key_area.area()).abs() < 1e-6,
            "{ctx}：触摸区域没有铺满键区"
        );
    }
}

#[test]
fn every_letter_and_digit_present_once() {
    for layout in all_layouts() {
        let count = |kind: KeyKind| layout.keys.iter().filter(|k| k.kind == kind).count();
        match layout.layer {
            Layer::Letters => {
                for c in 'a'..='z' {
                    assert_eq!(count(KeyKind::Letter(c)), 1, "字母 {c}");
                }
                assert_eq!(count(KeyKind::Shift), 2);
            }
            Layer::Numbers => {
                for d in ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"] {
                    assert_eq!(count(KeyKind::Char(d)), 1, "数字 {d}");
                }
            }
            Layer::Symbols => {}
        }
        for kind in [
            KeyKind::Backspace,
            KeyKind::Return,
            KeyKind::Space,
            KeyKind::Language,
            KeyKind::Hide,
        ] {
            assert_eq!(count(kind), 1, "{kind:?}");
        }
    }
}

#[test]
fn qwerty_rows_and_ipad_proportions() {
    let layout = KeyboardLayout::compute(
        &LayoutParams::for_screen(1280.0, 800.0),
        Layer::Letters,
        true,
    );
    let row = |r: usize| -> String {
        layout
            .keys
            .iter()
            .filter(|k| k.row == r)
            .filter_map(|k| match k.kind {
                KeyKind::Letter(c) => Some(c),
                _ => None,
            })
            .collect()
    };
    assert_eq!(row(0), "qwertyuiop");
    assert_eq!(row(1), "asdfghjkl");
    assert_eq!(row(2), "zxcvbnm");
    let w = |kind| layout.key(layout.find(kind).unwrap()).rect.w;
    let u = w(KeyKind::Letter('q'));
    assert!((95.0..105.0).contains(&u), "横屏字母键约 100pt 宽：{u}");
    assert!(
        (1.4..1.6).contains(&(w(KeyKind::Backspace) / u)),
        "退格约 1.5u"
    );
    assert!((1.9..2.2).contains(&(w(KeyKind::Return) / u)), "换行约 2u");
    assert!(w(KeyKind::Space) / u > 6.0, "空格最宽");
    // 第 2 行缩进半个键距。
    let q = layout.key(layout.find(KeyKind::Letter('q')).unwrap()).rect;
    let a = layout.key(layout.find(KeyKind::Letter('a')).unwrap()).rect;
    let pitch = layout.metrics.key_width + layout.metrics.column_gap;
    assert!((a.x - q.x - pitch / 2.0).abs() <= 1.0);
    // 中文字母层的标点是全角的。
    assert!(layout.find(KeyKind::Char("，")).is_some());
}

#[test]
fn same_grid_across_layers() {
    let params = LayoutParams::for_screen(1194.0, 834.0);
    let letters = KeyboardLayout::compute(&params, Layer::Letters, false);
    for layer in [Layer::Numbers, Layer::Symbols] {
        let other = KeyboardLayout::compute(&params, layer, true);
        for (a, b) in letters.keys.iter().zip(&other.keys) {
            assert_eq!(a.rect, b.rect, "换层时键的位置不变");
        }
    }
}

#[test]
fn hit_test_keys_gaps_and_slop() {
    let layout = KeyboardLayout::compute(
        &LayoutParams::for_screen(1280.0, 800.0),
        Layer::Letters,
        false,
    );
    let id = |c| layout.find(KeyKind::Letter(c)).unwrap();
    // 每个键的中心命中自己。
    for (i, k) in layout.keys.iter().enumerate() {
        let (x, y) = k.rect.center();
        assert_eq!(layout.hit_test(x, y), Some(Hit::Key(i)), "{:?}", k.kind);
    }
    let q = layout.key(id('q')).rect;
    let w = layout.key(id('w')).rect;
    let a = layout.key(id('a')).rect;
    // 横向空隙：归更近的键。
    let gap_mid = (q.right() + w.x) / 2.0;
    assert_eq!(layout.key_at(q.right() + 1.0, q.center().1), Some(id('q')));
    assert_eq!(layout.key_at(w.x - 1.0, q.center().1), Some(id('w')));
    assert_eq!(
        layout.key_at(gap_mid, q.center().1),
        Some(id('w')),
        "中线归右侧"
    );
    // 纵向空隙：归更近的行。
    assert_eq!(layout.key_at(q.center().0, q.bottom() + 1.0), Some(id('q')));
    assert_eq!(layout.key_at(a.center().0, a.y - 1.0), Some(id('a')));
    // 第 2 行左侧的缩进归 “A”；屏幕最左边也归 “A”。
    assert_eq!(layout.key_at(1.0, a.center().1), Some(id('a')));
    // 候选栏。
    assert_eq!(layout.hit_test(640.0, 10.0), Some(Hit::CandidateBar));
    // 键区外（底部安全区、屏幕左右边以外）：离触摸区域不超过触摸容差时归最近的键。
    let space = layout.find(KeyKind::Space).unwrap();
    let sx = layout.key(space).rect.center().0;
    let bottom = layout.key_area.bottom();
    assert_eq!(layout.key_at(sx, bottom + TOUCH_SLOP - 1.0), Some(space));
    assert_eq!(layout.key_at(sx, bottom + TOUCH_SLOP + 1.0), None);
    assert_eq!(
        layout.key_at(-TOUCH_SLOP + 1.0, q.center().1),
        Some(id('q'))
    );
    assert_eq!(layout.key_at(-40.0, q.center().1), None);
    assert_eq!(
        layout.hit_test(1280.0 + 4.0, a.center().1),
        layout.find(KeyKind::Return).map(Hit::Key),
        "屏幕右边以外归行尾的换行键"
    );
}

// ---------------------------------------------------------------- 状态机

fn keyboard(chinese: bool) -> Keyboard {
    let mut kb = Keyboard::new(LayoutParams::for_screen(1280.0, 800.0), chinese, 0.0);
    kb.show(0.0);
    kb
}

fn pos(kb: &Keyboard, kind: KeyKind) -> (f64, f64) {
    key_center(kb.layout(), kind)
}

/// 点按一个键（按下后 50ms 抬起），返回按下与抬起产生的动作。
fn tap(kb: &mut Keyboard, kind: KeyKind, t: f64) -> Vec<KeyAction> {
    let (x, y) = pos(kb, kind);
    let mut out = kb.touch_down(0, x, y, t);
    out.extend(kb.touch_up(0, t + 0.05));
    out
}

#[test]
fn tap_letter_and_highlight_feedback() {
    let mut kb = keyboard(false);
    let h = kb.layout().find(KeyKind::Letter('h')).unwrap();
    let (x, y) = pos(&kb, KeyKind::Letter('h'));
    assert!(kb.touch_down(0, x, y, 1.0).is_empty(), "字符键抬起时才输出");
    assert!(kb.is_pressed(h));
    assert_eq!(kb.highlight(h, 1.0), 1.0, "按下立即高亮，没有延迟");
    assert_eq!(kb.touch_up(0, 1.05), vec![KeyAction::Char('h')]);
    assert!(!kb.is_pressed(h));
    assert!(
        kb.highlight(h, 1.1) > 0.0 && kb.highlight(h, 1.1) < 1.0,
        "抬起后渐隐"
    );
    assert!(kb.is_animating(1.1));
    assert_eq!(kb.highlight(h, 2.0), 0.0);
}

#[test]
fn slide_to_neighbour_and_off_keyboard() {
    let mut kb = keyboard(false);
    let (qx, qy) = pos(&kb, KeyKind::Letter('q'));
    let (wx, wy) = pos(&kb, KeyKind::Letter('w'));
    kb.touch_down(0, qx, qy, 0.0);
    kb.touch_motion(0, wx, wy, 0.05);
    assert_eq!(
        kb.touch_up(0, 0.1),
        vec![KeyAction::Char('w')],
        "以抬起时手指下的键为准"
    );
    // 滑进候选栏：取消。
    kb.touch_down(0, qx, qy, 1.0);
    kb.touch_motion(0, qx, 5.0, 1.05);
    assert!(kb.touch_up(0, 1.1).is_empty());
}

#[test]
fn fast_typing_rollover_keeps_order() {
    let mut kb = keyboard(false);
    let (hx, hy) = pos(&kb, KeyKind::Letter('h'));
    let (ix, iy) = pos(&kb, KeyKind::Letter('i'));
    assert!(kb.touch_down(0, hx, hy, 0.0).is_empty());
    // 第二根手指按下时，第一根手指的键立即输出。
    assert_eq!(kb.touch_down(1, ix, iy, 0.03), vec![KeyAction::Char('h')]);
    assert_eq!(kb.touch_up(1, 0.06), vec![KeyAction::Char('i')]);
    assert!(kb.touch_up(0, 0.08).is_empty(), "已经输出过，不重复");
}

#[test]
fn shift_once_double_tap_and_hold() {
    let mut kb = keyboard(false);
    // 单击：下一个字母大写，之后恢复。
    tap(&mut kb, KeyKind::Shift, 0.0);
    assert_eq!(kb.shift(), ShiftState::Once);
    let a = kb.layout().find(KeyKind::Letter('a')).unwrap();
    assert_eq!(kb.label(a), KeyLabel::Text("A".into()));
    assert_eq!(
        tap(&mut kb, KeyKind::Letter('a'), 1.0),
        vec![KeyAction::Char('A')]
    );
    assert_eq!(kb.shift(), ShiftState::Off);
    assert_eq!(
        tap(&mut kb, KeyKind::Letter('a'), 2.0),
        vec![KeyAction::Char('a')]
    );
    // 双击：大写锁定。
    tap(&mut kb, KeyKind::Shift, 3.0);
    tap(&mut kb, KeyKind::Shift, 3.2);
    assert_eq!(kb.shift(), ShiftState::Locked);
    let shift = kb.layout().find(KeyKind::Shift).unwrap();
    assert_eq!(kb.label(shift), KeyLabel::Symbol(KeySymbol::CapsLock));
    assert_eq!(
        tap(&mut kb, KeyKind::Letter('b'), 4.0),
        vec![KeyAction::Char('B')]
    );
    assert_eq!(
        tap(&mut kb, KeyKind::Letter('c'), 5.0),
        vec![KeyAction::Char('C')]
    );
    tap(&mut kb, KeyKind::Shift, 6.0);
    assert_eq!(kb.shift(), ShiftState::Off);
    // 两次单击间隔太久：不是双击。
    tap(&mut kb, KeyKind::Shift, 7.0);
    tap(&mut kb, KeyKind::Shift, 8.0);
    assert_eq!(kb.shift(), ShiftState::Off);
    // 按住 Shift 连打：都大写，松开后恢复。
    let (sx, sy) = pos(&kb, KeyKind::Shift);
    kb.touch_down(5, sx, sy, 9.0);
    let (dx, dy) = pos(&kb, KeyKind::Letter('d'));
    let (ex, ey) = pos(&kb, KeyKind::Letter('e'));
    kb.touch_down(6, dx, dy, 9.1);
    assert_eq!(kb.touch_up(6, 9.15), vec![KeyAction::Char('D')]);
    kb.touch_down(6, ex, ey, 9.2);
    assert_eq!(kb.touch_up(6, 9.25), vec![KeyAction::Char('E')]);
    kb.touch_up(5, 9.3);
    assert_eq!(kb.shift(), ShiftState::Off);
}

#[test]
fn layer_switching_and_slide_back() {
    let mut kb = keyboard(true);
    let rev = kb.revision();
    tap(&mut kb, KeyKind::Layer(Layer::Numbers), 0.0);
    assert_eq!(kb.layer(), Layer::Numbers);
    assert!(kb.revision() > rev, "换层后需要重画键帽");
    assert_eq!(
        tap(&mut kb, KeyKind::Char("1"), 1.0),
        vec![KeyAction::Text("1".into())]
    );
    assert_eq!(kb.layer(), Layer::Numbers, "点按数字后停留在数字层");
    tap(&mut kb, KeyKind::Layer(Layer::Symbols), 2.0);
    assert_eq!(kb.layer(), Layer::Symbols);
    let back = kb.layout().find(KeyKind::Layer(Layer::Letters)).unwrap();
    assert_eq!(kb.label(back), KeyLabel::Text("拼音".into()));
    tap(&mut kb, KeyKind::Layer(Layer::Letters), 3.0);
    assert_eq!(kb.layer(), Layer::Letters);

    // 按住 123 滑到 “5” 抬起：输出 5 并回到字母层（iPad 的快捷手法）。
    let (lx, ly) = pos(&kb, KeyKind::Layer(Layer::Numbers));
    kb.touch_down(0, lx, ly, 4.0);
    assert_eq!(kb.layer(), Layer::Numbers, "换层键按下即切换");
    let (fx, fy) = pos(&kb, KeyKind::Char("5"));
    kb.touch_motion(0, fx, fy, 4.1);
    assert_eq!(kb.touch_up(0, 4.15), vec![KeyAction::Text("5".into())]);
    assert_eq!(kb.layer(), Layer::Letters);
}

#[test]
fn long_press_alternates() {
    let mut kb = keyboard(false);
    let (ax, ay) = pos(&kb, KeyKind::Letter('e'));
    kb.touch_down(0, ax, ay, 0.0);
    assert_eq!(kb.next_deadline(), Some(0.5));
    assert!(kb.tick(0.3).is_empty());
    assert!(kb.popup().is_none());
    kb.tick(0.5);
    let popup = kb.popup().expect("长按 0.5 秒弹出备选").clone();
    assert_eq!(popup.cells[0].1, "è");
    assert_eq!(popup.selected, 0);
    assert!(
        popup.frame.bottom() <= kb.layout().key(popup.key).rect.y,
        "备选条在键的上方"
    );
    // 横向滑到第二个备选。
    let second = popup.cells[1].0.center();
    kb.touch_motion(0, second.0, ay - 30.0, 0.7);
    assert_eq!(kb.popup().unwrap().selected, 1);
    assert_eq!(kb.touch_up(0, 0.8), vec![KeyAction::Text("é".into())]);
    assert!(kb.popup().is_none());

    // 没有备选的键不弹出，也不需要定时器。
    let (qx, qy) = pos(&kb, KeyKind::Letter('q'));
    kb.touch_down(0, qx, qy, 2.0);
    assert_eq!(kb.next_deadline(), None);
    kb.tick(3.0);
    assert!(kb.popup().is_none());
    assert_eq!(kb.touch_up(0, 3.1), vec![KeyAction::Char('q')]);

    // 中文标点：“，” 长按得到 “！” 等，键帽角上提示第一个备选。
    let mut kb = keyboard(true);
    let comma = kb.layout().find(KeyKind::Char("，")).unwrap();
    assert_eq!(
        kb.label(comma),
        KeyLabel::TextWithHint("，".into(), "！".into())
    );
    let (cx, cy) = pos(&kb, KeyKind::Char("，"));
    kb.touch_down(0, cx, cy, 0.0);
    kb.tick(0.6);
    let popup = kb.popup().unwrap();
    assert!(popup.cells.iter().any(|(_, s)| s == ","), "含英文逗号");
    assert!(
        popup.frame.right() <= 1280.0 && popup.frame.x >= 0.0,
        "备选条不出屏幕左右边"
    );
    assert_eq!(kb.touch_up(0, 0.7), vec![KeyAction::Text("！".into())]);
}

#[test]
fn backspace_deletes_on_press_and_repeats() {
    let mut kb = keyboard(false);
    let (bx, by) = pos(&kb, KeyKind::Backspace);
    assert_eq!(
        kb.touch_down(0, bx, by, 0.0),
        vec![KeyAction::Backspace],
        "按下立即删除"
    );
    assert!(kb.tick(0.4).is_empty());
    assert_eq!(kb.next_deadline(), Some(0.5));
    assert_eq!(kb.tick(0.5), vec![KeyAction::Backspace]);
    assert_eq!(kb.tick(0.75).len(), 2, "之后每 0.1 秒一次");
    // 滑出退格键：停止连删。
    kb.touch_motion(0, bx - 300.0, by, 0.8);
    assert!(kb.tick(2.0).is_empty());
    assert!(kb.touch_up(0, 2.1).is_empty());
}

#[test]
fn function_keys_and_labels() {
    let mut kb = keyboard(true);
    assert_eq!(tap(&mut kb, KeyKind::Space, 0.0), vec![KeyAction::Space]);
    assert_eq!(tap(&mut kb, KeyKind::Return, 1.0), vec![KeyAction::Return]);
    assert_eq!(
        tap(&mut kb, KeyKind::Language, 2.0),
        vec![KeyAction::ToggleLanguage]
    );
    assert_eq!(tap(&mut kb, KeyKind::Hide, 3.0), vec![KeyAction::Hide]);
    let layout = kb.layout();
    let label = |kind| kb.label(layout.find(kind).unwrap());
    assert_eq!(label(KeyKind::Space), KeyLabel::Text("空格".into()));
    assert_eq!(label(KeyKind::Return), KeyLabel::Text("换行".into()));
    assert_eq!(
        label(KeyKind::Language),
        KeyLabel::Language { chinese: true }
    );
    assert_eq!(
        label(KeyKind::Backspace),
        KeyLabel::Symbol(KeySymbol::Backspace)
    );
    assert_eq!(
        label(KeyKind::Layer(Layer::Numbers)),
        KeyLabel::Text("123".into())
    );
    kb.set_chinese(false);
    let layout = kb.layout();
    assert_eq!(
        kb.label(layout.find(KeyKind::Space).unwrap()),
        KeyLabel::Text("space".into())
    );
    assert!(
        layout.find(KeyKind::Char(",")).is_some(),
        "英文模式用半角标点"
    );
}

#[test]
fn present_and_dismiss_with_spring() {
    let mut kb = Keyboard::new(LayoutParams::for_screen(1280.0, 800.0), true, 0.0);
    assert_eq!(kb.presentation(0.0), 0.0);
    kb.show(1.0);
    let mid = kb.presentation(1.1);
    assert!(mid > 0.0 && mid < 1.0);
    assert!(kb.is_animating(1.1));
    assert_eq!(kb.presentation(3.0), 1.0);
    assert!(!kb.is_animating(3.0));
    let (x, y) = pos(&kb, KeyKind::Letter('a'));
    kb.touch_down(0, x, y, 3.0);
    kb.hide(3.1);
    assert!(!kb.is_shown());
    assert!(kb.touch_up(0, 3.2).is_empty(), "收起时取消触摸");
    assert_eq!(kb.presentation(6.0), 0.0);
}

#[test]
fn rotation_recomputes_layout() {
    let mut kb = keyboard(false);
    let before = kb.layout().key(0).rect;
    kb.set_params(LayoutParams::for_screen(800.0, 1280.0), 1.0);
    assert_eq!(kb.layout().params.width, 800.0);
    assert_ne!(kb.layout().key(0).rect, before);
}
