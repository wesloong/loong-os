//! 控制中心（设计规范 §8.7 布局、§9.1 手势）：从顶边右侧 1/3 下滑拉出一张「绢」面板，
//! 上面是素笺模块——无线局域网、夜墨（外观）、亮度（笔触滑块）、电池。
//!
//! ## 手势
//!
//! - **打开**：手指按在顶边 24pt（状态栏）的右侧 1/3，面板随手指 1:1 下拉（展开量 = 手指下移
//!   的距离），超过完全展开的位置按橡皮筋阻尼；背后取样的整屏（模糊 + 纸色罩染）随进度淡入。
//!   松手时 `should_complete(展开量 / 行程, 速度 / 行程, FAST)` 决定展开还是收回，`SMOOTH`
//!   弹簧继承手指速度。慢拖过半能打开，轻甩一下也能打开。
//! - **打断**：面板运动中按下即被“抓住”（停在手指下），之后上下拖动从当前位置 1:1 继续。
//! - **收起**：在任意位置上滑（同一个判定，方向相反），或点按面板以外的地方。
//! - **模块**：点按无线局域网 / 夜墨开关（按下缩到 0.96 并点墨）；亮度模块上横向拖动时滑块
//!   1:1 跟手（相对拖动，按下不跳），请求按帧率节流发给 loong-sysd；轻点轨道则跳到该处。
//!   外部改了亮度（热键、设置）时滑块用 `SMOOTH` 弹簧跟过去；自己刚拖过的 0.8 秒内不理会
//!   外部值（那是自己请求的回声，可能比手指慢）。
//! - **减弱动态效果**：面板不滑动，停在展开位置随进度淡入淡出（拖动仍然 1:1 控制进度）。
//! - **锁屏**：锁屏上没有控制中心（见 `Shell::touch_down`）：丢失的设备不应能被别人关掉
//!   Wi‑Fi；亮度在解锁后调节。锁屏落下时控制中心立即收起。
//!
//! 控制中心是模态的：显示期间所有触摸都归它，不交给应用。

use loong_motion::{Decay, Spring, SpringAnimation, VelocityTracker, snap};
use loong_ui::theme::Theme;

use super::{
    DrawItem, Scene, TAP_SLOP, TAP_TIMEOUT, TexKey,
    geometry::RectF,
    gesture::{Axis, lock_axis, rubber_clamp, rubber_clamp_inverse},
    layout::STATUS_BAR_HEIGHT,
    pin,
    system::{BatteryGlyph, StatusGlyphs, SystemAction, SystemStatus, Throttle, power_summary},
};

/// 面板距屏幕顶边、右边（点）。
pub const MARGIN_TOP: f64 = 32.0;
pub const MARGIN_RIGHT: f64 = 16.0;
/// 面板宽度（点）：4 列 × 80 + 3 × 16 + 两侧内边距 16。
pub const PANEL_W: f64 = 400.0;
/// 模块网格：单元 80pt，间距 16pt，面板内边距 16pt。
pub const UNIT: f64 = 80.0;
pub const GAP: f64 = 16.0;
pub const PAD: f64 = 16.0;
/// 面板圆角（`panel`）与模块圆角（`card`）。
pub const PANEL_RADIUS: f64 = 22.0;
pub const TILE_RADIUS: f64 = 16.0;
/// 面板墨影（`shadowMid`）、模块墨影（`shadowLow`）向四周扩展的距离（点）。
pub const PANEL_SHADOW: f64 = 28.0;
pub const TILE_SHADOW: f64 = 10.0;
/// 亮度滑块：轨道两端距模块边缘、轨道中线距模块顶边、轨道位图高度（点）。
pub const TRACK_INSET: f64 = 52.0;
pub const TRACK_Y: f64 = 54.0;
pub const TRACK_H: f64 = 16.0;
/// 滑块的纸团直径与墨影扩展（点）。
pub const KNOB: f64 = 26.0;
pub const KNOB_SHADOW: f64 = 6.0;
/// 拖完亮度后多久不理会外部的亮度值（秒）。
const HOLD_EXTERNAL: f64 = 0.8;
/// 亮度请求的最小间隔（秒）：约一帧。
const BRIGHTNESS_INTERVAL: f64 = 1.0 / 60.0;
/// 点了 Wi‑Fi 开关后，等 loong-sysd 报告新状态的最长时间（秒），之后按实际状态显示。
const WIFI_PENDING: f64 = 3.0;
/// 模块按下时的缩放。
const PRESSED_SCALE: f64 = 0.96;

/// 手指是否落在打开控制中心的区域：顶边状态栏高度内、屏幕右侧 1/3（左侧和中部留给通知中心）。
pub fn in_pull_zone(x: f64, y: f64, screen_w: f64) -> bool {
    y < STATUS_BAR_HEIGHT && x >= screen_w * 2.0 / 3.0
}

/// 模块。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tile {
    Wifi,
    Appearance,
    Brightness,
    Battery,
}

impl Tile {
    pub const ALL: [Tile; 4] = [
        Tile::Wifi,
        Tile::Appearance,
        Tile::Brightness,
        Tile::Battery,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// 点按即生效的开关（按下缩放、点墨）。
    fn is_button(self) -> bool {
        matches!(self, Tile::Wifi | Tile::Appearance)
    }
}

/// 控制中心各部分的位图（`render/control_center.rs` 生成）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    /// 打开时取样的整屏：模糊 + 纸色罩染（1/4 分辨率）。
    Backdrop,
    /// 「绢」面板，四周带 [`PANEL_SHADOW`] 的墨影。
    Panel,
    /// 一个模块：素笺 + 符号与文字，四周带 [`TILE_SHADOW`] 的墨影。
    Tile(Tile),
    /// 亮度滑块：一笔淡墨的轨道。
    Track,
    /// 亮度滑块：已着墨的一段（浓墨 / 泥金，按亮度裁剪）。
    Ink,
    /// 亮度滑块的纸团（四周带 [`KNOB_SHADOW`]）。
    Knob,
}

/// 开关模块上的符号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Symbol {
    Wifi,
    WifiOff,
    Moon,
}

/// 一个模块此刻显示的内容：变了才重画它的位图。
#[derive(Debug, Clone, PartialEq)]
pub enum TileView {
    /// 开关：符号、标题、说明；`active` 为开启（着颜料：亮色石青、暗色泥金），`enabled` 为可用。
    Toggle {
        symbol: Symbol,
        title: &'static str,
        subtitle: String,
        active: bool,
        enabled: bool,
    },
    /// 亮度：标题与两端的日；没有背光时显示“不可用”。
    Brightness { available: bool },
    /// 电池：电池符号与百分比（没有电池时为 `None`）、说明。
    Battery {
        glyph: Option<BatteryGlyph>,
        text: String,
    },
}

/// 布局（逻辑点）。模块与轨道相对面板左上角。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CcLayout {
    /// 完全展开时面板的位置。
    pub panel: RectF,
    tiles: [RectF; 4],
}

impl CcLayout {
    pub fn new(screen_w: f64) -> Self {
        let wide = 4.0 * UNIT + 3.0 * GAP;
        let half = 2.0 * UNIT + GAP;
        let row = |r: f64| PAD + r * (UNIT + GAP);
        let h = 2.0 * PAD + 3.0 * UNIT + 2.0 * GAP;
        CcLayout {
            panel: RectF::new(
                (screen_w - MARGIN_RIGHT - PANEL_W).max(MARGIN_RIGHT),
                MARGIN_TOP,
                PANEL_W,
                h,
            ),
            tiles: [
                RectF::new(PAD, row(0.0), half, UNIT),
                RectF::new(PAD + half + GAP, row(0.0), half, UNIT),
                RectF::new(PAD, row(1.0), wide, UNIT),
                RectF::new(PAD, row(2.0), wide, UNIT),
            ],
        }
    }

    /// 模块相对面板的位置。
    pub fn tile(&self, tile: Tile) -> RectF {
        self.tiles[tile.index()]
    }

    /// 亮度轨道（相对面板）：起点 x、终点 x、中线 y。
    pub fn track(&self) -> (f64, f64, f64) {
        let t = self.tile(Tile::Brightness);
        (t.x + TRACK_INSET, t.x + t.w - TRACK_INSET, t.y + TRACK_Y)
    }

    fn track_len(&self) -> f64 {
        let (x0, x1, _) = self.track();
        (x1 - x0).max(1.0)
    }

    /// 面板完全收起所需的下移量：面板连同墨影都离开屏幕。
    pub fn travel(&self) -> f64 {
        self.panel.y + self.panel.h + PANEL_SHADOW
    }
}

#[derive(Debug, Clone, Copy)]
enum Phase {
    /// 面板跟着手指上下走（从顶边拉出，或在打开的面板上锁定为纵向）。`base` 为开始跟手时的
    /// 展开量（已去掉橡皮筋），`origin_y` 为当时的手指位置。
    Drag { base: f64, origin_y: f64 },
    /// 面板打开时按下，还没决定是点按还是拖动。
    Pending { tile: Option<Tile> },
    /// 横向拖动亮度滑块。
    Slider { base: f64, origin_x: f64 },
    /// 横向划过别处：不做任何事。
    Ignored,
}

#[derive(Debug, Clone, Copy)]
struct Touch {
    slot: u32,
    start: (f64, f64),
    t0: f64,
    last: (f64, f64),
    phase: Phase,
    /// 按下时面板正在运动、被手指抓住（松手后要重新决定去向）。
    caught: bool,
}

/// 控制中心的状态。
pub struct ControlCenter {
    w: f64,
    h: f64,
    pub layout: CcLayout,
    /// 展开量（点）：0 收起，[`CcLayout::travel`] 完全展开，拖过头时更大（橡皮筋）。
    reveal: SpringAnimation<f64>,
    touch: Option<Touch>,
    tracker: VelocityTracker<[f64; 2]>,
    /// 滑块显示的亮度 0..1。
    slider: SpringAnimation<f64>,
    throttle: Throttle,
    /// 在此之前不理会外部亮度（自己刚拖过）。
    hold_until: f64,
    /// 最近一次跟随过的外部亮度。
    followed: Option<f64>,
    /// 刚点过的 Wi‑Fi 开关：（期望的状态，截止时间）。
    wifi_pending: Option<(bool, f64)>,
    /// 各模块按下时的缩放。
    press: [SpringAnimation<f64>; 4],
    actions: Vec<SystemAction>,
    /// 每次从收起开始展开加一：背后的取样要重新做。
    epoch: u64,
    /// 减弱动态效果：淡入淡出代替滑动。
    pub reduced_motion: bool,
}

impl ControlCenter {
    pub fn new(w: f64, h: f64) -> Self {
        let press = || SpringAnimation::at_rest(Spring::SNAPPY, 1.0, 0.0);
        ControlCenter {
            w,
            h,
            layout: CcLayout::new(w),
            reveal: SpringAnimation::at_rest(Spring::SMOOTH, 0.0, 0.0)
                .with_rest_thresholds(0.05, 0.5),
            touch: None,
            tracker: VelocityTracker::new(),
            slider: SpringAnimation::at_rest(Spring::SMOOTH, 0.5, 0.0),
            throttle: Throttle::new(BRIGHTNESS_INTERVAL),
            hold_until: f64::NEG_INFINITY,
            followed: None,
            wifi_pending: None,
            press: [press(), press(), press(), press()],
            actions: Vec::new(),
            epoch: 0,
            reduced_motion: false,
        }
    }

    fn travel(&self) -> f64 {
        self.layout.travel()
    }

    /// 展开进度 0..1（拖过头时为 1）。
    pub fn progress(&self, now: f64) -> f64 {
        (self.reveal.value(now) / self.travel()).clamp(0.0, 1.0)
    }

    /// 是否显示（或者手指正拉着它）。显示期间独占所有触摸。
    pub fn is_visible(&self, now: f64) -> bool {
        self.touch.is_some() || self.reveal.value(now) > 0.05 || self.reveal.target() > 0.0
    }

    /// 是否已经（或正在）完全展开。
    #[allow(dead_code, reason = "测试使用")]
    pub fn is_open(&self) -> bool {
        self.reveal.target() >= self.travel() - 1e-6
    }

    pub fn has_touch(&self) -> bool {
        self.touch.is_some()
    }

    /// 取样的序号：变了就要重新取样背后的画面。
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// 滑块此刻显示的亮度。
    pub fn slider_value(&self, now: f64) -> f64 {
        self.slider.value(now).clamp(0.0, 1.0)
    }

    pub fn is_idle(&self, now: f64) -> bool {
        self.touch.is_none()
            && self.reveal.is_done(now)
            && self.slider.is_done(now)
            && self.press.iter().all(|p| p.is_done(now))
            && self.throttle.is_idle()
            && self.wifi_pending.is_none()
    }

    /// 取走待执行的操作。
    pub fn take_actions(&mut self) -> Vec<SystemAction> {
        std::mem::take(&mut self.actions)
    }

    /// 立即收起（锁屏落下时）。
    pub fn close_now(&mut self, now: f64) {
        self.touch = None;
        self.reveal =
            SpringAnimation::at_rest(Spring::SMOOTH, 0.0, now).with_rest_thresholds(0.05, 0.5);
        for p in &mut self.press {
            *p = SpringAnimation::at_rest(Spring::SNAPPY, 1.0, now);
        }
        if let Some(v) = self.throttle.flush(now) {
            self.actions.push(SystemAction::Brightness(v));
        }
    }

    /// 面板此刻在屏幕上的下移量（相对完全展开的位置；减弱动态效果时不移动）。
    fn offset(&self, now: f64) -> f64 {
        if self.reduced_motion {
            0.0
        } else {
            self.reveal.value(now) - self.travel()
        }
    }

    /// 面板此刻在屏幕上的位置。
    pub fn panel_rect(&self, now: f64) -> RectF {
        let p = self.layout.panel;
        RectF::new(p.x, p.y + self.offset(now), p.w, p.h)
    }

    /// 模块此刻在屏幕上的位置（不含按下的缩放）。
    fn tile_rect(&self, tile: Tile, now: f64) -> RectF {
        let p = self.panel_rect(now);
        let t = self.layout.tile(tile);
        RectF::new(p.x + t.x, p.y + t.y, t.w, t.h)
    }

    /// 手指下的模块（面板展开过半时才算）。
    pub fn tile_at(&self, x: f64, y: f64, now: f64) -> Option<Tile> {
        if self.progress(now) < 0.5 {
            return None;
        }
        Tile::ALL
            .into_iter()
            .find(|&t| self.tile_rect(t, now).contains(x, y))
    }

    /// 拖动时的展开量：手指对应的位置 `raw` → 显示的位置（超过完全展开按橡皮筋）。
    fn rubber(&self, raw: f64) -> f64 {
        rubber_clamp(raw, 0.0, self.travel(), self.h).max(0.0)
    }

    // ───────────────────────── 触摸 ─────────────────────────

    /// 按下。由 `Shell` 在控制中心显示时、或手指落在 [`in_pull_zone`] 时调用。
    /// 返回按到的开关模块（用于点墨）。
    pub fn touch_down(
        &mut self,
        slot: u32,
        x: f64,
        y: f64,
        now: f64,
        status: &SystemStatus,
    ) -> Option<Tile> {
        if self.touch.is_some() {
            return None;
        }
        self.tracker.reset();
        self.tracker.add(now, [x, y]);
        let shown = self.reveal.value(now);
        let moving = !self.reveal.is_done(now);
        let (phase, caught) = if shown <= 0.05 && self.reveal.target() <= 0.0 {
            // 从收起状态拉出：重新取样背后的画面，滑块就位到当前亮度。
            self.epoch += 1;
            if let Some(b) = status.brightness() {
                self.slider = SpringAnimation::at_rest(Spring::SMOOTH, b, now);
                self.followed = Some(b);
            }
            pin(&mut self.reveal, 0.0, now);
            (
                Phase::Drag {
                    base: 0.0,
                    origin_y: y,
                },
                false,
            )
        } else {
            if moving {
                // 抓住运动中的面板。
                pin(&mut self.reveal, shown, now);
            }
            let tile = self.tile_at(x, y, now);
            if let Some(t) = tile.filter(|t| t.is_button()) {
                self.press[t.index()].retarget(PRESSED_SCALE, now);
            }
            (Phase::Pending { tile }, moving)
        };
        self.touch = Some(Touch {
            slot,
            start: (x, y),
            t0: now,
            last: (x, y),
            phase,
            caught,
        });
        match phase {
            Phase::Pending { tile: Some(t) } if t.is_button() => Some(t),
            _ => None,
        }
    }

    /// 手指移动。返回 `true` 表示这一下刚刚开始拖动（按下时的点墨该淡去了）。
    pub fn touch_motion(
        &mut self,
        slot: u32,
        x: f64,
        y: f64,
        now: f64,
        status: &SystemStatus,
    ) -> bool {
        let Some(mut touch) = self.touch.filter(|t| t.slot == slot) else {
            return false;
        };
        self.tracker.add(now, [x, y]);
        touch.last = (x, y);
        let mut started = false;
        if let Phase::Pending { tile } = touch.phase
            && let Some(axis) = lock_axis(x - touch.start.0, y - touch.start.1, TAP_SLOP)
        {
            started = true;
            self.release_press(now);
            touch.phase = match (axis, tile) {
                (Axis::Horizontal, Some(Tile::Brightness)) if status.brightness().is_some() => {
                    let base = self.slider.value(now).clamp(0.0, 1.0);
                    Phase::Slider { base, origin_x: x }
                }
                (Axis::Horizontal, _) => Phase::Ignored,
                (Axis::Vertical, _) => Phase::Drag {
                    base: rubber_clamp_inverse(self.reveal.value(now), 0.0, self.travel(), self.h),
                    origin_y: y,
                },
            };
        }
        match touch.phase {
            Phase::Drag { base, origin_y } => {
                let shown = self.rubber(base + (y - origin_y));
                pin(&mut self.reveal, shown, now);
            }
            Phase::Slider { base, origin_x } => {
                let value = (base + (x - origin_x) / self.layout.track_len()).clamp(0.0, 1.0);
                pin(&mut self.slider, value, now);
                if let Some(v) = self.throttle.offer(value, now) {
                    self.actions.push(SystemAction::Brightness(v));
                }
            }
            Phase::Pending { .. } | Phase::Ignored => {}
        }
        self.touch = Some(touch);
        started
    }

    /// 正在拖动面板的手指的起点（系统手势的笔迹从这里起笔）。
    pub fn drag_start(&self, slot: u32) -> Option<(f64, f64)> {
        self.touch
            .filter(|t| t.slot == slot && matches!(t.phase, Phase::Drag { .. }))
            .map(|t| t.start)
    }

    /// 抬起。
    pub fn touch_up(&mut self, slot: u32, now: f64, status: &SystemStatus, theme: Theme) {
        let Some(touch) = self.touch.filter(|t| t.slot == slot) else {
            return;
        };
        self.touch = None;
        self.release_press(now);
        match touch.phase {
            Phase::Drag { base, origin_y } => {
                let v = self.tracker.velocity_at(now)[1];
                let raw = base + (touch.last.1 - origin_y);
                let shown = self.rubber(raw);
                // 手指速度经橡皮筋映射成展开量的速度（数值微分），保证松手不“顿”。
                let dt = 0.01;
                let velocity = (self.rubber(raw + v * dt) - shown) / dt;
                let travel = self.travel();
                let open = snap::should_complete(shown / travel, velocity / travel, Decay::FAST);
                self.settle(shown, open, velocity, now);
            }
            Phase::Pending { tile } => {
                let (dx, dy) = (touch.last.0 - touch.start.0, touch.last.1 - touch.start.1);
                let tap = dx.hypot(dy) <= TAP_SLOP && now - touch.t0 < TAP_TIMEOUT;
                let inside = self.panel_rect(now).contains(touch.start.0, touch.start.1);
                if tap && !inside {
                    self.close(now);
                    return;
                }
                if tap && let Some(t) = tile {
                    self.activate(t, touch.start.0, now, status, theme);
                }
                if touch.caught {
                    let shown = self.reveal.value(now);
                    self.settle(shown, shown >= self.travel() / 2.0, 0.0, now);
                }
            }
            Phase::Slider { .. } => {
                if let Some(v) = self.throttle.flush(now) {
                    self.actions.push(SystemAction::Brightness(v));
                }
                self.hold_until = now + HOLD_EXTERNAL;
                if touch.caught {
                    self.settle(self.reveal.value(now), true, 0.0, now);
                }
            }
            Phase::Ignored => {
                if touch.caught {
                    let shown = self.reveal.value(now);
                    self.settle(shown, shown >= self.travel() / 2.0, 0.0, now);
                }
            }
        }
    }

    /// 从 `shown` 带着速度 `velocity` 用 `SMOOTH` 驶向展开或收起。
    fn settle(&mut self, shown: f64, open: bool, velocity: f64, now: f64) {
        let target = if open { self.travel() } else { 0.0 };
        self.reveal = SpringAnimation::new(Spring::SMOOTH, shown, target, velocity, now)
            .with_rest_thresholds(0.05, 0.5);
    }

    /// 收起（点按面板以外）。
    pub fn close(&mut self, now: f64) {
        let (shown, velocity) = (self.reveal.value(now), self.reveal.velocity(now));
        self.settle(shown, false, velocity, now);
    }

    fn release_press(&mut self, now: f64) {
        for p in &mut self.press {
            if p.target() != 1.0 {
                p.retarget(1.0, now);
            }
        }
    }

    /// 点按模块。
    fn activate(&mut self, tile: Tile, x: f64, now: f64, status: &SystemStatus, theme: Theme) {
        match tile {
            Tile::Wifi => {
                let Some(enabled) = self.wifi_enabled(status) else {
                    return;
                };
                self.actions.push(SystemAction::WifiEnabled(!enabled));
                self.wifi_pending = Some((!enabled, now + WIFI_PENDING));
            }
            Tile::Appearance => {
                let next = match theme {
                    Theme::Light => Theme::Dark,
                    Theme::Dark => Theme::Light,
                };
                self.actions.push(SystemAction::Theme(next));
            }
            Tile::Brightness => {
                // 轻点轨道：滑块跳到该处（SNAPPY）。纸团两侧留半个纸团的余量。
                if status.brightness().is_none() {
                    return;
                }
                let (x0, x1, _) = self.layout.track();
                let px = self.panel_rect(now).x;
                let (x0, x1) = (px + x0, px + x1);
                if x < x0 - KNOB / 2.0 || x > x1 + KNOB / 2.0 {
                    return;
                }
                let value = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
                self.slider.set_spring(Spring::SNAPPY, now);
                self.slider.retarget(value, now);
                self.throttle.flush(now);
                self.throttle.offer(value, now);
                self.actions.push(SystemAction::Brightness(value));
                self.hold_until = now + HOLD_EXTERNAL;
            }
            Tile::Battery => {}
        }
    }

    /// 显示的 Wi‑Fi 开关状态（刚点过时用期望的状态）；不可用时为 `None`。
    fn wifi_enabled(&self, status: &SystemStatus) -> Option<bool> {
        let wifi = status.wifi()?;
        Some(self.wifi_pending.map_or(wifi.enabled, |(on, _)| on))
    }

    /// 每帧调用：补发节流的亮度、跟随外部亮度、结束 Wi‑Fi 的等待。
    pub fn update(&mut self, now: f64, status: &SystemStatus) {
        if let Some(v) = self.throttle.poll(now) {
            self.actions.push(SystemAction::Brightness(v));
        }
        let dragging = self
            .touch
            .is_some_and(|t| matches!(t.phase, Phase::Slider { .. }));
        if let Some(b) = status.brightness()
            && !dragging
            && now >= self.hold_until
            && self.followed != Some(b)
        {
            if self.is_visible(now) {
                // 外部改了亮度：弹簧跟过去（速度连续）。
                if (self.slider.target() - b).abs() > 1e-6 {
                    self.slider.set_spring(Spring::SMOOTH, now);
                    self.slider.retarget(b, now);
                }
            } else {
                self.slider = SpringAnimation::at_rest(Spring::SMOOTH, b, now);
            }
            self.followed = Some(b);
        }
        if let Some((on, deadline)) = self.wifi_pending {
            let reached = status.wifi().is_none_or(|w| w.enabled == on);
            if reached || now >= deadline {
                self.wifi_pending = None;
            }
        }
    }

    // ───────────────────────── 画面 ─────────────────────────

    /// 某个模块此刻的内容。
    pub fn tile_view(&self, tile: Tile, status: &SystemStatus, theme: Theme) -> TileView {
        match tile {
            Tile::Wifi => {
                let Some(wifi) = status.wifi() else {
                    return TileView::Toggle {
                        symbol: Symbol::WifiOff,
                        title: "无线局域网",
                        subtitle: "不可用".into(),
                        active: false,
                        enabled: false,
                    };
                };
                let enabled = self.wifi_enabled(status).unwrap_or(wifi.enabled);
                let subtitle = if !enabled {
                    "已关闭".into()
                } else if self.wifi_pending.is_some() && !wifi.enabled {
                    "正在打开…".into()
                } else if wifi.state.is_connected() && !wifi.ssid.is_empty() {
                    wifi.ssid.clone()
                } else if wifi.state == super::system::WifiState::Connecting {
                    "正在连接…".into()
                } else {
                    "未连接".into()
                };
                TileView::Toggle {
                    symbol: if enabled {
                        Symbol::Wifi
                    } else {
                        Symbol::WifiOff
                    },
                    title: "无线局域网",
                    subtitle,
                    active: enabled,
                    enabled: true,
                }
            }
            Tile::Appearance => TileView::Toggle {
                symbol: Symbol::Moon,
                title: "夜墨",
                subtitle: match theme {
                    Theme::Dark => "已开启".into(),
                    Theme::Light => "已关闭".into(),
                },
                active: theme == Theme::Dark,
                enabled: true,
            },
            Tile::Brightness => TileView::Brightness {
                available: status.brightness().is_some(),
            },
            Tile::Battery => TileView::Battery {
                glyph: StatusGlyphs::from_status(status).battery,
                text: power_summary(status),
            },
        }
    }

    /// 画控制中心：背后的取样、面板、模块、滑块。`brightness` 为滑块是否可用。
    pub fn push_scene(&self, scene: &mut Scene, brightness: bool, now: f64) {
        // 完全收起时（含刚按下、还没拉动）面板与墨影都在屏幕外，什么也不画。
        let p = self.progress(now);
        if p <= 0.001 {
            return;
        }
        let tex = |key: Part, rect: RectF, alpha: f64, clip: Option<RectF>| DrawItem::Texture {
            key: TexKey::ControlCenter(key),
            rect,
            alpha,
            radius: 0.0,
            clip,
            reveal: None,
        };
        // 背后：打开时取样的整屏（模糊 + 罩染），随进度淡入。
        let screen = RectF::new(0.0, 0.0, self.w, self.h);
        scene.push(tex(Part::Backdrop, screen, p, None));
        let alpha = if self.reduced_motion { p } else { 1.0 };
        let panel = self.panel_rect(now);
        let s = PANEL_SHADOW;
        scene.push(tex(
            Part::Panel,
            RectF::new(
                panel.x - s,
                panel.y - s,
                panel.w + 2.0 * s,
                panel.h + 2.0 * s,
            ),
            alpha,
            None,
        ));
        for tile in Tile::ALL {
            let r = self.tile_rect(tile, now);
            let (cx, cy) = r.center();
            let k = self.press[tile.index()].value(now);
            let s = TILE_SHADOW;
            let outer = RectF::new(r.x - s, r.y - s, r.w + 2.0 * s, r.h + 2.0 * s);
            scene.push(tex(
                Part::Tile(tile),
                outer.scaled_about(cx, cy, k),
                alpha,
                None,
            ));
        }
        // 亮度滑块：一笔淡墨的轨道；已着墨的一段按亮度裁剪；纸团。
        let (x0, x1, ty) = self.layout.track();
        let (x0, x1, ty) = (panel.x + x0, panel.x + x1, panel.y + ty);
        let track = RectF::new(x0, ty - TRACK_H / 2.0, x1 - x0, TRACK_H);
        scene.push(tex(Part::Track, track, alpha, None));
        if brightness {
            let value = self.slider_value(now);
            let kx = x0 + value * (x1 - x0);
            let inked = RectF::new(track.x, track.y, kx - x0, track.h);
            if inked.w > 0.5 {
                scene.push(tex(Part::Ink, track, alpha, Some(inked)));
            }
            let d = KNOB + 2.0 * KNOB_SHADOW;
            scene.push(tex(Part::Knob, RectF::centered(kx, ty, d, d), alpha, None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::system::{
        BacklightStatus, PowerState, PowerStatus, Service, WifiState, WifiStatus,
    };

    const W: f64 = 1280.0;
    const H: f64 = 800.0;
    const FRAME: f64 = 1.0 / 120.0;

    fn status() -> SystemStatus {
        SystemStatus {
            service: Service::Running,
            power: Some(PowerStatus {
                has_battery: true,
                percentage: 57.0,
                state: PowerState::Discharging,
                time_to_empty: 3600,
                ..PowerStatus::default()
            }),
            wifi: Some(WifiStatus {
                available: true,
                enabled: true,
                state: WifiState::Connected,
                ssid: "听雨轩".into(),
                strength: 70,
            }),
            backlight: Some(BacklightStatus {
                available: true,
                brightness: 0.3,
            }),
        }
    }

    struct Sim {
        cc: ControlCenter,
        status: SystemStatus,
        now: f64,
    }

    impl Sim {
        fn new() -> Self {
            Sim {
                cc: ControlCenter::new(W, H),
                status: status(),
                now: 0.0,
            }
        }

        fn wait(&mut self, secs: f64) {
            let end = self.now + secs;
            while self.now < end - 1e-9 {
                self.now = (self.now + FRAME).min(end);
                self.cc.update(self.now, &self.status);
            }
        }

        fn down(&mut self, x: f64, y: f64) -> Option<Tile> {
            self.cc.touch_down(0, x, y, self.now, &self.status)
        }

        /// 拖动到 `to`（120Hz），停 `hold` 秒，不松手。
        fn drag(&mut self, from: (f64, f64), to: (f64, f64), dur: f64, hold: f64) {
            let steps = ((dur * 120.0).ceil() as usize).max(1);
            for k in 1..=steps {
                let f = k as f64 / steps as f64;
                self.now += dur / steps as f64;
                let x = from.0 + (to.0 - from.0) * f;
                let y = from.1 + (to.1 - from.1) * f;
                self.cc.touch_motion(0, x, y, self.now, &self.status);
                self.cc.update(self.now, &self.status);
            }
            self.wait(hold);
        }

        fn up(&mut self) {
            self.cc.touch_up(0, self.now, &self.status, Theme::Light);
        }

        fn swipe(&mut self, from: (f64, f64), to: (f64, f64), dur: f64, hold: f64) {
            self.down(from.0, from.1);
            self.drag(from, to, dur, hold);
            self.up();
        }

        fn tap(&mut self, x: f64, y: f64) {
            self.down(x, y);
            self.wait(0.05);
            self.up();
        }

        fn open(&mut self) {
            self.swipe((1100.0, 6.0), (1100.0, 300.0), 0.25, 0.0);
            self.wait(1.2);
            assert!(self.cc.is_open() && self.cc.reveal.is_done(self.now));
        }

        fn tile_center(&self, tile: Tile) -> (f64, f64) {
            self.cc.tile_rect(tile, self.now).center()
        }

        fn reveal(&self) -> f64 {
            self.cc.reveal.value(self.now)
        }
    }

    #[test]
    fn layout_follows_the_spec_grid() {
        let l = CcLayout::new(W);
        assert_eq!(l.panel, RectF::new(864.0, 32.0, 400.0, 304.0));
        assert_eq!(l.tile(Tile::Wifi), RectF::new(16.0, 16.0, 176.0, 80.0));
        assert_eq!(
            l.tile(Tile::Appearance),
            RectF::new(208.0, 16.0, 176.0, 80.0)
        );
        assert_eq!(l.tile(Tile::Brightness).w, 368.0);
        assert_eq!(l.tile(Tile::Battery).y, 208.0);
        assert!(in_pull_zone(1000.0, 10.0, W) && in_pull_zone(1279.0, 0.0, W));
        assert!(!in_pull_zone(800.0, 10.0, W), "左侧与中部留给通知中心");
        assert!(!in_pull_zone(1100.0, 30.0, W), "只在顶边状态栏高度内");
    }

    #[test]
    fn panel_follows_the_finger_one_to_one() {
        let mut s = Sim::new();
        s.down(1100.0, 4.0);
        assert!(s.cc.is_visible(s.now));
        s.drag((1100.0, 4.0), (1100.0, 154.0), 0.3, 0.1);
        assert!((s.reveal() - 150.0).abs() < 1e-6, "{}", s.reveal());
        let panel = s.cc.panel_rect(s.now);
        assert!((panel.y - (32.0 + 150.0 - s.cc.travel())).abs() < 1e-6);
        // 拖过完全展开的位置：橡皮筋，越拖越费劲。
        let travel = s.cc.travel();
        s.drag((1100.0, 154.0), (1100.0, 4.0 + travel + 200.0), 0.3, 0.1);
        let over = s.reveal() - travel;
        assert!(over > 0.0 && over < 200.0 * 0.56, "{over}");
    }

    #[test]
    fn slow_drag_past_half_opens_short_drag_closes() {
        let mut s = Sim::new();
        let travel = s.cc.travel();
        // 慢拖过半、停住再松手：展开。
        s.swipe((1100.0, 4.0), (1100.0, 4.0 + travel * 0.6), 0.6, 0.1);
        assert!(s.cc.is_open());
        s.wait(1.5);
        assert!((s.reveal() - travel).abs() < 0.1);
        assert!(s.cc.is_idle(s.now));
        // 在面板上慢慢上推不到一半、停住：留在展开。
        let (x, y) = (1000.0, 300.0);
        s.swipe((x, y), (x, y - travel * 0.3), 0.4, 0.1);
        assert!(s.cc.is_open());
        s.wait(1.5);
        // 新的一次：只拉一点点，停住再松手：收回。
        let mut s = Sim::new();
        s.swipe((1100.0, 4.0), (1100.0, 100.0), 0.3, 0.1);
        assert!(!s.cc.is_open());
        s.wait(1.5);
        assert!(!s.cc.is_visible(s.now) && s.cc.is_idle(s.now));
    }

    #[test]
    fn flick_opens_and_inherits_velocity() {
        let mut s = Sim::new();
        // 只拉了 80 点，但甩得快：展开，且松手后继续向下（速度连续）。
        s.swipe((1100.0, 4.0), (1100.0, 84.0), 0.05, 0.0);
        assert!(s.cc.is_open());
        let v = s.cc.reveal.velocity(s.now);
        assert!(v > 500.0, "{v}");
        // 向上甩：收起。
        s.wait(1.5);
        s.swipe((1000.0, 250.0), (1000.0, 180.0), 0.05, 0.0);
        assert!(!s.cc.is_open());
        assert!(s.cc.reveal.velocity(s.now) < -500.0);
    }

    #[test]
    fn moving_panel_can_be_caught_and_pulled_back() {
        let mut s = Sim::new();
        s.open();
        // 点按面板外：开始收起。
        s.tap(200.0, 500.0);
        assert!(!s.cc.is_open());
        s.wait(0.1);
        let caught_at = s.reveal();
        assert!(caught_at > 1.0 && caught_at < s.cc.travel() - 1.0);
        // 运动中按下：停在手指下（不跳）。
        s.down(1000.0, 100.0);
        assert!((s.reveal() - caught_at).abs() < 1e-6);
        s.wait(0.05);
        assert!((s.reveal() - caught_at).abs() < 1e-6, "按住时不动");
        // 往下拉：从当前位置 1:1 继续，松手展开。
        s.drag((1000.0, 100.0), (1000.0, 200.0), 0.2, 0.1);
        assert!(s.reveal() > caught_at + 80.0, "{} {caught_at}", s.reveal());
        s.up();
        assert!(s.cc.is_open());
    }

    #[test]
    fn tapping_toggles_emits_actions() {
        let mut s = Sim::new();
        s.open();
        let (x, y) = s.tile_center(Tile::Wifi);
        assert_eq!(s.down(x, y), Some(Tile::Wifi), "开关模块：点墨");
        assert!(s.cc.press[Tile::Wifi.index()].target() < 1.0, "按下缩小");
        s.wait(0.05);
        s.up();
        assert_eq!(s.cc.take_actions(), vec![SystemAction::WifiEnabled(false)]);
        // 等 loong-sysd 的回音期间显示期望的状态。
        assert!(matches!(
            s.cc.tile_view(Tile::Wifi, &s.status, Theme::Light),
            TileView::Toggle { active: false, .. }
        ));
        s.status.wifi.as_mut().unwrap().enabled = false;
        s.wait(0.1);
        assert!(s.cc.wifi_pending.is_none());
        let (x, y) = s.tile_center(Tile::Appearance);
        s.tap(x, y);
        assert_eq!(s.cc.take_actions(), vec![SystemAction::Theme(Theme::Dark)]);
        assert!(s.cc.is_open(), "点开关不收起面板");
        // 电池模块不是按钮。
        let (x, y) = s.tile_center(Tile::Battery);
        assert_eq!(s.down(x, y), None);
        s.up();
        assert!(s.cc.take_actions().is_empty());
        // 点按面板外：收起。
        s.tap(300.0, 600.0);
        assert!(!s.cc.is_open());
    }

    #[test]
    fn unavailable_wifi_does_nothing() {
        let mut s = Sim::new();
        s.status = SystemStatus::absent();
        s.open();
        let (x, y) = s.tile_center(Tile::Wifi);
        s.tap(x, y);
        assert!(s.cc.take_actions().is_empty());
        assert_eq!(
            s.cc.tile_view(Tile::Wifi, &s.status, Theme::Light),
            TileView::Toggle {
                symbol: Symbol::WifiOff,
                title: "无线局域网",
                subtitle: "不可用".into(),
                active: false,
                enabled: false,
            }
        );
        // 亮度不可用：横向拖动不改变任何东西。
        let (x, y) = s.tile_center(Tile::Brightness);
        s.swipe((x, y), (x + 100.0, y), 0.2, 0.0);
        assert!(s.cc.take_actions().is_empty());
    }

    #[test]
    fn slider_tracks_finger_and_throttles_requests() {
        let mut s = Sim::new();
        s.open();
        assert!(
            (s.cc.slider_value(s.now) - 0.3).abs() < 1e-9,
            "打开时就位到当前亮度"
        );
        let len = s.cc.layout.track_len();
        let (x, y) = s.tile_center(Tile::Brightness);
        // 相对拖动：按下不跳；从锁定方向时的位置开始 1:1 跟手（起手阈值内的那几点不算，
        // 与翻页相同，滑块不会在锁定时跳一下）。
        s.down(x, y);
        assert!((s.cc.slider_value(s.now) - 0.3).abs() < 1e-9);
        s.drag((x, y), (x + len * 0.4, y), 0.4, 0.0);
        let mid = s.cc.slider_value(s.now);
        assert!(mid > 0.6 && mid < 0.7, "{mid}");
        s.drag((x + len * 0.4, y), (x + len * 0.5, y), 0.1, 0.0);
        assert!((s.cc.slider_value(s.now) - (mid + 0.1)).abs() < 1e-9, "1:1");
        let v = s.cc.slider_value(s.now);
        // 拖动中外部值（自己请求的回声，落后于手指）不拉动滑块；记下的最后一个值按时补发。
        s.status.backlight.as_mut().unwrap().brightness = 0.5;
        s.wait(0.05);
        assert!((s.cc.slider_value(s.now) - v).abs() < 1e-9);
        let sent: Vec<f64> =
            s.cc.take_actions()
                .into_iter()
                .map(|a| match a {
                    SystemAction::Brightness(v) => v,
                    other => panic!("{other:?}"),
                })
                .collect();
        // 0.5 秒的 120Hz 拖动：每帧（1/60 秒）最多一次。
        assert!(sent.len() >= 20 && sent.len() <= 32, "{}", sent.len());
        assert!(sent.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(sent.last(), Some(&v));
        s.up();
        assert!(s.cc.take_actions().is_empty(), "没有待发的值");
        // 刚松手的一小段时间里，回声仍不理会。
        s.wait(0.3);
        assert!((s.cc.slider_value(s.now) - v).abs() < 1e-9);
        // 最后的回声与本地值一致：不动。
        s.status.backlight.as_mut().unwrap().brightness = v;
        s.wait(1.0);
        assert!((s.cc.slider_value(s.now) - v).abs() < 1e-9);
        assert!(s.cc.is_idle(s.now));
    }

    #[test]
    fn external_brightness_change_follows_with_a_spring() {
        let mut s = Sim::new();
        s.open();
        s.status.backlight.as_mut().unwrap().brightness = 0.9;
        s.wait(FRAME);
        let start = s.cc.slider_value(s.now);
        assert!(start < 0.35, "弹簧，不跳：{start}");
        let mut last = start;
        for _ in 0..90 {
            s.wait(FRAME);
            let v = s.cc.slider_value(s.now);
            assert!(v >= last - 1e-9, "单调（临界阻尼）");
            last = v;
        }
        s.wait(1.0);
        assert!((s.cc.slider_value(s.now) - 0.9).abs() < 1e-3);
        // 轻点轨道：跳到该处并立即发送。
        let (x0, x1, _) = s.cc.layout.track();
        let p = s.cc.panel_rect(s.now);
        let (_, y) = s.tile_center(Tile::Brightness);
        s.tap(p.x + x0 + (x1 - x0) * 0.25, y);
        assert_eq!(s.cc.take_actions(), vec![SystemAction::Brightness(0.25)]);
        s.wait(1.0);
        assert!((s.cc.slider_value(s.now) - 0.25).abs() < 1e-3);
    }

    #[test]
    fn closing_flushes_a_pending_brightness() {
        let mut s = Sim::new();
        s.open();
        let (x, y) = s.tile_center(Tile::Brightness);
        s.down(x, y);
        s.drag((x, y), (x + 40.0, y), 0.1, 0.0);
        s.cc.take_actions();
        // 同一帧内又动了一下（被节流），随即锁屏：最后的值也要发出去。
        s.now += 0.001;
        s.cc.touch_motion(0, x + 60.0, y, s.now, &s.status);
        s.cc.close_now(s.now);
        let actions = s.cc.take_actions();
        assert_eq!(actions.len(), 1);
        assert!(!s.cc.is_visible(s.now));
    }

    #[test]
    fn reduced_motion_fades_in_place() {
        let mut s = Sim::new();
        s.cc.reduced_motion = true;
        s.down(1100.0, 4.0);
        s.drag((1100.0, 4.0), (1100.0, 104.0), 0.2, 0.0);
        // 面板停在展开的位置，只是半透明。
        assert_eq!(s.cc.panel_rect(s.now), s.cc.layout.panel);
        let mut scene = Scene::new();
        s.cc.push_scene(&mut scene, true, s.now);
        let p = s.cc.progress(s.now);
        assert!(p > 0.2 && p < 0.4);
        let panel_alpha = scene.iter().find_map(|item| match item {
            DrawItem::Texture {
                key: TexKey::ControlCenter(Part::Panel),
                alpha,
                ..
            } => Some(*alpha),
            _ => None,
        });
        assert_eq!(panel_alpha, Some(p));
    }
}
