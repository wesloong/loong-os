//! librime C API 的手写绑定（对照 librime 1.17 的 `/usr/include/rime_api.h`）。
//!
//! librime 只导出一个入口 `rime_get_api()`，它返回一张函数表 [`RimeApi`]。
//! 这里按头文件**逐字段、同顺序**声明结构体（`#[repr(C)]`）：
//!
//! - 用到的函数写出完整签名；用不到的函数统一声明为 [`Unused`]（同样是一个可空的
//!   函数指针，大小与对齐和真正的签名相同），只占位，保证后续字段的偏移正确；
//! - 函数表与 `RimeContext` 等结构都是“自带版本”的：首个字段 `data_size` 等于
//!   `sizeof(结构体) - sizeof(int)`。调用前用 [`RimeApi::provides`] 检查库里的表
//!   是否足够长，旧版库缺少的函数不会被误调用；
//! - 单元测试 `layout_matches_c_header` 会用系统 C 编译器编译头文件，逐个比对
//!   结构体大小与字段偏移。
//!
//! 本模块只有类型声明，不做任何调用；安全封装见上级模块。

#![allow(dead_code)]

use std::ffi::{c_char, c_int, c_void};
use std::mem::{offset_of, size_of};
use std::ptr;

/// C 头文件中的 `Bool`（`#define Bool int`）。
pub type Bool = c_int;
/// 会话 ID（`uintptr_t`）。
pub type RimeSessionId = usize;
/// 占位用的函数指针：本项目不调用的接口。
pub type Unused = Option<unsafe extern "C" fn()>;
/// 通知回调：`(context_object, session_id, message_type, message_value)`。
pub type RimeNotificationHandler =
    Option<unsafe extern "C" fn(*mut c_void, RimeSessionId, *const c_char, *const c_char)>;

/// 自带版本的结构体，`data_size = sizeof(T) - sizeof(int)`（即 `RIME_STRUCT_INIT`）。
const fn data_size_of<T>() -> c_int {
    (size_of::<T>() - size_of::<c_int>()) as c_int
}

/// `RimeTraits`：数据目录、应用名、日志等启动参数。
#[repr(C)]
#[derive(Debug)]
pub struct RimeTraits {
    pub data_size: c_int,
    // v0.9
    pub shared_data_dir: *const c_char,
    pub user_data_dir: *const c_char,
    pub distribution_name: *const c_char,
    pub distribution_code_name: *const c_char,
    pub distribution_version: *const c_char,
    // v1.0
    /// glog 会**保存这个指针**（`InitGoogleLogging`），必须指向 `'static` 字符串。
    pub app_name: *const c_char,
    pub modules: *mut *const c_char,
    // v1.6
    pub min_log_level: c_int,
    pub log_dir: *const c_char,
    pub prebuilt_data_dir: *const c_char,
    pub staging_dir: *const c_char,
}

impl RimeTraits {
    /// 已初始化 `data_size`、其余字段为空的结构。
    pub fn new() -> Self {
        RimeTraits {
            data_size: data_size_of::<Self>(),
            shared_data_dir: ptr::null(),
            user_data_dir: ptr::null(),
            distribution_name: ptr::null(),
            distribution_code_name: ptr::null(),
            distribution_version: ptr::null(),
            app_name: ptr::null(),
            modules: ptr::null_mut(),
            min_log_level: 0,
            log_dir: ptr::null(),
            prebuilt_data_dir: ptr::null(),
            staging_dir: ptr::null(),
        }
    }
}

/// `RimeComposition`：预编辑文字与光标（位置以 UTF-8 字节计）。
#[repr(C)]
#[derive(Debug)]
pub struct RimeComposition {
    pub length: c_int,
    pub cursor_pos: c_int,
    pub sel_start: c_int,
    pub sel_end: c_int,
    pub preedit: *mut c_char,
}

/// `RimeCandidate`：候选文字与注释。
#[repr(C)]
#[derive(Debug)]
pub struct RimeCandidate {
    pub text: *mut c_char,
    pub comment: *mut c_char,
    pub reserved: *mut c_void,
}

impl RimeCandidate {
    const fn null() -> Self {
        RimeCandidate {
            text: ptr::null_mut(),
            comment: ptr::null_mut(),
            reserved: ptr::null_mut(),
        }
    }
}

/// `RimeMenu`：当前一页候选。
#[repr(C)]
#[derive(Debug)]
pub struct RimeMenu {
    pub page_size: c_int,
    pub page_no: c_int,
    pub is_last_page: Bool,
    pub highlighted_candidate_index: c_int,
    pub num_candidates: c_int,
    pub candidates: *mut RimeCandidate,
    pub select_keys: *mut c_char,
}

/// `RimeCommit`：待上屏文字。
#[repr(C)]
#[derive(Debug)]
pub struct RimeCommit {
    pub data_size: c_int,
    pub text: *mut c_char,
}

impl RimeCommit {
    pub fn new() -> Self {
        RimeCommit {
            data_size: data_size_of::<Self>(),
            text: ptr::null_mut(),
        }
    }
}

/// `RimeContext`：组字状态（预编辑 + 候选菜单）。
#[repr(C)]
#[derive(Debug)]
pub struct RimeContext {
    pub data_size: c_int,
    // v0.9
    pub composition: RimeComposition,
    pub menu: RimeMenu,
    // v0.9.2
    pub commit_text_preview: *mut c_char,
    pub select_labels: *mut *mut c_char,
}

impl RimeContext {
    pub fn new() -> Self {
        RimeContext {
            data_size: data_size_of::<Self>(),
            composition: RimeComposition {
                length: 0,
                cursor_pos: 0,
                sel_start: 0,
                sel_end: 0,
                preedit: ptr::null_mut(),
            },
            menu: RimeMenu {
                page_size: 0,
                page_no: 0,
                is_last_page: 0,
                highlighted_candidate_index: 0,
                num_candidates: 0,
                candidates: ptr::null_mut(),
                select_keys: ptr::null_mut(),
            },
            commit_text_preview: ptr::null_mut(),
            select_labels: ptr::null_mut(),
        }
    }

    /// 库返回的结构里是否包含 `commit_text_preview`（v0.9.2 起）。
    pub fn has_commit_text_preview(&self) -> bool {
        provides(
            self.data_size,
            offset_of!(RimeContext, commit_text_preview),
            size_of::<*mut c_char>(),
        )
    }
}

/// `RimeStatus`：方案与开关状态。
#[repr(C)]
#[derive(Debug)]
pub struct RimeStatus {
    pub data_size: c_int,
    // v0.9
    pub schema_id: *mut c_char,
    pub schema_name: *mut c_char,
    pub is_disabled: Bool,
    pub is_composing: Bool,
    pub is_ascii_mode: Bool,
    pub is_full_shape: Bool,
    pub is_simplified: Bool,
    pub is_traditional: Bool,
    pub is_ascii_punct: Bool,
}

impl RimeStatus {
    pub fn new() -> Self {
        RimeStatus {
            data_size: data_size_of::<Self>(),
            schema_id: ptr::null_mut(),
            schema_name: ptr::null_mut(),
            is_disabled: 0,
            is_composing: 0,
            is_ascii_mode: 0,
            is_full_shape: 0,
            is_simplified: 0,
            is_traditional: 0,
            is_ascii_punct: 0,
        }
    }
}

/// `RimeCandidateListIterator`：遍历完整候选列表（不受分页限制）。
#[repr(C)]
#[derive(Debug)]
pub struct RimeCandidateListIterator {
    pub ptr: *mut c_void,
    pub index: c_int,
    pub candidate: RimeCandidate,
}

impl RimeCandidateListIterator {
    pub fn new() -> Self {
        RimeCandidateListIterator {
            ptr: ptr::null_mut(),
            index: 0,
            candidate: RimeCandidate::null(),
        }
    }
}

/// `RimeApi` 函数表。字段顺序必须与 `rime_api.h` 完全一致。
#[repr(C)]
pub struct RimeApi {
    pub data_size: c_int,

    pub setup: Option<unsafe extern "C" fn(traits: *mut RimeTraits)>,
    pub set_notification_handler:
        Option<unsafe extern "C" fn(handler: RimeNotificationHandler, context: *mut c_void)>,

    // entry and exit
    pub initialize: Option<unsafe extern "C" fn(traits: *mut RimeTraits)>,
    pub finalize: Option<unsafe extern "C" fn()>,

    pub start_maintenance: Option<unsafe extern "C" fn(full_check: Bool) -> Bool>,
    pub is_maintenance_mode: Option<unsafe extern "C" fn() -> Bool>,
    pub join_maintenance_thread: Option<unsafe extern "C" fn()>,

    // deployment
    pub deployer_initialize: Unused,
    pub prebuild: Unused,
    pub deploy: Unused,
    pub deploy_schema: Unused,
    pub deploy_config_file: Unused,

    pub sync_user_data: Unused,

    // session management
    pub create_session: Option<unsafe extern "C" fn() -> RimeSessionId>,
    pub find_session: Option<unsafe extern "C" fn(session_id: RimeSessionId) -> Bool>,
    pub destroy_session: Option<unsafe extern "C" fn(session_id: RimeSessionId) -> Bool>,
    pub cleanup_stale_sessions: Unused,
    pub cleanup_all_sessions: Unused,

    // input
    pub process_key: Option<
        unsafe extern "C" fn(session_id: RimeSessionId, keycode: c_int, mask: c_int) -> Bool,
    >,
    pub commit_composition: Option<unsafe extern "C" fn(session_id: RimeSessionId) -> Bool>,
    pub clear_composition: Option<unsafe extern "C" fn(session_id: RimeSessionId)>,

    // output
    pub get_commit:
        Option<unsafe extern "C" fn(session_id: RimeSessionId, commit: *mut RimeCommit) -> Bool>,
    pub free_commit: Option<unsafe extern "C" fn(commit: *mut RimeCommit) -> Bool>,
    pub get_context:
        Option<unsafe extern "C" fn(session_id: RimeSessionId, context: *mut RimeContext) -> Bool>,
    pub free_context: Option<unsafe extern "C" fn(context: *mut RimeContext) -> Bool>,
    pub get_status:
        Option<unsafe extern "C" fn(session_id: RimeSessionId, status: *mut RimeStatus) -> Bool>,
    pub free_status: Option<unsafe extern "C" fn(status: *mut RimeStatus) -> Bool>,

    // runtime options
    pub set_option:
        Option<unsafe extern "C" fn(session_id: RimeSessionId, option: *const c_char, value: Bool)>,
    pub get_option:
        Option<unsafe extern "C" fn(session_id: RimeSessionId, option: *const c_char) -> Bool>,

    pub set_property: Unused,
    pub get_property: Unused,

    pub get_schema_list: Unused,
    pub free_schema_list: Unused,

    pub get_current_schema: Option<
        unsafe extern "C" fn(
            session_id: RimeSessionId,
            schema_id: *mut c_char,
            buffer_size: usize,
        ) -> Bool,
    >,
    pub select_schema:
        Option<unsafe extern "C" fn(session_id: RimeSessionId, schema_id: *const c_char) -> Bool>,

    // configuration
    pub schema_open: Unused,
    pub config_open: Unused,
    pub config_close: Unused,
    pub config_get_bool: Unused,
    pub config_get_int: Unused,
    pub config_get_double: Unused,
    pub config_get_string: Unused,
    pub config_get_cstring: Unused,
    pub config_update_signature: Unused,
    pub config_begin_map: Unused,
    pub config_next: Unused,
    pub config_end: Unused,

    // testing
    pub simulate_key_sequence: Unused,

    // module
    pub register_module: Unused,
    pub find_module: Unused,

    pub run_task: Unused,

    pub get_shared_data_dir: Unused,
    pub get_user_data_dir: Unused,
    pub get_sync_dir: Unused,

    pub get_user_id: Unused,
    pub get_user_data_sync_dir: Unused,

    pub config_init: Unused,
    pub config_load_string: Unused,

    pub config_set_bool: Unused,
    pub config_set_int: Unused,
    pub config_set_double: Unused,
    pub config_set_string: Unused,

    pub config_get_item: Unused,
    pub config_set_item: Unused,
    pub config_clear: Unused,
    pub config_create_list: Unused,
    pub config_create_map: Unused,
    pub config_list_size: Unused,
    pub config_begin_list: Unused,

    pub get_input: Option<unsafe extern "C" fn(session_id: RimeSessionId) -> *const c_char>,
    pub get_caret_pos: Unused,
    pub select_candidate:
        Option<unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> Bool>,
    pub get_version: Option<unsafe extern "C" fn() -> *const c_char>,
    pub set_caret_pos: Unused,
    pub select_candidate_on_current_page:
        Option<unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> Bool>,

    pub candidate_list_begin: Option<
        unsafe extern "C" fn(
            session_id: RimeSessionId,
            iterator: *mut RimeCandidateListIterator,
        ) -> Bool,
    >,
    pub candidate_list_next:
        Option<unsafe extern "C" fn(iterator: *mut RimeCandidateListIterator) -> Bool>,
    pub candidate_list_end: Option<unsafe extern "C" fn(iterator: *mut RimeCandidateListIterator)>,

    pub user_config_open: Unused,

    pub candidate_list_from_index: Option<
        unsafe extern "C" fn(
            session_id: RimeSessionId,
            iterator: *mut RimeCandidateListIterator,
            index: c_int,
        ) -> Bool,
    >,

    pub get_prebuilt_data_dir: Unused,
    pub get_staging_dir: Unused,

    pub commit_proto: Unused,
    pub context_proto: Unused,
    pub status_proto: Unused,

    pub get_state_label: Unused,

    pub delete_candidate: Unused,
    pub delete_candidate_on_current_page: Unused,

    pub get_state_label_abbreviated: Unused,

    pub set_input: Unused,

    pub get_shared_data_dir_s: Unused,
    pub get_user_data_dir_s: Unused,
    pub get_prebuilt_data_dir_s: Unused,
    pub get_staging_dir_s: Unused,
    pub get_sync_dir_s: Unused,

    pub highlight_candidate:
        Option<unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> Bool>,
    pub highlight_candidate_on_current_page:
        Option<unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> Bool>,

    pub change_page:
        Option<unsafe extern "C" fn(session_id: RimeSessionId, backward: Bool) -> Bool>,
}

impl RimeApi {
    /// 库里的函数表是否包含从 `offset` 开始的函数指针（即 `RIME_STRUCT_HAS_MEMBER`）。
    pub fn provides(&self, offset: usize) -> bool {
        provides(self.data_size, offset, size_of::<Unused>())
    }
}

/// `RIME_STRUCT_HAS_MEMBER`：`sizeof(data_size) + data_size` 是否覆盖了该字段。
fn provides(data_size: c_int, offset: usize, field_size: usize) -> bool {
    usize::try_from(data_size)
        .map(|n| size_of::<c_int>() + n >= offset + field_size)
        .unwrap_or(false)
}

/// 取出函数表中的函数指针；库版本太旧（表不够长）或指针为空时返回 `None`。
macro_rules! api_fn {
    ($api:expr, $field:ident) => {{
        let api: &$crate::rime::ffi::RimeApi = $api;
        if api.provides(::std::mem::offset_of!($crate::rime::ffi::RimeApi, $field)) {
            api.$field
        } else {
            None
        }
    }};
}
pub(crate) use api_fn;

unsafe extern "C" {
    /// 取得 librime 的函数表。返回指向库内静态数据的指针，进程内一直有效。
    pub fn rime_get_api() -> *mut RimeApi;
}

/// librime 用到的 X11 keysym 与修饰键掩码（与 `xkbcommon-keysyms.h`、
/// `rime/key_table.h` 一致）。可打印 ASCII 字符的 keysym 就是其码值。
#[allow(missing_docs)]
pub mod keysym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const TAB: u32 = 0xff09;
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const HOME: u32 = 0xff50;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const PAGE_UP: u32 = 0xff55;
    pub const PAGE_DOWN: u32 = 0xff56;
    pub const END: u32 = 0xff57;
    pub const DELETE: u32 = 0xffff;
    pub const SPACE: u32 = 0x0020;

    pub const SHIFT_MASK: u32 = 1 << 0;
    pub const LOCK_MASK: u32 = 1 << 1;
    pub const CONTROL_MASK: u32 = 1 << 2;
    pub const ALT_MASK: u32 = 1 << 3;
    pub const RELEASE_MASK: u32 = 1 << 30;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::process::Command;

    /// 用 C 编译器编译 `rime_api.h`，比对结构体大小与字段偏移。
    ///
    /// 没有 C 编译器或头文件时跳过（打印原因），不算失败。
    #[test]
    fn layout_matches_c_header() {
        // (C 表达式, Rust 侧的值)
        let mut checks: Vec<(String, usize)> = vec![
            ("sizeof(RimeTraits)".into(), size_of::<RimeTraits>()),
            (
                "offsetof(RimeTraits, staging_dir)".into(),
                offset_of!(RimeTraits, staging_dir),
            ),
            (
                "offsetof(RimeTraits, min_log_level)".into(),
                offset_of!(RimeTraits, min_log_level),
            ),
            (
                "sizeof(RimeComposition)".into(),
                size_of::<RimeComposition>(),
            ),
            ("sizeof(RimeCandidate)".into(), size_of::<RimeCandidate>()),
            ("sizeof(RimeMenu)".into(), size_of::<RimeMenu>()),
            (
                "offsetof(RimeMenu, candidates)".into(),
                offset_of!(RimeMenu, candidates),
            ),
            ("sizeof(RimeCommit)".into(), size_of::<RimeCommit>()),
            ("sizeof(RimeContext)".into(), size_of::<RimeContext>()),
            (
                "offsetof(RimeContext, menu)".into(),
                offset_of!(RimeContext, menu),
            ),
            (
                "offsetof(RimeContext, select_labels)".into(),
                offset_of!(RimeContext, select_labels),
            ),
            ("sizeof(RimeStatus)".into(), size_of::<RimeStatus>()),
            (
                "offsetof(RimeStatus, is_ascii_punct)".into(),
                offset_of!(RimeStatus, is_ascii_punct),
            ),
            (
                "sizeof(RimeCandidateListIterator)".into(),
                size_of::<RimeCandidateListIterator>(),
            ),
            ("sizeof(RimeApi)".into(), size_of::<RimeApi>()),
        ];
        macro_rules! api_offsets {
            ($($f:ident),* $(,)?) => {
                $(checks.push((
                    format!("offsetof(RimeApi, {})", stringify!($f)),
                    offset_of!(RimeApi, $f),
                ));)*
            };
        }
        api_offsets!(
            setup,
            set_notification_handler,
            initialize,
            start_maintenance,
            join_maintenance_thread,
            create_session,
            destroy_session,
            process_key,
            commit_composition,
            clear_composition,
            get_commit,
            free_commit,
            get_context,
            free_context,
            get_status,
            free_status,
            set_option,
            get_option,
            get_current_schema,
            select_schema,
            get_input,
            select_candidate,
            get_version,
            select_candidate_on_current_page,
            candidate_list_begin,
            candidate_list_next,
            candidate_list_end,
            candidate_list_from_index,
            highlight_candidate_on_current_page,
            change_page,
        );

        let mut src = String::from(
            "#include <stdio.h>\n#include <stddef.h>\n#include <rime_api.h>\nint main(void) {\n",
        );
        for (expr, _) in &checks {
            writeln!(src, "  printf(\"%zu\\n\", (size_t)({expr}));").unwrap();
        }
        src.push_str("  return 0;\n}\n");

        let dir = std::env::temp_dir().join(format!("loong-ime-abi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c_file = dir.join("abi.c");
        let exe = dir.join("abi");
        std::fs::write(&c_file, src).unwrap();
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
        let compiled = Command::new(&cc).arg(&c_file).arg("-o").arg(&exe).output();
        let ok = matches!(&compiled, Ok(o) if o.status.success());
        if !ok {
            eprintln!(
                "跳过 ABI 比对：无法用 `{cc}` 编译 rime_api.h（{:?}）",
                compiled.map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
            );
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let out = Command::new(&exe).output().expect("运行 ABI 探测程序");
        let _ = std::fs::remove_dir_all(&dir);
        let values: Vec<usize> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|l| l.trim().parse().unwrap())
            .collect();
        assert_eq!(values.len(), checks.len());
        for ((expr, rust), c) in checks.iter().zip(values) {
            assert_eq!(*rust, c, "{expr}: Rust = {rust}, C = {c}");
        }
    }

    #[test]
    fn data_size_follows_rime_struct_init() {
        assert_eq!(
            RimeContext::new().data_size as usize,
            size_of::<RimeContext>() - size_of::<c_int>()
        );
        assert!(RimeContext::new().has_commit_text_preview());
        assert!(!provides(4, 8, 8));
        assert!(provides(12, 8, 8));
        assert!(!provides(-1, 0, 8));
    }
}
