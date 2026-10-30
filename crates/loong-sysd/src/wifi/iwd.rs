//! iwd D-Bus 接口（`net.connman.iwd`）的名称常量，以及把 `GetManagedObjects` 的结果
//! 解析成简单的快照。解析是纯函数，便于单元测试。
//!
//! iwd 的对象树（参见 iwd 源码 `doc/*.txt`）：
//!
//! ```text
//! /net/connman/iwd                     AgentManager
//! /net/connman/iwd/0                   Adapter（Powered）
//! /net/connman/iwd/0/4                 Device（Powered, Adapter, Mode）+ Station（State, Scanning, ConnectedNetwork）
//! /net/connman/iwd/0/4/<hex>_psk       Network（Name, Type, Connected, KnownNetwork）
//! /net/connman/iwd/<hex>_psk           KnownNetwork（Name, Type）
//! ```
//!
//! 设备关闭（Device.Powered=false）时 Station 接口会消失。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Type, Value};

/// iwd 的总线名。
pub const SERVICE: &str = "net.connman.iwd";
/// 无线网卡（phy）。
pub const ADAPTER: &str = "net.connman.iwd.Adapter";
/// 网络接口。
pub const DEVICE: &str = "net.connman.iwd.Device";
/// 客户端模式。
pub const STATION: &str = "net.connman.iwd.Station";
/// 扫描到的网络。
pub const NETWORK: &str = "net.connman.iwd.Network";
/// 已保存的网络。
pub const KNOWN_NETWORK: &str = "net.connman.iwd.KnownNetwork";
/// Agent 管理。
pub const AGENT_MANAGER: &str = "net.connman.iwd.AgentManager";
/// AgentManager 所在路径。
pub const AGENT_MANAGER_PATH: &str = "/net/connman/iwd";

/// `org.freedesktop.DBus.ObjectManager.GetManagedObjects` 的返回值。
pub type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// 选中的 Wi‑Fi 设备。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Device {
    /// 设备对象路径。
    pub path: OwnedObjectPath,
    /// Device.Powered。
    pub powered: bool,
    /// 所属 Adapter 的路径。
    pub adapter: Option<OwnedObjectPath>,
    /// Adapter.Powered（没有 Adapter 信息时视为 true）。
    pub adapter_powered: bool,
    /// Station 接口（设备关闭或不在客户端模式时没有）。
    pub station: Option<Station>,
}

/// Station 接口的属性。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Station {
    /// `connected` / `disconnected` / `connecting` / `disconnecting` / `roaming`。
    pub state: String,
    /// 是否正在扫描。
    pub scanning: bool,
    /// 当前连接的网络。
    pub connected_network: Option<OwnedObjectPath>,
}

/// 扫描到的一个网络（iwd 的 Network 对象）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetworkObj {
    /// 对象路径。
    pub path: OwnedObjectPath,
    /// SSID。
    pub name: String,
    /// 安全类型：`open` / `wep` / `psk` / `8021x`（WPA3-SAE 也报告为 `psk`）。
    pub kind: String,
    /// 是否已连接。
    pub connected: bool,
    /// 对应的已保存网络。
    pub known: Option<OwnedObjectPath>,
    /// 所属设备。
    pub device: Option<OwnedObjectPath>,
}

/// 已保存的网络。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KnownObj {
    /// 对象路径。
    pub path: OwnedObjectPath,
    /// SSID。
    pub name: String,
}

/// iwd 当前状态的快照。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// 选中的设备（没有 Wi‑Fi 网卡时为 `None`）。
    pub device: Option<Device>,
    /// 选中设备扫描到的网络。
    pub networks: Vec<NetworkObj>,
    /// 已保存的网络。
    pub known: Vec<KnownObj>,
}

type Props = HashMap<String, OwnedValue>;

fn prop<'a>(props: &'a Props, key: &str) -> Option<&'a Value<'static>> {
    props.get(key).map(|v| &**v)
}

fn bool_prop(props: &Props, key: &str) -> Option<bool> {
    match prop(props, key)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn str_prop(props: &Props, key: &str) -> Option<String> {
    match prop(props, key)? {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn path_prop(props: &Props, key: &str) -> Option<OwnedObjectPath> {
    match prop(props, key)? {
        Value::ObjectPath(p) => Some(p.clone().into()),
        _ => None,
    }
}

/// 解析 iwd 的对象树。有多块网卡时选路径最小的、优先处于客户端模式（有 Station）的那块。
pub fn parse(objects: &ManagedObjects) -> Snapshot {
    let mut paths: Vec<&OwnedObjectPath> = objects.keys().collect();
    paths.sort_by(|a, b| a.as_str().cmp(b.as_str()));

    let device = paths
        .iter()
        .filter(|p| objects[**p].contains_key(DEVICE))
        .min_by_key(|p| !objects[**p].contains_key(STATION))
        .map(|path| {
            let ifaces = &objects[*path];
            let props = &ifaces[DEVICE];
            let adapter = path_prop(props, "Adapter");
            let adapter_powered = adapter
                .as_ref()
                .and_then(|a| objects.get(a)?.get(ADAPTER))
                .and_then(|a| bool_prop(a, "Powered"))
                .unwrap_or(true);
            Device {
                path: (*path).clone(),
                powered: bool_prop(props, "Powered").unwrap_or(false),
                adapter,
                adapter_powered,
                station: ifaces.get(STATION).map(|s| Station {
                    state: str_prop(s, "State").unwrap_or_else(|| "disconnected".into()),
                    scanning: bool_prop(s, "Scanning").unwrap_or(false),
                    connected_network: path_prop(s, "ConnectedNetwork"),
                }),
            }
        });

    let networks = paths
        .iter()
        .filter_map(|path| {
            let props = objects[*path].get(NETWORK)?;
            let net = NetworkObj {
                path: (*path).clone(),
                name: str_prop(props, "Name")?,
                kind: str_prop(props, "Type").unwrap_or_else(|| "open".into()),
                connected: bool_prop(props, "Connected").unwrap_or(false),
                known: path_prop(props, "KnownNetwork"),
                device: path_prop(props, "Device"),
            };
            let ours = match (&device, &net.device) {
                (Some(d), Some(nd)) => d.path == *nd,
                _ => true,
            };
            ours.then_some(net)
        })
        .collect();

    let known = paths
        .iter()
        .filter_map(|path| {
            let props = objects[*path].get(KNOWN_NETWORK)?;
            Some(KnownObj {
                path: (*path).clone(),
                name: str_prop(props, "Name")?,
            })
        })
        .collect();

    Snapshot {
        device,
        networks,
        known,
    }
}

/// `GetNetworks` 返回的一项。D-Bus 签名 `(suisbb)`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct NetworkInfo {
    /// SSID。
    pub ssid: String,
    /// 信号强度 0–100。
    pub strength: u32,
    /// 信号强度（dBm）。
    pub signal_dbm: i32,
    /// 安全类型：`open` / `wep` / `psk` / `8021x`。
    pub security: String,
    /// 是否已保存（连过）。
    pub known: bool,
    /// 是否已连接。
    pub connected: bool,
}

/// iwd 的信号值（100 × dBm）换算成 0–100：-90 dBm 及以下为 0，-30 dBm 及以上为 100。
pub fn strength_percent(signal: i16) -> u32 {
    let dbm = f64::from(signal) / 100.0;
    ((dbm + 90.0) / 60.0 * 100.0).round().clamp(0.0, 100.0) as u32
}

/// 按 `Station.GetOrderedNetworks` 的顺序（信号从强到弱）生成网络列表。
pub fn network_list(snapshot: &Snapshot, ordered: &[(OwnedObjectPath, i16)]) -> Vec<NetworkInfo> {
    ordered
        .iter()
        .filter_map(|(path, signal)| {
            let net = snapshot.networks.iter().find(|n| n.path == *path)?;
            Some(NetworkInfo {
                ssid: net.name.clone(),
                strength: strength_percent(*signal),
                signal_dbm: i32::from(*signal) / 100,
                security: net.kind.clone(),
                known: net.known.is_some(),
                connected: net.connected,
            })
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use zbus::zvariant::ObjectPath;

    fn path(p: &str) -> OwnedObjectPath {
        ObjectPath::try_from(p).unwrap().into()
    }

    fn props(entries: &[(&str, Value<'static>)]) -> Props {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.try_to_owned().unwrap()))
            .collect()
    }

    fn obj(objects: &mut ManagedObjects, p: &str, iface: &str, entries: &[(&str, Value<'static>)]) {
        objects
            .entry(path(p))
            .or_default()
            .insert(iface.to_string(), props(entries));
    }

    fn op(p: &'static str) -> Value<'static> {
        Value::ObjectPath(ObjectPath::try_from(p).unwrap())
    }

    /// 一台典型的设备：一块网卡，连着 Home，周围还有 Cafe（开放）和 Office（企业）。
    pub(crate) fn sample() -> ManagedObjects {
        let mut o = ManagedObjects::new();
        obj(&mut o, "/net/connman/iwd", AGENT_MANAGER, &[]);
        obj(
            &mut o,
            "/net/connman/iwd/0",
            ADAPTER,
            &[("Powered", true.into())],
        );
        obj(
            &mut o,
            "/net/connman/iwd/0/4",
            DEVICE,
            &[
                ("Name", "wlan0".into()),
                ("Powered", true.into()),
                ("Adapter", op("/net/connman/iwd/0")),
                ("Mode", "station".into()),
            ],
        );
        obj(
            &mut o,
            "/net/connman/iwd/0/4",
            STATION,
            &[
                ("State", "connected".into()),
                ("Scanning", false.into()),
                ("ConnectedNetwork", op("/net/connman/iwd/0/4/486f6d65_psk")),
            ],
        );
        obj(
            &mut o,
            "/net/connman/iwd/0/4/486f6d65_psk",
            NETWORK,
            &[
                ("Name", "Home".into()),
                ("Type", "psk".into()),
                ("Connected", true.into()),
                ("Device", op("/net/connman/iwd/0/4")),
                ("KnownNetwork", op("/net/connman/iwd/486f6d65_psk")),
            ],
        );
        obj(
            &mut o,
            "/net/connman/iwd/0/4/43616665_open",
            NETWORK,
            &[
                ("Name", "Cafe".into()),
                ("Type", "open".into()),
                ("Connected", false.into()),
                ("Device", op("/net/connman/iwd/0/4")),
            ],
        );
        obj(
            &mut o,
            "/net/connman/iwd/0/4/4f6666696365_8021x",
            NETWORK,
            &[
                ("Name", "Office".into()),
                ("Type", "8021x".into()),
                ("Connected", false.into()),
                ("Device", op("/net/connman/iwd/0/4")),
            ],
        );
        obj(
            &mut o,
            "/net/connman/iwd/486f6d65_psk",
            KNOWN_NETWORK,
            &[("Name", "Home".into()), ("Type", "psk".into())],
        );
        o
    }

    pub(crate) fn sample_ordered() -> Vec<(OwnedObjectPath, i16)> {
        vec![
            (path("/net/connman/iwd/0/4/486f6d65_psk"), -4500),
            (path("/net/connman/iwd/0/4/43616665_open"), -7200),
            (path("/net/connman/iwd/0/4/4f6666696365_8021x"), -9500),
        ]
    }

    #[test]
    fn parses_typical_tree() {
        let snap = parse(&sample());
        let dev = snap.device.as_ref().unwrap();
        assert_eq!(dev.path.as_str(), "/net/connman/iwd/0/4");
        assert!(dev.powered && dev.adapter_powered);
        let st = dev.station.as_ref().unwrap();
        assert_eq!(st.state, "connected");
        assert_eq!(
            st.connected_network.as_ref().unwrap().as_str(),
            "/net/connman/iwd/0/4/486f6d65_psk"
        );
        assert_eq!(snap.networks.len(), 3);
        assert_eq!(snap.known.len(), 1);
        assert_eq!(snap.known[0].name, "Home");

        let list = network_list(&snap, &sample_ordered());
        assert_eq!(
            list[0],
            NetworkInfo {
                ssid: "Home".into(),
                strength: 75,
                signal_dbm: -45,
                security: "psk".into(),
                known: true,
                connected: true,
            }
        );
        assert_eq!(list[1].ssid, "Cafe");
        assert_eq!(list[1].strength, 30);
        assert!(!list[1].known);
        assert_eq!(list[2].strength, 0);
        assert_eq!(list[2].security, "8021x");
    }

    #[test]
    fn powered_off_device_has_no_station() {
        let mut o = sample();
        o.retain(|p, _| !p.as_str().starts_with("/net/connman/iwd/0/4/"));
        let dev_ifaces = o.get_mut(&path("/net/connman/iwd/0/4")).unwrap();
        dev_ifaces.remove(STATION);
        dev_ifaces.insert(DEVICE.into(), props(&[("Powered", false.into())]));
        o.get_mut(&path("/net/connman/iwd/0"))
            .unwrap()
            .insert(ADAPTER.into(), props(&[("Powered", false.into())]));
        let snap = parse(&o);
        let dev = snap.device.unwrap();
        assert!(!dev.powered);
        assert!(dev.station.is_none());
        // Device 里没有 Adapter 属性时，不知道 Adapter 状态，按开启处理。
        assert!(dev.adapter_powered);
        assert!(snap.networks.is_empty());
    }

    #[test]
    fn empty_or_unexpected_trees() {
        assert_eq!(parse(&ManagedObjects::new()), Snapshot::default());
        // 属性类型不对、缺名称的网络被忽略，不会 panic。
        let mut o = ManagedObjects::new();
        obj(
            &mut o,
            "/net/connman/iwd/0/4",
            DEVICE,
            &[("Powered", "yes".into())],
        );
        obj(
            &mut o,
            "/net/connman/iwd/0/4/x",
            NETWORK,
            &[("Type", 3u32.into())],
        );
        let snap = parse(&o);
        assert!(!snap.device.unwrap().powered);
        assert!(snap.networks.is_empty());
        assert!(network_list(&Snapshot::default(), &sample_ordered()).is_empty());
    }

    #[test]
    fn signal_strength_mapping() {
        assert_eq!(strength_percent(-3000), 100);
        assert_eq!(strength_percent(-2000), 100);
        assert_eq!(strength_percent(-6000), 50);
        assert_eq!(strength_percent(-9000), 0);
        assert_eq!(strength_percent(i16::MIN), 0);
    }

    #[test]
    fn network_info_signature() {
        assert_eq!(NetworkInfo::SIGNATURE.to_string(), "(suisbb)");
    }
}
