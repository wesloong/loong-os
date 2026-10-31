//! 安装、确认、回滚的端到端测试：假的根目录（`LOONG_UPDATE_ROOT` 的做法），普通文件充当分区，
//! 目录充当 ESP，shell 脚本充当 qbootctl。

use std::{
    cell::RefCell,
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

use super::*;
use crate::{
    backend::SlotState,
    bundle::{PackInfo, PackItem, pack, tests::pseudo_random},
    error::ErrorKind,
    pe::fake_pe,
    sign,
    testutil::TempDir,
};

const MIB: usize = 1 << 20;

/// 假的 qbootctl：状态存在脚本旁边的 qbootctl.state，调用记录在 qbootctl.log。
const FAKE_QBOOTCTL: &str = r#"#!/bin/sh
dir=$(dirname "$0")
S="$dir/qbootctl.state"
. "$S"
echo "$*" >> "$dir/qbootctl.log"
save() {
	printf 'current=%s\nactive=%s\na_successful=%s\na_bootable=%s\nb_successful=%s\nb_bootable=%s\n' \
		"$current" "$active" "$a_successful" "$a_bootable" "$b_successful" "$b_bootable" > "$S"
}
case "$1" in
	"")
		echo "Current slot: _$current"
		for s in a b; do
			eval "su=\$${s}_successful bo=\$${s}_bootable"
			ac=0; [ "$active" = "$s" ] && ac=1
			printf 'SLOT _%s:\n\tActive      : %s\n\tSuccessful  : %s\n\tBootable    : %s\n' "$s" "$ac" "$su" "$bo"
		done ;;
	-s) active=$2; eval "${2}_bootable=1 ${2}_successful=0"; save; echo "SLOT 1: Set as active slot" ;;
	-m) eval "${current}_successful=1"; save; echo "SLOT _$current: Marked boot successful" ;;
	-u) eval "${2}_bootable=0 ${2}_successful=0"; save; echo "SLOT _$2: Set as unbootable" ;;
	*) exit 1 ;;
esac
"#;

/// 一台假设备。
struct Device {
    t: TempDir,
    key: SigningKey,
    backend: BootBackend,
}

impl Device {
    fn new(backend: BootBackend) -> Device {
        let t = TempDir::new("device");
        let key = sign::generate().unwrap();
        t.write(
            "usr/share/loong/update-keys/test.pub",
            sign::public_pem(&key.verifying_key()),
        );
        t.write("proc/cmdline", "console=ttyS0 quiet loong.slot=a psi=1\n");
        t.write("etc/os-release", "NAME=\"Loong OS\"\nVERSION_ID=1.0.0\n");
        t.write("proc/mounts", "");
        // 根分区：每个 4 MiB，旧内容是 0xee
        match backend {
            BootBackend::UefiSdboot => {
                t.write(
                    "etc/loong/update.conf",
                    "PROFILE=test-device\nARCH=x86_64\nBOOT_BACKEND=uefi-sdboot\nESP_DIR=/esp\n",
                );
                for (dev, name) in [("vda2", "root_a"), ("vda3", "root_b")] {
                    t.write(
                        &format!("sys/class/block/{dev}/uevent"),
                        format!("DEVNAME={dev}\nDEVTYPE=partition\nPARTNAME={name}\n"),
                    );
                    t.write(&format!("dev/{dev}"), vec![0xeeu8; 4 * MIB]);
                }
                t.write("esp/EFI/Linux/loong-a.efi", uki("1.0.0", "a"));
                t.write("esp/EFI/Linux/loong-b.efi", uki("0", "b"));
                fs::create_dir_all(t.path().join("sys/firmware/efi/efivars")).unwrap();
            }
            BootBackend::AndroidAbl => {
                let q = t.write("qbootctl", FAKE_QBOOTCTL);
                fs::set_permissions(&q, fs::Permissions::from_mode(0o755)).unwrap();
                t.write(
                    "qbootctl.state",
                    "current=a\nactive=a\na_successful=1\na_bootable=1\nb_successful=1\nb_bootable=1\n",
                );
                t.write(
                    "etc/loong/update.conf",
                    format!(
                        "PROFILE=test-device\nARCH=x86_64\nBOOT_BACKEND=android-abl\nSUPER_PART=super\n\
                         QBOOTCTL={}\n",
                        q.display()
                    ),
                );
                for slot in ["a", "b"] {
                    t.write(&format!("dev/mapper/system_{slot}"), vec![0xeeu8; 4 * MIB]);
                }
                for (dev, name) in [("sda5", "boot_a"), ("sda6", "boot_b")] {
                    t.write(
                        &format!("sys/class/block/{dev}/uevent"),
                        format!("DEVNAME={dev}\nPARTNAME={name}\n"),
                    );
                    t.write(&format!("dev/{dev}"), vec![0u8; MIB]);
                }
            }
        }
        Device { t, key, backend }
    }

    fn sys(&self) -> System {
        System::open(self.t.path()).unwrap()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.t.path().join(rel)
    }

    /// 打一个更新包；`tweak` 可以改清单字段。
    fn bundle(&self, version: &str, tweak: impl FnOnce(&mut PackInfo)) -> PathBuf {
        let mut info = PackInfo {
            version: version.into(),
            profile: "test-device".into(),
            arch: "x86_64".into(),
            boot_backend: self.backend,
            min_version: None,
            root_fs: Some("erofs".into()),
            created: 1_793_000_000,
        };
        tweak(&mut info);
        let dir = self.path(&format!("bundles/{version}"));
        fs::create_dir_all(&dir).unwrap();
        let root = dir.join("root.img");
        fs::write(&root, rootfs(version)).unwrap();
        let mut items = vec![PackItem {
            kind: PayloadKind::Rootfs,
            slot: None,
            path: root,
        }];
        for slot in Slot::ALL {
            let p = dir.join(format!("boot-{slot}"));
            fs::write(&p, boot_image(version, slot.as_str())).unwrap();
            items.push(PackItem {
                kind: self.backend.boot_payload(),
                slot: Some(slot),
                path: p,
            });
        }
        let out = self.path(&format!("bundles/update-{version}.loongup"));
        pack(&info, &items, &self.key, &mut File::create(&out).unwrap()).unwrap();
        out
    }

    fn esp_files(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(self.path("esp/EFI/Linux"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn digest(&self, rel: &str) -> [u8; 32] {
        Sha256::digest(fs::read(self.path(rel)).unwrap()).into()
    }

    /// 模拟重启进入 `slot`（版本 `version`）。uefi-sdboot 时像 systemd-boot 那样递减计数并设置
    /// `LoaderBootCountPath`。
    fn reboot_into(&self, slot: Slot, version: &str) {
        self.t
            .write("proc/cmdline", format!("quiet loong.slot={slot}\n"));
        self.t
            .write("etc/os-release", format!("VERSION_ID={version}\n"));
        match self.backend {
            BootBackend::UefiSdboot => {
                let var = format!(
                    "sys/firmware/efi/efivars/LoaderBootCountPath-{}",
                    crate::backend::sdboot::LOADER_GUID
                );
                let _ = fs::remove_file(self.path(&var));
                for f in self.esp_files() {
                    let name = crate::backend::sdboot::UkiName::parse(&f).unwrap();
                    if let (true, Some((left, done))) = (name.slot == slot, name.counter) {
                        let next = crate::backend::sdboot::UkiName {
                            slot,
                            counter: Some((left.saturating_sub(1), done + 1)),
                        }
                        .file_name();
                        fs::rename(
                            self.path(&format!("esp/EFI/Linux/{f}")),
                            self.path(&format!("esp/EFI/Linux/{next}")),
                        )
                        .unwrap();
                        let mut raw = vec![7, 0, 0, 0];
                        for u in format!("\\EFI\\Linux\\{next}").encode_utf16().chain([0]) {
                            raw.extend_from_slice(&u.to_le_bytes());
                        }
                        fs::write(self.path(&var), raw).unwrap();
                    }
                }
            }
            BootBackend::AndroidAbl => {
                let state = fs::read_to_string(self.path("qbootctl.state")).unwrap();
                let state: String = state
                    .lines()
                    .map(|l| {
                        if l.starts_with("current=") {
                            format!("current={slot}\n")
                        } else {
                            format!("{l}\n")
                        }
                    })
                    .collect();
                self.t.write("qbootctl.state", state);
            }
        }
    }
}

fn uki(version: &str, slot: &str) -> Vec<u8> {
    let osrel = format!("NAME=\"Loong OS\"\nVERSION_ID={version}\nIMAGE_VERSION={version}\n");
    let cmdline = format!("quiet loong.slot={slot}");
    fake_pe(&[
        (".osrel", osrel.as_bytes()),
        (".cmdline", cmdline.as_bytes()),
        (".linux", &pseudo_random(50_000, 7)),
    ])
}

fn boot_image(version: &str, slot: &str) -> Vec<u8> {
    let mut data = uki(version, slot);
    data.extend(pseudo_random(10_000, slot.as_bytes()[0]));
    data
}

fn rootfs(version: &str) -> Vec<u8> {
    let mut data = format!("rootfs {version}\n").into_bytes();
    data.extend(pseudo_random(2 * MIB + 4321, version.len() as u8));
    data
}

fn apply_file(dev: &Device, bundle: &Path, allow_downgrade: bool) -> Result<Applied> {
    apply(
        &dev.sys(),
        &Source::Path(bundle.to_owned()),
        ApplyOptions { allow_downgrade },
        &Quiet,
    )
}

/// 记录阶段与进度，可以在进度达到某个值时取消。
#[derive(Default)]
struct Recorder {
    phases: RefCell<Vec<Phase>>,
    last: Cell<f64>,
    cancel_at: Option<f64>,
}

impl Observer for Recorder {
    fn phase(&self, phase: Phase) {
        self.phases.borrow_mut().push(phase);
    }
    fn progress(&self, fraction: f64) {
        assert!(fraction >= self.last.get(), "进度不能倒退");
        self.last.set(fraction);
    }
    fn cancelled(&self) -> bool {
        self.cancel_at.is_some_and(|at| self.last.get() >= at)
    }
}

fn starts_with_file(dev: &Device, rel: &str, data: &[u8]) -> bool {
    fs::read(dev.path(rel)).unwrap().starts_with(data)
}

#[test]
fn uefi_upgrade_boot_confirm_and_rollback() {
    let dev = Device::new(BootBackend::UefiSdboot);
    let st = status(&dev.sys()).unwrap();
    assert_eq!((st.current, st.version.as_str()), (Slot::A, "1.0.0"));
    assert_eq!(st.boot, BootState::Good);
    assert_eq!(st.next_boot, Some(Slot::A));
    assert_eq!(st.other_info().version.as_deref(), Some("0"));

    let bundle = dev.bundle("1.1.0", |_| {});
    let rec = Recorder::default();
    let applied = apply(
        &dev.sys(),
        &Source::Path(bundle),
        ApplyOptions::default(),
        &rec,
    )
    .unwrap();
    assert_eq!((applied.slot, applied.version.as_str()), (Slot::B, "1.1.0"));
    assert_eq!(applied.signer, "test.pub");
    assert_eq!(
        *rec.phases.borrow(),
        [
            Phase::Checking,
            Phase::Writing,
            Phase::Verifying,
            Phase::Activating
        ]
    );
    assert_eq!(rec.last.get(), 1.0);

    // root_b 写入了新根文件系统，root_a 没动；新 UKI 带 3 次启动计数，旧的保持已确认
    assert!(starts_with_file(&dev, "dev/vda3", &rootfs("1.1.0")));
    assert_eq!(
        fs::read(dev.path("dev/vda2")).unwrap(),
        vec![0xeeu8; 4 * MIB]
    );
    assert_eq!(dev.esp_files(), ["loong-a.efi", "loong-b+3.efi"]);
    assert_eq!(
        fs::read(dev.path("esp/EFI/Linux/loong-b+3.efi")).unwrap(),
        boot_image("1.1.0", "b")
    );
    let st = status(&dev.sys()).unwrap();
    assert_eq!(st.next_boot, Some(Slot::B));
    assert_eq!(
        st.other_info().state,
        SlotState::Trial {
            tries_left: Some(3)
        }
    );
    assert_eq!(st.other_info().version.as_deref(), Some("1.1.0"));

    // 重启进入 B：试运行，未确认前拒绝安装下一个更新
    dev.reboot_into(Slot::B, "1.1.0");
    let st = status(&dev.sys()).unwrap();
    assert_eq!((st.current, st.boot), (Slot::B, BootState::Trial));
    let next = dev.bundle("1.2.0", |_| {});
    assert_eq!(
        apply_file(&dev, &next, false).unwrap_err().kind(),
        ErrorKind::Unconfirmed
    );

    // 确认：去掉计数
    assert!(mark_good(&dev.sys()).unwrap());
    assert_eq!(dev.esp_files(), ["loong-a.efi", "loong-b.efi"]);
    assert!(!mark_good(&dev.sys()).unwrap());
    let st = status(&dev.sys()).unwrap();
    assert_eq!(st.boot, BootState::Good);
    assert_eq!(st.next_boot, Some(Slot::B));

    // 手动回滚到 A：A 重新获得尝试次数，B 标成坏的
    assert_eq!(rollback(&dev.sys(), false).unwrap(), Slot::A);
    assert_eq!(dev.esp_files(), ["loong-a+3.efi", "loong-b+0.efi"]);
    assert_eq!(status(&dev.sys()).unwrap().next_boot, Some(Slot::A));
}

#[test]
fn uefi_failed_trial_falls_back() {
    let dev = Device::new(BootBackend::UefiSdboot);
    apply_file(&dev, &dev.bundle("1.1.0", |_| {}), false).unwrap();
    // 新系统连续 3 次没被确认：每次启动计数减一，用完后 systemd-boot 回到 A
    for _ in 0..3 {
        dev.reboot_into(Slot::B, "1.1.0");
    }
    assert_eq!(dev.esp_files(), ["loong-a.efi", "loong-b+0-3.efi"]);
    dev.reboot_into(Slot::A, "1.0.0");
    let st = status(&dev.sys()).unwrap();
    assert_eq!((st.current, st.boot), (Slot::A, BootState::Good));
    assert_eq!(st.other_info().state, SlotState::Bad);
    assert_eq!(st.next_boot, Some(Slot::A));
    // 坏 slot 不能直接回滚过去，--force 可以
    assert!(rollback(&dev.sys(), false).is_err());
    rollback(&dev.sys(), true).unwrap();
    assert_eq!(dev.esp_files(), ["loong-a+0.efi", "loong-b+3.efi"]);
    // 坏 slot 可以直接装新的更新
    let dev = Device::new(BootBackend::UefiSdboot);
    apply_file(&dev, &dev.bundle("1.1.0", |_| {}), false).unwrap();
    for _ in 0..3 {
        dev.reboot_into(Slot::B, "1.1.0");
    }
    dev.reboot_into(Slot::A, "1.0.0");
    apply_file(&dev, &dev.bundle("1.1.1", |_| {}), false).unwrap();
    assert_eq!(dev.esp_files(), ["loong-a.efi", "loong-b+3.efi"]);
}

#[test]
fn rejects_bad_bundles_before_writing_anything() {
    let dev = Device::new(BootBackend::UefiSdboot);
    let before = (dev.digest("dev/vda3"), dev.esp_files());

    // 别的密钥签的
    let good = dev.bundle("1.1.0", |_| {});
    let stranger = Device {
        t: TempDir::new("stranger"),
        key: sign::generate().unwrap(),
        backend: BootBackend::UefiSdboot,
    };
    let foreign = stranger.bundle("1.1.0", |_| {});
    assert_eq!(
        apply_file(&dev, &foreign, false).unwrap_err().kind(),
        ErrorKind::BadSignature
    );
    // 载荷被改了一个字节（签名仍然有效）：第一遍完整校验时发现
    let tampered = dev.path("tampered.loongup");
    let mut data = fs::read(&good).unwrap();
    let at = data.len() - 200_000;
    data[at] ^= 1;
    fs::write(&tampered, data).unwrap();
    let e = apply_file(&dev, &tampered, false).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::HashMismatch, "{e}");
    // 清单与本机不符
    for (bundle, kind) in [
        (
            dev.bundle("1.1.1", |i| i.profile = "other-device".into()),
            ErrorKind::Incompatible,
        ),
        (
            dev.bundle("1.1.2", |i| i.arch = "aarch64".into()),
            ErrorKind::Incompatible,
        ),
        (
            dev.bundle("1.1.3", |i| i.min_version = Some("1.0.5".into())),
            ErrorKind::Incompatible,
        ),
        (dev.bundle("0.9.0", |_| {}), ErrorKind::NotNewer),
        (dev.bundle("1.0.0", |_| {}), ErrorKind::NotNewer),
    ] {
        assert_eq!(apply_file(&dev, &bundle, false).unwrap_err().kind(), kind);
    }
    // 不是更新包
    let junk = dev.t.write("junk", pseudo_random(5000, 3));
    assert_eq!(
        apply_file(&dev, &junk, false).unwrap_err().kind(),
        ErrorKind::InvalidBundle
    );
    assert_eq!((dev.digest("dev/vda3"), dev.esp_files()), before);
    assert_eq!(status(&dev.sys()).unwrap().next_boot, Some(Slot::A));
}

#[test]
fn streamed_bundle_with_bad_payload_leaves_target_invalid() {
    let dev = Device::new(BootBackend::UefiSdboot);
    let good = dev.bundle("1.1.0", |_| {});
    let mut data = fs::read(&good).unwrap();
    let at = data.len() - 200_000; // 在根文件系统载荷的末尾
    data[at] ^= 1;
    let data = std::sync::Arc::new(data);
    let open = || -> Result<Input> {
        Ok(Input::Reader(Box::new(std::io::Cursor::new(
            data.as_ref().clone(),
        ))))
    };
    // 下载流只能读一遍：边写边校验，哈希不符时目标 slot 保持作废，不会被启动
    let e = apply_from(&dev.sys(), &open, false, ApplyOptions::default(), &Quiet).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::HashMismatch, "{e}");
    assert_eq!(dev.esp_files(), ["loong-a.efi"]);
    let st = status(&dev.sys()).unwrap();
    assert_eq!(st.other_info().state, SlotState::Missing);
    assert_eq!(st.next_boot, Some(Slot::A));
    // 好的流可以装
    let data = fs::read(&good).unwrap();
    let open =
        || -> Result<Input> { Ok(Input::Reader(Box::new(std::io::Cursor::new(data.clone())))) };
    apply_from(&dev.sys(), &open, false, ApplyOptions::default(), &Quiet).unwrap();
    assert_eq!(dev.esp_files(), ["loong-a.efi", "loong-b+3.efi"]);
}

#[test]
fn cancel_leaves_target_invalid() {
    let dev = Device::new(BootBackend::UefiSdboot);
    let bundle = dev.bundle("1.1.0", |_| {});
    let rec = Recorder {
        cancel_at: Some(0.6),
        ..Default::default()
    };
    let e = apply(
        &dev.sys(),
        &Source::Path(bundle),
        ApplyOptions::default(),
        &rec,
    )
    .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Cancelled, "{e}");
    assert_eq!(dev.esp_files(), ["loong-a.efi"]);
    // 锁已释放
    dev.sys().lock().unwrap();
}

#[test]
fn uefi_downgrade_demotes_current_slot() {
    let dev = Device::new(BootBackend::UefiSdboot);
    let old = dev.bundle("0.9.0", |_| {});
    apply_file(&dev, &old, true).unwrap();
    assert_eq!(dev.esp_files(), ["loong-a+0.efi", "loong-b+3.efi"]);
    assert_eq!(status(&dev.sys()).unwrap().next_boot, Some(Slot::B));
}

#[test]
fn too_large_rootfs_is_rejected_before_writing() {
    let dev = Device::new(BootBackend::UefiSdboot);
    let bundle = dev.bundle("1.1.0", |_| {});
    // 把 root_b 缩到放不下
    let f = fs::OpenOptions::new()
        .write(true)
        .open(dev.path("dev/vda3"))
        .unwrap();
    f.set_len(MIB as u64).unwrap();
    assert_eq!(
        apply_file(&dev, &bundle, false).unwrap_err().kind(),
        ErrorKind::TooLarge
    );
    assert_eq!(dev.esp_files(), ["loong-a.efi", "loong-b.efi"]);
}

#[test]
fn inspect_reports_manifest_and_compatibility() {
    let dev = Device::new(BootBackend::UefiSdboot);
    let i = inspect(
        &dev.sys(),
        &Source::Path(dev.bundle("1.1.0", |_| {})),
        false,
    )
    .unwrap();
    assert_eq!(i.manifest.version, "1.1.0");
    assert_eq!(i.signer, "test.pub");
    assert!(i.compatible.is_ok());
    let i = inspect(
        &dev.sys(),
        &Source::Path(dev.bundle("0.1.0", |_| {})),
        false,
    )
    .unwrap();
    assert_eq!(i.compatible.unwrap_err().kind(), ErrorKind::NotNewer);
}

fn qbootctl_log(dev: &Device) -> Vec<String> {
    fs::read_to_string(dev.path("qbootctl.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn abl_upgrade_confirm_and_rollback() {
    let dev = Device::new(BootBackend::AndroidAbl);
    let st = status(&dev.sys()).unwrap();
    assert_eq!((st.current, st.boot), (Slot::A, BootState::Good));
    assert_eq!(st.next_boot, Some(Slot::A));

    let bundle = dev.bundle("1.1.0", |_| {});
    apply_file(&dev, &bundle, false).unwrap();
    // 先作废 b，写入 system_b 与 boot_b，最后切换到 b（只读的查询不计）
    assert_eq!(qbootctl_log(&dev), ["-u b", "-s b"]);
    assert!(starts_with_file(
        &dev,
        "dev/mapper/system_b",
        &rootfs("1.1.0")
    ));
    assert_eq!(
        fs::read(dev.path("dev/mapper/system_a")).unwrap(),
        vec![0xeeu8; 4 * MIB]
    );
    assert!(starts_with_file(
        &dev,
        "dev/sda6",
        &boot_image("1.1.0", "b")
    ));
    assert_eq!(fs::read(dev.path("dev/sda5")).unwrap(), vec![0u8; MIB]);
    let st = status(&dev.sys()).unwrap();
    assert_eq!(st.next_boot, Some(Slot::B));
    assert_eq!(st.other_info().version.as_deref(), Some("1.1.0"));
    assert_eq!(st.other_info().state, SlotState::Trial { tries_left: None });

    dev.reboot_into(Slot::B, "1.1.0");
    let st = status(&dev.sys()).unwrap();
    assert_eq!((st.current, st.boot), (Slot::B, BootState::Trial));
    assert!(mark_good(&dev.sys()).unwrap());
    assert!(!mark_good(&dev.sys()).unwrap());
    assert_eq!(status(&dev.sys()).unwrap().boot, BootState::Good);

    assert_eq!(rollback(&dev.sys(), false).unwrap(), Slot::A);
    assert_eq!(qbootctl_log(&dev).last().unwrap(), "-s a");
    assert_eq!(status(&dev.sys()).unwrap().next_boot, Some(Slot::A));
}

#[test]
fn abl_rejects_boot_image_larger_than_partition() {
    let dev = Device::new(BootBackend::AndroidAbl);
    let bundle = dev.bundle("1.1.0", |_| {});
    let f = fs::OpenOptions::new()
        .write(true)
        .open(dev.path("dev/sda6"))
        .unwrap();
    f.set_len(4096).unwrap();
    assert_eq!(
        apply_file(&dev, &bundle, false).unwrap_err().kind(),
        ErrorKind::TooLarge
    );
    // 没有作废任何 slot
    assert!(qbootctl_log(&dev).is_empty());
}

#[test]
fn abl_hash_mismatch_keeps_target_unbootable() {
    let dev = Device::new(BootBackend::AndroidAbl);
    let good = dev.bundle("1.1.0", |_| {});
    let mut data = fs::read(&good).unwrap();
    let at = data.len() - 50_000;
    data[at] ^= 1;
    let open =
        || -> Result<Input> { Ok(Input::Reader(Box::new(std::io::Cursor::new(data.clone())))) };
    let e = apply_from(&dev.sys(), &open, false, ApplyOptions::default(), &Quiet).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::HashMismatch);
    assert_eq!(qbootctl_log(&dev), ["-u b"]);
    let st = status(&dev.sys()).unwrap();
    assert_eq!(st.other_info().state, SlotState::Bad);
    assert_eq!(st.next_boot, Some(Slot::A));
}
