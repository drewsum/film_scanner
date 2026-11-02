//! The SANE 1.0 C interface (`sane/sane.h`), loaded from `libsane` at run time.

use std::ffi::{c_char, c_void};
use std::sync::{Mutex, MutexGuard, OnceLock};

use libloading::Library;

pub type Word = i32;
pub type Status = i32;
pub type Handle = *mut c_void;

pub const STATUS_GOOD: Status = 0;
pub const STATUS_CANCELLED: Status = 2;
pub const STATUS_EOF: Status = 5;

pub const TYPE_BOOL: i32 = 0;
pub const TYPE_INT: i32 = 1;
pub const TYPE_FIXED: i32 = 2;
pub const TYPE_STRING: i32 = 3;
pub const TYPE_BUTTON: i32 = 4;
pub const TYPE_GROUP: i32 = 5;

pub const UNIT_PIXEL: i32 = 1;
pub const UNIT_MM: i32 = 3;
pub const UNIT_DPI: i32 = 4;

pub const CAP_SOFT_SELECT: Word = 1 << 0;
pub const CAP_INACTIVE: Word = 1 << 5;

pub const INFO_RELOAD_OPTIONS: Word = 1 << 1;

pub const CONSTRAINT_RANGE: i32 = 1;
pub const CONSTRAINT_WORD_LIST: i32 = 2;
pub const CONSTRAINT_STRING_LIST: i32 = 3;

pub const ACTION_GET_VALUE: i32 = 0;
pub const ACTION_SET_VALUE: i32 = 1;

pub const FRAME_GRAY: i32 = 0;
pub const FRAME_RGB: i32 = 1;
pub const FRAME_RED: i32 = 2;
pub const FRAME_GREEN: i32 = 3;
pub const FRAME_BLUE: i32 = 4;

/// `SANE_Fixed` values are 16.16 fixed point.
pub const FIXED_SCALE: f64 = 65536.0;

#[repr(C)]
pub struct Device {
    pub name: *const c_char,
    pub vendor: *const c_char,
    pub model: *const c_char,
    pub kind: *const c_char,
}

#[repr(C)]
pub struct Range {
    pub min: Word,
    pub max: Word,
    pub quant: Word,
}

#[repr(C)]
pub struct OptionDescriptor {
    pub name: *const c_char,
    pub title: *const c_char,
    pub desc: *const c_char,
    pub kind: i32,
    pub unit: i32,
    pub size: Word,
    pub cap: Word,
    pub constraint_type: i32,
    /// A `SANE_Range *`, a word list (`SANE_Word *`, count first) or a NULL-terminated
    /// string list (`SANE_String_Const *`), depending on `constraint_type`.
    pub constraint: *const c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Parameters {
    pub format: i32,
    pub last_frame: Word,
    pub bytes_per_line: Word,
    pub pixels_per_line: Word,
    /// -1 when the driver doesn't know the number of lines in advance.
    pub lines: Word,
    pub depth: Word,
}

type AuthCallback = Option<unsafe extern "C" fn(*const c_char, *mut c_char, *mut c_char)>;

/// The `libsane` entry points the backend uses.
#[derive(Clone, Copy)]
pub struct Api {
    pub init: unsafe extern "C" fn(*mut Word, AuthCallback) -> Status,
    pub get_devices: unsafe extern "C" fn(*mut *const *const Device, Word) -> Status,
    pub open: unsafe extern "C" fn(*const c_char, *mut Handle) -> Status,
    pub close: unsafe extern "C" fn(Handle),
    pub get_option_descriptor: unsafe extern "C" fn(Handle, Word) -> *const OptionDescriptor,
    pub control_option: unsafe extern "C" fn(Handle, Word, i32, *mut c_void, *mut Word) -> Status,
    pub get_parameters: unsafe extern "C" fn(Handle, *mut Parameters) -> Status,
    pub start: unsafe extern "C" fn(Handle) -> Status,
    pub read: unsafe extern "C" fn(Handle, *mut u8, Word, *mut Word) -> Status,
    pub cancel: unsafe extern "C" fn(Handle),
    pub strstatus: unsafe extern "C" fn(Status) -> *const c_char,
}

/// Where `libsane` is looked for, after `FILM_SCANNER_LIBSANE` if that is set.
const LIBRARY_NAMES: &[&str] = &[
    "libsane.so.1",
    "libsane.so",
    "libsane.1.dylib",
    "/opt/homebrew/lib/libsane.1.dylib",
    "/usr/local/lib/libsane.1.dylib",
    "/opt/local/lib/libsane.1.dylib",
];

struct Loaded {
    api: Mutex<Api>,
    // Keeps the function pointers in `api` valid; never unloaded.
    _library: Library,
}

static LIBSANE: OnceLock<Result<Loaded, String>> = OnceLock::new();

/// Lock SANE for a call, loading and initialising `libsane` on first use. SANE isn't
/// thread-safe, so every call goes through this one lock. `Err` says why SANE is
/// unavailable.
pub fn lock() -> Result<MutexGuard<'static, Api>, &'static str> {
    let loaded = LIBSANE.get_or_init(load).as_ref().map_err(String::as_str)?;
    Ok(loaded.api.lock().unwrap_or_else(|e| e.into_inner()))
}

fn load() -> Result<Loaded, String> {
    let names = std::env::var("FILM_SCANNER_LIBSANE")
        .ok()
        .into_iter()
        .chain(LIBRARY_NAMES.iter().map(|s| s.to_string()));
    let mut tried = Vec::new();
    for name in names {
        // SAFETY: loading libsane runs its initialisers, which are trusted like any other
        // system library.
        match unsafe { Library::new(&name) } {
            Ok(library) => return init(library).map_err(|e| format!("{name}: {e}")),
            Err(e) => tried.push(format!("{name}: {e}")),
        }
    }
    Err(format!("libsane not found ({})", tried.join("; ")))
}

fn init(library: Library) -> Result<Loaded, String> {
    // SAFETY: the symbol types match the declarations in sane/sane.h.
    let api = unsafe {
        macro_rules! sym {
            ($name:literal) => {
                *library
                    .get(concat!($name, "\0").as_bytes())
                    .map_err(|e| e.to_string())?
            };
        }
        Api {
            init: sym!("sane_init"),
            get_devices: sym!("sane_get_devices"),
            open: sym!("sane_open"),
            close: sym!("sane_close"),
            get_option_descriptor: sym!("sane_get_option_descriptor"),
            control_option: sym!("sane_control_option"),
            get_parameters: sym!("sane_get_parameters"),
            start: sym!("sane_start"),
            read: sym!("sane_read"),
            cancel: sym!("sane_cancel"),
            strstatus: sym!("sane_strstatus"),
        }
    };
    let mut version = 0;
    // SAFETY: called once, before any other SANE call; no authorisation callback.
    let status = unsafe { (api.init)(&mut version, None) };
    if status != STATUS_GOOD {
        return Err(format!("sane_init failed with status {status}"));
    }
    log::info!(
        "SANE {}.{}.{} loaded",
        (version >> 24) & 0xff,
        (version >> 16) & 0xff,
        version & 0xffff
    );
    Ok(Loaded {
        api: Mutex::new(api),
        _library: library,
    })
}

/// Copy a C string that may be NULL.
///
/// # Safety
/// `s` must be NULL or point to a NUL-terminated string.
pub unsafe fn string(s: *const c_char) -> String {
    if s.is_null() {
        String::new()
    } else {
        // SAFETY: guaranteed by the caller.
        unsafe { std::ffi::CStr::from_ptr(s) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Api {
    pub fn status_text(&self, status: Status) -> String {
        // SAFETY: sane_strstatus returns a static string.
        unsafe { string((self.strstatus)(status)) }
    }
}
