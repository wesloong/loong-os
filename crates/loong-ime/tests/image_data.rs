//! 系统镜像用的最小 Rime 数据集：按打包步骤预编译 loong_pinyin、删掉源文件，
//! 确认引擎在只有这些文件时能正常工作，并报告数据集大小。
//!
//! 打包步骤（`docs/api/loong-ime.md` 的“数据与体积”一节）：
//!
//! 1. 源文件：rime-plum-data 的 default / key_bindings / punctuation / pinyin、
//!    luna_pinyin.dict.yaml、essay.txt，加上本 crate 的 data/loong_pinyin.*.yaml；
//! 2. `rime_deployer --compile loong_pinyin.schema.yaml <临时用户目录> <源目录> <共享目录>/build`；
//! 3. 共享目录只保留 [`RUNTIME_YAML`]、`build/` 下的 [`PREBUILT`]、`opencc/` 下的 [`OPENCC`]。
//!
//! 需要 `rime_deployer`（librime-tools，随 librime-dev 安装）与 rime-plum-data；
//! 缺少时跳过（`LOONG_IME_REQUIRE_RIME=1` 时失败）。librime 是进程全局的，所以这个
//! 测试单独放一个文件（单独的测试进程）。

use std::path::{Path, PathBuf};
use std::process::Command;

use loong_ime::{ImeConfig, ImeEngine};

const SYSTEM_RIME: &str = "/usr/share/rime-data";
const SYSTEM_OPENCC: &str = "/usr/share/opencc";
/// 编译所需的源文件（来自 rime-plum-data）。
const SOURCES: [&str; 6] = [
    "default.yaml",
    "key_bindings.yaml",
    "punctuation.yaml",
    "pinyin.yaml",
    "luna_pinyin.dict.yaml",
    "essay.txt",
];
/// 运行时需要的 YAML（部署时编译方案用）。
const RUNTIME_YAML: [&str; 5] = [
    "default.yaml",
    "key_bindings.yaml",
    "punctuation.yaml",
    "pinyin.yaml",
    "loong_pinyin.schema.yaml",
];
/// 预编译产物。
const PREBUILT: [&str; 4] = [
    "loong_pinyin.table.bin",
    "loong_pinyin.reverse.bin",
    "loong_pinyin.prism.bin",
    "loong_pinyin.schema.yaml",
];
/// 繁转简（OpenCC t2s）。
const OPENCC: [&str; 3] = ["t2s.json", "TSPhrases.ocd2", "TSCharacters.ocd2"];

fn skip(msg: &str) {
    let msg = format!("跳过最小数据集测试：{msg}（Alpine：apk add librime-dev rime-plum-data）");
    if std::env::var_os("LOONG_IME_REQUIRE_RIME").is_some() {
        panic!("{msg}");
    }
    eprintln!("{msg}");
}

fn copy(from_dir: &Path, names: &[&str], to_dir: &Path) {
    std::fs::create_dir_all(to_dir).unwrap();
    for name in names {
        std::fs::copy(from_dir.join(name), to_dir.join(name))
            .unwrap_or_else(|e| panic!("复制 {name}：{e}"));
    }
}

fn dir_size(dir: &Path, files: &mut Vec<(String, u64)>) -> u64 {
    let mut total = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let meta = entry.metadata().unwrap();
        if meta.is_dir() {
            total += dir_size(&entry.path(), files);
        } else {
            files.push((entry.path().display().to_string(), meta.len()));
            total += meta.len();
        }
    }
    total
}

#[test]
fn prebuilt_minimal_data_set() {
    let system = Path::new(SYSTEM_RIME);
    if let Some(missing) = SOURCES.iter().find(|f| !system.join(f).is_file()) {
        return skip(&format!("缺少 {SYSTEM_RIME}/{missing}"));
    }
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("image-data");
    let _ = std::fs::remove_dir_all(&root);
    let (src, shared, user) = (root.join("src"), root.join("shared"), root.join("user"));
    copy(system, &SOURCES, &src);
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
    copy(
        &data,
        &["loong_pinyin.schema.yaml", "loong_pinyin.dict.yaml"],
        &src,
    );

    // 预编译（镜像构建时执行一次）。
    let t0 = std::time::Instant::now();
    // rime_deployer 的日志写在 stderr，很长：只在失败时打印。
    let output = Command::new("rime_deployer")
        .arg("--compile")
        .arg(src.join("loong_pinyin.schema.yaml"))
        .arg(root.join("prebuild-user"))
        .arg(&src)
        .arg(shared.join("build"))
        .output();
    match output {
        Ok(o) if o.status.success() => {}
        Ok(o) => panic!(
            "rime_deployer 失败：{}\n{}",
            o.status,
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => return skip(&format!("无法运行 rime_deployer：{e}")),
    }
    eprintln!("预编译 loong_pinyin 耗时 {:?}", t0.elapsed());

    // 只保留运行时需要的文件。
    for entry in std::fs::read_dir(shared.join("build")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if !PREBUILT.contains(&name.as_str()) {
            std::fs::remove_file(&path).unwrap(); // 调试用的 *.txt 导出等
        }
    }
    for name in PREBUILT {
        assert!(
            shared.join("build").join(name).is_file(),
            "缺少预编译产物 {name}"
        );
    }
    copy(&src, &RUNTIME_YAML, &shared);
    if Path::new(SYSTEM_OPENCC).join(OPENCC[0]).is_file() {
        copy(Path::new(SYSTEM_OPENCC), &OPENCC, &shared.join("opencc"));
    }

    let mut files = Vec::new();
    let size = dir_size(&shared, &mut files);
    files.sort();
    for (path, len) in &files {
        eprintln!("{len:>10}  {path}");
    }
    eprintln!(
        "最小数据集合计 {size} 字节（{:.1} MiB）",
        size as f64 / 1048576.0
    );
    assert!(size < 10 * 1024 * 1024, "最小数据集应小于 10MiB：{size}");

    // 只用这个数据集启动引擎。
    let config = ImeConfig {
        shared_data_dir: shared.clone(),
        user_data_dir: user.clone(),
        ..ImeConfig::default()
    };
    let t0 = std::time::Instant::now();
    let mut ime = ImeEngine::new(&config).expect("用最小数据集启动");
    eprintln!("首次启动（用户目录部署）耗时 {:?}", t0.elapsed());
    assert!(
        !user.join("loong_pinyin.dict.yaml").exists(),
        "共享目录有预编译词典时，不应在用户目录放词典源文件"
    );
    assert!(
        !user.join("build/loong_pinyin.table.bin").exists(),
        "词典直接用共享目录里预编译的，不在用户目录重新编译"
    );
    ime.type_str("nihao");
    assert_eq!(ime.state().candidates[0].text, "你好");
    ime.process_key(loong_ime::ImeKey::Space);
    assert_eq!(ime.take_commit().as_deref(), Some("你好"));
    ime.type_str("zhongguo");
    assert_eq!(ime.state().candidates[0].text, "中国");
    let mut user_files = Vec::new();
    let user_size = dir_size(&user, &mut user_files);
    eprintln!("用户目录 {user_size} 字节");

    // 重新部署：先销毁会话，部署后新建会话（本文件只有这一个测试，不会有别的会话）。
    let rime = loong_ime::Rime::running().unwrap();
    assert!(matches!(
        rime.redeploy(),
        Err(loong_ime::ImeError::SessionsAlive(1))
    ));
    drop(ime);
    let t0 = std::time::Instant::now();
    rime.redeploy().expect("没有会话时可以重新部署");
    eprintln!("完整检查重新部署耗时 {:?}", t0.elapsed());
    let mut ime = ImeEngine::with_rime(rime).unwrap();
    ime.type_str("nihao");
    assert_eq!(ime.state().candidates[0].text, "你好");
}
