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

pub mod firmware;
pub mod pcap;
pub mod replay;
pub mod trace;
pub mod transport;

/// Epson's USB vendor id.
pub const EPSON_VID: u16 = 0x04b8;
/// Perfection V500 Photo (GT-X770).
pub const V500_PID: u16 = 0x0130;
