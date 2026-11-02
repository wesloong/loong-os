//! 主屏上的已安装 Web 应用（PWA）：与注册表同步、主屏排列、落印入场、收墨移除、长按菜单。
//!
//! - 应用下标在合成器运行期间不变（任务、纹理都按下标索引）：移除的应用只打上
//!   [`AppEntry::removed`] 标记、从网格中拿掉；重新安装时追加一个新条目。
//! - 网格顺序（[`HomeState::grid`]）只含网格中的应用，Dock 固定为内置的 Dock 应用。新安装的应用
//!   追加到末尾；顺序变化时由后端写入 home.toml（[`Shell::take_home_order`]）。
//! - **落印**：新应用在主屏可见时入场——图标从 1.6 倍带回弹压到纸上（`BOUNCY`），盖下的一刻在纸上
//!   晕开一滴墨；它不在当前页时先展卷到那一页。之前它的格子空着、点不中。
//! - **收墨**：移除时图标像墨收回纸中（晕染遮罩逆向收拢，`SMOOTH`），之后后面的图标落位（`BOUNCY`）。
//! - **长按**（0.5 秒、位移 < 8pt，设计规范 §9.2）已安装应用的图标：弹出绢面小菜单“移除应用”，
//!   再确认一次（“移除「名称」？它的数据也会一并删除”）。内置应用没有这个菜单。
//! - 减少动态效果：落印只淡入、收墨只淡出、落位直接就位、不落墨。

use std::collections::BTreeMap;

use loong_motion::{Spring, SpringAnimation};
use loong_ui::theme::{Theme, relative_luminance};
use smithay::desktop::Window;

use super::{
    Align, DrawItem, Gesture, Mode, Scene, Shell, TexKey,
    apps::{self, AppEntry},
    geometry::{RectF, lerp, smoothstep},
    ink::{self, InkLayer},
    layout::{ICON_SHADOW_SPREAD, LABEL_HALO, Layout},
    offset_anim,
};
use crate::registry::{Change, WebApp};

/// 长按的时长（秒）。
pub const LONG_PRESS: f64 = 0.5;
/// 落印：图标从这个倍数压到纸上。
const PRESS_FROM: f64 = 1.6;
/// 落印时盖下的那滴墨（半径占图标边长的比例）。
const PRESS_INK: f64 = 0.72;
/// 落印时那滴墨用的“手指”槽位（加上应用下标，不与真实触摸冲突）。
const PRESS_INK_SLOT: u32 = 0x4C00_0000;
/// 那滴墨“按住”多久再松开（洇开、淡入纸中）。
const PRESS_INK_HOLD: f64 = 0.16;

/// 菜单面板：第一步“移除应用”一行。
pub const MENU_W: f64 = 220.0;
pub const MENU_H: f64 = 60.0;
/// 菜单面板：确认一步。
pub const CONFIRM_W: f64 = 300.0;
pub const CONFIRM_H: f64 = 150.0;
/// 确认一步底部按钮行的高度。
pub const CONFIRM_BUTTONS_H: f64 = 52.0;
/// 菜单纹理四周为墨影留的边（点）。
pub const MENU_SHADOW: f64 = 22.0;
/// 菜单与图标之间的距离（点）；放在图标下方时还要让开名称。
const MENU_GAP: f64 = 12.0;
const MENU_LABEL_ROOM: f64 = 24.0;
/// 菜单离屏幕边缘至少这么远。
const MENU_MARGIN: f64 = 16.0;

/// 菜单的一步。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuStep {
    /// “移除应用”。
    Actions,
    /// “移除「名称」？”+ 取消 / 移除。
    Confirm,
}

/// 菜单上的按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuButton {
    /// 第一步的“移除应用”。
    Remove,
    Cancel,
    /// 确认一步的“移除”。
    ConfirmRemove,
}

/// 菜单此刻的样子（渲染端据此画面板纹理）。
#[derive(Debug, Clone, PartialEq)]
pub struct MenuView {
    pub app: usize,
    pub step: MenuStep,
    /// 面板（不含墨影），屏幕坐标。
    pub rect: RectF,
    /// 按钮（面板内坐标）。
    pub buttons: Vec<(MenuButton, RectF)>,
}

/// 某一步的面板大小。
pub fn menu_size(step: MenuStep) -> (f64, f64) {
    match step {
        MenuStep::Actions => (MENU_W, MENU_H),
        MenuStep::Confirm => (CONFIRM_W, CONFIRM_H),
    }
}

/// 某一步的按钮（面板内坐标）。
pub fn menu_buttons(step: MenuStep) -> Vec<(MenuButton, RectF)> {
    match step {
        MenuStep::Actions => vec![(MenuButton::Remove, RectF::new(0.0, 0.0, MENU_W, MENU_H))],
        MenuStep::Confirm => {
            let y = CONFIRM_H - CONFIRM_BUTTONS_H;
            let half = CONFIRM_W / 2.0;
            vec![
                (
                    MenuButton::Cancel,
                    RectF::new(0.0, y, half, CONFIRM_BUTTONS_H),
                ),
                (
                    MenuButton::ConfirmRemove,
                    RectF::new(half, y, half, CONFIRM_BUTTONS_H),
                ),
            ]
        }
    }
}

/// 用户确认移除的应用：后端结束它的进程（这些窗口所属的客户端）、删除注册表目录与数据目录。
#[derive(Debug, Clone)]
pub struct Uninstall {
    pub id: String,
    pub windows: Vec<Window>,
}

/// 一个正在动的图标。
#[derive(Debug, Clone, Copy)]
struct IconMotion {
    /// 落位：相对排列位置的偏移（点），归零即就位。
    offset: SpringAnimation<[f64; 2]>,
    /// 落印：(缩放 PRESS_FROM → 1，显现 0 → 1)。
    press: Option<(SpringAnimation<f64>, SpringAnimation<f64>)>,
    /// 盖下时的那滴墨已经落过。
    inked: bool,
    /// 那滴墨何时松开（落下后按住一小会儿，墨才晕得开）。
    ink_release: Option<f64>,
    /// 收墨：显现程度 1 → 0，结束后从主屏移除。
    leave: Option<SpringAnimation<f64>>,
}

impl IconMotion {
    fn new(now: f64) -> Self {
        IconMotion {
            offset: SpringAnimation::at_rest(Spring::BOUNCY, [0.0; 2], now)
                .with_rest_thresholds(0.1, 1.0),
            press: None,
            inked: false,
            ink_release: None,
            leave: None,
        }
    }

    fn is_done(&self, now: f64) -> bool {
        self.offset.is_done(now)
            && self.press.is_none()
            && self.leave.is_none()
            && self.ink_release.is_none()
    }
}

/// 菜单状态。
#[derive(Debug, Clone, Copy)]
struct Menu {
    app: usize,
    step: MenuStep,
    /// 图标此刻在屏幕上的位置（打开时记下，菜单期间主屏不滚动）。
    anchor: RectF,
    /// 显现 0..1（出现：`SNAPPY`；消失：只淡出）。
    appear: SpringAnimation<f64>,
    closing: bool,
}

/// 主屏排列与已安装应用的状态（[`Shell`] 的一部分）。
#[derive(Debug, Default)]
pub struct HomeState {
    /// 网格中的应用（下标），按显示顺序。
    grid: Vec<usize>,
    /// 正在动的图标。
    motions: BTreeMap<usize, IconMotion>,
    /// 等主屏可见时落印的新应用。
    waiting: Vec<usize>,
    menu: Option<Menu>,
    /// 按在菜单上的手指：(槽位, 按下位置, 按到的按钮)。
    menu_touch: Option<(u32, (f64, f64), Option<MenuButton>)>,
    order_changed: bool,
    uninstalls: Vec<Uninstall>,
    /// 应用列表的版本：图标、名称、增删变化时递增（渲染端据此重画）。
    revision: u64,
    /// 菜单面板的版本（打开、换步时递增）。
    menu_revision: u64,
}

impl HomeState {
    /// 初始排列：内置应用中不在 Dock 的按列表顺序排进网格。
    pub fn new(apps: &[AppEntry]) -> Self {
        HomeState {
            grid: (0..apps.len()).filter(|&i| !apps[i].in_dock).collect(),
            ..HomeState::default()
        }
    }

    /// 由排列算出布局。
    pub fn layout(&self, apps: &[AppEntry], w: f64, h: f64) -> Layout {
        let dock: Vec<usize> = (0..apps.len())
            .filter(|&i| apps[i].in_dock && !apps[i].removed)
            .collect();
        Layout::arrange(w, h, apps.len(), &dock, &self.grid)
    }
}

impl Shell {
    // ───────────────────────── 与注册表同步 ─────────────────────────

    /// 内置应用的 id（注册表中同名的目录不当作已安装的应用，例如“设置”的 WebKit 数据目录）。
    pub fn builtin_ids(&self) -> Vec<String> {
        self.apps
            .iter()
            .filter(|a| a.web.is_none())
            .map(|a| a.id.clone())
            .collect()
    }

    /// 应用列表的版本（见 [`HomeState`]）。
    pub fn apps_revision(&self) -> u64 {
        self.home_state.revision
    }

    /// 注册表有变化。`command` 生成启动命令；`animate` 为假时（开机）不播放落印与收墨。
    pub fn apply_webapp_changes(
        &mut self,
        changes: Vec<Change>,
        command: &dyn Fn(&WebApp) -> String,
        now: f64,
        animate: bool,
    ) {
        let mut changed = false;
        for change in changes {
            changed |= match change {
                Change::Added(app) => self.add_webapp(&app, command(&app), now, animate),
                Change::Updated(app) => match self.installed_index(&app.id) {
                    Some(i) => {
                        tracing::info!("已更新：{}（{}）", app.name, app.id);
                        self.apps[i] = apps::web_app(&app, command(&app));
                        self.home_state.revision += 1;
                        false
                    }
                    None => self.add_webapp(&app, command(&app), now, animate),
                },
                Change::Removed(id) => self.remove_webapp(&id, now, animate),
            };
        }
        if changed {
            self.relayout(now, animate);
        }
    }

    /// 已安装（且没有被移除）的应用的下标。
    fn installed_index(&self, id: &str) -> Option<usize> {
        self.apps
            .iter()
            .position(|a| a.web.is_some() && !a.removed && a.id == id)
    }

    /// 返回排列是否变了。
    fn add_webapp(&mut self, app: &WebApp, command: String, now: f64, animate: bool) -> bool {
        if let Some(i) = self.installed_index(&app.id) {
            // 正在收墨时又装了回来：先把旧的移除干净。
            if self.is_leaving(i) {
                self.finish_removal(i, now, false);
            } else {
                self.apps[i] = apps::web_app(app, command);
                self.home_state.revision += 1;
                return false;
            }
        }
        tracing::info!("新应用：{}（{}）", app.name, app.id);
        let i = self.apps.len();
        self.apps.push(apps::web_app(app, command));
        self.home_state.grid.push(i);
        self.home_state.order_changed = true;
        self.home_state.revision += 1;
        if animate {
            self.home_state.waiting.push(i);
        }
        true
    }

    /// 注册表里没有了：关闭它的窗口；主屏可见时收墨，否则直接移除。返回排列是否已经变了
    /// （收墨结束时才变）。
    fn remove_webapp(&mut self, id: &str, now: f64, animate: bool) -> bool {
        let Some(i) = self.installed_index(id) else {
            return false;
        };
        if self.is_leaving(i) {
            // 用户在菜单里确认移除的：已经在收墨。
            return false;
        }
        tracing::info!("应用已移除：{id}");
        for task in self.tasks.iter().filter(|t| t.app == i) {
            task.request_close();
        }
        if animate && self.icon_on_screen(i) && self.home_presentable(now, true) {
            self.start_leave(i, now);
            return false;
        }
        self.finish_removal(i, now, false);
        true
    }

    /// 开机时（或测试中）按 home.toml 排列网格：文件里的顺序在前，其余按现在的顺序跟在后面。
    pub fn set_home_order(&mut self, ids: &[String], now: f64) {
        let current = std::mem::take(&mut self.home_state.grid);
        let mut grid = Vec::with_capacity(current.len());
        for id in ids {
            if let Some(&i) = current.iter().find(|&&i| self.apps[i].id == *id)
                && !grid.contains(&i)
            {
                grid.push(i);
            }
        }
        grid.extend(
            current
                .iter()
                .filter(|i| !grid.contains(i))
                .collect::<Vec<_>>(),
        );
        self.home_state.grid = grid;
        self.relayout(now, false);
    }

    /// 网格顺序变了（安装、移除）：返回新的顺序（应用 id），由后端写入 home.toml。
    pub fn take_home_order(&mut self) -> Option<Vec<String>> {
        std::mem::take(&mut self.home_state.order_changed).then(|| {
            self.home_state
                .grid
                .iter()
                .map(|&i| self.apps[i].id.clone())
                .collect()
        })
    }

    /// 取出用户确认移除的应用（由后端结束进程、删除目录）。
    pub fn take_uninstalls(&mut self) -> Vec<Uninstall> {
        std::mem::take(&mut self.home_state.uninstalls)
    }

    /// 排列变化后重新计算布局。`animate` 时位置变了的图标从旧位置落位到新位置。
    fn relayout(&mut self, now: f64, animate: bool) {
        let new = self.home_state.layout(&self.apps, self.w, self.h);
        if animate && !self.reduced_motion {
            let old = &self.layout;
            for i in 0..new.icons.len().min(old.icons.len()) {
                if !(old.shown[i] && new.shown[i]) || old.icons[i] == new.icons[i] {
                    continue;
                }
                let motion = self
                    .home_state
                    .motions
                    .entry(i)
                    .or_insert_with(|| IconMotion::new(now));
                let [ox, oy] = motion.offset.value(now);
                let from = [
                    old.icons[i].x + ox - new.icons[i].x,
                    old.icons[i].y + oy - new.icons[i].y,
                ];
                let velocity = motion.offset.velocity(now);
                motion.offset = SpringAnimation::new(Spring::BOUNCY, from, [0.0; 2], velocity, now)
                    .with_rest_thresholds(0.1, 1.0);
            }
        }
        self.layout = new;
        // 页数变少：停在已经不存在的页上时滚回最后一页。
        let max = self.layout.max_scroll();
        if self.home_scroll.target() > max + 0.5 {
            let (v, vel) = (self.home_scroll.value(now), self.home_scroll.velocity(now));
            self.home_scroll = offset_anim(Spring::SMOOTH, v, max, vel, now);
        }
        self.home_state.revision += 1;
    }

    // ───────────────────────── 每帧 ─────────────────────────

    /// 主屏是否“摆在用户眼前”：没有应用、多任务、锁屏挡着，没有手势、菜单，主屏也已经迎上来。
    /// `ignore_gesture` 时不看手势（判断移除时是否值得播放收墨）。
    fn home_presentable(&self, now: f64, ignore_gesture: bool) -> bool {
        self.mode == Mode::Home
            && (ignore_gesture || matches!(self.gesture, Gesture::Idle))
            && !self.lock.is_locked()
            && (!self.lock.is_visible(now) || self.lock.progress(now) >= 0.999)
            && self.tasks.iter().all(|t| !t.homing)
            && self.backdrop.target() <= 0.0
            && self.backdrop.value(now) < 0.05
            && self.home.value(now)[0] < 1.01
            && self.home_state.menu.is_none()
    }

    /// 第 `i` 个图标此刻是否在屏幕上（停稳后的这一页或 Dock）。
    fn icon_on_screen(&self, i: usize) -> bool {
        if i >= self.layout.icons.len() || !self.layout.shown[i] {
            return false;
        }
        let r = self.layout.icon_at(i, self.home_scroll.target());
        r.x + r.w > 0.0 && r.x < self.w
    }

    /// 每帧：长按、落印、收墨、落位、菜单的淡出。
    pub(super) fn update_home(&mut self, now: f64) {
        // 长按已安装应用的图标：弹出菜单，这次触摸不再打开应用。
        if let Gesture::Home {
            slot,
            t0,
            icon: Some(app),
            moved: false,
            ..
        } = self.gesture
            && now - t0 >= LONG_PRESS
            && self.apps[app].web.is_some()
            && !self.is_leaving(app)
            && self.home_state.menu.is_none()
        {
            if let Gesture::Home { icon, .. } = &mut self.gesture {
                *icon = None;
            }
            self.ink.release(slot, now);
            self.open_menu(app, now);
        }
        // 菜单只在主屏上：应用来到前台（例如被别的程序打开）时直接收起。
        if self.mode != Mode::Home && self.home_state.menu.is_some() {
            self.home_state.menu = None;
            self.home_state.menu_touch = None;
        }
        if let Some(menu) = &self.home_state.menu
            && menu.closing
            && menu.appear.is_done(now)
        {
            self.home_state.menu = None;
        }

        self.start_waiting_entrances(now);

        // 落印盖下的一刻：纸上晕开一滴墨（按住一小会儿再松开，让它洇开、淡入纸中）。
        let mut drops = Vec::new();
        let mut releases = Vec::new();
        let mut finished = Vec::new();
        for (&i, m) in self.home_state.motions.iter_mut() {
            if m.ink_release.is_some_and(|t| now >= t) {
                m.ink_release = None;
                releases.push(i);
            }
            if let Some((scale, fade)) = &m.press {
                if !m.inked && scale.value(now) <= 1.0 {
                    m.inked = true;
                    m.ink_release = Some(now + PRESS_INK_HOLD);
                    drops.push(i);
                }
                if scale.is_done(now) && fade.is_done(now) {
                    m.press = None;
                }
            }
            if let Some(leave) = &m.leave
                && leave.is_done(now)
            {
                finished.push(i);
            }
        }
        for i in drops {
            if i < self.layout.icons.len() && !self.reduced_motion {
                let (cx, cy) = self.layout.icons[i].center();
                let r = self.layout.icon * PRESS_INK;
                let scrolls = self.layout.page_of[i].is_some();
                let slot = PRESS_INK_SLOT + i as u32;
                self.ink
                    .press(slot, InkLayer::Home { scrolls }, cx, cy, r, now);
            }
        }
        for i in releases {
            self.ink.release(PRESS_INK_SLOT + i as u32, now);
        }
        for i in finished {
            self.finish_removal(i, now, true);
        }
        self.home_state.motions.retain(|_, m| !m.is_done(now));
    }

    /// 等待中的新应用：主屏摆在眼前时，先展卷到第一个新图标所在的页，停稳后一起落印。
    fn start_waiting_entrances(&mut self, now: f64) {
        let Some(&first) = self.home_state.waiting.first() else {
            return;
        };
        if !self.home_presentable(now, false) {
            return;
        }
        let page = self
            .layout
            .page_of
            .get(first)
            .copied()
            .flatten()
            .unwrap_or(0);
        let target = page as f64 * self.layout.page_width;
        if (self.home_scroll.target() - target).abs() > 0.5 {
            let (v, vel) = (self.home_scroll.value(now), self.home_scroll.velocity(now));
            self.home_scroll = offset_anim(Spring::SMOOTH, v, target, vel, now);
            return;
        }
        if (self.home_scroll.value(now) - target).abs() > 2.0 {
            return;
        }
        for i in std::mem::take(&mut self.home_state.waiting) {
            let reduced = self.reduced_motion;
            let motion = self
                .home_state
                .motions
                .entry(i)
                .or_insert_with(|| IconMotion::new(now));
            let (from, fade_spring) = if reduced {
                (1.0, Spring::SMOOTH)
            } else {
                (PRESS_FROM, Spring::SNAPPY)
            };
            motion.press = Some((
                SpringAnimation::new(Spring::BOUNCY, from, 1.0, 0.0, now),
                SpringAnimation::new(fade_spring, 0.0, 1.0, 0.0, now),
            ));
            motion.inked = reduced;
        }
    }

    /// 是否正在收墨（或已排定移除）。
    fn is_leaving(&self, i: usize) -> bool {
        self.home_state
            .motions
            .get(&i)
            .is_some_and(|m| m.leave.is_some())
    }

    /// 图标能不能点：等待落印、正在收墨的不能。
    pub(super) fn icon_tappable(&self, i: usize) -> bool {
        !self.home_state.waiting.contains(&i) && !self.is_leaving(i)
    }

    fn start_leave(&mut self, i: usize, now: f64) {
        self.home_state.waiting.retain(|&w| w != i);
        let motion = self
            .home_state
            .motions
            .entry(i)
            .or_insert_with(|| IconMotion::new(now));
        motion.press = None;
        motion.leave = Some(SpringAnimation::new(Spring::SMOOTH, 1.0, 0.0, 0.0, now));
    }

    /// 从主屏拿掉第 `i` 个应用（下标保留）。`animate` 时其余图标落位。
    fn finish_removal(&mut self, i: usize, now: f64, animate: bool) {
        let entry = &mut self.apps[i];
        entry.removed = true;
        entry.command = None;
        self.home_state.grid.retain(|&g| g != i);
        self.home_state.waiting.retain(|&w| w != i);
        // 落印的那滴墨还按着：松开它（否则它一直留在纸上，主屏也一直不静止）。
        if self
            .home_state
            .motions
            .remove(&i)
            .is_some_and(|m| m.ink_release.is_some())
        {
            self.ink.release(PRESS_INK_SLOT + i as u32, now);
        }
        if self.home_state.menu.is_some_and(|m| m.app == i) {
            self.home_state.menu = None;
        }
        self.home_state.order_changed = true;
        self.relayout(now, animate);
    }

    /// 动画是否都已停止（菜单打开且静止时也算）。
    pub(super) fn home_is_idle(&self, now: f64) -> bool {
        let entrance_pending =
            !self.home_state.waiting.is_empty() && self.home_presentable(now, false);
        self.home_state.motions.values().all(|m| m.is_done(now))
            && !entrance_pending
            && self
                .home_state
                .menu
                .is_none_or(|m| !m.closing && m.appear.is_done(now))
    }

    // ───────────────────────── 长按菜单 ─────────────────────────

    fn open_menu(&mut self, app: usize, now: f64) {
        let anchor = self.layout.icon_at(app, self.home_scroll.value(now));
        self.home_state.menu = Some(Menu {
            app,
            step: MenuStep::Actions,
            anchor,
            appear: SpringAnimation::new(Spring::SNAPPY, 0.0, 1.0, 0.0, now),
            closing: false,
        });
        self.home_state.menu_revision += 1;
    }

    fn close_menu(&mut self, now: f64) {
        if let Some(menu) = &mut self.home_state.menu
            && !menu.closing
        {
            menu.closing = true;
            menu.appear.set_spring(Spring::SMOOTH, now);
            menu.appear.retarget(0.0, now);
        }
    }

    /// 菜单面板的位置：放在图标下方（让开名称），下方放不下（Dock 附近）就放在上方；两步共用
    /// 同一侧，换步时面板不跳到另一侧。
    fn menu_rect(&self, anchor: RectF, step: MenuStep) -> RectF {
        let (w, h) = menu_size(step);
        let (_, tallest) = menu_size(MenuStep::Confirm);
        let (cx, _) = anchor.center();
        let x = (cx - w / 2.0).clamp(MENU_MARGIN, self.w - MENU_MARGIN - w);
        let below = anchor.y + anchor.h + MENU_LABEL_ROOM + MENU_GAP;
        let limit = self.layout.dock.y - MENU_GAP;
        let y = if below + tallest <= limit {
            below
        } else {
            (anchor.y - MENU_GAP - h).max(super::layout::STATUS_BAR_HEIGHT + MENU_GAP)
        };
        RectF::new(x, y, w, h)
    }

    /// 菜单此刻的样子（没有菜单时为 `None`）。
    pub fn home_menu(&self) -> Option<MenuView> {
        let m = self.home_state.menu?;
        Some(MenuView {
            app: m.app,
            step: m.step,
            rect: self.menu_rect(m.anchor, m.step),
            buttons: menu_buttons(m.step),
        })
    }

    /// 菜单面板的版本（见 [`HomeState`]）。
    pub fn menu_revision(&self) -> u64 {
        self.home_state.menu_revision
    }

    /// 菜单上被按到的按钮（屏幕坐标）。
    fn menu_hit(&self, x: f64, y: f64) -> (bool, Option<MenuButton>) {
        let Some(view) = self.home_menu() else {
            return (false, None);
        };
        let inside = view.rect.contains(x, y);
        let (lx, ly) = (x - view.rect.x, y - view.rect.y);
        let button = view
            .buttons
            .iter()
            .find(|(_, r)| r.contains(lx, ly))
            .map(|(b, _)| *b);
        (inside, button)
    }

    /// 菜单打开时的触摸：按在面板外收起菜单（这次触摸不再做别的）；按在按钮上落一滴墨，
    /// 抬起时仍在同一个按钮上才算点按。返回是否由菜单处理。
    pub(super) fn menu_touch_down(&mut self, slot: u32, x: f64, y: f64, now: f64) -> bool {
        let Some(menu) = self.home_state.menu else {
            return false;
        };
        if menu.closing || self.home_state.menu_touch.is_some() {
            return true;
        }
        let (inside, button) = self.menu_hit(x, y);
        if !inside {
            self.close_menu(now);
            return true;
        }
        if button.is_some() {
            self.ink.press(slot, InkLayer::Overlay, x, y, 26.0, now);
        }
        self.home_state.menu_touch = Some((slot, (x, y), button));
        true
    }

    pub(super) fn menu_touch_motion(&mut self, slot: u32, x: f64, y: f64, now: f64) -> bool {
        let Some((s, start, button)) = self.home_state.menu_touch else {
            return false;
        };
        if s != slot {
            return false;
        }
        if button.is_some() && (x - start.0).hypot(y - start.1) > super::TAP_SLOP {
            self.home_state.menu_touch = Some((s, start, None));
            self.ink.release(slot, now);
        }
        true
    }

    pub(super) fn menu_touch_up(&mut self, slot: u32, now: f64) -> bool {
        match self.home_state.menu_touch {
            Some((s, _, button)) if s == slot => {
                self.home_state.menu_touch = None;
                self.ink.release(slot, now);
                if let Some(b) = button {
                    self.menu_action(b, now);
                }
                true
            }
            // 菜单刚被面板外的触摸收起：这根手指抬起也由菜单吞掉。
            _ => self.home_state.menu.is_some() && matches!(self.gesture, Gesture::Idle),
        }
    }

    fn menu_action(&mut self, button: MenuButton, now: f64) {
        let Some(menu) = &mut self.home_state.menu else {
            return;
        };
        match button {
            MenuButton::Remove => {
                menu.step = MenuStep::Confirm;
                self.home_state.menu_revision += 1;
            }
            MenuButton::Cancel => self.close_menu(now),
            MenuButton::ConfirmRemove => {
                let app = menu.app;
                self.close_menu(now);
                self.begin_uninstall(app, now);
            }
        }
    }

    /// 用户确认移除：关闭它的窗口、交给后端卸载，图标收墨。
    fn begin_uninstall(&mut self, app: usize, now: f64) {
        if self.apps[app].web.is_none() || self.apps[app].removed {
            return;
        }
        let mut windows = Vec::new();
        for task in self.tasks.iter().filter(|t| t.app == app) {
            task.request_close();
            windows.extend(task.window.clone());
        }
        // 主屏上它的任务都在后台：直接从任务列表（多任务）里拿掉。
        self.tasks.retain(|t| t.app != app);
        tracing::info!("移除应用：{}", self.apps[app].id);
        self.home_state.uninstalls.push(Uninstall {
            id: self.apps[app].id.clone(),
            windows,
        });
        self.start_leave(app, now);
    }

    // ───────────────────────── 画面 ─────────────────────────

    /// 网格图标的墨影是否已经画在预先合成的整页山水里（内置应用是；已安装的应用随时可能
    /// 增删、移动，墨影单独画）。渲染端合成整页时用同一个判断。
    pub fn shadow_baked(&self, i: usize) -> bool {
        self.apps.get(i).is_some_and(|a| a.web.is_none())
    }

    /// 图标是否由 [`Shell::push_home_extras`] 画（落印、收墨中，或被菜单“托起”）。
    pub(super) fn icon_in_overlay(&self, i: usize) -> bool {
        self.home_state.waiting.contains(&i)
            || self
                .home_state
                .motions
                .get(&i)
                .is_some_and(|m| m.press.is_some() || m.leave.is_some())
            || self.home_state.menu.is_some_and(|m| m.app == i)
    }

    /// 落位中的图标相对排列位置的偏移。
    pub(super) fn icon_offset(&self, i: usize, now: f64) -> (f64, f64) {
        self.home_state.motions.get(&i).map_or((0.0, 0.0), |m| {
            let [x, y] = m.offset.value(now);
            (x, y)
        })
    }

    /// 应用卡片（启动闪屏）的纸色：已安装的应用用 manifest 的 `background_color`——暗色外观下只用
    /// 深色的背景色，避免夜里闪一下白（loong-webapp 的首帧底色用同一规则）。
    pub(super) fn card_paper(&self, app: usize) -> u32 {
        let paper = self.theme.palette().paper_raised;
        match self.apps[app].web.as_ref().and_then(|w| w.background_color) {
            Some(bg) if self.theme == Theme::Light || relative_luminance(bg) < 0.2 => bg,
            _ => paper,
        }
    }

    /// 一个图标（含墨影与名称）。`scale` 以图标中心缩放；`reveal` 进度 < 1 时收墨遮罩。
    #[allow(clippy::too_many_arguments)]
    fn push_icon(
        &self,
        scene: &mut Scene,
        i: usize,
        rect: RectF,
        zoom: f64,
        scale: f64,
        alpha: f64,
        shadow_alpha: f64,
        reveal: Option<f64>,
    ) {
        let (cx, cy) = rect.center();
        let r = RectF::centered(cx, cy, rect.w * scale, rect.h * scale);
        if shadow_alpha > 0.001 {
            let s = r.w * ICON_SHADOW_SPREAD;
            scene.push(DrawItem::Texture {
                key: TexKey::IconShadow,
                rect: RectF::new(r.x - s, r.y - s, r.w + 2.0 * s, r.h + 2.0 * s),
                alpha: shadow_alpha,
                radius: 0.0,
                clip: None,
                reveal: None,
            });
        }
        let seed = 3.0 + (apps::id_seed(&self.apps[i].id) % 97) as f32;
        scene.push(DrawItem::Texture {
            key: TexKey::Icon(i),
            rect: r,
            alpha,
            radius: 0.0,
            clip: None,
            reveal: reveal.and_then(|p| ink::spread_reveal(r, p, seed)),
        });
        if self.layout.labeled[i] {
            scene.push(DrawItem::Natural {
                key: TexKey::Label(i),
                x: rect.x + rect.w / 2.0,
                y: rect.y + rect.h + (4.0 - LABEL_HALO) * zoom,
                align: Align::Center,
                alpha: alpha.min(reveal.unwrap_or(1.0)),
                scale: zoom,
            });
        }
    }

    /// 主屏上叠在普通图标之上的部分：落印、收墨中的图标，菜单打开时的薄墨罩、被托起的图标与菜单面板。
    /// 坐标随主屏缩放（`zoom`，以屏幕中心）与翻页（`scroll`）。
    pub(super) fn push_home_extras(&self, scene: &mut Scene, zoom: f64, scroll: f64, now: f64) {
        let (cx, cy) = (self.w / 2.0, self.h / 2.0);
        for (&i, m) in &self.home_state.motions {
            if i >= self.layout.icons.len() || !self.layout.shown[i] {
                continue;
            }
            let (ox, oy) = self.icon_offset(i, now);
            let base = self.layout.icon_at(i, scroll);
            let rect =
                RectF::new(base.x + ox, base.y + oy, base.w, base.h).scaled_about(cx, cy, zoom);
            if let Some((scale, fade)) = &m.press {
                let s = scale.value(now).max(0.0);
                let a = fade.value(now).clamp(0.0, 1.0);
                // 墨影随图标贴近纸面而显出来。
                let shadow = a * smoothstep(PRESS_FROM - 0.1, 1.0, s);
                self.push_icon(scene, i, rect, zoom, s, a, shadow, None);
            } else if let Some(leave) = &m.leave {
                let p = leave.value(now).clamp(0.0, 1.0);
                if self.reduced_motion {
                    self.push_icon(scene, i, rect, zoom, 1.0, p, p, None);
                } else {
                    // 收墨：遮罩从四周收向中心，最后一点墨淡入纸中。
                    let a = smoothstep(0.0, 0.25, p);
                    let s = lerp(0.88, 1.0, p);
                    self.push_icon(scene, i, rect, zoom, s, a, p, Some(p));
                }
            }
        }
        let Some(menu) = &self.home_state.menu else {
            return;
        };
        let a = menu.appear.value(now).clamp(0.0, 1.0);
        if a <= 0.001 {
            return;
        }
        // 薄墨罩：主屏退后一点，菜单与它的图标浮在上面。
        let veil = match self.theme {
            Theme::Light => self.ink_rgba(0, 0.16 * a),
            Theme::Dark => [0.0, 0.0, 0.0, (0.32 * a) as f32],
        };
        scene.push(DrawItem::Solid {
            rect: RectF::new(0.0, 0.0, self.w, self.h),
            color: veil,
            radius: 0.0,
        });
        // 被托起的图标（确认移除后它在上面收墨，不再托起）。
        let i = menu.app;
        if i < self.layout.icons.len() && self.layout.shown[i] && !self.is_leaving(i) {
            let base = self.layout.icon_at(i, scroll).scaled_about(cx, cy, zoom);
            let lift = if self.reduced_motion {
                1.0
            } else {
                lerp(1.0, 1.06, a)
            };
            self.push_icon(scene, i, base, zoom, lift, 1.0, 1.0, None);
        }
        let rect = self.menu_rect(menu.anchor, menu.step);
        // 出现时从 1.08 倍缩到 1（SNAPPY），消失时只淡出（设计规范 §7.7 弹窗）。
        let s = if menu.closing || self.reduced_motion {
            1.0
        } else {
            lerp(1.08, 1.0, a)
        };
        let full = RectF::new(
            rect.x - MENU_SHADOW,
            rect.y - MENU_SHADOW,
            rect.w + 2.0 * MENU_SHADOW,
            rect.h + 2.0 * MENU_SHADOW,
        );
        let (mx, my) = rect.center();
        scene.push(DrawItem::Texture {
            key: TexKey::HomeMenu,
            rect: full.scaled_about(mx, my, s).scaled_about(cx, cy, zoom),
            alpha: a,
            radius: 0.0,
            clip: None,
            reveal: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::parse_manifest;

    const W: f64 = 1280.0;
    const H: f64 = 800.0;
    const FRAME: f64 = 1.0 / 120.0;

    struct Sim {
        shell: Shell,
        now: f64,
    }

    impl Sim {
        fn new() -> Self {
            Sim {
                shell: Shell::new(W, H, 18.0),
                now: 0.0,
            }
        }

        fn wait(&mut self, secs: f64) {
            let end = self.now + secs;
            while self.now < end - 1e-9 {
                self.now = (self.now + FRAME).min(end);
                self.shell.update(self.now);
            }
        }

        /// 底边快速上甩回主屏。
        fn flick_home(&mut self) {
            self.shell.touch_down(0, 640.0, 795.0, self.now);
            for k in 1..=15 {
                self.now += 0.008;
                self.shell
                    .touch_motion(0, 640.0, 795.0 - 20.0 * k as f64, self.now);
                self.shell.update(self.now);
            }
            self.shell.touch_up(0, self.now);
        }

        fn press(&mut self, x: f64, y: f64, hold: f64) {
            self.shell.touch_down(0, x, y, self.now);
            self.wait(hold);
            self.shell.touch_up(0, self.now);
        }

        fn apply(&mut self, changes: Vec<Change>, animate: bool) {
            self.shell.apply_webapp_changes(
                changes,
                &|a| format!("run {}", a.id),
                self.now,
                animate,
            );
        }

        fn index(&self, id: &str) -> usize {
            self.shell
                .apps
                .iter()
                .position(|a| a.id == id && !a.removed)
                .unwrap()
        }

        fn icon_center(&self, i: usize) -> (f64, f64) {
            self.shell
                .layout
                .icon_at(i, self.shell.home_scroll.value(self.now))
                .center()
        }
    }

    fn webapp(id: &str, name: &str) -> WebApp {
        parse_manifest(
            id,
            &format!(r##"{{"name": "{name}", "start_url": "https://{id}.example/", "theme_color": "#2e5e74"}}"##),
        )
        .unwrap()
    }

    fn count_icon_items(scene: &Scene, i: usize) -> usize {
        scene
            .iter()
            .filter(|d| matches!(d, DrawItem::Texture { key: TexKey::Icon(k), .. } if *k == i))
            .count()
    }

    #[test]
    fn startup_apps_are_appended_without_animation() {
        let mut s = Sim::new();
        let builtin = s.shell.apps.len();
        assert_eq!(s.shell.layout.pages, 2);
        let first = (0..builtin).find(|&i| !s.shell.apps[i].in_dock).unwrap();
        let slot0 = s.shell.layout.icons[first];
        s.apply(
            vec![
                Change::Added(webapp("a", "甲")),
                Change::Added(webapp("b", "乙")),
            ],
            false,
        );
        let (a, b) = (s.index("a"), s.index("b"));
        assert_eq!((a, b), (builtin, builtin + 1));
        // 排在网格末尾（第二页，演示应用之后），不在 Dock。
        assert_eq!(s.shell.layout.page_of[a], Some(1));
        assert!(s.shell.layout.icons[a].x < s.shell.layout.icons[b].x);
        assert!(s.shell.home_is_idle(s.now));
        assert_eq!(s.shell.apps[a].command.as_deref(), Some("run a"));
        // home.toml：顺序变化交给后端写入，只交一次。
        let order = s.shell.take_home_order().unwrap();
        assert_eq!(&order[order.len() - 2..], ["a", "b"]);
        assert!(s.shell.take_home_order().is_none());
        // 排列按 home.toml：b 排到最前。
        s.shell.set_home_order(&["b".into()], s.now);
        assert_eq!(s.shell.layout.page_of[b], Some(0));
        assert_eq!(s.shell.layout.icons[b], slot0);
    }

    #[test]
    fn new_app_waits_for_home_then_scrolls_and_presses() {
        let mut s = Sim::new();
        // 打开一个演示应用：主屏不可见。
        let music = s.shell.apps.iter().position(|a| a.id == "music").unwrap();
        s.shell.launch(music, s.now);
        s.wait(1.0);
        s.apply(vec![Change::Added(webapp("new-app", "新"))], true);
        let i = s.index("new-app");
        s.wait(0.5);
        assert!(s.shell.icon_in_overlay(i), "等待落印");
        assert!(!s.shell.icon_tappable(i));
        assert!(s.shell.is_idle(s.now), "应用在前台时不为等待中的入场重画");
        // 回主屏：卡片飞回后展卷到第二页，再落印。
        s.flick_home();
        s.wait(3.0);
        assert_eq!(s.shell.home_scroll.value(s.now), W, "展卷到新图标所在的页");
        assert!(s.shell.home_state.waiting.is_empty());
        assert!(s.shell.icon_tappable(i));
        assert!(s.shell.is_idle(s.now));
    }

    #[test]
    fn press_animation_scales_down_with_an_ink_drop() {
        let mut s = Sim::new();
        s.apply(vec![Change::Added(webapp("x", "X"))], true);
        let i = s.index("x");
        let r = s.shell.layout.icons[i];
        let mut max_w: f64 = 0.0;
        let mut min_w = f64::MAX;
        let mut inked = false;
        for _ in 0..360 {
            s.wait(FRAME);
            let scene = s.shell.scene(s.now);
            for d in &scene {
                match d {
                    DrawItem::Texture {
                        key: TexKey::Icon(k),
                        rect,
                        ..
                    } if *k == i => {
                        max_w = max_w.max(rect.w);
                        min_w = min_w.min(rect.w);
                    }
                    DrawItem::Ink(_) => inked = true,
                    _ => {}
                }
            }
        }
        assert!(max_w > r.w * 1.3, "从大处落下：{max_w}");
        assert!(min_w < r.w, "带一点回弹（压进纸里）：{min_w}");
        assert!(inked, "盖下时落一滴墨");
        s.wait(2.0);
        assert!(s.shell.is_idle(s.now));
        assert!(!s.shell.icon_in_overlay(i));
        let scene = s.shell.scene(s.now);
        assert_eq!(count_icon_items(&scene, i), 1);
    }

    #[test]
    fn reduced_motion_only_fades() {
        let mut s = Sim::new();
        s.shell.set_reduced_motion(true);
        s.apply(vec![Change::Added(webapp("x", "X"))], true);
        let i = s.index("x");
        let r = s.shell.layout.icons[i];
        // 不在当前页：减少动态效果时仍然翻到那一页（那是功能，不是装饰）。
        for _ in 0..200 {
            s.wait(FRAME);
            for d in s.shell.scene(s.now) {
                if let DrawItem::Texture {
                    key: TexKey::Icon(k),
                    rect,
                    ..
                } = d
                    && k == i
                {
                    assert!((rect.w - r.w).abs() < 1e-6, "不缩放");
                }
                assert!(!matches!(d, DrawItem::Ink(_)), "不落墨");
            }
        }
    }

    #[test]
    fn long_press_opens_menu_and_confirmed_removal_uninstalls() {
        let mut s = Sim::new();
        s.apply(
            vec![
                Change::Added(webapp("x", "X")),
                Change::Added(webapp("y", "Y")),
            ],
            false,
        );
        let (x, y) = (s.index("x"), s.index("y"));
        s.shell.take_home_order();
        // 到第二页。
        s.shell.home_scroll = offset_anim(Spring::SNAPPY, W, W, 0.0, s.now);
        let (px, py) = s.icon_center(x);
        let x_slot = s.shell.layout.icons[x];

        // 内置应用长按没有菜单，抬起也不打开（超过点按时长）。
        let photos = s.shell.apps.iter().position(|a| a.id == "photos").unwrap();
        assert!(s.shell.apps[photos].web.is_none());

        // 短按打开应用（对照）：这里只检查长按。
        s.shell.touch_down(0, px, py, s.now);
        s.wait(0.3);
        assert!(s.shell.home_menu().is_none(), "0.3 秒还不是长按");
        s.wait(0.3);
        let menu = s.shell.home_menu().expect("长按 0.5 秒弹出菜单");
        assert_eq!((menu.app, menu.step), (x, MenuStep::Actions));
        s.shell.touch_up(0, s.now);
        s.wait(1.0);
        assert!(s.shell.running_apps().is_empty(), "长按后抬起不打开应用");
        assert!(s.shell.is_idle(s.now), "菜单静止时不重画");

        // 点“移除应用” → 确认一步。
        let view = s.shell.home_menu().unwrap();
        let b = view.buttons[0].1;
        s.press(
            view.rect.x + b.x + b.w / 2.0,
            view.rect.y + b.y + b.h / 2.0,
            0.06,
        );
        let actions = view.rect;
        let view = s.shell.home_menu().unwrap();
        assert_eq!(view.step, MenuStep::Confirm);
        // 两步在图标的同一侧：放在下方时顶边对齐，放在上方时底边对齐。
        assert!(
            view.rect.y == actions.y || view.rect.y + view.rect.h == actions.y + actions.h,
            "{actions:?} {:?}",
            view.rect
        );
        // 取消：菜单收起，什么都没删。
        let cancel = view
            .buttons
            .iter()
            .find(|(b, _)| *b == MenuButton::Cancel)
            .unwrap()
            .1;
        s.press(
            view.rect.x + cancel.x + 10.0,
            view.rect.y + cancel.y + 10.0,
            0.06,
        );
        s.wait(1.0);
        assert!(s.shell.home_menu().is_none());
        assert!(s.shell.take_uninstalls().is_empty());

        // 再来一次，确认移除。
        s.press(px, py, 0.7);
        let view = s.shell.home_menu().unwrap();
        let b = view.buttons[0].1;
        s.press(view.rect.x + b.x + 5.0, view.rect.y + b.y + 5.0, 0.06);
        let view = s.shell.home_menu().unwrap();
        let ok = view
            .buttons
            .iter()
            .find(|(b, _)| *b == MenuButton::ConfirmRemove)
            .unwrap()
            .1;
        s.press(view.rect.x + ok.x + 10.0, view.rect.y + ok.y + 10.0, 0.06);
        let un = s.shell.take_uninstalls();
        assert_eq!(un.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(), ["x"]);
        // 收墨中：图标带遮罩，点不中。
        s.wait(0.2);
        assert!(!s.shell.icon_tappable(x));
        // 菜单还在淡出：图标只画一次（收墨的那个），不再被“托起”盖住。
        let scene = s.shell.scene(s.now);
        assert!(s.shell.home_menu().is_some(), "菜单正在淡出");
        assert_eq!(count_icon_items(&scene, x), 1);
        assert!(scene.iter().any(|d| matches!(d,
            DrawItem::Texture { key: TexKey::Icon(k), reveal: Some(_), .. } if *k == x)));
        s.wait(2.5);
        assert!(s.shell.apps[x].removed);
        assert!(!s.shell.layout.shown[x]);
        // 后面的图标落位到它的格子。
        assert_eq!(s.shell.layout.icons[y], x_slot);
        let order = s.shell.take_home_order().unwrap();
        assert!(!order.contains(&"x".to_string()) && order.contains(&"y".to_string()));
        assert!(s.shell.is_idle(s.now));
        // 注册表随后报告移除：已经处理过，不再变化。
        let rev = s.shell.apps_revision();
        s.apply(vec![Change::Removed("x".into())], true);
        assert_eq!(s.shell.apps_revision(), rev);
        // 重新安装同一个 id：追加新条目。
        s.apply(vec![Change::Added(webapp("x", "X"))], false);
        assert_ne!(s.index("x"), x);
    }

    #[test]
    fn builtin_apps_have_no_menu_and_tap_outside_closes() {
        let mut s = Sim::new();
        let photos = s.shell.apps.iter().position(|a| a.id == "photos").unwrap();
        let (px, py) = s.icon_center(photos);
        s.shell.touch_down(0, px, py, s.now);
        s.wait(0.9);
        assert!(s.shell.home_menu().is_none());
        s.shell.touch_up(0, s.now);
        s.wait(1.0);
        assert!(s.shell.running_apps().is_empty(), "按太久不算点按");

        s.apply(vec![Change::Added(webapp("x", "X"))], false);
        s.shell.home_scroll = offset_anim(Spring::SNAPPY, W, W, 0.0, s.now);
        let x = s.index("x");
        let (px, py) = s.icon_center(x);
        s.press(px, py, 0.6);
        assert!(s.shell.home_menu().is_some());
        // 面板外点一下：菜单收起，不翻页、不打开应用。
        s.press(40.0, 300.0, 0.06);
        s.wait(1.0);
        assert!(s.shell.home_menu().is_none());
        assert!(s.shell.running_apps().is_empty());
        assert!(s.shell.is_idle(s.now));
    }

    #[test]
    fn updates_and_external_removal() {
        let mut s = Sim::new();
        s.apply(vec![Change::Added(webapp("x", "X"))], false);
        let x = s.index("x");
        let rev = s.shell.apps_revision();
        let mut updated = webapp("x", "新名字");
        updated.short_name = "新".into();
        s.apply(vec![Change::Updated(updated)], true);
        assert_eq!(s.index("x"), x);
        assert_eq!(s.shell.apps[x].name, "新");
        assert!(s.shell.apps_revision() > rev);
        // 不在当前页时直接移除，不播放收墨。
        s.apply(vec![Change::Removed("x".into())], true);
        assert!(s.shell.apps[x].removed);
        assert!(s.shell.is_idle(s.now));
    }

    #[test]
    fn splash_uses_background_color() {
        let mut s = Sim::new();
        let mut a = webapp("x", "X");
        a.background_color = Some(0xFFFFFF);
        let mut b = webapp("y", "Y");
        b.background_color = Some(0x101820);
        s.apply(vec![Change::Added(a), Change::Added(b)], false);
        let (x, y) = (s.index("x"), s.index("y"));
        assert_eq!(s.shell.card_paper(x), 0xFFFFFF);
        s.shell.set_theme(Theme::Dark);
        assert_eq!(s.shell.card_paper(x), Theme::Dark.palette().paper_raised);
        assert_eq!(s.shell.card_paper(y), 0x101820);
    }

    #[test]
    fn many_apps_add_pages() {
        let mut s = Sim::new();
        assert_eq!(s.shell.layout.pages, 2);
        // 第二页已有 3 个演示应用：再装 22 个排满第二页（24 个），第 22 个开出第三页。
        let apps: Vec<Change> = (0..22)
            .map(|k| Change::Added(webapp(&format!("app-{k}"), "应")))
            .collect();
        s.apply(apps, false);
        assert_eq!(s.shell.layout.pages, 3);
        // 移除第三页唯一的应用：页数回到 2，停在第三页时滚回第二页。
        s.shell.home_scroll = offset_anim(Spring::SNAPPY, 2.0 * W, 2.0 * W, 0.0, s.now);
        s.apply(vec![Change::Removed("app-21".into())], false);
        assert_eq!(s.shell.layout.pages, 2);
        s.wait(1.5);
        assert_eq!(s.shell.home_scroll.value(s.now), W);
    }
}
