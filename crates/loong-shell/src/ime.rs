//! 输入法与应用之间的协议（M4）。状态与画面见 `shell::keyboard`，这里只做 Wayland 一侧：
//!
//! - **text-input-v3，合成器自己当输入法**：注册 `zwp_text_input_manager_v3`，打开
//!   `set_compositor_input_method`（没有 `zwp_input_method_v2` 客户端时，焦点应用的 text-input
//!   照常收到 `enter`、处理 `enable`/`commit`）。应用 enable / disable 经 [`TextInputActivation`]
//!   回调转给 [`ScreenKeyboard`](crate::shell::keyboard::ScreenKeyboard)；
//! - **输出**：[`TextOp`] → `commit_string` / `preedit_string` / `done`；没有组字时的退格、回车等
//!   用合成的 `wl_keyboard` 按键（默认 us 键位，xkb keycode = evdev + 8）；
//! - **键盘焦点跟随前台应用**：回主屏、进多任务时焦点离开应用，键盘收起；回到应用时
//!   Smithay 重新发 `enter`，输入框仍有焦点的话应用会再次 enable，键盘随之升起；
//! - **窗口避让**：键盘升起到位后，把前台窗口的高度减去键盘高度（`xdg_toplevel.configure`），
//!   收起开始时立即恢复。WebKit 只重排一次，并把光标所在的输入框滚到可见处；
//! - **拼音引擎**：librime 在后台线程启动（首次部署可能要几秒），完成后交回主线程。
//!   在此之前键盘照常工作，只能输入英文。环境变量 `LOONG_IME=off` 时不启动（纯英文）。

use smithay::{
    backend::input::{InputTime, KeyState},
    backend::renderer::utils::with_renderer_surface_state,
    input::{
        Seat,
        keyboard::{FilterResult, Keycode},
    },
    reexports::{
        calloop::{LoopHandle, channel},
        wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{
            ContentHint, ContentPurpose as TextInputPurpose,
        },
        wayland_server::DisplayHandle,
    },
    utils::SERIAL_COUNTER,
    wayland::text_input::{TextInputActivation, TextInputManagerState, TextInputSeat},
};

use loong_ime::{ContentPurpose, ImeConfig, ImeEngine, ImeKey};

use crate::{shell::keyboard::TextOp, state::LoongState};

/// 注册 text-input-v3，并让合成器自己充当这个 seat 的输入法。
pub fn init_text_input(dh: &DisplayHandle, seat: &Seat<LoongState>) {
    TextInputManagerState::new::<LoongState>(dh);
    seat.text_input().set_compositor_input_method(true);
}

impl TextInputActivation for LoongState {
    fn activated(&mut self, content_type: Option<(ContentHint, TextInputPurpose)>) {
        let purpose = content_type.map(|(hint, purpose)| {
            // 只标了“敏感数据”的输入框也按密码处理：不切中文，不让用户词库记住。
            if hint.contains(ContentHint::SensitiveData) {
                ContentPurpose::Password
            } else {
                ContentPurpose::from_text_input_v3(u32::from(purpose))
            }
        });
        tracing::debug!("text-input enable：{content_type:?} → {purpose:?}");
        self.shell.keyboard.activate(purpose, self.now);
        self.mark_damaged();
    }

    fn deactivated(&mut self) {
        tracing::debug!("text-input disable");
        self.shell.keyboard.deactivate(self.now);
        self.mark_damaged();
    }
}

/// 输入法不处理的按键 → xkb keycode（evdev 键码 + 8，默认 us 键位）。
fn keycode(key: ImeKey) -> Option<u32> {
    Some(match key {
        ImeKey::Backspace => 22,
        ImeKey::Enter => 36,
        ImeKey::Escape => 9,
        ImeKey::Tab => 23,
        ImeKey::Space => 65,
        ImeKey::Up => 111,
        ImeKey::PageUp => 112,
        ImeKey::Left => 113,
        ImeKey::Right => 114,
        ImeKey::Down => 116,
        ImeKey::PageDown => 117,
        ImeKey::Delete => 119,
        ImeKey::Char(_) => return None,
    })
}

impl LoongState {
    /// 每轮事件循环之后调用：键盘焦点跟随前台应用、把输入法消息发给应用、调整窗口高度。
    pub fn sync_ime(&mut self) {
        self.sync_keyboard_focus();
        self.flush_ime();
        self.sync_window_insets();
    }

    /// 键盘焦点（以及随之而来的 text-input `enter` / `leave`）跟随前台应用。
    fn sync_keyboard_focus(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        // 窗口提交第一帧画面（映射）之后才给焦点，与常见合成器一致。WPE 在创建输入法上下文时
        // 才给 text-input 挂上监听器，映射之前收到的 enter 会被它丢弃，之后就不会再 enable。
        let front = self
            .shell
            .front_window()
            .and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()))
            .filter(|s| {
                with_renderer_surface_state(s, |st| st.buffer().is_some()).unwrap_or(false)
            });
        if keyboard.current_focus() == front {
            return;
        }
        // 焦点离开时 Smithay 只给应用发 leave；应用随后的 disable 因为不再是焦点会被丢弃，
        // 不会回调 deactivated，所以在这里收起键盘。
        tracing::debug!("键盘焦点：{:?} → {front:?}", keyboard.current_focus());
        self.shell.keyboard.deactivate(self.now);
        keyboard.set_focus(self, front, SERIAL_COUNTER.next_serial());
    }

    /// 把输入法产生的消息按顺序发给应用。
    fn flush_ime(&mut self) {
        let ops = self.shell.keyboard.take_ops();
        if ops.is_empty() {
            return;
        }
        tracing::debug!("输入法 → 应用：{ops:?}");
        let text_input = self.seat.text_input().clone();
        for op in ops {
            match op {
                TextOp::Commit(text) => text_input.with_active_text_input(|ti, _| {
                    ti.commit_string(Some(text.clone()));
                }),
                TextOp::Preedit(preedit) => {
                    text_input.with_active_text_input(|ti, _| match &preedit {
                        Some((text, cursor)) => {
                            let c = *cursor as i32;
                            ti.preedit_string(Some(text.clone()), c, c);
                        }
                        None => ti.preedit_string(None, 0, 0),
                    })
                }
                TextOp::Done => text_input.done(false),
                TextOp::Key(key) => self.tap_key(key),
            }
        }
        self.mark_damaged();
    }

    /// 向焦点应用发一次合成按键（按下再抬起）。
    fn tap_key(&mut self, key: ImeKey) {
        let (Some(code), Some(keyboard)) = (keycode(key), self.seat.get_keyboard()) else {
            tracing::debug!("没有对应键码的按键：{key:?}");
            return;
        };
        let time = InputTime::from_micros((self.now.max(0.0) * 1e6) as u64);
        for state in [KeyState::Pressed, KeyState::Released] {
            keyboard.input::<(), _>(
                self,
                Keycode::new(code),
                state,
                SERIAL_COUNTER.next_serial(),
                time,
                |_, _, _| FilterResult::Forward,
            );
        }
    }

    /// 前台窗口的高度 = 屏幕高度 − 键盘让出的高度；其他窗口全屏。只在尺寸变化时发 configure。
    fn sync_window_insets(&mut self) {
        let (w, h) = self.shell.screen_size();
        let front = self.shell.front_window();
        let inset = self.shell.keyboard.window_inset();
        for window in &self.windows {
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            if !toplevel.is_initial_configure_sent() {
                continue;
            }
            let height = if front.as_ref() == Some(window) {
                h - inset
            } else {
                h
            };
            let size = (w.round() as i32, height.round() as i32).into();
            if toplevel.with_pending_state(|s| s.size) != Some(size) {
                toplevel.with_pending_state(|s| s.size = Some(size));
                toplevel.send_pending_configure();
            }
        }
    }
}

/// 启动 librime 并创建输入会话（阻塞：首次部署在镜像里约 0.1 秒，开发环境现场编译词典约 4 秒）。
/// 失败或 `LOONG_IME=off` 时返回 `None`（键盘只能输入英文）。
pub fn start_engine() -> Option<ImeEngine> {
    if std::env::var("LOONG_IME").is_ok_and(|v| v.trim() == "off") {
        tracing::info!("LOONG_IME=off：不启动拼音引擎，键盘只能输入英文");
        return None;
    }
    let config = ImeConfig::default();
    let t0 = std::time::Instant::now();
    match ImeEngine::new(&config) {
        Ok(engine) => {
            tracing::info!(
                "拼音引擎就绪：{:.0} ms（用户目录 {}）",
                t0.elapsed().as_secs_f64() * 1000.0,
                config.user_data_dir.display()
            );
            Some(engine)
        }
        Err(e) => {
            tracing::warn!("拼音引擎启动失败，键盘只能输入英文：{e}");
            None
        }
    }
}

/// 在后台线程启动拼音引擎，就绪后通过 calloop channel 交给主线程的键盘。
pub fn spawn_engine<D: 'static>(
    handle: &LoopHandle<'static, D>,
    get: fn(&mut D) -> &mut LoongState,
) -> anyhow::Result<()> {
    let (tx, rx) = channel::channel::<ImeEngine>();
    handle
        .insert_source(rx, move |event, _, data| {
            if let channel::Event::Msg(engine) = event {
                let state = get(data);
                state.shell.keyboard.set_engine(Box::new(engine));
                state.mark_damaged();
            }
        })
        .map_err(|e| anyhow::anyhow!("注册输入法通道失败: {e}"))?;
    std::thread::Builder::new()
        .name("loong-ime".into())
        .spawn(move || {
            if let Some(engine) = start_engine() {
                let _ = tx.send(engine);
            }
        })?;
    Ok(())
}
