//! Native USB support for Epson "interpreter" scanners, starting with the Perfection V500
//! (`04b8:0130`).
//!
//! These scanners don't speak ESC/I. Epson's closed plugin translates ESC/I into a
//! device-specific protocol, which this crate reverse-engineers from USB captures of the
//! official driver. Notes live in `docs/epson-v500/`.
//!
//! - [`pcap`] and [`trace`] read USBPcap captures and print the host/device conversation
//!   (the `epson-trace` tool)
//! - [`transport`] moves bytes over the scanner's bulk pipes (the `epson-probe` tool)
//! - [`replay`] plays a captured session back to a scanner (the `epson-replay` tool)
//! - [`firmware`] loads Epson's firmware into a freshly powered scanner (`epson-firmware`)
//! - [`session`] runs a flatbed scan from scratch over [`protocol`] (the `epson-scan` tool),
//!   [`film`] a film scan through the transparency unit; [`sim`] simulates the scanner for
//!   tests
//! - [`device`] is the [`scan_core`] backend built on it; [`system`] handles the Windows side
//!   (administrator rights, pausing the WIA service)

pub mod calibration;
pub mod data;
pub mod decode;
pub mod device;
pub mod film;
pub mod firmware;
pub mod geometry;
pub mod pcap;
pub mod protocol;
pub mod replay;
pub mod session;
pub mod sim;
pub mod system;
pub mod tables;
pub mod template;
pub mod trace;
pub mod transport;

/// Epson's USB vendor id.
pub const EPSON_VID: u16 = 0x04b8;
/// Perfection V500 Photo (GT-X770).
pub const V500_PID: u16 = 0x0130;

pub use device::EpsonBackend;

/// Register the native backend with `registry`.
pub fn register(registry: &mut scan_core::DeviceRegistry) {
    registry.register(EpsonBackend);
}
