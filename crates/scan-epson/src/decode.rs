//! Raw scan data to an image.

use scan_core::Gray16Image;
use scan_core::image::{ImageBuffer, Luma, Pixel, Rgb, RgbImage};

use crate::geometry::{FlatbedPlan, skew_lines};
use crate::tables;
use crate::template::RawLayout;

/// A 16-bit RGB image.
pub type Rgb16Image = ImageBuffer<Rgb<u16>, Vec<u16>>;

/// What a read becomes: the output size at the requested resolution, made from raw data at
/// the hardware resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resample {
    pub dpi: u32,
    pub hw_dpi: u32,
    pub width: u32,
    pub height: u32,
}

impl From<&FlatbedPlan> for Resample {
    fn from(plan: &FlatbedPlan) -> Self {
        Self {
            dpi: plan.dpi,
            hw_dpi: plan.hw_dpi,
            width: plan.width,
            height: plan.height,
        }
    }
}

/// Decode an 8-bit pixel-interleaved RGB read into the planned image: realign the color
/// rows, crop to the area and box-filter down to the requested resolution.
///
/// Output row `j` takes blue from raw line `j`, green from `j + skew` and red from
/// `j + 2 × skew` (at hardware resolution), as Epson's driver does.
pub fn flatbed_rgb8(raw: &[u8], layout: RawLayout, plan: &FlatbedPlan) -> RgbImage {
    resample(
        layout,
        plan.into(),
        |i| raw.get(i).copied().unwrap_or(0).into(),
        |mean| mean.round() as u8,
    )
}

/// Decode a 16-bit (little-endian) linear read like [`flatbed_rgb8`], then apply the tone
/// curve the scanner applies to 8-bit reads, so both depths look alike.
pub fn flatbed_rgb16(raw: &[u8], layout: RawLayout, plan: &FlatbedPlan) -> Rgb16Image {
    rgb16(raw, layout, plan.into())
}

/// Decode a 16-bit linear read (flatbed or film) into `out`, with the tone curve applied.
pub fn rgb16(raw: &[u8], layout: RawLayout, out: Resample) -> Rgb16Image {
    let curve: Vec<u16> = (0..=u16::MAX)
        .map(|v| (65535.0 * tables::tone(f64::from(v) / 65535.0)).round() as u16)
        .collect();
    resample(
        layout,
        out,
        |i| {
            raw.get(2 * i..2 * i + 2)
                .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]).into())
        },
        |mean| curve[mean.round() as usize],
    )
}

/// Decode an infrared read: all three CCD rows see the infrared, so realign them like color
/// and average them into one linear plane.
pub fn infrared(raw: &[u8], layout: RawLayout, out: Resample) -> Gray16Image {
    let rgb: Rgb16Image = resample(
        layout,
        out,
        |i| {
            raw.get(2 * i..2 * i + 2)
                .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]).into())
        },
        |mean| mean.round() as u16,
    );
    ImageBuffer::from_fn(out.width, out.height, |x, y| {
        let p = rgb.get_pixel(x, y).0;
        Luma([((u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]) + 1) / 3) as u16])
    })
}

/// Realign, crop and box-filter the raw samples (`sample(i)` is the `i`th, pixel-interleaved
/// RGB at hardware resolution; 0 past the end) into the planned image, mapping each channel's
/// mean with `out`.
fn resample<S>(
    layout: RawLayout,
    plan: Resample,
    sample: impl Fn(usize) -> u32,
    out: impl Fn(f64) -> S,
) -> ImageBuffer<Rgb<S>, Vec<S>>
where
    Rgb<S>: Pixel<Subpixel = S>,
{
    let width = layout.width as usize;
    let skew = skew_lines(plan.hw_dpi) as usize;
    let scale = f64::from(plan.hw_dpi) / f64::from(plan.dpi);
    // Raw lines/columns covered by output index `i`.
    let span = |i: u32| {
        let a = (f64::from(i) * scale) as usize;
        let b = ((f64::from(i + 1) * scale) as usize).max(a + 1);
        a..b
    };
    ImageBuffer::from_fn(plan.width, plan.height, |x, y| {
        let cols = span(x);
        let rows = span(y);
        let n = (cols.len() * rows.len()) as f64;
        let px = [(0, 2 * skew), (1, skew), (2, 0)].map(|(ch, offset)| {
            let mut sum = 0u64;
            for line in rows.clone() {
                for col in cols.clone() {
                    let i = ((line + offset) * width + col + layout.crop_x as usize) * 3 + ch;
                    sum += u64::from(sample(i));
                }
            }
            out(sum as f64 / n)
        });
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

        // The same read at 16 bits, scaled by 256, gets the tone curve.
        let raw16: Vec<u8> = raw
            .iter()
            .flat_map(|&v| (u16::from(v) << 8).to_le_bytes())
            .collect();
        let img16 = flatbed_rgb16(&raw16, layout, &plan);
        for (c, mean) in [106.5, 56.5, 6.5].into_iter().enumerate() {
            let want = 65535.0 * tables::tone(mean * 256.0 / 65535.0);
            assert_eq!(f64::from(img16.get_pixel(0, 0).0[c]), want.round());
        }

        // As infrared: the realigned rows averaged, linear.
        let ir = infrared(&raw16, layout, (&plan).into());
        assert_eq!(
            ir.get_pixel(0, 0).0,
            [((106.5 + 56.5 + 6.5) / 3.0 * 256.0) as u16]
        );
    }
}
