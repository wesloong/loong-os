//! 「墨韵」资源生成耗时：开机时要画的全部静态元素（2560×1600、2 页的山水长卷 + 37 个
//! 152 px 图标 + 状态栏 + 绢 Dock + 墨影）。预算：本机 release ≤ 0.6 s，目标设备 ≤ 1.5 s。
//!
//! 用法：`cargo run --release -p loong-ui --example ink_bench [轮数]`

use std::time::{Duration, Instant};

use loong_ui::icon::builtin_symbol;
use loong_ui::ink::{self, IconSpec};
use loong_ui::theme::Theme;
use loong_ui::tiny_skia::Rect;

const SYMBOLS: [&str; 34] = [
    "compass",
    "message-circle",
    "mail",
    "music",
    "settings",
    "image",
    "notebook-pen",
    "calendar",
    "clock",
    "folder",
    "camera",
    "map",
    "calculator",
    "cloud-sun",
    "play-circle",
    "book-open",
    "mic",
    "list-checks",
    "contact",
    "heart-pulse",
    "trending-up",
    "languages",
    "podcast",
    "house",
    "wallet",
    "shopping-bag",
    "gamepad-2",
    "newspaper",
    "timer",
    "ruler",
    "scan-line",
    "terminal",
    "wifi",
    "battery-full",
];

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    let rounds: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let (w, h) = (2560u32, 1600u32);
    // 预热：纸纹砖、字体只建一次（开机时也只建一次，计入第一轮）。
    for theme in [Theme::Light, Theme::Dark] {
        let mut best = [f64::MAX; 5];
        for round in 0..rounds {
            let t0 = Instant::now();
            let layers = ink::landscape(theme, w, h, 2, 7);
            let t_land = t0.elapsed();
            let t1 = Instant::now();
            for i in 0..37 {
                let sym = SYMBOLS[i % SYMBOLS.len()];
                let _ = ink::app_icon(
                    152,
                    &IconSpec {
                        symbol_svg: builtin_symbol(sym).unwrap(),
                        pigment: ink::suggested_pigment(sym),
                        theme,
                        seed: i as u32,
                    },
                );
            }
            let t_icons = t1.elapsed();
            let t2 = Instant::now();
            let _ = ink::battery(theme, 32, 0.8, false);
            let _ = ink::wifi(theme, 32, 3);
            let bg = ink::composite(&layers, w, h, 0.0);
            let dock = Rect::from_xywh(778.0, 1360.0, 1004.0, 212.0).unwrap();
            let _ = ink::silk_panel(theme, &bg, dock, 60.0);
            let _ = ink::ink_shadow(theme, 152, 152, 34.0, 24);
            let t_misc = t2.elapsed();
            let total = t0.elapsed();
            let row = [ms(t_land), ms(t_icons), ms(t_misc), ms(total), 0.0];
            for k in 0..4 {
                best[k] = best[k].min(row[k]);
            }
            println!(
                "{theme:?} 第 {} 轮：山水 {:.0} ms，37 个图标 {:.0} ms，状态栏/Dock/墨影 {:.0} ms，合计 {:.0} ms",
                round + 1,
                row[0],
                row[1],
                row[2],
                row[3]
            );
            let _ = layers;
        }
        let budget = if best[0] + best[1] <= 600.0 {
            "达标"
        } else {
            "超出"
        };
        println!(
            "{theme:?} 最好：山水 + 图标 = {:.0} ms（预算 600 ms，{budget}）；含状态栏/Dock 合计 {:.0} ms",
            best[0] + best[1],
            best[3]
        );
    }
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    println!("（{cores} 个 CPU 核；山水各层并行生成）");
}
