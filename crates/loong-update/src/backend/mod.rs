//! 启动后端：怎样让引导程序“下次试一次另一个 slot”，以及怎样确认、回滚。
//!
//! | | uefi-sdboot（[`sdboot`]） | android-abl（[`abl`]） |
//! |---|---|---|
//! | 启动镜像 | ESP 上的 UKI `EFI/Linux/loong-<slot>.efi` | `boot_<slot>` 分区里的 boot.img |
//! | 作废目标 slot | 删除它的 UKI | `qbootctl -u <slot>`（不可启动） |
//! | 切换 | 新 UKI 命名为 `loong-<slot>+3.efi`（启动计数） | `qbootctl -s <slot>` |
//! | 确认 | 去掉文件名里的计数 | `qbootctl -m` |
//! | 自动回滚 | 计数用完的 UKI 被 systemd-boot 排到最后 | ABL 的重试计数用完后换 slot |

pub mod abl;
pub mod sdboot;

use std::process::Command;

use crate::{
    error::{Result, err},
    manifest::{BootBackend, PayloadKind, Slot},
    storage::Target,
    system::System,
};

/// 一个 slot 的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// 已确认可以启动。
    Good,
    /// 待确认：引导程序会尝试启动它，没被确认就自动回滚。`tries_left` 未知时为 `None`。
    Trial {
        /// 剩余尝试次数。
        tries_left: Option<u32>,
    },
    /// 坏的：尝试次数已用完、被标记为不可启动，或被手动回滚。
    Bad,
    /// 没有启动镜像（例如更新写到一半）。
    Missing,
}

impl SlotState {
    /// 英文名（命令行、D-Bus）。
    pub fn as_str(self) -> &'static str {
        match self {
            SlotState::Good => "good",
            SlotState::Trial { .. } => "trial",
            SlotState::Bad => "bad",
            SlotState::Missing => "missing",
        }
    }
}

/// 本次启动的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootState {
    /// 已确认（或本次启动没有使用计数）。
    Good,
    /// 试运行：还没有确认，`loong-boot-ok` 会在系统界面正常运行后确认。
    Trial,
}

impl BootState {
    /// 英文名。
    pub fn as_str(self) -> &'static str {
        match self {
            BootState::Good => "good",
            BootState::Trial => "trial",
        }
    }
}

/// 后端报告的一个 slot。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotInfo {
    /// slot。
    pub slot: Slot,
    /// 状态。
    pub state: SlotState,
    /// 启动镜像里记录的版本（UKI 的 `.osrel`；android-abl 没有）。
    pub version: Option<String>,
    /// 细节（UKI 文件名、qbootctl 的标志）。
    pub detail: String,
}

/// 后端报告的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStatus {
    /// 本次启动是否已确认。
    pub boot: BootState,
    /// 两个 slot（按 A、B 排列）。
    pub slots: [SlotInfo; 2],
    /// 下次启动预计用哪个 slot。
    pub next_boot: Option<Slot>,
}

/// 启动后端的操作。
pub trait Backend {
    /// 读取状态。
    fn status(&mut self, current: Slot) -> Result<BackendStatus>;
    /// 每个 slot 的启动载荷种类。
    fn boot_payload(&self) -> PayloadKind;
    /// 确认放得下 `size` 字节的启动镜像（在作废目标 slot 之前调用）。
    fn check_boot_space(&mut self, target: Slot, size: u64) -> Result<()>;
    /// 作废目标 slot：写入期间、以及写入失败之后，引导程序都不会启动它。
    fn invalidate(&mut self, target: Slot) -> Result<()>;
    /// 启动载荷写到哪里（uefi-sdboot 是 ESP 上的临时文件，切换时才改成正式文件名）。
    fn boot_target(&mut self, target: Slot) -> Result<Target>;
    /// 写入失败后清理临时文件（尽力而为）。
    fn cleanup(&mut self, target: Slot);
    /// 让引导程序下次尝试启动目标 slot（失败时自动回到当前 slot）。
    /// `newer` 表示新系统的版本比当前的高。
    fn activate(&mut self, target: Slot, current: Slot, newer: bool) -> Result<()>;
    /// 确认本次启动成功。返回是否有改动（已经确认过时返回 false）。
    fn mark_good(&mut self, current: Slot) -> Result<bool>;
    /// 手动回滚：下次启动另一个 slot。`force` 时即使另一个 slot 是坏的也切换。
    fn rollback(&mut self, current: Slot, force: bool) -> Result<()>;
}

/// 打开本机的启动后端。
pub fn open(sys: &System) -> Box<dyn Backend + '_> {
    match sys.config.boot_backend {
        BootBackend::UefiSdboot => Box::new(sdboot::Sdboot::new(sys)),
        BootBackend::AndroidAbl => Box::new(abl::Abl::new(sys)),
    }
}

/// 运行外部命令，返回标准输出；失败时把标准错误带进错误信息。
pub(crate) fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| err!(Failed, "无法运行 {program}: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Err(err!(
            Failed,
            "{program} {} 失败（{}）: {}",
            args.join(" "),
            out.status,
            format!("{stderr}{stdout}").trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
