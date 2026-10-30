//! 总线名称与发信号的公共代码。

use std::{borrow::Cow, collections::HashMap};

use zbus::{Connection, names::InterfaceName, object_server::SignalEmitter, zvariant::Value};

/// loong-sysd 在系统总线上占用的名称。
pub const BUS_NAME: &str = "org.loong.System1";

/// 变化了的属性：(属性名, 新值)。
pub type Changed = Vec<(&'static str, Value<'static>)>;

/// 把一批属性变化合并成一个 `PropertiesChanged` 信号发出。
///
/// 后台任务（轮询、uevent、iwd 监视）发现变化时调用；方法内部的变化也可以用它。
/// 发送失败只记日志：信号丢了不影响服务本身，客户端可以随时重新读取属性。
pub async fn emit_changed(conn: &Connection, path: &str, interface: &str, changed: &Changed) {
    if changed.is_empty() {
        return;
    }
    let result = async {
        let emitter = SignalEmitter::new(conn, path)?;
        let map = changed
            .iter()
            .map(|(name, value)| Ok((*name, value.try_clone()?)))
            .collect::<zbus::zvariant::Result<HashMap<&str, Value<'_>>>>()?;
        zbus::fdo::Properties::properties_changed(
            &emitter,
            InterfaceName::try_from(interface)?,
            map,
            Cow::Borrowed(&[]),
        )
        .await
    }
    .await;
    if let Err(e) = result {
        tracing::warn!("发送 {interface} 的 PropertiesChanged 失败: {e}");
    }
}

/// 把普通值（数字、布尔、字符串）转换为不借用任何数据的 [`Value`]。
pub fn owned<'a>(value: impl Into<Value<'a>>) -> Value<'static> {
    value
        .into()
        .try_to_owned()
        .expect("不含文件描述符的值转换不会失败")
        .into()
}
