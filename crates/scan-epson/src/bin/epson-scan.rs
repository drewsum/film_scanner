//! Scan an area of the flatbed natively, up to 400 dpi, or of the film holder (`--film`), up
//! to 6,400.
//!
//! ```text
//! epson-scan --dpi N --area X,Y,W,H --out OUT.png [--bits 8|16]
//!            [--film [--ev EV | --ir] [--raw RAW.bin]] [--path DEV] [--simulate]
//! epson-scan --template CAPTURE.pcap [--template ...] --dpi N --area X,Y,W,H --out OUT.png
//!            [--calibration live|recorded|flat] [--path DEV] [--offline]
//! ```
//!
//! The area is in mm from the bed's corner, or with `--film` from the film area's (2.7 × 9.33 in,
//! SilverFast's preview). `--ev` exposes a film read up to one stop more than calibrated (a
//! multi-exposure pass); `--ir` makes the infrared pass instead, saved as a 16-bit grey plane.
//! `--raw` also saves the film read as the scanner sent it: 16-bit little-endian RGB, the
//! plan's read width per line.
//! Stop the WIA service first
//! (`Stop-Service stisvc -Force`), or its polling interferes. By default the whole session is
//! generated ([`scan_epson::session`]); `--simulate` runs it against a simulated scanner.
//!
//! With `--template`, a capture of Epson's driver is replayed instead, with the request's area
//! patched in. A template fits when its hardware resolution matches
//! the request's (200 dpi for up to 200 dpi, 400 for up to 400) and it was captured with the same
//! carriage-move profile (an area at the top of the bed, or further down); pass several and the
//! first that fits is used. `--offline` decodes the template's own recorded read instead of
//! scanning, for checking the decoding. Calibration is computed from the scan's own reads by
//! default; `recorded` replays the template's values and `flat` turns shading correction off,
//! for comparison.

use std::process::ExitCode;

use scan_core::{BitDepth, NoProgress, ScanArea};
use scan_epson::film::FilmSession;
use scan_epson::session::FlatbedSession;
use scan_epson::sim::SimulatedScanner;
use scan_epson::template::{Calibration, Template};
use scan_epson::transport::Transport;
use scan_epson::{decode, pcap, trace};

const POLL_DELAY: std::time::Duration = std::time::Duration::from_millis(25);

struct Args {
    templates: Vec<String>,
    dpi: u32,
    area: ScanArea,
    out: String,
    path: Option<String>,
    offline: bool,
    simulate: bool,
    calibration: Calibration,
    bit_depth: BitDepth,
    film: bool,
    ev: f32,
    infrared: bool,
    raw: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let usage = "usage: epson-scan --template CAPTURE.pcap --dpi N --area X,Y,W,H --out OUT.png \
                 [--calibration live|recorded|flat] [--path DEV] [--offline]";
    let mut args = std::env::args().skip(1);
    let (mut templates, mut dpi, mut area, mut out, mut path, mut offline) =
        (Vec::new(), None, None, None, None, false);
    let mut simulate = false;
    let mut calibration = Calibration::Live;
    let mut bit_depth = BitDepth::Eight;
    let (mut film, mut ev, mut infrared, mut raw) = (false, 0.0, false, None);
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
            "--simulate" => simulate = true,
            "--film" => film = true,
            "--ev" => ev = value()?.parse().map_err(|_| "bad --ev")?,
            "--ir" => infrared = true,
            "--raw" => raw = Some(value()?),
            "--bits" => {
                bit_depth = match value()?.as_str() {
                    "8" => BitDepth::Eight,
                    "16" => BitDepth::Sixteen,
                    _ => return Err("--bits is 8 or 16".into()),
                }
            }
            "--calibration" => {
                calibration = match value()?.as_str() {
                    "live" => Calibration::Live,
                    "recorded" => Calibration::Recorded,
                    "flat" => Calibration::Flat,
                    _ => return Err("--calibration is live, recorded or flat".into()),
                }
            }
            _ => return Err(usage.into()),
        }
    }
    if bit_depth == BitDepth::Sixteen && !templates.is_empty() {
        return Err("--template scans are 8-bit".into());
    }
    if raw.is_some() && !film {
        return Err("--raw needs --film".into());
    }
    if infrared && !film {
        return Err("--ir needs --film".into());
    }
    if film && !templates.is_empty() {
        return Err("--template is for the flatbed".into());
    }
    if offline && templates.is_empty() {
        return Err("--offline needs --template".into());
    }
    Ok(Args {
        templates,
        dpi: dpi.ok_or(usage)?,
        area: area.ok_or(usage)?,
        out: out.ok_or(usage)?,
        path,
        offline,
        simulate,
        calibration,
        bit_depth,
        film,
        ev,
        infrared,
        raw,
    })
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if args.film {
        return run_film(&args);
    }
    if args.templates.is_empty() {
        return run_session(&args);
    }
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
        let mut dev = open(args.path.clone())?;
        Template::run(&script, dev.as_mut(), POLL_DELAY, args.calibration)
            .map_err(|e| e.to_string())?
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

fn run_session(args: &Args) -> Result<(), String> {
    let session =
        FlatbedSession::new(&args.area, args.dpi, args.bit_depth).map_err(|e| e.to_string())?;
    let plan = &session.plan;
    println!(
        "plan: {}x{} px, {} lines at {} dpi, {:?} move of {} steps, scan {} steps, crop x {}",
        plan.width,
        plan.height,
        plan.lines,
        plan.hw_dpi,
        plan.move_profile,
        plan.move_steps,
        plan.scan_steps,
        session.layout.crop_x
    );
    let mut dev: Box<dyn Transport> = if args.simulate {
        Box::new(SimulatedScanner::default())
    } else {
        open(args.path.clone())?
    };
    let raw = session
        .run(dev.as_mut(), POLL_DELAY)
        .map_err(|e| e.to_string())?;
    let (width, height) = (plan.width, plan.height);
    let saved = match args.bit_depth {
        BitDepth::Eight => decode::flatbed_rgb8(&raw, session.layout, plan).save(&args.out),
        BitDepth::Sixteen => decode::flatbed_rgb16(&raw, session.layout, plan).save(&args.out),
    };
    saved.map_err(|e| format!("{}: {e}", args.out))?;
    println!("saved {width}x{height} to {}", args.out);
    Ok(())
}

fn run_film(args: &Args) -> Result<(), String> {
    let session = if args.infrared {
        FilmSession::infrared(&args.area, args.dpi)
    } else {
        FilmSession::new(&args.area, args.dpi).map(|s| s.with_exposure(args.ev))
    }
    .map_err(|e| e.to_string())?;
    let plan = &session.plan;
    println!(
        "plan: {}x{} px, {} lines of {} px at {} x {} dpi, x {:?}, {:?}, scan {} steps",
        plan.output.width,
        plan.output.height,
        plan.lines,
        plan.read_width,
        plan.output.hw_x_dpi,
        plan.output.hw_dpi,
        plan.x_units,
        plan.carriage,
        plan.scan_steps
    );
    let mut dev: Box<dyn Transport> = if args.simulate {
        Box::new(SimulatedScanner::default())
    } else {
        open(args.path.clone())?
    };
    use std::io::Write;
    let mut raw = match &args.raw {
        Some(path) => Some((
            path,
            std::io::BufWriter::new(
                std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?,
            ),
        )),
        None => None,
    };
    let (mut stream, mut written, mut write_error) = (session.decoder(), 0, None);
    session
        .run_into(dev.as_mut(), POLL_DELAY, &mut NoProgress, &mut |block| {
            stream.push(block);
            if let Some((_, file)) = &mut raw
                && write_error.is_none()
            {
                write_error = file.write_all(block).err();
                written += block.len();
            }
        })
        .map_err(|e| e.to_string())?;
    if let Some((path, mut file)) = raw {
        write_error
            .map_or_else(|| file.flush(), Err)
            .map_err(|e| format!("{path}: {e}"))?;
        println!("saved the {written} byte read to {path}");
    }
    let saved = if args.infrared {
        session
            .align_infrared_plane(stream, &args.area)
            .save(&args.out)
    } else {
        let image = scan_core::image::DynamicImage::ImageRgb16(stream.into_rgb16());
        match args.bit_depth {
            BitDepth::Eight => image.to_rgb8().save(&args.out),
            BitDepth::Sixteen => image.save(&args.out),
        }
    };
    saved.map_err(|e| format!("{}: {e}", args.out))?;
    println!(
        "saved {}x{} to {}",
        plan.output.width, plan.output.height, args.out
    );
    Ok(())
}

#[cfg(windows)]
fn open(path: Option<String>) -> Result<Box<dyn Transport>, String> {
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
    Ok(Box::new(dev))
}

#[cfg(not(windows))]
fn open(_: Option<String>) -> Result<Box<dyn Transport>, String> {
    Err("scanning currently supports only Windows (usbscan.sys); use --simulate".into())
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
