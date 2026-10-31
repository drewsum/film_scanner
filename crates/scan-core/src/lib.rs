//! Backend-agnostic scanner abstraction.
//!
//! A [`ScannerBackend`] (WIA, SANE, mock, ...) enumerates [`DeviceInfo`]s and opens
//! [`ScannerDevice`]s. A device reports its [`Capabilities`] and performs scans described
//! by a [`ScanRequest`], producing a [`ScanResult`] that can hold several exposure passes
//! and an optional infrared channel.

mod backend;
mod error;
pub mod export;
pub use image;
pub mod mock;
mod progress;
mod registry;
mod types;

pub use backend::{ScannerBackend, ScannerDevice};
pub use error::{ScanError, ScanResultExt};
pub use progress::{CancelToken, NoProgress, ProgressSink};
pub use registry::{DeviceListing, DeviceRegistry};
pub use types::*;
