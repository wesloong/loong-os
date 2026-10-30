//! 集成测试：启动一个私有的 dbus-daemon，在上面运行真正的 loong-sysd 二进制
//! （sysfs 指向临时目录里的假目录树），再通过 D-Bus 调用它的接口。
//!
//! 需要 `dbus-daemon` 和 `dbus-send`（Alpine：`apk add dbus`，开发 chroot 已安装）；
//! 找不到时测试打印提示后直接通过。以 root 运行时（开发 chroot、CI 容器）还会用
//! nobody（uid 65534）检查权限控制，并加载 `system/rootfs` 里真正的总线策略文件。
//!
//! ```sh
//! tools/dev-chroot.sh run sh -c 'cargo test -p loong-sysd --test dbus -- --nocapture'
//! ```

use std::{
    collections::HashMap,
    future::Future,
    io::{BufRead, BufReader},
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use async_io::Timer;
use futures_lite::{StreamExt, future};
use zbus::{
    Connection, MatchRule, MessageStream,
    message::Type as MessageType,
    zvariant::{OwnedValue, Value},
};

const BUS_NAME: &str = "org.loong.System1";
const BACKLIGHT: (&str, &str) = (
    "/org/loong/System1/Backlight",
    "org.loong.System1.Backlight",
);
const POWER: (&str, &str) = ("/org/loong/System1/Power", "org.loong.System1.Power");
const WIFI: (&str, &str) = ("/org/loong/System1/Wifi", "org.loong.System1.Wifi");
const NOBODY: u32 = 65534;

/// `PATH` 里是否有这个程序。
fn have(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(cmd).is_file()))
}

fn is_root() -> bool {
    std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

fn skip_without_dbus() -> bool {
    if have("dbus-daemon") && have("dbus-send") {
        return false;
    }
    eprintln!("跳过：没有 dbus-daemon / dbus-send（apk add dbus）");
    true
}

/// 测试结束时删除的临时目录（nobody 也能进入，用来放总线套接字）。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "loong-sysd-it-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.0.join(rel)).unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 总线策略。
enum Policy {
    /// 所有人都能做任何事：只靠 loong-sysd 自己的 uid 检查。
    Open,
    /// 与系统总线相同的默认策略 + 仓库里的 `org.loong.System1.conf`。
    System,
}

fn bus_config(dir: &Path, policy: &Policy) -> String {
    let body = match policy {
        Policy::Open => r#"
  <policy context="default">
    <allow user="*"/>
    <allow own="*"/>
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
  </policy>"#
            .to_string(),
        Policy::System => {
            let conf = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../system/rootfs/usr/share/dbus-1/system.d/org.loong.System1.conf");
            assert!(conf.exists(), "缺少策略文件 {}", conf.display());
            format!(
                r#"
  <policy context="default">
    <allow user="*"/>
    <deny own="*"/>
    <deny send_type="method_call"/>
    <allow send_type="signal"/>
    <allow send_requested_reply="true" send_type="method_return"/>
    <allow send_requested_reply="true" send_type="error"/>
    <allow receive_type="method_call"/>
    <allow receive_type="method_return"/>
    <allow receive_type="error"/>
    <allow receive_type="signal"/>
    <allow send_destination="org.freedesktop.DBus" send_interface="org.freedesktop.DBus"/>
    <allow send_destination="org.freedesktop.DBus" send_interface="org.freedesktop.DBus.Introspectable"/>
    <allow send_destination="org.freedesktop.DBus" send_interface="org.freedesktop.DBus.Properties"/>
  </policy>
  <include>{}</include>"#,
                conf.canonicalize().unwrap().display()
            )
        }
    };
    format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <listen>unix:path={}/bus</listen>
  <auth>EXTERNAL</auth>{body}
</busconfig>
"#,
        dir.display()
    )
}

/// 私有总线 + loong-sysd + 一个 root 客户端连接。
struct Fixture {
    sysfs: TempDir,
    run: TempDir,
    address: String,
    bus: Child,
    sysd: Child,
    conn: Connection,
}

impl Fixture {
    /// 假 sysfs：一个 raw 背光（0–1000，当前最亮）、一块放电中的电池（42%，7 W）、断开的充电器。
    fn fake_sysfs() -> TempDir {
        let t = TempDir::new("sysfs");
        t.write("class/backlight/panel/type", "raw\n");
        t.write("class/backlight/panel/max_brightness", "1000\n");
        t.write("class/backlight/panel/brightness", "1000\n");
        for (file, value) in [
            ("type", "Battery"),
            ("present", "1"),
            ("status", "Discharging"),
            ("capacity", "42"),
            ("energy_now", "21000000"),
            ("energy_full", "50000000"),
            ("power_now", "7000000"),
        ] {
            t.write(
                &format!("class/power_supply/BAT0/{file}"),
                &format!("{value}\n"),
            );
        }
        t.write("class/power_supply/AC/type", "Mains\n");
        t.write("class/power_supply/AC/online", "0\n");
        t
    }

    async fn start(policy: Policy) -> Fixture {
        let sysfs = Self::fake_sysfs();
        let run = TempDir::new("run");
        let config = run.0.join("bus.conf");
        std::fs::write(&config, bus_config(&run.0, &policy)).unwrap();

        let mut bus = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile", "--nosyslog", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(run.0.join("bus.log")).unwrap())
            .spawn()
            .expect("启动 dbus-daemon");
        let mut address = String::new();
        BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_string();
        assert!(
            !address.is_empty(),
            "dbus-daemon 没有输出地址（配置有误？）:\n{}",
            std::fs::read_to_string(run.0.join("bus.log")).unwrap_or_default()
        );

        let log = std::fs::File::create(run.0.join("sysd.log")).unwrap();
        let sysd = Command::new(env!("CARGO_BIN_EXE_loong-sysd"))
            .env("LOONG_SYSD_BUS", &address)
            .env("LOONG_SYSFS_ROOT", &sysfs.0)
            .env("RUST_LOG", "debug")
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("启动 loong-sysd");

        let conn = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let mut fixture = Fixture {
            sysfs,
            run,
            address,
            bus,
            sysd,
            conn,
        };
        fixture.wait_for_service().await;
        fixture
    }

    fn log(&self) -> String {
        let read = |name: &str| std::fs::read_to_string(self.run.0.join(name)).unwrap_or_default();
        format!(
            "---- loong-sysd ----\n{}---- dbus-daemon ----\n{}",
            read("sysd.log"),
            read("bus.log")
        )
    }

    async fn wait_for_service(&mut self) {
        let start = Instant::now();
        loop {
            let reply = self
                .conn
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "NameHasOwner",
                    &(BUS_NAME,),
                )
                .await
                .unwrap();
            if reply.body().deserialize::<bool>().unwrap() {
                return;
            }
            if let Ok(Some(status)) = self.sysd.try_wait() {
                panic!("loong-sysd 退出了（{status}）:\n{}", self.log());
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "loong-sysd 没有在 10 秒内占用 {BUS_NAME}:\n{}",
                self.log()
            );
            Timer::after(Duration::from_millis(50)).await;
        }
    }

    async fn get(&self, (path, iface): (&str, &str), name: &str) -> OwnedValue {
        let reply = self
            .conn
            .call_method(
                Some(BUS_NAME),
                path,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(iface, name),
            )
            .await
            .unwrap_or_else(|e| panic!("读取 {iface}.{name} 失败: {e}"));
        reply.body().deserialize::<OwnedValue>().unwrap()
    }

    async fn call<B>(
        &self,
        (path, iface): (&str, &str),
        method: &str,
        body: &B,
    ) -> zbus::Result<zbus::Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.conn
            .call_method(Some(BUS_NAME), path, Some(iface), method, body)
            .await
    }

    /// 以 nobody 身份用 dbus-send 调用，返回 (是否成功, 输出)。
    fn send_as_nobody(&self, path: &str, member: &str, args: &[&str]) -> (bool, String) {
        let out = Command::new("dbus-send")
            .arg(format!("--bus={}", self.address))
            .args(["--print-reply", &format!("--dest={BUS_NAME}"), path, member])
            .args(args)
            .uid(NOBODY)
            .gid(NOBODY)
            .output()
            .expect("运行 dbus-send");
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        (out.status.success(), text)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.sysd.kill();
        let _ = self.sysd.wait();
        let _ = self.bus.kill();
        let _ = self.bus.wait();
        if std::thread::panicking() {
            eprintln!("{}", self.log());
        }
    }
}

fn method_error_name(r: zbus::Result<zbus::Message>) -> String {
    match r {
        Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
        Err(e) => panic!("期望 D-Bus 方法错误，得到 {e}"),
        Ok(_) => panic!("期望失败，调用却成功了"),
    }
}

async fn timeout<T>(d: Duration, f: impl Future<Output = T>) -> Option<T> {
    future::or(async { Some(f.await) }, async {
        Timer::after(d).await;
        None
    })
    .await
}

#[test]
fn interfaces_on_private_bus() {
    if skip_without_dbus() {
        return;
    }
    async_io::block_on(async {
        let fx = Fixture::start(Policy::Open).await;

        // ── 背光 ──
        assert!(bool::try_from(fx.get(BACKLIGHT, "Available").await).unwrap());
        assert_eq!(
            f64::try_from(fx.get(BACKLIGHT, "Brightness").await).unwrap(),
            1.0
        );
        assert_eq!(
            u32::try_from(fx.get(BACKLIGHT, "MaxRaw").await).unwrap(),
            1000
        );
        assert_eq!(
            String::try_from(fx.get(BACKLIGHT, "Device").await).unwrap(),
            "panel"
        );

        fx.call(BACKLIGHT, "SetBrightness", &(0.5f64, false))
            .await
            .unwrap();
        // 感知亮度 0.5 → raw ≈ 5 + 995 × 0.5^2.2 = 222
        assert_eq!(fx.sysfs.read("class/backlight/panel/brightness"), "222");
        assert_eq!(
            f64::try_from(fx.get(BACKLIGHT, "Brightness").await).unwrap(),
            0.5
        );

        fx.call(BACKLIGHT, "SetBrightness", &(1.0f64, true))
            .await
            .unwrap();
        let start = Instant::now();
        while fx.sysfs.read("class/backlight/panel/brightness") != "1000" {
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "过渡动画没有到达目标"
            );
            Timer::after(Duration::from_millis(20)).await;
        }
        assert_eq!(
            method_error_name(
                fx.call(BACKLIGHT, "SetBrightness", &(f64::NAN, false))
                    .await
            ),
            "org.loong.System1.Error.InvalidArgs"
        );

        // ── 电源 ──
        assert!(bool::try_from(fx.get(POWER, "HasBattery").await).unwrap());
        assert_eq!(
            f64::try_from(fx.get(POWER, "Percentage").await).unwrap(),
            42.0
        );
        assert_eq!(
            String::try_from(fx.get(POWER, "State").await).unwrap(),
            "discharging"
        );
        assert_eq!(
            i64::try_from(fx.get(POWER, "TimeToEmpty").await).unwrap(),
            3 * 3600
        );
        assert!(!bool::try_from(fx.get(POWER, "AcOnline").await).unwrap());
        assert_eq!(
            String::try_from(fx.get(POWER, "WarningLevel").await).unwrap(),
            "none"
        );

        // 电量降到 10%：Refresh 后应收到 PropertiesChanged 和 LowBattery。
        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(BUS_NAME)
            .unwrap()
            .path(POWER.0)
            .unwrap()
            .build();
        let mut signals = MessageStream::for_match_rule(rule, &fx.conn, None)
            .await
            .unwrap();
        fx.sysfs.write("class/power_supply/BAT0/capacity", "10\n");
        fx.call(POWER, "Refresh", &()).await.unwrap();
        let (mut saw_changed, mut saw_low) = (false, false);
        while !(saw_changed && saw_low) {
            let msg = timeout(Duration::from_secs(5), signals.next())
                .await
                .expect("5 秒内没有收到电源信号")
                .unwrap()
                .unwrap();
            let hdr = msg.header();
            match hdr.member().map(|m| m.as_str()) {
                Some("PropertiesChanged") => {
                    let (iface, changed, _): (String, HashMap<String, OwnedValue>, Vec<String>) =
                        msg.body().deserialize().unwrap();
                    assert_eq!(iface, POWER.1);
                    assert_eq!(f64::try_from(&changed["Percentage"]).unwrap(), 10.0);
                    assert_eq!(&*changed["WarningLevel"], &Value::from("low"));
                    saw_changed = true;
                }
                Some("LowBattery") => {
                    let (level, pct): (String, f64) = msg.body().deserialize().unwrap();
                    assert_eq!((level.as_str(), pct), ("low", 10.0));
                    saw_low = true;
                }
                other => panic!("意外的信号 {other:?}"),
            }
        }

        // ── Wi‑Fi：私有总线上没有 iwd ──
        assert!(!bool::try_from(fx.get(WIFI, "Available").await).unwrap());
        assert_eq!(
            String::try_from(fx.get(WIFI, "State").await).unwrap(),
            "unavailable"
        );
        let nets = fx.call(WIFI, "GetNetworks", &()).await.unwrap();
        let nets: Vec<(String, u32, i32, String, bool, bool)> = nets.body().deserialize().unwrap();
        assert!(nets.is_empty());
        assert_eq!(
            method_error_name(fx.call(WIFI, "Scan", &()).await),
            "org.loong.System1.Error.Unavailable"
        );
        assert_eq!(
            method_error_name(fx.call(WIFI, "Connect", &("Home", "secret123")).await),
            "org.loong.System1.Error.Unavailable"
        );

        // ── 权限：nobody 能读，不能写（这里总线放行一切，由 loong-sysd 自己拒绝） ──
        if is_root() {
            let (ok, out) = fx.send_as_nobody(
                BACKLIGHT.0,
                "org.freedesktop.DBus.Properties.Get",
                &["string:org.loong.System1.Backlight", "string:Brightness"],
            );
            assert!(ok, "nobody 应能读取属性: {out}");
            let (ok, out) = fx.send_as_nobody(
                BACKLIGHT.0,
                "org.loong.System1.Backlight.SetBrightness",
                &["double:0.1", "boolean:false"],
            );
            assert!(
                !ok && out.contains("org.loong.System1.Error.AccessDenied"),
                "{out}"
            );
            assert_eq!(fx.sysfs.read("class/backlight/panel/brightness"), "1000");
            let (ok, out) = fx.send_as_nobody(WIFI.0, "org.loong.System1.Wifi.Scan", &[]);
            assert!(
                !ok && out.contains("org.loong.System1.Error.AccessDenied"),
                "{out}"
            );
            // Agent 只接受 iwd 的调用
            let (ok, out) = fx.send_as_nobody(
                "/org/loong/System1/Wifi/Agent",
                "net.connman.iwd.Agent.RequestPassphrase",
                &["objpath:/net/connman/iwd/0/4/x_psk"],
            );
            assert!(
                !ok && out.contains("net.connman.iwd.Agent.Error.Canceled"),
                "{out}"
            );
        } else {
            eprintln!("不是 root：跳过 nobody 权限检查");
        }
    });
}

#[test]
fn bus_policy_file_restricts_setters() {
    if skip_without_dbus() {
        return;
    }
    if !is_root() {
        eprintln!("跳过：需要 root（策略只允许 root 占用服务名，且要切换到 nobody）");
        return;
    }
    async_io::block_on(async {
        let fx = Fixture::start(Policy::System).await;

        // root 可以写
        fx.call(BACKLIGHT, "SetBrightness", &(0.0f64, false))
            .await
            .unwrap();
        assert_eq!(fx.sysfs.read("class/backlight/panel/brightness"), "5");

        // nobody：读属性、只读方法可以
        let (ok, out) = fx.send_as_nobody(
            POWER.0,
            "org.freedesktop.DBus.Properties.Get",
            &["string:org.loong.System1.Power", "string:Percentage"],
        );
        assert!(ok, "{out}");
        let (ok, out) = fx.send_as_nobody(POWER.0, "org.loong.System1.Power.Refresh", &[]);
        assert!(ok, "{out}");
        let (ok, out) = fx.send_as_nobody(WIFI.0, "org.loong.System1.Wifi.GetNetworks", &[]);
        assert!(ok, "{out}");
        let (ok, out) = fx.send_as_nobody(
            BACKLIGHT.0,
            "org.freedesktop.DBus.Introspectable.Introspect",
            &[],
        );
        assert!(ok && out.contains("SetBrightness"), "{out}");

        // nobody：写操作和 Agent 在总线层面就被拒绝
        for (path, member, args) in [
            (
                BACKLIGHT.0,
                "org.loong.System1.Backlight.SetBrightness",
                &["double:0.1", "boolean:false"][..],
            ),
            (
                WIFI.0,
                "org.loong.System1.Wifi.SetEnabled",
                &["boolean:false"][..],
            ),
            (WIFI.0, "org.loong.System1.Wifi.Scan", &[][..]),
            (
                WIFI.0,
                "org.loong.System1.Wifi.Connect",
                &["string:x", "string:y"][..],
            ),
            (WIFI.0, "org.loong.System1.Wifi.Disconnect", &[][..]),
            (WIFI.0, "org.loong.System1.Wifi.Forget", &["string:x"][..]),
            (
                "/org/loong/System1/Wifi/Agent",
                "net.connman.iwd.Agent.RequestPassphrase",
                &["objpath:/x"][..],
            ),
        ] {
            let (ok, out) = fx.send_as_nobody(path, member, args);
            assert!(
                !ok && out.contains("org.freedesktop.DBus.Error.AccessDenied"),
                "{member} 应被总线拒绝: {out}"
            );
        }
        assert_eq!(fx.sysfs.read("class/backlight/panel/brightness"), "5");
    });
}

/// 极简的假 iwd：一块网卡、两个网络（Home 需要密码 `correct horse`，Cafe 开放）。
/// Network.Connect 和真 iwd 一样回调已注册 Agent 的 RequestPassphrase。
mod mock_iwd {
    use std::sync::{Arc, Mutex};

    use zbus::{
        Connection, DBusError, interface,
        message::Header,
        zvariant::{ObjectPath, OwnedObjectPath},
    };

    pub const DEVICE: &str = "/net/connman/iwd/0/4";
    pub const HOME: &str = "/net/connman/iwd/0/4/486f6d65_psk";
    pub const CAFE: &str = "/net/connman/iwd/0/4/43616665_open";
    pub const PASSWORD: &str = "correct horse";

    fn path(p: &str) -> OwnedObjectPath {
        ObjectPath::try_from(p).unwrap().into()
    }

    #[derive(Debug, DBusError)]
    #[zbus(prefix = "net.connman.iwd")]
    pub enum IwdError {
        #[zbus(error)]
        ZBus(zbus::Error),
        Failed(String),
    }

    /// 测试可以检查的状态。
    #[derive(Default)]
    pub struct Shared {
        /// 已注册的 Agent：(唯一名, 路径)。
        pub agent: Option<(String, OwnedObjectPath)>,
        /// Agent 交回的密码。
        pub passphrases: Vec<String>,
    }

    pub type State = Arc<Mutex<Shared>>;

    struct AgentManager(State);

    #[interface(name = "net.connman.iwd.AgentManager")]
    impl AgentManager {
        fn register_agent(&self, path: OwnedObjectPath, #[zbus(header)] hdr: Header<'_>) {
            let sender = hdr.sender().unwrap().to_string();
            self.0.lock().unwrap().agent = Some((sender, path));
        }

        fn unregister_agent(&self, _path: OwnedObjectPath) {
            self.0.lock().unwrap().agent = None;
        }
    }

    struct Adapter;

    #[interface(name = "net.connman.iwd.Adapter")]
    impl Adapter {
        #[zbus(property)]
        fn powered(&self) -> bool {
            true
        }
    }

    struct Device {
        powered: bool,
    }

    #[interface(name = "net.connman.iwd.Device")]
    impl Device {
        #[zbus(property)]
        fn name(&self) -> String {
            "wlan0".into()
        }

        #[zbus(property)]
        fn powered(&self) -> bool {
            self.powered
        }

        #[zbus(property)]
        fn set_powered(&mut self, powered: bool) {
            self.powered = powered;
        }

        #[zbus(property)]
        fn adapter(&self) -> OwnedObjectPath {
            path("/net/connman/iwd/0")
        }
    }

    struct Station;

    #[interface(name = "net.connman.iwd.Station")]
    impl Station {
        #[zbus(property)]
        fn state(&self) -> String {
            "disconnected".into()
        }

        #[zbus(property)]
        fn scanning(&self) -> bool {
            false
        }

        fn scan(&self) {}

        fn get_ordered_networks(&self) -> Vec<(OwnedObjectPath, i16)> {
            vec![(path(HOME), -4500), (path(CAFE), -7000)]
        }
    }

    struct Network {
        name: &'static str,
        kind: &'static str,
        own_path: &'static str,
        state: State,
    }

    #[interface(name = "net.connman.iwd.Network")]
    impl Network {
        #[zbus(property)]
        fn name(&self) -> String {
            self.name.into()
        }

        #[zbus(property, name = "Type")]
        fn kind(&self) -> String {
            self.kind.into()
        }

        #[zbus(property)]
        fn connected(&self) -> bool {
            false
        }

        #[zbus(property)]
        fn device(&self) -> OwnedObjectPath {
            path(DEVICE)
        }

        async fn connect(&self, #[zbus(connection)] conn: &Connection) -> Result<(), IwdError> {
            if self.kind == "open" {
                return Ok(());
            }
            let (sender, agent) = self
                .state
                .lock()
                .unwrap()
                .agent
                .clone()
                .ok_or_else(|| IwdError::Failed("没有 Agent".into()))?;
            let reply = conn
                .call_method(
                    Some(sender.as_str()),
                    agent.as_str(),
                    Some("net.connman.iwd.Agent"),
                    "RequestPassphrase",
                    &(path(self.own_path),),
                )
                .await
                .map_err(|e| IwdError::Failed(format!("Agent 拒绝: {e}")))?;
            let passphrase: String = reply.body().deserialize()?;
            self.state
                .lock()
                .unwrap()
                .passphrases
                .push(passphrase.clone());
            if passphrase == PASSWORD {
                Ok(())
            } else {
                Err(IwdError::Failed("Operation failed".into()))
            }
        }
    }

    /// 在 `address` 上启动假 iwd。
    pub async fn start(address: &str) -> (Connection, State) {
        let state = State::default();
        let conn = zbus::connection::Builder::address(address)
            .unwrap()
            .serve_at("/", zbus::fdo::ObjectManager)
            .unwrap()
            .serve_at("/net/connman/iwd", AgentManager(state.clone()))
            .unwrap()
            .serve_at("/net/connman/iwd/0", Adapter)
            .unwrap()
            .serve_at(DEVICE, Device { powered: true })
            .unwrap()
            .serve_at(DEVICE, Station)
            .unwrap()
            .serve_at(
                HOME,
                Network {
                    name: "Home",
                    kind: "psk",
                    own_path: HOME,
                    state: state.clone(),
                },
            )
            .unwrap()
            .serve_at(
                CAFE,
                Network {
                    name: "Cafe",
                    kind: "open",
                    own_path: CAFE,
                    state: state.clone(),
                },
            )
            .unwrap()
            .name("net.connman.iwd")
            .unwrap()
            .build()
            .await
            .unwrap();
        (conn, state)
    }
}

/// 轮询直到条件成立（最多 5 秒）。
async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = Instant::now();
    while !check().await {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "5 秒内没有等到：{what}"
        );
        Timer::after(Duration::from_millis(50)).await;
    }
}

#[test]
fn wifi_connect_through_mock_iwd() {
    if skip_without_dbus() {
        return;
    }
    async_io::block_on(async {
        let fx = Fixture::start(Policy::Open).await;
        assert!(!bool::try_from(fx.get(WIFI, "Available").await).unwrap());

        // iwd 启动后，loong-sysd 自动注册 Agent 并读到网卡和网络。
        let (_iwd, iwd_state) = mock_iwd::start(&fx.address).await;
        eventually("Wi‑Fi 可用", || async {
            bool::try_from(fx.get(WIFI, "Available").await).unwrap()
        })
        .await;
        eventually("Agent 已注册", || async {
            iwd_state.lock().unwrap().agent.is_some()
        })
        .await;
        assert!(bool::try_from(fx.get(WIFI, "Enabled").await).unwrap());
        assert_eq!(
            String::try_from(fx.get(WIFI, "State").await).unwrap(),
            "disconnected"
        );

        let nets = fx.call(WIFI, "GetNetworks", &()).await.unwrap();
        let nets: Vec<(String, u32, i32, String, bool, bool)> = nets.body().deserialize().unwrap();
        assert_eq!(
            nets,
            [
                ("Home".into(), 75, -45, "psk".into(), false, false),
                ("Cafe".into(), 33, -70, "open".into(), false, false),
            ]
        );

        // 密码经 Agent 交给 iwd；错误密码返回 Failed，正确密码成功。
        let err = method_error_name(fx.call(WIFI, "Connect", &("Home", "wrong pass")).await);
        assert_eq!(err, "org.loong.System1.Error.Failed");
        fx.call(WIFI, "Connect", &("Home", mock_iwd::PASSWORD))
            .await
            .unwrap();
        assert_eq!(
            iwd_state.lock().unwrap().passphrases,
            ["wrong pass", mock_iwd::PASSWORD]
        );
        // 密码用完即丢：iwd 再来要也拿不到。
        fx.call(WIFI, "Connect", &("Cafe", "")).await.unwrap();
        assert_eq!(
            method_error_name(fx.call(WIFI, "Connect", &("Home", "")).await),
            "org.loong.System1.Error.InvalidArgs"
        );
        assert_eq!(
            method_error_name(fx.call(WIFI, "Connect", &("Nowhere", "x")).await),
            "org.loong.System1.Error.NotFound"
        );
        assert_eq!(
            method_error_name(fx.call(WIFI, "Forget", &("Home",)).await),
            "org.loong.System1.Error.NotFound"
        );

        // 不是 iwd 的连接（即使是 root）调用 Agent 会被拒绝。
        let agent = ("/org/loong/System1/Wifi/Agent", "net.connman.iwd.Agent");
        let path = zbus::zvariant::ObjectPath::try_from(mock_iwd::HOME).unwrap();
        assert_eq!(
            method_error_name(fx.call(agent, "RequestPassphrase", &(path,)).await),
            "net.connman.iwd.Agent.Error.Canceled"
        );

        fx.call(WIFI, "Scan", &()).await.unwrap();
        fx.call(WIFI, "SetEnabled", &(false,)).await.unwrap();
        eventually("Wi‑Fi 已关闭", || async {
            String::try_from(fx.get(WIFI, "State").await).unwrap() == "off"
        })
        .await;
        assert!(!bool::try_from(fx.get(WIFI, "Enabled").await).unwrap());
    });
}
