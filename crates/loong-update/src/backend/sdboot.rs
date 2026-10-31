//! uefi-sdboot：systemd-boot + 每个 slot 一个 UKI（`EFI/Linux/loong-<slot>[+剩余[-已用]].efi`）。
//!
//! systemd-boot 给 `EFI/Linux/*.efi` 排序（`loader.conf` 里 `default loong-*` 选排在最前的）：
//!
//! 1. 剩余次数为 0 的（坏的）排到最后；
//! 2. 其余按 `.osrel` 的 `IMAGE_VERSION` 从高到低；
//! 3. 版本相同时按文件名从大到小。
//!
//! 每次启动带计数的 UKI 之前，systemd-boot 把文件名中的计数减一（`+3` → `+2-1`），并把新文件名
//! 写进 EFI 变量 `LoaderBootCountPath`；系统确认成功后去掉计数（`mark-good`）。
//!
//! 因此一次正常的升级只需要：作废目标 slot（删掉它的 UKI）→ 写根分区 → 把新 UKI 写成
//! `loong-<目标>+3.efi`。新 UKI 版本更高，下次启动排在最前；连续 3 次没被确认，它的计数用完、
//! 排到最后，systemd-boot 自动回到当前 slot。降级（新版本不比当前高）时，另外把当前 slot 的
//! UKI 标成坏的（`+0`），新 slot 才会排在前面；新 slot 也失败时两个都是坏的，按版本选中当前 slot。

use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
};

use crate::{
    backend::{Backend, BackendStatus, BootState, SlotInfo, SlotState, run},
    error::{IoContext, Result, err},
    manifest::{PayloadKind, Slot},
    pe,
    storage::{self, Target},
    system::System,
    version::Version,
};

/// systemd-boot 的 EFI 变量 GUID（Boot Loader Interface）。
pub const LOADER_GUID: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
/// UKI 所在目录（相对 ESP）。
pub const UKI_DIR: &str = "EFI/Linux";
/// 写入中的 UKI 的后缀：不以 `.efi` 结尾，systemd-boot 不会把它当成启动项。
const TEMP_SUFFIX: &str = ".efi.new";

/// 解析过的 UKI 文件名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UkiName {
    /// slot。
    pub slot: Slot,
    /// 启动计数（剩余次数, 已用次数）；`None` 表示已确认。
    pub counter: Option<(u32, u32)>,
}

impl UkiName {
    /// 文件名，例如 `loong-a+3.efi`、`loong-a+0-3.efi`、`loong-a.efi`。
    pub fn file_name(&self) -> String {
        match self.counter {
            None => format!("loong-{}.efi", self.slot),
            Some((left, 0)) => format!("loong-{}+{left}.efi", self.slot),
            Some((left, done)) => format!("loong-{}+{left}-{done}.efi", self.slot),
        }
    }

    /// 解析文件名（不区分大小写，FAT 本来也不区分）。不是 loong 的 UKI 时返回 `None`。
    pub fn parse(name: &str) -> Option<UkiName> {
        let lower = name.to_ascii_lowercase();
        let rest = lower.strip_suffix(".efi")?.strip_prefix("loong-")?;
        let mut chars = rest.chars();
        let slot = Slot::parse(&chars.next()?.to_string())?;
        let counter = chars.as_str();
        if counter.is_empty() {
            return Some(UkiName {
                slot,
                counter: None,
            });
        }
        let counter = counter.strip_prefix('+')?;
        let (left, done) = match counter.split_once('-') {
            Some((l, d)) => (l, d),
            None => (counter, "0"),
        };
        let num = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u32>().ok())
                .flatten()
        };
        Some(UkiName {
            slot,
            counter: Some((num(left)?, num(done)?)),
        })
    }

    fn state(&self) -> SlotState {
        match self.counter {
            None => SlotState::Good,
            Some((0, _)) => SlotState::Bad,
            Some((left, _)) => SlotState::Trial {
                tries_left: Some(left),
            },
        }
    }

    fn is_bad(&self) -> bool {
        matches!(self.counter, Some((0, _)))
    }
}

/// ESP 上的一个 UKI。
#[derive(Debug, Clone)]
struct Uki {
    file: String,
    name: UkiName,
    version: Option<String>,
}

/// 按 systemd-boot 的规则排序：排在最前的就是下次启动的。
fn boot_order(ukis: &mut [Uki]) {
    ukis.sort_by(|a, b| {
        let ver = |u: &Uki| u.version.as_deref().and_then(|v| Version::parse(v).ok());
        a.name
            .is_bad()
            .cmp(&b.name.is_bad())
            .then_with(|| ver(b).cmp(&ver(a)))
            .then_with(|| {
                b.file
                    .to_ascii_lowercase()
                    .cmp(&a.file.to_ascii_lowercase())
            })
            .then_with(|| {
                let left = |u: &Uki| u.name.counter.map(|c| c.0);
                left(b).cmp(&left(a))
            })
    });
}

/// 挂载好的 ESP。由我们挂载的，用完卸载。
struct Esp {
    dir: PathBuf,
    mounted: bool,
}

impl Drop for Esp {
    fn drop(&mut self) {
        if self.mounted {
            let dir = self.dir.to_string_lossy().into_owned();
            if let Err(e) = run("umount", &[&dir]) {
                eprintln!("loong-update: 卸载 ESP 失败: {e}");
            } else {
                let _ = fs::remove_dir(&self.dir);
            }
        }
    }
}

/// 设备 `dev` 挂载在哪里（`/proc/mounts`）。
fn find_mountpoint(sys: &System, dev: &Path) -> Option<PathBuf> {
    let rdev = fs::metadata(dev)
        .ok()
        .filter(|m| m.file_type().is_block_device())?
        .rdev();
    let mounts = fs::read_to_string(sys.path("/proc/mounts")).ok()?;
    mounts.lines().find_map(|line| {
        let mut f = line.split_whitespace();
        let (src, target) = (f.next()?, f.next()?);
        let m = fs::metadata(src).ok()?;
        (m.file_type().is_block_device() && m.rdev() == rdev)
            .then(|| PathBuf::from(target.replace("\\040", " ")))
    })
}

/// uefi-sdboot 后端。
pub struct Sdboot<'a> {
    sys: &'a System,
    esp: Option<Esp>,
}

impl<'a> Sdboot<'a> {
    /// 新建（ESP 在第一次用到时才挂载）。
    pub fn new(sys: &'a System) -> Sdboot<'a> {
        Sdboot { sys, esp: None }
    }

    fn esp(&mut self) -> Result<&Path> {
        if self.esp.is_none() {
            self.esp = Some(self.mount_esp()?);
        }
        Ok(&self.esp.as_ref().expect("刚刚挂载").dir)
    }

    fn mount_esp(&self) -> Result<Esp> {
        let sys = self.sys;
        if let Some(dir) = &sys.config.esp_dir {
            return Ok(Esp {
                dir: sys.path(dir),
                mounted: false,
            });
        }
        let dev = sys.partition(&sys.config.esp_part)?;
        if let Some(dir) = find_mountpoint(sys, &dev) {
            return Ok(Esp {
                dir,
                mounted: false,
            });
        }
        if !sys.is_live() {
            return Err(err!(
                NotSupported,
                "测试环境请在 update.conf 中设置 ESP_DIR"
            ));
        }
        // 每个进程用自己的挂载点：同一个 FAT 可以同时挂载多次，互不影响卸载。
        let dir = sys.path(&format!("/run/loong/esp.{}", std::process::id()));
        fs::create_dir_all(&dir).ctx(dir.display())?;
        let _ = run("modprobe", &["-q", "vfat"]);
        run(
            "mount",
            &[
                "-t",
                "vfat",
                "-o",
                "noatime",
                &dev.to_string_lossy(),
                &dir.to_string_lossy(),
            ],
        )
        .map_err(|e| e.context("挂载 ESP"))?;
        Ok(Esp { dir, mounted: true })
    }

    fn uki_dir(&mut self) -> Result<PathBuf> {
        Ok(self.esp()?.join(UKI_DIR))
    }

    fn ukis(&mut self) -> Result<Vec<Uki>> {
        let dir = self.uki_dir()?;
        let mut out = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e).ctx(dir.display()),
        };
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if let Some(name) = UkiName::parse(&file) {
                let version = pe::uki_version(&entry.path()).ok().flatten();
                out.push(Uki {
                    file,
                    name,
                    version,
                });
            }
        }
        boot_order(&mut out);
        Ok(out)
    }

    fn slot_ukis(&mut self, slot: Slot) -> Result<Vec<Uki>> {
        Ok(self
            .ukis()?
            .into_iter()
            .filter(|u| u.name.slot == slot)
            .collect())
    }

    fn rename(&mut self, from: &str, to: &UkiName) -> Result<()> {
        let dir = self.uki_dir()?;
        let to = to.file_name();
        if from != to {
            fs::rename(dir.join(from), dir.join(&to)).ctx(format!("{from} → {to}"))?;
        }
        Ok(())
    }

    fn temp_path(&mut self, slot: Slot) -> Result<PathBuf> {
        Ok(self.uki_dir()?.join(format!("loong-{slot}{TEMP_SUFFIX}")))
    }

    /// 本次启动对应的 UKI（相对 ESP 的路径），来自 EFI 变量 `LoaderBootCountPath`。
    /// 只有 systemd-boot 启动的是带计数的 UKI 时才有这个变量。
    fn boot_count_path(&self) -> Option<String> {
        self.ensure_efivarfs();
        let var = self.sys.path(&format!(
            "/sys/firmware/efi/efivars/LoaderBootCountPath-{LOADER_GUID}"
        ));
        let raw = fs::read(var).ok()?;
        // 4 字节属性 + UTF-16LE 字符串
        let units: Vec<u16> = raw
            .get(4..)?
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        let path = String::from_utf16_lossy(&units).replace('\\', "/");
        let path = path.trim_start_matches('/').to_owned();
        (!path.is_empty()).then_some(path)
    }

    fn ensure_efivarfs(&self) {
        if !self.sys.is_live() || !Path::new("/sys/firmware/efi").exists() {
            return;
        }
        let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
        if mounts
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some("/sys/firmware/efi/efivars"))
        {
            return;
        }
        let _ = run("modprobe", &["-q", "efivarfs"]);
        let _ = run(
            "mount",
            &["-t", "efivarfs", "efivarfs", "/sys/firmware/efi/efivars"],
        );
    }

    /// 本次启动用的、还没确认的 UKI 文件名。
    fn pending_boot_entry(&mut self) -> Result<Option<(String, UkiName)>> {
        let Some(rel) = self.boot_count_path() else {
            return Ok(None);
        };
        let path = self.esp()?.join(&rel);
        let file = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        match UkiName::parse(&file) {
            Some(name) if name.counter.is_some() && path.exists() => Ok(Some((file, name))),
            _ => Ok(None),
        }
    }
}

impl Backend for Sdboot<'_> {
    fn status(&mut self, _current: Slot) -> Result<BackendStatus> {
        let ukis = self.ukis()?;
        let boot = if self.pending_boot_entry()?.is_some() {
            BootState::Trial
        } else {
            BootState::Good
        };
        let info = |slot: Slot| {
            let mine: Vec<&Uki> = ukis.iter().filter(|u| u.name.slot == slot).collect();
            match mine.first() {
                None => SlotInfo {
                    slot,
                    state: SlotState::Missing,
                    version: None,
                    detail: String::new(),
                },
                Some(u) => SlotInfo {
                    slot,
                    state: u.name.state(),
                    version: u.version.clone(),
                    detail: if mine.len() > 1 {
                        format!("{}（另有 {} 个同 slot 的 UKI）", u.file, mine.len() - 1)
                    } else {
                        u.file.clone()
                    },
                },
            }
        };
        Ok(BackendStatus {
            boot,
            slots: [info(Slot::A), info(Slot::B)],
            next_boot: ukis.first().map(|u| u.name.slot),
        })
    }

    fn boot_payload(&self) -> PayloadKind {
        PayloadKind::Uki
    }

    fn check_boot_space(&mut self, target: Slot, size: u64) -> Result<()> {
        let dir = self.uki_dir()?;
        let st = rustix::fs::statvfs(&dir).map_err(|e| err!(Io, "{}: {e}", dir.display()))?;
        let free = st.f_bavail * st.f_frsize;
        let mut reclaimable = 0;
        for u in self.slot_ukis(target)? {
            reclaimable += fs::metadata(dir.join(&u.file)).map_or(0, |m| m.len());
        }
        let temp = self.temp_path(target)?;
        reclaimable += fs::metadata(temp).map_or(0, |m| m.len());
        // FAT 的簇与目录项留一点余量
        if size + (256 << 10) > free + reclaimable {
            return Err(err!(
                TooLarge,
                "ESP 空间不足：需要 {} KiB，可用 {} KiB",
                size >> 10,
                (free + reclaimable) >> 10
            ));
        }
        Ok(())
    }

    fn invalidate(&mut self, target: Slot) -> Result<()> {
        let dir = self.uki_dir()?;
        for u in self.slot_ukis(target)? {
            storage::remove_if_exists(&dir.join(&u.file))?;
        }
        let temp = self.temp_path(target)?;
        storage::remove_if_exists(&temp)?;
        storage::sync_dir(&dir)
    }

    fn boot_target(&mut self, target: Slot) -> Result<Target> {
        let dir = self.uki_dir()?;
        fs::create_dir_all(&dir).ctx(dir.display())?;
        Ok(Target::File(self.temp_path(target)?))
    }

    fn cleanup(&mut self, target: Slot) {
        if let Ok(temp) = self.temp_path(target) {
            let _ = fs::remove_file(temp);
        }
    }

    fn activate(&mut self, target: Slot, current: Slot, newer: bool) -> Result<()> {
        let dir = self.uki_dir()?;
        let temp = self.temp_path(target)?;
        let name = UkiName {
            slot: target,
            counter: Some((self.sys.config.boot_tries, 0)),
        };
        fs::rename(&temp, dir.join(name.file_name())).ctx(format!(
            "{} → {}",
            temp.display(),
            name.file_name()
        ))?;
        if !newer {
            // 降级：新 UKI 版本不高，不会排在前面。把当前 slot 标成坏的，新 slot 才会先被尝试；
            // 新 slot 也失败时两个都是坏的，systemd-boot 按版本选回当前 slot。
            for u in self.slot_ukis(current)? {
                let done = u.name.counter.map_or(0, |c| c.1);
                self.rename(
                    &u.file,
                    &UkiName {
                        slot: current,
                        counter: Some((0, done)),
                    },
                )?;
            }
        }
        storage::sync_dir(&dir)
    }

    fn mark_good(&mut self, current: Slot) -> Result<bool> {
        let Some((file, name)) = self.pending_boot_entry()? else {
            return Ok(false);
        };
        if name.slot != current {
            return Err(err!(
                Failed,
                "LoaderBootCountPath 指向 {file}，但当前是 slot {current}，不确认"
            ));
        }
        let good = UkiName {
            slot: current,
            counter: None,
        };
        let dir = self.uki_dir()?;
        // 不应存在同名的已确认 UKI；万一有（例如手工复制），以本次启动的为准
        if !good.file_name().eq_ignore_ascii_case(&file) {
            storage::remove_if_exists(&dir.join(good.file_name()))?;
        }
        self.rename(&file, &good)?;
        storage::sync_dir(&dir)?;
        Ok(true)
    }

    fn rollback(&mut self, current: Slot, force: bool) -> Result<()> {
        let other = current.other();
        let others = self.slot_ukis(other)?;
        let Some(first) = others.first() else {
            return Err(err!(Failed, "slot {other} 没有启动镜像（UKI），无法回滚"));
        };
        if first.name.is_bad() && !force {
            return Err(err!(
                Failed,
                "slot {other} 被标记为坏的（{}），回滚过去多半也启动不了；确定要回滚请加 --force",
                first.file
            ));
        }
        // 另一个 slot 重新获得完整的尝试次数；当前 slot 标成坏的
        self.rename(
            &first.file,
            &UkiName {
                slot: other,
                counter: Some((self.sys.config.boot_tries, 0)),
            },
        )?;
        for u in self.slot_ukis(current)? {
            let done = u.name.counter.map_or(0, |c| c.1);
            self.rename(
                &u.file,
                &UkiName {
                    slot: current,
                    counter: Some((0, done)),
                },
            )?;
        }
        let dir = self.uki_dir()?;
        storage::sync_dir(&dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uki_names() {
        let p = |s| UkiName::parse(s);
        assert_eq!(
            p("loong-a.efi"),
            Some(UkiName {
                slot: Slot::A,
                counter: None
            })
        );
        assert_eq!(p("loong-b+3.efi").unwrap().counter, Some((3, 0)));
        assert_eq!(p("LOONG-B+2-1.EFI").unwrap().counter, Some((2, 1)));
        assert_eq!(p("loong-a+0-3.efi").unwrap().state(), SlotState::Bad);
        for bad in [
            "loong-c.efi",
            "loong-a.efi.new",
            "loong-a+.efi",
            "loong-a+x.efi",
            "loong-a+1-.efi",
            "loong-ab.efi",
            "other-a.efi",
            "loong-a-3.efi",
        ] {
            assert_eq!(p(bad), None, "{bad}");
        }
        for name in [
            "loong-a.efi",
            "loong-b+3.efi",
            "loong-a+0-3.efi",
            "loong-b+1-2.efi",
        ] {
            assert_eq!(p(name).unwrap().file_name(), name);
        }
    }

    fn uki(file: &str, version: &str) -> Uki {
        Uki {
            file: file.into(),
            name: UkiName::parse(file).unwrap(),
            version: Some(version.into()),
        }
    }

    fn first(mut v: Vec<Uki>) -> String {
        boot_order(&mut v);
        v[0].file.clone()
    }

    #[test]
    fn predicts_systemd_boot_order() {
        // 升级：新版本带计数，排在最前
        assert_eq!(
            first(vec![
                uki("loong-a.efi", "0.1.0"),
                uki("loong-b+3.efi", "0.1.1")
            ]),
            "loong-b+3.efi"
        );
        // 计数用完 → 排到最后，回到旧版本
        assert_eq!(
            first(vec![
                uki("loong-a.efi", "0.1.0"),
                uki("loong-b+0-3.efi", "0.1.1")
            ]),
            "loong-a.efi"
        );
        // 初始镜像：slot B 的 IMAGE_VERSION 是 0
        assert_eq!(
            first(vec![
                uki("loong-a+3.efi", "0.1.0-dev"),
                uki("loong-b.efi", "0")
            ]),
            "loong-a+3.efi"
        );
        // 降级：当前 slot 被标成坏的，新 slot 先被尝试
        assert_eq!(
            first(vec![
                uki("loong-a+0.efi", "0.2.0"),
                uki("loong-b+3.efi", "0.1.0")
            ]),
            "loong-b+3.efi"
        );
        // 两个都坏：按版本
        assert_eq!(
            first(vec![
                uki("loong-a+0.efi", "0.2.0"),
                uki("loong-b+0-3.efi", "0.1.0")
            ]),
            "loong-a+0.efi"
        );
        // 版本相同：文件名大的在前
        assert_eq!(
            first(vec![uki("loong-a.efi", "1"), uki("loong-b.efi", "1")]),
            "loong-b.efi"
        );
    }
}
