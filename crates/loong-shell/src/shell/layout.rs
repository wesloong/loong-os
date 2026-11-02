//! 主屏布局（平板 regular 尺寸类）。尺寸全部按屏幕逻辑尺寸推算，不写死机型。
//!
//! 网格中的应用按顺序排满一页再排下一页。图标位置使用“内容坐标”：第 p 页的图标
//! x 坐标已加上 `p × 页宽`，绘制时减去主屏的横向滚动位置即可。Dock 不随页面滚动。
//! 既不在 Dock 也不在网格中的应用（已从主屏移除）不显示（`shown` 为假）。

use super::geometry::RectF;

/// 状态栏高度（点）。
pub const STATUS_BAR_HEIGHT: f64 = 24.0;
/// 底部系统手势区高度（点）。
pub const BOTTOM_EDGE: f64 = 20.0;
/// 页面指示点直径（点）。
pub const PAGE_DOT: f64 = 8.0;
/// 页面指示点间距（点）。
pub const PAGE_DOT_GAP: f64 = 8.0;
/// 图标墨影向四周扩展的距离（占图标边长的比例）。
pub const ICON_SHADOW_SPREAD: f64 = 0.16;
/// 图标名称的纸色光晕半径（点）：名称位图四周因此多出这么宽的边距，排版时扣掉。
pub const LABEL_HALO: f64 = 5.0;

/// 主屏各元素的位置。
#[derive(Debug, Clone)]
pub struct Layout {
    /// 图标边长。
    pub icon: f64,
    /// 每个应用的图标位置（与应用列表下标一一对应；网格中的图标为内容坐标）。
    pub icons: Vec<RectF>,
    /// 网格中的应用是否显示名称（Dock 中不显示）。
    pub labeled: Vec<bool>,
    /// 图标所在页；Dock 中的应用（以及不显示的应用）为 `None`。
    pub page_of: Vec<Option<usize>>,
    /// 是否显示在主屏上（Dock 或网格中）。
    pub shown: Vec<bool>,
    /// 页数（至少 1）。
    pub pages: usize,
    /// 页宽（= 屏幕宽度）。
    pub page_width: f64,
    /// 页面指示点的中心 y。
    pub dots_y: f64,
    /// Dock 背板。
    pub dock: RectF,
}

impl Layout {
    /// 计算布局。`in_dock[i]` 表示第 i 个应用是否在 Dock，其余按下标顺序排进网格。
    #[cfg(test)]
    pub fn compute(w: f64, h: f64, in_dock: &[bool]) -> Layout {
        let dock: Vec<usize> = (0..in_dock.len()).filter(|&i| in_dock[i]).collect();
        let grid: Vec<usize> = (0..in_dock.len()).filter(|&i| !in_dock[i]).collect();
        Layout::arrange(w, h, in_dock.len(), &dock, &grid)
    }

    /// 计算 `n` 个应用的布局：`dock`、`grid` 分别是 Dock 与网格中应用的下标（按显示顺序），
    /// 两者都没有的应用不显示。
    pub fn arrange(w: f64, h: f64, n: usize, dock_apps: &[usize], grid_apps: &[usize]) -> Layout {
        let landscape = w >= h;
        let icon = (w.min(h) * 0.095).clamp(56.0, 84.0).round();
        let (cols, rows) = if landscape { (6usize, 4usize) } else { (4, 6) };
        let per_page = cols * rows;

        // Dock
        let dock_count = dock_apps.len().max(1) as f64;
        let dock_pad = (icon * 0.2).round();
        let dock_gap = (icon * 0.3).round();
        let dock_w = dock_count * icon + (dock_count - 1.0) * dock_gap + 2.0 * dock_pad;
        let dock_h = icon + 2.0 * dock_pad;
        let dock = RectF::new(
            ((w - dock_w) / 2.0).round(),
            h - dock_h - 14.0,
            dock_w,
            dock_h,
        );

        // 网格区域：状态栏下方到 Dock 上方；页面指示点在两者之间。
        let gap = (h * 0.05).round();
        let top = STATUS_BAR_HEIGHT + gap;
        let bottom = dock.y - gap;
        let margin_x = (w * if landscape { 0.12 } else { 0.08 }).round();
        let cell_w = (w - 2.0 * margin_x) / cols as f64;
        let cell_h = (bottom - top) / rows as f64;

        let mut icons = vec![RectF::default(); n];
        let mut labeled = vec![false; n];
        let mut page_of = vec![None; n];
        let mut shown = vec![false; n];
        for (dock_i, &i) in dock_apps.iter().enumerate().filter(|(_, i)| **i < n) {
            let x = dock.x + dock_pad + dock_i as f64 * (icon + dock_gap);
            icons[i] = RectF::new(x, dock.y + dock_pad, icon, icon);
            shown[i] = true;
        }
        let mut grid_i = 0usize;
        for &i in grid_apps {
            // 越界的、已经在 Dock（或网格中重复出现）的跳过。
            if i >= n || shown[i] {
                continue;
            }
            let (page, slot) = (grid_i / per_page, grid_i % per_page);
            let (c, r) = (slot % cols, slot / cols);
            let cx = page as f64 * w + margin_x + (c as f64 + 0.5) * cell_w;
            let cy = top + (r as f64 + 0.42) * cell_h;
            let rect = RectF::centered(cx, cy, icon, icon);
            icons[i] = RectF::new(rect.x.round(), rect.y.round(), icon, icon);
            labeled[i] = true;
            page_of[i] = Some(page);
            shown[i] = true;
            grid_i += 1;
        }
        Layout {
            icon,
            icons,
            labeled,
            page_of,
            shown,
            pages: grid_i.div_ceil(per_page).max(1),
            page_width: w,
            dots_y: (dock.y - gap / 2.0).round(),
            dock,
        }
    }

    /// 横向滚动到 `scroll` 时，第 `i` 个图标在屏幕上的位置。
    pub fn icon_at(&self, i: usize, scroll: f64) -> RectF {
        let r = self.icons[i];
        match self.page_of[i] {
            Some(_) => RectF::new(r.x - scroll, r.y, r.w, r.h),
            None => r,
        }
    }

    /// 最后一页的滚动位置。
    pub fn max_scroll(&self) -> f64 {
        (self.pages - 1) as f64 * self.page_width
    }

    /// 命中测试：主屏滚动到 `scroll` 时被点中的应用下标。
    pub fn hit(&self, x: f64, y: f64, scroll: f64) -> Option<usize> {
        // 点击区域比图标略大，手指更容易点中。
        let slop = self.icon * 0.1;
        (0..self.icons.len()).filter(|&i| self.shown[i]).find(|&i| {
            let r = self.icon_at(i, scroll);
            RectF::new(r.x - slop, r.y - slop, r.w + 2.0 * slop, r.h + 2.0 * slop).contains(x, y)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_do_not_overlap_and_fit_on_screen() {
        let dock = [
            true, true, true, true, true, false, false, false, false, false, false, false,
        ];
        for (w, h) in [(1280.0, 800.0), (800.0, 1280.0), (1024.0, 768.0)] {
            let l = Layout::compute(w, h, &dock);
            assert_eq!(l.pages, 1);
            for (i, a) in l.icons.iter().enumerate() {
                assert!(
                    a.x >= 0.0 && a.y >= STATUS_BAR_HEIGHT && a.x + a.w <= w && a.y + a.h <= h,
                    "{w}x{h} #{i}"
                );
                for b in l.icons.iter().skip(i + 1) {
                    let overlap =
                        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
                    assert!(!overlap);
                }
            }
            assert_eq!(l.hit(l.icons[6].x + 1.0, l.icons[6].y + 1.0, 0.0), Some(6));
            assert_eq!(l.hit(1.0, 1.0, 0.0), None);
            // 页面指示点在网格和 Dock 之间。
            assert!(l.dots_y + PAGE_DOT / 2.0 < l.dock.y);
            let grid = l.icons.iter().zip(&l.page_of).filter(|(_, p)| p.is_some());
            assert!(grid.clone().count() > 0);
            assert!(
                grid.into_iter()
                    .all(|(r, _)| r.y + r.h < l.dots_y - PAGE_DOT)
            );
        }
    }

    #[test]
    fn arranged_order_and_hidden_apps() {
        let (w, h) = (1280.0, 800.0);
        // 应用 0 在 Dock；网格顺序 3, 1；应用 2 不显示。
        let l = Layout::arrange(w, h, 4, &[0], &[3, 1]);
        assert_eq!(l.shown, [true, true, false, true]);
        assert_eq!(l.page_of, [None, Some(0), None, Some(0)]);
        assert!(l.icons[3].x < l.icons[1].x, "按给定顺序排");
        assert_eq!(l.pages, 1);
        let r = l.icons[2];
        assert_eq!(l.hit(r.x + 1.0, r.y + 1.0, 0.0), None, "不显示的应用点不中");
        let a = l.icons[3];
        assert_eq!(l.hit(a.x + 1.0, a.y + 1.0, 0.0), Some(3));
        // 与 compute 等价。
        let c = Layout::compute(w, h, &[true, false, false]);
        let d = Layout::arrange(w, h, 3, &[0], &[1, 2]);
        assert_eq!(c.icons, d.icons);
        // 25 个网格应用：两页。
        let grid: Vec<usize> = (1..26).collect();
        assert_eq!(Layout::arrange(w, h, 26, &[0], &grid).pages, 2);
    }

    #[test]
    fn overflowing_apps_go_to_the_next_page() {
        let (w, h) = (1280.0, 800.0);
        // 2 个 Dock 应用 + 30 个网格应用：横屏每页 24 个，共 2 页。
        let mut dock = vec![true, true];
        dock.extend([false; 30]);
        let l = Layout::compute(w, h, &dock);
        assert_eq!(l.pages, 2);
        assert_eq!(l.max_scroll(), w);
        assert_eq!(l.page_of[2], Some(0));
        assert_eq!(l.page_of[2 + 24], Some(1));
        // 第二页第一个图标与第一页第一个图标位置相同，只差一页宽度。
        let (a, b) = (l.icons[2], l.icons[2 + 24]);
        assert_eq!((b.x - a.x, b.y - a.y), (w, 0.0));
        // 滚动到第二页后点中它；Dock 不随页面滚动。
        assert_eq!(l.hit(a.x + 5.0, a.y + 5.0, w), Some(2 + 24));
        assert_eq!(l.hit(a.x + 5.0, a.y + 5.0, 0.0), Some(2));
        let d = l.icons[0];
        assert_eq!(l.hit(d.x + 5.0, d.y + 5.0, w), Some(0));
        assert_eq!(l.icon_at(0, w), d);
    }
}
