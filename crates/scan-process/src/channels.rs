//! Measuring how far a scan's color channels are apart, to line them up when a frame is cut
//! ([`crate::crop_frame_aligned`]).
//!
//! A CCD scanner reads red, green and blue through three rows a fixed distance apart, and
//! builds each line from the rows' reads as the carriage passes. When that distance isn't a
//! whole number of lines, the channels are offset by a fraction of a pixel. The V500's rows
//! are about 40.4 lines apart at 1,600 dpi, so its red and blue sit 0.2–0.5 px either side of
//! green along the carriage's travel, depending on where across the bed they are.
//!
//! [`channel_shifts`] measures it within a frame: for red and blue against green, along each
//! axis, the shift whose gradients correlate best with green's, searched at a twentieth of a
//! pixel with the channel interpolated by the same kernel the cut uses. Gradients make the
//! peak sharp and ignore the channels' different levels.

use scan_core::image::{DynamicImage, GenericImageView};
use scan_core::{MM_PER_INCH, ScanArea};

use crate::FrameRect;
use crate::lanczos;

/// The largest shift looked for, in pixels.
const MAX_SHIFT: f32 = 2.0;
/// The fine search's step, in pixels.
const STEP: f32 = 0.05;
/// Pixels (at most) sampled.
const SAMPLES: usize = 100_000;
/// The part of the frame measured, about its centre: clear of its edges and the holder.
const INNER: f32 = 0.8;
/// Samples within this of either end of the scale (0–1) in any channel are left out: clipped
/// light and the holder have no edges to match.
const CLIP: f32 = 0.02;
/// Rounds of measuring each axis with the other's estimate applied...
const ROUNDS: usize = 3;
/// ...searching within this of the last round's estimate, in pixels.
const NEAR: f32 = 0.25;
/// The least correlation of the best shift's gradients to trust it.
const MIN_SCORE: f64 = 0.3;

/// How far each channel of `image` (a scan of `area` at `dpi`) is from green within `frame`,
/// in scan pixels across and down: channel `c`'s detail at `p` in green is at `p + shift[c]`.
/// Green's is 0, and so is any shift that can't be measured (greyscale scans, frames without
/// detail).
pub fn channel_shifts(
    image: &DynamicImage,
    dpi: u32,
    area: ScanArea,
    frame: &FrameRect,
) -> [[f32; 2]; 3] {
    let mut shifts = [[0.0; 2]; 3];
    if image.color().channel_count() < 3 {
        return shifts;
    }
    let px_per_mm = dpi as f32 / MM_PER_INCH;
    let inner = FrameRect {
        width_mm: frame.width_mm * INNER,
        height_mm: frame.height_mm * INNER,
        ..*frame
    };
    let corners = inner.corners();
    let bound = |f: fn(f32, f32) -> f32, axis: fn(&(f32, f32)) -> f32, start: f32| {
        corners.iter().map(axis).fold(start, f)
    };
    let to_px =
        |mm: f32, origin: f32, n: u32| ((mm - origin) * px_per_mm).max(0.0).min(n as f32) as u32;
    let (iw, ih) = image.dimensions();
    let x0 = to_px(bound(f32::min, |c| c.0, f32::MAX), area.x_mm, iw);
    let y0 = to_px(bound(f32::min, |c| c.1, f32::MAX), area.y_mm, ih);
    let x1 = to_px(bound(f32::max, |c| c.0, f32::MIN), area.x_mm, iw);
    let y1 = to_px(bound(f32::max, |c| c.1, f32::MIN), area.y_mm, ih);
    let margin = MAX_SHIFT.ceil() as usize + lanczos::A + 2;
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    if w <= 2 * margin + 1 || h <= 2 * margin + 1 {
        return shifts;
    }
    let region = image.crop_imm(x0, y0, w as u32, h as u32).into_rgb32f();
    let planes = Planes::new(&region, w, h);
    let step = (((w * h) as f64 / SAMPLES as f64).sqrt().ceil() as usize).max(1);
    let points: Vec<(usize, usize)> = (margin..h - margin)
        .step_by(step)
        .flat_map(|y| (margin..w - margin).step_by(step).map(move |x| (x, y)))
        .filter(|&(x, y)| planes.clear(x, y, margin))
        .collect();
    // The rows (for shifts across) and columns (down) the samples are on: the only ones a
    // measurement reads along.
    let mut lines = [vec![false; h], vec![false; w]];
    for &(x, y) in &points {
        lines[0][y] = true;
        lines[1][x] = true;
    }
    // Oblique detail makes a shift along one axis look partly like one along the other: each
    // axis is measured on the channel moved by the other's estimate, until they settle.
    let measure = |c: usize| {
        let mut s: [Option<f32>; 2] = [None; 2];
        for round in 0..ROUNDS {
            for axis in 0..2 {
                let other = 1 - axis;
                let moved = planes.moved(c, other, s[other].unwrap_or(0.0), h, &lines[axis]);
                let near = if round == 0 { None } else { s[axis] };
                s[axis] = planes.shift(&moved, axis, &points, near);
            }
        }
        s.map(|v| v.unwrap_or(0.0))
    };
    let (red, blue) = std::thread::scope(|scope| {
        let red = scope.spawn(|| measure(0));
        (red.join().expect("measuring red"), measure(2))
    });
    shifts[0] = red;
    shifts[2] = blue;
    shifts
}

/// A region's channels as separate planes of 0–1 values.
struct Planes {
    data: [Vec<f32>; 3],
    w: usize,
}

impl Planes {
    fn new(rgb: &[f32], w: usize, h: usize) -> Self {
        let data = std::array::from_fn(|c| (0..w * h).map(|i| rgb[i * 3 + c]).collect());
        Self { data, w }
    }

    fn at(&self, c: usize, x: usize, y: usize) -> f32 {
        self.data[c][y * self.w + x]
    }

    /// Whether no channel is near either end of the scale within `r` of (x, y), along
    /// either axis.
    fn clear(&self, x: usize, y: usize, r: usize) -> bool {
        (0..3).all(|c| {
            (0..=2 * r).all(|k| {
                let ok = |v: f32| (CLIP..=1.0 - CLIP).contains(&v);
                ok(self.at(c, x + k - r, y)) && ok(self.at(c, x, y + k - r))
            })
        })
    }

    /// Channel `c` read `s` further along `axis` (0 across, 1 down), clamped at the region's
    /// edges: as it would be with that shift taken out. Only on the rows (`axis` 1) or
    /// columns (0) that `keep` marks; 0 elsewhere.
    fn moved(&self, c: usize, axis: usize, s: f32, h: usize, keep: &[bool]) -> Vec<f32> {
        if s == 0.0 {
            return self.data[c].clone();
        }
        let w = self.w;
        let (first, t) = lanczos::split(s);
        let weights = lanczos::weights(t);
        let n = if axis == 0 { w } else { h } as isize;
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if !keep[if axis == 0 { x } else { y }] {
                    return 0.0;
                }
                let at = if axis == 0 { x } else { y } as isize;
                (0..lanczos::TAPS)
                    .filter(|&k| weights[k] != 0.0)
                    .map(|k| {
                        let d = (at + first + k as isize).clamp(0, n - 1) as usize;
                        let j = if axis == 0 { y * w + d } else { d * w + x };
                        self.data[c][j] * weights[k]
                    })
                    .sum()
            })
            .collect()
    }

    /// `plane` at `p + s` along `axis` from (x, y).
    fn interpolated(&self, plane: &[f32], (x, y): (usize, usize), axis: usize, s: f32) -> f32 {
        let (first, t) = lanczos::split(s);
        let w = lanczos::weights(t);
        (0..lanczos::TAPS)
            .filter(|&k| w[k] != 0.0)
            .map(|k| {
                let d = first + k as isize;
                let (sx, sy) = match axis {
                    0 => ((x as isize + d) as usize, y),
                    _ => (x, (y as isize + d) as usize),
                };
                plane[sy * self.w + sx] * w[k]
            })
            .sum()
    }

    /// The correlation of green's gradients along `axis` at `points` with `plane`'s `s`
    /// further along.
    fn score(&self, plane: &[f32], axis: usize, points: &[(usize, usize)], s: f32) -> f64 {
        let (mut cross, mut green, mut other) = (0f64, 0f64, 0f64);
        for &(x, y) in points {
            let (gn, gp) = match axis {
                0 => (self.at(1, x - 1, y), self.at(1, x + 1, y)),
                _ => (self.at(1, x, y - 1), self.at(1, x, y + 1)),
            };
            let g = f64::from(gp - gn);
            let o = f64::from(
                self.interpolated(plane, (x, y), axis, s + 1.0)
                    - self.interpolated(plane, (x, y), axis, s - 1.0),
            );
            cross += g * o;
            green += g * g;
            other += o * o;
        }
        cross / (green * other).sqrt().max(f64::MIN_POSITIVE)
    }

    /// `plane`'s shift from green along `axis`, if its gradients correlate well enough:
    /// searched over the whole range, or within [`NEAR`] of `near`.
    fn shift(
        &self,
        plane: &[f32],
        axis: usize,
        points: &[(usize, usize)],
        near: Option<f32>,
    ) -> Option<f32> {
        if points.is_empty() {
            return None;
        }
        let best = |candidates: &mut dyn Iterator<Item = f32>| {
            candidates
                .map(|s| (s, self.score(plane, axis, points, s)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .expect("candidates")
        };
        let (coarse, n) = match near {
            Some(v) => (v, (NEAR / STEP).round() as i32),
            None => {
                let whole = MAX_SHIFT as i32;
                let coarse = best(&mut (-whole..=whole).map(|s| s as f32)).0;
                (coarse, (1.0 / STEP).round() as i32)
            }
        };
        let (fine, score) = best(&mut (-n..=n).map(|k| coarse + k as f32 * STEP));
        (score >= MIN_SCORE && fine.abs() < MAX_SHIFT).then_some(fine)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan_core::image::{ImageBuffer, Luma, Rgb};

    /// Smooth detail at mid-grey: a sum of waves at unrelated frequencies and angles.
    fn scene(x: f32, y: f32) -> f32 {
        let waves = [
            (0.21, 0.07, 0.0),
            (-0.05, 0.31, 1.3),
            (0.13, -0.17, 2.1),
            (0.37, 0.23, 0.4),
        ];
        0.5 + waves
            .iter()
            .map(|&(fx, fy, phase): &(f32, f32, f32)| 0.08 * (fx * x + fy * y + phase).sin())
            .sum::<f32>()
    }

    /// A 10 px/mm scan whose red and blue detail sits `shifts` from green's.
    fn scan(shifts: [[f32; 2]; 3]) -> DynamicImage {
        DynamicImage::ImageRgb16(ImageBuffer::from_fn(300, 300, |x, y| {
            let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
            Rgb(std::array::from_fn(|c| {
                (scene(x - shifts[c][0], y - shifts[c][1]) * 65535.0).round() as u16
            }))
        }))
    }

    const FRAME: FrameRect = FrameRect {
        cx_mm: 15.0,
        cy_mm: 15.0,
        width_mm: 24.0,
        height_mm: 24.0,
        angle_deg: 0.3,
    };

    #[test]
    fn measures_fractional_shifts() {
        let shifts = [[0.1, 0.42], [0.0, 0.0], [-0.15, -0.37]];
        let measured = channel_shifts(&scan(shifts), 254, ScanArea::full(30.0, 30.0), &FRAME);
        for c in 0..3 {
            for axis in 0..2 {
                let (m, e) = (measured[c][axis], shifts[c][axis]);
                assert!(
                    (m - e).abs() <= 0.03,
                    "channel {c} axis {axis}: {m} for {e}"
                );
            }
        }
    }

    #[test]
    fn aligned_cuts_line_the_channels_up() {
        let shifts = [[0.0, 0.42], [0.0, 0.0], [0.0, -0.42]];
        let area = ScanArea::full(30.0, 30.0);
        let image = scan(shifts);
        let measured = channel_shifts(&image, 254, area, &FRAME);
        let cut = crate::crop_frame_aligned(&image, 254, area, &FRAME, &measured).to_rgb16();
        let worst = cut
            .pixels()
            .skip(cut.width() as usize * 10)
            .take(cut.width() as usize * (cut.height() as usize - 20))
            .map(|p| {
                let g = i32::from(p[1]);
                (i32::from(p[0]) - g).abs().max((i32::from(p[2]) - g).abs())
            })
            .max()
            .unwrap();
        // The waves' steepest slope is about 0.06 of the scale per pixel: 0.4 px apart would
        // be about 1,500.
        assert!(worst < 200, "{worst}");
    }

    #[test]
    fn greyscale_and_featureless_scans_are_not_shifted() {
        let area = ScanArea::full(30.0, 30.0);
        let grey = DynamicImage::ImageLuma16(ImageBuffer::from_pixel(300, 300, Luma([30_000])));
        assert_eq!(channel_shifts(&grey, 254, area, &FRAME), [[0.0; 2]; 3]);
        let flat = DynamicImage::ImageRgb16(ImageBuffer::from_pixel(300, 300, Rgb([30_000; 3])));
        assert_eq!(channel_shifts(&flat, 254, area, &FRAME), [[0.0; 2]; 3]);
    }
}
