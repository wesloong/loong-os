//! loong-update：Loong OS 的 A/B 系统更新。
//!
//! - [`bundle`]：签名更新包（ustar 归档：`manifest.toml` + `manifest.sig` + 载荷）的读取与打包；
//! - [`manifest`]：清单格式；[`sign`]：ed25519 签名与公钥；[`version`]：版本比较；
//! - [`system`]：本机的更新配置、当前 slot、分区查找；[`storage`]：写入与回读校验。
//!
//! 设计说明见 `docs/adr/0006-ota-更新包与-loong-update.md`，接口见 `docs/api/loong-update.md`。

pub mod bundle;
pub mod error;
pub mod manifest;
pub mod sign;
pub mod storage;
pub mod system;
pub mod tar;
pub mod version;

#[cfg(test)]
pub(crate) mod testutil;

pub use error::{Error, ErrorKind, Result};
pub use manifest::{BootBackend, Manifest, Payload, PayloadKind, Slot};
pub use version::Version;
