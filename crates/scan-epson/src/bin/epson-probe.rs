//! Send raw bytes to a scanner's bulk OUT pipe and print what comes back.
//!
//! ```text
//! epson-probe --list
//! epson-probe [--path \\.\Usbscan0] [--read N] [--timeout S] HEX [HEX ...]
//! ```
//!
//! Each HEX argument (e.g. `1b40`) is written as one transfer, followed by one read of up to
//! N bytes (`--read 0` skips the read). Only send commands whose effect you know: some
//! scanners will move the carriage or lamp on command.

use std::process::ExitCode;

#[cfg(any(windows, test))]
fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let s: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':')
        .collect();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(windows)]
fn run() -> Result<(), String> {
    use scan_epson::trace::hex_ascii;
    use scan_epson::transport::{Transport, UsbScan, find_usbscan_devices};

    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut read_len = 512usize;
    let mut timeout = 5u32;
    let mut commands = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--list" => {
                for d in find_usbscan_devices() {
                    let id = d
                        .usb_id()
                        .map(|(v, p)| format!("{v:04x}:{p:04x}"))
                        .unwrap_or_default();
                    println!("{}  {id}  {}", d.path, d.description);
                }
                return Ok(());
            }
            "--path" => path = args.next(),
            "--read" => {
                read_len = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("bad --read")?
            }
            "--timeout" => {
                timeout = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("bad --timeout")?
            }
            hex => commands.push(parse_hex(hex).ok_or_else(|| format!("bad hex: {hex}"))?),
        }
    }
    let path = match path {
        Some(p) => p,
        None => find_usbscan_devices()
            .into_iter()
            .find(|d| d.usb_id().is_some_and(|(v, _)| v == scan_epson::EPSON_VID))
            .map(|d| d.path)
            .ok_or("no Epson usbscan device found; pass --path")?,
    };
    let mut dev = UsbScan::open(&path).map_err(|e| e.to_string())?;
    if let Err(e) = dev.set_timeouts(timeout, timeout) {
        eprintln!("warning: {e}");
    }
    println!("# {path}");
    for cmd in commands {
        println!("> {}", hex_ascii(&cmd, usize::MAX));
        dev.write(&cmd).map_err(|e| e.to_string())?;
        if read_len > 0 {
            let mut buf = vec![0u8; read_len];
            match dev.read(&mut buf) {
                Ok(n) => println!("< [{n}] {}", hex_ascii(&buf[..n], usize::MAX)),
                Err(e) => println!("< error: {e}"),
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn run() -> Result<(), String> {
    Err("epson-probe currently supports only Windows (usbscan.sys)".into())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("epson-probe: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_hex;

    #[test]
    fn hex_parsing() {
        assert_eq!(parse_hex("1b40"), Some(vec![0x1b, 0x40]));
        assert_eq!(parse_hex("1b:03 ff"), Some(vec![0x1b, 0x03, 0xff]));
        assert_eq!(parse_hex("1b4"), None);
        assert_eq!(parse_hex("zz"), None);
    }
}
