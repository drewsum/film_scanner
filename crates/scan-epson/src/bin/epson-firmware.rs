//! Load Epson's firmware into a V500 that was powered on without its driver running.
//!
//! ```text
//! epson-firmware [FIRMWARE.bin] [--path \\.\Usbscan0] [--force]
//! ```
//!
//! The firmware defaults to Epson Scan's `esfw7c.bin`. Nothing is sent if the scanner already
//! runs firmware, unless `--force` is given.

use std::process::ExitCode;

#[cfg(windows)]
fn run() -> Result<(), String> {
    use scan_epson::firmware::{self, Firmware};
    use scan_epson::transport::{UsbScan, find_usbscan_devices};

    let mut args = std::env::args().skip(1);
    let mut file = firmware::V500_FIRMWARE_PATH.to_string();
    let mut path = None;
    let mut force = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--path" => path = args.next(),
            "--force" => force = true,
            _ => file = arg,
        }
    }
    let fw = Firmware::parse(&std::fs::read(&file).map_err(|e| format!("{file}: {e}"))?)
        .map_err(|e| format!("{file}: {e}"))?;

    let path = match path {
        Some(p) => p,
        None => find_usbscan_devices()
            .into_iter()
            .find(|d| d.usb_id() == Some((fw.vendor_id, scan_epson::V500_PID)))
            .map(|d| d.path)
            .ok_or("no V500 found; pass --path")?,
    };
    let mut dev = UsbScan::open(&path).map_err(|e| e.to_string())?;
    dev.set_timeouts(5, 5).map_err(|e| e.to_string())?;
    let status = firmware::status(&mut dev).map_err(|e| e.to_string())?;
    println!("{path}: status {status:02x?}");
    if status[0] & firmware::STATUS_NO_FIRMWARE == 0 && !force {
        println!("firmware already running");
        return Ok(());
    }
    println!(
        "uploading {} bytes from {file} (checksum {:02x})",
        fw.image.len(),
        fw.checksum()
    );
    firmware::upload(&mut dev, &fw, std::time::Duration::from_secs(60))
        .map_err(|e| e.to_string())?;
    println!(
        "scanner up: status {:02x?}",
        firmware::status(&mut dev).map_err(|e| e.to_string())?
    );
    Ok(())
}

#[cfg(not(windows))]
fn run() -> Result<(), String> {
    Err("epson-firmware currently supports only Windows (usbscan.sys)".into())
}

fn main() -> ExitCode {
    env_logger::init();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("epson-firmware: {e}");
            ExitCode::FAILURE
        }
    }
}
