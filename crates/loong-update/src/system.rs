//! 本机信息：更新配置、当前 slot、当前版本、按 GPT 分区名查找分区、锁。
//!
//! 所有路径都相对于一个根目录（默认 `/`）。测试和开发时用 `LOONG_UPDATE_ROOT=<目录>`
//! 把它指向一个假目录树：`proc/cmdline`、`sys/class/block/*/uevent`、`dev/*`（普通文件
//! 充当块设备）、`etc/loong/update.conf` 等，不需要 root、也不会碰真实的磁盘。

use std::{
    collections::HashMap,
    fs::{self, File},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
};

use crate::{
    error::{IoContext, Result, err},
    manifest::{BootBackend, Slot},
    sign::Keyring,
    version::Version,
};

/// 环境变量：根目录（测试用）。
pub const ROOT_ENV: &str = "LOONG_UPDATE_ROOT";
/// 更新配置（mkimage.sh 生成，只读根分区上）。
pub const CONFIG_PATH: &str = "/etc/loong/update.conf";
/// 信任的更新公钥目录。
pub const KEYS_DIR: &str = "/usr/share/loong/update-keys";
/// 锁文件（同一时间只允许一个更新操作）。
pub const LOCK_PATH: &str = "/run/loong/update.lock";
/// 状态目录（在 /data 上，两个 slot 共用）：记录每个 slot 装的是哪个版本。
pub const STATE_DIR: &str = "/var/lib/loong-update";

/// 解析 shell 风格的 `KEY=value` 文件（os-release、update.conf）。
pub fn parse_env_file(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            &value[1..value.len() - 1]
        } else {
            value
        };
        out.insert(key.trim().to_owned(), value.to_owned());
    }
    out
}

/// 更新配置 `/etc/loong/update.conf`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// 机型（与更新包的 `profile` 比对）。
    pub profile: String,
    /// CPU 架构。
    pub arch: String,
    /// 启动后端。
    pub boot_backend: BootBackend,
    /// 根分区名前缀（`root_` / `system_`），后接 slot。
    pub root_part: String,
    /// ESP 的分区名（uefi-sdboot）。
    pub esp_part: String,
    /// boot 分区名前缀（android-abl：`boot_`）。
    pub boot_part: String,
    /// 动态分区所在的物理分区（android-abl：`super`；空表示根分区是普通 GPT 分区）。
    pub super_part: String,
    /// ESP 已挂载在这个目录（相对根目录）时直接使用，不再挂载（测试用）。
    pub esp_dir: Option<String>,
    /// 新写入的 slot 允许尝试启动的次数（systemd-boot 启动计数）。
    pub boot_tries: u32,
    /// qbootctl 命令（android-abl）。
    pub qbootctl: String,
}

impl Config {
    /// 解析配置文件内容。
    pub fn parse(text: &str) -> Result<Config> {
        let env = parse_env_file(text);
        let get = |key: &str| env.get(key).cloned().unwrap_or_default();
        let need = |key: &str| {
            env.get(key)
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or_else(|| err!(NotSupported, "{CONFIG_PATH} 缺少 {key}"))
        };
        let backend = need("BOOT_BACKEND")?;
        let boot_backend = BootBackend::parse(&backend)
            .ok_or_else(|| err!(NotSupported, "未知的启动后端 {backend:?}"))?;
        let or = |key: &str, default: &str| {
            let v = get(key);
            if v.is_empty() { default.to_owned() } else { v }
        };
        let boot_tries = or("BOOT_TRIES", "3")
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=9).contains(n))
            .ok_or_else(|| err!(NotSupported, "BOOT_TRIES 应为 1–9"))?;
        Ok(Config {
            profile: need("PROFILE")?,
            arch: need("ARCH")?,
            boot_backend,
            root_part: or(
                "ROOT_PART",
                match boot_backend {
                    BootBackend::UefiSdboot => "root_",
                    BootBackend::AndroidAbl => "system_",
                },
            ),
            esp_part: or("ESP_PART", "esp"),
            boot_part: or("BOOT_PART", "boot_"),
            super_part: get("SUPER_PART"),
            esp_dir: Some(get("ESP_DIR")).filter(|s| !s.is_empty()),
            boot_tries,
            qbootctl: or("QBOOTCTL", "qbootctl"),
        })
    }
}

/// 本机。
#[derive(Debug, Clone)]
pub struct System {
    root: PathBuf,
    /// 更新配置。
    pub config: Config,
}

impl System {
    /// 读取 `<root>/etc/loong/update.conf`。
    pub fn open(root: impl Into<PathBuf>) -> Result<System> {
        let root = root.into();
        let path = root.join(CONFIG_PATH.trim_start_matches('/'));
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(err!(
                    NotSupported,
                    "没有更新配置 {}：这个系统不支持 A/B 更新",
                    path.display()
                ));
            }
            Err(e) => return Err(e).ctx(path.display()),
        };
        Ok(System {
            config: Config::parse(&text)?,
            root,
        })
    }

    /// 根目录取自环境变量 `LOONG_UPDATE_ROOT`，默认 `/`。
    pub fn from_env() -> Result<System> {
        let root = std::env::var_os(ROOT_ENV)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        System::open(root)
    }

    /// 是否运行在真实系统上（根目录是 `/`）。只有这时才会挂载 ESP、efivarfs。
    pub fn is_live(&self) -> bool {
        self.root == Path::new("/")
    }

    /// 根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 绝对路径 → 根目录下的路径。
    pub fn path(&self, abs: &str) -> PathBuf {
        self.root.join(abs.trim_start_matches('/'))
    }

    /// 内核命令行。
    pub fn cmdline(&self) -> Result<String> {
        let p = self.path("/proc/cmdline");
        fs::read_to_string(&p).ctx(p.display())
    }

    /// 当前从哪个 slot 启动：内核命令行的 `loong.slot=`（两种后端的启动镜像都带），
    /// 没有时看 Android 引导程序附加的 `androidboot.slot_suffix=`。
    pub fn current_slot(&self) -> Result<Slot> {
        let cmdline = self.cmdline()?;
        let mut abl = None;
        for arg in cmdline.split_whitespace() {
            if let Some(s) = arg.strip_prefix("loong.slot=") {
                return Slot::parse(s)
                    .ok_or_else(|| err!(Failed, "内核命令行里的 slot {s:?} 无效"));
            }
            if let Some(s) = arg.strip_prefix("androidboot.slot_suffix=") {
                abl = Slot::parse(s);
            }
        }
        abl.ok_or_else(|| {
            err!(
                NotSupported,
                "内核命令行里没有 loong.slot：不是从 A/B 镜像启动的"
            )
        })
    }

    /// `/etc/os-release`。
    pub fn os_release(&self) -> Result<HashMap<String, String>> {
        let p = self.path("/etc/os-release");
        Ok(parse_env_file(&fs::read_to_string(&p).ctx(p.display())?))
    }

    /// 当前系统的版本（`/etc/os-release` 的 `VERSION_ID`）。
    pub fn current_version(&self) -> Result<Version> {
        let os = self.os_release()?;
        let v = os
            .get("VERSION_ID")
            .or_else(|| os.get("IMAGE_VERSION"))
            .ok_or_else(|| err!(Failed, "/etc/os-release 里没有 VERSION_ID"))?;
        Version::parse(v).map_err(|e| err!(Failed, "/etc/os-release: {e}"))
    }

    /// 信任的更新公钥。
    pub fn keyring(&self) -> Result<Keyring> {
        Keyring::load_dir(&self.path(KEYS_DIR))
    }

    /// 按 GPT 分区名（`/sys/class/block/*/uevent` 的 `PARTNAME`）查找分区设备。
    pub fn find_partition(&self, name: &str) -> Option<PathBuf> {
        let dir = self.path("/sys/class/block");
        let mut entries: Vec<_> = fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let Ok(text) = fs::read_to_string(entry.path().join("uevent")) else {
                continue;
            };
            let env = parse_env_file(&text);
            if env.get("PARTNAME").map(String::as_str) == Some(name) {
                let devname = env
                    .get("DEVNAME")
                    .cloned()
                    .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
                return Some(self.path("/dev").join(devname));
            }
        }
        None
    }

    /// 同 [`find_partition`](Self::find_partition)，找不到时报错。
    pub fn partition(&self, name: &str) -> Result<PathBuf> {
        self.find_partition(name)
            .ok_or_else(|| err!(NotSupported, "找不到分区 {name}"))
    }

    /// device-mapper 设备：`/dev/mapper/<名字>`，没有这个节点时按 `/sys/block/dm-*/dm/name` 查找。
    pub fn find_dm(&self, name: &str) -> Option<PathBuf> {
        let mapper = self.path("/dev/mapper").join(name);
        if mapper.exists() {
            return Some(mapper);
        }
        for entry in fs::read_dir(self.path("/sys/block")).ok()?.flatten() {
            let dm_name = fs::read_to_string(entry.path().join("dm/name")).unwrap_or_default();
            if dm_name.trim() == name {
                return Some(self.path("/dev").join(entry.file_name()));
            }
        }
        None
    }

    /// 某个 slot 的根分区。
    pub fn root_device(&self, slot: Slot) -> Result<PathBuf> {
        let name = format!("{}{slot}", self.config.root_part);
        if self.config.super_part.is_empty() {
            self.partition(&name)
        } else {
            self.find_dm(&name).ok_or_else(|| {
                err!(
                    NotSupported,
                    "找不到动态分区 {name}（/dev/mapper/{name}；initramfs 的 loong-dynpart 应已映射）"
                )
            })
        }
    }

    /// 取得更新锁；已有别的更新操作在进行时返回 `Busy`。锁随返回的文件关闭而释放。
    pub fn lock(&self) -> Result<File> {
        let path = self.path(LOCK_PATH);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).ctx(dir.display())?;
        }
        let f = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .ctx(path.display())?;
        match f.try_lock() {
            Ok(()) => Ok(f),
            Err(fs::TryLockError::WouldBlock) => Err(err!(Busy, "另一个更新操作正在进行")),
            Err(fs::TryLockError::Error(e)) => Err(e).ctx(path.display()),
        }
    }

    /// 读状态文件：每个 slot 记录的版本（`a=0.1.0`）。
    pub fn slot_versions(&self) -> HashMap<Slot, String> {
        let p = self.path(STATE_DIR).join("slots");
        let text = fs::read_to_string(p).unwrap_or_default();
        parse_env_file(&text)
            .into_iter()
            .filter_map(|(k, v)| Some((Slot::parse(&k)?, v)))
            .collect()
    }

    /// 记录某个 slot 装的版本（失败只影响 `status` 的显示，不报错）。
    pub fn record_slot_version(&self, slot: Slot, version: &str) {
        let dir = self.path(STATE_DIR);
        let mut versions = self.slot_versions();
        versions.insert(slot, version.to_owned());
        let mut text = String::from("# loong-update：各 slot 装入的系统版本\n");
        for s in Slot::ALL {
            if let Some(v) = versions.get(&s) {
                text.push_str(&format!("{s}={v}\n"));
            }
        }
        let tmp = dir.join("slots.tmp");
        let ok = fs::create_dir_all(&dir).is_ok()
            && fs::write(&tmp, text).is_ok()
            && fs::rename(&tmp, dir.join("slots")).is_ok();
        if !ok {
            eprintln!("loong-update: 无法写入 {}", dir.display());
        }
    }
}

/// 设备（或测试中充当设备的普通文件）是否正被使用：是当前的根文件系统，或已被挂载。
pub fn check_not_in_use(sys: &System, dev: &Path) -> Result<()> {
    let meta = fs::metadata(dev).ctx(dev.display())?;
    if !meta.file_type().is_block_device() {
        return Ok(());
    }
    let rdev = meta.rdev();
    if fs::metadata(sys.path("/")).is_ok_and(|m| m.dev() == rdev) {
        return Err(err!(
            Failed,
            "{} 是正在运行的根文件系统，拒绝写入",
            dev.display()
        ));
    }
    let mounts = fs::read_to_string(sys.path("/proc/mounts")).unwrap_or_default();
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(src), Some(target)) = (fields.next(), fields.next()) else {
            continue;
        };
        if !src.starts_with('/') {
            continue;
        }
        if fs::metadata(src).is_ok_and(|m| m.file_type().is_block_device() && m.rdev() == rdev) {
            return Err(err!(
                Failed,
                "{} 已挂载在 {target}，拒绝写入",
                dev.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{error::ErrorKind, testutil::TempDir};

    #[test]
    fn env_file() {
        let env = parse_env_file(
            "# comment\nNAME=\"Loong OS\"\nVERSION_ID=0.1.1\n\nX='y z'\nbroken line\n",
        );
        assert_eq!(env["NAME"], "Loong OS");
        assert_eq!(env["VERSION_ID"], "0.1.1");
        assert_eq!(env["X"], "y z");
        assert_eq!(env.len(), 3);
    }

    #[test]
    fn config_defaults_and_errors() {
        let c =
            Config::parse("PROFILE=qemu-x86_64\nARCH=x86_64\nBOOT_BACKEND=uefi-sdboot\n").unwrap();
        assert_eq!(c.root_part, "root_");
        assert_eq!(c.esp_part, "esp");
        assert_eq!(c.boot_tries, 3);
        assert_eq!(c.esp_dir, None);
        let c = Config::parse(
            "PROFILE=xiaomi-nabu\nARCH=aarch64\nBOOT_BACKEND=android-abl\nSUPER_PART=super\n",
        )
        .unwrap();
        assert_eq!(c.root_part, "system_");
        assert_eq!(c.super_part, "super");
        assert_eq!(c.qbootctl, "qbootctl");
        for bad in [
            "ARCH=x\nBOOT_BACKEND=uefi-sdboot\n",
            "PROFILE=p\nARCH=x\nBOOT_BACKEND=grub\n",
            "PROFILE=p\nARCH=x\nBOOT_BACKEND=uefi-sdboot\nBOOT_TRIES=0\n",
        ] {
            assert_eq!(
                Config::parse(bad).unwrap_err().kind(),
                ErrorKind::NotSupported
            );
        }
    }

    #[test]
    fn slot_partitions_and_versions() {
        let t = TempDir::new("system");
        t.write(
            "etc/loong/update.conf",
            "PROFILE=p\nARCH=x86_64\nBOOT_BACKEND=uefi-sdboot\n",
        );
        t.write("proc/cmdline", "console=ttyS0 quiet loong.slot=b psi=1\n");
        t.write("etc/os-release", "NAME=\"Loong OS\"\nVERSION_ID=0.1.1\n");
        t.write(
            "sys/class/block/vda/uevent",
            "MAJOR=253\nDEVNAME=vda\nDEVTYPE=disk\n",
        );
        t.write(
            "sys/class/block/vda3/uevent",
            "MAJOR=253\nMINOR=3\nDEVNAME=vda3\nDEVTYPE=partition\nPARTN=3\nPARTNAME=root_b\n",
        );
        t.write("sys/class/block/dm-1/dm/name", "system_b\n");
        let sys = System::open(t.path()).unwrap();
        assert!(!sys.is_live());
        assert_eq!(sys.current_slot().unwrap(), Slot::B);
        assert_eq!(sys.current_version().unwrap().as_str(), "0.1.1");
        assert_eq!(sys.root_device(Slot::B).unwrap(), t.path().join("dev/vda3"));
        assert_eq!(
            sys.root_device(Slot::A).unwrap_err().kind(),
            ErrorKind::NotSupported
        );
        // device-mapper：没有 /dev/mapper 节点时按 sysfs 的名字找
        t.write("sys/block/dm-1/dm/name", "system_b\n");
        assert_eq!(sys.find_dm("system_b"), Some(t.path().join("dev/dm-1")));
        t.write("dev/mapper/system_b", "");
        assert_eq!(
            sys.find_dm("system_b"),
            Some(t.path().join("dev/mapper/system_b"))
        );
        assert_eq!(sys.find_dm("system_a"), None);

        // Android 引导程序的后缀
        t.write("proc/cmdline", "androidboot.slot_suffix=_a quiet\n");
        assert_eq!(sys.current_slot().unwrap(), Slot::A);
        t.write("proc/cmdline", "quiet\n");
        assert!(sys.current_slot().is_err());

        // 状态文件
        assert!(sys.slot_versions().is_empty());
        sys.record_slot_version(Slot::B, "0.1.1");
        sys.record_slot_version(Slot::A, "0.1.2");
        let v = sys.slot_versions();
        assert_eq!(v[&Slot::A], "0.1.2");
        assert_eq!(v[&Slot::B], "0.1.1");

        // 锁
        let lock = sys.lock().unwrap();
        assert_eq!(sys.lock().unwrap_err().kind(), ErrorKind::Busy);
        drop(lock);
        sys.lock().unwrap();
    }

    #[test]
    fn missing_config_is_not_supported() {
        let t = TempDir::new("noconf");
        assert_eq!(
            System::open(t.path()).unwrap_err().kind(),
            ErrorKind::NotSupported
        );
    }
}
