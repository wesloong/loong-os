//! 错误类型。

use std::fmt;
use std::path::PathBuf;

/// 输入法引擎的错误。
#[derive(Debug)]
pub enum ImeError {
    /// 共享数据目录缺少文件（通常是没装词库：`apk add rime-plum-data`）。
    MissingData {
        /// 缺少的文件。
        path: PathBuf,
    },
    /// 读写用户目录失败。
    Io(std::io::Error, PathBuf),
    /// 路径里有 NUL 字节，无法传给 C 接口。
    InvalidPath(PathBuf),
    /// librime 函数表里缺少需要的函数（库版本太旧）。
    MissingApi(&'static str),
    /// 本进程已用另一份配置启动了 librime（librime 是进程全局的）。
    AlreadyRunning {
        /// 已启动实例的用户目录。
        user_data_dir: PathBuf,
    },
    /// 部署（编译方案与词典）失败。
    DeployFailed {
        /// 用户目录（部署产物在其中的 `build/`）。
        user_data_dir: PathBuf,
    },
    /// 部署后加载的方案与配置不符。
    SchemaNotLoaded {
        /// 配置的方案。
        expected: String,
        /// 实际加载的方案（可能为空）。
        actual: String,
    },
    /// 创建会话失败。
    SessionCreate,
    /// 还有存活的会话，不能重新部署（先丢掉所有 `ImeEngine`）。
    SessionsAlive(usize),
}

impl fmt::Display for ImeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImeError::MissingData { path } => write!(
                f,
                "缺少 Rime 数据文件 {}（Alpine：apk add rime-plum-data）",
                path.display()
            ),
            ImeError::Io(e, path) => write!(f, "读写 {} 失败：{e}", path.display()),
            ImeError::InvalidPath(path) => write!(f, "路径含 NUL 字节：{}", path.display()),
            ImeError::MissingApi(name) => write!(f, "librime 缺少接口 {name}（版本太旧？）"),
            ImeError::AlreadyRunning { user_data_dir } => write!(
                f,
                "librime 已在本进程中以用户目录 {} 启动，不能换配置",
                user_data_dir.display()
            ),
            ImeError::DeployFailed { user_data_dir } => {
                write!(f, "Rime 部署失败（用户目录 {}）", user_data_dir.display())
            }
            ImeError::SchemaNotLoaded { expected, actual } => {
                write!(f, "Rime 方案 {expected} 没有加载成功（实际为 “{actual}”）")
            }
            ImeError::SessionCreate => write!(f, "无法创建 Rime 会话"),
            ImeError::SessionsAlive(n) => {
                write!(f, "还有 {n} 个 Rime 会话存活，不能重新部署")
            }
        }
    }
}

impl std::error::Error for ImeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ImeError::Io(e, _) => Some(e),
            _ => None,
        }
    }
}
