//! Raw flatbed data to an image.

use scan_core::image::{Rgb, RgbImage};

use crate::geometry::{FlatbedPlan, skew_lines};
use crate::template::RawLayout;

/// Decode an 8-bit pixel-interleaved RGB read into the planned image: realign the colour
/// rows, crop to the area and box-filter down to the requested resolution.
///
/// Output row `j` takes blue from raw line `j`, green from `j + skew` and red from
/// `j + 2 × skew` (at hardware resolution), as Epson's driver does.
pub fn flatbed_rgb8(raw: &[u8], layout: RawLayout, plan: &FlatbedPlan) -> RgbImage {
    let width = layout.width as usize;
    let skew = skew_lines(plan.hw_dpi) as usize;
    let scale = f64::from(plan.hw_dpi) / f64::from(plan.dpi);
    // Raw lines/columns covered by output index `i`.
    let span = |i: u32| {
        let a = (f64::from(i) * scale) as usize;
        let b = ((f64::from(i + 1) * scale) as usize).max(a + 1);
        a..b
    };
    let sample = |line: usize, col: usize, ch: usize| -> u32 {
        raw.get((line * width + col) * 3 + ch)
            .copied()
            .unwrap_or(0)
            .into()
    };
    RgbImage::from_fn(plan.width, plan.height, |x, y| {
        let cols = span(x);
        let rows = span(y);
        let n = (cols.len() * rows.len()) as u32;
        let mut px = [0u8; 3];
        for (ch, offset) in [(0, 2 * skew), (1, skew), (2, 0)] {
            let mut sum = 0;
            for line in rows.clone() {
                for col in cols.clone() {
                    sum += sample(line + offset, col + layout.crop_x as usize, ch);
                }
            }
            px[ch] = ((sum + n / 2) / n) as u8;
        }
        Rgb(px)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan_core::ScanArea;

    #[test]
    fn realigns_crops_and_downsamples() {
        // 200 dpi hardware, 100 dpi output, 2x2 px: skew is 5 lines.
        let area = ScanArea {
            x_mm: 0.0,
            y_mm: 0.0,
            width_mm: 0.508,
            height_mm: 0.508,
        };
        let plan = FlatbedPlan::new(&area, 100, 0).unwrap();
        assert_eq!((plan.width, plan.height, plan.lines), (2, 2, 14));
        let layout = RawLayout {
            lines: plan.lines,
            width: 6,
            crop_x: 1,
        };
        // Each channel's value is its raw line number (blue) or line - skew offsets.
        let mut raw = vec![0u8; 14 * 6 * 3];
        for line in 0..14 {
            for col in 0..6 {
                let i = (line * 6 + col) * 3;
                raw[i] = (line * 10 + col) as u8; // R
                raw[i + 1] = (line * 10 + col) as u8; // G
                raw[i + 2] = (line * 10 + col) as u8; // B
            }
        }
        let img = flatbed_rgb8(&raw, layout, &plan);
        // Pixel (0,0): raw cols 1..3, rows 0..2; R +10 lines, G +5, B +0.
        // Mean of line*10+col over lines {l, l+1} and cols {1, 2} = 10l + 5 + 1.5.
        assert_eq!(img.get_pixel(0, 0).0, [107, 57, 7]);
        assert_eq!(img.get_pixel(1, 1).0, [129, 79, 29]);
    }
}
