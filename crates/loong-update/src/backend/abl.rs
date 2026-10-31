//! android-abl：高通 ABL 的 A/B，slot 属性由 `qbootctl` 读写。
//!
//! - 根文件系统：`super` 分区里的逻辑分区 `system_<slot>`。initramfs 的 `loong-dynpart` 启动时把
//!   两个 slot 都映射成了 `/dev/mapper/system_a`、`/dev/mapper/system_b`，直接写入非活动的那个
//!   （逻辑分区的大小在刷机时已经固定，新镜像必须放得下；不支持调整逻辑分区大小）。
//! - 启动镜像：`boot_<slot>` 分区（boot.img 的 cmdline 里写着 `loong.slot=<slot>`，所以每个 slot 一个）。
//! - 切换：`qbootctl -s <slot>`（设为活动、可启动，重置重试次数）。ABL 每次启动未确认的 slot
//!   都递减重试次数，用完后把它标为不可启动并换回另一个 slot。
//! - 确认：`qbootctl -m`；作废：`qbootctl -u <slot>`。
//!
//! **尚未在真机上验证**：只用假的块设备（临时文件）和假的 qbootctl 做了单元测试。

use crate::{
    backend::{Backend, BackendStatus, BootState, SlotInfo, SlotState, run},
    error::{Result, err},
    manifest::{PayloadKind, Slot},
    storage::{self, Target},
    system::System,
};

/// 一个 slot 的 ABL 属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    /// 活动 slot（下次启动它）。
    pub active: bool,
    /// 已确认启动成功。
    pub successful: bool,
    /// 可启动。
    pub bootable: bool,
}

/// `qbootctl`（不带参数）的输出。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dump {
    /// 当前 slot。
    pub current: Option<Slot>,
    /// 两个 slot 的属性。
    pub slots: [Option<Flags>; 2],
}

/// 解析 `qbootctl` 的输出：
///
/// ```text
/// Current slot: _a
/// SLOT _a:
///         Active      : 1
///         Successful  : 1
///         Bootable    : 1
/// SLOT _b:
///         ...
/// ```
pub fn parse_dump(text: &str) -> Dump {
    let mut dump = Dump::default();
    let mut section: Option<Slot> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Current slot:") {
            dump.current = Slot::parse(rest);
        } else if let Some(rest) = line.strip_prefix("SLOT ") {
            section = Slot::parse(rest.trim_end_matches(':'));
            if let Some(s) = section {
                dump.slots[s.index()] = Some(Flags::default());
            }
        } else if let (Some(s), Some((key, value))) = (section, line.split_once(':')) {
            let on = value.trim() == "1";
            let flags = dump.slots[s.index()].get_or_insert_with(Flags::default);
            match key.trim().to_ascii_lowercase().as_str() {
                "active" => flags.active = on,
                "successful" => flags.successful = on,
                "bootable" => flags.bootable = on,
                _ => {}
            }
        }
    }
    dump
}

/// android-abl 后端。
pub struct Abl<'a> {
    sys: &'a System,
}

impl<'a> Abl<'a> {
    /// 新建。
    pub fn new(sys: &'a System) -> Abl<'a> {
        Abl { sys }
    }

    fn qbootctl(&self, args: &[&str]) -> Result<String> {
        run(&self.sys.config.qbootctl, args)
    }

    fn dump(&self) -> Result<Dump> {
        let dump = parse_dump(&self.qbootctl(&[])?);
        if dump.slots.iter().all(Option::is_none) {
            return Err(err!(Failed, "无法解析 qbootctl 的输出"));
        }
        Ok(dump)
    }

    fn boot_partition(&self, slot: Slot) -> Result<std::path::PathBuf> {
        self.sys
            .partition(&format!("{}{slot}", self.sys.config.boot_part))
    }
}

impl Backend for Abl<'_> {
    fn status(&mut self, current: Slot) -> Result<BackendStatus> {
        let dump = self.dump()?;
        let flags = |s: Slot| dump.slots[s.index()].unwrap_or_default();
        let info = |slot: Slot| {
            let f = flags(slot);
            let state = if !f.bootable {
                SlotState::Bad
            } else if f.successful {
                SlotState::Good
            } else {
                SlotState::Trial { tries_left: None }
            };
            let mut detail = Vec::new();
            if f.active {
                detail.push("active");
            }
            if f.successful {
                detail.push("successful");
            }
            detail.push(if f.bootable { "bootable" } else { "unbootable" });
            SlotInfo {
                slot,
                state,
                version: None,
                detail: detail.join(" "),
            }
        };
        let next_boot = Slot::ALL
            .into_iter()
            .find(|&s| flags(s).active && flags(s).bootable)
            .or_else(|| Slot::ALL.into_iter().find(|&s| flags(s).bootable));
        Ok(BackendStatus {
            boot: if flags(current).successful {
                BootState::Good
            } else {
                BootState::Trial
            },
            slots: [info(Slot::A), info(Slot::B)],
            next_boot,
        })
    }

    fn boot_payload(&self) -> PayloadKind {
        PayloadKind::Boot
    }

    fn check_boot_space(&mut self, target: Slot, size: u64) -> Result<()> {
        storage::check_fits(&Target::Device(self.boot_partition(target)?), size)
    }

    fn invalidate(&mut self, target: Slot) -> Result<()> {
        self.qbootctl(&["-u", target.as_str()])?;
        Ok(())
    }

    fn boot_target(&mut self, target: Slot) -> Result<Target> {
        Ok(Target::Device(self.boot_partition(target)?))
    }

    fn cleanup(&mut self, _target: Slot) {}

    fn activate(&mut self, target: Slot, _current: Slot, _newer: bool) -> Result<()> {
        self.qbootctl(&["-s", target.as_str()])?;
        Ok(())
    }

    fn mark_good(&mut self, current: Slot) -> Result<bool> {
        let dump = self.dump()?;
        if dump.slots[current.index()].is_some_and(|f| f.successful) {
            return Ok(false);
        }
        // 不带参数时标记当前 slot（qbootctl 自己判断当前 slot，与 ABL 一致）
        self.qbootctl(&["-m"])?;
        Ok(true)
    }

    fn rollback(&mut self, current: Slot, force: bool) -> Result<()> {
        let other = current.other();
        let dump = self.dump()?;
        let bootable = dump.slots[other.index()].is_some_and(|f| f.bootable);
        if !bootable && !force {
            return Err(err!(
                Failed,
                "slot {other} 被标记为不可启动，回滚过去多半也启动不了；确定要回滚请加 --force"
            ));
        }
        self.qbootctl(&["-s", other.as_str()])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qbootctl_dump() {
        let d = parse_dump(
            "Current slot: _b\nSLOT _a:\n\tActive      : 0\n\tSuccessful  : 1\n\tBootable    : 1\n\
             SLOT _b:\n\tActive      : 1\n\tSuccessful  : 0\n\tBootable    : 1\n",
        );
        assert_eq!(d.current, Some(Slot::B));
        assert_eq!(
            d.slots[0],
            Some(Flags {
                active: false,
                successful: true,
                bootable: true
            })
        );
        assert_eq!(
            d.slots[1],
            Some(Flags {
                active: true,
                successful: false,
                bootable: true
            })
        );
        assert_eq!(parse_dump("No slots found"), Dump::default());
    }
}
