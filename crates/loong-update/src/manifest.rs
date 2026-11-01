//! 更新包清单（`manifest.toml`）。
//!
//! ```toml
//! format = 1
//! version = "0.1.1"
//! profile = "qemu-x86_64"
//! arch = "x86_64"
//! boot_backend = "uefi-sdboot"
//! min_version = "0.1.0"          # 可选：设备至少要运行这个版本才能安装
//! root_fs = "erofs"              # 可选：说明用
//! created = "2026-10-31T08:00:00Z"
//!
//! [[payload]]
//! kind = "rootfs"                # 根文件系统镜像 → 目标 slot 的根分区
//! file = "rootfs.img"
//! size = 201326592
//! sha256 = "…"
//!
//! [[payload]]
//! kind = "uki"                   # uefi-sdboot：目标 slot 的 UKI（cmdline 里带 loong.slot，所以每个 slot 一个）
//! slot = "a"
//! file = "loong-a.efi"
//! size = 16777216
//! sha256 = "…"
//! ```
//!
//! android-abl 的启动镜像是 `kind = "boot"`（`boot_a.img` / `boot_b.img`）。
//! 清单经 ed25519 签名（见 [`crate::sign`]），载荷的完整性由清单里的 SHA-256 保证。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    error::{Result, err},
    version::Version,
};

/// 清单格式版本。不兼容的修改要增加这个数字，旧的 loong-update 会拒绝新格式。
pub const FORMAT: u32 = 1;
/// 更新包中清单的文件名（第一个成员）。
pub const MANIFEST_NAME: &str = "manifest.toml";
/// 更新包中签名的文件名（第二个成员，64 字节 ed25519 签名）。
pub const SIGNATURE_NAME: &str = "manifest.sig";
/// 清单的大小上限。
pub const MAX_MANIFEST_SIZE: u64 = 64 * 1024;

/// A/B 的一个 slot。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    /// slot A。
    A,
    /// slot B。
    B,
}

impl Slot {
    /// 两个 slot。
    pub const ALL: [Slot; 2] = [Slot::A, Slot::B];

    /// 另一个 slot。
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }

    /// 小写字母 `a` / `b`。
    pub fn as_str(self) -> &'static str {
        match self {
            Slot::A => "a",
            Slot::B => "b",
        }
    }

    /// 解析 `a`、`b`（也接受 Android 的后缀写法 `_a`、`_b` 和大写）。
    pub fn parse(s: &str) -> Option<Slot> {
        match s.trim().trim_start_matches('_') {
            "a" | "A" => Some(Slot::A),
            "b" | "B" => Some(Slot::B),
            _ => None,
        }
    }

    /// 在数组里的下标（A = 0，B = 1）。
    pub fn index(self) -> usize {
        match self {
            Slot::A => 0,
            Slot::B => 1,
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 启动后端（与 `system/profiles/*.conf` 的 `BOOT_BACKEND` 相同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BootBackend {
    /// UEFI → systemd-boot → 每个 slot 一个 UKI（启动计数）。
    #[serde(rename = "uefi-sdboot")]
    UefiSdboot,
    /// 高通 ABL → boot_a/boot_b → super 里的 system_a/system_b（qbootctl）。
    #[serde(rename = "android-abl")]
    AndroidAbl,
}

impl BootBackend {
    /// 配置文件与清单里的名字。
    pub fn as_str(self) -> &'static str {
        match self {
            BootBackend::UefiSdboot => "uefi-sdboot",
            BootBackend::AndroidAbl => "android-abl",
        }
    }

    /// 解析名字。
    pub fn parse(s: &str) -> Option<BootBackend> {
        match s {
            "uefi-sdboot" => Some(BootBackend::UefiSdboot),
            "android-abl" => Some(BootBackend::AndroidAbl),
            _ => None,
        }
    }

    /// 这个后端每个 slot 需要的启动载荷种类。
    pub fn boot_payload(self) -> PayloadKind {
        match self {
            BootBackend::UefiSdboot => PayloadKind::Uki,
            BootBackend::AndroidAbl => PayloadKind::Boot,
        }
    }
}

impl fmt::Display for BootBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 载荷种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PayloadKind {
    /// 只读根文件系统镜像（erofs / squashfs），写入目标 slot 的根分区。
    Rootfs,
    /// uefi-sdboot：统一内核镜像，写入 ESP。
    Uki,
    /// android-abl：Android boot.img，写入 `boot_<slot>` 分区。
    Boot,
}

impl PayloadKind {
    /// 清单里的名字。
    pub fn as_str(self) -> &'static str {
        match self {
            PayloadKind::Rootfs => "rootfs",
            PayloadKind::Uki => "uki",
            PayloadKind::Boot => "boot",
        }
    }

    /// 解析名字。
    pub fn parse(s: &str) -> Option<PayloadKind> {
        match s {
            "rootfs" => Some(PayloadKind::Rootfs),
            "uki" => Some(PayloadKind::Uki),
            "boot" => Some(PayloadKind::Boot),
            _ => None,
        }
    }
}

impl fmt::Display for PayloadKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一个载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    /// 种类。
    pub kind: PayloadKind,
    /// 只用于某个 slot（UKI、boot.img 的 cmdline 里写着 slot）；不写表示两个 slot 通用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<Slot>,
    /// 更新包里的文件名。
    pub file: String,
    /// 字节数。
    pub size: u64,
    /// SHA-256（64 个小写十六进制字符）。
    pub sha256: String,
}

impl Payload {
    /// 期望的 SHA-256。
    pub fn digest(&self) -> [u8; 32] {
        parse_hex32(&self.sha256).expect("validate() 已检查格式")
    }

    /// 简短的说明，例如 `uki（slot a，loong-a.efi）`。
    pub fn describe(&self) -> String {
        match self.slot {
            Some(slot) => format!("{}（slot {slot}，{}）", self.kind, self.file),
            None => format!("{}（{}）", self.kind, self.file),
        }
    }
}

/// 更新包清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// 清单格式（目前是 1）。
    pub format: u32,
    /// 新系统的版本（写入新系统的 `/etc/os-release`）。
    pub version: String,
    /// 机型（`system/profiles/<机型>.conf`）。
    pub profile: String,
    /// CPU 架构（`x86_64`、`aarch64`）。
    pub arch: String,
    /// 启动后端。
    pub boot_backend: BootBackend,
    /// 设备至少要运行这个版本才能安装（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
    /// 根文件系统类型（说明用，可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_fs: Option<String>,
    /// 生成时间（RFC 3339，说明用，可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// 载荷，按在更新包里的顺序。
    #[serde(rename = "payload", default)]
    pub payloads: Vec<Payload>,
}

/// 名字是否只含安全的字符（文件名、机型名、架构名）。
pub(crate) fn is_safe_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// 解析 64 个十六进制字符。
pub fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hex = std::str::from_utf8(chunk).ok()?;
        out[i] = u8::from_str_radix(hex, 16).ok()?;
    }
    Some(out)
}

/// 32 字节 → 小写十六进制。
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Manifest {
    /// 解析并检查清单（不验签：签名由 [`crate::bundle`] 在解析之前检查）。
    pub fn parse(bytes: &[u8]) -> Result<Manifest> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| err!(InvalidBundle, "{MANIFEST_NAME} 不是 UTF-8 文本"))?;
        // 先只看 format：新格式的清单可能根本解析不成当前的结构。
        #[derive(Deserialize)]
        struct Probe {
            format: Option<u32>,
        }
        let probe: Probe = toml::from_str(text)
            .map_err(|e| err!(InvalidBundle, "{MANIFEST_NAME} 无法解析: {e}"))?;
        match probe.format {
            Some(FORMAT) => {}
            Some(f) => {
                return Err(err!(
                    InvalidBundle,
                    "不支持的清单格式 {f}（本程序支持 {FORMAT}）"
                ));
            }
            None => return Err(err!(InvalidBundle, "{MANIFEST_NAME} 缺少 format")),
        }
        let manifest: Manifest = toml::from_str(text)
            .map_err(|e| err!(InvalidBundle, "{MANIFEST_NAME} 无法解析: {e}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// 序列化成 TOML。
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("清单只含字符串、整数和数组，序列化不会失败")
    }

    /// 新系统的版本。
    pub fn version(&self) -> Version {
        Version::parse(&self.version).expect("validate() 已检查格式")
    }

    /// 最低版本要求。
    pub fn min_version(&self) -> Option<Version> {
        self.min_version
            .as_deref()
            .map(|v| Version::parse(v).expect("validate() 已检查格式"))
    }

    /// 某个 slot 需要的某种载荷：先找专属于这个 slot 的，再找通用的。
    pub fn payload_for(&self, kind: PayloadKind, slot: Slot) -> Option<&Payload> {
        self.payloads
            .iter()
            .find(|p| p.kind == kind && p.slot == Some(slot))
            .or_else(|| {
                self.payloads
                    .iter()
                    .find(|p| p.kind == kind && p.slot.is_none())
            })
    }

    /// 所有载荷的总字节数。
    pub fn total_size(&self) -> u64 {
        self.payloads.iter().map(|p| p.size).sum()
    }

    /// 检查字段格式和载荷是否齐全。
    pub fn validate(&self) -> Result<()> {
        if self.format != FORMAT {
            return Err(err!(InvalidBundle, "不支持的清单格式 {}", self.format));
        }
        Version::parse(&self.version).map_err(|e| err!(InvalidBundle, "{e}"))?;
        if let Some(min) = &self.min_version {
            Version::parse(min).map_err(|e| err!(InvalidBundle, "min_version: {e}"))?;
        }
        for (what, value) in [("profile", &self.profile), ("arch", &self.arch)] {
            if !is_safe_name(value) {
                return Err(err!(InvalidBundle, "{what} {value:?} 无效"));
            }
        }
        let mut seen_files: Vec<&str> = Vec::new();
        let mut seen_kinds: Vec<(PayloadKind, Option<Slot>)> = Vec::new();
        for p in &self.payloads {
            if !is_safe_name(&p.file) || p.file == MANIFEST_NAME || p.file == SIGNATURE_NAME {
                return Err(err!(InvalidBundle, "载荷文件名 {:?} 无效", p.file));
            }
            if seen_files.contains(&p.file.as_str()) {
                return Err(err!(InvalidBundle, "载荷文件名 {:?} 重复", p.file));
            }
            seen_files.push(&p.file);
            if seen_kinds.contains(&(p.kind, p.slot)) {
                return Err(err!(InvalidBundle, "载荷 {} 重复", p.describe()));
            }
            seen_kinds.push((p.kind, p.slot));
            if parse_hex32(&p.sha256).is_none() || p.sha256.bytes().any(|b| b.is_ascii_uppercase())
            {
                return Err(err!(InvalidBundle, "载荷 {} 的 sha256 格式不对", p.file));
            }
            if p.size == 0 {
                return Err(err!(InvalidBundle, "载荷 {} 是空的", p.file));
            }
            let allowed = match p.kind {
                PayloadKind::Rootfs => true,
                PayloadKind::Uki => self.boot_backend == BootBackend::UefiSdboot,
                PayloadKind::Boot => self.boot_backend == BootBackend::AndroidAbl,
            };
            if !allowed {
                return Err(err!(
                    InvalidBundle,
                    "{} 后端的更新包里不应有 {} 载荷",
                    self.boot_backend,
                    p.kind
                ));
            }
        }
        // 每个 slot 都要能装：根文件系统 + 这个后端的启动镜像。
        for slot in Slot::ALL {
            for kind in [PayloadKind::Rootfs, self.boot_backend.boot_payload()] {
                if self.payload_for(kind, slot).is_none() {
                    return Err(err!(InvalidBundle, "缺少 slot {slot} 的 {kind} 载荷"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    const SAMPLE: &str = r#"
format = 1
version = "0.1.1"
profile = "qemu-x86_64"
arch = "x86_64"
boot_backend = "uefi-sdboot"
min_version = "0.1.0"

[[payload]]
kind = "rootfs"
file = "rootfs.img"
size = 1048576
sha256 = "0000000000000000000000000000000000000000000000000000000000000001"

[[payload]]
kind = "uki"
slot = "a"
file = "loong-a.efi"
size = 4096
sha256 = "00000000000000000000000000000000000000000000000000000000000000aa"

[[payload]]
kind = "uki"
slot = "b"
file = "loong-b.efi"
size = 4096
sha256 = "00000000000000000000000000000000000000000000000000000000000000bb"
"#;

    #[test]
    fn parses_sample() {
        let m = Manifest::parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(m.version, "0.1.1");
        assert_eq!(m.boot_backend, BootBackend::UefiSdboot);
        assert_eq!(m.min_version().unwrap().as_str(), "0.1.0");
        assert_eq!(m.payloads.len(), 3);
        assert_eq!(m.total_size(), 1048576 + 8192);
        let uki_b = m.payload_for(PayloadKind::Uki, Slot::B).unwrap();
        assert_eq!(uki_b.file, "loong-b.efi");
        assert_eq!(uki_b.digest()[31], 0xbb);
        assert_eq!(
            m.payload_for(PayloadKind::Rootfs, Slot::A).unwrap().file,
            "rootfs.img"
        );
        // 序列化后再解析，结果不变
        let again = Manifest::parse(m.to_toml().as_bytes()).unwrap();
        assert_eq!(again, m);
    }

    fn broken(from: &str, to: &str) -> ErrorKind {
        assert!(SAMPLE.contains(from), "{from}");
        Manifest::parse(SAMPLE.replacen(from, to, 1).as_bytes())
            .unwrap_err()
            .kind()
    }

    #[test]
    fn rejects_invalid_manifests() {
        use ErrorKind::InvalidBundle;
        assert_eq!(broken("format = 1", "format = 2"), InvalidBundle);
        assert_eq!(broken("format = 1", ""), InvalidBundle);
        assert_eq!(broken("\"0.1.1\"", "\"latest\""), InvalidBundle);
        assert_eq!(broken("\"0.1.0\"", "\"x\""), InvalidBundle);
        assert_eq!(broken("\"qemu-x86_64\"", "\"../etc\""), InvalidBundle);
        assert_eq!(broken("\"uefi-sdboot\"", "\"grub\""), InvalidBundle);
        // uefi-sdboot 的包里不能有 boot.img，而 android-abl 的包里必须有
        assert_eq!(broken("kind = \"uki\"", "kind = \"boot\""), InvalidBundle);
        assert_eq!(broken("\"uefi-sdboot\"", "\"android-abl\""), InvalidBundle);
        assert_eq!(broken("\"loong-b.efi\"", "\"loong-a.efi\""), InvalidBundle);
        assert_eq!(broken("\"loong-b.efi\"", "\"manifest.sig\""), InvalidBundle);
        assert_eq!(broken("\"loong-b.efi\"", "\"../x.efi\""), InvalidBundle);
        assert_eq!(broken("slot = \"b\"", "slot = \"a\""), InvalidBundle);
        assert_eq!(broken("slot = \"b\"", "slot = \"c\""), InvalidBundle);
        assert_eq!(broken("size = 1048576", "size = 0"), InvalidBundle);
        assert_eq!(broken("size = 1048576", "size = -1"), InvalidBundle);
        assert_eq!(
            broken(
                "0000000000000000000000000000000000000000000000000000000000000001",
                "00000000000000000000000000000000000000000000000000000000000001"
            ),
            InvalidBundle
        );
        assert_eq!(
            broken(
                "00000000000000000000000000000000000000000000000000000000000000aa",
                "00000000000000000000000000000000000000000000000000000000000000AA"
            ),
            InvalidBundle
        );
        assert_eq!(
            Manifest::parse(b"\xff\xfe").unwrap_err().kind(),
            InvalidBundle
        );
    }

    #[test]
    fn rootfs_without_slot_serves_both_slots_but_uki_does_not() {
        // 缺少 slot b 的 UKI
        let text = SAMPLE.replacen(
            "[[payload]]\nkind = \"uki\"\nslot = \"b\"",
            "[[payload]]\nkind = \"rootfs\"\nslot = \"b\"",
            1,
        );
        let e = Manifest::parse(text.as_bytes()).unwrap_err();
        assert!(e.message().contains("slot b 的 uki"), "{e}");
    }

    #[test]
    fn slot_parsing() {
        assert_eq!(Slot::parse("a"), Some(Slot::A));
        assert_eq!(Slot::parse("_b"), Some(Slot::B));
        assert_eq!(Slot::parse("B\n"), Some(Slot::B));
        assert_eq!(Slot::parse("c"), None);
        assert_eq!(Slot::A.other(), Slot::B);
        assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
    }
}
