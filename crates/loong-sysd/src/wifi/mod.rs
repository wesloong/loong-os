//! Wi‑Fi：`/org/loong/System1/Wifi`，对 iwd（`net.connman.iwd`）的一层薄封装。
//!
//! - 后台任务订阅 iwd 的所有信号和 iwd 总线名的变化，去抖后用 `GetManagedObjects` +
//!   `Station.GetOrderedNetworks` 重新生成快照，比较后发 `PropertiesChanged` 与
//!   `NetworksChanged`。信号强度没有变化通知，所以每 30 秒也刷新一次。
//! - 连接需要密码的网络时，iwd 会回调 Agent（`net.connman.iwd.Agent`）的
//!   `RequestPassphrase`。loong-sysd 在 `/org/loong/System1/Wifi/Agent` 实现 Agent，
//!   iwd 每次（重新）启动后自动注册；`Connect(ssid, passphrase)` 把密码暂存，iwd 来取时交给它，
//!   连接结束后立即丢弃。Agent 只接受当前 iwd 进程的调用。
//! - iwd 没有运行时：`Available=false`、`State="unavailable"`，写操作返回 `Unavailable`；
//!   iwd 启动后自动恢复。

pub mod iwd;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use async_io::Timer;
use futures_lite::{FutureExt, StreamExt, future};
use serde::{Serialize, de::DeserializeOwned};
use zbus::{
    Connection, DBusError, MatchRule, MessageStream, interface,
    message::{Header, Type as MessageType},
    object_server::SignalEmitter,
    zvariant::{DynamicType, OwnedObjectPath, Value},
};

use self::iwd::{NetworkInfo, Snapshot};
use crate::{
    access,
    bus::{self, Changed},
    error::Error,
};

/// 对象路径。
pub const PATH: &str = "/org/loong/System1/Wifi";
/// 接口名。
pub const INTERFACE: &str = "org.loong.System1.Wifi";
/// iwd 回调用的 Agent 对象路径。
pub const AGENT_PATH: &str = "/org/loong/System1/Wifi/Agent";

/// 没有信号时的定时刷新间隔（更新信号强度）。
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
/// iwd 的信号通常成串出现，等安静这么久再刷新。
const DEBOUNCE: Duration = Duration::from_millis(150);

/// 对外的属性。
#[derive(Debug, Clone, PartialEq)]
struct Props {
    available: bool,
    enabled: bool,
    state: String,
    scanning: bool,
    ssid: String,
    strength: u32,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            available: false,
            enabled: false,
            state: "unavailable".into(),
            scanning: false,
            ssid: String::new(),
            strength: 0,
        }
    }
}

impl Props {
    fn from_snapshot(snap: &Snapshot, networks: &[NetworkInfo]) -> Props {
        let Some(dev) = &snap.device else {
            return Props::default();
        };
        let enabled = dev.powered && dev.adapter_powered;
        let station = dev.station.as_ref().filter(|_| enabled);
        let ssid = station
            .and_then(|s| s.connected_network.as_ref())
            .and_then(|p| snap.networks.iter().find(|n| n.path == *p))
            .map(|n| n.name.clone())
            .unwrap_or_default();
        let strength = networks
            .iter()
            .find(|n| n.connected)
            .map_or(0, |n| n.strength);
        Props {
            available: true,
            enabled,
            state: match station {
                Some(s) => s.state.clone(),
                None if enabled => "disconnected".into(),
                None => "off".into(),
            },
            scanning: station.is_some_and(|s| s.scanning),
            ssid,
            strength,
        }
    }

    fn diff(&self, new: &Props) -> Changed {
        let mut changed: Changed = vec![];
        if self.available != new.available {
            changed.push(("Available", bus::owned(new.available)));
        }
        if self.enabled != new.enabled {
            changed.push(("Enabled", bus::owned(new.enabled)));
        }
        if self.state != new.state {
            changed.push(("State", bus::owned(new.state.as_str())));
        }
        if self.scanning != new.scanning {
            changed.push(("Scanning", bus::owned(new.scanning)));
        }
        if self.ssid != new.ssid {
            changed.push(("Ssid", bus::owned(new.ssid.as_str())));
        }
        if self.strength != new.strength {
            changed.push(("Strength", bus::owned(new.strength)));
        }
        changed
    }
}

#[derive(Default)]
struct Inner {
    /// iwd 当前的唯一总线名（没有运行时为 `None`）。
    iwd_owner: Option<String>,
    /// Agent 已经向哪个 iwd 进程注册。
    agent_registered_with: Option<String>,
    snapshot: Snapshot,
    networks: Vec<NetworkInfo>,
    props: Props,
    /// 等待 iwd 来取的密码：网络对象路径 → 密码。
    pending: HashMap<OwnedObjectPath, String>,
}

/// Wi‑Fi 状态。接口对象、Agent 与后台任务共享。
#[derive(Default)]
pub struct Wifi {
    inner: Mutex<Inner>,
}

/// 把 iwd 返回的错误转换为对外错误。
fn iwd_error(e: zbus::Error) -> Error {
    match &e {
        zbus::Error::MethodError(name, msg, _) => {
            let detail = msg.clone().unwrap_or_default();
            let short = name.as_str().trim_start_matches("net.connman.iwd.");
            match name.as_str() {
                "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner" => {
                    Error::Unavailable("iwd 没有运行".into())
                }
                "net.connman.iwd.InvalidFormat" | "net.connman.iwd.InvalidArguments" => {
                    Error::InvalidArgs(format!("iwd: {short}: {detail}"))
                }
                "net.connman.iwd.Busy" | "net.connman.iwd.InProgress" => {
                    Error::Busy(format!("iwd: {short}: {detail}"))
                }
                "net.connman.iwd.NotFound" => Error::NotFound(format!("iwd: {detail}")),
                "net.connman.iwd.NotSupported" | "net.connman.iwd.NotImplemented" => {
                    Error::NotSupported(format!("iwd: {short}: {detail}"))
                }
                _ => Error::Failed(format!("iwd: {short}: {detail}")),
            }
        }
        _ => Error::Failed(format!("iwd: {e}")),
    }
}

/// 调用 iwd 的方法。
async fn iwd_call<B, R>(
    conn: &Connection,
    path: &str,
    interface: &str,
    method: &str,
    body: &B,
) -> Result<R, Error>
where
    B: Serialize + DynamicType,
    R: DeserializeOwned + zbus::zvariant::Type,
{
    let reply = conn
        .call_method(Some(iwd::SERVICE), path, Some(interface), method, body)
        .await
        .map_err(iwd_error)?;
    Ok(reply.body().deserialize::<R>()?)
}

/// 设置 iwd 对象的属性。
async fn iwd_set(
    conn: &Connection,
    path: &str,
    interface: &str,
    name: &str,
    value: Value<'_>,
) -> Result<(), Error> {
    iwd_call(
        conn,
        path,
        "org.freedesktop.DBus.Properties",
        "Set",
        &(interface, name, value),
    )
    .await
}

/// 查询某个总线名的当前所有者。
async fn name_owner(conn: &Connection, name: &str) -> Option<String> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &(name,),
        )
        .await
        .ok()?;
    reply.body().deserialize::<String>().ok()
}

impl Wifi {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn props(&self) -> Props {
        self.lock().props.clone()
    }

    fn networks(&self) -> Vec<NetworkInfo> {
        self.lock().networks.clone()
    }

    /// 写操作前的检查：iwd 在运行且有 Wi‑Fi 设备。返回快照副本。
    fn ready(&self) -> Result<Snapshot, Error> {
        let inner = self.lock();
        if inner.iwd_owner.is_none() {
            return Err(Error::Unavailable("iwd 没有运行".into()));
        }
        if inner.snapshot.device.is_none() {
            return Err(Error::NotSupported("没有 Wi‑Fi 设备".into()));
        }
        Ok(inner.snapshot.clone())
    }

    /// 需要 Station（设备已开启、客户端模式）时的检查。返回设备路径。
    fn station_path(snap: &Snapshot) -> Result<String, Error> {
        match &snap.device {
            Some(d) if d.station.is_some() => Ok(d.path.to_string()),
            _ => Err(Error::Failed("Wi‑Fi 已关闭".into())),
        }
    }

    /// 重新读取 iwd 的状态并发出变化信号。只由后台任务调用（保证顺序）。
    async fn refresh(&self, conn: &Connection) {
        let owner = name_owner(conn, iwd::SERVICE).await;
        let mut snapshot = Snapshot::default();
        let mut ordered = vec![];
        if let Some(owner) = &owner {
            let registered = self.lock().agent_registered_with.clone();
            if registered.as_ref() != Some(owner) {
                self.register_agent(conn, owner).await;
            }
            match iwd_call::<_, iwd::ManagedObjects>(
                conn,
                "/",
                "org.freedesktop.DBus.ObjectManager",
                "GetManagedObjects",
                &(),
            )
            .await
            {
                Ok(objects) => snapshot = iwd::parse(&objects),
                Err(e) => tracing::warn!("读取 iwd 状态失败: {e}"),
            }
            if let Some(dev) = snapshot.device.as_ref().filter(|d| d.station.is_some()) {
                ordered = iwd_call(
                    conn,
                    dev.path.as_str(),
                    iwd::STATION,
                    "GetOrderedNetworks",
                    &(),
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("读取网络列表失败: {e}");
                    vec![]
                });
            }
        }
        let networks = iwd::network_list(&snapshot, &ordered);
        let props = Props::from_snapshot(&snapshot, &networks);

        let (changed, networks_changed) = {
            let mut inner = self.lock();
            if owner.is_none() {
                inner.agent_registered_with = None;
            }
            inner.iwd_owner = owner;
            inner.snapshot = snapshot;
            let changed = inner.props.diff(&props);
            inner.props = props;
            let networks_changed = inner.networks != networks;
            inner.networks = networks;
            (changed, networks_changed)
        };
        bus::emit_changed(conn, PATH, INTERFACE, &changed).await;
        if networks_changed {
            let result = match SignalEmitter::new(conn, PATH) {
                Ok(emitter) => WifiIface::networks_changed(&emitter).await,
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                tracing::warn!("发送 NetworksChanged 失败: {e}");
            }
        }
    }

    async fn register_agent(&self, conn: &Connection, owner: &str) {
        let agent = zbus::zvariant::ObjectPath::from_static_str_unchecked(AGENT_PATH);
        match iwd_call::<_, ()>(
            conn,
            iwd::AGENT_MANAGER_PATH,
            iwd::AGENT_MANAGER,
            "RegisterAgent",
            &(agent,),
        )
        .await
        {
            Ok(()) => {
                tracing::info!("已向 iwd（{owner}）注册 Agent");
                self.lock().agent_registered_with = Some(owner.to_owned());
            }
            Err(e) => tracing::warn!("向 iwd 注册 Agent 失败: {e}"),
        }
    }

    /// 确认 Agent 调用来自当前的 iwd 进程。
    async fn check_iwd_caller(&self, conn: &Connection, hdr: &Header<'_>) -> bool {
        let Some(sender) = hdr.sender() else {
            return false;
        };
        if self.lock().iwd_owner.as_deref() == Some(sender.as_str()) {
            return true;
        }
        // 缓存可能还没更新（iwd 刚重启）：再问一次总线。
        name_owner(conn, iwd::SERVICE).await.as_deref() == Some(sender.as_str())
    }

    async fn set_enabled(&self, conn: &Connection, enabled: bool) -> Result<(), Error> {
        let snap = self.ready()?;
        let dev = snap.device.expect("ready() 已检查设备");
        if enabled
            && !dev.adapter_powered
            && let Some(adapter) = &dev.adapter
        {
            iwd_set(conn, adapter.as_str(), iwd::ADAPTER, "Powered", true.into()).await?;
        }
        iwd_set(
            conn,
            dev.path.as_str(),
            iwd::DEVICE,
            "Powered",
            enabled.into(),
        )
        .await
    }

    async fn scan(&self, conn: &Connection) -> Result<(), Error> {
        let snap = self.ready()?;
        let station = Self::station_path(&snap)?;
        match iwd_call::<_, ()>(conn, &station, iwd::STATION, "Scan", &()).await {
            // 已经在扫描：结果一样，不算错误。
            Err(Error::Busy(_)) => Ok(()),
            other => other,
        }
    }

    async fn connect(
        &self,
        conn: &Connection,
        ssid: &str,
        passphrase: String,
    ) -> Result<(), Error> {
        let snap = self.ready()?;
        Self::station_path(&snap)?;
        let networks = self.networks();
        // 同名网络可能有多个（例如开放和加密各一个）：优先已保存的，其次信号最强的。
        let rank = |path: &OwnedObjectPath| {
            snap.networks
                .iter()
                .find(|n| n.path == *path)
                .and_then(|n| {
                    networks
                        .iter()
                        .position(|i| i.ssid == n.name && i.security == n.kind)
                })
                .unwrap_or(usize::MAX)
        };
        let net = snap
            .networks
            .iter()
            .filter(|n| n.name == ssid)
            .min_by_key(|n| (n.known.is_none(), rank(&n.path)))
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("附近没有名为 “{ssid}” 的网络")))?;

        match net.kind.as_str() {
            "wep" => return Err(Error::NotSupported("不支持 WEP 网络".into())),
            "8021x" if net.known.is_none() => {
                return Err(Error::NotSupported("暂不支持连接企业网络（802.1X）".into()));
            }
            "psk" if net.known.is_none() && passphrase.is_empty() => {
                return Err(Error::InvalidArgs(format!("连接 “{ssid}” 需要密码")));
            }
            _ => {}
        }
        if !passphrase.is_empty() {
            // 给已保存的网络输入了新密码：先忘掉旧配置，确保用的是这次输入的密码。
            if let Some(known) = &net.known {
                let _ = iwd_call::<_, ()>(conn, known.as_str(), iwd::KNOWN_NETWORK, "Forget", &())
                    .await;
            }
            self.lock().pending.insert(net.path.clone(), passphrase);
        }
        tracing::info!("连接 Wi‑Fi “{ssid}”（{}）", net.kind);
        let result = iwd_call::<_, ()>(conn, net.path.as_str(), iwd::NETWORK, "Connect", &()).await;
        self.lock().pending.remove(&net.path);
        if let Err(e) = &result {
            tracing::info!("连接 “{ssid}” 失败: {e}");
        }
        result
    }

    async fn disconnect(&self, conn: &Connection) -> Result<(), Error> {
        let snap = self.ready()?;
        let station = Self::station_path(&snap)?;
        iwd_call(conn, &station, iwd::STATION, "Disconnect", &()).await
    }

    async fn forget(&self, conn: &Connection, ssid: &str) -> Result<(), Error> {
        let snap = self.ready()?;
        let known: Vec<_> = snap.known.iter().filter(|k| k.name == ssid).collect();
        if known.is_empty() {
            return Err(Error::NotFound(format!("没有保存名为 “{ssid}” 的网络")));
        }
        for k in known {
            iwd_call::<_, ()>(conn, k.path.as_str(), iwd::KNOWN_NETWORK, "Forget", &()).await?;
        }
        Ok(())
    }
}

/// 后台任务：监视 iwd 并保持快照最新。
pub async fn run_watcher(wifi: Arc<Wifi>, conn: Connection) {
    let rules = async {
        let iwd_signals = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(iwd::SERVICE)?
            .build();
        let owner_changes = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .add_arg(iwd::SERVICE)?
            .build();
        let a = MessageStream::for_match_rule(iwd_signals, &conn, Some(64)).await?;
        let b = MessageStream::for_match_rule(owner_changes, &conn, Some(8)).await?;
        zbus::Result::Ok(a.or(b))
    };
    let mut events = match rules.await {
        Ok(events) => events,
        Err(e) => {
            tracing::warn!("无法订阅 iwd 信号（{e}），Wi‑Fi 状态只靠定时刷新");
            wifi.refresh(&conn).await;
            loop {
                Timer::after(REFRESH_INTERVAL).await;
                wifi.refresh(&conn).await;
            }
        }
    };

    wifi.refresh(&conn).await;
    loop {
        // 等 iwd 的信号，或者到时间定时刷新。
        let event = async { events.next().await.is_some() }
            .or(async {
                Timer::after(REFRESH_INTERVAL).await;
                true
            })
            .await;
        if !event {
            // 连接已断开；主循环会退出进程。
            return;
        }
        // 去抖：等信号停下来再刷新。
        while future::or(async { events.next().await.is_some() }, async {
            Timer::after(DEBOUNCE).await;
            false
        })
        .await
        {}
        wifi.refresh(&conn).await;
    }
}

/// D-Bus 接口 `org.loong.System1.Wifi`。
pub struct WifiIface(pub Arc<Wifi>);

#[interface(name = "org.loong.System1.Wifi")]
impl WifiIface {
    /// iwd 在运行且有 Wi‑Fi 网卡。
    #[zbus(property)]
    fn available(&self) -> bool {
        self.0.props().available
    }

    /// Wi‑Fi 是否开启。
    #[zbus(property)]
    fn enabled(&self) -> bool {
        self.0.props().enabled
    }

    /// `unavailable` / `off` / `disconnected` / `connecting` / `connected` /
    /// `disconnecting` / `roaming`。
    #[zbus(property)]
    fn state(&self) -> String {
        self.0.props().state
    }

    /// 是否正在扫描。
    #[zbus(property)]
    fn scanning(&self) -> bool {
        self.0.props().scanning
    }

    /// 当前连接的网络名（未连接时为空）。
    #[zbus(property)]
    fn ssid(&self) -> String {
        self.0.props().ssid
    }

    /// 当前连接的信号强度 0–100（未连接时为 0）。
    #[zbus(property)]
    fn strength(&self) -> u32 {
        self.0.props().strength
    }

    /// 附近的网络，按信号从强到弱排列。对所有人开放。
    fn get_networks(&self) -> Vec<NetworkInfo> {
        self.0.networks()
    }

    /// 开关 Wi‑Fi。仅限 root 与 loong。
    async fn set_enabled(
        &self,
        enabled: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), Error> {
        access::require_privileged(conn, &hdr).await?;
        self.0.set_enabled(conn, enabled).await
    }

    /// 开始扫描；结果通过 `NetworksChanged` 通知。仅限 root 与 loong。
    async fn scan(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), Error> {
        access::require_privileged(conn, &hdr).await?;
        self.0.scan(conn).await
    }

    /// 连接网络，等到连上或失败才返回。已保存的网络或开放网络 `passphrase` 传空字符串。
    /// 仅限 root 与 loong。
    #[zbus(name = "Connect")]
    async fn connect_network(
        &self,
        ssid: String,
        passphrase: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), Error> {
        access::require_privileged(conn, &hdr).await?;
        self.0.connect(conn, &ssid, passphrase).await
    }

    /// 断开当前连接（不会忘记网络）。仅限 root 与 loong。
    async fn disconnect(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), Error> {
        access::require_privileged(conn, &hdr).await?;
        self.0.disconnect(conn).await
    }

    /// 忘记已保存的网络（删除密码，不再自动连接）。仅限 root 与 loong。
    async fn forget(
        &self,
        ssid: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), Error> {
        access::require_privileged(conn, &hdr).await?;
        self.0.forget(conn, &ssid).await
    }

    /// 网络列表（`GetNetworks` 的结果）变了。
    #[zbus(signal)]
    pub async fn networks_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// iwd Agent 返回的错误：`net.connman.iwd.Agent.Error.Canceled`。
#[derive(Debug, DBusError)]
#[zbus(prefix = "net.connman.iwd.Agent.Error")]
pub enum AgentError {
    /// D-Bus 层面的错误。
    #[zbus(error)]
    ZBus(zbus::Error),
    /// 取消（没有密码、调用者不是 iwd、或不支持的认证方式）。
    Canceled(String),
}

/// iwd 回调的 Agent（`net.connman.iwd.Agent`），只服务当前的 iwd 进程。
pub struct Agent(pub Arc<Wifi>);

impl Agent {
    async fn guard(&self, conn: &Connection, hdr: &Header<'_>) -> Result<(), AgentError> {
        if self.0.check_iwd_caller(conn, hdr).await {
            Ok(())
        } else {
            tracing::warn!("拒绝非 iwd 进程调用 Agent");
            Err(AgentError::Canceled("只接受 iwd 的调用".into()))
        }
    }
}

#[interface(name = "net.connman.iwd.Agent")]
impl Agent {
    /// iwd 不再使用这个 Agent（例如 iwd 退出）。
    async fn release(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.guard(conn, &hdr).await?;
        self.0.lock().agent_registered_with = None;
        Ok(())
    }

    /// 返回 `Connect` 暂存的密码。
    async fn request_passphrase(
        &self,
        network: OwnedObjectPath,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<String, AgentError> {
        self.guard(conn, &hdr).await?;
        self.0
            .lock()
            .pending
            .get(&network)
            .cloned()
            .ok_or_else(|| AgentError::Canceled("没有为这个网络提供密码".into()))
    }

    /// 802.1X 私钥密码：v1 不支持。
    async fn request_private_key_passphrase(
        &self,
        _network: OwnedObjectPath,
    ) -> Result<String, AgentError> {
        Err(AgentError::Canceled("不支持".into()))
    }

    /// 802.1X 用户名与密码：v1 不支持。
    async fn request_user_name_and_password(
        &self,
        _network: OwnedObjectPath,
    ) -> Result<(String, String), AgentError> {
        Err(AgentError::Canceled("不支持".into()))
    }

    /// 802.1X 用户密码：v1 不支持。
    async fn request_user_password(
        &self,
        _network: OwnedObjectPath,
        _user: String,
    ) -> Result<String, AgentError> {
        Err(AgentError::Canceled("不支持".into()))
    }

    /// iwd 取消了正在进行的请求。
    async fn cancel(&self, reason: String) {
        tracing::debug!("iwd 取消了 Agent 请求: {reason}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::iwd::tests::{sample, sample_ordered};

    #[test]
    fn props_from_typical_snapshot() {
        let snap = iwd::parse(&sample());
        let nets = iwd::network_list(&snap, &sample_ordered());
        let p = Props::from_snapshot(&snap, &nets);
        assert_eq!(
            p,
            Props {
                available: true,
                enabled: true,
                state: "connected".into(),
                scanning: false,
                ssid: "Home".into(),
                strength: 75,
            }
        );
        let names: Vec<_> = Props::default().diff(&p).iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["Available", "Enabled", "State", "Ssid", "Strength"]);
        assert!(p.diff(&p).is_empty());
    }

    #[test]
    fn props_without_iwd_or_device() {
        let p = Props::from_snapshot(&Snapshot::default(), &[]);
        assert_eq!(p, Props::default());
        assert_eq!(p.state, "unavailable");
        assert!(!p.available);
    }

    #[test]
    fn props_when_powered_off() {
        let mut snap = iwd::parse(&sample());
        let dev = snap.device.as_mut().unwrap();
        dev.powered = false;
        dev.station = None;
        let p = Props::from_snapshot(&snap, &[]);
        assert!(p.available && !p.enabled);
        assert_eq!(p.state, "off");
        assert_eq!(p.ssid, "");
    }

    #[test]
    fn iwd_errors_are_mapped() {
        let err = |name: &str| {
            let msg = zbus::message::Message::method_call("/", "X")
                .unwrap()
                .build(&())
                .unwrap();
            iwd_error(zbus::Error::MethodError(
                name.try_into().unwrap(),
                Some("详情".into()),
                msg,
            ))
        };
        assert!(matches!(err("net.connman.iwd.Busy"), Error::Busy(_)));
        assert!(matches!(
            err("net.connman.iwd.InvalidFormat"),
            Error::InvalidArgs(_)
        ));
        assert!(matches!(
            err("net.connman.iwd.NotFound"),
            Error::NotFound(_)
        ));
        assert!(matches!(
            err("org.freedesktop.DBus.Error.ServiceUnknown"),
            Error::Unavailable(_)
        ));
        match err("net.connman.iwd.Failed") {
            Error::Failed(m) => assert_eq!(m, "iwd: Failed: 详情"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn not_ready_without_iwd() {
        let wifi = Wifi::default();
        assert!(matches!(wifi.ready(), Err(Error::Unavailable(_))));
        wifi.lock().iwd_owner = Some(":1.5".into());
        assert!(matches!(wifi.ready(), Err(Error::NotSupported(_))));
        wifi.lock().snapshot = iwd::parse(&sample());
        let snap = wifi.ready().unwrap();
        assert_eq!(Wifi::station_path(&snap).unwrap(), "/net/connman/iwd/0/4");
    }
}
