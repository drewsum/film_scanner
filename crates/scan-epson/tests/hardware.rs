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

use scan_core::image::DynamicImage;
use scan_core::{
    BitDepth, Exposure, NoProgress, ScanArea, ScanRequest, ScanSource, ScannerBackend,
    ScannerDevice,
};
use scan_epson::EpsonBackend;

fn open() -> Box<dyn ScannerDevice> {
    let _ = env_logger::builder().is_test(true).try_init();
    let devices = EpsonBackend.list_devices().expect("list V500s");
    let info = devices.first().expect("no V500 attached");
    EpsonBackend.open(info).expect("open the V500")
}

fn scan(
    dev: &mut dyn ScannerDevice,
    dpi: u32,
    bit_depth: BitDepth,
    area: ScanArea,
    name: &str,
) -> DynamicImage {
    let request = ScanRequest {
        bit_depth,
        area: Some(area),
        ..ScanRequest::new(ScanSource::Flatbed, dpi)
    };
    let result = dev.scan(&request, &mut NoProgress).expect("scan");
    assert_eq!(result.dimensions(), area.pixel_size(dpi));
    let primary = result.primary().clone();
    let image = primary.to_rgb8();
    // The lamp was on and the read isn't blank.
    let mean = image.pixels().map(|p| u64::from(p.0[1])).sum::<u64>()
        / u64::from(image.width() * image.height());
    println!("{name}: {:?} px, mean green {mean}", result.dimensions());
    assert!(mean > 5, "{name} is black");
    save(&primary, name);
    primary
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
    scan(dev.as_mut(), 75, BitDepth::Eight, caps.max_area, "full-75");
    let area = ScanArea {
        x_mm: 20.0,
        y_mm: 50.0,
        width_mm: 60.0,
        height_mm: 40.0,
    };
    scan(dev.as_mut(), 300, BitDepth::Eight, area, "area-300");
}

/// Needs the 35 mm film holder in, with film.
#[test]
#[ignore = "requires a V500 with the film holder, and administrator rights"]
fn scans_film() {
    let mut dev = open();
    let caps = dev
        .capabilities()
        .source(ScanSource::Transparency)
        .unwrap()
        .clone();
    // A preview of the whole holder, then a frame-sized area in two exposures.
    let request = ScanRequest {
        bit_depth: BitDepth::Eight,
        ..ScanRequest::new(ScanSource::Transparency, 100)
    };
    let result = dev.scan(&request, &mut NoProgress).expect("film preview");
    assert_eq!(result.dimensions(), caps.max_area.pixel_size(100));
    save(result.primary(), "film-preview-100");

    let area = ScanArea {
        x_mm: 3.0,
        y_mm: 40.0,
        width_mm: 24.0,
        height_mm: 36.0,
    };
    let request = ScanRequest {
        area: Some(area),
        exposures: vec![Exposure::default(), Exposure { ev: 1.0 }],
        ..ScanRequest::new(ScanSource::Transparency, 800)
    };
    let result = dev.scan(&request, &mut NoProgress).expect("film scan");
    assert_eq!(result.dimensions(), area.pixel_size(800));
    let [normal, more] = [0, 1].map(|i| result.passes[i].image.to_rgb16());
    // The film lamp lit the frame, and the second pass is about a stop brighter (linear ×2,
    // so ×2^(1/1.8) through the tone curve) where it isn't clipped.
    let mean = |img: &scan_core::image::ImageBuffer<scan_core::image::Rgb<u16>, Vec<u16>>| {
        img.pixels().map(|p| f64::from(p.0[1])).sum::<f64>() / f64::from(img.width() * img.height())
    };
    let ratio = mean(&more) / mean(&normal);
    println!(
        "film: mean green {:.0}, +1 EV ratio {ratio:.2}",
        mean(&normal)
    );
    assert!(mean(&normal) > 2000.0, "film read is black");
    assert!((1.25..1.5).contains(&ratio), "+1 EV ratio {ratio:.2}");
    save(&result.passes[0].image, "film-800");
}

fn save(image: &DynamicImage, name: &str) {
    if let Ok(dir) = std::env::var("FILM_SCANNER_TEST_OUT") {
        image
            .save(std::path::Path::new(&dir).join(format!("{name}.png")))
            .expect("save");
    }
}

#[test]
#[ignore = "requires a V500 and administrator rights"]
fn sixteen_bit_matches_eight_bit() {
    let mut dev = open();
    let area = ScanArea {
        x_mm: 20.0,
        y_mm: 50.0,
        width_mm: 60.0,
        height_mm: 40.0,
    };
    let eight = scan(dev.as_mut(), 150, BitDepth::Eight, area, "area-150-8bit");
    let sixteen = scan(dev.as_mut(), 150, BitDepth::Sixteen, area, "area-150-16bit");
    let DynamicImage::ImageRgb16(sixteen) = sixteen else {
        panic!("16-bit scan returned {:?}", sixteen.color());
    };
    // The same tone curve: the 16-bit scan, rounded to 8 bits, is close to the 8-bit one.
    let eight = eight.to_rgb8();
    let (mut diff, mut fine) = (0.0, 0u64);
    for (a, b) in eight.pixels().zip(sixteen.pixels()) {
        for c in 0..3 {
            diff += (f64::from(a.0[c]) - f64::from(b.0[c]) / 257.0).abs();
            // Levels between the 8-bit steps.
            fine += u64::from(b.0[c] % 257 != 0);
        }
    }
    let n = f64::from(eight.width() * eight.height() * 3);
    println!(
        "16 vs 8 bit: mean abs diff {:.2}, {:.0} % between 8-bit levels",
        diff / n,
        100.0 * fine as f64 / n
    );
    assert!(diff / n < 4.0, "mean abs diff {:.2}", diff / n);
    assert!(fine as f64 / n > 0.5);
}
