//! Inverting scans of color negatives with a film's profile (see the `scan-film` crate),
//! image by image.
//!
//! [`invert`] turns a scan into a positive through an [`Inverter`]. The film base it needs
//! comes from frame detection ([`crate::FrameDetection::base`]), or failing that from
//! [`estimate_base`]. [`frame_levels`] measures frames for [`scan_film::auto_printing`].

use std::borrow::Cow;

use scan_core::image::{DynamicImage, GenericImageView, ImageBuffer, Rgb};
use scan_core::{MM_PER_INCH, ScanArea};
use scan_film::Inverter;

use crate::frames::FrameRect;

/// A 16-bit RGB image.
pub type Rgb16Image = ImageBuffer<Rgb<u16>, Vec<u16>>;

/// The positive of `image`, a scan of a negative whose values encode light with `gamma`
/// (see `scan_core::ScanResult`): 16-bit sRGB.
pub fn invert(image: &DynamicImage, gamma: f32, inverter: &Inverter) -> Rgb16Image {
    let rgb: Cow<Rgb16Image> = match image {
        DynamicImage::ImageRgb16(i) => Cow::Borrowed(i),
        other => Cow::Owned(other.to_rgb16()),
    };
    let (w, h) = rgb.dimensions();
    let decode = decode_table(gamma);
    let scene: [Vec<f32>; 3] =
        std::array::from_fn(|c| decode.iter().map(|&l| inverter.scene(c, l)).collect());
    let mut out = vec![0u16; rgb.as_raw().len()];
    let src = rgb.as_raw();
    if inverter.separable() {
        // Each channel on its own: one table from input to output per channel.
        let tables: [Vec<u16>; 3] = std::array::from_fn(|c| {
            scene[c]
                .iter()
                .map(|&e| to_u16(inverter.display_channel(e)))
                .collect()
        });
        in_parallel(src, &mut out, |i, o| {
            for (p, q) in i.as_chunks::<3>().0.iter().zip(o.as_chunks_mut::<3>().0) {
                for c in 0..3 {
                    q[c] = tables[c][usize::from(p[c])];
                }
            }
        });
    } else {
        in_parallel(src, &mut out, |i, o| {
            for (p, q) in i.as_chunks::<3>().0.iter().zip(o.as_chunks_mut::<3>().0) {
                let e = std::array::from_fn(|c| scene[c][usize::from(p[c])]);
                let d = inverter.display(e);
                for c in 0..3 {
                    q[c] = to_u16(d[c]);
                }
            }
        });
    }
    ImageBuffer::from_raw(w, h, out).expect("same size as the scan")
}

/// Linear light (0–1 of full scale) of every 16-bit value.
pub(crate) fn decode_table(gamma: f32) -> Vec<f32> {
    let gamma = if gamma > 0.0 { gamma } else { 1.0 };
    (0..=65535u32)
        .map(|v| (v as f32 / 65535.0).powf(gamma))
        .collect()
}

pub(crate) fn to_u16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0).round() as u16
}

/// Run `f` over matching row bands of `src` and `dst` (RGB samples) on every core.
pub(crate) fn in_parallel(src: &[u16], dst: &mut [u16], f: impl Fn(&[u16], &mut [u16]) + Sync) {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    // Whole pixels per band, and not so small that threads cost more than they save.
    let band = (src.len().div_ceil(threads * 3) * 3).max(3 * 65536);
    std::thread::scope(|s| {
        for (i, o) in src.chunks(band).zip(dst.chunks_mut(band)) {
            let f = &f;
            s.spawn(move || f(i, o));
        }
    });
}

/// Each frame's mean log10 scene light per channel through `neutral` (an inverter with no
/// printing exposure or balance), as [`scan_film::auto_printing`] takes them. `image` is
/// the scan of `area` at `dpi`, its values encoding light with `gamma`. Frames are sampled
/// about every quarter millimetre; a frame off the scan is left out.
pub fn frame_levels(
    image: &DynamicImage,
    gamma: f32,
    dpi: u32,
    area: ScanArea,
    frames: &[FrameRect],
    neutral: &Inverter,
) -> Vec<[f32; 3]> {
    let (w, h) = (image.width() as f32, image.height() as f32);
    let decode = decode_table(gamma);
    let px_per_mm = dpi as f32 / MM_PER_INCH;
    let step = 0.25f32;
    // A speck of dust or a clear scratch shouldn't count for more than a midtone's worth.
    let (lo, hi) = (-4.0f32, 1.5f32);
    let mut levels = Vec::new();
    for frame in frames {
        let (nu, nv) = (
            (frame.width_mm / step).floor() as i32,
            (frame.height_mm / step).floor() as i32,
        );
        let (mut sum, mut n) = ([0.0f64; 3], 0usize);
        for j in 0..nv {
            for i in 0..nu {
                let u = -frame.width_mm / 2.0 + (i as f32 + 0.5) * step;
                let v = -frame.height_mm / 2.0 + (j as f32 + 0.5) * step;
                let (x, y) = frame.from_local(u, v);
                let px = ((x - area.x_mm) * px_per_mm).floor();
                let py = ((y - area.y_mm) * px_per_mm).floor();
                if px < 0.0 || py < 0.0 || px >= w || py >= h {
                    continue;
                }
                let p = pixel16(image, px as u32, py as u32);
                for c in 0..3 {
                    let e = neutral.scene(c, decode[usize::from(p[c])]);
                    sum[c] += f64::from(e.max(1e-9).log10().clamp(lo, hi));
                }
                n += 1;
            }
        }
        if n > 0 {
            levels.push(sum.map(|s| (s / n as f64) as f32));
        }
    }
    levels
}

/// A guess at the light through the bare film base when frame detection found no strips:
/// the clearest film on the scan, per channel (the 99.5th percentile of pixels not
/// clipped), so clear glass and perforations, which clip, are passed over. Worse than a
/// measured base: the clearest part of an image may not be bare film.
pub fn estimate_base(image: &DynamicImage, gamma: f32) -> Option<[f32; 3]> {
    let decode = decode_table(gamma);
    let (w, h) = (image.width(), image.height());
    let stride = ((w as u64 * h as u64 / 200_000) as f64).sqrt().max(1.0) as u32;
    let mut samples: [Vec<f32>; 3] = Default::default();
    for y in (0..h).step_by(stride as usize) {
        for x in (0..w).step_by(stride as usize) {
            let p = pixel16(image, x, y);
            if p.iter().all(|&v| v < 64_000) && p.iter().any(|&v| v > 0) {
                for c in 0..3 {
                    samples[c].push(decode[usize::from(p[c])]);
                }
            }
        }
    }
    if samples[0].len() < 100 {
        return None;
    }
    Some(samples.map(|mut s| {
        let i = ((s.len() - 1) as f32 * 0.995).round() as usize;
        s.select_nth_unstable_by(i, f32::total_cmp).1.max(1e-6)
    }))
}

/// The pixel at (x, y) as 16-bit RGB, without converting the whole image.
pub(crate) fn pixel16(image: &DynamicImage, x: u32, y: u32) -> [u16; 3] {
    let wide = |v: u8| u16::from(v) * 257;
    match image {
        DynamicImage::ImageRgb16(i) => i.get_pixel(x, y).0,
        DynamicImage::ImageRgb8(i) => i.get_pixel(x, y).0.map(wide),
        DynamicImage::ImageLuma16(i) => [i.get_pixel(x, y).0[0]; 3],
        DynamicImage::ImageLuma8(i) => [wide(i.get_pixel(x, y).0[0]); 3],
        other => {
            let p = other.get_pixel(x, y).0;
            [wide(p[0]), wide(p[1]), wide(p[2])]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan_film::{FilmModel, Printing, stock};

    const GAMMA: f32 = 1.8;

    /// A scan, gamma-encoded, of a strip of negative: scene light rising from left to right
    /// in steps, with a color cast per row band.
    fn negative(
        model: &FilmModel,
        base: [f32; 3],
        scene: impl Fn(u32, u32) -> [f32; 3],
    ) -> DynamicImage {
        let img = ImageBuffer::from_fn(300, 200, |x, y| {
            let d = model.negative(scene(x, y));
            Rgb(std::array::from_fn(|c| {
                let light = base[c] * 10f32.powf(-d[c]);
                (light.powf(1.0 / GAMMA) * 65535.0).round() as u16
            }))
        });
        DynamicImage::ImageRgb16(img)
    }

    #[test]
    fn greys_come_out_grey_and_in_order() {
        let model = stock::default_stock().model();
        let base = [0.56, 0.22, 0.10];
        let grey = |x: u32, _| [0.01 * 1.6f32.powi((x / 30) as i32); 3];
        let img = negative(model, base, grey);
        for printing in [
            Printing::default(),
            Printing {
                rendering: scan_film::Rendering::Colorimetric,
                ..Printing::default()
            },
        ] {
            let out = invert(&img, GAMMA, &Inverter::new(model, base, &printing));
            let mut last = 0;
            for x in (15..300).step_by(30) {
                let p = out.get_pixel(x, 100).0;
                let spread = *p.iter().max().unwrap() - *p.iter().min().unwrap();
                // 16-bit quantisation of a 16-bit scan of dense film: within half a percent.
                assert!(spread < 330, "{x}: {p:?}");
                assert!(p[1] > last, "{x}: {p:?} after {last}");
                last = p[1];
            }
        }
    }

    #[test]
    fn eight_bit_scans_invert_too() {
        let model = stock::default_stock().model();
        let base = [0.56, 0.22, 0.10];
        let img = negative(model, base, |_, _| [0.18; 3]);
        let img = DynamicImage::ImageRgb8(img.to_rgb8());
        let out = invert(
            &img,
            GAMMA,
            &Inverter::new(model, base, &Printing::default()),
        );
        let p = out.get_pixel(10, 10).0;
        // A grey card prints at sRGB 118/255; 8-bit steps of dense film are coarse.
        for v in p {
            assert!((f32::from(v) / 257.0 - 118.0).abs() < 6.0, "{p:?}");
        }
    }

    #[test]
    fn frame_levels_measure_each_frame() {
        let model = stock::default_stock().model();
        let base = [0.56, 0.22, 0.10];
        // Left half lit warm and a stop over, right half neutral.
        let img = negative(model, base, |x, _| {
            if x < 150 {
                [0.5, 0.36, 0.25]
            } else {
                [0.18; 3]
            }
        });
        let dpi = 254; // 10 px per mm.
        let area = ScanArea::full(30.0, 20.0);
        let frame = |cx| FrameRect {
            cx_mm: cx,
            cy_mm: 10.0,
            width_mm: 10.0,
            height_mm: 10.0,
            angle_deg: 0.0,
        };
        let levels = frame_levels(
            &img,
            GAMMA,
            dpi,
            area,
            &[frame(7.0), frame(22.0)],
            &Inverter::new(model, base, &Printing::default()),
        );
        assert_eq!(levels.len(), 2);
        let expect = [[0.5f32, 0.36, 0.25], [0.18; 3]];
        for (got, want) in levels.iter().zip(expect) {
            for c in 0..3 {
                assert!((got[c] - want[c].log10()).abs() < 0.01, "{got:?} {want:?}");
            }
        }
    }

    #[test]
    fn mock_negatives_invert_to_their_scenes() {
        use scan_core::mock::{self, MockBackend};
        use scan_core::{NoProgress, ScanRequest, ScanSource, ScannerBackend};

        // The whole chain: the mock shoots its frames on Kodacolor 200 and scans them, frame
        // detection measures the base, and the profile inverts them.
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let request = ScanRequest::new(ScanSource::Transparency, 150);
        let result = dev.scan(&request, &mut NoProgress).unwrap();
        let detection = crate::detect_frames(
            result.primary(),
            result.gamma,
            result.dpi,
            result.area,
            &Default::default(),
        );
        let base = detection.base.unwrap();
        for c in 0..3 {
            assert!(
                (base[c] / mock::FILM_BASE[c] - 1.0).abs() < 0.01,
                "{base:?}"
            );
        }
        let inverter = Inverter::new(mock::FILM.model(), base, &Printing::default());
        let positive = invert(result.primary(), result.gamma, &inverter);
        let specks = mock::dust_specks(result.area);
        let mm = |px: u32| (px as f32 + 0.5) / result.dpi as f32 * MM_PER_INCH;
        let (mut checked, mut worst) = (0, 0.0f32);
        for index in 0..12 {
            let (fx, fy, fw, fh) = mock::frame_rect_mm(index);
            for (u, v) in [(0.1, 0.1), (0.9, 0.5), (0.5, 0.9), (0.2, 0.7)] {
                let px = ((fx + u * fw) / MM_PER_INCH * result.dpi as f32) as u32;
                let py = ((fy + v * fh) / MM_PER_INCH * result.dpi as f32) as u32;
                let (x, y) = (mm(px), mm(py));
                if specks
                    .iter()
                    .any(|&(sx, sy, r)| (x - sx).powi(2) + (y - sy).powi(2) <= (r + 0.3).powi(2))
                {
                    continue;
                }
                let scene = mock::frame_scene(index, (x - fx) / fw, (y - fy) / fh);
                let want = inverter.display(scene.map(|s| 0.9 * s));
                let got = positive.get_pixel(px, py).0.map(|v| f32::from(v) / 65535.0);
                for c in 0..3 {
                    worst = worst.max((got[c] - want[c]).abs());
                }
                checked += 1;
            }
        }
        assert!(checked > 40, "{checked}");
        // Within half a percent of full scale, everywhere.
        assert!(worst < 0.005, "worst {worst}");
    }

    #[test]
    fn base_estimate_finds_the_clearest_film() {
        let model = stock::default_stock().model();
        let base = [0.56, 0.22, 0.10];
        // Mostly image, with a band of bare film along the top.
        let img = negative(model, base, |_, y| if y < 20 { [0.0; 3] } else { [0.3; 3] });
        let est = estimate_base(&img, GAMMA).unwrap();
        for c in 0..3 {
            assert!((est[c] / base[c] - 1.0).abs() < 0.01, "{est:?}");
        }
    }
}
