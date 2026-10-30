//! 主屏布局（平板 regular 尺寸类）。尺寸全部按屏幕逻辑尺寸推算，不写死机型。

use super::geometry::RectF;

/// 状态栏高度（点）。
pub const STATUS_BAR_HEIGHT: f64 = 24.0;
/// 底部系统手势区高度（点）。
pub const BOTTOM_EDGE: f64 = 20.0;

/// 主屏各元素的位置。
#[derive(Debug, Clone)]
pub struct Layout {
    /// 图标边长。
    pub icon: f64,
    /// 每个应用的图标位置（与应用列表下标一一对应）。
    pub icons: Vec<RectF>,
    /// 网格中的应用是否显示名称（Dock 中不显示）。
    pub labeled: Vec<bool>,
    /// Dock 背板。
    pub dock: RectF,
}

impl Layout {
    /// 计算布局。`in_dock[i]` 表示第 i 个应用是否在 Dock。
    pub fn compute(w: f64, h: f64, in_dock: &[bool]) -> Layout {
        let landscape = w >= h;
        let icon = (w.min(h) * 0.095).clamp(56.0, 84.0).round();
        let (cols, rows) = if landscape { (6usize, 4usize) } else { (4, 6) };

        // Dock
        let dock_count = in_dock.iter().filter(|&&d| d).count().max(1) as f64;
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

        // 网格区域：状态栏下方到 Dock 上方。
        let top = STATUS_BAR_HEIGHT + (h * 0.05).round();
        let bottom = dock.y - (h * 0.05).round();
        let margin_x = (w * if landscape { 0.12 } else { 0.08 }).round();
        let cell_w = (w - 2.0 * margin_x) / cols as f64;
        let cell_h = (bottom - top) / rows as f64;

        let mut icons = Vec::with_capacity(in_dock.len());
        let mut labeled = Vec::with_capacity(in_dock.len());
        let (mut grid_i, mut dock_i) = (0usize, 0usize);
        for &d in in_dock {
            if d {
                let x = dock.x + dock_pad + dock_i as f64 * (icon + dock_gap);
                icons.push(RectF::new(x, dock.y + dock_pad, icon, icon));
                labeled.push(false);
                dock_i += 1;
            } else {
                // v1 只有一页；超出一页的应用暂不显示（后续加分页）。
                let (c, r) = (grid_i % cols, grid_i / cols);
                let cx = margin_x + (c as f64 + 0.5) * cell_w;
                let cy = top + (r as f64 + 0.42) * cell_h;
                let rect = if r < rows {
                    RectF::centered(cx, cy, icon, icon)
                } else {
                    RectF::new(-1e4, -1e4, icon, icon)
                };
                icons.push(RectF::new(rect.x.round(), rect.y.round(), icon, icon));
                labeled.push(true);
                grid_i += 1;
            }
        }
        Layout {
            icon,
            icons,
            labeled,
            dock,
        }
    }

    /// 命中测试：返回被点中的应用下标。
    pub fn hit(&self, x: f64, y: f64) -> Option<usize> {
        // 点击区域比图标略大，手指更容易点中。
        let slop = self.icon * 0.1;
        self.icons.iter().position(|r| {
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
            assert_eq!(l.hit(l.icons[6].x + 1.0, l.icons[6].y + 1.0), Some(6));
            assert_eq!(l.hit(1.0, 1.0), None);
        }
    }
}
