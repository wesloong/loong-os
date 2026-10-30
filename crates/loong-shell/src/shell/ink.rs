//! 「墨韵」水墨动效的状态（只描述“画什么”，绘制见 `render`）。
//!
//! - **点墨**：手指按下时，指下晕开一滴墨（半径、浓度都由弹簧驱动）；抬起后墨继续洇开、淡入纸中。
//! - **笔迹**：系统手势（回主屏、翻页、多任务）中，手指身后留下一道渐隐的笔触：
//!   慢则湿而粗，快则干而细（飞白），约 0.5 秒消失。笔迹由一串短笔段组成（环形缓冲，数量有上限）。
//! - **晕染开启 / 收墨**：应用内容像墨在湿宣纸上从图标处晕开；回主屏时逆向收回。
//!   晕染半径由卡片弹簧的进度换算（见 [`spread_progress`]），因此天然可打断、速度连续。
//! - **墨散**：锁屏上移时像墨入清水般向上散开（进度即解锁进度）。
//!
//! 所有效果只在交互/动画期间存在：墨滴与笔段淡尽后立即移除，[`InkState::is_idle`] 随之为真，
//! 画面静止时不做任何计算。

use std::collections::VecDeque;

use loong_motion::{Spring, SpringAnimation};

use super::geometry::{RectF, smoothstep};

/// 墨滴按下时晕开的弹簧（略带一点过冲，像墨“啪”地落在纸上）。
const DROP_BLOOM: Spring = Spring::new(0.32, 0.82);
/// 抬起后墨继续洇开、淡入纸中的弹簧。
const DROP_FADE: Spring = Spring::new(0.5, 1.0);
/// 笔迹淡去的弹簧（临界阻尼：开头墨色保持一下，然后在约 0.5 秒内消失）。
const TRAIL_FADE: Spring = Spring::new(0.42, 1.0);
/// 同时存在的墨滴上限。
pub const MAX_DROPS: usize = 6;
/// 笔段上限（环形缓冲）。
pub const MAX_TRAIL: usize = 72;
/// 笔段的最小长度（点）：手指移动不足时并入下一段。
const MIN_SEGMENT: f64 = 3.0;
/// 墨滴抬起后继续洇开到的比例。
const DROP_SOAK: f64 = 1.3;
/// 墨滴的浓度（0..1）。
const DROP_DENSITY: f64 = 0.3;
/// 笔迹的浓度：湿笔、干笔。
const TRAIL_DENSITY_WET: f64 = 0.74;
const TRAIL_DENSITY_DRY: f64 = 0.86;
/// 笔迹半径（点）：湿而粗、干而细。
const TRAIL_RADIUS_WET: f64 = 11.0;
const TRAIL_RADIUS_DRY: f64 = 5.6;
/// 手指速度（点/秒）：低于前者为湿笔，高于后者为完全的干笔飞白。
const SPEED_WET: f64 = 300.0;
const SPEED_DRY: f64 = 2600.0;
/// 起笔：笔画开头这段距离（点）内由细到粗。
const STROKE_ENTRY: f64 = 36.0;

/// 墨滴所在的层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InkLayer {
    /// 主屏纸面：画在图标之上、应用卡片之下，随主屏缩放。`scrolls` 为真时位置是主屏内容坐标
    /// （随翻页移动，Dock 上的墨滴不随翻页移动）。
    Home { scrolls: bool },
    /// 最上层（多任务卡片之上），屏幕坐标。
    Overlay,
}

/// 一个水墨印记（墨滴或一段笔触），坐标为逻辑点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkDab {
    pub kind: DabKind,
    /// 墨滴：中心（`a == b`）；笔段：起点、终点。
    pub a: (f64, f64),
    pub b: (f64, f64),
    /// 起点、终点处的半径。
    pub ra: f64,
    pub rb: f64,
    /// 浓度 0..1。
    pub density: f64,
    /// 干湿：0 为湿笔，1 为干笔（飞白）。
    pub dry: f64,
    /// 笔画在起点处的累计长度（点），让飞白纹理沿整条笔画连续。
    pub along: f64,
    pub seed: f32,
    /// 起点、终点是否画圆头（整条笔画的起笔与收笔处）。墨滴不用。
    pub caps: (bool, bool),
}

/// 印记种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DabKind {
    Drop,
    Stroke,
}

impl InkDab {
    /// 平移（逻辑点）。
    pub fn offset(mut self, dx: f64, dy: f64) -> Self {
        self.a = (self.a.0 + dx, self.a.1 + dy);
        self.b = (self.b.0 + dx, self.b.1 + dy);
        self
    }

    /// 以 `(ox, oy)` 为中心缩放 `s` 倍（主屏缩放时墨滴跟着缩放）。
    pub fn scaled_about(mut self, ox: f64, oy: f64, s: f64) -> Self {
        let f = |(x, y): (f64, f64)| (ox + (x - ox) * s, oy + (y - oy) * s);
        self.a = f(self.a);
        self.b = f(self.b);
        self.ra *= s;
        self.rb *= s;
        self
    }

    /// 包围盒（逻辑点），留出噪声毛边的余量。
    pub fn bounds(&self) -> RectF {
        let m = self.ra.max(self.rb) * 1.5 + 2.0;
        let (x0, x1) = (self.a.0.min(self.b.0) - m, self.a.0.max(self.b.0) + m);
        let (y0, y1) = (self.a.1.min(self.b.1) - m, self.a.1.max(self.b.1) + m);
        RectF::new(x0, y0, x1 - x0, y1 - y0)
    }
}

struct Drop {
    slot: u32,
    layer: InkLayer,
    x: f64,
    y: f64,
    seed: f32,
    radius: SpringAnimation<f64>,
    density: SpringAnimation<f64>,
    released: bool,
}

struct Segment {
    a: (f64, f64),
    b: (f64, f64),
    ra: f64,
    rb: f64,
    dry: f64,
    along: f64,
    seed: f32,
    /// 墨色 1 → 0。
    fade: SpringAnimation<f64>,
}

/// 正在书写的笔画。
#[derive(Debug, Clone, Copy)]
struct Stroke {
    last: (f64, f64),
    radius: f64,
    dry: f64,
    along: f64,
    seed: f32,
}

/// 水墨动效状态。
pub struct InkState {
    drops: VecDeque<Drop>,
    trail: VecDeque<Segment>,
    stroke: Option<Stroke>,
    seeds: u32,
    /// 关闭时（“减少动态效果”）不产生墨滴与笔迹。
    pub enabled: bool,
}

impl Default for InkState {
    fn default() -> Self {
        Self::new()
    }
}

impl InkState {
    pub fn new() -> Self {
        InkState {
            drops: VecDeque::new(),
            trail: VecDeque::new(),
            stroke: None,
            seeds: 0,
            enabled: true,
        }
    }

    fn next_seed(&mut self) -> f32 {
        // 黄金比例序列：相邻种子差别大，结果确定（截图可复现）。
        self.seeds = self.seeds.wrapping_add(1);
        ((self.seeds as f64 * 0.618_033_988_75).fract() * 97.0) as f32
    }

    /// 点墨：手指 `slot` 在 `(x, y)` 按下，墨滴晕开到半径 `radius`（点）。
    pub fn press(&mut self, slot: u32, layer: InkLayer, x: f64, y: f64, radius: f64, now: f64) {
        if !self.enabled {
            return;
        }
        let seed = self.next_seed();
        self.drops.push_back(Drop {
            slot,
            layer,
            x,
            y,
            seed,
            radius: SpringAnimation::new(DROP_BLOOM, radius * 0.25, radius, 0.0, now)
                .with_rest_thresholds(0.05, 0.5),
            density: SpringAnimation::new(DROP_BLOOM, 0.0, DROP_DENSITY, 0.0, now)
                .with_rest_thresholds(0.002, 0.01),
            released: false,
        });
        while self.drops.len() > MAX_DROPS {
            self.drops.pop_front();
        }
    }

    /// 手指 `slot` 抬起（或开始拖动）：它的墨滴继续洇开并淡入纸中。
    pub fn release(&mut self, slot: u32, now: f64) {
        for d in self
            .drops
            .iter_mut()
            .filter(|d| d.slot == slot && !d.released)
        {
            d.released = true;
            let target = d.radius.target() * DROP_SOAK;
            d.radius.set_spring(DROP_FADE, now);
            d.radius.retarget(target, now);
            d.density.set_spring(DROP_FADE, now);
            d.density.retarget(0.0, now);
        }
    }

    /// 笔迹：手指移动到 `(x, y)`（屏幕坐标），`speed` 为手指速度（点/秒）。
    /// 没有正在书写的笔画时从这里起笔。
    pub fn stroke_to(&mut self, x: f64, y: f64, speed: f64, now: f64) {
        if !self.enabled {
            return;
        }
        let dry = smoothstep(SPEED_WET, SPEED_DRY, speed);
        let Some(mut s) = self.stroke else {
            let seed = self.next_seed();
            self.stroke = Some(Stroke {
                last: (x, y),
                radius: TRAIL_RADIUS_WET * 0.4,
                dry,
                along: 0.0,
                seed,
            });
            return;
        };
        let len = (x - s.last.0).hypot(y - s.last.1);
        if len < MIN_SEGMENT {
            return;
        }
        // 半径与干湿跟随速度平滑变化；起笔一段由细到粗。
        let target = TRAIL_RADIUS_WET + (TRAIL_RADIUS_DRY - TRAIL_RADIUS_WET) * dry;
        let entry = 0.4 + 0.6 * smoothstep(0.0, STROKE_ENTRY, s.along + len);
        let radius = s.radius + (target * entry - s.radius) * 0.35;
        let dry = s.dry + (dry - s.dry) * 0.35;
        self.trail.push_back(Segment {
            a: s.last,
            b: (x, y),
            ra: s.radius,
            rb: radius,
            dry,
            along: s.along,
            seed: s.seed,
            fade: SpringAnimation::new(TRAIL_FADE, 1.0, 0.0, 0.0, now)
                .with_rest_thresholds(0.004, 0.02),
        });
        while self.trail.len() > MAX_TRAIL {
            self.trail.pop_front();
        }
        s.along += len;
        s.last = (x, y);
        s.radius = radius;
        s.dry = dry;
        self.stroke = Some(s);
    }

    /// 笔画结束（手指抬起）。已经画下的笔段继续淡去。
    pub fn end_stroke(&mut self) {
        self.stroke = None;
    }

    /// 是否正在书写笔画。
    pub fn stroking(&self) -> bool {
        self.stroke.is_some()
    }

    /// 每帧调用：移除已经淡尽的墨滴与笔段。
    pub fn update(&mut self, now: f64) {
        self.drops
            .retain(|d| !(d.released && d.density.is_done(now)));
        self.trail.retain(|s| !s.fade.is_done(now));
    }

    /// 没有任何墨迹需要绘制。
    pub fn is_idle(&self) -> bool {
        self.drops.is_empty() && self.trail.is_empty()
    }

    /// 某一层上的墨滴。
    pub fn drops(&self, now: f64) -> impl Iterator<Item = (InkLayer, InkDab)> + '_ {
        self.drops.iter().filter_map(move |d| {
            let density = d.density.value(now);
            (density > 0.003).then(|| {
                let r = d.radius.value(now).max(0.5);
                (
                    d.layer,
                    InkDab {
                        kind: DabKind::Drop,
                        a: (d.x, d.y),
                        b: (d.x, d.y),
                        ra: r,
                        rb: r,
                        density,
                        dry: 0.0,
                        along: 0.0,
                        seed: d.seed,
                        caps: (false, false),
                    },
                )
            })
        })
    }

    /// 笔迹（屏幕坐标），从旧到新。越旧的笔段墨色越淡、笔锋越细（像提笔），尾巴自然收尖。
    pub fn trail(&self, now: f64) -> impl Iterator<Item = InkDab> + '_ {
        let n = self.trail.len();
        self.trail.iter().enumerate().filter_map(move |(i, s)| {
            let f = s.fade.value(now).clamp(0.0, 1.0);
            // 同一笔画的笔段共用种子；下一段属于另一笔画（或没有下一段）时，这一段是收笔。
            let last = self.trail.get(i + 1).is_none_or(|next| next.seed != s.seed);
            (f > 0.004).then(|| {
                let taper = 0.4 + 0.6 * f;
                InkDab {
                    kind: DabKind::Stroke,
                    a: s.a,
                    b: s.b,
                    ra: s.ra * taper,
                    rb: s.rb * taper,
                    density: f
                        * (TRAIL_DENSITY_WET + (TRAIL_DENSITY_DRY - TRAIL_DENSITY_WET) * s.dry),
                    dry: s.dry,
                    along: s.along,
                    seed: s.seed,
                    caps: (s.along == 0.0, last || i + 1 == n),
                }
            })
        })
    }
}

// ───────────────────────── 晕染开启 / 收墨 / 墨散 ─────────────────────────

/// 遮罩模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealMode {
    /// 晕染：以 `center` 为中心、带噪声起伏的圆形前沿向外扩散（前沿积墨）。
    Spread,
    /// 墨散：从下往上像墨入清水般散开（噪声位移 + 透明度阈值）。
    Dissolve,
}

/// 绘制遮罩（作用在纹理 / 窗口上，坐标为屏幕逻辑点，多个元素可以共用同一个遮罩）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reveal {
    pub mode: RevealMode,
    /// 晕染中心（屏幕坐标）。
    pub center: (f64, f64),
    /// 晕染：半径（点）；墨散：进度 0..1。
    pub amount: f64,
    /// 晕染前沿的宽度（点）。
    pub softness: f64,
    pub seed: f32,
}

/// 晕染进度：卡片大小进度 `q`（0 = 图标大小，1 = 全屏）→ 遮罩进度 0..1。
///
/// `q_ref` 为晕染完成时的卡片进度：打开应用时为 1；回主屏时为松手那一刻的进度，
/// 保证收墨从“完全显示”开始、不跳变。卡片快铺满时内容才完全晕开（打开动画的前约 0.3 秒），
/// 晕染的过程看得见，又不让人觉得内容出来得慢。
pub fn spread_progress(q: f64, q_ref: f64) -> f64 {
    let t = (q / q_ref.max(1e-3)).clamp(0.0, 1.0);
    smoothstep(0.06, 0.95, t)
}

/// 卡片 `card`（屏幕坐标）在晕染进度 `progress` 时的遮罩；完全显示时为 `None`（不做任何遮罩计算）。
pub fn spread_reveal(card: RectF, progress: f64, seed: f32) -> Option<Reveal> {
    if progress >= 0.999 {
        return None;
    }
    let (cx, cy) = card.center();
    let half_diag = (card.w * card.w + card.h * card.h).sqrt() / 2.0;
    let softness = (half_diag * 0.12).clamp(10.0, 64.0);
    // 前沿有噪声起伏（约 ±1.5 倍前沿宽度），半径要走得更远才能盖满四角。
    let reach = half_diag + softness * 2.5;
    Some(Reveal {
        mode: RevealMode::Spread,
        center: (cx, cy),
        amount: progress.max(0.0) * reach,
        softness,
        seed,
    })
}

/// 锁屏墨散：解锁进度 `progress`（0 = 盖满，1 = 移开）；没有散开时为 `None`。
pub fn dissolve_reveal(progress: f64, seed: f32) -> Option<Reveal> {
    (progress > 0.001).then_some(Reveal {
        mode: RevealMode::Dissolve,
        center: (0.0, 0.0),
        amount: progress.min(1.0),
        softness: 0.0,
        seed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f64 = 1.0 / 120.0;

    fn run(ink: &mut InkState, from: f64, to: f64) -> f64 {
        let mut t = from;
        while t < to {
            t += FRAME;
            ink.update(t);
        }
        t
    }

    #[test]
    fn drop_blooms_then_fades_and_goes_idle() {
        let mut ink = InkState::new();
        assert!(ink.is_idle());
        ink.press(0, InkLayer::Overlay, 100.0, 200.0, 30.0, 0.0);
        assert!(!ink.is_idle());
        // 按住：半径长到目标附近并保持，浓度不消失。
        let t = run(&mut ink, 0.0, 0.6);
        let (_, d) = ink.drops(t).next().expect("墨滴可见");
        assert!((d.ra - 30.0).abs() < 1.5, "{}", d.ra);
        assert!(d.density > 0.25);
        assert!(!ink.is_idle());
        // 抬起：继续洇开（半径变大、速度连续），浓度淡到 0 后移除。
        ink.release(0, t);
        let (_, at_release) = ink.drops(t).next().unwrap();
        let (_, soon) = ink.drops(t + 0.05).next().unwrap();
        assert!((soon.ra - at_release.ra).abs() < 3.0, "半径不跳变");
        assert!(soon.ra > at_release.ra && soon.density < at_release.density);
        let t = run(&mut ink, t, t + 1.2);
        assert_eq!(ink.drops(t).count(), 0);
        assert!(ink.is_idle());
    }

    #[test]
    fn quick_tap_keeps_growing_after_release() {
        let mut ink = InkState::new();
        ink.press(3, InkLayer::Home { scrolls: true }, 0.0, 0.0, 30.0, 0.0);
        ink.release(3, 0.06);
        let r = |t: f64| ink.drops(t).next().map(|(_, d)| d.ra).unwrap_or(0.0);
        assert!(r(0.06) < r(0.12) && r(0.12) < r(0.3));
        // 只释放对应手指的墨滴。
        ink.press(4, InkLayer::Overlay, 0.0, 0.0, 30.0, 0.1);
        ink.release(3, 0.2);
        let t = run(&mut ink, 0.1, 2.0);
        assert_eq!(ink.drops(t).count(), 1);
        ink.release(4, t);
        run(&mut ink, t, t + 1.5);
        assert!(ink.is_idle());
    }

    #[test]
    fn drops_are_bounded() {
        let mut ink = InkState::new();
        for i in 0..20 {
            ink.press(i, InkLayer::Overlay, 0.0, 0.0, 20.0, 0.0);
        }
        assert_eq!(ink.drops(0.1).count(), MAX_DROPS);
    }

    #[test]
    fn disabled_ink_draws_nothing() {
        let mut ink = InkState::new();
        ink.enabled = false;
        ink.press(0, InkLayer::Overlay, 0.0, 0.0, 20.0, 0.0);
        ink.stroke_to(0.0, 0.0, 0.0, 0.0);
        ink.stroke_to(100.0, 0.0, 0.0, 0.1);
        assert!(ink.is_idle());
    }

    #[test]
    fn slow_strokes_are_wet_and_thick_fast_ones_dry_and_thin() {
        let draw = |speed: f64| {
            let mut ink = InkState::new();
            for k in 0..=40 {
                ink.stroke_to(k as f64 * 6.0, 0.0, speed, k as f64 * FRAME);
            }
            let last = ink.trail(40.0 * FRAME).last().unwrap();
            (last.rb, last.dry)
        };
        let (wet_r, wet_dry) = draw(150.0);
        let (dry_r, dry_dry) = draw(4000.0);
        assert!(wet_r > 1.8 * dry_r, "{wet_r} vs {dry_r}");
        assert!(wet_dry < 0.05 && dry_dry > 0.9);
    }

    #[test]
    fn trail_is_bounded_and_fades_in_about_half_a_second() {
        let mut ink = InkState::new();
        let mut t = 0.0;
        for k in 0..300 {
            t = k as f64 * FRAME;
            ink.stroke_to(k as f64 * 5.0, 100.0, 600.0, t);
            ink.update(t);
        }
        assert!(ink.trail(t).count() <= MAX_TRAIL);
        // 笔段连续：每段的起点是上一段的终点，累计长度递增。
        let segs: Vec<InkDab> = ink.trail(t).collect();
        for w in segs.windows(2) {
            assert_eq!(w[0].b, w[1].a);
            assert!(w[1].along > w[0].along);
        }
        // 越旧的笔段越淡。
        assert!(segs.first().unwrap().density < segs.last().unwrap().density);
        ink.end_stroke();
        assert!(!ink.stroking());
        let after = run(&mut ink, t, t + 0.3);
        assert!(ink.trail(after).count() > 0, "0.3 秒时还有余墨");
        let later = run(&mut ink, after, t + 0.75);
        assert!(ink.is_idle(), "0.75 秒内淡尽");
        assert_eq!(ink.trail(later).count(), 0);
    }

    #[test]
    fn spread_progress_mapping() {
        // 图标大小时什么都没有，全屏前已经完全晕开，单调不减。
        assert_eq!(spread_progress(0.0, 1.0), 0.0);
        assert_eq!(spread_progress(1.0, 1.0), 1.0);
        assert_eq!(spread_progress(0.96, 1.0), 1.0);
        assert_eq!(spread_progress(0.05, 1.0), 0.0);
        assert!((0.3..0.7).contains(&spread_progress(0.5, 1.0)));
        let mut last = 0.0;
        for i in 0..=100 {
            let p = spread_progress(i as f64 / 100.0, 1.0);
            assert!((0.0..=1.0).contains(&p) && p >= last);
            last = p;
        }
        // 收墨从松手时的卡片大小开始：此刻仍完全显示，不跳变。
        assert_eq!(spread_progress(0.4, 0.4), 1.0);
        assert!(spread_progress(0.2, 0.4) < 1.0);
        assert_eq!(spread_progress(0.0, 0.4), 0.0);
    }

    #[test]
    fn spread_reveal_covers_the_card_when_done() {
        let card = RectF::new(100.0, 50.0, 400.0, 300.0);
        assert_eq!(spread_reveal(card, 1.0, 1.0), None);
        let r = spread_reveal(card, 0.0, 1.0).unwrap();
        assert_eq!(r.amount, 0.0);
        assert_eq!(r.center, (300.0, 200.0));
        let almost = spread_reveal(card, 0.998, 1.0).unwrap();
        assert!(
            almost.amount > 250.0 + almost.softness,
            "半径超过半对角线加前沿"
        );
        assert_eq!(dissolve_reveal(0.0, 1.0), None);
        assert_eq!(dissolve_reveal(0.5, 1.0).unwrap().amount, 0.5);
    }
}
