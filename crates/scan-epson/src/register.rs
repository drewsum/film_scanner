//! Lining the infrared pass up with the color one.
//!
//! The infrared plane is a second session's read: the carriage lands from its own move, and
//! where it lands varies by 0.1 to 0.35 mm between sessions on a V500, against the 0.015 mm
//! of a 1,600 dpi pixel. Above 2,400 dpi the infrared is also a different hardware mode's read
//! ([`crate::film`]). Nor do the two passes keep step down the holder: on a whole holder the
//! infrared drifts from the color by up to 0.15 mm over the first third of its length. So
//! [`infrared_offsets`] measures, after the scan, how far the infrared's detail is from the
//! color's (both show the film's grain and edges, and the dust) in bands down the scan, and
//! [`shifted`] moves each row of it by the offset there.
//!
//! The measure is the normalised correlation of the two planes' fine detail (log light less
//! its local mean), searched coarse to fine: over the whole scan at most [`COARSE_SIDE`]
//! pixels on a side (and few enough that the search reaches [`COARSE_REACH`] of them); then,
//! in each band, over the [`TILES`] parts of it with the most detail, from as coarse a level
//! as reaches [`BAND_REACH`] of the scan's reach, at each finer power of two within a pixel or
//! two of the last estimate, to the scan's own pixels, and to a fraction of one by fitting a
//! parabola through the best and its neighbours. A running median over the bands' offsets
//! drops those that went astray.

use scan_core::Gray16Image;
use scan_core::image::{DynamicImage, GenericImageView, ImageBuffer, Luma};

/// The coarse search's planes are at most this many pixels on their longer side.
const COARSE_SIDE: u32 = 1024;
/// ...and its search reaches at most this many of their pixels.
const COARSE_REACH: f32 = 8.0;
/// A band's parts are at most this many pixels across.
const TILE: u32 = 1024;
/// A band is measured on at most this many of its parts.
const TILES: usize = 4;
/// A band's offset is searched this fraction of the whole scan's reach either side of the
/// whole scan's (and the coarse search's pixel more).
const BAND_REACH: f32 = 0.25;
/// The running median over the bands' offsets takes this many either side.
const MEDIAN_REACH: usize = 2;
/// Correlations are summed over at most this many pixels of each part.
const SAMPLES: usize = 250_000;
/// Fine detail: light less its mean over this many pixels (at each level) either side.
const DETAIL_RADIUS: usize = 3;
/// The least correlation of the best shift to trust it.
const MIN_SCORE: f64 = 0.05;

/// How far the infrared's detail is from the visible's, in pixels across and down, row by
/// row: the visible at p shows what the infrared shows at p + offset.
#[derive(Clone, Debug, PartialEq)]
pub struct Offsets {
    /// (row, offset), by row; between them the offset is interpolated, beyond them the
    /// nearest's.
    knots: Vec<(f32, [f32; 2])>,
}

impl Offsets {
    /// The same offset everywhere.
    pub fn uniform(offset: [f32; 2]) -> Self {
        Self {
            knots: vec![(0.0, offset)],
        }
    }

    /// The measured offsets, by row.
    pub fn knots(&self) -> &[(f32, [f32; 2])] {
        &self.knots
    }

    /// The offset at row `y`.
    pub fn at(&self, y: f32) -> [f32; 2] {
        let k = &self.knots;
        let next = k.partition_point(|&(row, _)| row <= y);
        if next == 0 {
            return k[0].1;
        }
        if next == k.len() {
            return k[next - 1].1;
        }
        let ((y0, a), (y1, b)) = (k[next - 1], k[next]);
        let t = (y - y0) / (y1 - y0);
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
    }
}

/// How far the infrared's detail is from `visible`'s ([`Offsets`]), measured in bands `band`
/// rows tall. `None` when no shift within `max_shift` pixels either way stands out over the
/// whole scan (a frame without detail, or planes of different sizes); bands without detail
/// take their neighbours' offsets.
pub fn infrared_offsets(
    visible: &DynamicImage,
    infrared: &Gray16Image,
    max_shift: f32,
    band: u32,
) -> Option<Offsets> {
    let (w, h) = infrared.dimensions();
    if visible.dimensions() != (w, h) || w < 16 || h < 16 {
        return None;
    }
    let light = luminance(visible)?;
    let ir = |x: u32, y: u32| f32::from(infrared.get_pixel(x, y).0[0]) / 65535.0;
    let full = Rect { x: 0, y: 0, w, h };

    // Coarse: the whole scan.
    let mut factor = 1;
    while w.max(h) / factor > COARSE_SIDE || max_shift / factor as f32 > COARSE_REACH {
        factor *= 2;
    }
    let radius = (max_shift / factor as f32).ceil() as i64 + 1;
    let whole = [Pair {
        a: Plane::new(&light, full, factor),
        b: Plane::new(&ir, full, factor),
        origin: (0, 0),
    }];
    let (d, score) = search(&whole, (0, 0), radius)?;
    log::debug!("infrared offset at 1/{factor}: {d:?} (score {score:.3})");
    if score < MIN_SCORE {
        return None;
    }
    let coarse = [
        (d.0 * i64::from(factor)) as f32,
        (d.1 * i64::from(factor)) as f32,
    ];

    // Each band, on its parts with the most detail in both planes.
    let tile = TILE.min(w);
    let band = band.clamp(16, h);
    let reach = max_shift * BAND_REACH + factor as f32;
    let mut found = Vec::new();
    for y in (0..h).step_by(band as usize) {
        let rows = band.min(h - y);
        let mut tiles: Vec<(f64, Rect)> = (0..w.div_ceil(tile))
            .map(|t| {
                let rect = Rect {
                    x: t * tile,
                    y,
                    w: tile.min(w - t * tile),
                    h: rows,
                };
                (detail_in(&whole[0], rect, factor), rect)
            })
            .filter(|&(energy, _)| energy > 0.0)
            .collect();
        tiles.sort_by(|p, q| q.0.total_cmp(&p.0));
        let tiles: Vec<Rect> = tiles.into_iter().take(TILES).map(|(_, r)| r).collect();
        match refine(&light, &ir, &tiles, full, coarse, reach) {
            Some((offset, score)) if score >= MIN_SCORE => {
                log::debug!("infrared offset at rows {y}..: {offset:.2?} (score {score:.3})");
                found.push((y as f32 + rows as f32 / 2.0, offset));
            }
            other => log::debug!("infrared offset at rows {y}..: none ({other:.3?})"),
        }
    }
    if found.is_empty() {
        log::debug!("no band of the infrared stands out; offset {coarse:?} throughout");
        return Some(Offsets::uniform(coarse));
    }
    // A running median, axis by axis, reaching as far each way (so a drift doesn't bias the
    // ends).
    let median = |near: &[(f32, [f32; 2])], axis: usize| {
        let mut v: Vec<f32> = near.iter().map(|(_, o)| o[axis]).collect();
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    let knots = (0..found.len())
        .map(|i| {
            let r = MEDIAN_REACH.min(i).min(found.len() - 1 - i);
            let near = &found[i - r..=i + r];
            (found[i].0, [median(near, 0), median(near, 1)])
        })
        .collect();
    Some(Offsets { knots })
}

/// How much fine detail the planes of `whole` (the whole scan at `factor`) have in common in
/// `rect`: the sum of their detail's magnitudes' products.
fn detail_in(whole: &Pair, rect: Rect, factor: u32) -> f64 {
    let (a, b) = (&whole.a, &whole.b);
    let f = factor as usize;
    let (x0, y0) = (rect.x as usize / f, rect.y as usize / f);
    let (x1, y1) = (
        ((rect.x + rect.w) as usize / f).min(a.w),
        ((rect.y + rect.h) as usize / f).min(a.h),
    );
    let mut sum = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = y * a.w + x;
            sum += f64::from((a.detail[i] * b.detail[i]).abs());
        }
    }
    sum
}

/// The offset over `tiles` within `reach` pixels of `around`, and its correlation, searched
/// coarse to fine, as fine as the planes still correlate: the infrared read at a lower
/// resolution and scaled up hasn't the scan's finest detail.
fn refine(
    light: &dyn Fn(u32, u32) -> f32,
    ir: &dyn Fn(u32, u32) -> f32,
    tiles: &[Rect],
    full: Rect,
    around: [f32; 2],
    reach: f32,
) -> Option<([f32; 2], f64)> {
    let mut factor = 1u32;
    while reach / factor as f32 > COARSE_REACH {
        factor *= 2;
    }
    let mut d = (
        (around[0] / factor as f32).round() as i64,
        (around[1] / factor as f32).round() as i64,
    );
    let mut radius = (reach / factor as f32).ceil() as i64 + 1;
    let mut found = None;
    loop {
        let grow = (d.0.abs().max(d.1.abs()) + radius + 1) as u32 * factor;
        let pairs: Vec<Pair> = tiles
            .iter()
            .map(|&tile| {
                let grown = tile.grown(grow, factor, full);
                Pair {
                    a: Plane::new(light, tile, factor),
                    b: Plane::new(ir, grown, factor),
                    origin: (
                        (i64::from(grown.x) - i64::from(tile.x)) / i64::from(factor),
                        (i64::from(grown.y) - i64::from(tile.y)) / i64::from(factor),
                    ),
                }
            })
            .collect();
        let Some((best, score)) = search(&pairs, d, radius) else {
            return found;
        };
        if score < MIN_SCORE {
            return found.or(Some((
                [best.0 as f32, best.1 as f32].map(|v| v * factor as f32),
                score,
            )));
        }
        d = best;
        // A parabola through the best shift's score and its neighbours', along each axis.
        let at = |dx: i64, dy: i64| correlation(&pairs, (d.0 + dx, d.1 + dy));
        let mut offset = [d.0 as f32, d.1 as f32];
        for (axis, (m, p)) in [(at(-1, 0)?, at(1, 0)?), (at(0, -1)?, at(0, 1)?)]
            .into_iter()
            .enumerate()
        {
            let curve = m - 2.0 * score + p;
            if curve < 0.0 {
                offset[axis] += ((m - p) / (2.0 * curve)).clamp(-0.5, 0.5) as f32;
            }
        }
        found = Some((offset.map(|v| v * factor as f32), score));
        if factor == 1 {
            return found;
        }
        factor /= 2;
        d = (d.0 * 2, d.1 * 2);
        radius = 2;
    }
}

/// `infrared` moved by `offsets` ([`infrared_offsets`]): each pixel p takes its value at
/// p + the offset at its row, interpolated, the nearest edge's beyond it.
pub fn shifted(infrared: &Gray16Image, offsets: &Offsets) -> Gray16Image {
    let (w, h) = infrared.dimensions();
    let at = |x: i64, y: i64| {
        let (x, y) = (x.clamp(0, w as i64 - 1), y.clamp(0, h as i64 - 1));
        f32::from(infrared.get_pixel(x as u32, y as u32).0[0])
    };
    let split = |v: f32| (v.floor() as i64, v - v.floor());
    let rows: Vec<_> = (0..h)
        .map(|y| {
            let [dx, dy] = offsets.at(y as f32);
            (split(dx), split(dy))
        })
        .collect();
    ImageBuffer::from_fn(w, h, |x, y| {
        let ((ix, fx), (iy, fy)) = rows[y as usize];
        let (x, y) = (x as i64 + ix, y as i64 + iy);
        let top = at(x, y) * (1.0 - fx) + at(x + 1, y) * fx;
        let bottom = at(x, y + 1) * (1.0 - fx) + at(x + 1, y + 1) * fx;
        Luma([(top * (1.0 - fy) + bottom * fy).round() as u16])
    })
}

/// The mean of the channels at each pixel, 0–1.
fn luminance(image: &DynamicImage) -> Option<Box<dyn Fn(u32, u32) -> f32 + '_>> {
    let w = image.width() as usize;
    Some(match image {
        DynamicImage::ImageRgb16(i) => Box::new(move |x, y| {
            let at = (y as usize * w + x as usize) * 3;
            let p = &i.as_raw()[at..at + 3];
            (u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])) as f32 / (3.0 * 65535.0)
        }),
        DynamicImage::ImageRgb8(i) => Box::new(move |x, y| {
            let at = (y as usize * w + x as usize) * 3;
            let p = &i.as_raw()[at..at + 3];
            (u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])) as f32 / (3.0 * 255.0)
        }),
        DynamicImage::ImageLuma16(i) => {
            Box::new(move |x, y| f32::from(i.get_pixel(x, y).0[0]) / 65535.0)
        }
        _ => return None,
    })
}

/// A part of the scan, in its pixels.
#[derive(Clone, Copy, Debug)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Rect {
    /// This grown by `by` each way, within `bounds`, in whole steps of `step` from its own
    /// corner (so planes of both at `step` pixels to one line up).
    fn grown(self, by: u32, step: u32, bounds: Rect) -> Rect {
        let near = |at: u32| at - at.min(by) / step * step;
        let far = |at: u32, end: u32| at + (end - at).min(by) / step * step;
        let (x0, y0) = (near(self.x), near(self.y));
        let (x1, y1) = (
            far(self.x + self.w, bounds.w),
            far(self.y + self.h, bounds.h),
        );
        Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }
}

/// The fine detail of a part of the scan, `factor` pixels to each of its own.
struct Plane {
    w: usize,
    h: usize,
    detail: Vec<f32>,
}

impl Plane {
    fn new(light: &dyn Fn(u32, u32) -> f32, rect: Rect, factor: u32) -> Self {
        let (w, h) = ((rect.w / factor) as usize, (rect.h / factor) as usize);
        let n = (factor * factor) as f32;
        let log: Vec<f32> = (0..w * h)
            .map(|i| {
                let (px, py) = ((i % w) as u32 * factor, (i / w) as u32 * factor);
                let mut sum = 0.0;
                for y in 0..factor {
                    for x in 0..factor {
                        sum += light(rect.x + px + x, rect.y + py + y);
                    }
                }
                (sum / n + 1.0 / 256.0).ln()
            })
            .collect();
        // Less the mean about each pixel, from a summed-area table.
        let mut table = vec![0.0f64; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row = 0.0;
            for x in 0..w {
                row += f64::from(log[y * w + x]);
                table[(y + 1) * (w + 1) + x + 1] = table[y * (w + 1) + x + 1] + row;
            }
        }
        let r = DETAIL_RADIUS;
        let detail = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let (x0, y0) = (x.saturating_sub(r), y.saturating_sub(r));
                let (x1, y1) = ((x + r + 1).min(w), (y + r + 1).min(h));
                let sum =
                    table[y1 * (w + 1) + x1] - table[y0 * (w + 1) + x1] - table[y1 * (w + 1) + x0]
                        + table[y0 * (w + 1) + x0];
                let mean = sum / ((x1 - x0) * (y1 - y0)) as f64;
                log[i] - mean as f32
            })
            .collect();
        Self { w, h, detail }
    }
}

/// A part of the visible plane, `a`, and of the infrared about it, `b`, whose origin is
/// `origin` in `a`'s pixels.
struct Pair {
    a: Plane,
    b: Plane,
    origin: (i64, i64),
}

/// The normalised correlation of each pair's `a` with its `b` moved by `d`, all together: over
/// the pixels of `a` (a grid of them) whose moved point is in `b`, away from both edges.
/// `None` with too few such pixels.
fn correlation(pairs: &[Pair], d: (i64, i64)) -> Option<f64> {
    let edge = DETAIL_RADIUS + 1;
    let (mut ab, mut aa, mut bb, mut n) = (0.0f64, 0.0f64, 0.0f64, 0usize);
    for Pair { a, b, origin } in pairs {
        let step = (((a.w * a.h) as f64 / SAMPLES as f64).sqrt().ceil() as usize).max(1);
        for y in (edge..a.h.saturating_sub(edge)).step_by(step) {
            let by = y as i64 + d.1 - origin.1;
            if by < edge as i64 || by >= (b.h - edge.min(b.h)) as i64 {
                continue;
            }
            for x in (edge..a.w.saturating_sub(edge)).step_by(step) {
                let bx = x as i64 + d.0 - origin.0;
                if bx < edge as i64 || bx >= (b.w - edge.min(b.w)) as i64 {
                    continue;
                }
                let (va, vb) = (
                    f64::from(a.detail[y * a.w + x]),
                    f64::from(b.detail[by as usize * b.w + bx as usize]),
                );
                ab += va * vb;
                aa += va * va;
                bb += vb * vb;
                n += 1;
            }
        }
    }
    (n >= 64 && aa > 0.0 && bb > 0.0).then(|| ab / (aa * bb).sqrt())
}

/// The shift within `radius` of `around` that correlates the pairs best, and its score; `None`
/// if the best is on the edge of the search (the true one may lie beyond).
fn search(pairs: &[Pair], around: (i64, i64), radius: i64) -> Option<((i64, i64), f64)> {
    let mut best: Option<((i64, i64), f64)> = None;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let d = (around.0 + dx, around.1 + dy);
            if let Some(score) = correlation(pairs, d)
                && best.is_none_or(|(_, s)| score > s)
            {
                best = Some((d, score));
            }
        }
    }
    let (d, score) = best?;
    let inside = (d.0 - around.0).abs() < radius && (d.1 - around.1).abs() < radius;
    inside.then_some((d, score))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan_core::image::Rgb;

    /// Film-like detail: blobs of every size, from a fixed seed, drawn once on a `w` × `h`
    /// grid and read between its points by interpolation.
    fn texture(w: usize, h: usize) -> impl Fn(f32, f32) -> f32 {
        let mut seed = 12345u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 33) as f32 / (1u64 << 31) as f32
        };
        let mut grid = vec![0.5f32; w * h];
        for _ in 0..400 {
            let r = 1.5 + 12.0 * next() * next();
            let (cx, cy, a) = (next() * w as f32, next() * h as f32, next() - 0.5);
            let reach = (3.0 * r) as usize + 1;
            let (x0, y0) = (
                (cx as usize).saturating_sub(reach),
                (cy as usize).saturating_sub(reach),
            );
            for y in y0..(cy as usize + reach).min(h) {
                for x in x0..(cx as usize + reach).min(w) {
                    let d2 = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)) / (r * r);
                    grid[y * w + x] += 0.3 * a * (-d2).exp();
                }
            }
        }
        move |x, y| {
            let (ix, iy) = (x.floor() as usize, y.floor() as usize);
            let (fx, fy) = (x - x.floor(), y - y.floor());
            let at = |x: usize, y: usize| grid[y * w + x].clamp(0.05, 1.0);
            let top = at(ix, iy) * (1.0 - fx) + at(ix + 1, iy) * fx;
            let bottom = at(ix, iy + 1) * (1.0 - fx) + at(ix + 1, iy + 1) * fx;
            top * (1.0 - fy) + bottom * fy
        }
    }

    #[test]
    fn finds_and_undoes_the_infrared_offset() {
        let (w, h) = (600, 800);
        let film = texture(w as usize + 80, h as usize + 80);
        let visible = DynamicImage::ImageRgb16(ImageBuffer::from_fn(w, h, |x, y| {
            let v = (film(x as f32 + 40.0, y as f32 + 40.0) * 60000.0) as u16;
            Rgb([v, v / 2, v / 3])
        }));
        for offset in [[0.0, 0.0], [7.3, -21.6], [-12.5, 3.25]] {
            // The infrared shows the film `offset` further on, a little dimmer.
            let ir = ImageBuffer::from_fn(w, h, |x, y| {
                let v = film(x as f32 + 40.0 - offset[0], y as f32 + 40.0 - offset[1]);
                Luma([(v * 40000.0) as u16])
            });
            let found = infrared_offsets(&visible, &ir, 30.0, 200).expect("an offset");
            // visible(p) = ir(p + offset)
            for y in [0.0, 300.0, 799.0] {
                let at = found.at(y);
                for axis in 0..2 {
                    assert!(
                        (at[axis] - offset[axis]).abs() < 0.2,
                        "{offset:?}: found {at:?} at row {y}"
                    );
                }
            }
            let aligned = shifted(&ir, &found);
            let (x, y) = (300, 400);
            let want = film(x as f32 + 40.0, y as f32 + 40.0) * 40000.0;
            let got = f32::from(aligned.get_pixel(x, y).0[0]);
            assert!((got - want).abs() < 0.02 * 40000.0, "{got} vs {want}");
        }
        // Nothing to go on: a flat infrared plane.
        let flat = ImageBuffer::from_pixel(w, h, Luma([30000u16]));
        assert_eq!(infrared_offsets(&visible, &flat, 30.0, 200), None);
    }

    #[test]
    fn follows_an_offset_that_drifts_down_the_scan() {
        let (w, h) = (500, 1600);
        let film = texture(w as usize + 80, h as usize + 80);
        let visible = DynamicImage::ImageRgb16(ImageBuffer::from_fn(w, h, |x, y| {
            let v = (film(x as f32 + 40.0, y as f32 + 40.0) * 60000.0) as u16;
            Rgb([v, v, v])
        }));
        // Across, 3 px throughout; down, from -6 px at the top to +4 at the bottom.
        let (dx, dy0, slope) = (3.0f32, -6.0f32, 10.0 / h as f32);
        let ir = ImageBuffer::from_fn(w, h, |x, y| {
            // The visible row v lands on infrared row v + dy0 + slope * v.
            let v = (y as f32 - dy0) / (1.0 + slope);
            let film = film(x as f32 + 40.0 - dx, v + 40.0);
            Luma([(film * 40000.0) as u16])
        });
        let found = infrared_offsets(&visible, &ir, 30.0, 200).expect("offsets");
        assert!(found.knots().len() >= 6, "{found:?}");
        for y in (100..1500).step_by(100) {
            let at = found.at(y as f32);
            let want = [dx, dy0 + slope * y as f32];
            for axis in 0..2 {
                assert!(
                    (at[axis] - want[axis]).abs() < 0.35,
                    "row {y}: found {at:?}, want {want:?}"
                );
            }
        }
        let aligned = shifted(&ir, &found);
        let (x, y) = (250, 1200);
        let want = film(x as f32 + 40.0, y as f32 + 40.0) * 40000.0;
        let got = f32::from(aligned.get_pixel(x, y).0[0]);
        assert!((got - want).abs() < 0.03 * 40000.0, "{got} vs {want}");
    }

    #[test]
    fn interpolates_between_knots() {
        let o = Offsets {
            knots: vec![(100.0, [0.0, 2.0]), (300.0, [4.0, -2.0])],
        };
        assert_eq!(o.at(0.0), [0.0, 2.0]);
        assert_eq!(o.at(200.0), [2.0, 0.0]);
        assert_eq!(o.at(1000.0), [4.0, -2.0]);
        assert_eq!(Offsets::uniform([1.0, 2.0]).at(50.0), [1.0, 2.0]);
    }
}
