//! Tests against a real WIA scanner. Ignored by default (and in CI); run locally with a
//! scanner attached:
//!
//! ```text
//! cargo test -p scan-wia -- --ignored --nocapture
//! ```
//!
//! `FILM_SCANNER_TEST_DEVICE` selects a device by substring of its model name (default: the
//! first scanner found); `FILM_SCANNER_TEST_SOURCE=film` scans from the transparency unit.
#![cfg(windows)]

use scan_core::{BitDepth, NoProgress, ScanArea, ScanRequest, ScanSource, ScannerBackend};
use scan_wia::WiaBackend;

fn pick_device() -> scan_core::DeviceInfo {
    let _ = env_logger::builder().is_test(true).try_init();
    let devices = WiaBackend.list_devices().expect("list WIA devices");
    println!("WIA devices: {devices:#?}");
    let wanted = std::env::var("FILM_SCANNER_TEST_DEVICE").unwrap_or_default();
    devices
        .into_iter()
        .find(|d| d.model.contains(&wanted))
        .expect("no matching WIA scanner attached")
}

#[test]
#[ignore = "requires a scanner attached via WIA"]
fn lists_and_opens_scanner() {
    let info = pick_device();
    let dev = WiaBackend.open(&info).expect("open device");
    let caps = dev.capabilities();
    println!("capabilities: {caps:#?}");
    assert!(!caps.sources.is_empty());
    for src in &caps.sources {
        assert!(!src.resolutions.is_empty());
        assert!(src.max_area.width_mm > 10.0 && src.max_area.height_mm > 10.0);
    }
}

#[test]
#[ignore = "requires a scanner attached via WIA"]
fn low_resolution_preview_scan() {
    let source = match std::env::var("FILM_SCANNER_TEST_SOURCE").as_deref() {
        Ok("film") => ScanSource::Transparency,
        _ => ScanSource::Flatbed,
    };
    let info = pick_device();
    let mut dev = WiaBackend.open(&info).expect("open device");
    let src = dev
        .capabilities()
        .source(source)
        .expect("source available")
        .clone();
    let dpi = src.preview_resolution(75).unwrap();

    let mut req = ScanRequest::new(source, dpi);
    req.bit_depth = BitDepth::Eight;
    req.area = Some(ScanArea::full(50.0, 50.0));
    let result = dev.scan(&req, &mut NoProgress).expect("scan");

    let (w, h) = result.dimensions();
    println!("scanned {w}x{h} at {dpi} dpi");
    let expected = ScanArea::full(50.0, 50.0).pixel_size(dpi);
    assert!(
        w.abs_diff(expected.0) <= 2 && h.abs_diff(expected.1) <= 2,
        "{w}x{h} vs {expected:?}"
    );

    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("wia_preview.png");
    result.primary().save(&out).unwrap();
    println!("saved {}", out.display());
}

#[test]
#[ignore = "requires a scanner attached via WIA"]
fn sixteen_bit_scan() {
    let info = pick_device();
    let mut dev = WiaBackend.open(&info).expect("open device");
    let src = dev
        .capabilities()
        .source(ScanSource::Flatbed)
        .expect("flatbed")
        .clone();
    if !src.bit_depths.contains(&BitDepth::Sixteen) {
        println!("device does not offer 16-bit; skipping");
        return;
    }
    let mut req = ScanRequest::new(ScanSource::Flatbed, src.preview_resolution(75).unwrap());
    req.area = Some(ScanArea::full(25.0, 25.0));
    let result = dev.scan(&req, &mut NoProgress).expect("16-bit scan");
    assert!(matches!(
        result.primary(),
        image::DynamicImage::ImageRgb16(_)
    ));
}

/// A scan with settings from the environment, for capturing the USB traffic of known
/// requests while reverse-engineering a scanner:
///
/// - `FILM_SCANNER_TEST_DPI` (default 75)
/// - `FILM_SCANNER_TEST_AREA` as `x,y,width,height` in mm (default `0,0,50,50`)
/// - `FILM_SCANNER_TEST_GRAY=1` for greyscale
#[test]
#[ignore = "requires a scanner attached via WIA"]
fn configured_scan() {
    let env = |k: &str| std::env::var(k).ok();
    let dpi: u32 = env("FILM_SCANNER_TEST_DPI").map_or(75, |v| v.parse().expect("dpi"));
    let area: Vec<f32> = env("FILM_SCANNER_TEST_AREA")
        .unwrap_or_else(|| "0,0,50,50".into())
        .split(',')
        .map(|v| v.trim().parse().expect("area in mm"))
        .collect();
    let [x_mm, y_mm, width_mm, height_mm] = area[..] else {
        panic!("FILM_SCANNER_TEST_AREA needs four values");
    };
    let info = pick_device();
    let mut dev = WiaBackend.open(&info).expect("open device");
    let mut req = ScanRequest::new(ScanSource::Flatbed, dpi);
    req.bit_depth = BitDepth::Eight;
    if env("FILM_SCANNER_TEST_GRAY").is_some_and(|v| v == "1") {
        req.color_mode = scan_core::ColorMode::Gray;
    }
    req.area = Some(ScanArea {
        x_mm,
        y_mm,
        width_mm,
        height_mm,
    });
    let result = dev.scan(&req, &mut NoProgress).expect("scan");
    let (w, h) = result.dimensions();
    println!(
        "scanned {w}x{h} at {dpi} dpi, area {area:?}, {:?}",
        req.color_mode
    );
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("wia_configured.png");
    result.primary().save(&out).unwrap();
}

/// Regression: the full bed at 75 dpi is a fractional pixel count (637.5 px for 8.5in), which
/// the driver rejects unless truncated.
#[test]
#[ignore = "requires a scanner attached via WIA"]
fn full_bed_preview_scan() {
    let info = pick_device();
    let mut dev = WiaBackend.open(&info).expect("open device");
    let src = dev
        .capabilities()
        .source(ScanSource::Flatbed)
        .expect("flatbed")
        .clone();
    let mut req = ScanRequest::new(ScanSource::Flatbed, src.preview_resolution(75).unwrap());
    req.bit_depth = BitDepth::Eight;
    let result = dev.scan(&req, &mut NoProgress).expect("full-bed scan");
    let (w, h) = result.dimensions();
    let expected = src.max_area.pixel_size(req.dpi);
    assert!(
        w.abs_diff(expected.0) <= 1 && h.abs_diff(expected.1) <= 1,
        "{w}x{h} vs {expected:?}"
    );
}
