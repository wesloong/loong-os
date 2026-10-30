//! 输入法配置：数据目录、方案，以及写入用户目录的 Rime 文件。
//!
//! ## 默认方案 `loong_pinyin`
//!
//! 全拼、简体输出，词典约 8MB（见 `data/loong_pinyin.*.yaml`）。数据有两种来源：
//!
//! - **系统镜像**：Rime 数据包把方案和**预先编译好**的词典装在共享目录
//!   （`build/loong_pinyin.table.bin` 等），运行时只需几十毫秒的部署；
//! - **开发环境**（只装了 Alpine 的 `rime-plum-data`）：启动时把本 crate 自带的
//!   `loong_pinyin.schema.yaml` / `loong_pinyin.dict.yaml` 写进用户目录，首次部署时由
//!   `luna_pinyin.dict.yaml` + `essay.txt` 现场编译（x86 约 4 秒，只发生一次）。
//!
//! 也可以改用 Rime 自带的 `luna_pinyin_simp`（词典约 23MB）：此时写入一个补丁，
//! 去掉对笔画（stroke）数据的依赖。

use std::path::{Path, PathBuf};

use crate::error::ImeError;

/// 系统共享的 Rime 数据目录（Alpine `rime-plum-data` 包和系统镜像都用这个位置）。
pub const DEFAULT_SHARED_DATA_DIR: &str = "/usr/share/rime-data";
/// 默认方案：Loong 拼音（全拼，输出简体中文）。
pub const DEFAULT_SCHEMA: &str = "loong_pinyin";
/// 默认每页候选数。
pub const DEFAULT_PAGE_SIZE: u8 = 9;

/// 本 crate 自带的方案文件。
const LOONG_PINYIN_SCHEMA: &str = include_str!("../data/loong_pinyin.schema.yaml");
/// 本 crate 自带的词典定义（编译时导入 luna_pinyin 与 essay）。
const LOONG_PINYIN_DICT: &str = include_str!("../data/loong_pinyin.dict.yaml");
/// 生成文件的标记：带这个标记的文件由 loong-ime 管理，可以覆盖或删除。
const MANAGED_MARK: &str = "crates/loong-ime";

/// librime 日志级别（glog 的 `minloglevel`）。日志只写到 stderr。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum LogLevel {
    /// 信息（部署时很啰嗦）。
    Info = 0,
    /// 警告。
    Warning = 1,
    /// 错误（默认）。
    Error = 2,
    /// 只记录致命错误。
    Fatal = 3,
}

/// 输入法配置。
#[derive(Debug, Clone, PartialEq)]
pub struct ImeConfig {
    /// 共享数据目录（只读，随系统镜像发布）。
    pub shared_data_dir: PathBuf,
    /// 用户数据目录（可写）：部署产物 `build/`、用户词库 `*.userdb/`、补丁文件。
    pub user_data_dir: PathBuf,
    /// 方案 ID。
    pub schema: String,
    /// 每页候选数（1–10；组字时数字键 1–9、0 选词）。
    pub page_size: u8,
    /// librime 日志级别。
    pub log_level: LogLevel,
}

impl Default for ImeConfig {
    fn default() -> Self {
        ImeConfig {
            shared_data_dir: PathBuf::from(DEFAULT_SHARED_DATA_DIR),
            user_data_dir: default_user_data_dir(),
            schema: DEFAULT_SCHEMA.to_owned(),
            page_size: DEFAULT_PAGE_SIZE,
            log_level: LogLevel::Error,
        }
    }
}

impl ImeConfig {
    /// 使用默认共享目录与方案、指定用户目录的配置。
    pub fn with_user_data_dir(dir: impl Into<PathBuf>) -> Self {
        ImeConfig {
            user_data_dir: dir.into(),
            ..ImeConfig::default()
        }
    }
}

/// 默认用户数据目录：`$XDG_DATA_HOME/loong/rime`，未设置时为 `~/.local/share/loong/rime`。
pub fn default_user_data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".local/share"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("loong/rime")
}

/// 共享目录里是否有某个文件。
fn shared_has(config: &ImeConfig, name: &str) -> bool {
    config.shared_data_dir.join(name).is_file()
}

fn missing(config: &ImeConfig, name: &str) -> ImeError {
    ImeError::MissingData {
        path: config.shared_data_dir.join(name),
    }
}

/// 检查共享数据目录能否支撑所选方案。
pub(crate) fn check_shared_data(config: &ImeConfig) -> Result<(), ImeError> {
    if !shared_has(config, "default.yaml") {
        return Err(missing(config, "default.yaml"));
    }
    if config.schema == DEFAULT_SCHEMA {
        // 预编译好的词典（系统镜像），或可以现场编译的源文件（开发环境）。
        if !shared_has(config, "build/loong_pinyin.table.bin") {
            for source in ["luna_pinyin.dict.yaml", "essay.txt", "pinyin.yaml"] {
                if !shared_has(config, source) {
                    return Err(missing(config, source));
                }
            }
        }
        return Ok(());
    }
    let schema_file = format!("{}.schema.yaml", config.schema);
    if !shared_has(config, &schema_file) && !shared_has(config, &format!("build/{schema_file}")) {
        return Err(missing(config, &schema_file));
    }
    Ok(())
}

/// 写到用户目录的文件：`(文件名, Some(内容))`；`None` 表示应删除（如果是本 crate 写的）。
///
/// 这些文件由 loong-ime 管理，每次启动按配置覆盖（内容不变时不写）。
pub(crate) fn user_files(config: &ImeConfig) -> Vec<(String, Option<String>)> {
    let page_size = config.page_size.clamp(1, 10);
    let mut files = vec![(
        "default.custom.yaml".to_owned(),
        Some(format!(
            "\
# 由 {MANAGED_MARK} 生成（src/config.rs），每次启动按配置覆盖，请勿手改。
patch:
  schema_list:
    - schema: {schema}
  menu/page_size: {page_size}
  # 屏幕键盘用候选栏翻页、用 中/英 键切换。default.yaml 默认让逗号、句号、减号、
  # 等号兼作翻页键，在屏幕键盘上“nihao，”会变成翻页而不是上屏，这里全部去掉。
  key_binder/bindings: []
",
            schema = config.schema,
        )),
    )];
    if config.schema == DEFAULT_SCHEMA {
        // 共享目录（系统镜像）里有的就用共享的；没有才用本 crate 自带的。
        let schema = (!shared_has(config, "loong_pinyin.schema.yaml"))
            .then(|| LOONG_PINYIN_SCHEMA.to_owned());
        let dict = (!shared_has(config, "build/loong_pinyin.table.bin"))
            .then(|| LOONG_PINYIN_DICT.to_owned());
        files.push(("loong_pinyin.schema.yaml".to_owned(), schema));
        files.push(("loong_pinyin.dict.yaml".to_owned(), dict));
    } else if config.schema.starts_with("luna_pinyin") {
        files.push((
            format!("{}.custom.yaml", config.schema),
            Some(format!(
                "\
# 由 {MANAGED_MARK} 生成（src/config.rs），请勿手改。
patch:
  # 不使用笔画反查和自定义短语：不依赖 stroke 方案的数据（约 8MB）。
  schema/dependencies: []
  engine/translators:
    - punct_translator
    - script_translator
  recognizer/patterns/reverse_lookup: null
"
            )),
        ));
    }
    files
}

/// 创建用户目录并写入（或清理）由 loong-ime 管理的文件。
///
/// 内容没变的文件不重写，保持修改时间不变，这样 librime 不会在每次启动时都重新部署。
pub(crate) fn write_user_files(config: &ImeConfig) -> Result<(), ImeError> {
    let dir = &config.user_data_dir;
    std::fs::create_dir_all(dir).map_err(|e| ImeError::Io(e, dir.clone()))?;
    for (name, content) in user_files(config) {
        let path = dir.join(name);
        let existing = read_if_exists(&path);
        match content {
            Some(content) => {
                if existing.as_deref() == Some(content.as_str()) {
                    continue;
                }
                std::fs::write(&path, content).map_err(|e| ImeError::Io(e, path.clone()))?;
            }
            // 共享目录里已有正式版本：删掉以前写入的副本，避免遮住共享数据。
            None if existing.is_some_and(|s| s.contains(MANAGED_MARK)) => {
                std::fs::remove_file(&path).map_err(|e| ImeError::Io(e, path.clone()))?;
            }
            None => {}
        }
    }
    Ok(())
}

fn read_if_exists(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loong-ime-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn content<'a>(files: &'a [(String, Option<String>)], name: &str) -> Option<&'a str> {
        files
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, c)| c.as_deref())
    }

    #[test]
    fn default_custom_selects_schema_and_page_size() {
        let config = ImeConfig {
            shared_data_dir: PathBuf::from("/nonexistent"),
            page_size: 30,
            ..ImeConfig::default()
        };
        let files = user_files(&config);
        let default = content(&files, "default.custom.yaml").unwrap();
        assert!(default.contains("- schema: loong_pinyin"));
        assert!(default.contains("menu/page_size: 10"), "页大小被限制到 10");
        // 共享目录里没有 loong_pinyin：使用自带的方案与词典定义。
        assert_eq!(
            content(&files, "loong_pinyin.schema.yaml"),
            Some(LOONG_PINYIN_SCHEMA)
        );
        assert_eq!(
            content(&files, "loong_pinyin.dict.yaml"),
            Some(LOONG_PINYIN_DICT)
        );
    }

    #[test]
    fn prebuilt_shared_data_wins_over_bundled_files() {
        let shared = temp_dir("shared");
        let user = temp_dir("user");
        std::fs::create_dir_all(shared.join("build")).unwrap();
        std::fs::write(shared.join("loong_pinyin.schema.yaml"), "").unwrap();
        std::fs::write(shared.join("build/loong_pinyin.table.bin"), "").unwrap();
        // 以前（开发环境）写入的副本
        std::fs::write(user.join("loong_pinyin.dict.yaml"), LOONG_PINYIN_DICT).unwrap();
        let config = ImeConfig {
            shared_data_dir: shared.clone(),
            user_data_dir: user.clone(),
            ..ImeConfig::default()
        };
        write_user_files(&config).unwrap();
        assert!(user.join("default.custom.yaml").is_file());
        assert!(
            !user.join("loong_pinyin.dict.yaml").exists(),
            "旧副本应被删除"
        );
        assert!(!user.join("loong_pinyin.schema.yaml").exists());
        std::fs::remove_dir_all(shared).unwrap();
        std::fs::remove_dir_all(user).unwrap();
    }

    #[test]
    fn luna_pinyin_gets_stroke_free_patch() {
        let config = ImeConfig {
            schema: "luna_pinyin_simp".into(),
            ..ImeConfig::default()
        };
        let files = user_files(&config);
        let patch = content(&files, "luna_pinyin_simp.custom.yaml").unwrap();
        assert!(patch.contains("schema/dependencies: []"));
    }

    #[test]
    fn user_files_written_once() {
        let dir = temp_dir("cfg");
        let config = ImeConfig::with_user_data_dir(&dir);
        write_user_files(&config).unwrap();
        let path = dir.join("default.custom.yaml");
        let t1 = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_user_files(&config).unwrap();
        let t2 = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(t1, t2, "内容不变时不应重写（否则每次启动都会重新部署）");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_data_is_reported() {
        let config = ImeConfig {
            shared_data_dir: PathBuf::from("/nonexistent/rime-data"),
            ..ImeConfig::default()
        };
        assert!(matches!(
            check_shared_data(&config),
            Err(ImeError::MissingData { .. })
        ));
    }
}
