//! 对外的操作：状态、检查更新包、安装、确认、回滚。命令行与 loong-sysd 的 D-Bus 接口共用。
//!
//! 安装（[`apply`]）的步骤：
//!
//! 1. 取得更新锁；本次启动必须已确认（否则另一个 slot 是唯一的后备，不能覆盖）。
//! 2. 读清单并验签（签名不对时连清单都不解析），检查机型、架构、启动后端、版本。
//! 3. 来源可以读两遍时（文件、块设备），先完整读一遍、校验所有载荷的 SHA-256——
//!    **签名或哈希不对的包在这一步就被拒绝，什么都不会写**。来源是下载流时跳过这一步。
//! 4. 作废目标 slot（引导程序不会再启动它），然后写入根分区与启动镜像，边写边再算一遍哈希。
//! 5. 落盘、丢掉页缓存，回读校验写入的数据。
//! 6. 切换：让引导程序下次试一次目标 slot；没被确认就自动回到当前 slot。
//!
//! 任何一步失败，目标 slot 都保持作废状态，当前 slot 不受影响。

use std::cell::Cell;

use crate::{
    backend::{self, BackendStatus, BootState, SlotInfo},
    bundle::{BundleReader, Input, Progress, Source},
    error::{Error, Result, err},
    manifest::{BootBackend, Manifest, PayloadKind, Slot},
    storage::{self, Target},
    system::{self, System},
    version::Version,
};

/// 本机的更新状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// 机型。
    pub profile: String,
    /// 架构。
    pub arch: String,
    /// 启动后端。
    pub backend: BootBackend,
    /// 当前 slot。
    pub current: Slot,
    /// 当前系统版本（`/etc/os-release`）。
    pub version: String,
    /// 本次启动是否已确认。
    pub boot: BootState,
    /// 两个 slot。版本：当前 slot 取 os-release，另一个取启动镜像或安装记录。
    pub slots: [SlotInfo; 2],
    /// 下次启动预计用哪个 slot。
    pub next_boot: Option<Slot>,
}

impl Status {
    /// 当前 slot 的信息。
    pub fn current_info(&self) -> &SlotInfo {
        &self.slots[self.current.index()]
    }

    /// 另一个 slot 的信息。
    pub fn other_info(&self) -> &SlotInfo {
        &self.slots[self.current.other().index()]
    }
}

fn fill_versions(sys: &System, current: Slot, version: &str, st: &mut BackendStatus) {
    let recorded = sys.slot_versions();
    for info in &mut st.slots {
        if info.slot == current {
            info.version = Some(version.to_owned());
        } else if info.version.is_none() {
            info.version = recorded.get(&info.slot).cloned();
        }
    }
}

/// 读取状态。
pub fn status(sys: &System) -> Result<Status> {
    let current = sys.current_slot()?;
    let version = sys.current_version()?.to_string();
    let mut be = backend::open(sys);
    let mut st = be.status(current)?;
    fill_versions(sys, current, &version, &mut st);
    Ok(Status {
        profile: sys.config.profile.clone(),
        arch: sys.config.arch.clone(),
        backend: sys.config.boot_backend,
        current,
        version,
        boot: st.boot,
        slots: st.slots,
        next_boot: st.next_boot,
    })
}

/// 更新包是否适用于本机。
pub fn check_compat(
    sys: &System,
    m: &Manifest,
    current: &Version,
    allow_downgrade: bool,
) -> Result<()> {
    let c = &sys.config;
    if m.profile != c.profile {
        return Err(err!(
            Incompatible,
            "更新包是给机型 {} 的，本机是 {}",
            m.profile,
            c.profile
        ));
    }
    if m.arch != c.arch {
        return Err(err!(
            Incompatible,
            "更新包的架构是 {}，本机是 {}",
            m.arch,
            c.arch
        ));
    }
    if m.boot_backend != c.boot_backend {
        return Err(err!(
            Incompatible,
            "更新包的启动后端是 {}，本机是 {}",
            m.boot_backend,
            c.boot_backend
        ));
    }
    if let Some(min) = m.min_version()
        && *current < min
    {
        return Err(err!(
            Incompatible,
            "这个更新要求先升级到 {min} 或更高版本（当前 {current}）"
        ));
    }
    let new = m.version();
    if !allow_downgrade && new <= *current {
        return Err(err!(
            NotNewer,
            "更新包的版本 {new} 不比当前系统 {current} 新（要降级或重装请加 --allow-downgrade）"
        ));
    }
    Ok(())
}

/// [`inspect`] 的结果。
#[derive(Debug, Clone)]
pub struct Inspection {
    /// 清单（已验签）。
    pub manifest: Manifest,
    /// 签名所用公钥的名字。
    pub signer: String,
    /// 能否装到本机；不能时是原因。
    pub compatible: Result<(), Error>,
}

/// 只读清单并验签（很快，不读载荷）。
pub fn inspect(sys: &System, source: &Source, allow_downgrade: bool) -> Result<Inspection> {
    let keyring = sys.keyring()?;
    let input = source.open()?;
    let reader = BundleReader::open(input, &keyring)?;
    let manifest = reader.manifest().clone();
    let signer = reader.signer().to_owned();
    // 下载流读到清单就够了：直接结束下载进程。
    drop(reader);
    let compatible = sys
        .current_version()
        .and_then(|v| check_compat(sys, &manifest, &v, allow_downgrade));
    Ok(Inspection {
        manifest,
        signer,
        compatible,
    })
}

/// 验签并完整校验所有载荷（不写入）。返回清单与签名者。
pub fn verify(
    keyring: &crate::sign::Keyring,
    source: &Source,
    progress: &dyn Progress,
) -> Result<(Manifest, String)> {
    let reader = BundleReader::open(source.open()?, keyring)?;
    let manifest = reader.manifest().clone();
    let signer = reader.signer().to_owned();
    reader.verify_all(progress)?.finish()?;
    Ok((manifest, signer))
}

/// 安装的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 验签、完整校验更新包（还没有写入任何东西）。
    Checking,
    /// 写入非活动 slot。
    Writing,
    /// 回读校验写入的数据。
    Verifying,
    /// 切换启动项。
    Activating,
}

impl Phase {
    /// 英文名（D-Bus 的 `State`）。
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Checking => "checking",
            Phase::Writing => "writing",
            Phase::Verifying => "verifying",
            Phase::Activating => "activating",
        }
    }

    /// 中文说明。
    pub fn describe(self) -> &'static str {
        match self {
            Phase::Checking => "校验更新包",
            Phase::Writing => "写入",
            Phase::Verifying => "回读校验",
            Phase::Activating => "切换启动项",
        }
    }
}

/// 安装进度的接收者。
pub trait Observer {
    /// 进入新阶段。
    fn phase(&self, phase: Phase);
    /// 总进度 0.0–1.0。
    fn progress(&self, fraction: f64);
    /// 是否已取消（写入阶段取消时，目标 slot 保持作废状态）。
    fn cancelled(&self) -> bool {
        false
    }
}

/// 什么也不做的观察者。
pub struct Quiet;

impl Observer for Quiet {
    fn phase(&self, _phase: Phase) {}
    fn progress(&self, _fraction: f64) {}
}

/// 安装选项。
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyOptions {
    /// 允许装不比当前新的版本（降级、重装）。
    pub allow_downgrade: bool,
}

/// 安装结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// 新系统的版本。
    pub version: String,
    /// 写入的 slot（下次启动它）。
    pub slot: Slot,
    /// 签名所用公钥的名字。
    pub signer: String,
}

/// 把字节进度换算成总进度。
struct Tracker<'a> {
    observer: &'a dyn Observer,
    done: Cell<u64>,
    total: u64,
    last: Cell<f64>,
}

impl Progress for Tracker<'_> {
    fn advance(&self, bytes: u64) {
        self.done.set(self.done.get() + bytes);
        let f = (self.done.get() as f64 / self.total.max(1) as f64).min(1.0);
        // 每 0.5% 报告一次，免得刷屏
        if f - self.last.get() >= 0.005 || f >= 1.0 && self.last.get() < 1.0 {
            self.last.set(f);
            self.observer.progress(f);
        }
    }

    fn cancelled(&self) -> bool {
        self.observer.cancelled()
    }
}

/// 安装更新包到非活动 slot。
pub fn apply(
    sys: &System,
    source: &Source,
    opts: ApplyOptions,
    observer: &dyn Observer,
) -> Result<Applied> {
    apply_from(sys, &|| source.open(), source.is_seekable(), opts, observer)
}

/// 同 [`apply`]，来源由 `open` 打开；`seekable` 表示可以打开两次、读到相同的内容。
pub fn apply_from(
    sys: &System,
    open: &dyn Fn() -> Result<Input>,
    seekable: bool,
    opts: ApplyOptions,
    observer: &dyn Observer,
) -> Result<Applied> {
    let _lock = sys.lock()?;
    let current = sys.current_slot()?;
    let current_version = sys.current_version()?;
    let target = current.other();
    let mut be = backend::open(sys);
    if be.status(current)?.boot == BootState::Trial {
        return Err(err!(
            Unconfirmed,
            "本次启动（slot {current}）还没有确认成功：另一个 slot 是唯一的后备，现在不能覆盖它。\
             等系统确认启动（loong-boot-ok），或先回滚"
        ));
    }
    let keyring = sys.keyring()?;

    // ── 读清单、验签、检查是否适用 ──
    observer.phase(Phase::Checking);
    let reader = BundleReader::open(open()?, &keyring)?;
    let manifest = reader.manifest().clone();
    let manifest_bytes = reader.manifest_bytes().to_vec();
    let signer = reader.signer().to_owned();
    check_compat(sys, &manifest, &current_version, opts.allow_downgrade)?;
    let rootfs = manifest
        .payload_for(PayloadKind::Rootfs, target)
        .expect("validate() 保证存在")
        .clone();
    let boot = manifest
        .payload_for(be.boot_payload(), target)
        .expect("validate() 保证存在")
        .clone();
    let root_target = Target::Device(sys.root_device(target)?);
    system::check_not_in_use(sys, root_target.path())?;
    storage::check_fits(&root_target, rootfs.size)?;
    be.check_boot_space(target, boot.size)?;

    let total = if seekable { manifest.total_size() } else { 0 }
        + manifest.total_size()
        + rootfs.size
        + boot.size;
    let tracker = Tracker {
        observer,
        done: Cell::new(0),
        total,
        last: Cell::new(0.0),
    };

    // ── 第一遍：完整校验，不写入 ──
    let reader = if seekable {
        reader.verify_all(&tracker)?.finish()?;
        let again = BundleReader::open(open()?, &keyring)?;
        if again.manifest_bytes() != manifest_bytes {
            return Err(err!(Failed, "更新包在校验期间被改动了"));
        }
        again
    } else {
        reader
    };

    // ── 第二遍：作废目标 slot，写入 ──
    observer.phase(Phase::Writing);
    be.invalidate(target)?;
    let boot_target = be.boot_target(target)?;
    let written = reader
        .read_payloads(&tracker, |p, stream| {
            if p.file == rootfs.file {
                storage::write(&root_target, stream, p.size)
                    .map_err(|e| e.context(format!("写入 {}", root_target.path().display())))
            } else if p.file == boot.file {
                storage::write(&boot_target, stream, p.size)
                    .map_err(|e| e.context(format!("写入 {}", boot_target.path().display())))
            } else {
                Ok(()) // 另一个 slot 用的载荷：只校验，不写
            }
        })
        .and_then(|input| input.finish());
    if let Err(e) = written {
        be.cleanup(target);
        return Err(e);
    }

    // ── 回读校验 ──
    observer.phase(Phase::Verifying);
    for (t, p) in [(&root_target, &rootfs), (&boot_target, &boot)] {
        if let Err(e) = storage::verify(t, p.size, &p.digest(), &tracker) {
            be.cleanup(target);
            return Err(e);
        }
    }

    // ── 切换 ──
    observer.phase(Phase::Activating);
    let newer = manifest.version() > current_version;
    be.activate(target, current, newer)?;
    sys.record_slot_version(target, &manifest.version);
    observer.progress(1.0);
    Ok(Applied {
        version: manifest.version.clone(),
        slot: target,
        signer,
    })
}

/// 确认本次启动成功。返回是否有改动。
pub fn mark_good(sys: &System) -> Result<bool> {
    let _lock = sys.lock()?;
    let current = sys.current_slot()?;
    backend::open(sys).mark_good(current)
}

/// 手动回滚：下次启动另一个 slot。返回那个 slot。
pub fn rollback(sys: &System, force: bool) -> Result<Slot> {
    let _lock = sys.lock()?;
    let current = sys.current_slot()?;
    backend::open(sys).rollback(current, force)?;
    Ok(current.other())
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
