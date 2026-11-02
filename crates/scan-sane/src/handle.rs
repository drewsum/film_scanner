//! [`Driver`] over libsane.

use std::ffi::{CString, c_void};
use std::ptr;

use scan_core::ScanError;

use crate::ffi::{self, Api, Status};
use crate::options::{Constraint, Driver, Frame, Kind, OptionDesc, Parameters, Unit, Value};

/// A device as `sane_get_devices` lists it.
#[derive(Clone, Debug)]
pub struct DeviceEntry {
    /// The name to open it by, e.g. `epson2:libusb:001:004`.
    pub name: String,
    pub vendor: String,
    pub model: String,
}

/// Why SANE can't be used, if it can't.
pub fn unavailable() -> Option<&'static str> {
    ffi::lock().err()
}

fn lock() -> Result<std::sync::MutexGuard<'static, Api>, ScanError> {
    ffi::lock().map_err(|e| ScanError::Backend(format!("SANE unavailable: {e}")))
}

fn check(api: &Api, status: Status, what: &str) -> Result<(), ScanError> {
    match status {
        ffi::STATUS_GOOD => Ok(()),
        ffi::STATUS_CANCELLED => Err(ScanError::Cancelled),
        _ => Err(ScanError::Backend(format!(
            "SANE {what}: {}",
            api.status_text(status)
        ))),
    }
}

/// Every device SANE's drivers can reach, including network ones.
pub fn devices() -> Result<Vec<DeviceEntry>, ScanError> {
    let api = lock()?;
    let mut list: *const *const ffi::Device = ptr::null();
    // SAFETY: `list` receives a NULL-terminated array that stays valid until the next
    // sane_get_devices call, which the lock prevents while we copy it.
    unsafe {
        check(&api, (api.get_devices)(&mut list, 0), "listing devices")?;
        let mut out = Vec::new();
        let mut entry = list;
        while !entry.is_null() && !(*entry).is_null() {
            let d = &**entry;
            out.push(DeviceEntry {
                name: ffi::string(d.name),
                vendor: ffi::string(d.vendor),
                model: ffi::string(d.model),
            });
            entry = entry.add(1);
        }
        Ok(out)
    }
}

/// An open libsane device.
pub struct SaneHandle {
    handle: ffi::Handle,
}

// SAFETY: the handle is only used under the process-wide SANE lock.
unsafe impl Send for SaneHandle {}

impl SaneHandle {
    pub fn open(name: &str) -> Result<Self, ScanError> {
        let api = lock()?;
        let name_c = CString::new(name).map_err(|e| ScanError::DeviceNotFound(e.to_string()))?;
        let mut handle = ptr::null_mut();
        // SAFETY: valid C string and out pointer.
        check(
            &api,
            unsafe { (api.open)(name_c.as_ptr(), &mut handle) },
            &format!("opening {name}"),
        )?;
        Ok(Self { handle })
    }
}

impl Drop for SaneHandle {
    fn drop(&mut self) {
        if let Ok(api) = lock() {
            // SAFETY: the handle came from sane_open and is closed once.
            unsafe { (api.close)(self.handle) };
        }
    }
}

/// Read one option descriptor into Rust values.
///
/// # Safety
/// `d` must point to a valid descriptor from `sane_get_option_descriptor`.
unsafe fn descriptor(index: usize, d: &ffi::OptionDescriptor) -> OptionDesc {
    let kind = match d.kind {
        ffi::TYPE_BOOL => Kind::Bool,
        ffi::TYPE_INT => Kind::Int,
        ffi::TYPE_FIXED => Kind::Fixed,
        ffi::TYPE_STRING => Kind::String,
        _ => Kind::Other,
    };
    let unit = match d.unit {
        0 => Unit::None,
        ffi::UNIT_PIXEL => Unit::Pixel,
        ffi::UNIT_MM => Unit::Mm,
        ffi::UNIT_DPI => Unit::Dpi,
        _ => Unit::Other,
    };
    let scale = if kind == Kind::Fixed {
        1.0 / ffi::FIXED_SCALE
    } else {
        1.0
    };
    let number = |w: ffi::Word| w as f64 * scale;
    // SAFETY: the constraint pointer has the type its constraint_type says.
    let constraint = unsafe {
        match d.constraint_type {
            _ if d.constraint.is_null() => Constraint::None,
            ffi::CONSTRAINT_RANGE => {
                let r = &*(d.constraint as *const ffi::Range);
                Constraint::Range {
                    min: number(r.min),
                    max: number(r.max),
                    quant: number(r.quant),
                }
            }
            ffi::CONSTRAINT_WORD_LIST => {
                let words = d.constraint as *const ffi::Word;
                let count = (*words).max(0) as usize;
                Constraint::Numbers((1..=count).map(|i| number(*words.add(i))).collect())
            }
            ffi::CONSTRAINT_STRING_LIST => {
                let mut strings = Vec::new();
                let mut s = d.constraint as *const *const std::ffi::c_char;
                while !(*s).is_null() {
                    strings.push(ffi::string(*s));
                    s = s.add(1);
                }
                Constraint::Strings(strings)
            }
            _ => Constraint::None,
        }
    };
    OptionDesc {
        index,
        // SAFETY: descriptor strings are NULL or NUL-terminated.
        name: unsafe { ffi::string(d.name) },
        kind,
        unit,
        size: d.size.max(0) as usize,
        settable: d.cap & ffi::CAP_SOFT_SELECT != 0 && d.cap & ffi::CAP_INACTIVE == 0,
        constraint,
    }
}

impl Driver for SaneHandle {
    fn options(&mut self) -> Result<Vec<OptionDesc>, ScanError> {
        let api = lock()?;
        let mut count: ffi::Word = 0;
        // SAFETY: option 0 is always the option count, an Int.
        check(
            &api,
            unsafe {
                (api.control_option)(
                    self.handle,
                    0,
                    ffi::ACTION_GET_VALUE,
                    (&mut count as *mut ffi::Word).cast(),
                    ptr::null_mut(),
                )
            },
            "reading the option count",
        )?;
        let mut options = Vec::new();
        for index in 1..count.max(0) {
            // SAFETY: index is within the option count; the descriptor stays valid until
            // the options are reloaded, and is copied straight away.
            unsafe {
                let d = (api.get_option_descriptor)(self.handle, index);
                if !d.is_null() {
                    options.push(descriptor(index as usize, &*d));
                }
            }
        }
        Ok(options)
    }

    fn set(&mut self, option: &OptionDesc, value: &Value) -> Result<bool, ScanError> {
        let word_option = matches!(option.kind, Kind::Bool | Kind::Int | Kind::Fixed);
        let mut word: ffi::Word = match (option.kind, value) {
            (Kind::Bool, Value::Bool(b)) => *b as ffi::Word,
            (Kind::Int, Value::Number(n)) => n.round() as ffi::Word,
            (Kind::Fixed, Value::Number(n)) => (n * ffi::FIXED_SCALE).round() as ffi::Word,
            (Kind::String, Value::String(_)) => 0,
            _ => {
                return Err(ScanError::Backend(format!(
                    "SANE option {} can't be set to {value:?}",
                    option.name
                )));
            }
        };
        if word_option && option.size != size_of::<ffi::Word>() {
            return Err(ScanError::Unsupported(format!(
                "SANE option {} holds an array",
                option.name
            )));
        }
        // SANE copies a string option's value from a buffer of the option's full size.
        let mut text = Vec::new();
        if let Value::String(s) = value {
            text = s.as_bytes().to_vec();
            text.resize(option.size.max(s.len() + 1), 0);
        }
        let target: *mut c_void = if word_option {
            (&mut word as *mut ffi::Word).cast()
        } else {
            text.as_mut_ptr().cast()
        };
        let api = lock()?;
        let mut info: ffi::Word = 0;
        // SAFETY: `target` matches the option's type and size.
        check(
            &api,
            unsafe {
                (api.control_option)(
                    self.handle,
                    option.index as ffi::Word,
                    ffi::ACTION_SET_VALUE,
                    target,
                    &mut info,
                )
            },
            &format!("setting {} to {value:?}", option.name),
        )?;
        Ok(info & ffi::INFO_RELOAD_OPTIONS != 0)
    }

    fn start(&mut self) -> Result<(), ScanError> {
        let api = lock()?;
        // SAFETY: valid handle.
        check(
            &api,
            unsafe { (api.start)(self.handle) },
            "starting the scan",
        )
    }

    fn parameters(&mut self) -> Result<Parameters, ScanError> {
        let api = lock()?;
        let mut p = ffi::Parameters::default();
        // SAFETY: valid handle and out pointer.
        check(
            &api,
            unsafe { (api.get_parameters)(self.handle, &mut p) },
            "reading the scan parameters",
        )?;
        let format = match p.format {
            ffi::FRAME_GRAY => Frame::Gray,
            ffi::FRAME_RGB => Frame::Rgb,
            ffi::FRAME_RED => Frame::Red,
            ffi::FRAME_GREEN => Frame::Green,
            ffi::FRAME_BLUE => Frame::Blue,
            other => {
                return Err(ScanError::Unsupported(format!("SANE frame format {other}")));
            }
        };
        Ok(Parameters {
            format,
            last_frame: p.last_frame != 0,
            bytes_per_line: p.bytes_per_line.max(0) as usize,
            pixels_per_line: p.pixels_per_line.max(0) as usize,
            lines: (p.lines >= 0).then_some(p.lines as usize),
            depth: p.depth.max(0) as u32,
        })
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ScanError> {
        let api = lock()?;
        let max = buf.len().min(i32::MAX as usize) as ffi::Word;
        let mut len: ffi::Word = 0;
        // SAFETY: `buf` holds at least `max` bytes.
        let status = unsafe { (api.read)(self.handle, buf.as_mut_ptr(), max, &mut len) };
        if status == ffi::STATUS_EOF {
            return Ok(None);
        }
        check(&api, status, "reading scan data")?;
        Ok(Some(len.clamp(0, max) as usize))
    }

    fn cancel(&mut self) {
        if let Ok(api) = lock() {
            // SAFETY: valid handle.
            unsafe { (api.cancel)(self.handle) };
        }
    }
}
