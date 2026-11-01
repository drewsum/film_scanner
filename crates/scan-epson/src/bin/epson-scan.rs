//! Scan an area of the flatbed natively, using a full-width capture of Epson's driver as the
//! template for everything we don't generate yet.
//!
//! ```text
//! epson-scan --template CAPTURE.pcap [--template ...] --dpi N --area X,Y,W,H --out OUT.png [--path DEV]
//! epson-scan --template CAPTURE.pcap --dpi N --area X,Y,W,H --out OUT.png --offline
//! ```
//!
//! The area is in mm from the bed's corner. A template fits when its hardware resolution matches
//! the request's (200 dpi for up to 200 dpi, 400 for up to 400) and it was captured with the same
//! carriage-move profile (an area at the top of the bed, or further down); pass several and the
//! first that fits is used. Stop the WIA service first
//! (`Stop-Service stisvc -Force`), or its polling interferes. `--offline` decodes the
//! template's own recorded read instead of scanning, for checking the decoding.

use std::process::ExitCode;

use scan_core::ScanArea;
use scan_epson::template::Template;
use scan_epson::{decode, pcap, trace};

struct Args {
    templates: Vec<String>,
    dpi: u32,
    area: ScanArea,
    out: String,
    path: Option<String>,
    offline: bool,
}

fn parse_args() -> Result<Args, String> {
    let usage = "usage: epson-scan --template CAPTURE.pcap --dpi N --area X,Y,W,H --out OUT.png \
                 [--path DEV] [--offline]";
    let mut args = std::env::args().skip(1);
    let (mut templates, mut dpi, mut area, mut out, mut path, mut offline) =
        (Vec::new(), None, None, None, None, false);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--template" => templates.push(value()?),
            "--dpi" => dpi = Some(value()?.parse().map_err(|_| "bad --dpi")?),
            "--area" => {
                let v: Vec<f32> = value()?
                    .split(',')
                    .map(|s| s.trim().parse().map_err(|_| "bad --area"))
                    .collect::<Result<_, _>>()?;
                let [x_mm, y_mm, width_mm, height_mm] = v[..] else {
                    return Err("--area needs X,Y,W,H".into());
                };
                area = Some(ScanArea {
                    x_mm,
                    y_mm,
                    width_mm,
                    height_mm,
                });
            }
            "--out" => out = Some(value()?),
            "--path" => path = Some(value()?),
            "--offline" => offline = true,
            _ => return Err(usage.into()),
        }
    }
    Ok(Args {
        templates: if templates.is_empty() {
            return Err(usage.into());
        } else {
            templates
        },
        dpi: dpi.ok_or(usage)?,
        area: area.ok_or(usage)?,
        out: out.ok_or(usage)?,
        path,
        offline,
    })
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    // Use the first template that fits the request (resolution and carriage move).
    let mut chosen = None;
    let mut reasons = Vec::new();
    for path in &args.templates {
        let template = load_template(path)?;
        match template.script(&args.area, args.dpi) {
            Ok(script) => {
                println!(
                    "template {path}: {} dpi hardware, {:?} move, x {}..{}, {} px per line",
                    template.hw_dpi,
                    template.move_profile,
                    template.x_start,
                    template.x_end,
                    template.width
                );
                chosen = Some((template, script));
                break;
            }
            Err(e) => reasons.push(format!("{path}: {e}")),
        }
    }
    let (template, script) = chosen.ok_or_else(|| reasons.join("\n"))?;
    println!(
        "plan: {}x{} px, {} lines at {} dpi, move {} steps, scan {} steps, crop x {}",
        script.plan.width,
        script.plan.height,
        script.plan.lines,
        script.plan.hw_dpi,
        script.plan.move_steps,
        script.plan.scan_steps,
        script.layout.crop_x
    );

    let raw = if args.offline {
        let data = template.captured_data();
        if template.lines() < script.plan.lines {
            return Err(format!(
                "--offline: the template read has {} lines, the plan needs {}",
                template.lines(),
                script.plan.lines
            ));
        }
        data
    } else {
        scan(&script, args.path)?
    };
    let image = decode::flatbed_rgb8(&raw, script.layout, &script.plan);
    image
        .save(&args.out)
        .map_err(|e| format!("{}: {e}", args.out))?;
    println!("saved {}x{} to {}", image.width(), image.height(), args.out);
    Ok(())
}

fn load_template(path: &str) -> Result<Template, String> {
    let file = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let packets = pcap::parse(&file).map_err(|e| format!("{path}: {e}"))?;
    let device = trace::busiest_device(&packets).ok_or(format!("{path}: no bulk traffic"))?;
    Template::from_transfers(&trace::transfers(&packets, device))
        .map_err(|e| format!("{path}: {e}"))
}

#[cfg(windows)]
fn scan(script: &scan_epson::template::Script, path: Option<String>) -> Result<Vec<u8>, String> {
    use scan_epson::transport::{UsbScan, find_usbscan_devices};
    let path = match path {
        Some(p) => p,
        None => find_usbscan_devices()
            .into_iter()
            .find(|d| d.usb_id() == Some((scan_epson::EPSON_VID, scan_epson::V500_PID)))
            .map(|d| d.path)
            .ok_or("no V500 found; pass --path")?,
    };
    let mut dev = UsbScan::open(&path).map_err(|e| e.to_string())?;
    dev.set_timeouts(30, 30).map_err(|e| e.to_string())?;
    Template::run(script, &mut dev, std::time::Duration::from_millis(25)).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn scan(_: &scan_epson::template::Script, _: Option<String>) -> Result<Vec<u8>, String> {
    Err("scanning currently supports only Windows (usbscan.sys); use --offline".into())
}

fn main() -> ExitCode {
    env_logger::init();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("epson-scan: {e}");
            ExitCode::FAILURE
        }
    }
}
