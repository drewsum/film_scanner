//! Tests against a live Perfection V500. Ignored by default (and in CI); run locally, as
//! administrator, with the scanner attached:
//!
//! ```text
//! cargo test -p scan-epson --test hardware -- --ignored --nocapture --test-threads 1
//! ```
//!
//! The WIA service is stopped for each scan and started again after it. Set
//! `FILM_SCANNER_TEST_OUT` to a directory to save the scans as PNG.
#![cfg(windows)]

use scan_core::{
    BitDepth, NoProgress, ScanArea, ScanRequest, ScanSource, ScannerBackend, ScannerDevice,
};
use scan_epson::EpsonBackend;

fn open() -> Box<dyn ScannerDevice> {
    let _ = env_logger::builder().is_test(true).try_init();
    let devices = EpsonBackend.list_devices().expect("list V500s");
    let info = devices.first().expect("no V500 attached");
    EpsonBackend.open(info).expect("open the V500")
}

fn scan(dev: &mut dyn ScannerDevice, dpi: u32, area: ScanArea, name: &str) {
    let request = ScanRequest {
        bit_depth: BitDepth::Eight,
        area: Some(area),
        ..ScanRequest::new(ScanSource::Flatbed, dpi)
    };
    let result = dev.scan(&request, &mut NoProgress).expect("scan");
    assert_eq!(result.dimensions(), area.pixel_size(dpi));
    let image = result.primary().to_rgb8();
    // The lamp was on and the read isn't blank.
    let mean = image.pixels().map(|p| u64::from(p.0[1])).sum::<u64>()
        / u64::from(image.width() * image.height());
    println!("{name}: {:?} px, mean green {mean}", result.dimensions());
    assert!(mean > 5, "{name} is black");
    if let Ok(dir) = std::env::var("FILM_SCANNER_TEST_OUT") {
        image
            .save(std::path::Path::new(&dir).join(format!("{name}.png")))
            .expect("save");
    }
}

#[test]
#[ignore = "requires a V500 and administrator rights"]
fn scans_flatbed_areas() {
    let mut dev = open();
    let caps = dev
        .capabilities()
        .source(ScanSource::Flatbed)
        .unwrap()
        .clone();
    assert_eq!(caps.max_area.width_mm, 215.9);
    scan(dev.as_mut(), 75, caps.max_area, "full-75");
    let area = ScanArea {
        x_mm: 20.0,
        y_mm: 50.0,
        width_mm: 60.0,
        height_mm: 40.0,
    };
    scan(dev.as_mut(), 300, area, "area-300");
}
