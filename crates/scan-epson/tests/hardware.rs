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

#[test]
#[ignore = "requires a V500 with the film holder, and administrator rights"]
fn scans_film_infrared() {
    let mut dev = open();
    let area = ScanArea {
        x_mm: 5.0,
        y_mm: 40.0,
        width_mm: 24.0,
        height_mm: 36.0,
    };
    let request = ScanRequest {
        area: Some(area),
        infrared: true,
        ..ScanRequest::new(ScanSource::Transparency, 800)
    };
    let result = dev
        .scan(&request, &mut NoProgress)
        .expect("infrared film scan");
    let ir = result.infrared.as_ref().expect("infrared plane");
    assert_eq!(ir.dimensions(), result.dimensions());
    let colour = result.primary().to_luma16();
    save(result.primary(), "film-ir-colour");
    save(&DynamicImage::ImageLuma16(ir.clone()), "film-ir");

    // Film dyes are nearly transparent to infrared: the frame reads bright.
    let (w, h) = ir.dimensions();
    let frame = (w / 8..w * 3 / 4).flat_map(|x| (h / 8..h * 7 / 8).map(move |y| (x, y)));
    let n = f64::from((w * 3 / 4 - w / 8) * (h * 7 / 8 - h / 8));
    let mean = frame
        .map(|(x, y)| f64::from(ir.get_pixel(x, y).0[0]))
        .sum::<f64>()
        / n;
    println!("infrared: frame mean {mean:.0}");
    assert!(mean > 30000.0, "infrared frame mean {mean:.0}");

    // The planes line up: the detail (each pixel less its neighbourhood) of both correlates
    // best within a pixel of no shift.
    let detail = |at: &dyn Fn(u32, u32) -> f64, x: u32, y: u32| {
        let mut s = 0.0;
        for dy in 0..5 {
            for dx in 0..5 {
                s += at(x + dx - 2, y + dy - 2);
            }
        }
        at(x, y) - s / 25.0
    };
    let ir_at = |x: u32, y: u32| f64::from(ir.get_pixel(x, y).0[0]);
    let colour_at = |x: u32, y: u32| f64::from(colour.get_pixel(x, y).0[0]);
    let best = (-4i32..=4)
        .flat_map(|dy| (-4i32..=4).map(move |dx| (dx, dy)))
        .map(|(dx, dy)| {
            let mut c = 0.0;
            for y in (h / 8..h * 7 / 8).step_by(2) {
                for x in (w / 8..w * 3 / 4).step_by(2) {
                    let (sx, sy) = ((x as i32 + dx) as u32, (y as i32 + dy) as u32);
                    c += detail(&ir_at, sx, sy) * detail(&colour_at, x, y);
                }
            }
            ((dx, dy), c)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap()
        .0;
    println!("infrared: best shift {best:?}");
    assert!(
        best.0.abs() <= 1 && best.1.abs() <= 1,
        "infrared shifted {best:?}"
    );
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
