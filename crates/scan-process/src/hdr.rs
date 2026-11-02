//! Merging a scan's exposure passes into one image with less noise in the film's dense
//! parts, as SilverFast's Multi-Exposure does.
//!
//! Little light gets through the densest parts of a frame (a negative's highlights, a slide's
//! shadows), so the scanner's noise covers them. A longer exposure lifts them clear of it but
//! clips the thin parts. [`merge_exposures`] starts from the shortest pass and folds in each
//! longer one:
//!
//! 1. Each pass is a separate scan, so a longer one may be shifted a little. The shift that
//!    best correlates its gradients with the merge's is found, to a fraction of a pixel.
//! 2. Its gain is measured per channel in linear light: a line fit of its values against the
//!    merge's, over pixels clear of noise in the one and of clipping in the other. It is
//!    rarely exactly 2^EV.
//! 3. Where it isn't near clipping, it replaces the merge, scaled back by its gain. Its weight
//!    fades out smoothly as it nears clipping, the same for every channel of a pixel so
//!    colors don't shift.
//!
//! The merge keeps the shortest pass's scale and encoding, at 16 bits.

use scan_core::ScanPass;
use scan_core::image::{DynamicImage, ImageBuffer};

use crate::dust::dilate;
use crate::fit::{self, Line};

/// A longer pass replaces the merge fully up to this linear level (0–1 of full scale) in its
/// brightest channel...
const BLEND_START: f32 = 0.5;
/// ...and not at all from this one, short of clipping and of any non-linearity near it.
const BLEND_END: f32 = 0.85;
/// The gain is fitted over pixels at least this bright (linear) in the merge, clear of noise.
const FIT_FLOOR: f32 = 0.01;
/// The least spread of a gain fit's points about the line, in linear levels.
const FIT_MIN_SPREAD: f64 = 0.005;
/// The largest shift between passes looked for, in pixels.
const MAX_SHIFT: i32 = 6;
/// Shifts within this of a whole pixel are rounded to it: not worth the blur of resampling.
const SNAP: f32 = 0.1;
/// Pixels (at most) sampled for the shift and gain.
const SAMPLES: usize = 250_000;
/// Pixels at least this bright (encoded) are taken as clipped. The shift search leaves out
/// the gradients near them: clipping flattens an edge on one side only, which would pull the
/// fitted shift towards that side.
const CLIPPED: u16 = 62_000;

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum MergeError {
    #[error("a merge needs at least two exposure passes")]
    TooFewPasses,
    #[error("the exposure passes differ in size or color")]
    Mismatch,
    #[error(
        "the {ev:+} EV pass shares too few unclipped pixels with the shorter ones to measure \
         its gain"
    )]
    NoOverlap { ev: f32 },
}

/// How a longer pass was folded in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PassReport {
    /// The pass's requested exposure.
    pub ev: f32,
    /// Its gain in linear light against the shortest pass, per channel (all three alike for
    /// greyscale).
    pub gain: [f32; 3],
    /// Its pixel `(x + dx, y + dy)` matches the merge's `(x, y)`.
    pub shift: (f32, f32),
    /// The share of the image taken from it, from 0 to 1.
    pub share: f32,
}

impl PassReport {
    /// The gain in stops, averaged over the channels.
    pub fn measured_ev(&self) -> f32 {
        self.gain.iter().map(|g| g.log2()).sum::<f32>() / 3.0
    }
}

/// A scan's exposure passes, merged.
#[derive(Clone, Debug)]
pub struct ExposureMerge {
    /// `ImageRgb16`, or `ImageLuma16` for greyscale passes, in the shortest pass's scale and
    /// encoding.
    pub image: DynamicImage,
    /// The longer passes, shortest first.
    pub passes: Vec<PassReport>,
}

/// Merge `passes`, whose values encode light with `gamma` (see `ScanResult::gamma`).
pub fn merge_exposures(passes: &[ScanPass], gamma: f32) -> Result<ExposureMerge, MergeError> {
    let first = passes.first().ok_or(MergeError::TooFewPasses)?;
    if passes.len() < 2 {
        return Err(MergeError::TooFewPasses);
    }
    let (w, h) = (first.image.width() as usize, first.image.height() as usize);
    let color = first.image.color().has_color();
    let c = if color { 3 } else { 1 };
    if passes.iter().any(|p| {
        (p.image.width() as usize, p.image.height() as usize) != (w, h)
            || p.image.color().has_color() != color
    }) {
        return Err(MergeError::Mismatch);
    }
    let samples = |p: &ScanPass| -> Vec<u16> {
        if color {
            p.image.to_rgb16().into_raw()
        } else {
            p.image.to_luma16().into_raw()
        }
    };
    let mut order: Vec<&ScanPass> = passes.iter().collect();
    order.sort_by(|a, b| a.exposure.ev.total_cmp(&b.exposure.ev));

    let codec = Codec::new(gamma);
    let mut merged = samples(order[0]);
    let mut reports = Vec::new();
    // Each pass's gain is fitted against the merge so far, which is in the shortest scale.
    for pass in &order[1..] {
        let longer = samples(pass);
        let ev = pass.exposure.ev;
        let shift = register(&merged, &longer, w, h, c);
        let sampler = Sampler::new(&longer, w, h, c, shift, &codec);
        let lines = fit_gain(&merged, &sampler, &codec).ok_or(MergeError::NoOverlap { ev })?;
        if lines.iter().any(|l| l.slope <= 0.0) {
            return Err(MergeError::NoOverlap { ev });
        }
        let share = blend(&mut merged, &sampler, &lines, &codec);
        let gain = std::array::from_fn(|i| lines[i.min(c - 1)].slope as f32);
        reports.push(PassReport {
            ev,
            gain,
            shift,
            share,
        });
    }

    let (w, h) = (w as u32, h as u32);
    let image = if color {
        DynamicImage::ImageRgb16(ImageBuffer::from_raw(w, h, merged).expect("size matches"))
    } else {
        DynamicImage::ImageLuma16(ImageBuffer::from_raw(w, h, merged).expect("size matches"))
    };
    Ok(ExposureMerge {
        image,
        passes: reports,
    })
}

/// Encoded 16-bit samples to linear light and back.
struct Codec {
    linear: Vec<f32>,
    inverse: f32,
}

impl Codec {
    fn new(gamma: f32) -> Self {
        let gamma = if gamma > 0.0 { gamma } else { 1.0 };
        Self {
            linear: (0..=u16::MAX)
                .map(|v| (f32::from(v) / 65535.0).powf(gamma))
                .collect(),
            inverse: 1.0 / gamma,
        }
    }

    fn linear(&self, v: u16) -> f32 {
        self.linear[usize::from(v)]
    }

    fn encode(&self, l: f32) -> u16 {
        (l.clamp(0.0, 1.0).powf(self.inverse) * 65535.0).round() as u16
    }
}

/// A pass read at the merge's pixels: shifted, and bilinear in linear light.
struct Sampler<'a> {
    data: &'a [u16],
    w: usize,
    h: usize,
    c: usize,
    /// Whole and fractional parts of the shift.
    whole: (isize, isize),
    frac: (f32, f32),
    codec: &'a Codec,
}

impl<'a> Sampler<'a> {
    fn new(
        data: &'a [u16],
        w: usize,
        h: usize,
        c: usize,
        (dx, dy): (f32, f32),
        codec: &'a Codec,
    ) -> Self {
        Self {
            data,
            w,
            h,
            c,
            whole: (dx.floor() as isize, dy.floor() as isize),
            frac: (dx - dx.floor(), dy - dy.floor()),
            codec,
        }
    }

    /// The linear value of every channel at the merge's pixel `(x, y)`, or `None` where the
    /// pass doesn't cover it.
    fn at(&self, x: usize, y: usize) -> Option<[f32; 3]> {
        let x0 = x as isize + self.whole.0;
        let y0 = y as isize + self.whole.1;
        if x0 < 0 || y0 < 0 || x0 + 1 >= self.w as isize || y0 + 1 >= self.h as isize {
            return None;
        }
        let (x0, y0) = (x0 as usize, y0 as usize);
        let (tx, ty) = self.frac;
        let i = (y0 * self.w + x0) * self.c;
        let below = i + self.w * self.c;
        let mut out = [0.0; 3];
        for (ch, v) in out.iter_mut().enumerate().take(self.c) {
            let l = |j: usize| self.codec.linear(self.data[j + ch]);
            let top = l(i) * (1.0 - tx) + l(i + self.c) * tx;
            let bottom = l(below) * (1.0 - tx) + l(below + self.c) * tx;
            *v = top * (1.0 - ty) + bottom * ty;
        }
        Some(out)
    }
}

/// Grid spacing that samples at most about [`SAMPLES`] of `w × h` pixels.
fn sample_step(w: usize, h: usize) -> usize {
    ((w * h) as f64 / SAMPLES as f64).sqrt().ceil().max(1.0) as usize
}

/// The shift of `longer` against `merged` (see [`PassReport::shift`]): the whole-pixel shift
/// whose gradients correlate best, refined with a parabola through its neighbours. Gradients
/// make the peak sharp, and correlating them ignores the passes' difference in gain.
fn register(merged: &[u16], longer: &[u16], w: usize, h: usize, c: usize) -> (f32, f32) {
    let ch = c / 2; // green, or the only channel
    let value = |data: &[u16], x: usize, y: usize| f32::from(data[(y * w + x) * c + ch]);
    let gradient = |data: &[u16], x: usize, y: usize| {
        (
            value(data, x + 1, y) - value(data, x - 1, y),
            value(data, x, y + 1) - value(data, x, y - 1),
        )
    };
    let margin = MAX_SHIFT as usize + 1;
    if w <= 2 * margin + 1 || h <= 2 * margin + 1 {
        return (0.0, 0.0);
    }
    // Clipped in either pass, or within reach of a clipped pixel at any shift searched.
    let mut clipped: Vec<bool> = (0..w * h)
        .map(|i| merged[i * c + ch] >= CLIPPED || longer[i * c + ch] >= CLIPPED)
        .collect();
    dilate(&mut clipped, w, h, margin);
    let step = sample_step(w, h);
    let mut points = Vec::new();
    for y in (margin..h - margin).step_by(step) {
        for x in (margin..w - margin).step_by(step) {
            if !clipped[y * w + x] {
                points.push((x, y, gradient(merged, x, y)));
            }
        }
    }
    let side = (2 * MAX_SHIFT + 1) as usize;
    let mut scores = vec![f32::MIN; side * side];
    let merged_energy: f64 = points
        .iter()
        .map(|(_, _, (gx, gy))| f64::from(gx * gx + gy * gy))
        .sum();
    for dy in -MAX_SHIFT..=MAX_SHIFT {
        for dx in -MAX_SHIFT..=MAX_SHIFT {
            let (mut cross, mut energy) = (0f64, 0f64);
            for &(x, y, (mx, my)) in &points {
                let (px, py) = (
                    (x as isize + dx as isize) as usize,
                    (y as isize + dy as isize) as usize,
                );
                let (lx, ly) = gradient(longer, px, py);
                cross += f64::from(mx * lx + my * ly);
                energy += f64::from(lx * lx + ly * ly);
            }
            let score = cross / (merged_energy * energy).sqrt().max(1.0);
            let i = (dy + MAX_SHIFT) as usize * side + (dx + MAX_SHIFT) as usize;
            scores[i] = score as f32;
        }
    }
    let best = (0..scores.len())
        .max_by(|&a, &b| scores[a].total_cmp(&scores[b]))
        .expect("at least one shift");
    let (bx, by) = (best % side, best / side);
    let score = |x: usize, y: usize| scores[y * side + x];
    // A parabola's vertex through the best score and its neighbours on one axis.
    let refine = |before: Option<f32>, at: f32, after: Option<f32>| match (before, after) {
        (Some(a), Some(b)) if a + b - 2.0 * at < 0.0 => {
            let f = 0.5 * (a - b) / (a + b - 2.0 * at);
            if f.abs() < SNAP { 0.0 } else { f }
        }
        _ => 0.0,
    };
    let fx = refine(
        bx.checked_sub(1).map(|x| score(x, by)),
        score(bx, by),
        (bx + 1 < side).then(|| score(bx + 1, by)),
    );
    let fy = refine(
        by.checked_sub(1).map(|y| score(bx, y)),
        score(bx, by),
        (by + 1 < side).then(|| score(bx, by + 1)),
    );
    (
        bx as f32 - MAX_SHIFT as f32 + fx,
        by as f32 - MAX_SHIFT as f32 + fy,
    )
}

/// Lines of the longer pass's linear values against the merge's, one per channel.
///
/// Each point is a 3 × 3 mean in both passes: noise in the merge's values would flatten the
/// line (regression dilution), and the means have a ninth of it. The points are picked by
/// pixels two out from their centre, not by their own values: picking them by their own noisy
/// values would bias the fit too, mostly at the noise floor, where the ones that noise lifted
/// above it would get in and the ones it pushed below wouldn't.
fn fit_gain(merged: &[u16], longer: &Sampler<'_>, codec: &Codec) -> Option<Vec<Line>> {
    const RING: [(isize, isize); 8] = [
        (-2, -2),
        (0, -2),
        (2, -2),
        (-2, 0),
        (2, 0),
        (-2, 2),
        (0, 2),
        (2, 2),
    ];
    let (w, h, c) = (longer.w, longer.h, longer.c);
    let step = sample_step(w, h);
    let mut points = vec![Vec::new(); c];
    for y in (2..h.saturating_sub(2)).step_by(step) {
        'pixel: for x in (2..w.saturating_sub(2)).step_by(step) {
            // Whether each channel is clear of clipping in the longer pass, and its mean in
            // the merge, around the pixel. Channels clip separately: a negative's orange
            // mask clips red long before blue.
            let mut usable = [true; 3];
            let mut floor = [0f32; 3];
            for (dx, dy) in RING {
                let (rx, ry) = (x.wrapping_add_signed(dx), y.wrapping_add_signed(dy));
                let Some(r) = longer.at(rx, ry) else {
                    continue 'pixel;
                };
                for ch in 0..c {
                    usable[ch] &= r[ch] < BLEND_END;
                    floor[ch] += codec.linear(merged[(ry * w + rx) * c + ch]) / RING.len() as f32;
                }
            }
            let (mut m, mut l) = ([0f32; 3], [0f32; 3]);
            for (bx, by) in (0..9).map(|i| (x + i % 3 - 1, y + i / 3 - 1)) {
                let Some(lb) = longer.at(bx, by) else {
                    continue 'pixel;
                };
                for ch in 0..c {
                    m[ch] += codec.linear(merged[(by * w + bx) * c + ch]) / 9.0;
                    l[ch] += lb[ch] / 9.0;
                }
            }
            for (ch, points) in points.iter_mut().enumerate() {
                if usable[ch] && floor[ch] >= FIT_FLOOR {
                    points.push((f64::from(m[ch]), f64::from(l[ch])));
                }
            }
        }
    }
    points.iter().map(|p| gain_line(p)).collect()
}

/// A line through `points`, or where they hardly vary (a flat patch of film), the median of
/// their ratios through the origin.
fn gain_line(points: &[(f64, f64)]) -> Option<Line> {
    fit::trimmed_line(points, FIT_MIN_SPREAD).or_else(|| {
        let mut ratios: Vec<f64> = points.iter().map(|&(m, l)| l / m).collect();
        if ratios.len() < 100 {
            return None;
        }
        let mid = ratios.len() / 2;
        let (_, &mut median, _) = ratios.select_nth_unstable_by(mid, f64::total_cmp);
        Some(Line {
            slope: median,
            offset: 0.0,
            spread: FIT_MIN_SPREAD,
        })
    })
}

/// Fold `longer` into `merged` with its fitted `lines`; returns the share taken from it.
fn blend(merged: &mut [u16], longer: &Sampler<'_>, lines: &[Line], codec: &Codec) -> f32 {
    let (w, h, c) = (longer.w, longer.h, longer.c);
    let mut taken = 0f64;
    for y in 0..h {
        for x in 0..w {
            let Some(l) = longer.at(x, y) else { continue };
            let brightest = l[..c].iter().copied().fold(0.0, f32::max);
            let weight = fade(brightest);
            if weight <= 0.0 {
                continue;
            }
            taken += f64::from(weight);
            let i = (y * w + x) * c;
            for (ch, line) in lines.iter().enumerate() {
                let scaled = ((f64::from(l[ch]) - line.offset) / line.slope) as f32;
                let m = codec.linear(merged[i + ch]);
                merged[i + ch] = codec.encode(weight * scaled + (1.0 - weight) * m);
            }
        }
    }
    (taken / (w * h) as f64) as f32
}

/// A longer pass's weight at linear level `v` of its brightest channel: 1 up to
/// [`BLEND_START`], easing to 0 at [`BLEND_END`].
fn fade(v: f32) -> f32 {
    let t = ((BLEND_END - v) / (BLEND_END - BLEND_START)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan_core::image::{Luma, Rgb};
    use scan_core::mock::MockBackend;
    use scan_core::{
        BitDepth, Exposure, NoProgress, ScanArea, ScanRequest, ScanSource, ScannerBackend,
    };

    const GAMMA: f32 = 1.8;
    const W: u32 = 400;
    const H: u32 = 200;
    /// Per-channel color of the test scene, like the orange cast of a negative.
    const TINT: [f32; 3] = [1.0, 0.75, 0.55];

    /// A reproducible standard normal sample for `(seed, i)`.
    fn gaussian(seed: u64, i: u64) -> f32 {
        let hash = |n: u64| {
            let mut z = n.wrapping_add(0x9e37_79b9_7f4a_7c15);
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
        };
        let (u, v) = (
            hash((seed << 40) ^ (2 * i)),
            hash((seed << 40) ^ (2 * i + 1)),
        );
        ((-2.0 * u.max(1e-12).ln()).sqrt() * (std::f64::consts::TAU * v).cos()) as f32
    }

    /// Smooth texture in 0..1 at any point: bilinear between hashed values on an 6 px grid.
    fn texture(x: f32, y: f32) -> f32 {
        let at = |i: i64, j: i64| {
            let n = (i.wrapping_mul(73_856_093) ^ j.wrapping_mul(19_349_663)) as u64;
            (gaussian(7, n % 1_000_003).clamp(-2.0, 2.0) + 2.0) / 4.0
        };
        let (gx, gy) = (x / 6.0, y / 6.0);
        let (i, j) = (gx.floor() as i64, gy.floor() as i64);
        let (tx, ty) = (gx - gx.floor(), gy - gy.floor());
        let top = at(i, j) * (1.0 - tx) + at(i + 1, j) * tx;
        let bottom = at(i, j + 1) * (1.0 - tx) + at(i + 1, j + 1) * tx;
        top * (1.0 - ty) + bottom * ty
    }

    /// The scene's linear light at a point: dense on the left, thin on the right.
    fn scene(x: f32, y: f32, ch: usize) -> f32 {
        let ramp = 0.003 * (x / W as f32 * (1.0f32 / 0.003).ln()).exp();
        ramp * (0.7 + 0.3 * texture(x, y)) * TINT[ch]
    }

    /// A noisy, gamma-encoded 16-bit scan of the scene at `gain`, seen shifted by `shift`.
    fn pass(seed: u64, ev: f32, gain: [f32; 3], shift: (f32, f32)) -> ScanPass {
        let image = ImageBuffer::from_fn(W, H, |x, y| {
            Rgb(std::array::from_fn(|ch| {
                let truth = scene(x as f32 - shift.0, y as f32 - shift.1, ch) * gain[ch];
                let i = u64::from(y * W + x) * 3 + ch as u64;
                // Read noise, and a little shot noise.
                let sigma = 0.0015 + 0.004 * truth.sqrt();
                let v = (truth + sigma * gaussian(seed, i)).clamp(0.0, 1.0);
                (v.powf(1.0 / GAMMA) * 65535.0).round() as u16
            }))
        });
        ScanPass {
            exposure: Exposure { ev },
            image: DynamicImage::ImageRgb16(image),
        }
    }

    fn linear(image: &DynamicImage, x: u32, y: u32, ch: usize) -> f32 {
        (f32::from(image.to_rgb16().get_pixel(x, y).0[ch]) / 65535.0).powf(GAMMA)
    }

    /// RMS and mean error against the scene over columns `xs`, relative to its mean level.
    fn error(image: &DynamicImage, xs: std::ops::Range<u32>, ch: usize) -> (f32, f32) {
        let rgb = image.to_rgb16();
        let (mut sq, mut sum, mut level, mut n) = (0f64, 0f64, 0f64, 0f64);
        for y in 10..H - 10 {
            for x in xs.clone() {
                let v = (f32::from(rgb.get_pixel(x, y).0[ch]) / 65535.0).powf(GAMMA);
                let truth = scene(x as f32, y as f32, ch);
                let e = f64::from(v - truth);
                sq += e * e;
                sum += e;
                level += f64::from(truth);
                n += 1.0;
            }
        }
        let level = level / n;
        (((sq / n).sqrt() / level) as f32, (sum / n / level) as f32)
    }

    fn two_passes() -> Vec<ScanPass> {
        vec![
            pass(1, 0.0, [1.0; 3], (0.0, 0.0)),
            // About a stop more, but not exactly, and the carriage landed elsewhere.
            pass(2, 1.0, [1.9, 2.0, 2.1], (1.5, -2.25)),
        ]
    }

    #[test]
    fn measures_the_gain_and_shift() {
        let merge = merge_exposures(&two_passes(), GAMMA).unwrap();
        let [report] = merge.passes[..] else {
            panic!("{:?}", merge.passes)
        };
        for (g, want) in report.gain.iter().zip([1.9, 2.0, 2.1]) {
            assert!((g - want).abs() < 0.02 * want, "gain {:?}", report.gain);
        }
        let (dx, dy) = report.shift;
        assert!(
            (dx - 1.5).abs() < 0.25 && (dy + 2.25).abs() < 0.25,
            "shift {:?}",
            report.shift
        );
        assert!((report.measured_ev() - 1.0).abs() < 0.05);
        // The longer pass reaches its blend at about 80 % of the way across the ramp.
        assert!((0.7..0.9).contains(&report.share), "share {}", report.share);
    }

    #[test]
    fn dense_areas_are_less_noisy() {
        let passes = two_passes();
        let merge = merge_exposures(&passes, GAMMA).unwrap();
        for ch in 0..3 {
            let (before, _) = error(&passes[0].image, 10..80, ch);
            let (after, _) = error(&merge.image, 10..80, ch);
            assert!(
                after < 0.65 * before,
                "channel {ch}: {before:.3} -> {after:.3}"
            );
        }
    }

    #[test]
    fn no_seam_or_color_shift() {
        // Bands across the ramp, through the blend (about 250 to 350) and into where only the
        // normal pass is used: none is brighter or darker than the scene beyond noise.
        let merge = merge_exposures(&two_passes(), GAMMA).unwrap();
        for band in (10..W - 10).step_by(20) {
            for ch in 0..3 {
                let (_, bias) = error(&merge.image, band..band + 20, ch);
                // The densest bands are only a few hundred levels of 65,535 bright, so their
                // means aren't known as closely.
                let tolerance = if band < 100 { 0.015 } else { 0.003 };
                assert!(
                    bias.abs() < tolerance,
                    "columns {band}+, channel {ch}: bias {bias:.4}"
                );
            }
        }
        // The thin right edge, where the longer pass clipped, is the normal pass's.
        let normal = &two_passes()[0].image;
        for ch in 0..3 {
            let (a, b) = (
                linear(normal, W - 5, 100, ch),
                linear(&merge.image, W - 5, 100, ch),
            );
            assert!((a - b).abs() < 1e-4, "channel {ch}: {a} vs {b}");
        }
    }

    #[test]
    fn passes_merge_in_order_of_exposure() {
        // Listed longest first: the shortest still sets the scale.
        let mut passes = two_passes();
        passes.swap(0, 1);
        let merge = merge_exposures(&passes, GAMMA).unwrap();
        assert_eq!(merge.passes[0].ev, 1.0);
        let (_, bias) = error(&merge.image, 200..220, 1);
        assert!(bias.abs() < 0.01, "bias {bias}");
    }

    #[test]
    fn greyscale_and_8_bit_passes() {
        let to_gray8 = |p: ScanPass| ScanPass {
            image: DynamicImage::ImageLuma8(p.image.to_luma8()),
            ..p
        };
        let passes: Vec<_> = two_passes().into_iter().map(to_gray8).collect();
        let merge = merge_exposures(&passes, GAMMA).unwrap();
        assert!(matches!(merge.image, DynamicImage::ImageLuma16(_)));
        let gain = merge.passes[0].gain;
        assert!(
            gain[0] == gain[2] && (1.8..2.2).contains(&gain[0]),
            "{gain:?}"
        );
    }

    #[test]
    fn mock_scanner_passes_merge() {
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let mut req = ScanRequest::new(ScanSource::Transparency, 150);
        // The first frame and the strip's edge and sprocket holes. The frame's disc is dense
        // enough in red not to clip at +1 EV.
        req.area = Some(ScanArea {
            x_mm: 0.0,
            y_mm: 5.0,
            width_mm: 35.0,
            height_mm: 30.0,
        });
        req.bit_depth = BitDepth::Sixteen;
        req.exposures = vec![Exposure::default(), Exposure { ev: 1.0 }];
        let result = dev.scan(&req, &mut NoProgress).unwrap();
        let merge = merge_exposures(&result.passes, result.gamma).unwrap();
        let report = merge.passes[0];
        assert!((report.measured_ev() - 1.0).abs() < 0.01, "{report:?}");
        // The film's clipped edges (sprocket holes, the holder) don't pull the shift.
        assert_eq!(report.shift, (0.0, 0.0));
        // The mock is noiseless, so the merge is the normal pass to within rounding.
        let (a, b) = (result.primary().to_rgb16(), merge.image.to_rgb16());
        let worst = a
            .pixels()
            .zip(b.pixels())
            .flat_map(|(p, q)| (0..3).map(move |c| p.0[c].abs_diff(q.0[c])))
            .max()
            .unwrap();
        assert!(worst <= 2, "differs by up to {worst}");
    }

    #[test]
    fn rejects_unmergeable_passes() {
        let passes = two_passes();
        assert_eq!(
            merge_exposures(&passes[..1], GAMMA).unwrap_err(),
            MergeError::TooFewPasses
        );
        let small = ScanPass {
            image: DynamicImage::ImageRgb16(ImageBuffer::new(10, 10)),
            ..passes[1].clone()
        };
        assert_eq!(
            merge_exposures(&[passes[0].clone(), small], GAMMA).unwrap_err(),
            MergeError::Mismatch
        );
        // A longer pass that's clipped everywhere has nothing to fit.
        let white = ScanPass {
            image: DynamicImage::ImageLuma16(ImageBuffer::from_pixel(W, H, Luma([65535]))),
            ..passes[1].clone()
        };
        let gray = ScanPass {
            image: DynamicImage::ImageLuma16(passes[0].image.to_luma16()),
            ..passes[0].clone()
        };
        assert_eq!(
            merge_exposures(&[gray, white], GAMMA).unwrap_err(),
            MergeError::NoOverlap { ev: 1.0 }
        );
    }
}
