//! Windows Image Acquisition (WIA) backend.
//!
//! WIA ships with Windows and is supported by most flatbed scanner drivers (including the
//! Epson Perfection V-series). It exposes flatbed and, for some drivers, film (transparency
//! adapter) sources with resolution, bit depth and scan-area properties. It does not expose infrared or
//! multi-exposure scanning, so those are reported as unsupported.
//!
//! On non-Windows targets this crate compiles to an empty backend list so the rest of the
//! workspace builds everywhere.

mod convert;
mod dib;

#[cfg(windows)]
mod com;
#[cfg(windows)]
mod device;
#[cfg(windows)]
mod props;
#[cfg(windows)]
mod transfer;

#[cfg(windows)]
pub use device::WiaBackend;

pub use convert::*;
pub use dib::decode_memory_bmp;

/// Register the WIA backend with `registry` when running on Windows; a no-op elsewhere.
pub fn register(registry: &mut scan_core::DeviceRegistry) {
    #[cfg(windows)]
    registry.register(WiaBackend);
    #[cfg(not(windows))]
    let _ = registry;
}
