//! Shell 的行为测试：用虚拟时钟回放触摸，检查手势的去向和动画的连续性。
//! 演示应用没有窗口，不需要 Wayland。

use super::*;

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

    /// 推进时间，每帧调用 `update`。
    fn wait(&mut self, secs: f64) {
        let end = self.now + secs;
        while self.now < end - 1e-9 {
            self.now = (self.now + FRAME).min(end);
            self.shell.update(self.now);
        }
    }

    fn app(&self, id: &str) -> usize {
        self.shell.apps.iter().position(|a| a.id == id).unwrap()
    }

    fn launch(&mut self, id: &str) {
        let app = self.app(id);
        self.shell.launch(app, self.now);
        self.wait(1.0);
    }

    /// 滑动：按 120Hz 送触摸点，到达终点后停住 `hold` 秒再松手。
    fn swipe(&mut self, from: (f64, f64), to: (f64, f64), dur: f64, hold: f64) {
        self.shell.touch_down(0, from.0, from.1, self.now);
        let steps = ((dur * 120.0).ceil() as usize).max(1);
        for k in 1..=steps {
            let f = k as f64 / steps as f64;
            self.now += dur / steps as f64;
            let (x, y) = (lerp(from.0, to.0, f), lerp(from.1, to.1, f));
            self.shell.touch_motion(0, x, y, self.now);
            self.shell.update(self.now);
        }
        self.wait(hold);
        self.shell.touch_up(0, self.now);
    }

    fn tap(&mut self, x: f64, y: f64) {
        self.shell.touch_down(0, x, y, self.now);
        self.wait(0.06);
        self.shell.touch_up(0, self.now);
    }

    /// 底边快速上甩回主屏。
    fn flick_home(&mut self) {
        self.swipe((640.0, 795.0), (640.0, 500.0), 0.12, 0.0);
        self.wait(1.5);
    }

    /// 底边上滑后停住：进入多任务。
    fn enter_switcher(&mut self) {
        self.swipe((640.0, 795.0), (640.0, 640.0), 0.2, 0.3);
        assert!(self.shell.in_switcher(), "上滑停顿应进入多任务");
        self.wait(1.5);
    }

    /// 依次打开这些应用（每个都回主屏），最后一个留在前台。
    fn open_apps(&mut self, ids: &[&str]) {
        for (k, id) in ids.iter().enumerate() {
            self.launch(id);
            if k + 1 < ids.len() {
                self.flick_home();
            }
        }
    }

    fn running(&self) -> Vec<usize> {
        self.shell.running_apps()
    }

    fn apps(&self, ids: &[&str]) -> Vec<usize> {
        ids.iter().map(|id| self.app(id)).collect()
    }

    /// 多任务中某个应用卡片的屏幕矩形。
    fn card(&self, id: &str) -> [f64; 5] {
        let i = self.shell.task_index(self.app(id)).expect("应用在运行");
        self.shell.switcher_card_state(i, self.now).0
    }

    fn card_center(&self, id: &str) -> (f64, f64) {
        let r = self.card(id);
        (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
    }
}

fn assert_rect_eq(a: [f64; 5], b: [f64; 5], tol: f64) {
    for i in 0..5 {
        assert!((a[i] - b[i]).abs() <= tol, "{a:?} ≠ {b:?}");
    }
}

#[test]
fn pause_during_bottom_swipe_enters_switcher() {
    let mut s = Sim::new();
    s.open_apps(&["photos", "notes"]);
    assert_eq!(s.running(), s.apps(&["photos", "notes"]));
    s.enter_switcher();
    assert!(s.shell.is_idle(s.now));
    // 最近的应用在右侧槽位，更早的在左侧，与布局一致。
    let sw = s.shell.switcher;
    let scroll = sw.initial_scroll(2);
    assert_rect_eq(s.card("photos"), sw.slot(0, scroll), 0.1);
    assert_rect_eq(s.card("notes"), sw.slot(1, scroll), 0.1);
    assert!(s.card("notes")[0] > W / 2.0);
}

#[test]
fn fast_flick_still_goes_home() {
    let mut s = Sim::new();
    s.open_apps(&["photos"]);
    s.flick_home();
    assert!(!s.shell.in_switcher() && !s.shell.app_in_front());
    assert_eq!(s.running(), s.apps(&["photos"]), "应用留在后台");
    assert!(s.shell.is_idle(s.now));
}

#[test]
fn short_or_moving_swipe_does_not_enter_switcher() {
    let mut s = Sim::new();
    s.open_apps(&["photos"]);
    // 只上移 50pt（不足 0.5·D）就停住：不进入多任务，松手后回到应用。
    s.swipe((640.0, 795.0), (640.0, 745.0), 0.15, 0.4);
    assert!(!s.shell.in_switcher());
    s.wait(1.0);
    assert!(s.shell.app_in_front());
    // 一直在移动（没有停顿）、慢慢拖回原处：也不进入。
    s.swipe((640.0, 795.0), (640.0, 640.0), 0.2, 0.0);
    assert!(!s.shell.in_switcher());
}

#[test]
fn held_finger_keeps_card_still() {
    let mut s = Sim::new();
    s.open_apps(&["photos"]);
    s.shell.touch_down(0, 640.0, 795.0, s.now);
    for k in 1..=12 {
        s.now += FRAME;
        s.shell
            .touch_motion(0, 640.0, 795.0 - k as f64 * 5.0, s.now);
        s.shell.update(s.now);
    }
    // 上移 60pt 后停住（不足以进入多任务）：卡片停在手指下，不会被拉回全屏。
    let held = s.shell.tasks[0].rect.value(s.now);
    s.wait(0.3);
    assert!(!s.shell.in_switcher());
    assert_rect_eq(s.shell.tasks[0].rect.value(s.now), held, 1e-9);
    s.shell.touch_up(0, s.now);
}

#[test]
fn releasing_after_entering_keeps_switcher_open() {
    let mut s = Sim::new();
    s.open_apps(&["photos", "notes"]);
    // 停顿后继续往上快速甩：仍然停在多任务。
    s.shell.touch_down(0, 640.0, 795.0, s.now);
    for k in 1..=24 {
        s.now += FRAME;
        s.shell
            .touch_motion(0, 640.0, 795.0 - k as f64 * 6.0, s.now);
        s.shell.update(s.now);
    }
    s.wait(0.3);
    assert!(s.shell.in_switcher());
    for k in 1..=10 {
        s.now += FRAME;
        s.shell
            .touch_motion(0, 640.0, 651.0 - k as f64 * 25.0, s.now);
    }
    s.shell.touch_up(0, s.now);
    s.wait(1.5);
    assert!(s.shell.in_switcher() && s.shell.is_idle(s.now));
}

#[test]
fn tap_card_opens_app_without_jump() {
    let mut s = Sim::new();
    s.open_apps(&["photos", "notes", "maps"]);
    s.enter_switcher();
    let (x, y) = s.card_center("notes");
    let before = s.card("notes");
    s.shell.touch_down(0, x, y, s.now);
    s.now += 0.05;
    s.shell.touch_up(0, s.now);
    assert!(s.shell.app_in_front());
    // 打开时卡片从原位置出发，不跳变。
    let front = s.shell.tasks.last().unwrap();
    assert_eq!(front.app, s.app("notes"));
    assert_rect_eq(front.rect.value(s.now), before, 0.5);
    s.wait(1.5);
    assert_eq!(
        s.running(),
        s.apps(&["photos", "maps", "notes"]),
        "点开的应用变为最近使用"
    );
    assert_rect_eq(
        s.shell.tasks.last().unwrap().rect.value(s.now),
        s.shell.full_rect(),
        0.1,
    );
    assert!(s.shell.is_idle(s.now));
}

#[test]
fn swipe_card_up_closes_app_and_reflows() {
    let mut s = Sim::new();
    s.open_apps(&["photos", "notes", "maps"]);
    s.enter_switcher();
    let (x, y) = s.card_center("notes");
    s.swipe((x, y), (x, y - 220.0), 0.1, 0.0);
    assert_eq!(s.running(), s.apps(&["photos", "maps"]));
    assert_eq!(s.shell.dismissed.len(), 1);
    // 关闭的卡片带着松手速度向上飞出。
    let flying = s.shell.dismissed[0].rect;
    assert!(flying.velocity(s.now)[1] < -1000.0);
    s.wait(1.5);
    assert!(s.shell.dismissed.is_empty() && s.shell.in_switcher());
    // 剩下的卡片补位到各自的槽位。
    let sw = s.shell.switcher;
    let scroll = s.shell.scroll.value(s.now);
    assert_eq!(scroll, sw.initial_scroll(2));
    assert_rect_eq(s.card("photos"), sw.slot(0, scroll), 0.1);
    assert_rect_eq(s.card("maps"), sw.slot(1, scroll), 0.1);
    assert!(s.shell.is_idle(s.now));
}

#[test]
fn small_card_drag_springs_back() {
    let mut s = Sim::new();
    s.open_apps(&["photos", "notes"]);
    s.enter_switcher();
    let (x, y) = s.card_center("photos");
    s.swipe((x, y), (x, y - 80.0), 0.3, 0.1);
    assert_eq!(s.running(), s.apps(&["photos", "notes"]));
    s.wait(1.0);
    assert_eq!(s.shell.tasks[0].lift.value(s.now), 0.0);
    // 往下拖按橡皮筋：卡片移动距离小于手指。
    s.shell.touch_down(0, x, y, s.now);
    for k in 1..=20 {
        s.now += FRAME;
        s.shell.touch_motion(0, x, y + k as f64 * 10.0, s.now);
    }
    let lift = s.shell.tasks[0].lift.value(s.now);
    assert!(lift > 0.0 && lift < 200.0 * 0.6, "{lift}");
    s.shell.touch_up(0, s.now);
    s.wait(1.0);
    assert_eq!(s.running(), s.apps(&["photos", "notes"]));
}

#[test]
fn closing_the_last_card_goes_home() {
    let mut s = Sim::new();
    s.open_apps(&["photos"]);
    s.enter_switcher();
    let (x, y) = s.card_center("photos");
    // 一张卡片时居中。
    assert!((x - W / 2.0).abs() < 1e-6);
    s.swipe((x, y), (x, y - 300.0), 0.12, 0.0);
    assert!(s.running().is_empty());
    assert!(!s.shell.in_switcher() && !s.shell.app_in_front());
    s.wait(1.5);
    assert!(s.shell.is_idle(s.now));
}

#[test]
fn tap_empty_space_or_bottom_swipe_goes_home() {
    let mut s = Sim::new();
    s.open_apps(&["photos", "notes"]);
    s.enter_switcher();
    s.tap(640.0, 120.0);
    assert!(!s.shell.in_switcher() && !s.shell.app_in_front());
    s.wait(1.5);
    assert!(s.shell.is_idle(s.now));
    assert_eq!(s.running(), s.apps(&["photos", "notes"]));

    s.launch("notes");
    s.enter_switcher();
    s.swipe((640.0, 795.0), (640.0, 600.0), 0.12, 0.0);
    assert!(!s.shell.in_switcher() && !s.shell.app_in_front());
}

#[test]
fn scrolling_tracks_finger_and_snaps_to_cards() {
    let mut s = Sim::new();
    s.open_apps(&["photos", "notes", "maps", "clock", "files"]);
    s.enter_switcher();
    let sw = s.shell.switcher;
    let (lo, hi) = sw.scroll_range(5);
    assert_eq!(s.shell.scroll.value(s.now), hi);
    // 1:1 跟手。
    s.shell.touch_down(0, 800.0, 300.0, s.now);
    for k in 1..=40 {
        s.now += FRAME;
        s.shell
            .touch_motion(0, 800.0 + k as f64 * 10.0, 300.0, s.now);
    }
    let shown = s.shell.scroll.value(s.now);
    // 锁定方向后从锁定位置开始跟手，所以位移略小于手指总位移。
    assert!(hi - shown >= 380.0 && hi - shown <= 400.0, "{shown}");
    // 停顿后松手：吸附到最近的卡片位置。
    s.wait(0.2);
    s.shell.touch_up(0, s.now);
    s.wait(1.0);
    let settled = s.shell.scroll.value(s.now);
    assert_eq!(settled, hi - sw.stride);
    // 轻甩：吸附到下一张。
    s.swipe((300.0, 300.0), (450.0, 300.0), 0.06, 0.0);
    s.wait(1.0);
    assert_eq!(s.shell.scroll.value(s.now), hi - 2.0 * sw.stride);
    // 越过最左端：橡皮筋，松手后回到边界。
    s.swipe((200.0, 300.0), (1100.0, 300.0), 0.6, 0.1);
    s.wait(1.5);
    assert_eq!(s.shell.scroll.value(s.now), lo);
    s.shell.touch_down(0, 200.0, 300.0, s.now);
    for k in 1..=40 {
        s.now += FRAME;
        s.shell
            .touch_motion(0, 200.0 + k as f64 * 15.0, 300.0, s.now);
    }
    let over = s.shell.scroll.value(s.now);
    assert!(over < lo && over > lo - 600.0 * 0.56, "{over}");
    s.shell.touch_up(0, s.now);
    s.wait(1.5);
    assert_eq!(s.shell.scroll.value(s.now), lo);
    assert!(s.shell.is_idle(s.now));
}

#[test]
fn touch_catches_moving_scroll() {
    let mut s = Sim::new();
    s.open_apps(&["photos", "notes", "maps", "clock"]);
    s.enter_switcher();
    s.swipe((300.0, 300.0), (500.0, 300.0), 0.08, 0.0);
    s.wait(0.05);
    let moving = s.shell.scroll.value(s.now);
    assert!(!s.shell.scroll.is_done(s.now));
    // 手指按下：列表立即停住，不跳。
    s.shell.touch_down(0, 640.0, 120.0, s.now);
    assert_eq!(s.shell.scroll.value(s.now), moving);
    s.wait(0.2);
    assert_eq!(s.shell.scroll.value(s.now), moving);
    // 拖动卡片（纵向）后松手：列表吸附回最近的卡片位置。
    s.shell.touch_up(0, s.now);
    assert!(!s.shell.in_switcher(), "点按空白处回主屏");
}

#[test]
fn caught_scroll_resnaps_after_vertical_drag() {
    let mut s = Sim::new();
    s.open_apps(&["photos", "notes", "maps", "clock"]);
    s.enter_switcher();
    s.swipe((300.0, 300.0), (500.0, 300.0), 0.08, 0.0);
    s.wait(0.05);
    let (x, y) = s.card_center("notes");
    s.swipe((x, y), (x, y - 40.0), 0.3, 0.1);
    s.wait(1.0);
    let sw = s.shell.switcher;
    let scroll = s.shell.scroll.value(s.now);
    assert!(sw.snap_points(4).contains(&scroll), "{scroll}");
    assert_eq!(s.running().len(), 4);
}

#[test]
fn opening_from_home_still_works_after_switcher() {
    let mut s = Sim::new();
    s.open_apps(&["photos", "notes"]);
    s.enter_switcher();
    s.tap(640.0, 120.0);
    s.wait(1.5);
    // 回到主屏后点图标打开应用：从图标位置弹出。
    s.shell.launch(s.app("maps"), s.now);
    let icon = s.shell.icon_target(s.app("maps"));
    assert_rect_eq(s.shell.tasks.last().unwrap().rect.value(s.now), icon, 1e-6);
    s.wait(1.5);
    assert!(s.shell.app_in_front());
    assert_eq!(s.running(), s.apps(&["photos", "notes", "maps"]));
    s.flick_home();
    assert!(s.shell.is_idle(s.now));
}

#[test]
fn home_pages_track_finger_and_snap() {
    let mut s = Sim::new();
    assert_eq!(s.shell.layout.pages, 2, "演示应用排满两页");
    let scroll = |s: &Sim| s.shell.home_scroll.value(s.now);
    // 慢慢拖过半页后停住再松手：翻到第二页。
    s.swipe((1000.0, 400.0), (300.0, 400.0), 0.6, 0.1);
    s.wait(1.0);
    assert_eq!(scroll(&s), W);
    // 末页继续往左拖：1:1 变为橡皮筋，松手后弹回。
    s.shell.touch_down(0, 1000.0, 400.0, s.now);
    for k in 1..=40 {
        s.now += FRAME;
        s.shell
            .touch_motion(0, 1000.0 - k as f64 * 10.0, 400.0, s.now);
    }
    let over = scroll(&s);
    assert!(over > W && over < W + 400.0 * 0.56, "{over}");
    s.shell.touch_up(0, s.now);
    s.wait(1.0);
    assert_eq!(scroll(&s), W);
    // 拖得不多但甩得快：翻回第一页。
    s.swipe((300.0, 400.0), (560.0, 400.0), 0.05, 0.0);
    s.wait(1.0);
    assert_eq!(scroll(&s), 0.0);
    // 慢拖不到半页：回到原页。
    s.swipe((900.0, 400.0), (700.0, 400.0), 0.5, 0.1);
    s.wait(1.0);
    assert_eq!(scroll(&s), 0.0);
    // 首页往右拖：橡皮筋。
    s.shell.touch_down(0, 300.0, 400.0, s.now);
    for k in 1..=40 {
        s.now += FRAME;
        s.shell
            .touch_motion(0, 300.0 + k as f64 * 10.0, 400.0, s.now);
    }
    let under = scroll(&s);
    assert!(under < 0.0 && under > -400.0 * 0.56, "{under}");
    s.shell.touch_up(0, s.now);
    s.wait(1.0);
    assert_eq!(scroll(&s), 0.0);
    assert!(s.shell.is_idle(s.now));
}

#[test]
fn paging_catches_and_resnaps() {
    let mut s = Sim::new();
    s.swipe((1100.0, 400.0), (700.0, 400.0), 0.03, 0.0);
    s.wait(0.05);
    let moving = s.shell.home_scroll.value(s.now);
    assert!(moving > 0.0 && moving < W);
    // 翻页途中按住：主屏停住；松手后吸附到最近的一页。
    s.shell.touch_down(0, 640.0, 400.0, s.now);
    s.wait(0.2);
    assert_eq!(s.shell.home_scroll.value(s.now), moving);
    s.shell.touch_up(0, s.now);
    s.wait(1.0);
    let settled = s.shell.home_scroll.value(s.now);
    assert!(settled == 0.0 || settled == W);
}

#[test]
fn app_on_second_page_opens_from_and_returns_to_its_icon() {
    let mut s = Sim::new();
    s.swipe((1000.0, 400.0), (300.0, 400.0), 0.6, 0.1);
    s.wait(1.0);
    let app = s.app("terminal");
    let icon = s.shell.layout.icon_at(app, W);
    assert!(icon.x > 0.0 && icon.x + icon.w < W, "图标在第二页上可见");
    s.tap(icon.x + icon.w / 2.0, icon.y + icon.h / 2.0);
    assert!(s.shell.app_in_front());
    assert_eq!(s.shell.tasks.last().unwrap().app, app);
    s.wait(1.0);
    s.flick_home();
    assert_eq!(s.shell.home_scroll.value(s.now), W);
    let target = [icon.x, icon.y, icon.w, icon.h, s.shell.icon_target(app)[4]];
    assert_rect_eq(s.shell.tasks[0].rect.value(s.now), target, 1e-6);
}

#[test]
fn background_task_added_in_switcher_keeps_view_still() {
    let mut s = Sim::new();
    s.open_apps(&["photos"]);
    s.enter_switcher();
    let before = s.card("photos");
    let maps = s.app("maps");
    s.shell
        .add_background_task(Task::new(maps, None, s.now), s.now);
    // 加入的瞬间画面不动，之后滚回新的吸附位置：两张卡片并排，新加入的在左侧。
    assert_rect_eq(s.card("photos"), before, 1e-6);
    s.wait(1.5);
    let sw = s.shell.switcher;
    let scroll = sw.initial_scroll(2);
    assert_rect_eq(s.card("maps"), sw.slot(0, scroll), 0.1);
    assert_rect_eq(s.card("photos"), sw.slot(1, scroll), 0.1);
    assert!(s.shell.in_switcher() && s.shell.is_idle(s.now));
}

/// 系统繁忙时触摸事件被批量处理：停顿期间 `update` 一次也没跑，
/// 只要抬起时手指已静止足够久，仍应进入多任务。
#[test]
fn pause_seen_only_at_release_still_enters_switcher() {
    let mut s = Sim::new();
    s.open_apps(&["photos", "notes"]);
    s.shell.touch_down(0, 640.0, 795.0, s.now);
    for k in 1..=24 {
        s.now += 0.2 / 24.0;
        s.shell
            .touch_motion(0, 640.0, 795.0 - 155.0 * k as f64 / 24.0, s.now);
    }
    // 停住 0.3 秒，但期间没有任何 update（模拟事件被积压后一起处理）。
    s.now += 0.3;
    s.shell.touch_up(0, s.now);
    assert!(s.shell.in_switcher(), "抬起前已停住，应进入多任务");
    s.wait(1.5);
    assert!(s.shell.in_switcher());
}

// ───────────────────────── 墨韵：水墨动效 ─────────────────────────

impl Sim {
    fn scene(&self) -> Scene {
        self.shell.scene(self.now)
    }

    /// 场景中的水墨印记（墨滴、笔段）数量。
    fn ink_items(&self) -> usize {
        self.scene()
            .iter()
            .filter(|i| matches!(i, DrawItem::Ink(_)))
            .count()
    }

    /// 山水长卷的（是否停在某页，滚动位置）。
    fn landscape(&self) -> (Option<usize>, f64) {
        self.scene()
            .into_iter()
            .find_map(|i| match i {
                DrawItem::Landscape { page, scroll, .. } => Some((page, scroll)),
                _ => None,
            })
            .expect("主屏画了山水")
    }

    /// 单独画出的图标墨影数量。
    fn icon_shadows(&self) -> usize {
        self.scene()
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    DrawItem::Texture {
                        key: TexKey::IconShadow,
                        ..
                    }
                )
            })
            .count()
    }

    /// 某个应用的占位画面上的遮罩（`None` 表示占位画面没有画）。
    fn placeholder_reveal(&self, id: &str) -> Option<Option<Reveal>> {
        let app = self.app(id);
        self.scene().into_iter().find_map(|i| match i {
            DrawItem::Texture {
                key: TexKey::Placeholder(a),
                reveal,
                ..
            } if a == app => Some(reveal),
            _ => None,
        })
    }

    /// 锁屏上的遮罩（`None` 表示锁屏没有画）。
    fn lock_reveal(&self) -> Option<Option<Reveal>> {
        self.scene().into_iter().find_map(|i| match i {
            DrawItem::Texture {
                key: TexKey::LockSheet,
                reveal,
                ..
            } => Some(reveal),
            _ => None,
        })
    }
}

#[test]
fn press_blooms_an_ink_drop_that_fades_after_release() {
    let mut s = Sim::new();
    assert_eq!(s.ink_items(), 0);
    // 按在主屏空白处（不是图标）。
    s.shell.touch_down(0, 1150.0, 480.0, s.now);
    s.wait(0.3);
    assert_eq!(s.ink_items(), 1);
    assert!(!s.shell.is_idle(s.now), "按住时一直在画");
    s.shell.touch_up(0, s.now);
    s.wait(0.2);
    assert_eq!(s.ink_items(), 1, "抬起后墨还在淡去");
    s.wait(1.0);
    assert_eq!(s.ink_items(), 0);
    assert!(s.shell.is_idle(s.now), "墨淡尽后不再重画");
}

#[test]
fn paging_leaves_a_brush_trail_and_unrolls_the_landscape() {
    let mut s = Sim::new();
    // 停在第一页：画预先合成的整页，网格图标的墨影已在其中，只单独画 Dock 图标的墨影。
    assert_eq!(s.landscape(), (Some(0), 0.0));
    let dock = s
        .shell
        .layout
        .page_of
        .iter()
        .filter(|p| p.is_none())
        .count();
    assert_eq!(s.icon_shadows(), dock);
    // 拖动中：逐层视差 + 逐个墨影 + 指后的笔迹。
    s.shell.touch_down(0, 1000.0, 400.0, s.now);
    for k in 1..=20 {
        s.now += FRAME;
        s.shell
            .touch_motion(0, 1000.0 - k as f64 * 35.0, 400.0, s.now);
        s.shell.update(s.now);
    }
    let (page, scroll) = s.landscape();
    assert_eq!(page, None);
    assert!(scroll > 0.0);
    assert!(s.icon_shadows() > dock);
    assert!(s.ink_items() > 10, "笔迹由一串笔段组成");
    s.shell.touch_up(0, s.now);
    s.wait(1.5);
    assert!(s.shell.is_idle(s.now));
    assert_eq!(s.ink_items(), 0, "笔迹约半秒后淡尽");
    assert_eq!(s.landscape(), (Some(1), W), "翻到第二页并停稳");
    assert_eq!(s.icon_shadows(), dock);
}

#[test]
fn opening_spreads_like_ink_and_going_home_gathers_it_back() {
    let mut s = Sim::new();
    let photos = s.app("photos");
    s.shell.launch(photos, s.now);
    s.wait(0.06);
    let r = s
        .placeholder_reveal("photos")
        .expect("内容在画")
        .expect("打开初期内容正在晕开");
    assert_eq!(r.mode, RevealMode::Spread);
    s.wait(0.05);
    let later = s.placeholder_reveal("photos").unwrap().unwrap();
    assert!(later.amount > r.amount, "晕染半径随卡片长大");
    s.wait(1.0);
    assert_eq!(s.placeholder_reveal("photos"), Some(None), "铺满后不再遮罩");
    // 回主屏：手指拖动时内容完整显示，松手飞回图标时收墨。
    s.shell.touch_down(0, 640.0, 795.0, s.now);
    for k in 1..=12 {
        s.now += FRAME;
        s.shell
            .touch_motion(0, 640.0, 795.0 - k as f64 * 25.0, s.now);
        s.shell.update(s.now);
        assert_eq!(s.placeholder_reveal("photos"), Some(None));
    }
    s.shell.touch_up(0, s.now);
    s.wait(0.1);
    let gathering = s.placeholder_reveal("photos").unwrap().expect("收墨");
    s.wait(0.08);
    match s.placeholder_reveal("photos") {
        Some(Some(r)) => assert!(r.amount < gathering.amount),
        // 内容已经完全收回（晕染进度为 0 时不画）。
        other => assert_eq!(other, None),
    }
    s.wait(1.5);
    assert!(s.shell.is_idle(s.now));
}

#[test]
fn unlocking_dissolves_the_lock_sheet() {
    let mut s = Sim::new();
    s.shell.set_locked(true);
    assert_eq!(s.lock_reveal(), Some(None), "盖满时不做遮罩");
    s.shell.touch_down(0, 640.0, 760.0, s.now);
    for k in 1..=10 {
        s.now += FRAME;
        s.shell
            .touch_motion(0, 640.0, 760.0 - k as f64 * 20.0, s.now);
    }
    let r = s.lock_reveal().unwrap().expect("上移时墨散");
    assert_eq!(r.mode, RevealMode::Dissolve);
    assert!((r.amount - 200.0 / H).abs() < 1e-6, "散开的进度即解锁进度");
    s.shell.touch_up(0, s.now);
    s.wait(1.5);
    assert_eq!(s.lock_reveal(), None, "解锁后锁屏不再绘制");
    assert!(s.shell.is_idle(s.now));
}

#[test]
fn reduced_motion_falls_back_to_crossfades() {
    let mut s = Sim::new();
    s.shell.set_reduced_motion(true);
    s.shell.touch_down(0, 1150.0, 480.0, s.now);
    s.wait(0.2);
    assert_eq!(s.ink_items(), 0, "不画墨滴");
    s.shell.touch_up(0, s.now);
    // 翻页：山水不动，没有笔迹。
    s.swipe((1000.0, 400.0), (300.0, 400.0), 0.3, 0.0);
    assert_eq!(s.ink_items(), 0);
    assert_eq!(s.landscape().1, 0.0);
    s.wait(1.5);
    assert!(s.shell.is_idle(s.now));
    // 打开应用：内容淡入，不晕染。
    let photos = s.app("photos");
    s.shell.launch(photos, s.now);
    for _ in 0..30 {
        s.wait(FRAME);
        if let Some(reveal) = s.placeholder_reveal("photos") {
            assert_eq!(reveal, None);
        }
    }
}

/// 场景中状态栏时间的各层：（墨, 不透明度），从下到上。
fn status_layers(s: &Sim) -> Vec<(StatusInk, f64)> {
    s.shell
        .scene(s.now)
        .iter()
        .filter_map(|item| match item {
            DrawItem::Natural {
                key: TexKey::StatusTime(ink),
                alpha,
                ..
            } => Some((*ink, *alpha)),
            _ => None,
        })
        .collect()
}

#[test]
fn status_bar_follows_the_system_appearance_and_crossfades() {
    let mut s = Sim::new();
    s.shell.set_theme(Theme::Dark);
    // 第一帧之前、之后都直接是月白：开机不会从浓墨淡过来。
    assert_eq!(status_layers(&s), vec![(StatusInk::Light, 1.0)]);
    s.wait(FRAME);
    assert_eq!(s.shell.status_ink(s.now), Some(StatusInk::Light));
    assert_eq!(status_layers(&s), vec![(StatusInk::Light, 1.0)]);
    assert!(s.shell.is_idle(s.now));

    // 切换外观：两种墨交叉淡化，字形始终是实的（下层不透明）。
    s.shell.set_theme(Theme::Light);
    s.wait(0.15);
    assert!(!s.shell.is_idle(s.now));
    let layers = status_layers(&s);
    assert_eq!(layers.len(), 2, "{layers:?}");
    assert_eq!(layers[0].1, 1.0);
    assert!(layers[1].1 > 0.05 && layers[1].1 <= 0.5, "{layers:?}");
    s.wait(1.0);
    assert_eq!(status_layers(&s), vec![(StatusInk::Dark, 1.0)]);
    assert!(s.shell.is_idle(s.now));

    // 没有窗口的演示应用（占位画面按系统外观画）、多任务：都跟随系统外观。
    s.shell.set_theme(Theme::Dark);
    s.wait(1.0);
    s.launch("photos");
    assert!(s.shell.app_in_front());
    assert_eq!(s.shell.status_ink(s.now), Some(StatusInk::Light));
    s.enter_switcher();
    assert_eq!(s.shell.status_ink(s.now), Some(StatusInk::Light));
}
