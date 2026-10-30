//! librime 的安全封装。
//!
//! librime 的状态是**进程全局**的（单例 `Service`、glog 只能初始化一次），而且会话
//! 接口不是线程安全的。因此：
//!
//! - [`Rime`] 每个进程只启动一次（[`Rime::start`]），之后一直存活到进程退出，以
//!   `&'static Rime` 的形式共享。再次调用 `start` 时，配置相同则直接返回已启动的
//!   实例，不同则报错 [`ImeError::AlreadyRunning`]；
//! - 所有对 librime 的调用都经过 `Rime` 内部的一把互斥锁串行化。合成器里输入法只在
//!   主线程使用，锁没有竞争，开销可以忽略；
//! - [`Session`] 是一个输入会话（对应一个输入焦点上下文），析构时自动销毁。
//!
//! 返回给调用方的数据（[`Context`]、[`Candidate`] 等）都是拷贝出来的 Rust 值，
//! 不持有 librime 的内存。

pub(crate) mod ffi;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, PoisonError};

use self::ffi::{
    Bool, RimeCandidateListIterator, RimeCommit, RimeContext, RimeSessionId, RimeStatus,
    RimeTraits, api_fn,
};
use crate::config::{self, ImeConfig};
use crate::error::ImeError;

pub use self::ffi::keysym;

/// 一个候选词。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Candidate {
    /// 候选文字。
    pub text: String,
    /// 注释（例如拼音提示、反查编码）；没有时为 `None`。
    pub comment: Option<String>,
}

/// 预编辑（组字中）文字。光标与选区位置都以 **UTF-8 字节**计，可直接用于
/// `zwp_text_input_v3.preedit_string` 的 `cursor_begin` / `cursor_end`。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Preedit {
    /// 预编辑文字，例如 `ni hao`；部分选定后可能是 `你 hao`。
    pub text: String,
    /// 光标位置（字节）。
    pub cursor: usize,
    /// 正在转换的片段起点（字节）。
    pub sel_start: usize,
    /// 正在转换的片段终点（字节）。
    pub sel_end: usize,
}

/// 当前一页候选。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Menu {
    /// 每页候选数。
    pub page_size: usize,
    /// 页码（从 0 开始）。
    pub page_no: usize,
    /// 是否最后一页。
    pub is_last_page: bool,
    /// 高亮候选在本页中的下标。
    pub highlighted: usize,
    /// 本页候选。
    pub candidates: Vec<Candidate>,
}

/// 组字状态快照。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Context {
    /// 预编辑文字；不在组字时为 `None`。
    pub preedit: Option<Preedit>,
    /// 候选菜单。
    pub menu: Menu,
    /// 如果现在确认，会上屏的文字（例如 `你好`）。
    pub commit_preview: Option<String>,
}

/// 方案与开关状态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    /// 方案 ID，例如 `luna_pinyin_simp`。
    pub schema_id: String,
    /// 方案名称，例如 `朙月拼音·简化字`。
    pub schema_name: String,
    /// 正在组字。
    pub is_composing: bool,
    /// 西文模式（ascii_mode）。
    pub is_ascii_mode: bool,
    /// 全角。
    pub is_full_shape: bool,
    /// 西文标点。
    pub is_ascii_punct: bool,
    /// 输入法暂停（例如正在部署）。
    pub is_disabled: bool,
}

/// 解析后的函数表：必需的函数都已检查过非空。
struct Api {
    initialize: unsafe extern "C" fn(*mut RimeTraits),
    finalize: unsafe extern "C" fn(),
    start_maintenance: unsafe extern "C" fn(Bool) -> Bool,
    join_maintenance_thread: unsafe extern "C" fn(),
    create_session: unsafe extern "C" fn() -> RimeSessionId,
    destroy_session: unsafe extern "C" fn(RimeSessionId) -> Bool,
    process_key: unsafe extern "C" fn(RimeSessionId, c_int, c_int) -> Bool,
    commit_composition: unsafe extern "C" fn(RimeSessionId) -> Bool,
    clear_composition: unsafe extern "C" fn(RimeSessionId),
    get_commit: unsafe extern "C" fn(RimeSessionId, *mut RimeCommit) -> Bool,
    free_commit: unsafe extern "C" fn(*mut RimeCommit) -> Bool,
    get_context: unsafe extern "C" fn(RimeSessionId, *mut RimeContext) -> Bool,
    free_context: unsafe extern "C" fn(*mut RimeContext) -> Bool,
    get_status: unsafe extern "C" fn(RimeSessionId, *mut RimeStatus) -> Bool,
    free_status: unsafe extern "C" fn(*mut RimeStatus) -> Bool,
    set_option: unsafe extern "C" fn(RimeSessionId, *const c_char, Bool),
    get_option: unsafe extern "C" fn(RimeSessionId, *const c_char) -> Bool,
    get_current_schema: unsafe extern "C" fn(RimeSessionId, *mut c_char, usize) -> Bool,
    get_input: unsafe extern "C" fn(RimeSessionId) -> *const c_char,
    select_candidate: unsafe extern "C" fn(RimeSessionId, usize) -> Bool,
    candidate_list_begin:
        unsafe extern "C" fn(RimeSessionId, *mut RimeCandidateListIterator) -> Bool,
    candidate_list_next: unsafe extern "C" fn(*mut RimeCandidateListIterator) -> Bool,
    candidate_list_end: unsafe extern "C" fn(*mut RimeCandidateListIterator),
    // 较新版本才有的接口：没有时退回到等价的旧接口。
    select_candidate_on_current_page: Option<unsafe extern "C" fn(RimeSessionId, usize) -> Bool>,
    candidate_list_from_index:
        Option<unsafe extern "C" fn(RimeSessionId, *mut RimeCandidateListIterator, c_int) -> Bool>,
    change_page: Option<unsafe extern "C" fn(RimeSessionId, Bool) -> Bool>,
}

impl Api {
    fn load(raw: &ffi::RimeApi) -> Result<Api, ImeError> {
        macro_rules! required {
            ($f:ident) => {
                api_fn!(raw, $f).ok_or(ImeError::MissingApi(stringify!($f)))?
            };
        }
        Ok(Api {
            initialize: required!(initialize),
            finalize: required!(finalize),
            start_maintenance: required!(start_maintenance),
            join_maintenance_thread: required!(join_maintenance_thread),
            create_session: required!(create_session),
            destroy_session: required!(destroy_session),
            process_key: required!(process_key),
            commit_composition: required!(commit_composition),
            clear_composition: required!(clear_composition),
            get_commit: required!(get_commit),
            free_commit: required!(free_commit),
            get_context: required!(get_context),
            free_context: required!(free_context),
            get_status: required!(get_status),
            free_status: required!(free_status),
            set_option: required!(set_option),
            get_option: required!(get_option),
            get_current_schema: required!(get_current_schema),
            get_input: required!(get_input),
            select_candidate: required!(select_candidate),
            candidate_list_begin: required!(candidate_list_begin),
            candidate_list_next: required!(candidate_list_next),
            candidate_list_end: required!(candidate_list_end),
            select_candidate_on_current_page: api_fn!(raw, select_candidate_on_current_page),
            candidate_list_from_index: api_fn!(raw, candidate_list_from_index),
            change_page: api_fn!(raw, change_page),
        })
    }
}

/// 进程内唯一的 librime 实例。
pub struct Rime {
    api: Api,
    /// 串行化所有 librime 调用；同时记录存活的会话数。
    lock: Mutex<Inner>,
    shared_data_dir: PathBuf,
    user_data_dir: PathBuf,
    schema: String,
    version: String,
}

/// 受 [`Rime`] 的锁保护的状态。
struct Inner {
    /// 存活的会话数。部署会让已有会话持有的词典映射失效，所以只能在没有会话时进行。
    live_sessions: usize,
}

struct Global {
    /// `setup`（含 glog 初始化）每个进程只能调用一次。
    setup_done: bool,
    running: Option<&'static Rime>,
}

static GLOBAL: Mutex<Global> = Mutex::new(Global {
    setup_done: false,
    running: None,
});

/// 部署（maintenance）结果，由通知回调写入。
static DEPLOY_STATE: AtomicU8 = AtomicU8::new(DEPLOY_NONE);
const DEPLOY_NONE: u8 = 0;
const DEPLOY_START: u8 = 1;
const DEPLOY_SUCCESS: u8 = 2;
const DEPLOY_FAILURE: u8 = 3;

/// librime 的通知回调。可能在部署线程中被调用，只写原子变量。
///
/// `extern "C"` 函数里发生 panic 会直接中止进程；这里没有可能 panic 的操作。
unsafe extern "C" fn on_notification(
    _context: *mut c_void,
    _session: RimeSessionId,
    message_type: *const c_char,
    message_value: *const c_char,
) {
    // SAFETY: librime 传入的是以 NUL 结尾、在回调期间有效的字符串（可能为空指针）。
    let (ty, value) = unsafe { (opt_cstr(message_type), opt_cstr(message_value)) };
    if ty == Some(b"deploy".as_slice()) {
        let state = match value {
            Some(b"start") => DEPLOY_START,
            Some(b"success") => DEPLOY_SUCCESS,
            Some(b"failure") => DEPLOY_FAILURE,
            _ => return,
        };
        DEPLOY_STATE.store(state, Ordering::SeqCst);
    }
}

/// 把可能为空的 C 字符串指针转成字节切片。
///
/// # Safety
/// `p` 为空，或指向在返回值使用期间有效、以 NUL 结尾的字符串。
unsafe fn opt_cstr<'a>(p: *const c_char) -> Option<&'a [u8]> {
    if p.is_null() {
        None
    } else {
        // SAFETY: 由调用方保证。
        Some(unsafe { CStr::from_ptr(p) }.to_bytes())
    }
}

/// 把可能为空的 C 字符串拷贝成 `String`（非法 UTF-8 用 U+FFFD 替换）。
///
/// # Safety
/// 同 [`opt_cstr`]。
unsafe fn opt_string(p: *const c_char) -> Option<String> {
    // SAFETY: 由调用方保证。
    unsafe { opt_cstr(p) }.map(|b| String::from_utf8_lossy(b).into_owned())
}

/// C 接口返回的非负整数（字节位置、页码等）转成 `usize`。
fn to_usize(v: c_int) -> usize {
    usize::try_from(v).unwrap_or(0)
}

fn path_cstring(path: &Path) -> Result<CString, ImeError> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).map_err(|_| ImeError::InvalidPath(path.to_owned()))
}

impl Rime {
    /// 启动 librime（每个进程只真正启动一次），必要时部署数据。
    ///
    /// 1. 检查共享数据目录里有所选方案；
    /// 2. 在用户目录写入 `default.custom.yaml` 等补丁（内容没变时不写，避免触发重新部署）；
    /// 3. `setup` → `initialize` → `start_maintenance(false)` → `join_maintenance_thread`：
    ///    首次运行或配置变化时会编译方案（同步等待，首次约 1 秒，见 `docs/api/loong-ime.md`）；
    /// 4. 创建一个探测会话，确认方案确实加载成功。
    ///
    /// 这个函数会阻塞到部署结束。合成器应在后台线程调用它，完成后再把键盘切到中文。
    pub fn start(config: &ImeConfig) -> Result<&'static Rime, ImeError> {
        let mut global = GLOBAL.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(rime) = global.running {
            return if rime.shared_data_dir == config.shared_data_dir
                && rime.user_data_dir == config.user_data_dir
                && rime.schema == config.schema
            {
                Ok(rime)
            } else {
                Err(ImeError::AlreadyRunning {
                    user_data_dir: rime.user_data_dir.clone(),
                })
            };
        }

        config::check_shared_data(config)?;
        config::write_user_files(config)?;

        // SAFETY: `rime_get_api` 没有前置条件，返回库内静态函数表的指针（或空指针）。
        let raw = unsafe { ffi::rime_get_api() };
        // SAFETY: 非空时指向 librime 的静态 `RimeApi`，进程内一直有效且不会被修改。
        let raw: &'static ffi::RimeApi =
            unsafe { raw.as_ref() }.ok_or(ImeError::MissingApi("rime_get_api"))?;
        let api = Api::load(raw)?;
        let setup = api_fn!(raw, setup).ok_or(ImeError::MissingApi("setup"))?;
        let set_notification_handler = api_fn!(raw, set_notification_handler)
            .ok_or(ImeError::MissingApi("set_notification_handler"))?;
        let version = api_fn!(raw, get_version)
            // SAFETY: get_version 无参数，返回库内的静态字符串（可能为空指针）。
            .and_then(|f| unsafe { opt_string(f()) })
            .unwrap_or_default();

        let shared = path_cstring(&config.shared_data_dir)?;
        let user = path_cstring(&config.user_data_dir)?;
        let dist_version = CString::new(env!("CARGO_PKG_VERSION")).expect("版本号不含 NUL");
        let mut traits = RimeTraits::new();
        traits.shared_data_dir = shared.as_ptr();
        traits.user_data_dir = user.as_ptr();
        traits.distribution_name = c"Loong OS".as_ptr();
        traits.distribution_code_name = c"loong".as_ptr();
        traits.distribution_version = dist_version.as_ptr();
        // glog 保存 app_name 指针，必须是 'static。
        traits.app_name = c"rime.loong".as_ptr();
        traits.min_log_level = config.log_level as c_int;
        // 空字符串：日志只写 stderr，不在 /tmp 里留日志文件。
        traits.log_dir = c"".as_ptr();

        // SAFETY: `traits` 的 data_size 已按 RIME_STRUCT_INIT 初始化；其中的字符串指针在
        // 这几个调用期间有效（librime 会拷贝目录路径；app_name 是 'static）。
        // 通知回调只访问原子变量，context 不使用，传空指针。
        unsafe {
            if !global.setup_done {
                setup(&mut traits);
                global.setup_done = true;
            }
            set_notification_handler(Some(on_notification), std::ptr::null_mut());
            (api.initialize)(&mut traits);
        }

        let rime = Rime {
            api,
            lock: Mutex::new(Inner { live_sessions: 0 }),
            shared_data_dir: config.shared_data_dir.clone(),
            user_data_dir: config.user_data_dir.clone(),
            schema: config.schema.clone(),
            version,
        };
        if let Err(e) = rime.deploy_and_verify(false) {
            // SAFETY: initialize 之后、没有存活会话时调用 finalize 是允许的；之后可以重新 start。
            unsafe { (rime.api.finalize)() };
            return Err(e);
        }
        let rime: &'static Rime = Box::leak(Box::new(rime));
        global.running = Some(rime);
        Ok(rime)
    }

    /// 已启动的实例（没有启动时为 `None`）。
    pub fn running() -> Option<&'static Rime> {
        GLOBAL
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running
    }

    /// 部署（只在检测到数据变化时真正编译），然后确认方案可用。
    fn deploy_and_verify(&self, full_check: bool) -> Result<(), ImeError> {
        {
            let guard = self.guard();
            if guard.live_sessions > 0 {
                return Err(ImeError::SessionsAlive(guard.live_sessions));
            }
            DEPLOY_STATE.store(DEPLOY_NONE, Ordering::SeqCst);
            // SAFETY: 已 initialize，且没有存活的会话（部署会让会话持有的词典失效）；
            // 持锁直到部署线程结束，期间不会有别的调用或新会话。
            unsafe {
                (self.api.start_maintenance)(Bool::from(full_check));
                (self.api.join_maintenance_thread)();
            }
        }
        if DEPLOY_STATE.load(Ordering::SeqCst) == DEPLOY_FAILURE {
            return Err(ImeError::DeployFailed {
                user_data_dir: self.user_data_dir.clone(),
            });
        }
        let actual = self.probe_schema()?;
        if actual != self.schema {
            return Err(ImeError::SchemaNotLoaded {
                expected: self.schema.clone(),
                actual,
            });
        }
        Ok(())
    }

    /// 用一个临时会话读出实际加载的方案 ID。
    fn probe_schema(&self) -> Result<String, ImeError> {
        let _guard = self.guard();
        let mut buf = [0 as c_char; 128];
        // SAFETY: 已 initialize 且部署已结束；临时会话创建后立即销毁；缓冲区长度如实传入，
        // librime 写入以 NUL 结尾的字符串（必要时截断）。
        unsafe {
            let id = (self.api.create_session)();
            if id == 0 {
                return Err(ImeError::SessionCreate);
            }
            let ok = (self.api.get_current_schema)(id, buf.as_mut_ptr(), buf.len()) != 0;
            (self.api.destroy_session)(id);
            buf[buf.len() - 1] = 0;
            Ok(if ok {
                opt_string(buf.as_ptr()).unwrap_or_default()
            } else {
                String::new()
            })
        }
    }

    /// 强制完整检查并重新部署（例如用户在设置里改了词库）。会阻塞到部署结束。
    ///
    /// librime 部署时会替换词典等资源，已有会话继续使用会读到失效的内存映射
    /// （实测会段错误）。因此必须先销毁所有会话（丢掉 [`crate::ImeEngine`]），否则返回
    /// [`ImeError::SessionsAlive`]；部署完成后再新建会话。
    pub fn redeploy(&self) -> Result<(), ImeError> {
        self.deploy_and_verify(true)
    }

    /// librime 版本，例如 `1.17.0`。
    pub fn version(&self) -> &str {
        &self.version
    }

    /// 共享数据目录。
    pub fn shared_data_dir(&self) -> &Path {
        &self.shared_data_dir
    }

    /// 用户数据目录。
    pub fn user_data_dir(&self) -> &Path {
        &self.user_data_dir
    }

    /// 方案 ID。
    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// 新建输入会话。
    pub fn create_session(&'static self) -> Result<Session, ImeError> {
        let mut guard = self.guard();
        // SAFETY: 已 initialize 且不在部署中（部署全程持锁）。
        let id = unsafe { (self.api.create_session)() };
        if id == 0 {
            return Err(ImeError::SessionCreate);
        }
        guard.live_sessions += 1;
        Ok(Session { rime: self, id })
    }

    /// 存活的会话数。
    pub fn live_sessions(&self) -> usize {
        self.guard().live_sessions
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 一个输入会话。所有方法都会短暂持有 [`Rime`] 的全局锁。
pub struct Session {
    rime: &'static Rime,
    id: RimeSessionId,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("id", &self.id).finish()
    }
}

impl Session {
    fn api(&self) -> (&Api, std::sync::MutexGuard<'_, Inner>) {
        (&self.rime.api, self.rime.guard())
    }

    /// 按键（X11 keysym + 修饰键掩码，见 [`keysym`]）。返回 librime 是否处理了这个键；
    /// 没处理的键应由调用方按普通按键交给应用。
    pub fn process_key(&self, keysym: u32, mask: u32) -> bool {
        let (api, _g) = self.api();
        // keysym 与掩码都不超过 i32 范围（掩码最高位是 1<<30）。
        // SAFETY: 会话有效（Session 存活即未销毁），参数为普通整数。
        unsafe { (api.process_key)(self.id, keysym as c_int, mask as c_int) != 0 }
    }

    /// 把当前组字内容（按已选/高亮的候选转换后）上屏。返回是否有内容上屏。
    pub fn commit_composition(&self) -> bool {
        let (api, _g) = self.api();
        // SAFETY: 会话有效。
        unsafe { (api.commit_composition)(self.id) != 0 }
    }

    /// 丢弃当前组字内容。
    pub fn clear_composition(&self) {
        let (api, _g) = self.api();
        // SAFETY: 会话有效。
        unsafe { (api.clear_composition)(self.id) }
    }

    /// 取出待上屏文字（取出后清空）。
    pub fn take_commit(&self) -> Option<String> {
        let (api, _g) = self.api();
        let mut commit = RimeCommit::new();
        // SAFETY: commit 已初始化 data_size；get_commit 成功后由 free_commit 释放其中的字符串，
        // 释放前把文字拷贝出来。
        unsafe {
            if (api.get_commit)(self.id, &mut commit) == 0 {
                return None;
            }
            let text = opt_string(commit.text);
            (api.free_commit)(&mut commit);
            text.filter(|t| !t.is_empty())
        }
    }

    /// 当前组字状态。
    pub fn context(&self) -> Context {
        let (api, _g) = self.api();
        let mut ctx = RimeContext::new();
        // SAFETY: ctx 已初始化 data_size；get_context 成功后，其中的指针在 free_context 之前
        // 有效：candidates 指向 num_candidates 个元素的数组。全部拷贝后再释放。
        unsafe {
            if (api.get_context)(self.id, &mut ctx) == 0 {
                return Context::default();
            }
            let comp = &ctx.composition;
            let preedit = opt_string(comp.preedit).map(|text| Preedit {
                text,
                cursor: to_usize(comp.cursor_pos),
                sel_start: to_usize(comp.sel_start),
                sel_end: to_usize(comp.sel_end),
            });
            let m = &ctx.menu;
            let count = to_usize(m.num_candidates);
            let candidates = if m.candidates.is_null() || count == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(m.candidates, count)
                    .iter()
                    .map(|c| Candidate {
                        text: opt_string(c.text).unwrap_or_default(),
                        comment: opt_string(c.comment).filter(|s| !s.is_empty()),
                    })
                    .collect()
            };
            let menu = Menu {
                page_size: to_usize(m.page_size),
                page_no: to_usize(m.page_no),
                is_last_page: m.is_last_page != 0,
                highlighted: to_usize(m.highlighted_candidate_index),
                candidates,
            };
            let commit_preview = if ctx.has_commit_text_preview() {
                opt_string(ctx.commit_text_preview)
            } else {
                None
            };
            (api.free_context)(&mut ctx);
            Context {
                preedit: preedit.filter(|p| !p.text.is_empty()),
                menu,
                commit_preview,
            }
        }
    }

    /// 方案与开关状态。
    pub fn status(&self) -> Status {
        let (api, _g) = self.api();
        let mut st = RimeStatus::new();
        // SAFETY: st 已初始化 data_size；字符串在 free_status 之前有效，先拷贝再释放。
        unsafe {
            if (api.get_status)(self.id, &mut st) == 0 {
                return Status::default();
            }
            let status = Status {
                schema_id: opt_string(st.schema_id).unwrap_or_default(),
                schema_name: opt_string(st.schema_name).unwrap_or_default(),
                is_composing: st.is_composing != 0,
                is_ascii_mode: st.is_ascii_mode != 0,
                is_full_shape: st.is_full_shape != 0,
                is_ascii_punct: st.is_ascii_punct != 0,
                is_disabled: st.is_disabled != 0,
            };
            (api.free_status)(&mut st);
            status
        }
    }

    /// 设置开关（例如 `ascii_mode`、`zh_simp`、`full_shape`）。
    pub fn set_option(&self, option: &str, value: bool) {
        let Ok(name) = CString::new(option) else {
            return;
        };
        let (api, _g) = self.api();
        // SAFETY: 会话有效；name 在调用期间有效。
        unsafe { (api.set_option)(self.id, name.as_ptr(), Bool::from(value)) }
    }

    /// 读取开关。
    pub fn option(&self, option: &str) -> bool {
        let Ok(name) = CString::new(option) else {
            return false;
        };
        let (api, _g) = self.api();
        // SAFETY: 会话有效；name 在调用期间有效。
        unsafe { (api.get_option)(self.id, name.as_ptr()) != 0 }
    }

    /// 当前方案 ID。
    pub fn schema_id(&self) -> Option<String> {
        let (api, _g) = self.api();
        let mut buf = [0 as c_char; 128];
        // SAFETY: 会话有效；缓冲区长度如实传入，librime 写入以 NUL 结尾的字符串（必要时截断）。
        unsafe {
            if (api.get_current_schema)(self.id, buf.as_mut_ptr(), buf.len()) == 0 {
                return None;
            }
            buf[buf.len() - 1] = 0;
            opt_string(buf.as_ptr())
        }
    }

    /// 会话 ID（调试用）。
    pub fn id(&self) -> usize {
        self.id
    }

    /// 原始输入（例如 `nihao`）。
    pub fn input(&self) -> String {
        let (api, _g) = self.api();
        // SAFETY: 返回的指针在下一次编辑前有效；持锁期间立即拷贝。
        unsafe { opt_string((api.get_input)(self.id)) }.unwrap_or_default()
    }

    /// 选择整个候选列表中的第 `index` 个候选。
    pub fn select_candidate(&self, index: usize) -> bool {
        let (api, _g) = self.api();
        // SAFETY: 会话有效；越界下标由 librime 检查并返回 False。
        unsafe { (api.select_candidate)(self.id, index) != 0 }
    }

    /// 选择当前页的第 `index` 个候选。
    pub fn select_candidate_on_current_page(&self, index: usize) -> bool {
        if let Some(f) = self.rime.api.select_candidate_on_current_page {
            let _g = self.rime.guard();
            // SAFETY: 会话有效；越界下标由 librime 检查并返回 False。
            return unsafe { f(self.id, index) != 0 };
        }
        let menu = self.context().menu;
        if index >= menu.candidates.len() {
            return false;
        }
        self.select_candidate(menu.page_no * menu.page_size + index)
    }

    /// 翻页：`backward` 为 `true` 时向前翻。返回是否翻页成功。
    pub fn change_page(&self, backward: bool) -> bool {
        if let Some(f) = self.rime.api.change_page {
            let _g = self.rime.guard();
            // SAFETY: 会话有效。
            return unsafe { f(self.id, Bool::from(backward)) != 0 };
        }
        let key = if backward {
            keysym::PAGE_UP
        } else {
            keysym::PAGE_DOWN
        };
        self.process_key(key, 0)
    }

    /// 完整候选列表中从 `start` 开始的最多 `max` 个候选（不受分页限制），
    /// 用于展开的候选面板。
    pub fn candidates(&self, start: usize, max: usize) -> Vec<Candidate> {
        let (api, _g) = self.api();
        let mut out = Vec::new();
        let mut it = RimeCandidateListIterator::new();
        // SAFETY: 迭代器由 begin/from_index 初始化、由 end 释放；每个候选的字符串在下一次
        // next 之前有效，立即拷贝。
        unsafe {
            let started = match (api.candidate_list_from_index, c_int::try_from(start)) {
                (Some(from), Ok(s)) => from(self.id, &mut it, s) != 0,
                _ => (api.candidate_list_begin)(self.id, &mut it) != 0,
            };
            if !started {
                return out;
            }
            let mut skip = if api.candidate_list_from_index.is_some() {
                0
            } else {
                start
            };
            while out.len() < max && (api.candidate_list_next)(&mut it) != 0 {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                out.push(Candidate {
                    text: opt_string(it.candidate.text).unwrap_or_default(),
                    comment: opt_string(it.candidate.comment).filter(|s| !s.is_empty()),
                });
            }
            (api.candidate_list_end)(&mut it);
        }
        out
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let (api, mut guard) = self.api();
        // SAFETY: 会话由 create_session 创建，只销毁一次。
        unsafe {
            (api.destroy_session)(self.id);
        }
        guard.live_sessions -= 1;
    }
}
