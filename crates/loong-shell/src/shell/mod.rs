//! 系统界面模型：主屏、应用卡片、系统手势。
//!
//! 这里只描述“画什么、在哪里”（[`Scene`]），不涉及 GPU；渲染由 `render` 模块完成。
//! 所有运动都由 `loong-motion` 的弹簧驱动，并且可以被手势随时打断、继承速度。

pub mod apps;
pub mod geometry;
pub mod layout;
pub mod lock;

use loong_motion::{Decay, Spring, SpringAnimation, VelocityTracker, snap};
use smithay::{desktop::Window, reexports::wayland_server::protocol::wl_surface::WlSurface};

use self::{
    apps::AppEntry,
    geometry::{RectF, lerp, smoothstep},
    layout::{BOTTOM_EDGE, Layout},
    lock::LockScreen,
};

/// 应用打开时主屏放大的倍数（营造“进入”的纵深感）。
const HOME_ZOOM_BEHIND_APP: f64 = 1.1;
/// 应用打开时主屏变暗的程度。
const HOME_DIM_BEHIND_APP: f64 = 0.45;
/// 手指移动超过该距离（点）不再算点击。
const TAP_SLOP: f64 = 8.0;
/// 卡片在动画过程中的最小圆角（点）。
const IN_FLIGHT_RADIUS: f64 = 22.0;
/// 回主屏手势：卡片最小缩放。
const SWIPE_MIN_SCALE: f64 = 0.42;

/// 纹理标识（具体位图由 `render::Assets` 生成）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TexKey {
    Wallpaper,
    Dock,
    Icon(usize),
    Label(usize),
    StatusTime,
    StatusIcons,
    /// 没有真实窗口的演示应用的占位画面。
    Placeholder(usize),
    /// 锁屏：大号时间、日期、“上滑解锁”提示。
    LockTime,
    LockDate,
    LockHint,
}

/// 画面中的一个元素。
#[derive(Debug, Clone)]
pub enum DrawItem {
    /// `clip` 为裁剪矩形（圆角作用在裁剪矩形上）；`None` 表示不裁剪、圆角作用在 `rect` 上。
    Texture {
        key: TexKey,
        rect: RectF,
        alpha: f64,
        radius: f64,
        clip: Option<RectF>,
    },
    Solid {
        rect: RectF,
        color: [f32; 4],
        radius: f64,
    },
    Window {
        window: Window,
        rect: RectF,
        alpha: f64,
        radius: f64,
        clip: Option<RectF>,
    },
    /// 按位图原始尺寸绘制（文字等），`scale` 为额外缩放。
    Natural {
        key: TexKey,
        x: f64,
        y: f64,
        align: Align,
        alpha: f64,
        scale: f64,
    },
}

/// 水平对齐方式（相对 `x`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// 一帧的画面，按从下到上的顺序。
pub type Scene = Vec<DrawItem>;

/// 触摸事件交给谁处理。
pub enum TouchRoute {
    /// 系统界面消费了这个触摸。
    Shell,
    /// 转发给应用窗口（窗口左上角在屏幕上的位置）。
    Client(Window, (f64, f64)),
}

/// 前台应用卡片（打开中 / 全屏 / 正在被手势拖动 / 飞回主屏）。
struct Card {
    app: usize,
    window: Option<Window>,
    /// [x, y, w, h, 圆角]
    rect: SpringAnimation<[f64; 5]>,
    /// 应用内容的可见度（窗口首帧到达后从 0 变到 1）。
    content: SpringAnimation<f64>,
    closing: bool,
}

enum Gesture {
    Idle,
    /// 主屏上的触摸（可能是点击图标）。
    Home {
        slot: u32,
        start: (f64, f64),
        t0: f64,
        icon: Option<usize>,
        moved: bool,
    },
    /// 底边上滑：拖动应用卡片。
    AppSwipe {
        slot: u32,
        start: (f64, f64),
        progress: f64,
    },
    /// 转发给应用的触摸。
    Forward {
        slot: u32,
    },
}

/// 系统界面。
pub struct Shell {
    w: f64,
    h: f64,
    screen_radius: f64,
    pub apps: Vec<AppEntry>,
    pub layout: Layout,
    /// 主屏 [缩放, 变暗]。
    home: SpringAnimation<[f64; 2]>,
    card: Option<Card>,
    /// 后台应用的窗口。
    background: Vec<(usize, Window)>,
    gesture: Gesture,
    tracker: VelocityTracker<[f64; 2]>,
    pending_spawn: Vec<usize>,
    last_swipe_pos: Option<(f64, f64)>,
    /// 锁屏。
    lock: LockScreen,
    /// 状态栏与锁屏的时间文字。
    pub clock_text: String,
    /// 锁屏日期文字。
    pub date_text: String,
}

impl Shell {
    pub fn new(w: f64, h: f64, screen_radius: f64) -> Self {
        let apps = apps::builtin_apps();
        let in_dock: Vec<bool> = apps.iter().map(|a| a.in_dock).collect();
        Shell {
            w,
            h,
            screen_radius,
            layout: Layout::compute(w, h, &in_dock),
            apps,
            home: SpringAnimation::at_rest(Spring::SMOOTH, [1.0, 0.0], 0.0),
            card: None,
            background: Vec::new(),
            gesture: Gesture::Idle,
            tracker: VelocityTracker::new(),
            pending_spawn: Vec::new(),
            last_swipe_pos: None,
            lock: LockScreen::new(h, false),
            clock_text: "9:41".into(),
            date_text: "10月30日 星期五".into(),
        }
    }

    /// 屏幕逻辑尺寸。
    pub fn screen_size(&self) -> (f64, f64) {
        (self.w, self.h)
    }

    /// 设置某个应用的启动命令。
    pub fn set_command(&mut self, app_id: &str, command: Option<String>) {
        if let Some(app) = self.apps.iter_mut().find(|a| a.id == app_id) {
            app.command = command;
        }
    }

    /// 取出需要启动进程的应用（由后端执行命令）。
    pub fn take_pending_spawns(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.pending_spawn)
    }

    fn full_rect(&self) -> [f64; 5] {
        [0.0, 0.0, self.w, self.h, self.screen_radius]
    }

    fn icon_target(&self, app: usize) -> [f64; 5] {
        let r = self
            .layout
            .icons
            .get(app)
            .copied()
            .unwrap_or(RectF::centered(self.w / 2.0, self.h / 2.0, 60.0, 60.0));
        [
            r.x,
            r.y,
            r.w,
            r.h,
            r.w * loong_ui::theme::radius::ICON_RATIO as f64,
        ]
    }

    /// 是否有应用在前台（含打开动画中）。
    #[allow(dead_code, reason = "真机后端用它决定输入路由与亮屏策略")]
    pub fn app_in_front(&self) -> bool {
        self.card.as_ref().is_some_and(|c| !c.closing)
    }

    /// 动画是否都已停止（用于空闲时降低帧率）。
    #[allow(dead_code, reason = "真机后端用它在空闲时停止重绘")]
    /// 立即设为锁定 / 未锁定（开机时使用，不播放动画）。
    pub fn set_locked(&mut self, locked: bool) {
        self.lock = LockScreen::new(self.h, locked);
    }

    /// 锁屏（例如按电源键）：锁屏从上方落下。
    #[allow(dead_code, reason = "电源键处理（M3/M4）")]
    pub fn lock_now(&mut self, now: f64) {
        self.lock.lock(now);
    }

    pub fn is_idle(&self, now: f64) -> bool {
        self.lock.is_idle(now)
            && self.home.is_done(now)
            && self
                .card
                .as_ref()
                .is_none_or(|c| c.rect.is_done(now) && c.content.is_done(now))
            && matches!(self.gesture, Gesture::Idle)
    }

    // ───────────────────────── 应用生命周期 ─────────────────────────

    /// 打开应用：卡片从图标位置弹出到全屏。
    pub fn launch(&mut self, app: usize, now: f64) {
        if app >= self.apps.len() {
            return;
        }
        if let Some(card) = &mut self.card {
            if card.app == app {
                // 正在飞回主屏时再次点击同一个应用：直接“接住”卡片，弹回全屏。
                if card.closing {
                    card.closing = false;
                    card.rect.set_spring(Spring::APP_OPEN, now);
                    card.rect
                        .retarget([0.0, 0.0, self.w, self.h, self.screen_radius], now);
                    self.home
                        .retarget([HOME_ZOOM_BEHIND_APP, HOME_DIM_BEHIND_APP], now);
                }
                return;
            }
            if !card.closing {
                return;
            }
        }
        // 另一张卡片还在飞回：立即收进后台。
        self.finish_closing();

        let window = self
            .background
            .iter()
            .position(|(a, _)| *a == app)
            .map(|i| self.background.remove(i).1);
        let has_command = self.apps[app].command.is_some();
        let ready = window.is_some() || !has_command;
        if window.is_none() && has_command {
            self.pending_spawn.push(app);
        }
        self.card = Some(Card {
            app,
            window,
            rect: SpringAnimation::new(
                Spring::APP_OPEN,
                self.icon_target(app),
                self.full_rect(),
                [0.0; 5],
                now,
            )
            .with_rest_thresholds(0.05, 0.5),
            content: SpringAnimation::at_rest(Spring::SMOOTH, if ready { 1.0 } else { 0.0 }, now),
            closing: false,
        });
        self.home.set_spring(Spring::SMOOTH, now);
        self.home
            .retarget([HOME_ZOOM_BEHIND_APP, HOME_DIM_BEHIND_APP], now);
    }

    /// 回到主屏（带初速度，用于手势松手）。
    fn go_home(&mut self, velocity: [f64; 5], now: f64) {
        let Some(card) = &self.card else { return };
        let target = self.icon_target(card.app);
        let current = card.rect.value(now);
        let card = self.card.as_mut().expect("卡片存在");
        card.closing = true;
        card.rect = SpringAnimation::new(Spring::APP_CLOSE, current, target, velocity, now)
            .with_rest_thresholds(0.05, 0.5);
        self.home.set_spring(Spring::SMOOTH, now);
        self.home.retarget([1.0, 0.0], now);
    }

    /// 手势取消：卡片弹回全屏。
    fn restore_app(&mut self, velocity: [f64; 5], now: f64) {
        let full = self.full_rect();
        let Some(card) = &mut self.card else { return };
        let current = card.rect.value(now);
        card.rect = SpringAnimation::new(Spring::SNAPPY, current, full, velocity, now)
            .with_rest_thresholds(0.05, 0.5);
        self.home.set_spring(Spring::SNAPPY, now);
        self.home
            .retarget([HOME_ZOOM_BEHIND_APP, HOME_DIM_BEHIND_APP], now);
    }

    fn finish_closing(&mut self) {
        if let Some(card) = self.card.take_if(|c| c.closing)
            && let Some(window) = card.window
        {
            self.background.push((card.app, window));
        }
    }

    /// 新窗口出现：交给正在打开的应用卡片。
    pub fn on_new_window(&mut self, window: Window, now: f64) {
        let app_id = window.toplevel().and_then(|t| {
            smithay::wayland::compositor::with_states(t.wl_surface(), |states| {
                states
                    .data_map
                    .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok().and_then(|d| d.app_id.clone()))
            })
        });
        // loong-webapp 的 app_id 形如 loong.webapp.<id>（id 中的 '-' 被替换为 '_'）。
        let matched = app_id.as_deref().and_then(|raw| {
            let id = raw
                .strip_prefix("loong.webapp.")
                .unwrap_or(raw)
                .replace('_', "-");
            self.apps.iter().position(|a| a.id == id)
        });
        match &mut self.card {
            Some(card) if card.window.is_none() && !card.closing => {
                card.window = Some(window);
                card.content = SpringAnimation::at_rest(Spring::SMOOTH, 0.0, now);
            }
            _ => {
                // 没有正在打开的应用：按 app_id 匹配，否则归到第一个应用，并直接打开。
                let app = matched.unwrap_or(0);
                self.background.push((app, window));
                self.launch(app, now);
                if let Some(card) = &mut self.card {
                    card.content = SpringAnimation::at_rest(Spring::SMOOTH, 0.0, now);
                }
            }
        }
    }

    /// 窗口第一次（或再次）提交了画面。
    pub fn on_window_ready(&mut self, window: &Window, now: f64) {
        if let Some(card) = &mut self.card
            && card.window.as_ref() == Some(window)
            && card.content.target() < 1.0
        {
            card.content.retarget(1.0, now);
        }
    }

    /// 窗口关闭（应用退出）。
    pub fn on_window_closed(&mut self, surface: &WlSurface, now: f64) {
        self.background
            .retain(|(_, w)| w.toplevel().is_some_and(|t| t.wl_surface() != surface));
        let is_front = self
            .card
            .as_ref()
            .and_then(|c| c.window.as_ref())
            .and_then(|w| w.toplevel())
            .is_some_and(|t| t.wl_surface() == surface);
        if is_front {
            if let Some(card) = &mut self.card {
                card.window = None;
            }
            self.go_home([0.0; 5], now);
        }
    }

    /// 每帧调用：处理动画结束后的状态切换。
    pub fn update(&mut self, now: f64) {
        if self
            .card
            .as_ref()
            .is_some_and(|c| c.closing && c.rect.is_done(now))
        {
            self.finish_closing();
        }
    }

    // ───────────────────────── 触摸手势 ─────────────────────────

    pub fn touch_down(&mut self, slot: u32, x: f64, y: f64, now: f64) -> TouchRoute {
        if self.lock.is_locked() {
            self.lock.touch_down(slot, y, now);
            return TouchRoute::Shell;
        }
        if !matches!(self.gesture, Gesture::Idle) {
            // v1 只处理单指系统手势；多指交给应用。
            return self.forward_route();
        }
        if let Some(card) = &self.card
            && !card.closing
        {
            let settled = card.rect.is_done(now);
            if settled && y >= self.h - BOTTOM_EDGE {
                self.tracker.reset();
                self.tracker.add(now, [x, y]);
                self.last_swipe_pos = Some((x, y));
                self.gesture = Gesture::AppSwipe {
                    slot,
                    start: (x, y),
                    progress: 0.0,
                };
                return TouchRoute::Shell;
            }
            if settled {
                self.gesture = Gesture::Forward { slot };
                return self.forward_route();
            }
            // 打开动画进行中：吞掉触摸。
            return TouchRoute::Shell;
        }
        self.gesture = Gesture::Home {
            slot,
            start: (x, y),
            t0: now,
            icon: self.layout.hit(x, y),
            moved: false,
        };
        TouchRoute::Shell
    }

    fn forward_route(&self) -> TouchRoute {
        match self.card.as_ref().and_then(|c| c.window.clone()) {
            Some(w) => TouchRoute::Client(w, (0.0, 0.0)),
            None => TouchRoute::Shell,
        }
    }

    pub fn touch_motion(&mut self, slot: u32, x: f64, y: f64, now: f64) -> TouchRoute {
        if self.lock.is_dragging() {
            self.lock.touch_motion(slot, y, now);
            return TouchRoute::Shell;
        }
        match &mut self.gesture {
            Gesture::Home {
                slot: s,
                start,
                moved,
                ..
            } if *s == slot => {
                if (x - start.0).hypot(y - start.1) > TAP_SLOP {
                    *moved = true;
                }
                TouchRoute::Shell
            }
            Gesture::AppSwipe { slot: s, start, .. } if *s == slot => {
                let start = *start;
                self.tracker.add(now, [x, y]);
                self.last_swipe_pos = Some((x, y));
                let (rect, p) = self.swipe_rect(start, (x, y));
                if let Gesture::AppSwipe { progress, .. } = &mut self.gesture {
                    *progress = p;
                }
                if let Some(card) = &mut self.card {
                    card.rect.jump_to(rect, [0.0; 5], now);
                }
                let zoom = lerp(HOME_ZOOM_BEHIND_APP, 1.0, p);
                let dim = lerp(HOME_DIM_BEHIND_APP, 0.12, p);
                self.home.jump_to([zoom, dim], [0.0; 2], now);
                TouchRoute::Shell
            }
            Gesture::Forward { slot: s } if *s == slot => self.forward_route(),
            _ => TouchRoute::Shell,
        }
    }

    pub fn touch_up(&mut self, slot: u32, now: f64) -> TouchRoute {
        if self.lock.is_dragging() {
            self.lock.touch_up(slot, now);
            return TouchRoute::Shell;
        }
        let gesture = std::mem::replace(&mut self.gesture, Gesture::Idle);
        match gesture {
            Gesture::Home {
                slot: s,
                icon,
                moved,
                t0,
                ..
            } if s == slot => {
                if let (Some(app), false) = (icon, moved)
                    && now - t0 < 0.8
                {
                    self.launch(app, now);
                }
                TouchRoute::Shell
            }
            Gesture::AppSwipe { slot: s, start, .. } if s == slot => {
                let v = self.tracker.velocity_at(now);
                let last = self.last_touch().unwrap_or(start);
                // 手指速度 → 卡片各分量的速度（对映射函数做数值微分），交给弹簧，保证松手不“顿”。
                let dt = 0.01;
                let (r0, _) = self.swipe_rect(start, last);
                let (r1, _) = self.swipe_rect(start, (last.0 + v[0] * dt, last.1 + v[1] * dt));
                let rect_velocity: [f64; 5] = std::array::from_fn(|i| (r1[i] - r0[i]) / dt);
                // 判定用参考距离 D = 0.25 × 屏幕高度（见设计规范 9.1），与视觉映射分开。
                let reference = self.h * 0.25;
                let decision = (start.1 - last.1).max(0.0) / reference;
                let decision_velocity = -v[1] / reference;
                if snap::should_complete(decision, decision_velocity, Decay::FAST) {
                    self.go_home(rect_velocity, now);
                } else {
                    self.restore_app(rect_velocity, now);
                }
                TouchRoute::Shell
            }
            Gesture::Forward { slot: s } if s == slot => self.forward_route(),
            other => {
                self.gesture = other;
                TouchRoute::Shell
            }
        }
    }

    fn last_touch(&self) -> Option<(f64, f64)> {
        self.last_swipe_pos
    }

    /// 回主屏手势中，手指位置 → 卡片矩形。返回 ([x,y,w,h,r], 进度 0..1)。
    fn swipe_rect(&self, start: (f64, f64), pos: (f64, f64)) -> ([f64; 5], f64) {
        let dy = (start.1 - pos.1).max(0.0);
        let dx = pos.0 - start.0;
        let p = (dy / (self.h * 0.55)).min(1.0);
        // 开始时缩得快、之后变慢，像在“拉起”一张卡片。
        let s = lerp(1.0, SWIPE_MIN_SCALE, 1.0 - (1.0 - p).powi(2));
        let (w, h) = (self.w * s, self.h * s);
        // 手指在卡片上的相对位置保持不变。
        let bottom = pos.1 + (self.h - start.1) * s;
        let cx = self.w / 2.0 + dx * 0.85;
        let radius = lerp(self.screen_radius.max(12.0), 28.0, smoothstep(0.0, 0.25, p));
        ([cx - w / 2.0, bottom - h, w, h, radius], p)
    }

    // ───────────────────────── 画面 ─────────────────────────

    /// 生成当前帧的画面。
    pub fn scene(&self, now: f64) -> Scene {
        let mut scene = Scene::new();
        let [zoom, dim] = self.home.value(now);
        // 锁屏移开的过程中，主屏从稍大、较暗的状态“迎上来”。
        let unlock = self.lock.progress(now);
        let zoom = lerp(HOME_ZOOM_BEHIND_APP, zoom, unlock);
        let dim = lerp(HOME_DIM_BEHIND_APP, dim, unlock);
        let (cx, cy) = (self.w / 2.0, self.h / 2.0);
        let full = RectF::new(0.0, 0.0, self.w, self.h);

        // 主屏
        scene.push(DrawItem::Texture {
            key: TexKey::Wallpaper,
            rect: full.scaled_about(cx, cy, zoom.max(1.0)),
            alpha: 1.0,
            radius: 0.0,
            clip: None,
        });
        scene.push(DrawItem::Texture {
            key: TexKey::Dock,
            rect: self.layout.dock.scaled_about(cx, cy, zoom),
            alpha: 1.0,
            radius: 0.0,
            clip: None,
        });
        let hidden_app = self.card.as_ref().map(|c| c.app);
        for (i, rect) in self.layout.icons.iter().enumerate() {
            if Some(i) == hidden_app {
                continue;
            }
            let r = rect.scaled_about(cx, cy, zoom);
            scene.push(DrawItem::Texture {
                key: TexKey::Icon(i),
                rect: r,
                alpha: 1.0,
                radius: 0.0,
                clip: None,
            });
            if self.layout.labeled[i] {
                scene.push(DrawItem::Natural {
                    key: TexKey::Label(i),
                    x: r.x + r.w / 2.0,
                    y: r.y + r.h + 4.0 * zoom,
                    align: Align::Center,
                    alpha: 1.0,
                    scale: zoom,
                });
            }
        }
        if dim > 0.001 {
            scene.push(DrawItem::Solid {
                rect: full,
                color: [0.0, 0.0, 0.0, dim as f32],
                radius: 0.0,
            });
        }

        // 应用卡片
        if let Some(card) = &self.card {
            let [x, y, w, h, radius] = card.rect.value(now);
            let rect = RectF::new(x, y, w.max(1.0), h.max(1.0));
            let icon = self.layout.icon;
            let q = ((w - icon) / (self.w - icon)).clamp(0.0, 1.0);
            // 飞行中保持圆角，接近全屏时才过渡到屏幕本身的圆角。
            let radius = radius.max(IN_FLIGHT_RADIUS * (1.0 - smoothstep(0.9, 1.0, q)));
            let visible = card.content.value(now).clamp(0.0, 1.0) * smoothstep(0.02, 0.45, q);
            let app = &self.apps[card.app];
            let bg = loong_ui::theme::hex(app.colors.1, 1.0);
            scene.push(DrawItem::Solid {
                rect,
                color: [bg.red(), bg.green(), bg.blue(), 1.0],
                radius,
            });
            // 图标在卡片中央：卡片接近图标大小时图标铺满卡片，卡片变大时图标相对变小并淡出。
            let isz = w.min(h) * lerp(1.0, 0.3, smoothstep(0.0, 0.5, q));
            let (rcx, rcy) = rect.center();
            scene.push(DrawItem::Texture {
                key: TexKey::Icon(card.app),
                rect: RectF::centered(rcx, rcy, isz, isz),
                alpha: 1.0 - visible,
                radius: 0.0,
                clip: None,
            });
            if visible > 0.001 {
                // 内容按屏幕比例铺满卡片（aspect fill），超出部分由着色器按卡片圆角裁掉。
                let content = rect.fill(self.w, self.h);
                match &card.window {
                    Some(window) => scene.push(DrawItem::Window {
                        window: window.clone(),
                        rect: content,
                        alpha: visible,
                        radius,
                        clip: Some(rect),
                    }),
                    None if app.command.is_none() => scene.push(DrawItem::Texture {
                        key: TexKey::Placeholder(card.app),
                        rect: content,
                        alpha: visible,
                        radius,
                        clip: Some(rect),
                    }),
                    None => {}
                }
            }
            // Home 指示条
            if !card.closing {
                let alpha = (1.0 - smoothstep(0.0, 0.15, 1.0 - q)) * 0.85;
                if alpha > 0.01 {
                    let pill = RectF::centered(self.w / 2.0, self.h - 9.0, 140.0, 5.0);
                    scene.push(DrawItem::Solid {
                        rect: pill,
                        color: [1.0, 1.0, 1.0, alpha as f32],
                        radius: 2.5,
                    });
                }
            }
        }

        // 锁屏
        if self.lock.is_visible(now) {
            let off = self.lock.offset(now);
            let sheet = RectF::new(0.0, off, self.w, self.h);
            scene.push(DrawItem::Texture {
                key: TexKey::Wallpaper,
                rect: sheet,
                alpha: 1.0,
                radius: 0.0,
                clip: None,
            });
            scene.push(DrawItem::Solid {
                rect: sheet,
                color: [0.0, 0.0, 0.0, 0.18],
                radius: 0.0,
            });
            let cx = self.w / 2.0;
            let top = self.h * 0.13 + off;
            scene.push(DrawItem::Natural {
                key: TexKey::LockDate,
                x: cx,
                y: top,
                align: Align::Center,
                alpha: 1.0,
                scale: 1.0,
            });
            scene.push(DrawItem::Natural {
                key: TexKey::LockTime,
                x: cx,
                y: top + 28.0,
                align: Align::Center,
                alpha: 1.0,
                scale: 1.0,
            });
            let hint_alpha = 1.0 - smoothstep(0.0, 0.15, unlock);
            scene.push(DrawItem::Natural {
                key: TexKey::LockHint,
                x: cx,
                y: self.h - 46.0 + off,
                align: Align::Center,
                alpha: hint_alpha * 0.9,
                scale: 1.0,
            });
            scene.push(DrawItem::Solid {
                rect: RectF::centered(cx, self.h - 9.0 + off, 140.0, 5.0),
                color: [1.0, 1.0, 1.0, 0.85],
                radius: 2.5,
            });
        }

        // 状态栏（始终在最上层）
        scene.push(DrawItem::Natural {
            key: TexKey::StatusTime,
            x: 20.0,
            y: 2.0,
            align: Align::Left,
            alpha: 1.0,
            scale: 1.0,
        });
        scene.push(DrawItem::Natural {
            key: TexKey::StatusIcons,
            x: self.w - 18.0,
            y: 2.0,
            align: Align::Right,
            alpha: 1.0,
            scale: 1.0,
        });
        scene
    }
}
