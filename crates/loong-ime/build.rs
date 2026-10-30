//! 通过 pkg-config 链接系统的 librime（Alpine：`apk add librime-dev`）。
//!
//! 只链接 `rime_get_api` 这一个符号，其余接口都经由它返回的函数表调用，
//! 因此不需要 bindgen，也不依赖头文件的具体版本（见 `src/rime/ffi.rs`）。

fn main() {
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    if let Err(e) = pkg_config::Config::new()
        .atleast_version("1.8")
        .probe("rime")
    {
        panic!(
            "找不到 librime（需要 1.8 及以上，项目使用 1.17）：{e}\n\
             在 Alpine chroot 中执行：apk add librime-dev rime-plum-data"
        );
    }
}
