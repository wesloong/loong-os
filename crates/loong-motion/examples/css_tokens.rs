//! 输出 LoongKit 的弹簧缓动 CSS 变量：`cargo run -p loong-motion --example css_tokens`
use loong_motion::{Spring, css::spring_easing};

fn main() {
    println!(":root {{");
    for (name, spring) in [
        ("interactive", Spring::INTERACTIVE),
        ("snappy", Spring::SNAPPY),
        ("smooth", Spring::SMOOTH),
        ("bouncy", Spring::BOUNCY),
    ] {
        let e = spring_easing(spring, 0.002);
        println!("  --loong-spring-{name}-duration: {}ms;", e.duration_ms);
        println!("  --loong-spring-{name}-easing: {};", e.easing);
    }
    println!("}}");
}
