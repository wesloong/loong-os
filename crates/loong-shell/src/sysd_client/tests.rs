//! loong-sysd 客户端的测试：属性映射、请求合并，以及在私有 dbus-daemon 上对一个假的
//! `org.loong.System1` 的端到端测试（订阅、读取、变化、请求、服务退出与重启）。
//! 没有 `dbus-daemon` 时跳过端到端部分。

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use zbus::{object_server::SignalEmitter, zvariant::Str};

use super::*;

fn props(pairs: &[(&str, OwnedValue)]) -> HashMap<String, OwnedValue> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.try_clone().unwrap()))
        .collect()
}

#[test]
fn properties_are_applied_by_name_and_type() {
    let mut s = SystemStatus {
        service: Service::Running,
        ..SystemStatus::default()
    };
    apply(
        &mut s,
        Object::Power,
        &props(&[
            ("HasBattery", true.into()),
            ("Percentage", 57.0.into()),
            ("State", Str::from("discharging").into()),
            ("TimeToEmpty", 17100i64.into()),
            ("AcOnline", false.into()),
            ("WarningLevel", Str::from("none").into()),
        ]),
    );
    let p = s.power.clone().unwrap();
    assert!(p.has_battery);
    assert_eq!(p.percentage, 57.0);
    assert_eq!(p.state, PowerState::Discharging);
    assert_eq!(p.time_to_empty, 17100);
    // 只更新信号里带的属性；类型不对的值忽略。
    apply(
        &mut s,
        Object::Power,
        &props(&[
            ("Percentage", 80.0.into()),
            ("State", Str::from("charging").into()),
            ("AcOnline", 1u32.into()),
        ]),
    );
    let p = s.power.clone().unwrap();
    assert_eq!((p.percentage, p.state), (80.0, PowerState::Charging));
    assert_eq!(p.time_to_empty, 17100);
    assert!(!p.ac_online, "类型不对（u32 而不是 b）时保持原值");

    apply(
        &mut s,
        Object::Wifi,
        &props(&[
            ("Available", true.into()),
            ("Enabled", true.into()),
            ("State", Str::from("connected").into()),
            ("Ssid", Str::from("听雨轩").into()),
            ("Strength", 72u32.into()),
        ]),
    );
    let w = s.wifi.clone().unwrap();
    assert_eq!(
        (w.state, w.ssid.as_str(), w.strength),
        (WifiState::Connected, "听雨轩", 72)
    );
    apply(
        &mut s,
        Object::Backlight,
        &props(&[("Available", true.into()), ("Brightness", 0.3.into())]),
    );
    assert_eq!(s.brightness(), Some(0.3));
}

#[test]
fn only_the_last_brightness_request_is_sent() {
    use SystemAction::*;
    assert_eq!(
        coalesce(vec![
            Brightness(0.1),
            WifiEnabled(false),
            Brightness(0.2),
            Brightness(0.3),
            WifiEnabled(true),
        ]),
        vec![WifiEnabled(false), Brightness(0.3), WifiEnabled(true)]
    );
    assert_eq!(coalesce(vec![Brightness(0.5)]), vec![Brightness(0.5)]);
}

#[test]
fn bus_target_from_string() {
    assert_eq!(BusTarget::parse(""), Some(BusTarget::System));
    assert_eq!(BusTarget::parse("system"), Some(BusTarget::System));
    assert_eq!(BusTarget::parse("session"), Some(BusTarget::Session));
    assert_eq!(BusTarget::parse("off"), None);
    assert_eq!(
        BusTarget::parse("unix:path=/tmp/bus"),
        Some(BusTarget::Address("unix:path=/tmp/bus".into()))
    );
}

// ───────────────────────── 私有总线上的假 loong-sysd ─────────────────────────

struct FakePower {
    percentage: f64,
    state: String,
}

#[zbus::interface(name = "org.loong.System1.Power")]
impl FakePower {
    #[zbus(property)]
    fn has_battery(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn percentage(&self) -> f64 {
        self.percentage
    }
    #[zbus(property)]
    fn state(&self) -> String {
        self.state.clone()
    }
    #[zbus(property)]
    fn time_to_empty(&self) -> i64 {
        0
    }
    #[zbus(property)]
    fn time_to_full(&self) -> i64 {
        0
    }
    #[zbus(property)]
    fn ac_online(&self) -> bool {
        self.state == "charging"
    }
}

struct FakeBacklight {
    brightness: f64,
    calls: Arc<Mutex<Vec<f64>>>,
}

#[zbus::interface(name = "org.loong.System1.Backlight")]
impl FakeBacklight {
    #[zbus(property)]
    fn available(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn brightness(&self) -> f64 {
        self.brightness
    }
    async fn set_brightness(
        &mut self,
        value: f64,
        animate: bool,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        assert!(animate, "控制中心的请求都带过渡");
        self.calls.lock().unwrap().push(value);
        self.brightness = value;
        self.brightness_changed(&emitter).await?;
        Ok(())
    }
}

struct FakeWifi;

#[zbus::interface(name = "org.loong.System1.Wifi")]
impl FakeWifi {
    #[zbus(property)]
    fn available(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn enabled(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn state(&self) -> String {
        "connected".into()
    }
    #[zbus(property)]
    fn ssid(&self) -> String {
        "听雨轩".into()
    }
    #[zbus(property)]
    fn strength(&self) -> u32 {
        64
    }
}

/// 私有 dbus-daemon。
struct Bus {
    dir: PathBuf,
    address: String,
    child: Child,
}

impl Bus {
    fn start() -> Option<Bus> {
        let have = std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|d| d.join("dbus-daemon").is_file())
        });
        if !have {
            eprintln!("跳过：没有 dbus-daemon（apk add dbus）");
            return None;
        }
        let dir = std::env::temp_dir().join(format!("loong-shell-sysd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("bus.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <listen>unix:path={}/bus</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow user="*"/>
    <allow own="*"/>
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
  </policy>
</busconfig>
"#,
                dir.display()
            ),
        )
        .unwrap();
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile", "--nosyslog", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("启动 dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Some(Bus {
            dir,
            address: address.trim().to_string(),
            child,
        })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 在总线上起一个假的 loong-sysd（占用 `org.loong.System1`）。
async fn fake_sysd(address: &str, calls: Arc<Mutex<Vec<f64>>>) -> Connection {
    zbus::connection::Builder::address(address)
        .unwrap()
        .serve_at(
            Object::Power.path(),
            FakePower {
                percentage: 57.0,
                state: "discharging".into(),
            },
        )
        .unwrap()
        .serve_at(
            Object::Backlight.path(),
            FakeBacklight {
                brightness: 0.3,
                calls,
            },
        )
        .unwrap()
        .serve_at(Object::Wifi.path(), FakeWifi)
        .unwrap()
        .name(BUS_NAME)
        .unwrap()
        .build()
        .await
        .unwrap()
}

/// 等到某个快照满足条件（超时则失败），返回它。
fn wait_for(
    rx: &mpsc::Receiver<SystemStatus>,
    what: &str,
    pred: impl Fn(&SystemStatus) -> bool,
) -> SystemStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = None;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(s) if pred(&s) => return s,
            Ok(s) => last = Some(s),
            Err(_) => break,
        }
    }
    panic!("等待“{what}”超时，最后的快照：{last:#?}");
}

#[test]
fn follows_a_service_on_a_private_bus() {
    let Some(bus) = Bus::start() else {
        return;
    };
    let (tx, rx) = mpsc::channel();
    let client = spawn(BusTarget::Address(bus.address.clone()), move |s| {
        tx.send(s).is_ok()
    });
    // 服务还没起来：不在。
    wait_for(&rx, "服务不在", |s| s.service == Service::Absent);

    let calls = Arc::new(Mutex::new(Vec::new()));
    let server = async_io::block_on(fake_sysd(&bus.address, calls.clone()));
    let s = wait_for(&rx, "读到电量", |s| {
        s.power.is_some() && s.wifi.is_some()
    });
    assert_eq!(s.service, Service::Running);
    assert_eq!(s.power.as_ref().unwrap().percentage, 57.0);
    assert_eq!(s.wifi.as_ref().unwrap().ssid, "听雨轩");
    assert_eq!(s.brightness(), Some(0.3));

    // 属性变化：插上充电器，电量 80%。
    async_io::block_on(async {
        let iface = server
            .object_server()
            .interface::<_, FakePower>(Object::Power.path())
            .await
            .unwrap();
        let mut power = iface.get_mut().await;
        power.percentage = 80.0;
        power.state = "charging".into();
        power
            .percentage_changed(iface.signal_emitter())
            .await
            .unwrap();
        power.state_changed(iface.signal_emitter()).await.unwrap();
    });
    let s = wait_for(&rx, "充电 80%", |s| {
        s.power
            .as_ref()
            .is_some_and(|p| p.percentage == 80.0 && p.state == PowerState::Charging)
    });
    assert!(s.is_available());

    // 请求：连续几次调亮度，最后的值一定送到，服务端的属性变化再传回来。
    for v in [0.4, 0.5, 0.7] {
        client.request(SystemAction::Brightness(v));
    }
    wait_for(&rx, "亮度 0.7", |s| s.brightness() == Some(0.7));
    let sent = calls.lock().unwrap().clone();
    assert_eq!(sent.last(), Some(&0.7));
    assert!(sent.len() <= 3);

    // 服务退出：不在；重新启动：再读一遍。
    drop(server);
    wait_for(&rx, "服务退出", |s| s.service == Service::Absent);
    let server = async_io::block_on(fake_sysd(&bus.address, calls.clone()));
    let s = wait_for(&rx, "服务重启", |s| {
        s.service == Service::Running && s.power.is_some()
    });
    assert_eq!(s.power.unwrap().percentage, 57.0, "重启后重新读取");
    // 新进程发出的变化照样收到（信号按总线名订阅，不绑在旧进程上）。
    async_io::block_on(async {
        let iface = server
            .object_server()
            .interface::<_, FakePower>(Object::Power.path())
            .await
            .unwrap();
        let mut power = iface.get_mut().await;
        power.percentage = 12.5;
        power
            .percentage_changed(iface.signal_emitter())
            .await
            .unwrap();
    });
    wait_for(&rx, "重启后的变化", |s| {
        s.power.as_ref().is_some_and(|p| p.percentage == 12.5)
    });
    // 请求也发给新进程。
    client.request(SystemAction::Brightness(0.2));
    wait_for(&rx, "重启后调亮度", |s| s.brightness() == Some(0.2));
}
