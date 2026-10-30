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
