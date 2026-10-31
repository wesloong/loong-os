//! 错误类型。
//!
//! 每个错误有一个 [`ErrorKind`]（给程序判断，D-Bus 上映射成 `org.loong.Update1.Error.<种类>`）
//! 和一段给人看的中文说明。

use std::fmt;

/// 错误种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 不是合法的更新包（格式错误、缺文件、清单无效）。
    InvalidBundle,
    /// 清单签名无效，或没有可信的公钥。
    BadSignature,
    /// 载荷的 SHA-256 或大小与清单不符；或写入后回读校验失败。
    HashMismatch,
    /// 更新包与本机不匹配（机型、架构、启动后端、最低版本）。
    Incompatible,
    /// 更新包的版本不比当前系统新（未允许降级）。
    NotNewer,
    /// 目标分区放不下载荷。
    TooLarge,
    /// 本次启动还没有确认成功：先确认（或回滚），再安装新的更新。
    Unconfirmed,
    /// 另一个更新操作正在进行。
    Busy,
    /// 被取消。
    Cancelled,
    /// 本机不支持（没有更新配置、找不到分区等）。
    NotSupported,
    /// 输入输出错误。
    Io,
    /// 其他失败。
    Failed,
}

impl ErrorKind {
    /// 用于 D-Bus 错误名与日志的英文名。
    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::InvalidBundle => "InvalidBundle",
            ErrorKind::BadSignature => "BadSignature",
            ErrorKind::HashMismatch => "HashMismatch",
            ErrorKind::Incompatible => "Incompatible",
            ErrorKind::NotNewer => "NotNewer",
            ErrorKind::TooLarge => "TooLarge",
            ErrorKind::Unconfirmed => "Unconfirmed",
            ErrorKind::Busy => "Busy",
            ErrorKind::Cancelled => "Cancelled",
            ErrorKind::NotSupported => "NotSupported",
            ErrorKind::Io => "Io",
            ErrorKind::Failed => "Failed",
        }
    }
}

/// loong-update 的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

/// 本库的 `Result`。
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// 构造错误。
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Error {
        Error {
            kind,
            message: message.into(),
        }
    }

    /// 错误种类。
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// 给人看的说明。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 在说明前面加上下文（例如“写入 root_b”）。
    pub fn context(self, what: impl fmt::Display) -> Error {
        Error {
            kind: self.kind,
            message: format!("{what}: {}", self.message),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        if is_cancelled(&e) {
            return Error::new(ErrorKind::Cancelled, "已取消");
        }
        Error::new(ErrorKind::Io, e.to_string())
    }
}

/// 数据流里表示“已取消”的 I/O 错误（取消检查发生在 `Read` 实现里，只能用 `io::Error` 往外传）。
#[derive(Debug)]
struct CancelledIo;

impl fmt::Display for CancelledIo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("已取消")
    }
}

impl std::error::Error for CancelledIo {}

/// 构造“已取消”的 I/O 错误。
pub(crate) fn cancelled_io() -> std::io::Error {
    std::io::Error::other(CancelledIo)
}

fn is_cancelled(e: &std::io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<CancelledIo>())
}

/// 构造错误的简写：`err!(InvalidBundle, "…{x}…")`。
macro_rules! err {
    ($kind:ident, $($arg:tt)*) => {
        $crate::error::Error::new($crate::error::ErrorKind::$kind, format!($($arg)*))
    };
}
pub(crate) use err;

/// 给 `io::Result` 加上下文（路径等），转成 [`Error`]。
pub(crate) trait IoContext<T> {
    fn ctx(self, what: impl fmt::Display) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn ctx(self, what: impl fmt::Display) -> Result<T> {
        self.map_err(|e| {
            if is_cancelled(&e) {
                Error::from(e)
            } else {
                Error::new(ErrorKind::Io, format!("{what}: {e}"))
            }
        })
    }
}
