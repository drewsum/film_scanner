//! Print the conversation with a scanner from a USBPcap capture.
//!
//! ```text
//! epson-trace capture.pcap [--device BUS.DEV] [--bytes N] [--dump-data out.bin]
//! ```

use std::process::ExitCode;

use scan_epson::{pcap, trace};

fn usage() -> ExitCode {
    eprintln!(
        "usage: epson-trace CAPTURE.pcap [--device BUS.DEV] [--bytes N] [--threshold N] \
         [--dump-data OUT.bin]"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut device = None;
    let mut opts = trace::LogOptions::default();
    let mut dump = None;
    while let Some(arg) = args.next() {
        let mut value = || args.next();
        match arg.as_str() {
            "--device" => {
                let Some((bus, dev)) = value().and_then(|v| {
                    let (b, d) = v.split_once('.')?;
                    Some((b.parse().ok()?, d.parse().ok()?))
                }) else {
                    return usage();
                };
                device = Some((bus, dev));
            }
            "--bytes" => match value().and_then(|v| v.parse().ok()) {
                Some(n) => opts.max_bytes = n,
                None => return usage(),
            },
            "--threshold" => match value().and_then(|v| v.parse().ok()) {
                Some(n) => opts.data_threshold = n,
                None => return usage(),
            },
            "--dump-data" => match value() {
                Some(v) => dump = Some(v),
                None => return usage(),
            },
            _ if path.is_none() && !arg.starts_with("--") => path = Some(arg),
            _ => return usage(),
        }
    }
    let Some(path) = path else {
        return usage();
    };

    let packets = match std::fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(|f| pcap::parse(&f).map_err(|e| e.to_string()))
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(device) = device.or_else(|| trace::busiest_device(&packets)) else {
        eprintln!("{path}: no bulk traffic");
        return ExitCode::FAILURE;
    };
    let transfers = trace::transfers(&packets, device);
    println!(
        "# {path}: {} packets, device {}.{}: {} transfers",
        packets.len(),
        device.0,
        device.1,
        transfers.len()
    );
    print!("{}", trace::format_log(&transfers, opts));
    if let Some(out) = dump {
        let blocks = trace::data_blocks(&transfers);
        let stream = blocks.concat();
        if let Err(e) = std::fs::write(&out, &stream) {
            eprintln!("{out}: {e}");
            return ExitCode::FAILURE;
        }
        eprintln!(
            "wrote {} bytes of image data ({} blocks) to {out}",
            stream.len(),
            blocks.len()
        );
    }
    ExitCode::SUCCESS
}
