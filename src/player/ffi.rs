//! Minimal libmpv bindings, loaded at runtime so the app starts without libmpv installed.
#![allow(non_camel_case_types)]
use std::ffi::{CStr, CString, c_char, c_double, c_int, c_ulong, c_void};
use std::path::{Path, PathBuf};

pub enum mpv_handle {}
pub enum mpv_render_context {}

pub const MPV_FORMAT_STRING: c_int = 1;
pub const MPV_FORMAT_FLAG: c_int = 3;
pub const MPV_FORMAT_INT64: c_int = 4;
pub const MPV_FORMAT_DOUBLE: c_int = 5;

pub const MPV_EVENT_NONE: c_int = 0;
pub const MPV_EVENT_SHUTDOWN: c_int = 1;
pub const MPV_EVENT_FILE_LOADED: c_int = 8;

pub const MPV_RENDER_PARAM_INVALID: c_int = 0;
pub const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
pub const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
pub const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;
pub const MPV_RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
pub struct mpv_event {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_render_param {
    pub type_: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_opengl_init_params {
    pub get_proc_address:
        Option<unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void>,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
pub struct mpv_opengl_fbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

type Callback = unsafe extern "C" fn(*mut c_void);

/// Function table resolved from the library.
pub struct Mpv {
    _lib: libloading::Library,
    pub path: PathBuf,
    pub client_api_version: unsafe extern "C" fn() -> c_ulong,
    pub create: unsafe extern "C" fn() -> *mut mpv_handle,
    pub initialize: unsafe extern "C" fn(*mut mpv_handle) -> c_int,
    pub terminate_destroy: unsafe extern "C" fn(*mut mpv_handle),
    pub set_option_string:
        unsafe extern "C" fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int,
    pub command: unsafe extern "C" fn(*mut mpv_handle, *mut *const c_char) -> c_int,
    pub set_property_string:
        unsafe extern "C" fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int,
    pub get_property_string: unsafe extern "C" fn(*mut mpv_handle, *const c_char) -> *mut c_char,
    pub get_property:
        unsafe extern "C" fn(*mut mpv_handle, *const c_char, c_int, *mut c_void) -> c_int,
    pub set_property:
        unsafe extern "C" fn(*mut mpv_handle, *const c_char, c_int, *mut c_void) -> c_int,
    pub free: unsafe extern "C" fn(*mut c_void),
    pub wait_event: unsafe extern "C" fn(*mut mpv_handle, c_double) -> *mut mpv_event,
    pub set_wakeup_callback: unsafe extern "C" fn(*mut mpv_handle, Option<Callback>, *mut c_void),
    pub error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    pub render_context_create: unsafe extern "C" fn(
        *mut *mut mpv_render_context,
        *mut mpv_handle,
        *mut mpv_render_param,
    ) -> c_int,
    pub render_context_set_update_callback:
        unsafe extern "C" fn(*mut mpv_render_context, Option<Callback>, *mut c_void),
    pub render_context_update: unsafe extern "C" fn(*mut mpv_render_context) -> u64,
    pub render_context_render:
        unsafe extern "C" fn(*mut mpv_render_context, *mut mpv_render_param) -> c_int,
    pub render_context_free: unsafe extern "C" fn(*mut mpv_render_context),
}

/// Library names to try, most specific first. `BEARDL_LIBMPV` overrides them all.
fn candidates() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("BEARDL_LIBMPV") {
        v.push(PathBuf::from(p));
    }
    let names: &[&str] = if cfg!(windows) {
        &["libmpv-2.dll", "mpv-2.dll", "mpv-1.dll"]
    } else if cfg!(target_os = "macos") {
        &["libmpv.2.dylib", "libmpv.dylib"]
    } else {
        &["libmpv.so.2", "libmpv.so.1", "libmpv.so"]
    };
    // Next to the app and in the tools folder first, then the system search path.
    let mut dirs = Vec::new();
    if let Some(d) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        dirs.push(d);
    }
    dirs.push(crate::paths::tools_dir().join("mpv"));
    if cfg!(target_os = "macos") {
        for d in ["/opt/homebrew/lib", "/usr/local/lib", "/opt/local/lib"] {
            dirs.push(PathBuf::from(d));
        }
    }
    for d in &dirs {
        for n in names {
            v.push(d.join(n));
        }
    }
    for n in names {
        v.push(PathBuf::from(n));
    }
    v
}

macro_rules! sym {
    ($lib:expr, $name:literal) => {
        // SAFETY: the signature matches libmpv's client.h / render.h for this symbol.
        *unsafe { $lib.get(concat!($name, "\0").as_bytes()) }
            .map_err(|e| format!("{}: {e}", $name))?
    };
}

impl Mpv {
    /// Loads libmpv, or explains why it could not.
    pub fn load() -> Result<Mpv, String> {
        let mut last = String::from("libmpv not found");
        for c in candidates() {
            let is_path = c.components().count() > 1;
            if is_path && !c.is_file() {
                continue;
            }
            // SAFETY: loading a shared library runs its initialisers; libmpv's are benign.
            match unsafe { libloading::Library::new(&c) } {
                Ok(lib) => return Self::from_lib(lib, c),
                Err(e) => last = format!("{}: {e}", c.display()),
            }
        }
        Err(last)
    }

    fn from_lib(lib: libloading::Library, path: PathBuf) -> Result<Mpv, String> {
        let m = Mpv {
            client_api_version: sym!(lib, "mpv_client_api_version"),
            create: sym!(lib, "mpv_create"),
            initialize: sym!(lib, "mpv_initialize"),
            terminate_destroy: sym!(lib, "mpv_terminate_destroy"),
            set_option_string: sym!(lib, "mpv_set_option_string"),
            command: sym!(lib, "mpv_command"),
            set_property_string: sym!(lib, "mpv_set_property_string"),
            get_property_string: sym!(lib, "mpv_get_property_string"),
            get_property: sym!(lib, "mpv_get_property"),
            set_property: sym!(lib, "mpv_set_property"),
            free: sym!(lib, "mpv_free"),
            wait_event: sym!(lib, "mpv_wait_event"),
            set_wakeup_callback: sym!(lib, "mpv_set_wakeup_callback"),
            error_string: sym!(lib, "mpv_error_string"),
            render_context_create: sym!(lib, "mpv_render_context_create"),
            render_context_set_update_callback: sym!(lib, "mpv_render_context_set_update_callback"),
            render_context_update: sym!(lib, "mpv_render_context_update"),
            render_context_render: sym!(lib, "mpv_render_context_render"),
            render_context_free: sym!(lib, "mpv_render_context_free"),
            _lib: lib,
            path,
        };
        // SAFETY: plain call with no arguments.
        let version = unsafe { (m.client_api_version)() };
        if version >> 16 < 2 {
            return Err(format!(
                "libmpv client API {}.{} is too old (2.0 or newer is needed)",
                version >> 16,
                version & 0xffff
            ));
        }
        Ok(m)
    }

    pub fn error(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string returns a static string for any code.
        unsafe { CStr::from_ptr((self.error_string)(code)) }
            .to_string_lossy()
            .into_owned()
    }
}

pub fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}
