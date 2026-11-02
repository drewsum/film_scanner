//! Scans through libsane with SANE's own `test` driver, which simulates a flatbed with a
//! document feeder. `tests/sane.d` enables it and sets it up.
//!
//! Skipped when libsane isn't installed, unless `SCAN_SANE_REQUIRE_TEST=1` (as on Linux
//! and macOS CI).

use scan_core::{
    BitDepth, ColorMode, NoProgress, ScanArea, ScanRequest, ScanSource, ScannerBackend,
};
use scan_sane::SaneBackend;

#[test]
fn scans_with_the_sane_test_driver() {
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/sane.d");
    // SAFETY: this is the only test in its binary, and no other thread reads the
    // environment; libsane reads it on first use, below.
    unsafe { std::env::set_var("SANE_CONFIG_DIR", config) };

    let devices = SaneBackend.list_devices().unwrap();
    let Some(info) = devices.iter().find(|d| d.id.0.starts_with("test")) else {
        assert!(
            std::env::var("SCAN_SANE_REQUIRE_TEST").as_deref() != Ok("1"),
            "SANE's test driver not found; devices: {devices:?}"
        );
        eprintln!("skipped: libsane isn't installed");
        return;
    };

    let mut device = SaneBackend.open(info).unwrap();
    let caps = device.capabilities().clone();
    assert_eq!(caps.sources.len(), 1, "the feeder is skipped: {caps:?}");
    let flatbed = caps.source(ScanSource::Flatbed).unwrap();
    assert!(flatbed.resolutions.contains(&100), "{flatbed:?}");
    assert_eq!(flatbed.bit_depths, [BitDepth::Eight, BitDepth::Sixteen]);
    assert_eq!(flatbed.color_modes, [ColorMode::Color, ColorMode::Gray]);
    assert!(flatbed.max_area.width_mm > 100.0 && flatbed.max_area.height_mm > 100.0);

    let area = ScanArea {
        x_mm: 10.0,
        y_mm: 20.0,
        width_mm: 180.4,
        height_mm: 150.0,
    };
    for (bit_depth, color_mode) in [
        (BitDepth::Sixteen, ColorMode::Color),
        (BitDepth::Eight, ColorMode::Color),
        (BitDepth::Sixteen, ColorMode::Gray),
    ] {
        let request = ScanRequest {
            bit_depth,
            color_mode,
            area: Some(area),
            ..ScanRequest::new(ScanSource::Flatbed, 100)
        };
        let result = device.scan(&request, &mut NoProgress).unwrap();
        // The driver rounds the area to whole millimetres, and says so.
        assert_eq!(
            result.area,
            ScanArea {
                width_mm: 180.0,
                height_mm: 150.0,
                ..area
            }
        );
        let (w, h) = result.dimensions();
        let (ew, eh) = result.area.pixel_size(100);
        assert!(
            w.abs_diff(ew) <= 1 && h.abs_diff(eh) <= 1,
            "{w}x{h}, expected {ew}x{eh}"
        );
        let image = result.primary();
        assert_eq!(image.color().has_color(), color_mode == ColorMode::Color);
        assert_eq!(
            image.color().bits_per_pixel() / image.color().channel_count() as u16,
            bit_depth.bits() as u16
        );
        // At 16 bits the colour pattern has red, green and blue stripes from left to
        // right. The driver sends them as blue, green and red frames, so channels put in
        // the wrong order would move the stripes.
        if color_mode == ColorMode::Color && bit_depth == BitDepth::Sixteen {
            let rgb = image.to_rgb16();
            let centre = |c: usize| {
                let xs: Vec<u32> = rgb
                    .enumerate_pixels()
                    .filter(|(_, _, p)| p[c] > 0 && (0..3).all(|o| o == c || p[o] == 0))
                    .map(|(x, _, _)| x)
                    .collect();
                assert!(!xs.is_empty(), "no pure {c} pixels");
                xs.iter().sum::<u32>() / xs.len() as u32
            };
            let (red, green, blue) = (centre(0), centre(1), centre(2));
            assert!(red < green && green < blue, "{red}, {green}, {blue}");
        }
    }
}
