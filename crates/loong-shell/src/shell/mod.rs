//! 系统界面模型：主屏、应用卡片、多任务、系统手势。
//!
//! 这里只描述“画什么、在哪里”（[`Scene`]），不涉及 GPU；渲染由 `render` 模块完成。
//! 所有运动都由 `loong-motion` 的弹簧驱动，并且可以被手势随时打断、继承速度。
//!
//! 运行中的应用统一放在任务列表里（按最近使用排序，最后一个最近）。每个任务有一个
//! 矩形弹簧 [x, y, w, h, 圆角]：前台应用、回主屏、多任务卡片都用它，所以从一种状态
//! 切到另一种状态时，位置和速度都是连续的。

pub mod apps;
pub mod geometry;
pub mod gesture;
pub mod layout;
pub mod lock;
pub mod switcher;

use loong_motion::{Animatable, Decay, Spring, SpringAnimation, VelocityTracker, snap};
use smithay::{desktop::Window, reexports::wayland_server::protocol::wl_surface::WlSurface};

use self::{
    apps::AppEntry,
    geometry::{RectF, lerp, smoothstep},
    gesture::{Axis, lock_axis, rubber_clamp, rubber_clamp_inverse},
    layout::{BOTTOM_EDGE, Layout, PAGE_DOT, PAGE_DOT_GAP},
    lock::LockScreen,
    switcher::{PauseDetector, SwitcherLayout},
};

/// 应用打开时主屏放大的倍数（营造“进入”的纵深感）。
const HOME_ZOOM_BEHIND_APP: f64 = 1.1;
/// 应用打开时主屏变暗的程度。
const HOME_DIM_BEHIND_APP: f64 = 0.45;
/// 手指移动超过该距离（点）不再算点击。
const TAP_SLOP: f64 = 8.0;
/// 按住超过该时间（秒）不再算点击。
const TAP_TIMEOUT: f64 = 0.8;
/// 卡片在动画过程中的最小圆角（点）。
const IN_FLIGHT_RADIUS: f64 = 22.0;
/// 回主屏手势：卡片最小缩放。
const SWIPE_MIN_SCALE: f64 = 0.42;
/// 进入多任务时，其他卡片从左侧滑入的距离（屏幕宽度的比例）。
const SWITCHER_SLIDE_IN: f64 = 0.6;
/// 离开多任务时，其余卡片淡出并缩小到的比例。
const SWITCHER_EXIT_SCALE: f64 = 0.92;

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
    /// 多任务背景：模糊后的壁纸 + `thin` 材质叠加色。
    SwitcherBackdrop,
    /// 多任务卡片上方的标签：小图标 + 应用名称。
    CardLabel(usize),
    /// 多任务卡片阴影（`shadowHigh`），按标准卡片尺寸生成，绘制时拉伸。
    CardShadow,
    /// 主屏页面指示点（白色圆点）。
    PageDot,
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

const ZERO5: [f64; 5] = [0.0; 5];

fn add5(a: [f64; 5], b: [f64; 5]) -> [f64; 5] {
    std::array::from_fn(|i| a[i] + b[i])
}

fn sub5(a: [f64; 5], b: [f64; 5]) -> [f64; 5] {
    std::array::from_fn(|i| a[i] - b[i])
}

/// 卡片矩形动画（位置类属性，静止阈值按点计）。
fn rect_anim(
    spring: Spring,
    from: [f64; 5],
    to: [f64; 5],
    velocity: [f64; 5],
    now: f64,
) -> SpringAnimation<[f64; 5]> {
    SpringAnimation::new(spring, from, to, velocity, now).with_rest_thresholds(0.05, 0.5)
}

/// 位置类标量动画。
fn offset_anim(spring: Spring, from: f64, to: f64, velocity: f64, now: f64) -> SpringAnimation {
    SpringAnimation::new(spring, from, to, velocity, now).with_rest_thresholds(0.05, 0.5)
}

/// 让动画停在 `value`（目标也设为 `value`、速度为 0）。拖动中用它让元素被手指“按住”：
/// 只用 `jump_to` 的话目标不变，两次触摸事件之间（例如手指停住不动时）元素会被原来的
/// 目标拉走。
pub(crate) fn pin<V: Animatable>(anim: &mut SpringAnimation<V>, value: V, now: f64) {
    anim.retarget(value, now);
    anim.jump_to(value, V::from_fn(|_| 0.0), now);
}

/// 手指速度 → 映射后的速度：对映射函数做数值微分，交给弹簧，保证松手不“顿”。
fn map_velocity<const N: usize>(
    f: impl Fn((f64, f64)) -> [f64; N],
    pos: (f64, f64),
    v: [f64; 2],
) -> [f64; N] {
    let dt = 0.01;
    let a = f(pos);
    let b = f((pos.0 + v[0] * dt, pos.1 + v[1] * dt));
    std::array::from_fn(|i| (b[i] - a[i]) / dt)
}

/// 一个正在运行的应用。
struct Task {
    app: usize,
    window: Option<Window>,
    /// [x, y, w, h, 圆角]。多任务界面中是相对卡片槽位的偏移，其他时候是屏幕坐标。
    rect: SpringAnimation<[f64; 5]>,
    /// 多任务中上下拖动卡片（上滑关闭）的位移，向上为负。
    lift: SpringAnimation<f64>,
    /// 应用内容的可见度（窗口首帧到达后从 0 变到 1）。
    content: SpringAnimation<f64>,
    /// 正在飞回主屏图标。
    homing: bool,
    /// 应用已经退出（窗口关闭），飞回主屏后移除。
    exited: bool,
}

impl Task {
    fn new(app: usize, window: Option<Window>, now: f64) -> Self {
        Task {
            app,
            window,
            rect: rect_anim(Spring::SMOOTH, ZERO5, ZERO5, ZERO5, now),
            lift: offset_anim(Spring::SNAPPY, 0.0, 0.0, 0.0, now),
            content: SpringAnimation::at_rest(Spring::SMOOTH, 1.0, now),
            homing: false,
            exited: false,
        }
    }

    fn is_settled(&self, now: f64) -> bool {
        self.rect.is_done(now) && self.lift.is_done(now) && self.content.is_done(now)
    }

    /// 请求应用关闭窗口（xdg_toplevel.close）。没有窗口的演示应用直接消失。
    fn request_close(&self) {
        if let Some(toplevel) = self.window.as_ref().and_then(|w| w.toplevel()) {
            toplevel.send_close();
        }
    }
}

/// 界面所处的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 主屏（可能有一张卡片正飞回图标）。
    Home,
    /// 前台应用：任务列表的最后一个（打开中 / 全屏 / 正在被手势拖动）。
    App,
    /// 多任务界面。
    Switcher,
}

#[derive(Debug, Clone, Copy)]
enum Gesture {
    Idle,
    /// 主屏上的触摸：点击图标，或横滑翻页。
    Home {
        slot: u32,
        start: (f64, f64),
        t0: f64,
        icon: Option<usize>,
        moved: bool,
        /// 锁定的拖动方向（横向即翻页）。
        axis: Option<Axis>,
        /// 锁定方向时的手指位置。
        origin: (f64, f64),
        /// 锁定方向时的滚动位置（已去掉橡皮筋）。
        base: f64,
        /// 手势开始时所在页（一次手势最多翻一页）。
        start_page: usize,
    },
    /// 底边上滑：拖动前台应用卡片。停顿则进入多任务。
    AppSwipe {
        slot: u32,
        start: (f64, f64),
        pause: PauseDetector,
    },
    /// 上滑停顿后已进入多任务，手指仍按着原来的前台卡片。
    SwitcherEnter {
        slot: u32,
        start: (f64, f64),
        app: usize,
    },
    /// 转发给应用的触摸。
    Forward {
        slot: u32,
    },
    /// 多任务界面中的触摸：点按卡片 / 点按空白处 / 横向滚动 / 上滑关闭卡片。
    Switcher {
        slot: u32,
        start: (f64, f64),
        t0: f64,
        /// 按到的卡片（应用下标）。
        app: Option<usize>,
        /// 锁定的拖动方向（超过起手阈值后确定）。
        axis: Option<Axis>,
        /// 锁定方向时的手指位置。
        origin: (f64, f64),
        /// 锁定方向时的起点：横向为滚动位置，纵向为卡片位移（都已去掉橡皮筋）。
        base: f64,
    },
    /// 多任务界面中从底边上滑：回主屏。
    SwitcherEdge {
        slot: u32,
        start: (f64, f64),
    },
}

/// 系统界面。
pub struct Shell {
    w: f64,
    h: f64,
    screen_radius: f64,
    pub apps: Vec<AppEntry>,
    pub layout: Layout,
    /// 多任务布局。
    pub switcher: SwitcherLayout,
    /// 主屏 [缩放, 变暗]。
    home: SpringAnimation<[f64; 2]>,
    /// 主屏横向滚动位置（第 p 页为 p × 屏幕宽度）。
    home_scroll: SpringAnimation<f64>,
    mode: Mode,
    /// 运行中的应用，按最近使用排序（最后一个最近）。
    tasks: Vec<Task>,
    /// 在多任务中被上滑关闭、正在飞出屏幕的卡片（屏幕坐标）。
    dismissed: Vec<Task>,
    /// 多任务界面的显现程度 0..1（背景、标签、阴影随它淡入淡出）。
    switcher_progress: SpringAnimation<f64>,
    /// 多任务背景（模糊壁纸）的不透明度。从多任务打开应用时背景保持到应用铺满屏幕，
    /// 避免过渡中露出主屏图标。
    backdrop: SpringAnimation<f64>,
    /// 多任务横向滚动位置（见 [`SwitcherLayout`]）。
    scroll: SpringAnimation<f64>,
    gesture: Gesture,
    tracker: VelocityTracker<[f64; 2]>,
    /// 当前手势中最后一次的手指位置。
    last_pos: Option<(f64, f64)>,
    pending_spawn: Vec<usize>,
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
            switcher: SwitcherLayout::new(w, h),
            apps,
            home: SpringAnimation::at_rest(Spring::SMOOTH, [1.0, 0.0], 0.0),
            home_scroll: offset_anim(Spring::SNAPPY, 0.0, 0.0, 0.0, 0.0),
            mode: Mode::Home,
            tasks: Vec::new(),
            dismissed: Vec::new(),
            switcher_progress: SpringAnimation::at_rest(Spring::SMOOTH, 0.0, 0.0),
            backdrop: SpringAnimation::at_rest(Spring::SMOOTH, 0.0, 0.0),
            scroll: offset_anim(Spring::SNAPPY, 0.0, 0.0, 0.0, 0.0),
            gesture: Gesture::Idle,
            tracker: VelocityTracker::new(),
            last_pos: None,
            pending_spawn: Vec::new(),
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

    /// 应用图标在主屏（停稳后）上的位置：打开时从这里弹出，回主屏时飞回这里。
    /// 图标不在当前页时，缩小到屏幕中央。
    fn icon_target(&self, app: usize) -> [f64; 5] {
        let scroll = self.home_scroll.target();
        let on_screen = |r: &RectF| r.x + r.w > 0.0 && r.x < self.w;
        let r = (app < self.layout.icons.len())
            .then(|| self.layout.icon_at(app, scroll))
            .filter(on_screen)
            .unwrap_or(RectF::centered(
                self.w / 2.0,
                self.h / 2.0,
                self.layout.icon,
                self.layout.icon,
            ));
        [
            r.x,
            r.y,
            r.w,
            r.h,
            r.w * loong_ui::theme::radius::ICON_RATIO as f64,
        ]
    }

    fn task_index(&self, app: usize) -> Option<usize> {
        self.tasks.iter().position(|t| t.app == app)
    }

    /// 是否有应用在前台（含打开动画中）。
    #[allow(dead_code, reason = "真机后端用它决定输入路由与亮屏策略")]
    pub fn app_in_front(&self) -> bool {
        self.mode == Mode::App
    }

    /// 是否在多任务界面。
    #[allow(dead_code, reason = "测试与后续的状态栏样式使用")]
    pub fn in_switcher(&self) -> bool {
        self.mode == Mode::Switcher
    }

    /// 运行中的应用（按最近使用排序，最后一个最近）。
    #[allow(dead_code, reason = "测试与后续的后台冻结策略使用")]
    pub fn running_apps(&self) -> Vec<usize> {
        self.tasks.iter().map(|t| t.app).collect()
    }

    /// 立即设为锁定 / 未锁定（开机时使用，不播放动画）。
    pub fn set_locked(&mut self, locked: bool) {
        self.lock = LockScreen::new(self.h, locked);
    }

    /// 锁屏（例如按电源键）：锁屏从上方落下。
    #[allow(dead_code, reason = "电源键处理（M3/M4）")]
    pub fn lock_now(&mut self, now: f64) {
        self.lock.lock(now);
    }

    /// 动画是否都已停止、也没有进行中的手势（用于空闲时停止重绘）。
    pub fn is_idle(&self, now: f64) -> bool {
        self.lock.is_idle(now)
            && self.home.is_done(now)
            && self.home_scroll.is_done(now)
            && self.switcher_progress.is_done(now)
            && self.backdrop.is_done(now)
            && self.scroll.is_done(now)
            && self.dismissed.is_empty()
            && self.tasks.iter().all(|t| t.is_settled(now))
            && matches!(self.gesture, Gesture::Idle)
    }

    // ───────────────────────── 应用生命周期 ─────────────────────────

    /// 打开应用：卡片从图标位置弹出到全屏。在多任务中则从该应用的卡片放大。
    pub fn launch(&mut self, app: usize, now: f64) {
        if app >= self.apps.len() {
            return;
        }
        match self.mode {
            Mode::App => return,
            Mode::Switcher => {
                if let Some(i) = self.task_index(app) {
                    self.open_from_switcher(i, now);
                    return;
                }
                self.go_home_from_switcher(now);
            }
            Mode::Home => {}
        }
        let full = self.full_rect();
        // 正在飞回主屏时再次点击同一个应用：直接“接住”卡片，弹回全屏。
        if let Some(task) = self.tasks.last_mut()
            && task.app == app
            && task.homing
            && !task.exited
        {
            task.homing = false;
            task.rect.set_spring(Spring::APP_OPEN, now);
            task.rect.retarget(full, now);
            self.show_app(now);
            return;
        }
        // 另一张卡片还在飞回：立即收进后台。
        for task in &mut self.tasks {
            task.homing = false;
        }
        self.tasks.retain(|t| !t.exited);

        let has_command = self.apps[app].command.is_some();
        let mut task = match self.task_index(app) {
            Some(i) => self.tasks.remove(i),
            None => {
                if has_command {
                    self.pending_spawn.push(app);
                }
                Task::new(app, None, now)
            }
        };
        let ready = task.window.is_some() || !has_command;
        task.rect = rect_anim(Spring::APP_OPEN, self.icon_target(app), full, ZERO5, now);
        task.lift = offset_anim(Spring::SNAPPY, 0.0, 0.0, 0.0, now);
        task.content = SpringAnimation::at_rest(Spring::SMOOTH, if ready { 1.0 } else { 0.0 }, now);
        self.tasks.push(task);
        self.show_app(now);
    }

    /// 切到“前台应用”状态：主屏退到后方。
    fn show_app(&mut self, now: f64) {
        self.mode = Mode::App;
        self.home.set_spring(Spring::SMOOTH, now);
        self.home
            .retarget([HOME_ZOOM_BEHIND_APP, HOME_DIM_BEHIND_APP], now);
    }

    /// 回到主屏（带初速度，用于手势松手）。
    fn go_home(&mut self, velocity: [f64; 5], now: f64) {
        if self.mode != Mode::App {
            return;
        }
        let Some(app) = self.tasks.last().map(|t| t.app) else {
            return;
        };
        let target = self.icon_target(app);
        let task = self.tasks.last_mut().expect("前台应用存在");
        let current = task.rect.value(now);
        task.homing = true;
        task.rect = rect_anim(Spring::APP_CLOSE, current, target, velocity, now);
        self.mode = Mode::Home;
        self.home.set_spring(Spring::SMOOTH, now);
        self.home.retarget([1.0, 0.0], now);
        self.backdrop.set_spring(Spring::SMOOTH, now);
        self.backdrop.retarget(0.0, now);
    }

    /// 手势取消：卡片弹回全屏。
    fn restore_app(&mut self, velocity: [f64; 5], now: f64) {
        let full = self.full_rect();
        let Some(task) = self.tasks.last_mut() else {
            return;
        };
        let current = task.rect.value(now);
        task.rect = rect_anim(Spring::SNAPPY, current, full, velocity, now);
        self.home.set_spring(Spring::SNAPPY, now);
        self.home
            .retarget([HOME_ZOOM_BEHIND_APP, HOME_DIM_BEHIND_APP], now);
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
        if self.mode == Mode::App
            && let Some(task) = self.tasks.last_mut()
            && task.window.is_none()
        {
            task.window = Some(window);
            task.content = SpringAnimation::at_rest(Spring::SMOOTH, 0.0, now);
            return;
        }
        // 没有正在打开的应用：按 app_id 匹配，否则归到第一个应用，并直接打开。
        let app = matched.unwrap_or(0);
        match self.task_index(app) {
            Some(i) if self.tasks[i].window.is_none() => self.tasks[i].window = Some(window),
            // v1 每个应用只有一个窗口：多出来的窗口请应用关闭。
            Some(_) => {
                if let Some(toplevel) = window.toplevel() {
                    toplevel.send_close();
                }
                return;
            }
            None => self.tasks.push(Task::new(app, Some(window), now)),
        }
        self.launch(app, now);
        if self.mode == Mode::App
            && let Some(task) = self.tasks.last_mut()
            && task.app == app
        {
            task.content = SpringAnimation::at_rest(Spring::SMOOTH, 0.0, now);
        }
    }

    /// 窗口第一次（或再次）提交了画面。
    pub fn on_window_ready(&mut self, window: &Window, now: f64) {
        for task in &mut self.tasks {
            if task.window.as_ref() == Some(window) && task.content.target() < 1.0 {
                task.content.retarget(1.0, now);
            }
        }
    }

    /// 窗口关闭（应用退出）。
    pub fn on_window_closed(&mut self, surface: &WlSurface, now: f64) {
        let is_closed = |w: &Option<Window>| {
            w.as_ref()
                .and_then(|w| w.toplevel())
                .is_some_and(|t| t.wl_surface() == surface)
        };
        for task in &mut self.dismissed {
            if is_closed(&task.window) {
                task.window = None;
            }
        }
        let Some(i) = self.tasks.iter().position(|t| is_closed(&t.window)) else {
            return;
        };
        let front = self.mode == Mode::App && i + 1 == self.tasks.len();
        if front || self.tasks[i].homing {
            // 前台（或正在飞回）的应用退出：卡片飞回图标后移除。
            let task = &mut self.tasks[i];
            task.window = None;
            task.exited = true;
            self.go_home(ZERO5, now);
        } else if self.mode == Mode::Switcher {
            self.tasks.remove(i);
            self.reflow_after_removal(i, now);
            if self.tasks.is_empty() {
                self.go_home_from_switcher(now);
            }
        } else {
            self.tasks.remove(i);
        }
    }

    /// 每帧调用：处理动画结束后的状态切换。
    pub fn update(&mut self, now: f64) {
        for task in &mut self.tasks {
            if task.homing && task.rect.is_done(now) {
                task.homing = false;
            }
        }
        // 已退出的应用飞回主屏后移除。
        self.tasks.retain(|t| !t.exited || t.homing);
        // 从多任务打开的应用已经铺满屏幕：背后的多任务背景不再需要。
        if self.mode == Mode::App
            && self.backdrop.target() > 0.0
            && self.tasks.last().is_some_and(|t| t.rect.is_done(now))
        {
            self.backdrop = SpringAnimation::at_rest(Spring::SMOOTH, 0.0, now);
        }
        // 飞出屏幕的卡片：完全离开屏幕后移除。
        self.dismissed.retain(|t| {
            let r = t.rect.value(now);
            r[1] + r[3] > 0.0 && !t.rect.is_done(now)
        });
        // 手指停住时触摸屏不再上报事件，停顿检测需要每帧进行。
        if matches!(self.gesture, Gesture::AppSwipe { .. }) {
            self.check_pause(now);
        }
    }

    // ───────────────────────── 多任务 ─────────────────────────

    /// 底边上滑中检测停顿；满足条件时进入多任务，手指继续拖着原来的前台卡片。
    fn check_pause(&mut self, now: f64) {
        let v = self.tracker.velocity_at(now);
        let reference = self.h * 0.25;
        let Gesture::AppSwipe {
            slot,
            start,
            mut pause,
        } = self.gesture
        else {
            return;
        };
        let last = self.last_pos.unwrap_or(start);
        let entered = pause.update(now, start.1 - last.1, v[0].hypot(v[1]), 0.5 * reference);
        self.gesture = Gesture::AppSwipe { slot, start, pause };
        if entered && let Some(app) = self.tasks.last().map(|t| t.app) {
            self.enter_switcher(now);
            self.gesture = Gesture::SwitcherEnter { slot, start, app };
        }
    }

    /// 从前台应用进入多任务：前台卡片保持当前位置（之后吸附到最右侧的槽位），
    /// 其他应用的卡片从左侧滑入，背景模糊淡入。
    fn enter_switcher(&mut self, now: f64) {
        if self.mode != Mode::App {
            return;
        }
        self.tasks.retain(|t| !t.exited);
        let n = self.tasks.len();
        if n == 0 {
            return;
        }
        let sw = self.switcher;
        let scroll = sw.initial_scroll(n);
        self.scroll = offset_anim(Spring::SNAPPY, scroll, scroll, 0.0, now);
        let slide_in = [-self.w * SWITCHER_SLIDE_IN, 0.0, 0.0, 0.0, 0.0];
        for (i, task) in self.tasks.iter_mut().enumerate() {
            task.rect = if i + 1 == n {
                // 手指仍按着这张卡片：停在手指下，松手后再吸附到槽位。
                let held = sub5(task.rect.value(now), sw.slot(i, scroll));
                rect_anim(Spring::SMOOTH, held, held, ZERO5, now)
            } else {
                rect_anim(Spring::SMOOTH, slide_in, ZERO5, ZERO5, now)
            };
            task.lift = offset_anim(Spring::SNAPPY, 0.0, 0.0, 0.0, now);
            task.homing = false;
        }
        self.mode = Mode::Switcher;
        self.switcher_progress.set_spring(Spring::SMOOTH, now);
        self.switcher_progress.retarget(1.0, now);
        self.backdrop.set_spring(Spring::SMOOTH, now);
        self.backdrop.retarget(1.0, now);
        self.home.set_spring(Spring::SMOOTH, now);
        self.home
            .retarget([HOME_ZOOM_BEHIND_APP, HOME_DIM_BEHIND_APP], now);
    }

    /// 多任务中第 `i` 张卡片在屏幕上的（矩形, 速度）。
    fn switcher_card_state(&self, i: usize, now: f64) -> ([f64; 5], [f64; 5]) {
        let task = &self.tasks[i];
        let mut value = add5(
            self.switcher.slot(i, self.scroll.value(now)),
            task.rect.value(now),
        );
        value[1] += task.lift.value(now);
        let mut velocity = task.rect.velocity(now);
        velocity[0] -= self.scroll.velocity(now);
        velocity[1] += task.lift.velocity(now);
        (value, velocity)
    }

    /// 多任务中被点中的卡片（应用下标）。标签区域也算作卡片。
    fn switcher_hit(&self, x: f64, y: f64, now: f64) -> Option<usize> {
        (0..self.tasks.len()).rev().find_map(|i| {
            let ([rx, ry, rw, rh, _], _) = self.switcher_card_state(i, now);
            let top = switcher::label_top(ry, 1.0);
            (x >= rx && x < rx + rw && y >= top && y < ry + rh).then(|| self.tasks[i].app)
        })
    }

    /// 离开多任务：卡片换回屏幕坐标（位置、速度连续，继续驶向原本的目标位置），背景淡出。
    fn leave_switcher(&mut self, now: f64) {
        if self.mode != Mode::Switcher {
            return;
        }
        let sw = self.switcher;
        let scroll = self.scroll.value(now);
        let scroll_velocity = self.scroll.velocity(now);
        let scroll_end = self.scroll.target();
        for (i, task) in self.tasks.iter_mut().enumerate() {
            let mut value = add5(sw.slot(i, scroll), task.rect.value(now));
            value[1] += task.lift.value(now);
            let mut velocity = task.rect.velocity(now);
            velocity[0] -= scroll_velocity;
            velocity[1] += task.lift.velocity(now);
            let end = add5(sw.slot(i, scroll_end), task.rect.target());
            task.rect = rect_anim(Spring::SMOOTH, value, end, velocity, now);
            task.lift = offset_anim(Spring::SNAPPY, 0.0, 0.0, 0.0, now);
        }
        self.scroll = offset_anim(Spring::SNAPPY, scroll_end, scroll_end, 0.0, now);
        self.mode = Mode::Home;
        self.switcher_progress.set_spring(Spring::SMOOTH, now);
        self.switcher_progress.retarget(0.0, now);
    }

    /// 多任务 → 主屏：卡片淡出，主屏从后方迎上来。
    fn go_home_from_switcher(&mut self, now: f64) {
        self.leave_switcher(now);
        self.home.set_spring(Spring::SMOOTH, now);
        self.home.retarget([1.0, 0.0], now);
        self.backdrop.set_spring(Spring::SMOOTH, now);
        self.backdrop.retarget(0.0, now);
    }

    /// 多任务 → 应用：被点中的卡片从当前位置（带着当前速度）放大到全屏。
    fn open_from_switcher(&mut self, i: usize, now: f64) {
        self.leave_switcher(now);
        let full = self.full_rect();
        let mut task = self.tasks.remove(i);
        let (value, velocity) = (task.rect.value(now), task.rect.velocity(now));
        task.rect = rect_anim(Spring::APP_OPEN, value, full, velocity, now);
        self.tasks.push(task);
        self.show_app(now);
    }

    /// 上滑关闭第 `i` 张卡片：卡片带着松手速度飞出屏幕，其余卡片补位。
    fn dismiss(&mut self, i: usize, now: f64) {
        let (value, velocity) = self.switcher_card_state(i, now);
        let mut task = self.tasks.remove(i);
        let above = value[3] + switcher::SHADOW_SPREAD + value[1].max(0.0);
        let target = [value[0], value[1] - above, value[2], value[3], value[4]];
        task.rect = rect_anim(Spring::SMOOTH, value, target, velocity, now);
        task.lift = offset_anim(Spring::SNAPPY, 0.0, 0.0, 0.0, now);
        task.request_close();
        self.dismissed.push(task);
        self.reflow_after_removal(i, now);
        if self.tasks.is_empty() {
            self.go_home_from_switcher(now);
        }
    }

    /// 第 `removed` 张卡片被移除后补位：右侧的卡片槽位左移一格，先把偏移加回来保持
    /// 画面不动，再由弹簧收回；滚动位置超出新范围时用同样的弹簧滚回范围内。
    fn reflow_after_removal(&mut self, removed: usize, now: f64) {
        let stride = self.switcher.stride;
        for task in self.tasks.iter_mut().skip(removed) {
            let mut value = task.rect.value(now);
            value[0] += stride;
            let velocity = task.rect.velocity(now);
            task.rect = rect_anim(Spring::SMOOTH, value, ZERO5, velocity, now);
        }
        let (lo, hi) = self.switcher.scroll_range(self.tasks.len());
        let target = self.scroll.target();
        let clamped = target.clamp(lo, hi);
        if clamped != target {
            self.scroll.set_spring(Spring::SMOOTH, now);
            self.scroll.retarget(clamped, now);
        }
    }

    // ───────────────────────── 触摸手势 ─────────────────────────

    fn track(&mut self, now: f64, x: f64, y: f64) {
        self.tracker.add(now, [x, y]);
        self.last_pos = Some((x, y));
    }

    fn begin_tracking(&mut self, now: f64, x: f64, y: f64) {
        self.tracker.reset();
        self.track(now, x, y);
    }

    pub fn touch_down(&mut self, slot: u32, x: f64, y: f64, now: f64) -> TouchRoute {
        if self.lock.is_locked() {
            self.lock.touch_down(slot, y, now);
            return TouchRoute::Shell;
        }
        if !matches!(self.gesture, Gesture::Idle) {
            // v1 只处理单指系统手势；多指交给应用。
            return self.forward_route();
        }
        match self.mode {
            Mode::App => {
                let settled = self.tasks.last().is_some_and(|t| t.rect.is_done(now));
                if settled && y >= self.h - BOTTOM_EDGE {
                    self.begin_tracking(now, x, y);
                    self.gesture = Gesture::AppSwipe {
                        slot,
                        start: (x, y),
                        pause: PauseDetector::default(),
                    };
                    return TouchRoute::Shell;
                }
                if settled {
                    self.gesture = Gesture::Forward { slot };
                    return self.forward_route();
                }
                // 打开动画进行中：吞掉触摸。
                TouchRoute::Shell
            }
            Mode::Switcher => {
                self.begin_tracking(now, x, y);
                if y >= self.h - BOTTOM_EDGE {
                    self.gesture = Gesture::SwitcherEdge {
                        slot,
                        start: (x, y),
                    };
                    return TouchRoute::Shell;
                }
                // 手指按下即“接住”正在滚动的列表：停在当前位置。
                let shown = self.scroll.value(now);
                pin(&mut self.scroll, shown, now);
                self.gesture = Gesture::Switcher {
                    slot,
                    start: (x, y),
                    t0: now,
                    app: self.switcher_hit(x, y, now),
                    axis: None,
                    origin: (x, y),
                    base: 0.0,
                };
                TouchRoute::Shell
            }
            Mode::Home => {
                self.begin_tracking(now, x, y);
                // 手指按下即“接住”正在翻页的主屏。
                let scroll = self.home_scroll.value(now);
                pin(&mut self.home_scroll, scroll, now);
                let page = (scroll / self.layout.page_width).round();
                self.gesture = Gesture::Home {
                    slot,
                    start: (x, y),
                    t0: now,
                    icon: self.layout.hit(x, y, scroll),
                    moved: false,
                    axis: None,
                    origin: (x, y),
                    base: 0.0,
                    start_page: (page.max(0.0) as usize).min(self.layout.pages - 1),
                };
                TouchRoute::Shell
            }
        }
    }

    fn forward_route(&self) -> TouchRoute {
        let front = (self.mode == Mode::App)
            .then(|| self.tasks.last().and_then(|t| t.window.clone()))
            .flatten();
        match front {
            Some(w) => TouchRoute::Client(w, (0.0, 0.0)),
            None => TouchRoute::Shell,
        }
    }

    pub fn touch_motion(&mut self, slot: u32, x: f64, y: f64, now: f64) -> TouchRoute {
        if self.lock.is_dragging() {
            self.lock.touch_motion(slot, y, now);
            return TouchRoute::Shell;
        }
        match self.gesture {
            Gesture::Home { slot: s, .. } if s == slot => {
                self.track(now, x, y);
                self.home_drag(x, y, now);
            }
            Gesture::AppSwipe { slot: s, start, .. } if s == slot => {
                self.track(now, x, y);
                let (rect, p) = self.swipe_rect(start, (x, y));
                if let Some(task) = self.tasks.last_mut() {
                    pin(&mut task.rect, rect, now);
                }
                let zoom = lerp(HOME_ZOOM_BEHIND_APP, 1.0, p);
                let dim = lerp(HOME_DIM_BEHIND_APP, 0.12, p);
                pin(&mut self.home, [zoom, dim], now);
                self.check_pause(now);
            }
            Gesture::SwitcherEnter {
                slot: s,
                start,
                app,
            } if s == slot => {
                self.track(now, x, y);
                if let Some(i) = self.task_index(app) {
                    let (rect, _) = self.swipe_rect(start, (x, y));
                    let slot_rect = self.switcher.slot(i, self.scroll.value(now));
                    pin(&mut self.tasks[i].rect, sub5(rect, slot_rect), now);
                }
            }
            Gesture::Switcher { slot: s, .. } if s == slot => {
                self.track(now, x, y);
                self.switcher_drag(x, y, now);
            }
            Gesture::SwitcherEdge { slot: s, .. } if s == slot => self.track(now, x, y),
            Gesture::Forward { slot: s } if s == slot => return self.forward_route(),
            _ => {}
        }
        TouchRoute::Shell
    }

    /// 主屏翻页的滚动范围内 1:1，首页之前和末页之后按橡皮筋（d = 屏幕宽度）。
    fn rubber_home(&self, raw: f64) -> f64 {
        rubber_clamp(raw, 0.0, self.layout.max_scroll(), self.w)
    }

    /// 主屏上的拖动：横向锁定后 1:1 翻页。
    fn home_drag(&mut self, x: f64, y: f64, now: f64) {
        let Gesture::Home {
            slot,
            start,
            t0,
            icon,
            mut moved,
            mut axis,
            mut origin,
            mut base,
            start_page,
        } = self.gesture
        else {
            return;
        };
        if (x - start.0).hypot(y - start.1) > TAP_SLOP {
            moved = true;
        }
        if axis.is_none()
            && let Some(locked) = lock_axis(x - start.0, y - start.1, TAP_SLOP)
        {
            axis = Some(locked);
            origin = (x, y);
            let shown = self.home_scroll.value(now);
            base = rubber_clamp_inverse(shown, 0.0, self.layout.max_scroll(), self.w);
        }
        if axis == Some(Axis::Horizontal) {
            let shown = self.rubber_home(base - (x - origin.0));
            pin(&mut self.home_scroll, shown, now);
        }
        self.gesture = Gesture::Home {
            slot,
            start,
            t0,
            icon,
            moved,
            axis,
            origin,
            base,
            start_page,
        };
    }

    /// 主屏翻页松手：`page_for_release`（FAST 投影，一次最多一页），`SNAPPY` 继承速度；
    /// 越界松手用 `SMOOTH` 弹回。
    fn home_release(
        &mut self,
        start: (f64, f64),
        origin: (f64, f64),
        base: f64,
        start_page: usize,
        now: f64,
    ) {
        let v = self.tracker.velocity_at(now);
        let last = self.last_pos.unwrap_or(start);
        let raw = base - (last.0 - origin.0);
        let shown = self.rubber_home(raw);
        let [velocity] = map_velocity(|p| [self.rubber_home(p.0)], (raw, 0.0), [-v[0], 0.0]);
        let w = self.layout.page_width;
        let page = snap::page_for_release(shown, velocity, w, self.layout.pages, start_page);
        let in_range = (-0.5..=self.layout.max_scroll() + 0.5).contains(&shown);
        let spring = if in_range {
            Spring::SNAPPY
        } else {
            Spring::SMOOTH
        };
        self.home_scroll = offset_anim(spring, shown, page as f64 * w, velocity, now);
    }

    /// 主屏被手指停在两页之间（按住后没有横向拖动）：吸附回最近的一页。
    fn settle_home(&mut self, now: f64) {
        let shown = self.home_scroll.value(now);
        let w = self.layout.page_width;
        let page = (shown / w)
            .round()
            .clamp(0.0, (self.layout.pages - 1) as f64);
        self.home_scroll = offset_anim(Spring::SNAPPY, shown, page * w, 0.0, now);
    }

    /// 多任务中的拖动：先锁定方向，横向滚动列表，纵向拖动卡片。
    fn switcher_drag(&mut self, x: f64, y: f64, now: f64) {
        let Gesture::Switcher {
            slot,
            start,
            t0,
            app,
            mut axis,
            mut origin,
            mut base,
        } = self.gesture
        else {
            return;
        };
        let n = self.tasks.len();
        let sw = self.switcher;
        let card = app.and_then(|a| self.task_index(a));
        if axis.is_none()
            && let Some(locked) = lock_axis(x - start.0, y - start.1, TAP_SLOP)
        {
            // 从锁定方向时的位置开始跟手，元素不会因为起手阈值而跳一下。
            axis = Some(locked);
            origin = (x, y);
            base = match (locked, card) {
                (Axis::Horizontal, _) => sw.unrubber_scroll(self.scroll.value(now), n),
                (Axis::Vertical, Some(i)) => {
                    switcher::card_drag_inverse(self.tasks[i].lift.value(now), sw.card_h)
                }
                (Axis::Vertical, None) => 0.0,
            };
        }
        match (axis, card) {
            (Some(Axis::Horizontal), _) => {
                let shown = sw.rubber_scroll(base - (x - origin.0), n);
                pin(&mut self.scroll, shown, now);
            }
            (Some(Axis::Vertical), Some(i)) => {
                let lift = switcher::card_drag_offset(base + (y - origin.1), sw.card_h);
                pin(&mut self.tasks[i].lift, lift, now);
            }
            _ => {}
        }
        self.gesture = Gesture::Switcher {
            slot,
            start,
            t0,
            app,
            axis,
            origin,
            base,
        };
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
                start,
                t0,
                icon,
                moved,
                axis,
                origin,
                base,
                start_page,
            } if s == slot => {
                if axis == Some(Axis::Horizontal) {
                    self.home_release(start, origin, base, start_page, now);
                } else {
                    self.settle_home(now);
                    if let (Some(app), false) = (icon, moved)
                        && now - t0 < TAP_TIMEOUT
                    {
                        self.launch(app, now);
                    }
                }
            }
            Gesture::AppSwipe { slot: s, start, .. } if s == slot => {
                let v = self.tracker.velocity_at(now);
                let last = self.last_pos.unwrap_or(start);
                let rect_velocity = map_velocity(|p| self.swipe_rect(start, p).0, last, v);
                // 判定用参考距离 D = 0.25 × 屏幕高度（见设计规范 9.1），与视觉映射分开。
                let reference = self.h * 0.25;
                let decision = (start.1 - last.1).max(0.0) / reference;
                let decision_velocity = -v[1] / reference;
                if snap::should_complete(decision, decision_velocity, Decay::FAST) {
                    self.go_home(rect_velocity, now);
                } else {
                    self.restore_app(rect_velocity, now);
                }
            }
            Gesture::SwitcherEnter {
                slot: s,
                start,
                app,
            } if s == slot => {
                // 停留在多任务：卡片带着手指速度吸附到自己的槽位。
                let v = self.tracker.velocity_at(now);
                let last = self.last_pos.unwrap_or(start);
                let velocity = map_velocity(|p| self.swipe_rect(start, p).0, last, v);
                if let Some(i) = self.task_index(app) {
                    let task = &mut self.tasks[i];
                    let current = task.rect.value(now);
                    task.rect = rect_anim(Spring::SMOOTH, current, ZERO5, velocity, now);
                }
            }
            Gesture::Switcher {
                slot: s,
                start,
                t0,
                app,
                axis,
                origin,
                base,
            } if s == slot => self.switcher_release(start, t0, app, axis, origin, base, now),
            Gesture::SwitcherEdge { slot: s, start } if s == slot => {
                let v = self.tracker.velocity_at(now);
                let last = self.last_pos.unwrap_or(start);
                let lifted = start.1 - last.1;
                let reference = self.h * 0.25;
                let tap = (last.0 - start.0).hypot(lifted) <= TAP_SLOP;
                if tap || snap::should_complete(lifted / reference, -v[1] / reference, Decay::FAST)
                {
                    self.go_home_from_switcher(now);
                }
            }
            Gesture::Forward { slot: s } if s == slot => return self.forward_route(),
            other => self.gesture = other,
        }
        TouchRoute::Shell
    }

    /// 多任务中松手。
    #[allow(clippy::too_many_arguments)]
    fn switcher_release(
        &mut self,
        start: (f64, f64),
        t0: f64,
        app: Option<usize>,
        axis: Option<Axis>,
        origin: (f64, f64),
        base: f64,
        now: f64,
    ) {
        let v = self.tracker.velocity_at(now);
        let last = self.last_pos.unwrap_or(start);
        let n = self.tasks.len();
        let sw = self.switcher;
        let card = app.and_then(|a| self.task_index(a));
        match (axis, card) {
            (None, _) => {
                let tap = (last.0 - start.0).hypot(last.1 - start.1) <= TAP_SLOP
                    && now - t0 < TAP_TIMEOUT;
                if !tap {
                    self.settle_scroll(now);
                    return;
                }
                match card {
                    Some(i) => self.open_from_switcher(i, now),
                    None => self.go_home_from_switcher(now),
                }
            }
            (Some(Axis::Horizontal), _) => {
                // 用投影落点吸附到卡片；越界松手则用 SMOOTH 弹回边界（设计规范 7.4）。
                let raw = base - (last.0 - origin.0);
                let shown = sw.rubber_scroll(raw, n);
                let [velocity] =
                    map_velocity(|p| [sw.rubber_scroll(p.0, n)], (raw, 0.0), [-v[0], 0.0]);
                let target = sw.release_target(shown, velocity, n);
                let spring = if sw.in_range(shown, n) {
                    Spring::SNAPPY
                } else {
                    Spring::SMOOTH
                };
                self.scroll = offset_anim(spring, shown, target, velocity, now);
            }
            (Some(Axis::Vertical), Some(i)) => {
                let raw = base + (last.1 - origin.1);
                let lift = switcher::card_drag_offset(raw, sw.card_h);
                let [velocity] = map_velocity(
                    |p| [switcher::card_drag_offset(p.1, sw.card_h)],
                    (0.0, raw),
                    [0.0, v[1]],
                );
                self.settle_scroll(now);
                let task = &mut self.tasks[i];
                if switcher::should_dismiss(-lift, -velocity, sw.card_h) {
                    // 记下松手时的位移和速度，卡片从这里带着速度飞出。
                    task.lift = offset_anim(Spring::SNAPPY, lift, lift, velocity, now);
                    self.dismiss(i, now);
                } else {
                    task.lift = offset_anim(Spring::SNAPPY, lift, 0.0, velocity, now);
                }
            }
            (Some(Axis::Vertical), None) => self.settle_scroll(now),
        }
    }

    /// 手指按下时列表被“接住”、之后没有横向拖动：松手后吸附回最近的卡片位置。
    fn settle_scroll(&mut self, now: f64) {
        let shown = self.scroll.value(now);
        let target = self.switcher.release_target(shown, 0.0, self.tasks.len());
        self.scroll = offset_anim(Spring::SNAPPY, shown, target, 0.0, now);
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
        let full = RectF::new(0.0, 0.0, self.w, self.h);
        let progress = self.switcher_progress.value(now).clamp(0.0, 1.0);

        let backdrop = self.backdrop.value(now).clamp(0.0, 1.0);

        // 主屏（被多任务背景完全盖住时不画）
        if backdrop < 0.999 {
            self.push_home(&mut scene, zoom, dim, now);
        }
        // 多任务背景
        if backdrop > 0.001 {
            scene.push(DrawItem::Texture {
                key: TexKey::SwitcherBackdrop,
                rect: full,
                alpha: backdrop,
                radius: 0.0,
                clip: None,
            });
        }

        // 应用卡片
        let mut cards: Vec<(&Task, [f64; 5], f64)> = Vec::new();
        match self.mode {
            Mode::Switcher => {
                let margin = switcher::SHADOW_SPREAD;
                for i in 0..self.tasks.len() {
                    let (r, _) = self.switcher_card_state(i, now);
                    if r[0] + r[2] > -margin && r[0] < self.w + margin {
                        cards.push((&self.tasks[i], r, 1.0));
                    }
                }
            }
            Mode::App | Mode::Home => {
                let n = self.tasks.len();
                // 刚离开多任务：其余卡片原地淡出、略微缩小。
                if progress > 0.001 {
                    let s = lerp(SWITCHER_EXIT_SCALE, 1.0, progress);
                    for (i, task) in self.tasks.iter().enumerate() {
                        let front = self.mode == Mode::App && i + 1 == n;
                        if front || task.homing {
                            continue;
                        }
                        let [x, y, w, h, r] = task.rect.value(now);
                        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
                        let rect = [cx - w * s / 2.0, cy - h * s / 2.0, w * s, h * s, r * s];
                        cards.push((task, rect, progress));
                    }
                }
                for (i, task) in self.tasks.iter().enumerate() {
                    let front = self.mode == Mode::App && i + 1 == n;
                    if front || task.homing {
                        cards.push((task, task.rect.value(now), 1.0));
                    }
                }
            }
        }
        for task in &self.dismissed {
            cards.push((task, task.rect.value(now), 1.0));
        }
        self.push_cards(&mut scene, &cards, progress, now);

        // Home 指示条
        if self.mode == Mode::App
            && let Some(task) = self.tasks.last()
        {
            let w = task.rect.value(now)[2];
            let q = ((w - self.layout.icon) / (self.w - self.layout.icon)).clamp(0.0, 1.0);
            let alpha = (1.0 - smoothstep(0.0, 0.15, 1.0 - q)) * 0.85 * (1.0 - progress);
            if alpha > 0.01 {
                let pill = RectF::centered(self.w / 2.0, self.h - 9.0, 140.0, 5.0);
                scene.push(DrawItem::Solid {
                    rect: pill,
                    color: [1.0, 1.0, 1.0, alpha as f32],
                    radius: 2.5,
                });
            }
        }

        // 锁屏
        if self.lock.is_visible(now) {
            self.push_lock(&mut scene, unlock, now);
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

    fn push_home(&self, scene: &mut Scene, zoom: f64, dim: f64, now: f64) {
        let (cx, cy) = (self.w / 2.0, self.h / 2.0);
        let scroll = self.home_scroll.value(now);
        let full = RectF::new(0.0, 0.0, self.w, self.h);
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
        // 前台应用和正在飞回的应用的图标由卡片代替。
        let hidden_app = self
            .tasks
            .last()
            .filter(|t| self.mode == Mode::App || t.homing)
            .map(|t| t.app);
        for i in 0..self.layout.icons.len() {
            let rect = self.layout.icon_at(i, scroll);
            // 其他页上的图标不画（名称比图标宽，留一些余量）。
            let margin = self.layout.icon;
            if Some(i) == hidden_app || rect.x + rect.w < -margin || rect.x > self.w + margin {
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
        // 页面指示点（设计规范 8.4）：当前页不透明，其余 40%，随滚动连续过渡；只有一页时隐藏。
        let pages = self.layout.pages;
        if pages > 1 {
            let total = pages as f64 * PAGE_DOT + (pages - 1) as f64 * PAGE_DOT_GAP;
            let x0 = (self.w - total) / 2.0;
            let position = scroll / self.layout.page_width;
            for p in 0..pages {
                let near = 1.0 - (position - p as f64).abs().min(1.0);
                let dot = RectF::new(
                    x0 + p as f64 * (PAGE_DOT + PAGE_DOT_GAP),
                    self.layout.dots_y - PAGE_DOT / 2.0,
                    PAGE_DOT,
                    PAGE_DOT,
                )
                .scaled_about(cx, cy, zoom);
                scene.push(DrawItem::Texture {
                    key: TexKey::PageDot,
                    rect: dot,
                    alpha: lerp(0.4, 1.0, near),
                    radius: 0.0,
                    clip: None,
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
    }

    /// 画一组卡片（[`Task`]，屏幕矩形，不透明度）：先画全部阴影，再按顺序画卡片，
    /// 避免后一张卡片的阴影压在前一张卡片上。`chrome` 为标签与阴影的显现程度。
    fn push_cards(
        &self,
        scene: &mut Scene,
        cards: &[(&Task, [f64; 5], f64)],
        chrome: f64,
        now: f64,
    ) {
        if chrome > 0.001 {
            for &(_, [x, y, w, h, _], alpha) in cards {
                let k = w / self.switcher.card_w;
                let s = switcher::SHADOW_SPREAD * k;
                scene.push(DrawItem::Texture {
                    key: TexKey::CardShadow,
                    rect: RectF::new(
                        x - s,
                        y - s + switcher::SHADOW_OFFSET_Y * k,
                        w + 2.0 * s,
                        h + 2.0 * s,
                    ),
                    alpha: chrome * alpha,
                    radius: 0.0,
                    clip: None,
                });
            }
        }
        for &(task, rect, alpha) in cards {
            self.push_card(scene, task, rect, alpha, chrome * alpha, now);
        }
    }

    /// 画一张应用卡片：底色、图标（启动闪屏）、应用内容，以及上方的标签。
    fn push_card(
        &self,
        scene: &mut Scene,
        task: &Task,
        [x, y, w, h, radius]: [f64; 5],
        alpha: f64,
        label_alpha: f64,
        now: f64,
    ) {
        let rect = RectF::new(x, y, w.max(1.0), h.max(1.0));
        let icon = self.layout.icon;
        let q = ((w - icon) / (self.w - icon)).clamp(0.0, 1.0);
        // 飞行中保持圆角，接近全屏时才过渡到屏幕本身的圆角。
        let radius = radius.max(IN_FLIGHT_RADIUS * (1.0 - smoothstep(0.9, 1.0, q)));
        let visible = task.content.value(now).clamp(0.0, 1.0) * smoothstep(0.02, 0.4, q);
        let app = &self.apps[task.app];
        let bg = loong_ui::theme::hex(app.colors.1, 1.0);
        scene.push(DrawItem::Solid {
            rect,
            color: [bg.red(), bg.green(), bg.blue(), alpha as f32],
            radius,
        });
        // 图标在卡片中央：卡片接近图标大小时图标铺满卡片，卡片变大时图标相对变小并淡出。
        let isz = w.min(h) * lerp(1.0, 0.3, smoothstep(0.0, 0.5, q));
        let (rcx, rcy) = rect.center();
        scene.push(DrawItem::Texture {
            key: TexKey::Icon(task.app),
            rect: RectF::centered(rcx, rcy, isz, isz),
            alpha: (1.0 - visible) * alpha,
            radius: 0.0,
            clip: None,
        });
        if visible > 0.001 {
            // 内容按屏幕比例铺满卡片（aspect fill），超出部分由着色器按卡片圆角裁掉。
            let content = rect.fill(self.w, self.h);
            match &task.window {
                Some(window) => scene.push(DrawItem::Window {
                    window: window.clone(),
                    rect: content,
                    alpha: visible * alpha,
                    radius,
                    clip: Some(rect),
                }),
                None if app.command.is_none() => scene.push(DrawItem::Texture {
                    key: TexKey::Placeholder(task.app),
                    rect: content,
                    alpha: visible * alpha,
                    radius,
                    clip: Some(rect),
                }),
                None => {}
            }
        }
        if label_alpha > 0.001 {
            let k = (w / self.switcher.card_w).min(1.0);
            scene.push(DrawItem::Natural {
                key: TexKey::CardLabel(task.app),
                x: x + w / 2.0,
                y: switcher::label_top(y, k),
                align: Align::Center,
                alpha: label_alpha,
                scale: k,
            });
        }
    }

    fn push_lock(&self, scene: &mut Scene, unlock: f64, now: f64) {
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
}

#[cfg(test)]
mod tests;
