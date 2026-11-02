//! [SANE](http://www.sane-project.org/) backend, the standard scanner interface on Linux
//! and, through Homebrew or MacPorts, on macOS.
//!
//! `libsane` is loaded at run time, so building needs no SANE headers or libraries, and
//! without SANE installed the backend simply lists no devices. Set `FILM_SCANNER_LIBSANE`
//! to load it from somewhere else.
//!
//! - [`ffi`] is the C interface, behind one process-wide lock (SANE isn't thread-safe)
//! - [`options`] reads SANE's self-describing options into Rust values
//! - [`device`] maps the well-known options (`source`, `mode`, `depth`, `resolution` and
//!   the scan area) onto [`scan_core`] capabilities, and reads the frames of a scan

pub mod device;
pub mod ffi;
mod handle;
pub mod options;

#[cfg(test)]
mod fake;

pub use device::{SaneBackend, SaneDevice};

/// Register the SANE backend with `registry` outside Windows; a no-op on Windows.
pub fn register(registry: &mut scan_core::DeviceRegistry) {
    #[cfg(not(windows))]
    registry.register(SaneBackend);
    #[cfg(windows)]
    let _ = registry;
}
