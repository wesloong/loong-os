//! 调用者鉴权。
//!
//! 读取属性、只读方法对所有人开放；会改变系统状态的方法（设置亮度、开关 Wi‑Fi、连接网络……）
//! 只允许 root（uid 0）和会话用户 `loong`（uid 1000）调用。总线策略文件
//! `org.loong.System1.conf` 已经挡掉了其他用户，这里按调用者的 uid 再复核一次，
//! 不依赖策略文件是否装对。

use zbus::{Connection, message::Header};

use crate::error::Error;

/// 会话用户 `loong` 的 uid（见 `system/mkimage/mkimage.sh`）。
pub const SESSION_UID: u32 = 1000;

/// 该 uid 是否可以调用写操作。
pub fn is_privileged(uid: u32) -> bool {
    uid == 0 || uid == SESSION_UID
}

/// 通过总线的 `GetConnectionUnixUser` 查询方法调用者的 uid。
pub async fn caller_uid(conn: &Connection, hdr: &Header<'_>) -> Result<u32, Error> {
    let sender = hdr
        .sender()
        .ok_or_else(|| Error::AccessDenied("无法确定调用者".into()))?;
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetConnectionUnixUser",
            &(sender.as_str(),),
        )
        .await?;
    Ok(reply.body().deserialize::<u32>()?)
}

/// 要求调用者有写权限，否则返回 `AccessDenied`。
pub async fn require_privileged(conn: &Connection, hdr: &Header<'_>) -> Result<(), Error> {
    let uid = caller_uid(conn, hdr).await?;
    if is_privileged(uid) {
        Ok(())
    } else {
        let member = hdr.member().map(|m| m.as_str()).unwrap_or("?");
        tracing::warn!("拒绝 uid {uid} 调用 {member}");
        Err(Error::AccessDenied(format!("uid {uid} 无权调用 {member}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_root_and_session_user_are_privileged() {
        assert!(is_privileged(0));
        assert!(is_privileged(1000));
        assert!(!is_privileged(1));
        assert!(!is_privileged(999));
        assert!(!is_privileged(1001));
        assert!(!is_privileged(65534));
    }
}
