//! Lining the infrared pass up with the color one.
//!
//! The infrared plane is a second session's read: the carriage lands from its own move, and
//! where it lands varies by 0.1 to 0.35 mm between sessions on a V500, against the 0.015 mm
//! of a 1,600 dpi pixel. Above 2,400 dpi the infrared is also a different hardware mode's read
//! ([`crate::film`]). So [`infrared_offset`] measures, after the scan, how far the infrared's
//! detail is from the color's (both show the film's grain and edges, and the dust), and
//! [`shifted`] moves it there.
//!
//! The measure is the normalised correlation of the two planes' fine detail (log light less
//! its local mean), searched coarse to fine: over the whole scan at most [`COARSE_SIDE`]
//! pixels on a side (and few enough that the search reaches [`COARSE_REACH`] of them), then at each finer power of two within a pixel or two of the last
//! estimate, on a window about the middle, to the scan's own pixels, and to a fraction of one
//! by fitting a parabola through the best and its neighbours.

use scan_core::Gray16Image;
use scan_core::image::{DynamicImage, GenericImageView, ImageBuffer, Luma};

/// The coarse search's planes are at most this many pixels on their longer side.
const COARSE_SIDE: u32 = 1024;
/// ...and its search reaches at most this many of their pixels.
const COARSE_REACH: f32 = 8.0;
/// The finer searches' windows are at most this many pixels on a side.
const WINDOW: u32 = 1024;
/// Correlations are summed over at most this many pixels.
const SAMPLES: usize = 250_000;
/// Fine detail: light less its mean over this many pixels (at each level) either side.
const DETAIL_RADIUS: usize = 3;
/// The least correlation of the best shift to trust it.
const MIN_SCORE: f64 = 0.05;

/// How far the infrared's detail is from `visible`'s, in pixels across and down: `visible` at
/// p shows what `infrared` shows at p + offset. `None` when no shift within `max_shift` pixels
/// either way stands out (a frame without detail, or planes of different sizes).
pub fn infrared_offset(
    visible: &DynamicImage,
    infrared: &Gray16Image,
    max_shift: f32,
) -> Option<[f32; 2]> {
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
    let a = Plane::new(&light, full, factor);
    let b = Plane::new(&ir, full, factor);
    let (mut d, score) = search(&a, &b, (0, 0), (0, 0), radius)?;
    log::debug!("infrared offset at 1/{factor}: {d:?} (score {score:.3})");
    if score < MIN_SCORE {
        return None;
    }
    // Finer: a window about the middle, the infrared's moved by the estimate so far.
    let mut fit = (factor == 1).then_some((a, b, (0, 0)));
    while factor > 1 {
        factor /= 2;
        d = (d.0 * 2, d.1 * 2);
        let (ww, wh) = ((WINDOW * factor).min(w), (WINDOW * factor).min(h));
        let window = Rect {
            x: (w - ww) / 2,
            y: (h - wh) / 2,
            w: ww,
            h: wh,
        };
        let reach = (d.0.abs().max(d.1.abs()) + 3) as u32 * factor;
        let grown = window.grown(reach, factor, full);
        let a = Plane::new(&light, window, factor);
        let b = Plane::new(&ir, grown, factor);
        let origin = (
            (i64::from(grown.x) - i64::from(window.x)) / i64::from(factor),
            (i64::from(grown.y) - i64::from(window.y)) / i64::from(factor),
        );
        let (best, score) = search(&a, &b, origin, d, 2)?;
        log::debug!("infrared offset at 1/{factor}: {best:?} (score {score:.3})");
        d = best;
        if factor == 1 {
            fit = Some((a, b, origin));
        }
    }
    let mut offset = [d.0 as f32, d.1 as f32];
    if let Some((a, b, origin)) = fit {
        // A parabola through the best shift's score and its neighbours', along each axis.
        let at = |dx: i64, dy: i64| correlation(&a, &b, origin, (d.0 + dx, d.1 + dy));
        let c = at(0, 0)?;
        for (axis, (m, p)) in [(at(-1, 0)?, at(1, 0)?), (at(0, -1)?, at(0, 1)?)]
            .into_iter()
            .enumerate()
        {
            let curve = m - 2.0 * c + p;
            if curve < 0.0 {
                offset[axis] += ((m - p) / (2.0 * curve)).clamp(-0.5, 0.5) as f32;
            }
        }
    } else {
        offset = offset.map(|v| v * factor as f32);
    }
    Some(offset)
}

/// `infrared` moved by `offset` ([`infrared_offset`]): each pixel p takes its value at
/// p + offset, interpolated, the nearest edge's beyond it.
pub fn shifted(infrared: &Gray16Image, [dx, dy]: [f32; 2]) -> Gray16Image {
    let (w, h) = infrared.dimensions();
    let at = |x: i64, y: i64| {
        let (x, y) = (x.clamp(0, w as i64 - 1), y.clamp(0, h as i64 - 1));
        f32::from(infrared.get_pixel(x as u32, y as u32).0[0])
    };
    let (ix, fx) = (dx.floor() as i64, dx - dx.floor());
    let (iy, fy) = (dy.floor() as i64, dy - dy.floor());
    ImageBuffer::from_fn(w, h, |x, y| {
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

/// The normalised correlation of `a` with `b` moved by `d`, where `b`'s origin is `origin` in
/// `a`'s pixels: over the pixels of `a` (a grid of them) whose moved point is in `b`, away
/// from both edges. `None` with too few such pixels.
fn correlation(a: &Plane, b: &Plane, origin: (i64, i64), d: (i64, i64)) -> Option<f64> {
    let edge = DETAIL_RADIUS + 1;
    let step = (((a.w * a.h) as f64 / SAMPLES as f64).sqrt().ceil() as usize).max(1);
    let (mut ab, mut aa, mut bb, mut n) = (0.0f64, 0.0f64, 0.0f64, 0usize);
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
    (n >= 64 && aa > 0.0 && bb > 0.0).then(|| ab / (aa * bb).sqrt())
}

/// The shift within `radius` of `around` that correlates `a` and `b` best, and its score;
/// `None` if the best is on the edge of the search (the true one may lie beyond).
fn search(
    a: &Plane,
    b: &Plane,
    origin: (i64, i64),
    around: (i64, i64),
    radius: i64,
) -> Option<((i64, i64), f64)> {
    let mut best: Option<((i64, i64), f64)> = None;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let d = (around.0 + dx, around.1 + dy);
            if let Some(score) = correlation(a, b, origin, d)
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
            let found = infrared_offset(&visible, &ir, 30.0).expect("an offset");
            // visible(p) = ir(p + offset)
            for axis in 0..2 {
                assert!(
                    (found[axis] - offset[axis]).abs() < 0.2,
                    "{offset:?}: found {found:?}"
                );
            }
            let aligned = shifted(&ir, found);
            let (x, y) = (300, 400);
            let want = film(x as f32 + 40.0, y as f32 + 40.0) * 40000.0;
            let got = f32::from(aligned.get_pixel(x, y).0[0]);
            assert!((got - want).abs() < 0.02 * 40000.0, "{got} vs {want}");
        }
        // Nothing to go on: a flat infrared plane.
        let flat = ImageBuffer::from_pixel(w, h, Luma([30000u16]));
        assert_eq!(infrared_offset(&visible, &flat, 30.0), None);
    }
}
