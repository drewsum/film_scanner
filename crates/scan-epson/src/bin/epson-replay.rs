//! Replay a USBPcap capture of a scan against the attached scanner and save the image data.
//!
//! ```text
//! epson-replay CAPTURE.pcap OUT.bin [--path \\.\Usbscan0] [--threshold N]
//! ```
//!
//! Only replay captures of the same scanner model made by its own driver: the sequence moves
//! the carriage and switches the lamp exactly as the original session did.

use std::process::ExitCode;

#[cfg(windows)]
fn run() -> Result<(), String> {
    use scan_epson::transport::{UsbScan, find_usbscan_devices};
    use scan_epson::{pcap, replay, trace};

    let mut args = std::env::args().skip(1);
    let mut positional = Vec::new();
    let mut path = None;
    let mut threshold = 512usize;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--path" => path = args.next(),
            "--threshold" => {
                threshold = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("bad --threshold")?
            }
            _ => positional.push(arg),
        }
    }
    let [capture, out] = &positional[..] else {
        return Err("usage: epson-replay CAPTURE.pcap OUT.bin [--path DEV] [--threshold N]".into());
    };
    let file = std::fs::read(capture).map_err(|e| format!("{capture}: {e}"))?;
    let packets = pcap::parse(&file).map_err(|e| e.to_string())?;
    let device = trace::busiest_device(&packets).ok_or("capture has no bulk traffic")?;
    let transfers = trace::transfers(&packets, device);

    let path = match path {
        Some(p) => p,
        None => find_usbscan_devices()
            .into_iter()
            .find(|d| d.usb_id() == Some((scan_epson::EPSON_VID, scan_epson::V500_PID)))
            .map(|d| d.path)
            .ok_or("no V500 found; pass --path")?,
    };
    let mut dev = UsbScan::open(&path).map_err(|e| e.to_string())?;
    dev.set_timeouts(20, 20).map_err(|e| e.to_string())?;
    let report = replay::replay(
        &transfers,
        &mut dev,
        threshold,
        std::time::Duration::from_millis(25),
    );
    for m in &report.mismatches {
        println!(
            "transfer {} (after {:02x?}): expected {:02x?}, got {:02x?}",
            m.index,
            last_write(&transfers, m.index),
            m.expected,
            m.actual
        );
    }
    if let Some((index, e)) = &report.error {
        println!(
            "stopped at transfer {index} ({:?} {} bytes, after {:02x?}): {e}",
            transfers[*index].direction,
            transfers[*index].data.len(),
            last_write(&transfers, *index),
        );
    }
    std::fs::write(out, &report.data).map_err(|e| format!("{out}: {e}"))?;
    println!(
        "{} writes, {} reads, {} mismatched replies, {} data bytes -> {out}",
        report.writes,
        report.reads,
        report.mismatches.len(),
        report.data.len()
    );
    Ok(())
}

/// The first bytes of the last OUT transfer before `index`, to place a reply in context.
#[cfg(windows)]
fn last_write(transfers: &[scan_epson::trace::Transfer], index: usize) -> &[u8] {
    transfers[..index]
        .iter()
        .rev()
        .find(|t| t.direction == scan_epson::trace::Direction::Out)
        .map_or(&[], |t| &t.data[..t.data.len().min(8)])
}

#[cfg(not(windows))]
fn run() -> Result<(), String> {
    Err("epson-replay currently supports only Windows (usbscan.sys)".into())
}

fn main() -> ExitCode {
    env_logger::init();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("epson-replay: {e}");
            ExitCode::FAILURE
        }
    }
}
