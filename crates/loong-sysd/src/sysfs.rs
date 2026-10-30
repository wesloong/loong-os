//! sysfs 读写的小工具。
//!
//! sysfs 根目录可以用环境变量 `LOONG_SYSFS_ROOT` 改到别处，测试时指向临时目录里的
//! 假目录树。内核驱动的文件内容并不总是规整：可能有多余的空白、读取时返回 EIO/ENODATA、
//! 或者干脆不存在，所以这里的读取函数一律返回 `Option`，由调用方决定缺省行为。

use std::{
    io,
    path::{Path, PathBuf},
    str::FromStr,
};

/// 环境变量：sysfs 根目录（默认 `/sys`）。
pub const ROOT_ENV: &str = "LOONG_SYSFS_ROOT";

/// sysfs 根目录。
#[derive(Debug, Clone)]
pub struct Sysfs {
    root: PathBuf,
}

impl Sysfs {
    /// 使用指定的根目录。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Sysfs { root: root.into() }
    }

    /// 读取 `LOONG_SYSFS_ROOT`，未设置时为 `/sys`。
    pub fn from_env() -> Self {
        match std::env::var_os(ROOT_ENV) {
            Some(root) if !root.is_empty() => Sysfs::new(root),
            _ => Sysfs::new("/sys"),
        }
    }

    /// 根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `class/<class>` 下的所有条目（名称, 路径），按名称排序，保证选择结果稳定。
    pub fn class_entries(&self, class: &str) -> Vec<(String, PathBuf)> {
        let dir = self.root.join("class").join(class);
        let Ok(read) = std::fs::read_dir(&dir) else {
            return vec![];
        };
        let mut entries: Vec<(String, PathBuf)> = read
            .filter_map(Result::ok)
            .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
            .collect();
        entries.sort();
        entries
    }
}

/// 读取文本属性并去掉首尾空白。文件不存在、读取失败或内容为空时返回 `None`。
pub fn read_string(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// 读取数值属性。无法解析时返回 `None`。
pub fn read_num<T: FromStr>(path: &Path) -> Option<T> {
    read_string(path)?.parse().ok()
}

/// 读取数值属性为 `f64`，过滤掉 NaN / 无穷大。
pub fn read_f64(path: &Path) -> Option<f64> {
    read_num::<f64>(path).filter(|v| v.is_finite())
}

/// 写入属性（sysfs 属性要求一次 write 写完）。
pub fn write_string(path: &Path, value: &str) -> io::Result<()> {
    std::fs::write(path, value)
}

#[cfg(test)]
pub(crate) mod testutil {
    //! 测试用的临时目录（不引入额外依赖）。

    use std::{
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
    };

    /// 测试结束时自动删除的临时目录。
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("loong-sysd-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }

        /// 写入 `<根>/<rel>`，自动创建上级目录。
        pub fn write(&self, rel: &str, content: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }

        pub fn read(&self, rel: &str) -> String {
            std::fs::read_to_string(self.0.join(rel)).unwrap()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{testutil::TempDir, *};

    #[test]
    fn read_helpers_tolerate_odd_content() {
        let t = TempDir::new("sysfs");
        t.write("a", "  42\n");
        t.write("b", "\n");
        t.write("c", "abc");
        t.write("d", "nan");
        t.write("e", "-1500000\n");
        assert_eq!(read_num::<u32>(&t.path().join("a")), Some(42));
        assert_eq!(read_string(&t.path().join("b")), None);
        assert_eq!(read_num::<u32>(&t.path().join("c")), None);
        assert_eq!(read_f64(&t.path().join("d")), None);
        assert_eq!(read_f64(&t.path().join("e")), Some(-1_500_000.0));
        assert_eq!(read_string(&t.path().join("missing")), None);
    }

    #[test]
    fn class_entries_sorted_and_missing_class_is_empty() {
        let t = TempDir::new("class");
        t.write("class/backlight/zz/type", "raw");
        t.write("class/backlight/aa/type", "raw");
        let sysfs = Sysfs::new(t.path());
        let names: Vec<_> = sysfs
            .class_entries("backlight")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["aa", "zz"]);
        assert!(sysfs.class_entries("power_supply").is_empty());
    }
}
