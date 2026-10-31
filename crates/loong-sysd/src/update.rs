//! 系统更新：总线名 `org.loong.Update1`，对象 `/org/loong/Update1`，接口 `org.loong.Update1`。
//!
//! 直接链接 `loong-update` 库（与命令行 `loong-update` 走同一套代码）。安装在单独的线程里
//! 进行（几分钟的块设备读写），进度通过 `ProgressChanged` 信号与 `State`/`Progress` 属性报告，
//! 结束时发 `Finished`。状态（两个 slot、本次启动是否已确认）在启动时、每次操作之后、
//! 以及调用 `Status()` 时重新读取。
//!
//! 读属性、`Status()` 对所有人开放；`Inspect`、`Apply`、`Cancel`、`Rollback` 只允许 root 与
//! 会话用户 loong。非 root 调用者给的更新包路径必须是它自己能读的普通文件
//! （loong-sysd 以 root 运行，不能替调用者读它读不到的文件）。
//! 接口说明见 `docs/api/loong-update.md`。

use std::{
    collections::HashMap,
    os::unix::fs::{FileTypeExt, MetadataExt},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

use loong_update::{
    ApplyOptions, BootState, ErrorKind, Observer, Phase, SlotState, Source, System, ops,
};
use zbus::{
    Connection, DBusError, interface,
    message::Header,
    object_server::SignalEmitter,
    zvariant::{OwnedValue, Value},
};

use crate::{
    access,
    bus::{self, Changed},
};

/// 总线名。
pub const BUS_NAME: &str = "org.loong.Update1";
/// 对象路径。
pub const PATH: &str = "/org/loong/Update1";
/// 接口名。
pub const INTERFACE: &str = "org.loong.Update1";

/// `org.loong.Update1` 的错误：`org.loong.Update1.Error.<名称>`。
#[derive(Debug, DBusError)]
#[zbus(prefix = "org.loong.Update1.Error")]
pub enum UpdateError {
    /// D-Bus 层面的错误。
    #[zbus(error)]
    ZBus(zbus::Error),
    /// 调用者无权执行这个操作。
    AccessDenied(String),
    /// 参数不合法（例如相对路径、调用者读不到的文件）。
    InvalidArgs(String),
    /// 不是合法的更新包。
    InvalidBundle(String),
    /// 签名无效。
    BadSignature(String),
    /// 载荷哈希不符或回读校验失败。
    HashMismatch(String),
    /// 更新包与本机不匹配。
    Incompatible(String),
    /// 版本不比当前新。
    NotNewer(String),
    /// 分区放不下。
    TooLarge(String),
    /// 本次启动还没确认。
    Unconfirmed(String),
    /// 正在进行另一个更新操作。
    Busy(String),
    /// 已取消。
    Cancelled(String),
    /// 本机不支持 A/B 更新。
    NotSupported(String),
    /// 读写错误。
    Io(String),
    /// 其他失败。
    Failed(String),
}

impl From<loong_update::Error> for UpdateError {
    fn from(e: loong_update::Error) -> UpdateError {
        let m = e.message().to_owned();
        match e.kind() {
            ErrorKind::InvalidBundle => UpdateError::InvalidBundle(m),
            ErrorKind::BadSignature => UpdateError::BadSignature(m),
            ErrorKind::HashMismatch => UpdateError::HashMismatch(m),
            ErrorKind::Incompatible => UpdateError::Incompatible(m),
            ErrorKind::NotNewer => UpdateError::NotNewer(m),
            ErrorKind::TooLarge => UpdateError::TooLarge(m),
            ErrorKind::Unconfirmed => UpdateError::Unconfirmed(m),
            ErrorKind::Busy => UpdateError::Busy(m),
            ErrorKind::Cancelled => UpdateError::Cancelled(m),
            ErrorKind::NotSupported => UpdateError::NotSupported(m),
            ErrorKind::Io => UpdateError::Io(m),
            ErrorKind::Failed => UpdateError::Failed(m),
        }
    }
}

impl From<crate::error::Error> for UpdateError {
    fn from(e: crate::error::Error) -> UpdateError {
        match e {
            crate::error::Error::ZBus(e) => UpdateError::ZBus(e),
            crate::error::Error::AccessDenied(m) => UpdateError::AccessDenied(m),
            other => UpdateError::Failed(other.to_string()),
        }
    }
}

/// 对外的错误名（`Finished` 信号里用）。
fn error_name(kind: ErrorKind) -> String {
    format!("{INTERFACE}.Error.{}", kind.name())
}

/// 属性的当前值。
#[derive(Debug, Clone, PartialEq)]
struct Props {
    available: bool,
    state: &'static str,
    progress: f64,
    profile: String,
    boot_backend: String,
    current_slot: String,
    current_version: String,
    boot_state: String,
    other_version: String,
    other_state: String,
    next_boot: String,
    pending_version: String,
    last_error: String,
    slots: Vec<(String, String, String, String)>,
}

impl Default for Props {
    fn default() -> Props {
        Props {
            available: false,
            state: "idle",
            progress: 0.0,
            profile: String::new(),
            boot_backend: String::new(),
            current_slot: String::new(),
            current_version: String::new(),
            boot_state: String::new(),
            other_version: String::new(),
            other_state: String::new(),
            next_boot: String::new(),
            pending_version: String::new(),
            last_error: String::new(),
            slots: Vec::new(),
        }
    }
}

impl Props {
    fn changed(&self, old: &Props) -> Changed {
        let mut c: Changed = Vec::new();
        macro_rules! diff {
            ($($field:ident => $name:literal),* $(,)?) => {
                $(if self.$field != old.$field {
                    c.push(($name, bus::owned(self.$field.clone())));
                })*
            };
        }
        diff!(
            available => "Available",
            state => "State",
            progress => "Progress",
            profile => "Profile",
            boot_backend => "BootBackend",
            current_slot => "CurrentSlot",
            current_version => "CurrentVersion",
            boot_state => "BootState",
            other_version => "OtherVersion",
            other_state => "OtherState",
            next_boot => "NextBoot",
            pending_version => "PendingVersion",
            last_error => "LastError",
        );
        c
    }
}

fn slot_state(s: SlotState) -> &'static str {
    s.as_str()
}

/// 更新服务的状态。
pub struct Update {
    props: Mutex<Props>,
    cancel: Arc<AtomicBool>,
    running: AtomicBool,
}

impl Default for Update {
    fn default() -> Update {
        Update {
            props: Mutex::new(Props::default()),
            cancel: Arc::new(AtomicBool::new(false)),
            running: AtomicBool::new(false),
        }
    }
}

impl Update {
    fn props(&self) -> MutexGuard<'_, Props> {
        self.props.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 修改属性，返回变化（由调用者发 `PropertiesChanged`）。
    fn update(&self, f: impl FnOnce(&mut Props)) -> Changed {
        let mut props = self.props();
        let old = props.clone();
        f(&mut props);
        props.changed(&old)
    }

    /// 重新读取状态（阻塞：可能要挂载 ESP、运行 qbootctl）。
    fn reload(&self) -> Changed {
        let status = System::from_env().and_then(|sys| ops::status(&sys));
        self.update(|p| match status {
            Ok(st) => {
                p.available = true;
                p.profile = st.profile.clone();
                p.boot_backend = st.backend.as_str().into();
                p.current_slot = st.current.as_str().into();
                p.current_version = st.version.clone();
                p.boot_state = match st.boot {
                    BootState::Good => "good".into(),
                    BootState::Trial => "trial".into(),
                };
                let other = st.other_info();
                p.other_version = other.version.clone().unwrap_or_default();
                p.other_state = slot_state(other.state).into();
                p.next_boot = st
                    .next_boot
                    .map(|s| s.as_str().to_owned())
                    .unwrap_or_default();
                p.slots = st
                    .slots
                    .iter()
                    .map(|s| {
                        (
                            s.slot.as_str().to_owned(),
                            slot_state(s.state).to_owned(),
                            s.version.clone().unwrap_or_default(),
                            s.detail.clone(),
                        )
                    })
                    .collect();
            }
            Err(e) => {
                if e.kind() == ErrorKind::NotSupported {
                    p.available = false;
                } else {
                    tracing::warn!("读取更新状态失败: {e}");
                }
                p.slots.clear();
            }
        })
    }
}

/// 启动时读取一次状态。
pub async fn init(update: Arc<Update>, conn: Connection) {
    let u = update.clone();
    let changed = blocking::unblock(move || u.reload()).await;
    bus::emit_changed(&conn, PATH, INTERFACE, &changed).await;
    let p = update.props();
    if p.available {
        tracing::info!(
            "更新：slot {}，版本 {}，本次启动 {}，另一个 slot {}（{}）",
            p.current_slot,
            p.current_version,
            p.boot_state,
            p.other_version,
            p.other_state
        );
    } else {
        tracing::info!("更新：本机没有 A/B 更新配置，Update1 不可用");
    }
}

/// 检查非 root 调用者能否读这个来源。
fn check_source_access(source: &Source, uid: u32) -> Result<(), UpdateError> {
    let path = match source {
        Source::Url(_) => return Ok(()),
        Source::Stdin => return Err(UpdateError::InvalidArgs("不能从标准输入安装".into())),
        Source::Path(p) => p,
    };
    if !path.is_absolute() {
        return Err(UpdateError::InvalidArgs("更新包路径必须是绝对路径".into()));
    }
    if uid == 0 {
        return Ok(());
    }
    let meta = std::fs::metadata(path)
        .map_err(|e| UpdateError::InvalidArgs(format!("{}: {e}", path.display())))?;
    if meta.file_type().is_block_device() || !meta.is_file() {
        return Err(UpdateError::AccessDenied(
            "只有 root 可以从块设备或特殊文件安装".into(),
        ));
    }
    let mode = meta.mode();
    let readable = (meta.uid() == uid && mode & 0o400 != 0) || mode & 0o004 != 0;
    if !readable {
        return Err(UpdateError::AccessDenied(format!(
            "uid {uid} 不能读取 {}",
            path.display()
        )));
    }
    Ok(())
}

async fn require(conn: &Connection, hdr: &Header<'_>) -> Result<u32, UpdateError> {
    access::require_privileged(conn, hdr).await?;
    Ok(access::caller_uid(conn, hdr).await?)
}

/// 安装线程里的进度接收者：直接在本线程把信号发到总线上。
struct Reporter {
    update: Arc<Update>,
    conn: Connection,
    cancel: Arc<AtomicBool>,
    last_pct: std::cell::Cell<i64>,
}

impl Reporter {
    fn emit(&self, changed: Changed) {
        let (state, progress) = {
            let p = self.update.props();
            (p.state, p.progress)
        };
        async_io::block_on(async {
            bus::emit_changed(&self.conn, PATH, INTERFACE, &changed).await;
            if let Ok(emitter) = SignalEmitter::new(&self.conn, PATH)
                && let Err(e) = UpdateIface::progress_signal(&emitter, state, progress).await
            {
                tracing::warn!("发送 ProgressChanged 信号失败: {e}");
            }
        });
    }
}

impl Observer for Reporter {
    fn phase(&self, phase: Phase) {
        tracing::info!("更新：{}", phase.describe());
        let changed = self.update.update(|p| p.state = phase.as_str());
        self.emit(changed);
    }

    fn progress(&self, fraction: f64) {
        // 信号每 1% 发一次
        let pct = (fraction * 100.0).floor() as i64;
        if pct == self.last_pct.get() {
            return;
        }
        self.last_pct.set(pct);
        let changed = self.update.update(|p| p.progress = fraction);
        self.emit(changed);
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// D-Bus 接口 `org.loong.Update1`。
pub struct UpdateIface(pub Arc<Update>);

#[interface(name = "org.loong.Update1")]
impl UpdateIface {
    /// 本机是否支持 A/B 更新（有 `/etc/loong/update.conf`）。
    #[zbus(property)]
    fn available(&self) -> bool {
        self.0.props().available
    }

    /// `idle` / `checking` / `writing` / `verifying` / `activating` / `ready`（已写入，重启后生效）/ `failed`。
    #[zbus(property)]
    fn state(&self) -> String {
        self.0.props().state.into()
    }

    /// 安装进度 0.0–1.0。
    #[zbus(property)]
    fn progress(&self) -> f64 {
        self.0.props().progress
    }

    /// 机型。
    #[zbus(property)]
    fn profile(&self) -> String {
        self.0.props().profile.clone()
    }

    /// 启动后端（`uefi-sdboot` / `android-abl`）。
    #[zbus(property)]
    fn boot_backend(&self) -> String {
        self.0.props().boot_backend.clone()
    }

    /// 当前 slot（`a` / `b`）。
    #[zbus(property)]
    fn current_slot(&self) -> String {
        self.0.props().current_slot.clone()
    }

    /// 当前系统版本。
    #[zbus(property)]
    fn current_version(&self) -> String {
        self.0.props().current_version.clone()
    }

    /// 本次启动：`good`（已确认）/ `trial`（试运行，等待 loong-boot-ok 确认）。
    #[zbus(property)]
    fn boot_state(&self) -> String {
        self.0.props().boot_state.clone()
    }

    /// 另一个 slot 的版本（不知道时为空）。
    #[zbus(property)]
    fn other_version(&self) -> String {
        self.0.props().other_version.clone()
    }

    /// 另一个 slot 的状态：`good` / `trial` / `bad` / `missing`。
    #[zbus(property)]
    fn other_state(&self) -> String {
        self.0.props().other_state.clone()
    }

    /// 下次启动预计用哪个 slot。
    #[zbus(property)]
    fn next_boot(&self) -> String {
        self.0.props().next_boot.clone()
    }

    /// 已安装、等待重启的版本（没有时为空）。
    #[zbus(property)]
    fn pending_version(&self) -> String {
        self.0.props().pending_version.clone()
    }

    /// 上一次失败的原因（给人看的中文）。
    #[zbus(property)]
    fn last_error(&self) -> String {
        self.0.props().last_error.clone()
    }

    /// 重新读取并返回完整状态。对所有人开放。
    async fn status(
        &self,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<HashMap<String, OwnedValue>, UpdateError> {
        let u = self.0.clone();
        let changed = blocking::unblock(move || u.reload()).await;
        bus::emit_changed(conn, PATH, INTERFACE, &changed).await;
        let p = self.0.props().clone();
        let mut out: HashMap<String, OwnedValue> = HashMap::new();
        let mut put = |k: &str, v: Value<'_>| {
            out.insert(k.into(), v.try_into().expect("不含文件描述符"));
        };
        put("Available", p.available.into());
        put("State", p.state.into());
        put("Profile", p.profile.as_str().into());
        put("BootBackend", p.boot_backend.as_str().into());
        put("CurrentSlot", p.current_slot.as_str().into());
        put("CurrentVersion", p.current_version.as_str().into());
        put("BootState", p.boot_state.as_str().into());
        put("NextBoot", p.next_boot.as_str().into());
        put("PendingVersion", p.pending_version.as_str().into());
        put("Slots", Value::from(p.slots.clone()));
        Ok(out)
    }

    /// 读清单并验签（不读载荷，很快），返回版本、机型等以及能否安装到本机。
    async fn inspect(
        &self,
        source: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<HashMap<String, OwnedValue>, UpdateError> {
        let uid = require(conn, &hdr).await?;
        let source = Source::parse(&source);
        check_source_access(&source, uid)?;
        let i = blocking::unblock(move || {
            System::from_env().and_then(|sys| ops::inspect(&sys, &source, false))
        })
        .await?;
        let m = &i.manifest;
        let mut out: HashMap<String, OwnedValue> = HashMap::new();
        let mut put = |k: &str, v: Value<'_>| {
            out.insert(k.into(), v.try_into().expect("不含文件描述符"));
        };
        put("Version", m.version.as_str().into());
        put("Profile", m.profile.as_str().into());
        put("Arch", m.arch.as_str().into());
        put("BootBackend", m.boot_backend.as_str().into());
        put(
            "MinVersion",
            m.min_version.clone().unwrap_or_default().into(),
        );
        put("Created", m.created.clone().unwrap_or_default().into());
        put("Size", m.total_size().into());
        put("Signer", i.signer.as_str().into());
        put("Compatible", i.compatible.is_ok().into());
        let (name, reason) = match &i.compatible {
            Ok(()) => (String::new(), String::new()),
            Err(e) => (error_name(e.kind()), e.message().to_owned()),
        };
        put("Error", name.into());
        put("Reason", reason.into());
        Ok(out)
    }

    /// 开始安装（立即返回）。进度见 `ProgressChanged` 信号，结束时发 `Finished`。
    /// `allow_downgrade`：允许装不比当前新的版本（降级、重装）。
    /// （参数用简单类型而不是 `a{sv}`，镜像里只有 dbus-send，它构造不出 `a{sv}`。）
    async fn apply(
        &self,
        source: String,
        allow_downgrade: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), UpdateError> {
        let uid = require(conn, &hdr).await?;
        let source = Source::parse(&source);
        check_source_access(&source, uid)?;
        if self.0.running.swap(true, Ordering::AcqRel) {
            return Err(UpdateError::Busy("正在安装另一个更新".into()));
        }
        self.0.cancel.store(false, Ordering::Relaxed);
        let changed = self.0.update(|p| {
            p.state = "checking";
            p.progress = 0.0;
            p.last_error.clear();
            p.pending_version.clear();
        });
        bus::emit_changed(conn, PATH, INTERFACE, &changed).await;
        tracing::info!("uid {uid} 请求安装更新 {source}");

        let update = self.0.clone();
        let conn = conn.clone();
        std::thread::Builder::new()
            .name("loong-update".into())
            .spawn(move || run_apply(update, conn, source, allow_downgrade))
            .map_err(|e| {
                self.0.running.store(false, Ordering::Release);
                UpdateError::Failed(format!("无法启动安装线程: {e}"))
            })?;
        Ok(())
    }

    /// 取消正在进行的安装（目标 slot 保持作废，当前系统不受影响）。
    async fn cancel(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), UpdateError> {
        require(conn, &hdr).await?;
        if !self.0.running.load(Ordering::Acquire) {
            return Err(UpdateError::Failed("没有正在进行的安装".into()));
        }
        self.0.cancel.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// 下次启动改用另一个 slot（重启后生效）。`force` 时即使另一个 slot 被标为坏的也切换。
    /// 返回那个 slot。
    async fn rollback(
        &self,
        force: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<String, UpdateError> {
        let uid = require(conn, &hdr).await?;
        tracing::info!("uid {uid} 请求回滚（force={force}）");
        let slot = blocking::unblock(move || {
            System::from_env().and_then(|sys| ops::rollback(&sys, force))
        })
        .await?;
        let u = self.0.clone();
        let changed = blocking::unblock(move || u.reload()).await;
        bus::emit_changed(conn, PATH, INTERFACE, &changed).await;
        Ok(slot.as_str().into())
    }

    /// 安装进度（每 1% 一次，阶段变化时也发）：`state` 同 `State` 属性，`progress` 0.0–1.0。
    #[zbus(signal, name = "ProgressChanged")]
    pub async fn progress_signal(
        emitter: &SignalEmitter<'_>,
        state: &str,
        progress: f64,
    ) -> zbus::Result<()>;

    /// 安装结束。成功时 `error` 为空；失败时是错误名（`org.loong.Update1.Error.*`）与说明。
    #[zbus(signal)]
    pub async fn finished(
        emitter: &SignalEmitter<'_>,
        success: bool,
        error: &str,
        message: &str,
    ) -> zbus::Result<()>;
}

/// 安装线程。
fn run_apply(update: Arc<Update>, conn: Connection, source: Source, allow_downgrade: bool) {
    let reporter = Reporter {
        update: update.clone(),
        conn: conn.clone(),
        cancel: update.cancel.clone(),
        last_pct: std::cell::Cell::new(-1),
    };
    let result = System::from_env()
        .and_then(|sys| ops::apply(&sys, &source, ApplyOptions { allow_downgrade }, &reporter));
    let mut changed = match &result {
        Ok(a) => {
            tracing::info!("更新：已把 {} 写入 slot {}，重启后生效", a.version, a.slot);
            update.update(|p| {
                p.state = "ready";
                p.progress = 1.0;
                p.pending_version = a.version.clone();
            })
        }
        Err(e) => {
            tracing::warn!("更新失败（{}）: {e}", e.kind().name());
            update.update(|p| {
                p.state = "failed";
                p.last_error = e.message().to_owned();
            })
        }
    };
    // 状态变了（另一个 slot 的版本、下次启动的 slot）
    changed.extend(update.reload());
    let (success, error, message) = match &result {
        Ok(a) => (
            true,
            String::new(),
            format!("已安装 {}，重启后生效", a.version),
        ),
        Err(e) => (false, error_name(e.kind()), e.message().to_owned()),
    };
    update.running.store(false, Ordering::Release);
    async_io::block_on(async {
        // 同一属性可能出现两次（先 apply 的结果，再 reload）：去重，保留后一次
        let mut seen = std::collections::HashSet::new();
        let mut deduped: Changed = Vec::new();
        for (name, value) in changed.into_iter().rev() {
            if seen.insert(name) {
                deduped.push((name, value));
            }
        }
        bus::emit_changed(&conn, PATH, INTERFACE, &deduped).await;
        if let Ok(emitter) = SignalEmitter::new(&conn, PATH)
            && let Err(e) = UpdateIface::finished(&emitter, success, &error, &message).await
        {
            tracing::warn!("发送 Finished 信号失败: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_access_for_unprivileged_callers() {
        let dir = std::env::temp_dir().join(format!("loong-sysd-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("u.loongup");
        std::fs::write(&file, b"x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let set =
            |mode| std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
        let owner = std::fs::metadata(&file).unwrap().uid();
        let src = Source::Path(file.clone());
        set(0o600);
        assert!(check_source_access(&src, owner).is_ok());
        assert!(check_source_access(&src, 0).is_ok());
        assert!(matches!(
            check_source_access(&src, owner + 4242),
            Err(UpdateError::AccessDenied(_))
        ));
        set(0o644);
        assert!(check_source_access(&src, owner + 4242).is_ok());
        assert!(matches!(
            check_source_access(&Source::Path("rel/u.loongup".into()), 0),
            Err(UpdateError::InvalidArgs(_))
        ));
        assert!(matches!(
            check_source_access(&Source::Stdin, 0),
            Err(UpdateError::InvalidArgs(_))
        ));
        assert!(check_source_access(&Source::Url("https://x/u".into()), 4242).is_ok());
        assert!(matches!(
            check_source_access(&Source::Path(dir.clone()), owner + 4242),
            Err(UpdateError::AccessDenied(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn error_names() {
        assert_eq!(
            error_name(ErrorKind::BadSignature),
            "org.loong.Update1.Error.BadSignature"
        );
        let e: UpdateError = loong_update::Error::new(ErrorKind::HashMismatch, "坏了").into();
        assert!(matches!(e, UpdateError::HashMismatch(ref m) if m == "坏了"));
    }
}
