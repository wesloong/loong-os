//! loong-sysd 客户端：在后台线程里读 `org.loong.System1` 的电量、Wi‑Fi、亮度，把整份快照
//! 交给 calloop 主循环；控制中心的请求（调亮度、开关 Wi‑Fi）经通道送过来，主线程从不等待 D-Bus。
//!
//! ```text
//!  主线程（calloop）                         后台线程 loong-sysd-client（async-io 执行器）
//!  Shell ── SystemAction ──▶ async_channel ──▶ 合并亮度请求 → SetBrightness / SetEnabled
//!  Shell ◀── SystemStatus ── calloop channel ◀── PropertiesChanged / GetAll / NameOwnerChanged
//! ```
//!
//! - **订阅再读**（`docs/api/loong-sysd.md` 的约定）：先订阅三个对象的 `PropertiesChanged`
//!   与 `org.loong.System1` 的 `NameOwnerChanged`，再 `GetAll`，之后只靠信号更新。
//! - **服务重启**：`NameOwnerChanged` 报告所有者消失时快照变为“不在”，新的所有者出现时重新
//!   `GetAll`；总线连接断了（dbus-daemon 重启）就按 1、2、4、5 秒退避重连。
//! - **请求**：亮度请求可能一帧来好几个（主线程已按帧率节流），这里再合并一次：一次 D-Bus 调用
//!   返回前积压的亮度请求只发最后一个。服务不在时请求直接丢弃（控制中心已显示“不可用”）。
//! - **总线**：环境变量 `LOONG_SYSD_BUS`（`system` / `session` / D-Bus 地址 / `off`，与 loong-sysd
//!   相同）优先；真机后端默认系统总线，无屏幕后端默认不连（截图可复现）。

use std::{collections::HashMap, time::Duration};

use futures_lite::{Stream, StreamExt, stream};
use smithay::reexports::calloop::{LoopHandle, channel};
use zbus::{
    Connection,
    fdo::{DBusProxy, PropertiesProxy},
    names::{BusName, InterfaceName},
    proxy::CacheProperties,
    zvariant::{OwnedValue, Value},
};

use crate::{
    shell::system::{
        BacklightStatus, PowerState, PowerStatus, Service, SystemAction, SystemStatus, WifiState,
        WifiStatus,
    },
    state::LoongState,
};

/// loong-sysd 的总线名。
pub const BUS_NAME: &str = "org.loong.System1";
/// 环境变量：连接哪条总线。
pub const BUS_ENV: &str = "LOONG_SYSD_BUS";

/// 三个对象（路径、接口）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Object {
    Power,
    Backlight,
    Wifi,
}

impl Object {
    const ALL: [Object; 3] = [Object::Power, Object::Backlight, Object::Wifi];

    fn path(self) -> &'static str {
        match self {
            Object::Power => "/org/loong/System1/Power",
            Object::Backlight => "/org/loong/System1/Backlight",
            Object::Wifi => "/org/loong/System1/Wifi",
        }
    }

    fn interface(self) -> &'static str {
        match self {
            Object::Power => "org.loong.System1.Power",
            Object::Backlight => "org.loong.System1.Backlight",
            Object::Wifi => "org.loong.System1.Wifi",
        }
    }
}

/// 要连接的总线。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusTarget {
    System,
    Session,
    Address(String),
}

impl BusTarget {
    /// 与 loong-sysd 的 `--bus` 相同；另外 `off` / `none` 表示不连接。
    pub fn parse(s: &str) -> Option<BusTarget> {
        match s.trim() {
            "" | "system" => Some(BusTarget::System),
            "session" => Some(BusTarget::Session),
            "off" | "none" | "0" => None,
            address => Some(BusTarget::Address(address.to_owned())),
        }
    }

    /// 环境变量 [`BUS_ENV`] 优先，没有时用后端的默认值。
    pub fn from_env(default: Option<BusTarget>) -> Option<BusTarget> {
        match std::env::var(BUS_ENV) {
            Ok(v) => BusTarget::parse(&v),
            Err(_) => default,
        }
    }

    async fn connect(&self) -> zbus::Result<Connection> {
        match self {
            BusTarget::System => Connection::system().await,
            BusTarget::Session => Connection::session().await,
            BusTarget::Address(a) => {
                zbus::connection::Builder::address(a.as_str())?
                    .build()
                    .await
            }
        }
    }
}

/// 主线程一侧：发请求（不阻塞）。
#[derive(Clone)]
pub struct SysdClient {
    requests: async_channel::Sender<SystemAction>,
}

impl SysdClient {
    /// 发出请求（通道无界，立即返回）。
    pub fn request(&self, action: SystemAction) {
        let _ = self.requests.try_send(action);
    }
}

/// 启动后台线程，把快照接到事件循环：收到后交给 `Shell::set_system_status` 并请求重画。
pub fn start<D: 'static>(
    target: BusTarget,
    handle: &LoopHandle<'static, D>,
    get: fn(&mut D) -> &mut LoongState,
) -> anyhow::Result<SysdClient> {
    let (status_tx, status_rx) = channel::channel::<SystemStatus>();
    handle
        .insert_source(status_rx, move |event, _, data| {
            if let channel::Event::Msg(status) = event {
                let state = get(data);
                state.shell.set_system_status(status);
                state.mark_damaged();
            }
        })
        .map_err(|e| anyhow::anyhow!("注册 loong-sysd 通道失败: {e}"))?;
    Ok(spawn(target, move |status| status_tx.send(status).is_ok()))
}

/// 启动后台线程；`out` 返回 `false`（接收方已不在）时线程退出。
pub fn spawn(
    target: BusTarget,
    out: impl FnMut(SystemStatus) -> bool + Send + 'static,
) -> SysdClient {
    let (tx, rx) = async_channel::unbounded();
    let result = std::thread::Builder::new()
        .name("loong-sysd-client".into())
        .spawn(move || async_io::block_on(run(target, rx, out)));
    if let Err(e) = result {
        tracing::warn!("无法启动 loong-sysd 客户端线程: {e}");
    }
    SysdClient { requests: tx }
}

/// 把快照交给主线程（与上次相同就不发）。
struct Out<F> {
    send: F,
    last: Option<SystemStatus>,
}

impl<F: FnMut(SystemStatus) -> bool> Out<F> {
    /// 返回 `false`：主线程已不在。
    fn publish(&mut self, status: &SystemStatus) -> bool {
        if self.last.as_ref() == Some(status) {
            return true;
        }
        self.last = Some(status.clone());
        (self.send)(status.clone())
    }
}

/// 一次会话怎么结束的。
enum End {
    /// 总线连接断了：重连。
    Reconnect,
    /// 主线程不在了（通道关闭）：退出线程。
    Quit,
}

async fn run(
    target: BusTarget,
    rx: async_channel::Receiver<SystemAction>,
    send: impl FnMut(SystemStatus) -> bool,
) {
    let mut out = Out { send, last: None };
    // 一开始什么都不知道：状态栏不画电量与 Wi‑Fi。
    if !out.publish(&SystemStatus::absent()) {
        return;
    }
    let mut backoff = Duration::from_secs(1);
    let mut warned = false;
    loop {
        match target.connect().await {
            Ok(conn) => {
                tracing::info!("loong-sysd 客户端：已连接总线 {target:?}");
                warned = false;
                backoff = Duration::from_secs(1);
                match session(&conn, &rx, &mut out).await {
                    Ok(End::Quit) => return,
                    Ok(End::Reconnect) => tracing::warn!("loong-sysd 客户端：总线连接断开，重连"),
                    Err(e) => tracing::warn!("loong-sysd 客户端：{e}，重连"),
                }
            }
            Err(e) if !warned => {
                tracing::warn!("loong-sysd 客户端：连不上总线 {target:?}: {e}（稍后重试）");
                warned = true;
            }
            Err(e) => tracing::debug!("loong-sysd 客户端：连不上总线: {e}"),
        }
        if !out.publish(&SystemStatus::absent()) {
            return;
        }
        // 等待重连；期间的请求作废（界面显示“不可用”），主线程关闭就退出。
        let wait = async {
            async_io::Timer::after(backoff).await;
            true
        };
        let quit = async {
            loop {
                match rx.recv().await {
                    Ok(action) => tracing::debug!("loong-sysd 不在，丢弃请求 {action:?}"),
                    Err(_) => return false,
                }
            }
        };
        if !futures_lite::future::or(wait, quit).await {
            return;
        }
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

/// 会话中的事件。
enum Event {
    /// 某个对象的属性变了（新值）。
    Changed(Object, HashMap<String, OwnedValue>),
    /// `org.loong.System1` 有了所有者（`true`，含换了一个进程）/ 没了所有者。
    Owner(bool),
    Request(SystemAction),
    /// 主线程关闭了请求通道。
    Quit,
    /// 无关的信号（解析失败等）。
    Ignore,
}

type Events<'a> = std::pin::Pin<Box<dyn Stream<Item = Event> + 'a>>;

async fn session<F: FnMut(SystemStatus) -> bool>(
    conn: &Connection,
    rx: &async_channel::Receiver<SystemAction>,
    out: &mut Out<F>,
) -> zbus::Result<End> {
    let name = BusName::try_from(BUS_NAME)?;
    let dbus = DBusProxy::new(conn).await?;
    // 先订阅，再读（读之前发生的变化不会漏掉）。
    let owner: Events = dbus
        .receive_name_owner_changed_with_args(&[(0, BUS_NAME)])
        .await?
        .map(|signal| match signal.args() {
            Ok(args) => Event::Owner(args.new_owner().is_some()),
            Err(_) => Event::Ignore,
        })
        .boxed_local();
    let mut proxies = Vec::new();
    let mut events = owner;
    for object in Object::ALL {
        let proxy = PropertiesProxy::builder(conn)
            .destination(BUS_NAME)?
            .path(object.path())?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let changes: Events = proxy
            .receive_properties_changed()
            .await?
            .map(move |signal| match signal.args() {
                Ok(args) if args.interface_name().as_str() == object.interface() => {
                    let map = args
                        .changed_properties()
                        .iter()
                        .filter_map(|(k, v)| Some((k.to_string(), v.try_to_owned().ok()?)))
                        .collect();
                    Event::Changed(object, map)
                }
                _ => Event::Ignore,
            })
            .boxed_local();
        events = stream::or(events, changes).boxed_local();
        proxies.push((object, proxy));
    }
    let requests: Events = rx
        .clone()
        .map(Event::Request)
        .chain(stream::once(Event::Quit))
        .boxed_local();
    let mut events = stream::or(events, requests);

    let mut status = SystemStatus::absent();
    if dbus.name_has_owner(name.clone()).await? {
        refresh(&proxies, &mut status).await;
    }
    if !out.publish(&status) {
        return Ok(End::Quit);
    }
    while let Some(event) = events.next().await {
        match event {
            Event::Owner(true) => {
                tracing::info!("loong-sysd 客户端：{BUS_NAME} 已运行，读取状态");
                refresh(&proxies, &mut status).await;
            }
            Event::Owner(false) => {
                tracing::warn!("loong-sysd 客户端：{BUS_NAME} 已退出");
                status = SystemStatus::absent();
            }
            Event::Changed(object, map) if status.service == Service::Running => {
                apply(&mut status, object, &map);
            }
            Event::Changed(..) | Event::Ignore => continue,
            Event::Request(first) => {
                // 一次调用返回前积压的请求：亮度只发最后一个。
                let mut batch = vec![first];
                while let Ok(action) = rx.try_recv() {
                    batch.push(action);
                }
                for action in coalesce(batch) {
                    if status.service == Service::Running {
                        call(conn, action).await;
                    } else {
                        tracing::debug!("loong-sysd 不在，丢弃请求 {action:?}");
                    }
                }
                continue;
            }
            Event::Quit => return Ok(End::Quit),
        }
        if !out.publish(&status) {
            return Ok(End::Quit);
        }
    }
    // 信号流结束：连接断了。
    Ok(End::Reconnect)
}

/// 合并请求：亮度只保留最后一个（放在原来最后一个亮度请求的位置），其余按顺序保留。
fn coalesce(batch: Vec<SystemAction>) -> Vec<SystemAction> {
    let last_brightness = batch
        .iter()
        .rposition(|a| matches!(a, SystemAction::Brightness(_)));
    batch
        .into_iter()
        .enumerate()
        .filter(|(i, a)| !matches!(a, SystemAction::Brightness(_)) || Some(*i) == last_brightness)
        .map(|(_, a)| a)
        .collect()
}

async fn call(conn: &Connection, action: SystemAction) {
    let result = match action {
        SystemAction::Brightness(value) => {
            let object = Object::Backlight;
            conn.call_method(
                Some(BUS_NAME),
                object.path(),
                Some(object.interface()),
                "SetBrightness",
                &(value.clamp(0.0, 1.0), true),
            )
            .await
        }
        SystemAction::WifiEnabled(enabled) => {
            let object = Object::Wifi;
            conn.call_method(
                Some(BUS_NAME),
                object.path(),
                Some(object.interface()),
                "SetEnabled",
                &(enabled,),
            )
            .await
        }
        // 外观由主线程写文件，不经过 loong-sysd。
        SystemAction::Theme(_) => return,
    };
    if let Err(e) = result {
        tracing::warn!("loong-sysd 请求 {action:?} 失败: {e}");
    }
}

/// 重新读取三个对象的全部属性。服务不在（读失败）时快照为“不在”。
async fn refresh(proxies: &[(Object, PropertiesProxy<'static>)], status: &mut SystemStatus) {
    let mut fresh = SystemStatus {
        service: Service::Running,
        ..SystemStatus::default()
    };
    for (object, proxy) in proxies {
        let Ok(interface) = InterfaceName::try_from(object.interface()) else {
            continue;
        };
        match proxy.get_all(interface).await {
            Ok(map) => apply(&mut fresh, *object, &map),
            Err(e) => {
                tracing::warn!("读取 {} 失败: {e}", object.interface());
                *status = SystemStatus::absent();
                return;
            }
        }
    }
    *status = fresh;
}

fn as_f64(v: &Value<'_>) -> Option<f64> {
    match v {
        Value::F64(x) => Some(*x),
        _ => None,
    }
}

fn as_bool(v: &Value<'_>) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn as_str<'a>(v: &'a Value<'_>) -> Option<&'a str> {
    match v {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

fn as_i64(v: &Value<'_>) -> Option<i64> {
    match v {
        Value::I64(x) => Some(*x),
        _ => None,
    }
}

fn as_u32(v: &Value<'_>) -> Option<u32> {
    match v {
        Value::U32(x) => Some(*x),
        _ => None,
    }
}

/// 把一组属性（名称 → 值）写进快照。类型不对的值忽略。
fn apply(status: &mut SystemStatus, object: Object, props: &HashMap<String, OwnedValue>) {
    let props = props.iter().map(|(k, v)| (k.as_str(), &**v));
    match object {
        Object::Power => {
            let p = status.power.get_or_insert_with(PowerStatus::default);
            for (name, v) in props {
                match name {
                    "HasBattery" => p.has_battery = as_bool(v).unwrap_or(p.has_battery),
                    "Percentage" => p.percentage = as_f64(v).unwrap_or(p.percentage),
                    "State" => p.state = as_str(v).map_or(p.state, PowerState::parse),
                    "TimeToEmpty" => p.time_to_empty = as_i64(v).unwrap_or(p.time_to_empty),
                    "TimeToFull" => p.time_to_full = as_i64(v).unwrap_or(p.time_to_full),
                    "AcOnline" => p.ac_online = as_bool(v).unwrap_or(p.ac_online),
                    _ => {}
                }
            }
        }
        Object::Backlight => {
            let b = status
                .backlight
                .get_or_insert_with(BacklightStatus::default);
            for (name, v) in props {
                match name {
                    "Available" => b.available = as_bool(v).unwrap_or(b.available),
                    "Brightness" => b.brightness = as_f64(v).unwrap_or(b.brightness),
                    _ => {}
                }
            }
        }
        Object::Wifi => {
            let w = status.wifi.get_or_insert_with(WifiStatus::default);
            for (name, v) in props {
                match name {
                    "Available" => w.available = as_bool(v).unwrap_or(w.available),
                    "Enabled" => w.enabled = as_bool(v).unwrap_or(w.enabled),
                    "State" => w.state = as_str(v).map_or(w.state, WifiState::parse),
                    "Ssid" => {
                        if let Some(s) = as_str(v) {
                            w.ssid = s.to_owned();
                        }
                    }
                    "Strength" => w.strength = as_u32(v).unwrap_or(w.strength),
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
