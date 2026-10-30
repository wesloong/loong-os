//! 对外的 D-Bus 错误：`org.loong.System1.Error.*`。

use zbus::DBusError;

/// loong-sysd 方法返回的错误。D-Bus 错误名为 `org.loong.System1.Error.<变体名>`，
/// 错误消息是给人看的中文说明。
#[derive(Debug, DBusError)]
#[zbus(prefix = "org.loong.System1.Error")]
pub enum Error {
    /// D-Bus 层面的错误（转换为对应的标准错误）。
    #[zbus(error)]
    ZBus(zbus::Error),
    /// 调用者无权执行写操作（只允许 root 与会话用户 loong）。
    AccessDenied(String),
    /// 本机没有这项硬件（例如没有背光、没有电池）。
    NotSupported(String),
    /// 依赖的系统服务没有运行（例如 iwd）。
    Unavailable(String),
    /// 参数不合法。
    InvalidArgs(String),
    /// 找不到对象（例如指定的 Wi‑Fi 网络）。
    NotFound(String),
    /// 正在执行同类操作（例如正在扫描、正在连接）。
    Busy(String),
    /// 其他失败（消息中带有底层错误）。
    Failed(String),
}
