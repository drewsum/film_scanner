//! Editing a frame: the Adjust step, applied to each frame with its own [`Adjustments`].
//!
//! [`develop`] makes a frame's positive, in this order:
//!
//! 1. **Color, in scene light.** The frame's exposure and color balance are added to the
//!    roll's printing (see [`Adjustments::printing`]), so a negative is inverted with them
//!    before the print tone curve, the way a photofinisher corrects each frame's printer
//!    lights. A positive (a flatbed scan, or film left uninverted) gets them as gains on its
//!    linear light.
//!
//! Then [`apply`] edits the positive, 16-bit sRGB:
//!
//! 2. **Grain and color noise reduction**: an edge-preserving (guided) filter of the
//!    image's brightness and of its color, separately.
//! 3. **Tone**, per channel then for all three: levels, then curves, then brightness and
//!    contrast. Together these are one table per channel.
//! 4. **Saturation and selective color**: chroma scaled, and hue, saturation and lightness
//!    moved for six hue ranges.
//! 5. **Sharpening**: an unsharp mask of brightness alone, so it adds no color fringes.
//!
//! [`orient`] turns and mirrors the result. The spatial steps (2 and 5) are sized in pixels
//! of the saved image; a preview passes its `scale` so it shows what is saved.

use std::borrow::Cow;
use std::f32::consts::PI;

use scan_core::image::DynamicImage;
use scan_film::{Inverter, Printing};

use crate::negative::{Rgb16Image, decode_table, invert, to_u16};

/// Index of the levels and curves that apply to all three channels; red, green and blue
/// follow it.
pub const MASTER: usize = 0;
/// The channels levels and curves have, as labelled.
pub const CHANNELS: [&str; 4] = ["RGB", "Red", "Green", "Blue"];
/// The hue ranges of selective color, with the hue each is centred on, in degrees.
pub const HUE_RANGES: [(&str, f32); 6] = [
    ("Reds", 0.0),
    ("Yellows", 60.0),
    ("Greens", 120.0),
    ("Cyans", 180.0),
    ("Blues", 240.0),
    ("Magentas", 300.0),
];

/// Levels: input `black` to `white` stretched to the output range, with a midtone `gamma`
/// (above 1 lightens). Values are 0–1 of full scale, sRGB encoded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Levels {
    pub black: f32,
    pub white: f32,
    pub gamma: f32,
    pub out_black: f32,
    pub out_white: f32,
}

impl Default for Levels {
    fn default() -> Self {
        Self {
            black: 0.0,
            white: 1.0,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
        }
    }
}

impl Levels {
    pub fn apply(&self, v: f32) -> f32 {
        let span = (self.white - self.black).max(1e-4);
        let t = ((v - self.black) / span).clamp(0.0, 1.0);
        let t = t.powf(1.0 / self.gamma.max(0.01));
        self.out_black + t * (self.out_white - self.out_black)
    }
}

/// A tone curve through control points, interpolated with a monotone cubic spline
/// (Fritsch–Carlson), so it never overshoots between points. Flat beyond its end points.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    /// (input, output), 0–1, in increasing input.
    points: Vec<[f32; 2]>,
}

/// The closest two points of a curve may be, in input.
const MIN_GAP: f32 = 0.01;

impl Default for Curve {
    fn default() -> Self {
        Self {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
        }
    }
}

impl Curve {
    /// A curve through `points`, sorted; points closer than 0.01 in input are dropped.
    pub fn new(points: impl IntoIterator<Item = [f32; 2]>) -> Self {
        let mut p: Vec<[f32; 2]> = points
            .into_iter()
            .map(|q| q.map(|v| v.clamp(0.0, 1.0)))
            .collect();
        p.sort_by(|a, b| a[0].total_cmp(&b[0]));
        p.dedup_by(|b, a| b[0] - a[0] < MIN_GAP);
        if p.len() < 2 {
            return Self::default();
        }
        Self { points: p }
    }

    pub fn points(&self) -> &[[f32; 2]] {
        &self.points
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// Add a point, returning its index, or `None` if one is already that close in input.
    pub fn insert(&mut self, x: f32, y: f32) -> Option<usize> {
        let (x, y) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
        if self.points.iter().any(|p| (p[0] - x).abs() < MIN_GAP) {
            return None;
        }
        let i = self.points.partition_point(|p| p[0] < x);
        self.points.insert(i, [x, y]);
        Some(i)
    }

    /// Move point `i`, keeping it between its neighbours in input.
    pub fn set(&mut self, i: usize, x: f32, y: f32) {
        let lo = if i == 0 {
            0.0
        } else {
            self.points[i - 1][0] + MIN_GAP
        };
        let hi = match self.points.get(i + 1) {
            Some(p) => p[0] - MIN_GAP,
            None => 1.0,
        };
        if let Some(p) = self.points.get_mut(i) {
            *p = [x.clamp(lo, hi.max(lo)), y.clamp(0.0, 1.0)];
        }
    }

    /// Remove point `i`, if the curve keeps at least two.
    pub fn remove(&mut self, i: usize) {
        if self.points.len() > 2 && i < self.points.len() {
            self.points.remove(i);
        }
    }

    /// The curve's output for input `x`.
    pub fn eval(&self, x: f32) -> f32 {
        let p = &self.points;
        let n = p.len();
        if x <= p[0][0] {
            return p[0][1];
        }
        if x >= p[n - 1][0] {
            return p[n - 1][1];
        }
        let k = p.partition_point(|q| q[0] <= x) - 1;
        let m = self.tangents();
        let (x0, y0, x1, y1) = (p[k][0], p[k][1], p[k + 1][0], p[k + 1][1]);
        let h = x1 - x0;
        let t = (x - x0) / h;
        let (t2, t3) = (t * t, t * t * t);
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        (h00 * y0 + h10 * h * m[k] + h01 * y1 + h11 * h * m[k + 1]).clamp(0.0, 1.0)
    }

    /// Fritsch–Carlson tangents: monotone between every pair of points.
    fn tangents(&self) -> Vec<f32> {
        let p = &self.points;
        let n = p.len();
        let d: Vec<f32> = p
            .windows(2)
            .map(|w| (w[1][1] - w[0][1]) / (w[1][0] - w[0][0]))
            .collect();
        let mut m = vec![0.0; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for k in 1..n - 1 {
            m[k] = if d[k - 1] * d[k] <= 0.0 {
                0.0
            } else {
                (d[k - 1] + d[k]) / 2.0
            };
        }
        for k in 0..n - 1 {
            if d[k] == 0.0 {
                m[k] = 0.0;
                m[k + 1] = 0.0;
                continue;
            }
            let (a, b) = (m[k] / d[k], m[k + 1] / d[k]);
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[k] = t * a * d[k];
                m[k + 1] = t * b * d[k];
            }
        }
        m
    }
}

/// Selective color for one hue range: hue moved by `hue_deg` (towards the next range up
/// for +), saturation and lightness scaled by −1 to +1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HueAdjust {
    pub hue_deg: f32,
    pub saturation: f32,
    pub lightness: f32,
}

impl HueAdjust {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }
}

/// Grain and color noise reduction, 0 (off) to 1 each.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseReduction {
    /// Smooths brightness: film grain.
    pub grain: f32,
    /// Smooths color only, over twice the radius: the blotchy color noise of dense
    /// negatives and dark slides.
    pub color: f32,
    /// The size of what is smoothed, in pixels of the saved image.
    pub radius_px: f32,
}

impl Default for NoiseReduction {
    fn default() -> Self {
        Self {
            grain: 0.0,
            color: 0.0,
            radius_px: 2.0,
        }
    }
}

/// Unsharp masking of brightness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sharpen {
    /// How much of the difference from the blurred image is added: 0 (off) to 3.
    pub amount: f32,
    /// The blur's standard deviation, in pixels of the saved image.
    pub radius_px: f32,
    /// Differences smaller than this (0–1 of full scale) are left alone, so grain and noise
    /// aren't sharpened.
    pub threshold: f32,
}

impl Default for Sharpen {
    fn default() -> Self {
        Self {
            amount: 0.0,
            radius_px: 1.0,
            threshold: 0.02,
        }
    }
}

/// Everything the Adjust step sets for one frame (or the whole scan). The default changes
/// nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Adjustments {
    /// Brighter (+) or darker (−), in stops, on top of the roll's printing.
    pub exposure_ev: f32,
    /// More (+) or less (−) red, green, blue, in stops, on top of the roll's printing.
    pub balance_ev: [f32; 3],
    /// [`MASTER`], then red, green, blue.
    pub levels: [Levels; 4],
    /// [`MASTER`], then red, green, blue.
    pub curves: [Curve; 4],
    /// −1 to 1: midtones darker or lighter, black and white kept.
    pub brightness: f32,
    /// −1 to 1: an S-curve about the middle grey (+), or its inverse.
    pub contrast: f32,
    /// −1 (grey) to 1 (twice the chroma).
    pub saturation: f32,
    /// One per [`HUE_RANGES`].
    pub selective: [HueAdjust; 6],
    pub noise: NoiseReduction,
    pub sharpen: Sharpen,
    /// Clockwise, after mirroring, on top of the scan's own turn.
    pub quarter_turns: u8,
    /// Flipped left to right, before turning.
    pub mirror: bool,
}

impl Adjustments {
    /// The frame's printing: the roll's, with this frame's exposure and balance added.
    pub fn printing(&self, roll: &Printing) -> Printing {
        Printing {
            exposure_ev: roll.exposure_ev + self.exposure_ev,
            balance_ev: std::array::from_fn(|c| roll.balance_ev[c] + self.balance_ev[c]),
            ..*roll
        }
    }

    /// The factor on each channel's linear light of the exposure and balance.
    pub fn gains(&self) -> [f32; 3] {
        self.balance_ev.map(|b| 2f32.powf(self.exposure_ev + b))
    }

    /// Whether these change nothing.
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// Whether [`apply`] changes anything: everything but exposure, balance and orientation.
    pub fn edits_positive(&self) -> bool {
        tone_changes(self)
            || color_changes(self)
            || self.noise.grain > 0.0
            || self.noise.color > 0.0
            || self.sharpen.amount > 0.0
    }
}

/// A frame's positive (16-bit sRGB): `image`, a scan encoding light with `gamma`, inverted
/// by `inverter` (which must already have the frame's printing, see
/// [`Adjustments::printing`]) or, without one, taken as a positive and given the
/// adjustments' gains; then edited by [`apply`]. `scale` is the image's size relative to the
/// saved one (1 when saving).
pub fn develop(
    image: &DynamicImage,
    gamma: f32,
    inverter: Option<&Inverter>,
    adjust: &Adjustments,
    scale: f32,
) -> Rgb16Image {
    let positive = match inverter {
        Some(inverter) => invert(image, gamma, inverter),
        None => gained(image, gamma, adjust.gains()),
    };
    apply(positive, adjust, scale)
}

/// `image` with each channel's linear light (decoded with `gamma`) multiplied by its gain.
fn gained(image: &DynamicImage, gamma: f32, gains: [f32; 3]) -> Rgb16Image {
    let mut rgb = image.to_rgb16();
    if gains == [1.0; 3] {
        return rgb;
    }
    let decode = decode_table(gamma);
    let gamma = if gamma > 0.0 { gamma } else { 1.0 };
    let tables: [Vec<u16>; 3] = std::array::from_fn(|c| {
        decode
            .iter()
            .map(|&l| to_u16((l * gains[c]).min(1.0).powf(1.0 / gamma)))
            .collect()
    });
    par_pixels(&mut rgb, |_, p| {
        for c in 0..3 {
            p[c] = tables[c][usize::from(p[c])];
        }
    });
    rgb
}

/// The positive `image` edited by `adjust`: noise reduction, tone, color, sharpening (see
/// the module). `scale` is the image's size relative to the saved one, which the spatial
/// steps are sized for. Exposure, balance and orientation are left to [`develop`] and
/// [`orient`].
pub fn apply(mut image: Rgb16Image, adjust: &Adjustments, scale: f32) -> Rgb16Image {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w == 0 || h == 0 {
        return image;
    }
    if adjust.noise.grain > 0.0 || adjust.noise.color > 0.0 {
        denoise(&mut image, &adjust.noise, scale);
    }
    if tone_changes(adjust) {
        let tables = tone_tables(adjust);
        par_pixels(&mut image, |_, p| {
            for c in 0..3 {
                p[c] = tables[c][usize::from(p[c])];
            }
        });
    }
    if color_changes(adjust) {
        let (saturation, selective) = (adjust.saturation, adjust.selective);
        par_pixels(&mut image, |_, p| {
            let rgb = recolor(p.map(|v| f32::from(v) / 65535.0), saturation, &selective);
            *p = rgb.map(to_u16);
        });
    }
    if adjust.sharpen.amount > 0.0 {
        sharpen(&mut image, &adjust.sharpen, scale);
    }
    image
}

fn tone_changes(a: &Adjustments) -> bool {
    a.levels.iter().any(|l| *l != Levels::default())
        || a.curves.iter().any(|c| !c.is_identity())
        || a.brightness != 0.0
        || a.contrast != 0.0
}

fn color_changes(a: &Adjustments) -> bool {
    a.saturation != 0.0 || a.selective.iter().any(|s| !s.is_identity())
}

/// Steepness of the contrast S-curve at full contrast.
const CONTRAST_K: f32 = 8.0;

/// One table per channel, of every 16-bit value: levels (the channel's, then the master),
/// curves (likewise), brightness, contrast.
fn tone_tables(a: &Adjustments) -> [Vec<u16>; 3] {
    let sig = |x: f32| 1.0 / (1.0 + (-CONTRAST_K * (x - 0.5)).exp());
    let (s0, s1) = (sig(0.0), sig(1.0));
    let brightness = 2f32.powf(-a.brightness.clamp(-1.0, 1.0));
    let contrast = a.contrast.clamp(-1.0, 1.0);
    let curve = |v: f32| {
        let s = (sig(v) - s0) / (s1 - s0);
        let t = (s0 + v * (s1 - s0)).clamp(1e-6, 1.0 - 1e-6);
        let inverse = (0.5 + (t / (1.0 - t)).ln() / CONTRAST_K).clamp(0.0, 1.0);
        if contrast >= 0.0 {
            v + (s - v) * contrast
        } else {
            v + (inverse - v) * -contrast
        }
    };
    std::array::from_fn(|c| {
        let (levels, curves) = (
            (&a.levels[c + 1], &a.levels[MASTER]),
            (&a.curves[c + 1], &a.curves[MASTER]),
        );
        (0..=65535u32)
            .map(|i| {
                let v = i as f32 / 65535.0;
                let v = levels.1.apply(levels.0.apply(v));
                let v = curves.1.eval(curves.0.eval(v));
                to_u16(curve(v.clamp(0.0, 1.0).powf(brightness)))
            })
            .collect()
    })
}

/// Opponent coordinates of a color: its mean, and its chroma as a vector whose angle is the
/// hue (red 0°, yellow 60°, ... magenta 300°, as in HSV).
fn opponent(p: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = p;
    [
        (r + g + b) / 3.0,
        r - (g + b) / 2.0,
        3f32.sqrt() / 2.0 * (g - b),
    ]
}

fn from_opponent([m, u, v]: [f32; 3]) -> [f32; 3] {
    let s = v / 3f32.sqrt();
    [m + 2.0 * u / 3.0, m - u / 3.0 + s, m - u / 3.0 - s]
}

/// How much each hue range takes of `hue_deg`: raised cosines 120° wide on the ranges'
/// centres, which add up to 1 at every hue.
pub fn hue_weights(hue_deg: f32) -> [f32; 6] {
    std::array::from_fn(|k| {
        let d = (hue_deg - HUE_RANGES[k].1 + 180.0).rem_euclid(360.0) - 180.0;
        if d.abs() < 60.0 {
            0.5 * (1.0 + (PI * d / 60.0).cos())
        } else {
            0.0
        }
    })
}

/// Saturation and selective color of one pixel, 0–1 sRGB encoded.
fn recolor(p: [f32; 3], saturation: f32, selective: &[HueAdjust; 6]) -> [f32; 3] {
    let [m, u, v] = opponent(p);
    let chroma = u.hypot(v);
    let hue = v.atan2(u).to_degrees();
    let w = hue_weights(hue);
    // Greys have no hue to move: hue and lightness take hold as chroma grows.
    let colored = smoothstep(0.0, 0.08, chroma);
    let (mut dh, mut ds, mut dl) = (0.0, 0.0, 0.0);
    for (k, s) in selective.iter().enumerate() {
        dh += w[k] * s.hue_deg;
        ds += w[k] * s.saturation;
        dl += w[k] * s.lightness;
    }
    let (sin, cos) = (dh * colored).to_radians().sin_cos();
    let gain = ((1.0 + saturation) * (1.0 + ds)).max(0.0);
    let (u, v) = ((u * cos - v * sin) * gain, (u * sin + v * cos) * gain);
    let rgb = from_opponent([m, u, v]).map(|x| x.clamp(0.0, 1.0));
    let dl = (dl * colored).clamp(-1.0, 1.0);
    rgb.map(|x| {
        if dl >= 0.0 {
            x + (1.0 - x) * dl
        } else {
            x * (1.0 + dl)
        }
    })
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Guided-filter noise reduction (He, Sun and Tang, 2010) of the image's brightness and,
/// separately and over twice the radius, its color.
fn denoise(image: &mut Rgb16Image, nr: &NoiseReduction, scale: f32) {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let mut planes: [Vec<f32>; 3] = std::array::from_fn(|_| Vec::with_capacity(w * h));
    for p in image.as_raw().as_chunks::<3>().0 {
        let o = opponent(p.map(|v| f32::from(v) / 65535.0));
        for c in 0..3 {
            planes[c].push(o[c]);
        }
    }
    let radius = |r: f32| ((r * scale).round() as usize).max(1);
    if nr.grain > 0.0 {
        let eps = (0.06 * nr.grain).powi(2);
        planes[0] = guided(&planes[0], w, h, radius(nr.radius_px), eps);
    }
    if nr.color > 0.0 {
        let eps = (0.1 * nr.color).powi(2);
        for plane in &mut planes[1..] {
            *plane = guided(plane, w, h, radius(2.0 * nr.radius_px), eps);
        }
    }
    par_pixels(image, |i, p| {
        *p = from_opponent([planes[0][i], planes[1][i], planes[2][i]]).map(to_u16);
    });
}

/// The guided filter of `p` by itself: smooths where `p` varies less than about
/// `sqrt(eps)` over `radius`, and keeps edges where it varies more.
fn guided(p: &[f32], w: usize, h: usize, radius: usize, eps: f32) -> Vec<f32> {
    let squares: Vec<f32> = p.iter().map(|v| v * v).collect();
    let mean = box_blur(p, w, h, radius);
    let mean_sq = box_blur(&squares, w, h, radius);
    let a: Vec<f32> = mean
        .iter()
        .zip(&mean_sq)
        .map(|(m, s)| {
            let var = (s - m * m).max(0.0);
            var / (var + eps)
        })
        .collect();
    let b: Vec<f32> = mean.iter().zip(&a).map(|(m, a)| m - a * m).collect();
    let (a, b) = (box_blur(&a, w, h, radius), box_blur(&b, w, h, radius));
    (0..p.len()).map(|i| a[i] * p[i] + b[i]).collect()
}

/// The mean of `src` (w × h) over a (2r+1)² box, the box clipped at the edges: running
/// sums across each row, then down the columns.
fn box_blur(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut across = vec![0.0; w * h];
    par_bands(&mut across, w, |y0, band| {
        for (j, row) in band.chunks_mut(w).enumerate() {
            let line = &src[(y0 + j) * w..][..w];
            let mut sum: f64 = line[..(r + 1).min(w)].iter().map(|&v| f64::from(v)).sum();
            for x in 0..w {
                let n = (x + r + 1).min(w) - x.saturating_sub(r);
                row[x] = (sum / n as f64) as f32;
                if x + r + 1 < w {
                    sum += f64::from(line[x + r + 1]);
                }
                if x >= r {
                    sum -= f64::from(line[x - r]);
                }
            }
        }
    });
    let mut out = vec![0.0; w * h];
    par_bands(&mut out, w, |y0, band| {
        let row_of = |y: usize| &across[y * w..][..w];
        let mut acc = vec![0.0f64; w];
        let add = |acc: &mut [f64], y: usize, sign: f64| {
            for (a, &v) in acc.iter_mut().zip(row_of(y)) {
                *a += sign * f64::from(v);
            }
        };
        for y in y0.saturating_sub(r)..(y0 + r + 1).min(h) {
            add(&mut acc, y, 1.0);
        }
        for (j, row) in band.chunks_mut(w).enumerate() {
            let y = y0 + j;
            let n = ((y + r + 1).min(h) - y.saturating_sub(r)) as f64;
            for (o, a) in row.iter_mut().zip(&acc) {
                *o = (a / n) as f32;
            }
            if y + r + 1 < h {
                add(&mut acc, y + r + 1, 1.0);
            }
            if y >= r {
                add(&mut acc, y - r, -1.0);
            }
        }
    });
    out
}

/// `src` (w × h) blurred with a Gaussian of standard deviation `sigma`, clamped at the
/// edges: across each row, then down the columns.
fn gaussian_blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let r = (3.0 * sigma).ceil().max(1.0) as usize;
    let kernel: Vec<f32> = (0..=2 * r)
        .map(|i| (-((i as f32 - r as f32).powi(2)) / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / total).collect();
    let mut across = vec![0.0; w * h];
    par_bands(&mut across, w, |y0, band| {
        for (j, row) in band.chunks_mut(w).enumerate() {
            let line = &src[(y0 + j) * w..][..w];
            for (x, o) in row.iter_mut().enumerate() {
                *o = kernel
                    .iter()
                    .enumerate()
                    .map(|(i, k)| k * line[(x + i).saturating_sub(r).min(w - 1)])
                    .sum();
            }
        }
    });
    let mut out = vec![0.0; w * h];
    par_bands(&mut out, w, |y0, band| {
        for (j, row) in band.chunks_mut(w).enumerate() {
            for (i, k) in kernel.iter().enumerate() {
                let y = (y0 + j + i).saturating_sub(r).min(h - 1);
                for (o, &v) in row.iter_mut().zip(&across[y * w..][..w]) {
                    *o += k * v;
                }
            }
        }
    });
    out
}

/// Unsharp masking of the image's brightness (the mean of its channels), added equally to
/// every channel.
fn sharpen(image: &mut Rgb16Image, s: &Sharpen, scale: f32) {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let light: Vec<f32> = image
        .as_raw()
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| p.iter().map(|&v| f32::from(v)).sum::<f32>() / (3.0 * 65535.0))
        .collect();
    let blurred = gaussian_blur(&light, w, h, (s.radius_px * scale).max(0.2));
    let (amount, t) = (s.amount.clamp(0.0, 3.0), s.threshold.max(0.0));
    par_pixels(image, |i, p| {
        let d = light[i] - blurred[i];
        // A soft threshold, so detail doesn't switch on with a jump.
        let k = if t > 0.0 {
            smoothstep(0.5 * t, t, d.abs())
        } else {
            1.0
        };
        let add = amount * d * k * 65535.0;
        *p = p.map(|v| (f32::from(v) + add).round().clamp(0.0, 65535.0) as u16);
    });
}

/// Run `f` on every pixel of `image`, with its index, on every core.
fn par_pixels(image: &mut Rgb16Image, f: impl Fn(usize, &mut [u16; 3]) + Sync) {
    let data: &mut [u16] = image;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let band = (data.len() / 3).div_ceil(threads).max(65536);
    std::thread::scope(|s| {
        for (b, chunk) in data.chunks_mut(band * 3).enumerate() {
            let f = &f;
            s.spawn(move || {
                for (i, p) in chunk.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                    f(b * band + i, p);
                }
            });
        }
    });
}

/// Run `f` on bands of whole rows (of width `w`) of `out`, with each band's first row, on
/// every core.
fn par_bands(out: &mut [f32], w: usize, f: impl Fn(usize, &mut [f32]) + Sync) {
    let h = out.len() / w.max(1);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let rows = h.div_ceil(threads).max(16);
    std::thread::scope(|s| {
        for (b, band) in out.chunks_mut(rows * w.max(1)).enumerate() {
            let f = &f;
            s.spawn(move || f(b * rows, band));
        }
    });
}

/// `image` flipped left to right if `mirror`, then turned `quarter_turns` clockwise.
pub fn orient(image: &DynamicImage, quarter_turns: u8, mirror: bool) -> Cow<'_, DynamicImage> {
    let image = if mirror {
        Cow::Owned(image.fliph())
    } else {
        Cow::Borrowed(image)
    };
    match quarter_turns % 4 {
        0 => image,
        1 => Cow::Owned(image.rotate90()),
        2 => Cow::Owned(image.rotate180()),
        _ => Cow::Owned(image.rotate270()),
    }
}

/// Counts of 256 levels: [`MASTER`] holds brightness (Rec. 709 weights of the encoded
/// values), then red, green, blue.
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub bins: [[u32; 256]; 4],
}

/// The histogram of `image`, from at most about a million of its pixels.
pub fn histogram(image: &Rgb16Image) -> Histogram {
    let mut bins = [[0u32; 256]; 4];
    let pixels = image.as_raw().as_chunks::<3>().0;
    let step = (pixels.len() / 1_000_000).max(1);
    for p in pixels.iter().step_by(step) {
        for c in 0..3 {
            bins[c + 1][usize::from(p[c] >> 8)] += 1;
        }
        let y = 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]);
        bins[MASTER][(y as usize >> 8).min(255)] += 1;
    }
    Histogram { bins }
}

/// Master levels that stretch the image's range to full scale, as SilverFast's and
/// Photoshop's auto levels do: black where `clip` (a fraction, e.g. 0.001) of the darkest
/// channel's pixels are darker, white where `clip` of the lightest's are lighter. One
/// stretch for all three channels, so the color balance is kept.
pub fn auto_levels(histogram: &Histogram, clip: f32) -> Levels {
    let (mut black, mut white) = (255usize, 0usize);
    for bins in &histogram.bins[1..] {
        let n: u64 = bins.iter().map(|&b| u64::from(b)).sum();
        if n == 0 {
            continue;
        }
        let limit = (clip as f64 * n as f64) as u64;
        let edge = |order: &mut dyn Iterator<Item = usize>| {
            let mut sum = 0u64;
            for i in order {
                sum += u64::from(bins[i]);
                if sum > limit {
                    return i;
                }
            }
            0
        };
        black = black.min(edge(&mut (0..256)));
        white = white.max(edge(&mut (0..256).rev()));
    }
    let (b, w) = (black as f32 / 256.0, (white + 1) as f32 / 256.0);
    if w - b < 0.1 {
        return Levels::default();
    }
    Levels {
        black: b,
        white: w,
        ..Levels::default()
    }
}

/// The change of color balance, in stops per channel, that makes light `e` (linear, or a
/// negative's scene light) neutral, keeping its overall level.
pub fn neutralize(e: [f32; 3]) -> [f32; 3] {
    let l = e.map(|v| v.max(1e-6).log2());
    let mean = l.iter().sum::<f32>() / 3.0;
    l.map(|v| mean - v)
}

/// The mean log2 linear light of each channel of `image` (encoded with `gamma`), from at
/// most about 100,000 pixels, ignoring clipped ones: for balancing a positive to grey.
pub fn log_means(image: &DynamicImage, gamma: f32) -> [f32; 3] {
    let rgb: Cow<Rgb16Image> = match image {
        DynamicImage::ImageRgb16(i) => Cow::Borrowed(i),
        other => Cow::Owned(other.to_rgb16()),
    };
    let decode = decode_table(gamma);
    let pixels = rgb.as_raw().as_chunks::<3>().0;
    let step = (pixels.len() / 100_000).max(1);
    let (mut sum, mut n) = ([0.0f64; 3], 0usize);
    for p in pixels.iter().step_by(step) {
        if p.iter().any(|v| !(256..=65_000).contains(v)) {
            continue;
        }
        for c in 0..3 {
            sum[c] += f64::from(decode[usize::from(p[c])].log2());
        }
        n += 1;
    }
    sum.map(|s| if n > 0 { (s / n as f64) as f32 } else { 0.0 })
}

#[cfg(test)]
mod tests;
