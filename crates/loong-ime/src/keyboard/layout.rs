//! 键盘布局：根据键盘宽高（逻辑点）计算每个键的位置与触摸区域。
//!
//! regular（平板）尺寸类参考 iPad 的非 Pro 键盘：字母键宽 u，退格与右 Shift 约 1.5u，
//! 换行约 2u，第 2 行缩进半个键距。所有尺寸都由宽高推算，不写死机型：
//!
//! | | 1280×800 横屏 | 800×1280 竖屏 |
//! |---|---|---|
//! | 键盘高度（含候选栏与底部 20pt 安全区） | 396pt | 332pt |
//! | 字母键 | ≈ 100×68pt | ≈ 62×59pt |
//! | 键间距 | 12pt | 8pt |
//! | 候选栏 | 50pt | 40pt |
//!
//! 触摸区域（[`Key::hit`]）把键区无缝铺满：键与键之间的空隙按中线分给两侧的键，
//! 每行最左/最右的键延伸到屏幕边缘，第 2 行的缩进归 “A”。键区以外的点，
//! 在触摸容差（[`TOUCH_SLOP`]）内归最近的键。

use super::geometry::Rect;
use super::keymap::{KeyKind, Layer, layer_keys};

/// 触摸容差（pt），与系统手势的起手阈值一致（设计规范 §9.2）。
pub const TOUCH_SLOP: f64 = 8.0;
/// 默认底部安全区（pt）：Home 指示条与底边手势区（设计规范 §8.1）。
pub const DEFAULT_BOTTOM_INSET: f64 = 20.0;

/// 尺寸类（设计规范 §2.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SizeClass {
    /// 平板（逻辑短边 ≥ 600pt）。
    #[default]
    Regular,
    /// 手机（v2）。目前沿用 regular 的网格，见 [`KeyboardLayout::compute`]。
    Compact,
}

impl SizeClass {
    /// 由屏幕逻辑尺寸判断尺寸类。
    pub fn for_screen(width: f64, height: f64) -> SizeClass {
        if width.min(height) >= 600.0 {
            SizeClass::Regular
        } else {
            SizeClass::Compact
        }
    }

    /// 是否在按键上方显示放大的按键气泡（iPhone 有，iPad 没有）。
    pub fn shows_key_preview(self) -> bool {
        self == SizeClass::Compact
    }
}

/// 布局输入。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutParams {
    /// 尺寸类。
    pub size_class: SizeClass,
    /// 键盘宽度（通常等于屏幕宽度）。
    pub width: f64,
    /// 键盘总高度：候选栏 + 键区 + 底部安全区。
    pub height: f64,
    /// 底部安全区高度（不放键，留给 Home 指示条与系统手势）。
    pub bottom_inset: f64,
}

impl LayoutParams {
    /// 按屏幕逻辑尺寸给出推荐的键盘尺寸（宽度占满屏幕）。
    pub fn for_screen(screen_width: f64, screen_height: f64) -> LayoutParams {
        let size_class = SizeClass::for_screen(screen_width, screen_height);
        let h = Horizontal::new(screen_width);
        // 横屏键较扁（≈ 0.68u），竖屏键接近方形；不小于 44pt 触控目标。
        let key_h = (screen_height * 0.085)
            .clamp(44.0, (0.95 * h.key_width).max(44.0))
            .round();
        let pad = (h.gap * 0.75).round();
        let height = h.bar_height + pad + 4.0 * key_h + 3.0 * h.gap + pad + DEFAULT_BOTTOM_INSET;
        LayoutParams {
            size_class,
            width: screen_width,
            height,
            bottom_inset: DEFAULT_BOTTOM_INSET,
        }
    }
}

/// 由键盘宽度推出的水平尺寸。
struct Horizontal {
    edge_padding: f64,
    gap: f64,
    key_width: f64,
    bar_height: f64,
}

impl Horizontal {
    fn new(width: f64) -> Horizontal {
        let width = width.max(1.0);
        let gap = (width * 0.0095).round().clamp(6.0, 14.0);
        let edge_padding = (width * 0.005).round().clamp(3.0, 8.0);
        // 第 1 行：10 个字母键 + 1.5u 的退格 + 10 个间距。
        let key_width = ((width - 2.0 * edge_padding - 10.0 * gap) / 11.5).max(1.0);
        let bar_height = (key_width * 0.5).round().clamp(40.0, 52.0);
        Horizontal {
            edge_padding,
            gap,
            key_width,
            bar_height,
        }
    }
}

/// 布局推算出的尺寸，供绘制使用。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// 字母键宽 u。
    pub key_width: f64,
    /// 键高。
    pub key_height: f64,
    /// 同一行键之间的间距。
    pub column_gap: f64,
    /// 行间距。
    pub row_gap: f64,
    /// 左右边距。
    pub edge_padding: f64,
    /// 候选栏高度。
    pub candidate_bar_height: f64,
    /// 键的圆角（连续圆角，见 loong-ui `shape`）。
    pub corner_radius: f64,
    /// 字母键帽字号。
    pub letter_font_size: f64,
    /// 功能键文字（“123”“换行”等）字号。
    pub label_font_size: f64,
    /// 候选字号。
    pub candidate_font_size: f64,
}

/// 键在布局中的下标。三层键盘共用同一网格，同一位置的键下标相同。
pub type KeyId = usize;

/// 一个键。
#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    /// 种类。
    pub kind: KeyKind,
    /// 可见的键帽矩形（整数点对齐）。
    pub rect: Rect,
    /// 触摸区域：比键帽大，所有键的触摸区域无缝铺满键区。
    pub hit: Rect,
    /// 所在行（0 为最上一行）。
    pub row: usize,
}

/// 命中结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// 候选栏（由宿主处理：点选候选、横向滚动）。
    CandidateBar,
    /// 某个键。
    Key(KeyId),
}

/// 一层键盘的完整布局。
#[derive(Debug, Clone, PartialEq)]
pub struct KeyboardLayout {
    /// 布局输入。
    pub params: LayoutParams,
    /// 推算出的尺寸。
    pub metrics: Metrics,
    /// 层。
    pub layer: Layer,
    /// 中文模式（影响第 3 行的标点键）。
    pub chinese: bool,
    /// 候选栏区域（键盘顶部）。
    pub candidate_bar: Rect,
    /// 键区（候选栏以下、底部安全区以上）。
    pub key_area: Rect,
    /// 所有键，按行、从左到右排列。
    pub keys: Vec<Key>,
}

impl KeyboardLayout {
    /// 计算布局。
    ///
    /// compact（手机）布局是 v2 的工作：届时在这里分派到单独的 `compact()`
    /// （10 列等宽字母键、无缩进、Shift/退格占第 3 行两端、更高的键，并配合
    /// [`SizeClass::shows_key_preview`] 显示按键气泡）。键的种类、层与命中规则
    /// 不变，`Keyboard` 状态机无需改动。在此之前 compact 先沿用 regular 的网格。
    pub fn compute(params: &LayoutParams, layer: Layer, chinese: bool) -> KeyboardLayout {
        match params.size_class {
            SizeClass::Regular | SizeClass::Compact => regular(params, layer, chinese),
        }
    }

    /// 命中测试：候选栏、键（含键间空隙），或在触摸容差内最近的键。
    pub fn hit_test(&self, x: f64, y: f64) -> Option<Hit> {
        if self.candidate_bar.contains(x, y) {
            return Some(Hit::CandidateBar);
        }
        self.key_at(x, y).map(Hit::Key)
    }

    /// 点所在的键（不考虑候选栏）。
    pub fn key_at(&self, x: f64, y: f64) -> Option<KeyId> {
        if let Some(id) = self.keys.iter().position(|k| k.hit.contains(x, y)) {
            return Some(id);
        }
        // 键区以外（屏幕边缘、底部安全区）：离触摸区域不超过触摸容差时，归最近的键。
        self.keys
            .iter()
            .enumerate()
            .map(|(id, k)| (id, k.hit.distance_to(x, y)))
            .filter(|&(_, d)| d <= TOUCH_SLOP)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }

    /// 第一个指定种类的键。
    pub fn find(&self, kind: KeyKind) -> Option<KeyId> {
        self.keys.iter().position(|k| k.kind == kind)
    }

    /// 某个键。
    pub fn key(&self, id: KeyId) -> &Key {
        &self.keys[id]
    }
}

fn regular(params: &LayoutParams, layer: Layer, chinese: bool) -> KeyboardLayout {
    let w = params.width.max(1.0);
    let h = Horizontal::new(w);
    let (pad_x, gap, u) = (h.edge_padding, h.gap, h.key_width);
    let pitch = u + gap;

    // 纵向：候选栏 | 上边距 | 4 行键（3 个行距）| 下边距 | 底部安全区
    let bar = Rect::new(0.0, 0.0, w, h.bar_height.min(params.height.max(0.0)));
    let area_bottom = (params.height - params.bottom_inset.max(0.0)).max(bar.bottom());
    let key_area = Rect::from_edges(0.0, bar.bottom(), w, area_bottom);
    let mut row_gap = gap;
    let mut pad_y = (row_gap * 0.75).round();
    if key_area.h - 2.0 * pad_y - 3.0 * row_gap < 4.0 * 20.0 {
        // 高度不够时先压缩间距，保证键都在键盘范围内。
        row_gap = 0.0;
        pad_y = 0.0;
    }
    let key_h = ((key_area.h - 2.0 * pad_y - 3.0 * row_gap) / 4.0).max(0.0);

    let keymap = layer_keys(layer, chinese);
    let right = w - pad_x;
    let mut keys = Vec::with_capacity(37);
    for (row, kinds) in keymap.rows.iter().enumerate() {
        let top = key_area.y + pad_y + row as f64 * (key_h + row_gap);
        let spans: Vec<(f64, f64)> = match row {
            // 10 个字符键 + 退格
            0 => (0..10)
                .map(|i| {
                    let x = pad_x + i as f64 * pitch;
                    (x, x + u)
                })
                .chain(std::iter::once((pad_x + 10.0 * pitch, right)))
                .collect(),
            // 缩进半个键距：9 个字符键 + 换行
            1 => {
                let off = pad_x + pitch / 2.0;
                (0..9)
                    .map(|i| {
                        let x = off + i as f64 * pitch;
                        (x, x + u)
                    })
                    .chain(std::iter::once((off + 9.0 * pitch, right)))
                    .collect()
            }
            // 左功能键 + 9 个字符键 + 右功能键
            2 => std::iter::once((pad_x, pad_x + u))
                .chain((1..10).map(|i| {
                    let x = pad_x + i as f64 * pitch;
                    (x, x + u)
                }))
                .chain(std::iter::once((pad_x + 10.0 * pitch, right)))
                .collect(),
            // 换层 | 中/英 | 空格 | 换层 | 收起
            _ => {
                let wide = 1.5 * u + 0.5 * gap;
                let fixed = [wide, u, 0.0, wide, u];
                let space = (right - pad_x) - (2.0 * wide + 2.0 * u) - 4.0 * gap;
                let mut x = pad_x;
                fixed
                    .iter()
                    .map(|&kw| {
                        let kw = if kw == 0.0 { space } else { kw };
                        let span = (x, x + kw);
                        x += kw + gap;
                        span
                    })
                    .collect()
            }
        };
        debug_assert_eq!(spans.len(), kinds.len());
        for (&kind, &(x0, x1)) in kinds.iter().zip(&spans) {
            keys.push(Key {
                kind,
                rect: Rect::from_edges(x0.round(), top.round(), x1.round(), (top + key_h).round()),
                hit: Rect::default(),
                row,
            });
        }
    }
    assign_hit_rects(&mut keys, &key_area);

    let letter_font_size = (key_h * 0.34).round().clamp(17.0, 26.0);
    KeyboardLayout {
        params: *params,
        metrics: Metrics {
            key_width: u,
            key_height: key_h,
            column_gap: gap,
            row_gap,
            edge_padding: pad_x,
            candidate_bar_height: bar.h,
            corner_radius: (key_h * 0.11).round().clamp(5.0, 8.0),
            letter_font_size,
            label_font_size: (letter_font_size * 0.7).round().clamp(13.0, 18.0),
            candidate_font_size: (bar.h * 0.42).round().clamp(17.0, 22.0),
        },
        layer,
        chinese,
        candidate_bar: bar,
        key_area,
        keys,
    }
}

/// 触摸区域：键间空隙按中线平分，行首行尾延伸到键区边缘，首行/末行延伸到键区上下边。
fn assign_hit_rects(keys: &mut [Key], area: &Rect) {
    let rows = keys.iter().map(|k| k.row).max().map_or(0, |r| r + 1);
    let row_span = |keys: &[Key], row: usize| -> (f64, f64) {
        let k = keys.iter().find(|k| k.row == row).expect("每行至少一个键");
        (k.rect.y, k.rect.bottom())
    };
    let bounds: Vec<(f64, f64)> = (0..rows)
        .map(|r| {
            let (top, bottom) = row_span(keys, r);
            let hit_top = if r == 0 {
                area.y
            } else {
                (row_span(keys, r - 1).1 + top) / 2.0
            };
            let hit_bottom = if r + 1 == rows {
                area.bottom()
            } else {
                (bottom + row_span(keys, r + 1).0) / 2.0
            };
            (hit_top, hit_bottom)
        })
        .collect();
    for (r, &(top, bottom)) in bounds.iter().enumerate() {
        let ids: Vec<usize> = (0..keys.len()).filter(|&i| keys[i].row == r).collect();
        for (n, &i) in ids.iter().enumerate() {
            let left = if n == 0 {
                area.x
            } else {
                (keys[ids[n - 1]].rect.right() + keys[i].rect.x) / 2.0
            };
            let right = if n + 1 == ids.len() {
                area.right()
            } else {
                (keys[i].rect.right() + keys[ids[n + 1]].rect.x) / 2.0
            };
            keys[i].hit = Rect::from_edges(left, top, right, bottom);
        }
    }
}
