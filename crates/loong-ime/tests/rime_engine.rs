//! 真实 librime 的引擎测试（默认方案 loong_pinyin，开发环境下首次部署时现场编译词典）。
//!
//! 需要系统装有 Rime 数据（Alpine：`apk add librime-dev rime-plum-data`）。数据不存在时，
//! 每个测试打印跳过原因后直接返回（不算失败）；设置 `LOONG_IME_REQUIRE_RIME=1` 时改为失败，
//! CI 用它保证测试真的跑了。
//!
//! librime 是进程全局的：本文件的所有测试共用一个实例和一个用户目录（每次运行先清空，
//! 所以每次都会走一遍“首次部署”），各自使用独立的会话。用户词库也是共用的：选了非首选
//! 的候选会改变之后的排序，所以各测试检查候选顺序时使用互不相同的拼音。

use std::path::PathBuf;
use std::sync::OnceLock;

use loong_ime::keyboard::{KeyKind, Keyboard, LayoutParams};
use loong_ime::{
    ContentPurpose, DEFAULT_SHARED_DATA_DIR, ImeConfig, ImeEngine, ImeEvent, ImeKey, InputMethod,
    KeyOutcome, Rime,
};

fn config() -> ImeConfig {
    ImeConfig::with_user_data_dir(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rime-user"))
}

/// 启动（只启动一次）并新建会话；没有 Rime 数据时返回 `None`。
fn engine() -> Option<ImeEngine> {
    static START: OnceLock<Result<&'static Rime, String>> = OnceLock::new();
    let rime = START.get_or_init(|| {
        let config = config();
        let _ = std::fs::remove_dir_all(&config.user_data_dir);
        let t0 = std::time::Instant::now();
        let r = Rime::start(&config).map_err(|e| e.to_string());
        eprintln!("Rime::start（首次部署）耗时 {:?}", t0.elapsed());
        r
    });
    match rime {
        Ok(rime) => Some(ImeEngine::with_rime(rime).expect("创建会话")),
        Err(e) => {
            let msg = format!(
                "跳过 Rime 测试：{e}（共享数据目录 {DEFAULT_SHARED_DATA_DIR}；\
                 Alpine 下 apk add librime-dev rime-plum-data）"
            );
            if std::env::var_os("LOONG_IME_REQUIRE_RIME").is_some() {
                panic!("{msg}");
            }
            eprintln!("{msg}");
            None
        }
    }
}

fn texts(engine: &ImeEngine) -> Vec<String> {
    engine
        .state()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect()
}

#[test]
fn nihao_candidate_and_commit() {
    let Some(mut ime) = engine() else { return };
    assert_eq!(ime.session().schema_id().as_deref(), Some("loong_pinyin"));
    ime.type_str("nihao");
    let state = ime.state();
    assert!(ime.is_composing());
    assert_eq!(state.raw_input, "nihao");
    let preedit = state.preedit.as_ref().expect("有预编辑");
    assert_eq!(preedit.text, "ni hao");
    assert_eq!(preedit.cursor, preedit.text.len(), "光标以字节计，在末尾");
    assert_eq!(state.commit_preview.as_deref(), Some("你好"));
    let idx = texts(&ime)
        .iter()
        .position(|t| t == "你好")
        .expect("候选里应有“你好”");
    assert!(ime.take_commit().is_none(), "选词前不上屏");
    assert!(ime.select_candidate(idx));
    assert_eq!(ime.take_commit().as_deref(), Some("你好"));
    assert!(!ime.is_composing());
    assert!(ime.state().candidates.is_empty());
}

#[test]
fn space_and_digits_select() {
    let Some(mut ime) = engine() else { return };
    ime.type_str("nihao");
    assert_eq!(ime.state().highlighted, 0);
    assert_eq!(ime.process_key(ImeKey::Space), KeyOutcome::Handled);
    assert_eq!(ime.take_commit().as_deref(), Some("你好"), "空格选高亮候选");

    // 选第 2 个候选会被 Rime 学习（调到前面），用一个别的测试不检查顺序的拼音。
    ime.type_str("pingguo");
    let second = texts(&ime)[1].clone();
    ime.process_key(ImeKey::Char('2'));
    assert_eq!(
        ime.take_commit(),
        Some(second.clone()),
        "数字键选第 2 个候选"
    );
    // 用户词库：刚选过的词排到前面。
    ime.type_str("pingguo");
    assert_eq!(texts(&ime)[0], second, "用户词库应记住刚选的词");
    ime.clear();

    // 不在组字时，数字和空格直接上屏。
    ime.type_str("12 ");
    assert_eq!(ime.take_commit().as_deref(), Some("12 "));
}

#[test]
fn simplified_chinese_output() {
    let Some(mut ime) = engine() else { return };
    for (input, want) in [
        ("zhongguo", "中国"),
        ("guojia", "国家"),
        ("shurufa", "输入法"),
    ] {
        ime.clear();
        ime.type_str(input);
        let cands = texts(&ime);
        assert_eq!(cands[0], want, "{input} → {cands:?}");
    }
    ime.clear();
    ime.type_str("zhongguo");
    assert!(!texts(&ime).iter().any(|t| t == "中國"), "默认输出简体");
    ime.clear();
    ime.type_str("woxiangqubeijing");
    assert_eq!(
        ime.state().commit_preview.as_deref(),
        Some("我想去北京"),
        "整句输入"
    );
    ime.clear();
}

#[test]
fn backspace_enter_and_escape() {
    let Some(mut ime) = engine() else { return };
    assert_eq!(
        ime.process_key(ImeKey::Backspace),
        KeyOutcome::Forward(ImeKey::Backspace),
        "没有组字时退格交给应用"
    );
    assert_eq!(
        ime.process_key(ImeKey::Enter),
        KeyOutcome::Forward(ImeKey::Enter)
    );
    ime.type_str("nihaoo");
    assert_eq!(ime.process_key(ImeKey::Backspace), KeyOutcome::Handled);
    assert_eq!(ime.state().raw_input, "nihao");
    // 回车：原样上屏字母（与 iPad 拼音键盘的“确认”一致）。
    assert_eq!(ime.process_key(ImeKey::Enter), KeyOutcome::Handled);
    assert_eq!(ime.take_commit().as_deref(), Some("nihao"));
    ime.type_str("ceshi");
    ime.process_key(ImeKey::Escape);
    assert!(!ime.is_composing());
    assert!(ime.take_commit().is_none(), "Esc 丢弃组字");
}

#[test]
fn punctuation_commits_composition() {
    let Some(mut ime) = engine() else { return };
    // 屏幕键盘的 “，” 键：直接文字。
    ime.type_str("nihao");
    ime.insert_text("，");
    assert_eq!(ime.take_commit().as_deref(), Some("你好，"));
    // 实体键盘的 ',' '.'：由 Rime 转成全角标点（不再兼作翻页键）。
    ime.type_str("nihao.");
    assert_eq!(ime.take_commit().as_deref(), Some("你好。"));
    ime.type_str("ceshi,");
    assert_eq!(ime.take_commit().as_deref(), Some("测试，"));
    // 拼音分隔符。
    ime.type_str("xi");
    ime.insert_text("'");
    ime.type_str("an");
    assert_eq!(texts(&ime)[0], "西安");
    ime.clear();
}

#[test]
fn ascii_mode_toggle() {
    let Some(mut ime) = engine() else { return };
    assert!(!ime.ascii_mode(), "默认中文");
    ime.type_str("ni");
    ime.set_ascii_mode(true);
    assert!(ime.ascii_mode());
    assert_eq!(
        ime.take_commit().as_deref(),
        Some("ni"),
        "切到英文时字母原样上屏"
    );
    ime.type_str("ok,");
    assert!(!ime.is_composing());
    assert_eq!(ime.take_commit().as_deref(), Some("ok,"));
    assert!(!ime.toggle_ascii_mode());
    ime.type_str("hao");
    assert!(ime.is_composing());
    ime.clear();
}

#[test]
fn candidate_paging() {
    let Some(mut ime) = engine() else { return };
    ime.type_str("shi");
    let page0 = texts(&ime);
    assert_eq!(page0.len(), 9, "默认每页 9 个");
    assert_eq!(ime.state().page_no, 0);
    assert!(!ime.state().is_last_page);
    assert!(ime.next_page());
    assert_eq!(ime.state().page_no, 1);
    let page1 = texts(&ime);
    assert_ne!(page0, page1);
    assert!(ime.prev_page());
    assert_eq!(texts(&ime), page0);
    assert!(!ime.prev_page(), "第一页不能再往前翻");
    // 完整候选列表（展开的候选面板）。
    let all = ime.candidates(0, 30);
    assert_eq!(all.len(), 30);
    assert_eq!(
        all[..9].iter().map(|c| c.text.clone()).collect::<Vec<_>>(),
        page0
    );
    assert_eq!(
        all[9..18]
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>(),
        page1
    );
    let tail = ime.candidates(9, 3);
    assert_eq!(tail[0].text, page1[0]);
    // 选择完整列表中的候选。
    assert!(ime.select_candidate_global(9));
    assert_eq!(ime.take_commit(), Some(page1[0].clone()));
}

#[test]
fn redeploy_refused_while_sessions_alive() {
    let Some(mut ime) = engine() else { return };
    let rime = Rime::running().expect("已启动");
    assert!(rime.live_sessions() >= 1);
    // 部署会让会话持有的词典映射失效（实测段错误），有会话时必须拒绝。
    assert!(matches!(
        rime.redeploy(),
        Err(loong_ime::ImeError::SessionsAlive(_))
    ));
    ime.type_str("nihao");
    assert_eq!(
        ime.state().candidates.first().map(|c| c.text.as_str()),
        Some("你好")
    );
    ime.clear();
    // 同一进程不能换配置。
    let other = ImeConfig::with_user_data_dir("/tmp/loong-ime-other");
    assert!(matches!(
        Rime::start(&other),
        Err(loong_ime::ImeError::AlreadyRunning { .. })
    ));
}

/// 端到端：点按屏幕键盘 → KeyAction → 引擎 → 交给合成器的事件。
#[test]
fn keyboard_drives_engine() {
    let Some(engine) = engine() else { return };
    let keyboard = Keyboard::new(LayoutParams::for_screen(1280.0, 800.0), true, 0.0);
    let mut im = InputMethod::new(keyboard, Some(engine));
    assert!(im.is_chinese());
    im.activate(ContentPurpose::Normal, 0.0);

    let mut t = 1.0;
    let mut tap = |im: &mut InputMethod, kind: KeyKind| -> Vec<ImeEvent> {
        let id = im.keyboard().layout().find(kind).unwrap();
        let (x, y) = im.keyboard().layout().key(id).rect.center();
        t += 0.2;
        let mut ev = im.touch_down(0, x, y, t);
        ev.extend(im.touch_up(0, t + 0.05));
        ev
    };
    for c in "nihao".chars() {
        let ev = tap(&mut im, KeyKind::Letter(c));
        assert_eq!(ev, vec![ImeEvent::CompositionChanged], "{c}");
    }
    assert_eq!(im.preedit().unwrap().text, "ni hao");
    assert_eq!(im.candidates()[0].text, "你好");
    // 键盘上的 “，”：先上屏 “你好” 再输入 “，”。
    let ev = tap(&mut im, KeyKind::Char("，"));
    assert_eq!(
        ev,
        vec![
            ImeEvent::Commit("你好，".into()),
            ImeEvent::CompositionChanged
        ]
    );
    assert!(im.preedit().is_none());
    // 点候选栏选词。
    for c in "shijie".chars() {
        tap(&mut im, KeyKind::Letter(c));
    }
    let idx = im
        .candidates()
        .iter()
        .position(|c| c.text == "世界")
        .expect("世界");
    assert_eq!(
        im.select_candidate(idx),
        vec![
            ImeEvent::Commit("世界".into()),
            ImeEvent::CompositionChanged
        ]
    );
    // 没有组字时退格交给应用。
    assert_eq!(
        tap(&mut im, KeyKind::Backspace),
        vec![ImeEvent::Forward(ImeKey::Backspace)]
    );
    // 中/英切换：英文模式下字母直接上屏。
    tap(&mut im, KeyKind::Language);
    assert!(!im.is_chinese());
    assert_eq!(
        tap(&mut im, KeyKind::Letter('o')),
        vec![ImeEvent::Commit("o".into())]
    );
    tap(&mut im, KeyKind::Language);
    assert!(im.is_chinese());
    // 密码框强制英文，也切不到中文；回到普通输入框恢复用户的选择（中文）。
    im.activate(ContentPurpose::Password, 10.0);
    assert!(!im.is_chinese());
    tap(&mut im, KeyKind::Language);
    assert!(!im.is_chinese(), "密码框不能切到中文");
    assert_eq!(
        tap(&mut im, KeyKind::Letter('p')),
        vec![ImeEvent::Commit("p".into())]
    );
    im.activate(ContentPurpose::Normal, 11.0);
    assert!(im.is_chinese(), "普通输入框用回中文");
}
