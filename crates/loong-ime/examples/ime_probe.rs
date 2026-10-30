//! 输入法探针：测量 Rime 启动/部署耗时、单键延迟和内存占用，并打印候选。
//!
//! ```sh
//! cargo run --release -p loong-ime --example ime_probe -- \
//!     [--shared /usr/share/rime-data] [--user DIR] [--schema luna_pinyin_simp] [拼音 ...]
//! ```
//!
//! 默认用户目录为临时目录（每次都是“首次部署”）。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use loong_ime::{ImeConfig, ImeEngine, ImeKey, Rime};

/// 读取 /proc/self/status 中的内存字段（kB）。
fn mem_kb(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(field))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn print_mem(stage: &str) {
    println!(
        "[内存] {stage:<10} RSS {:>6} kB（匿名 {:>6} kB，文件映射 {:>6} kB）",
        mem_kb("VmRSS:"),
        mem_kb("RssAnon:"),
        mem_kb("RssFile:")
    );
}

fn main() {
    let mut config = ImeConfig::with_user_data_dir(
        std::env::temp_dir().join(format!("loong-ime-probe-{}", std::process::id())),
    );
    let mut inputs = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--shared" => {
                config.shared_data_dir = PathBuf::from(args.next().expect("--shared DIR"))
            }
            "--user" => config.user_data_dir = PathBuf::from(args.next().expect("--user DIR")),
            "--schema" => config.schema = args.next().expect("--schema ID"),
            _ => inputs.push(arg),
        }
    }
    if inputs.is_empty() {
        inputs = [
            "nihao",
            "zhongguo",
            "shurufa",
            "woxiangqubeijing",
            "jintiantianqizhenhao",
            "pingguo",
            "xi'an",
            "nvhai",
            "loong",
        ]
        .map(String::from)
        .to_vec();
    }

    print_mem("启动前");
    let t0 = Instant::now();
    let rime = match Rime::start(&config) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("启动失败：{e}");
            std::process::exit(1);
        }
    };
    println!(
        "librime {}，方案 {}，启动+部署 {:?}",
        rime.version(),
        rime.schema(),
        t0.elapsed()
    );
    print_mem("启动后");

    let mut ime = ImeEngine::with_rime(rime).expect("创建会话");
    let mut keys = 0u32;
    let mut total = Duration::ZERO;
    let mut worst = Duration::ZERO;
    for input in &inputs {
        ime.clear();
        for c in input.chars() {
            let t = Instant::now();
            ime.process_key(ImeKey::Char(c));
            let dt = t.elapsed();
            keys += 1;
            total += dt;
            worst = worst.max(dt);
        }
        let cands: Vec<&str> = ime
            .state()
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect();
        println!("{input:<22} → {}", cands.join(" "));
    }
    ime.clear();
    println!(
        "单键延迟（含刷新候选）：平均 {:?}，最大 {:?}（{keys} 次）",
        total / keys.max(1),
        worst
    );
    print_mem("输入后");
}
