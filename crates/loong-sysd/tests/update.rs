//! 集成测试：私有 dbus-daemon 上的 `org.loong.Update1`。loong-sysd 的 `LOONG_UPDATE_ROOT`
//! 指向一个假设备（普通文件充当 root_a / root_b，目录充当 ESP），用真正的签名更新包走一遍
//! Status → Inspect → Apply（ProgressChanged / Finished 信号）→ Rollback，并检查权限。
//!
//! 需要 `dbus-daemon` 与 `dbus-send`（没有时跳过）；以 root 运行时还会用 nobody 检查权限，
//! 并加载 `system/rootfs` 里真正的总线策略文件。

use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader},
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use async_io::Timer;
use futures_lite::{StreamExt, future};
use loong_update::{
    BootBackend, PayloadKind, Slot,
    bundle::{PackInfo, PackItem, pack},
    sign,
};
use zbus::{
    Connection, MatchRule, MessageStream,
    message::Type as MessageType,
    zvariant::{OwnedValue, Value},
};

const BUS_NAME: &str = "org.loong.Update1";
const PATH: &str = "/org/loong/Update1";
const IFACE: &str = "org.loong.Update1";
const NOBODY: u32 = 65534;
const MIB: usize = 1 << 20;

fn have(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(cmd).is_file()))
}

fn is_root() -> bool {
    fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "loong-sysd-update-it-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn write(&self, rel: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect()
}

/// 带 `.osrel` 的假 UKI（systemd-boot 按其中的 IMAGE_VERSION 排序）。
fn uki(version: &str, seed: u32) -> Vec<u8> {
    let osrel = format!("ID=loong\nVERSION_ID={version}\nIMAGE_VERSION={version}\n");
    loong_update::pe::fake_pe(&[
        (".osrel", osrel.as_bytes()),
        (".linux", &noise(50_000, seed)),
    ])
}

/// 假设备：slot a 运行 1.0.0；bundles/ 下有好的、被篡改的、别的密钥签的更新包。
struct Device {
    root: TempDir,
    good: PathBuf,
    tampered: PathBuf,
    foreign: PathBuf,
}

impl Device {
    fn new() -> Device {
        let t = TempDir::new("root");
        // 所有人可进入（nobody 的 dbus-send 要能找到总线套接字之外，没有别的要求）
        fs::set_permissions(&t.0, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        t.write(
            "etc/loong/update.conf",
            "PROFILE=test-device\nARCH=x86_64\nBOOT_BACKEND=uefi-sdboot\nESP_DIR=/esp\n",
        );
        t.write("proc/cmdline", "quiet loong.slot=a\n");
        t.write("proc/mounts", "");
        t.write("etc/os-release", "NAME=\"Loong OS\"\nVERSION_ID=1.0.0\n");
        for (dev, name) in [("vda2", "root_a"), ("vda3", "root_b")] {
            t.write(
                &format!("sys/class/block/{dev}/uevent"),
                format!("DEVNAME={dev}\nPARTNAME={name}\n"),
            );
            t.write(&format!("dev/{dev}"), vec![0u8; 4 * MIB]);
        }
        t.write("esp/EFI/Linux/loong-a.efi", uki("1.0.0", 1));
        t.write("esp/EFI/Linux/loong-b.efi", uki("0", 2));
        fs::create_dir_all(t.0.join("sys/firmware/efi/efivars")).unwrap();

        let key = sign::generate().unwrap();
        t.write(
            "usr/share/loong/update-keys/test.pub",
            sign::public_pem(&key.verifying_key()),
        );
        let items = vec![
            PackItem {
                kind: PayloadKind::Rootfs,
                slot: None,
                path: t.write("in/root.img", noise(2 * MIB + 77, 3)),
            },
            PackItem {
                kind: PayloadKind::Uki,
                slot: Some(Slot::A),
                path: t.write("in/a.efi", uki("1.1.0", 4)),
            },
            PackItem {
                kind: PayloadKind::Uki,
                slot: Some(Slot::B),
                path: t.write("in/b.efi", uki("1.1.0", 5)),
            },
        ];
        let info = PackInfo {
            version: "1.1.0".into(),
            profile: "test-device".into(),
            arch: "x86_64".into(),
            boot_backend: BootBackend::UefiSdboot,
            min_version: None,
            root_fs: None,
            created: 0,
        };
        let bundle = |name: &str, key: &sign::SigningKey| {
            let p = t.0.join("bundles").join(name);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            pack(&info, &items, key, &mut fs::File::create(&p).unwrap()).unwrap();
            fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
            p
        };
        let good = bundle("good.loongup", &key);
        let foreign = bundle("foreign.loongup", &sign::generate().unwrap());
        let tampered = t.0.join("bundles/tampered.loongup");
        let mut data = fs::read(&good).unwrap();
        let at = 4096 + MIB;
        data[at] ^= 1;
        fs::write(&tampered, data).unwrap();
        fs::set_permissions(
            t.0.join("bundles"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        Device {
            root: t,
            good,
            tampered,
            foreign,
        }
    }

    fn esp_files(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(self.root.0.join("esp/EFI/Linux"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }
}

/// 私有总线 + loong-sysd。
struct Fixture {
    dev: Device,
    run: TempDir,
    address: String,
    bus: Child,
    sysd: Child,
    conn: Connection,
}

impl Fixture {
    async fn start(system_policy: bool) -> Fixture {
        let dev = Device::new();
        let run = TempDir::new("run");
        fs::set_permissions(&run.0, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let policy = if system_policy {
            let conf = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../system/rootfs/usr/share/dbus-1/system.d");
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
  </policy>
  <include>{0}/org.loong.System1.conf</include>"#,
                conf.canonicalize().unwrap().display()
            )
        } else {
            r#"
  <policy context="default">
    <allow user="*"/>
    <allow own="*"/>
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
  </policy>"#
                .into()
        };
        let config = run.write(
            "bus.conf",
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <listen>unix:path={}/bus</listen>
  <auth>EXTERNAL</auth>{policy}
</busconfig>
"#,
                run.0.display()
            ),
        );
        let mut bus = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile", "--nosyslog", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(fs::File::create(run.0.join("bus.log")).unwrap())
            .spawn()
            .expect("启动 dbus-daemon");
        let mut address = String::new();
        BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_string();
        assert!(!address.is_empty(), "dbus-daemon 没有输出地址");

        let sysfs = run.0.join("sysfs");
        fs::create_dir_all(&sysfs).unwrap();
        let log = fs::File::create(run.0.join("sysd.log")).unwrap();
        let sysd = Command::new(env!("CARGO_BIN_EXE_loong-sysd"))
            .env("LOONG_SYSD_BUS", &address)
            .env("LOONG_SYSFS_ROOT", &sysfs)
            .env("LOONG_UPDATE_ROOT", &dev.root.0)
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
        let mut fx = Fixture {
            dev,
            run,
            address,
            bus,
            sysd,
            conn,
        };
        fx.wait_ready().await;
        fx
    }

    fn log(&self) -> String {
        let read = |n: &str| fs::read_to_string(self.run.0.join(n)).unwrap_or_default();
        format!(
            "---- loong-sysd ----\n{}---- dbus-daemon ----\n{}",
            read("sysd.log"),
            read("bus.log")
        )
    }

    async fn wait_ready(&mut self) {
        let start = Instant::now();
        loop {
            if let Ok(v) = self.try_get("Available").await
                && bool::try_from(v).unwrap()
            {
                return;
            }
            if let Ok(Some(status)) = self.sysd.try_wait() {
                panic!("loong-sysd 退出了（{status}）:\n{}", self.log());
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "Update1 没有在 10 秒内可用:\n{}",
                self.log()
            );
            Timer::after(Duration::from_millis(50)).await;
        }
    }

    async fn try_get(&self, name: &str) -> zbus::Result<OwnedValue> {
        let reply = self
            .conn
            .call_method(
                Some(BUS_NAME),
                PATH,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(IFACE, name),
            )
            .await?;
        reply.body().deserialize::<OwnedValue>()
    }

    async fn get_str(&self, name: &str) -> String {
        String::try_from(self.try_get(name).await.unwrap()).unwrap()
    }

    async fn call<B>(&self, method: &str, body: &B) -> zbus::Result<zbus::Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.conn
            .call_method(Some(BUS_NAME), PATH, Some(IFACE), method, body)
            .await
    }

    fn send_as_nobody(&self, member: &str, args: &[&str]) -> (bool, String) {
        let out = Command::new("dbus-send")
            .arg(format!("--bus={}", self.address))
            .args(["--print-reply", &format!("--dest={BUS_NAME}"), PATH, member])
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

fn error_name(r: zbus::Result<zbus::Message>) -> String {
    match r {
        Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
        Err(e) => panic!("期望 D-Bus 方法错误，得到 {e}"),
        Ok(_) => panic!("期望失败，调用却成功了"),
    }
}

async fn timeout<T>(d: Duration, f: impl std::future::Future<Output = T>) -> Option<T> {
    future::or(async { Some(f.await) }, async {
        Timer::after(d).await;
        None
    })
    .await
}

/// 等到 `Finished` 信号，返回 (成功, 错误名) 以及期间收到的 ProgressChanged 个数。
async fn wait_finished(signals: &mut MessageStream) -> ((bool, String), usize) {
    let mut progress = 0;
    loop {
        let msg = timeout(Duration::from_secs(30), signals.next())
            .await
            .expect("30 秒内没有收到 Finished")
            .unwrap()
            .unwrap();
        match msg
            .header()
            .member()
            .map(|m| m.as_str().to_owned())
            .as_deref()
        {
            Some("ProgressChanged") => {
                let (_state, fraction): (String, f64) = msg.body().deserialize().unwrap();
                assert!((0.0..=1.0).contains(&fraction));
                progress += 1;
            }
            Some("Finished") => {
                let (ok, name, _msg): (bool, String, String) = msg.body().deserialize().unwrap();
                return ((ok, name), progress);
            }
            _ => {}
        }
    }
}

fn skip() -> bool {
    if have("dbus-daemon") && have("dbus-send") {
        return false;
    }
    eprintln!("跳过：没有 dbus-daemon / dbus-send（apk add dbus）");
    true
}

#[test]
fn update_interface_end_to_end() {
    if skip() {
        return;
    }
    async_io::block_on(async {
        let fx = Fixture::start(false).await;
        assert_eq!(fx.get_str("CurrentSlot").await, "a");
        assert_eq!(fx.get_str("CurrentVersion").await, "1.0.0");
        assert_eq!(fx.get_str("BootState").await, "good");
        assert_eq!(fx.get_str("BootBackend").await, "uefi-sdboot");
        assert_eq!(fx.get_str("NextBoot").await, "a");
        assert_eq!(fx.get_str("State").await, "idle");

        let status = fx.call("Status", &()).await.unwrap();
        let status: HashMap<String, OwnedValue> = status.body().deserialize().unwrap();
        assert_eq!(&*status["Profile"], &Value::from("test-device"));
        let slots: Vec<(String, String, String, String)> =
            status["Slots"].try_clone().unwrap().try_into().unwrap();
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].0, "a");
        assert_eq!(slots[0].2, "1.0.0");

        // Inspect：只验签读清单
        let good = fx.dev.good.to_string_lossy().into_owned();
        let info = fx.call("Inspect", &(good.as_str(),)).await.unwrap();
        let info: HashMap<String, OwnedValue> = info.body().deserialize().unwrap();
        assert_eq!(&*info["Version"], &Value::from("1.1.0"));
        assert_eq!(&*info["Compatible"], &Value::from(true));
        assert_eq!(&*info["Signer"], &Value::from("test.pub"));
        let foreign = fx.dev.foreign.to_string_lossy().into_owned();
        assert_eq!(
            error_name(fx.call("Inspect", &(foreign.as_str(),)).await),
            "org.loong.Update1.Error.BadSignature"
        );
        assert_eq!(
            error_name(fx.call("Inspect", &("relative/path",)).await),
            "org.loong.Update1.Error.InvalidArgs"
        );

        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(BUS_NAME)
            .unwrap()
            .path(PATH)
            .unwrap()
            .build();
        let mut signals = MessageStream::for_match_rule(rule, &fx.conn, None)
            .await
            .unwrap();
        // 被篡改的包：第一遍校验就失败，什么都没写
        let tampered = fx.dev.tampered.to_string_lossy().into_owned();
        fx.call("Apply", &(tampered.as_str(), false)).await.unwrap();
        let ((ok, name), _) = wait_finished(&mut signals).await;
        assert!(!ok);
        assert_eq!(name, "org.loong.Update1.Error.HashMismatch");
        assert_eq!(fx.get_str("State").await, "failed");
        assert!(fx.get_str("LastError").await.contains("SHA-256"));
        assert_eq!(fx.dev.esp_files(), ["loong-a.efi", "loong-b.efi"]);

        // 好的包
        fx.call("Apply", &(good.as_str(), false)).await.unwrap();
        // 安装进行中再调用：Busy（也可能已经装完了，那就是 NotNewer 以外的结果都不对）
        let again = fx.call("Apply", &(good.as_str(), false)).await;
        if let Err(zbus::Error::MethodError(name, _, _)) = &again {
            assert_eq!(name.as_str(), "org.loong.Update1.Error.Busy");
        }
        let ((ok, name), progress) = wait_finished(&mut signals).await;
        assert!(ok, "安装失败: {name}\n{}", fx.log());
        assert!(progress >= 10, "只收到 {progress} 个 ProgressChanged");
        if again.is_ok() {
            // 第二次调用排在第一次之后完成；它会因为 slot b 已是待确认而照常重装
            let _ = wait_finished(&mut signals).await;
        }
        assert_eq!(fx.get_str("State").await, "ready");
        assert_eq!(fx.get_str("PendingVersion").await, "1.1.0");
        assert_eq!(fx.get_str("NextBoot").await, "b");
        assert_eq!(fx.get_str("OtherState").await, "trial");
        assert_eq!(fx.get_str("OtherVersion").await, "1.1.0");
        assert_eq!(fx.dev.esp_files(), ["loong-a.efi", "loong-b+3.efi"]);

        // 回滚：下次启动改回 b 之外的另一个 slot（当前是 a，所以“另一个”是 b）
        let r = fx.call("Rollback", &(false,)).await.unwrap();
        assert_eq!(r.body().deserialize::<String>().unwrap(), "b");
        assert_eq!(fx.dev.esp_files(), ["loong-a+0.efi", "loong-b+3.efi"]);

        // 权限：nobody 能读状态，不能安装（总线放行一切，由 loong-sysd 自己拒绝）
        if is_root() {
            let (ok, out) = fx.send_as_nobody("org.loong.Update1.Status", &[]);
            assert!(ok, "{out}");
            let (ok, out) = fx.send_as_nobody(
                "org.loong.Update1.Apply",
                &[&format!("string:{good}"), "boolean:false"],
            );
            assert!(
                !ok && out.contains("org.loong.Update1.Error.AccessDenied"),
                "{out}"
            );
        }
    });
}

#[test]
fn bus_policy_restricts_update_methods() {
    if skip() {
        return;
    }
    if !is_root() {
        eprintln!("跳过：需要 root");
        return;
    }
    async_io::block_on(async {
        let fx = Fixture::start(true).await;
        let (ok, out) = fx.send_as_nobody(
            "org.freedesktop.DBus.Properties.Get",
            &["string:org.loong.Update1", "string:CurrentVersion"],
        );
        assert!(ok && out.contains("1.0.0"), "{out}");
        let (ok, out) = fx.send_as_nobody("org.loong.Update1.Status", &[]);
        assert!(ok, "{out}");
        for (member, args) in [
            ("org.loong.Update1.Inspect", &["string:/x"][..]),
            (
                "org.loong.Update1.Apply",
                &["string:/x", "boolean:false"][..],
            ),
            ("org.loong.Update1.Cancel", &[][..]),
            ("org.loong.Update1.Rollback", &["boolean:false"][..]),
        ] {
            let (ok, out) = fx.send_as_nobody(member, args);
            assert!(
                !ok && out.contains("org.freedesktop.DBus.Error.AccessDenied"),
                "{member}: {out}"
            );
        }
        // root 仍然可以
        fx.call("Rollback", &(false,)).await.unwrap();
    });
}
