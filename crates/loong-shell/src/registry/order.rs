//! 主屏网格的排列：`$XDG_CONFIG_HOME/loong/home.toml`（默认 `~/.config/loong/home.toml`）。
//!
//! ```toml
//! # 主屏网格中应用的顺序（loong-shell 写入）
//! order = ["photos", "notes", "example-com"]
//! ```
//!
//! 只记网格（Dock 固定为系统应用）。文件里没有的应用排在最后（新安装的应用追加到末尾），
//! 文件里有、但已不存在的 id 被忽略。写入时先写临时文件再 rename，目录权限 0700。

use std::{
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// home.toml 的路径。
pub fn path() -> Option<PathBuf> {
    let non_empty = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let base = non_empty("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| non_empty("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("loong").join("home.toml"))
}

/// 解析文件内容；格式不对时返回空列表（主屏按默认顺序排）。
pub fn parse(text: &str) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Home {
        #[serde(default)]
        order: Vec<String>,
    }
    match toml::from_str::<Home>(text) {
        Ok(h) => h.order,
        Err(e) => {
            tracing::warn!("home.toml 格式不对，按默认顺序排列：{e}");
            Vec::new()
        }
    }
}

/// 读取网格顺序；文件不存在时为空。
pub fn read(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// 生成文件内容。
pub fn format(order: &[String]) -> String {
    let mut out = String::from(
        "# 主屏网格中应用的顺序（loong-shell 写入；新安装的应用追加到末尾）\norder = [\n",
    );
    for id in order {
        // id 可能来自手写的文件，按 TOML 基本字符串转义。
        let escaped = id.replace('\\', "\\\\").replace('"', "\\\"");
        out += &format!("  \"{escaped}\",\n");
    }
    out += "]\n";
    out
}

/// 原子地写入网格顺序。
pub fn write(path: &Path, order: &[String]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "home.toml 没有上级目录"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let tmp = path.with_extension("toml.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(format(order).as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let order: Vec<String> = ["photos", "example-com", "we\"ird"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(parse(&format(&order)), order);
        assert!(parse("order = 3").is_empty());
        assert!(parse("").is_empty());
        let dir = std::env::temp_dir().join(format!("loong-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("loong").join("home.toml");
        assert!(read(&file).is_empty());
        write(&file, &order).unwrap();
        assert_eq!(read(&file), order);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
