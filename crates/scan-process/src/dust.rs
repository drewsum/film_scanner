//! Infrared dust and scratch removal, as in Digital ICE and SilverFast iSRD.
//!
//! Film dyes are nearly transparent to infrared, so an infrared scan of colour film is a flat,
//! bright plane in which dust, hairs and scratches show as dark specks: they block infrared
//! as well as visible light. [`defect_mask`] finds them:
//!
//! 1. Areas that are mostly dark in infrared (the holder around the film) are left alone.
//! 2. Each pixel is compared with its local background: the mean infrared around it,
//!    leaving out pixels that are themselves dark. A pixel more than
//!    [`DustParams::threshold`] darker than its background is a defect.
//! 3. The cyan dye of some films absorbs a little infrared, so frame detail shows faintly in
//!    it. A linear fit of the infrared against the red channel, over pixels found clean in
//!    step 2, measures how much; that much of the red is added back, and step 2 is repeated.
//! 4. The mask is grown by [`DustParams::grow`] pixels, to cover each defect's soft edge.
//!
//! [`remove_dust`] then fills the masked pixels of the visible image from their surroundings
//! ([`crate::inpaint`]).
//!
//! Silver-based black-and-white film and Kodachrome block infrared, so their infrared scan
//! shows the image itself, not just the dust. It can't be used on them.

use scan_core::image::{DynamicImage, GenericImageView, GrayImage};
use scan_core::{Gray16Image, MM_PER_INCH};

use crate::fit;
use crate::inpaint::inpaint;

/// Radius of the local background in millimetres (its mean reaches a little further; see
/// [`Background`]).
const BACKGROUND_RADIUS_MM: f32 = 0.3;
/// A pixel is checked only where the mean infrared over a wide area around it is at least
/// this fraction of the film's level, so the opaque holder and the strip edges next to it are
/// skipped.
const FILM_FRACTION: f32 = 0.6;
/// That area is this many times the background's radius: wide enough that a cluster of dust
/// doesn't make itself look like the holder.
const FILM_AREA_SCALE: usize = 4;
/// Pixels below this fraction of the brightest infrared are not film.
const OPAQUE_FRACTION: f32 = 0.25;

#[derive(Debug, thiserror::Error)]
pub enum DustError {
    #[error("the infrared channel is {infrared:?} px but the image is {visible:?} px")]
    SizeMismatch {
        visible: (u32, u32),
        infrared: (u32, u32),
    },
}

/// How defects are found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DustParams {
    /// How much darker than its local background a pixel must be in infrared, as a fraction
    /// of the background, to count as a defect.
    pub threshold: f32,
    /// Pixels the mask is grown by on every side.
    pub grow: u32,
    /// Radius of the local background, in pixels.
    pub background_radius: u32,
    /// Correct for the infrared crosstalk of the cyan dye.
    pub crosstalk: bool,
}

impl DustParams {
    /// Settings for a scan at `dpi`. `strength` runs from 0 (only obvious defects) to 1
    /// (faint ones too, with more risk of touching image detail).
    pub fn for_scan(dpi: u32, strength: f32) -> Self {
        let strength = strength.clamp(0.0, 1.0);
        let radius = dpi as f32 * BACKGROUND_RADIUS_MM / MM_PER_INCH;
        Self {
            threshold: 0.35 - 0.25 * strength,
            grow: 1 + dpi / 1200,
            background_radius: (radius.round() as u32).max(4),
            crosstalk: true,
        }
    }
}

/// What [`defect_mask`] found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DustReport {
    /// Masked pixels, after growing the mask.
    pub defect_pixels: usize,
    /// Typical infrared level of the film, from 0 to 1.
    pub film_level: f32,
    /// Infrared lost per unit of red lost to the cyan dye; 0 when not corrected.
    pub crosstalk: f32,
}

/// A visible image with its defects filled.
#[derive(Clone, Debug)]
pub struct DustRemoval {
    /// The cleaned image, in the same format as the original.
    pub image: DynamicImage,
    /// 255 where a defect was filled, 0 elsewhere.
    pub mask: GrayImage,
    pub report: DustReport,
}

/// Find defects with `infrared` and fill them in `visible`. Both come from the same scan.
pub fn remove_dust(
    visible: &DynamicImage,
    infrared: &Gray16Image,
    params: &DustParams,
) -> Result<DustRemoval, DustError> {
    let (mask, report) = defect_mask(visible, infrared, params)?;
    let image = inpaint(visible, &mask);
    Ok(DustRemoval {
        image,
        mask,
        report,
    })
}

/// Find dust and scratches: a mask that is 255 on defects and 0 elsewhere. `visible` is
/// only read for the crosstalk correction.
pub fn defect_mask(
    visible: &DynamicImage,
    infrared: &Gray16Image,
    params: &DustParams,
) -> Result<(GrayImage, DustReport), DustError> {
    let (w, h) = infrared.dimensions();
    if visible.dimensions() != (w, h) {
        return Err(DustError::SizeMismatch {
            visible: visible.dimensions(),
            infrared: (w, h),
        });
    }
    let detector = Detector::new(visible, infrared, params);
    let (crosstalk, red_mean) = if params.crosstalk {
        detector.fit_crosstalk()
    } else {
        (0.0, 0.0)
    };
    let mut mask = detector.detect(crosstalk, red_mean);
    dilate(&mut mask, w as usize, h as usize, params.grow as usize);

    let defect_pixels = mask.iter().filter(|&&m| m).count();
    let data = mask.iter().map(|&m| if m { 255 } else { 0 }).collect();
    let mask = GrayImage::from_raw(w, h, data).expect("mask size matches");
    Ok((
        mask,
        DustReport {
            defect_pixels,
            film_level: detector.level,
            crosstalk,
        },
    ))
}

/// One channel of an image, read as 0..1 without copying it.
enum Channel<'a> {
    U8(&'a [u8], usize),
    U16(&'a [u16], usize),
    Owned(Vec<u16>),
}

impl<'a> Channel<'a> {
    /// The red channel, or the grey one of a greyscale image.
    fn red(img: &'a DynamicImage) -> Self {
        match img {
            DynamicImage::ImageLuma8(b) => Channel::U8(b.as_raw(), 1),
            DynamicImage::ImageLumaA8(b) => Channel::U8(b.as_raw(), 2),
            DynamicImage::ImageRgb8(b) => Channel::U8(b.as_raw(), 3),
            DynamicImage::ImageRgba8(b) => Channel::U8(b.as_raw(), 4),
            DynamicImage::ImageLuma16(b) => Channel::U16(b.as_raw(), 1),
            DynamicImage::ImageLumaA16(b) => Channel::U16(b.as_raw(), 2),
            DynamicImage::ImageRgb16(b) => Channel::U16(b.as_raw(), 3),
            DynamicImage::ImageRgba16(b) => Channel::U16(b.as_raw(), 4),
            other => Channel::Owned(other.to_rgb16().pixels().map(|p| p.0[0]).collect()),
        }
    }

    fn get(&self, i: usize) -> f32 {
        match self {
            Channel::U8(d, stride) => d[i * stride] as f32 / 255.0,
            Channel::U16(d, stride) => d[i * stride] as f32 / 65535.0,
            Channel::Owned(d) => d[i] as f32 / 65535.0,
        }
    }
}

struct Detector<'a> {
    w: usize,
    h: usize,
    ir: &'a [u16],
    red: Channel<'a>,
    /// Typical infrared of the film, 0..1.
    level: f32,
    /// Where pixels are checked: on the film, away from the holder.
    film: Vec<bool>,
    threshold: f32,
    radius: usize,
}

impl<'a> Detector<'a> {
    fn new(visible: &'a DynamicImage, infrared: &'a Gray16Image, params: &DustParams) -> Self {
        let (w, h) = (infrared.width() as usize, infrared.height() as usize);
        let ir = infrared.as_raw().as_slice();
        let level = film_level(ir);
        let radius = params.background_radius.max(1) as usize;
        let overall = Background::new(w, h, radius * FILM_AREA_SCALE, |_, _, i| {
            Some(ir[i] as f32 / 65535.0)
        });
        let floor = (OPAQUE_FRACTION * level * 65535.0) as u16;
        let mut holder = holder(ir, w, h, floor, (4 * radius).pow(2));
        dilate(&mut holder, w, h, (radius / 2).max(2));
        let mut film = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                film[i] = !holder[i] && overall.at(x, y) >= FILM_FRACTION * level;
            }
        }
        Self {
            w,
            h,
            ir,
            red: Channel::red(visible),
            level,
            film,
            threshold: params.threshold,
            radius,
        }
    }

    fn ir(&self, i: usize) -> f32 {
        self.ir[i] as f32 / 65535.0
    }

    /// Infrared with `slope` of the red's deviation from `red_mean` added back.
    fn corrected(&self, i: usize, slope: f32, red_mean: f32) -> f32 {
        if slope == 0.0 {
            self.ir(i)
        } else {
            self.ir(i) + slope * (red_mean - self.red.get(i))
        }
    }

    fn detect(&self, slope: f32, red_mean: f32) -> Vec<bool> {
        let (w, h) = (self.w, self.h);
        let value = |i| self.corrected(i, slope, red_mean);
        let keep = 1.0 - self.threshold;
        let floor = OPAQUE_FRACTION * self.level;
        // A first background over everything but the opaque, then one leaving out what is
        // dark against the first, so large defects don't darken their own background.
        let first = Background::new(w, h, self.radius, |_, _, i| {
            Some(value(i)).filter(|&v| v >= floor)
        });
        let clean = Background::new(w, h, self.radius, |x, y, i| {
            Some(value(i)).filter(|&v| v >= keep * first.at(x, y))
        });
        let mut mask = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                mask[i] = self.film[i] && value(i) < keep * clean.at(x, y);
            }
        }
        mask
    }

    /// Slope of infrared against red over the film, and the mean red there. A least-squares
    /// line, refitted without the points far from it: dust is far below it, as it darkens
    /// the infrared much more than the cyan dye does.
    fn fit_crosstalk(&self) -> (f32, f32) {
        let step = ((self.w * self.h) as f64 / 1e6).sqrt().ceil().max(1.0) as usize;
        let mut points = Vec::new();
        for y in (0..self.h).step_by(step) {
            for x in (0..self.w).step_by(step) {
                let i = y * self.w + x;
                let ir = self.ir(i);
                // Sprocket holes are brighter than the film and on a line of their own.
                if self.film[i] && ir <= 1.05 * self.level {
                    points.push((self.red.get(i) as f64, ir as f64));
                }
            }
        }
        let Some(line) = fit::trimmed_line(&points, 0.02) else {
            return (0.0, 0.0);
        };
        let kept: Vec<f64> = points
            .iter()
            .filter(|&&p| line.keeps(p))
            .map(|&(r, _)| r)
            .collect();
        let red_mean = kept.iter().sum::<f64>() / kept.len().max(1) as f64;
        (line.slope.clamp(0.0, 1.0) as f32, red_mean as f32)
    }
}

/// The opaque holder: the dark (below `floor`) regions that touch the image's edge or are
/// larger than `max_dust` pixels. Smaller dark regions inside the image are dust.
fn holder(ir: &[u16], w: usize, h: usize, floor: u16, max_dust: usize) -> Vec<bool> {
    let mut holder = vec![false; w * h];
    let mut seen = vec![false; w * h];
    let (mut region, mut stack) = (Vec::new(), Vec::new());
    for start in 0..w * h {
        if seen[start] || ir[start] >= floor {
            continue;
        }
        region.clear();
        stack.push(start);
        seen[start] = true;
        let mut at_edge = false;
        while let Some(i) = stack.pop() {
            region.push(i);
            let (x, y) = (i % w, i / w);
            at_edge |= x == 0 || y == 0 || x == w - 1 || y == h - 1;
            let neighbours = [
                (x > 0).then(|| i - 1),
                (x + 1 < w).then(|| i + 1),
                (y > 0).then(|| i - w),
                (y + 1 < h).then(|| i + w),
            ];
            for j in neighbours.into_iter().flatten() {
                if !seen[j] && ir[j] < floor {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        if at_edge || region.len() > max_dust {
            for &i in &region {
                holder[i] = true;
            }
        }
    }
    holder
}

/// The film's typical infrared, 0..1: the median of everything that isn't opaque.
fn film_level(ir: &[u16]) -> f32 {
    let mut histogram = vec![0usize; 1 << 16];
    for &v in ir {
        histogram[v as usize] += 1;
    }
    let percentile = |from: usize, fraction: f64| {
        let total: usize = histogram[from..].iter().sum();
        let target = (total as f64 * fraction) as usize;
        let mut seen = 0;
        for (v, &count) in histogram.iter().enumerate().skip(from) {
            seen += count;
            if seen > target {
                return v;
            }
        }
        histogram.len() - 1
    };
    let bright = percentile(0, 0.99);
    let floor = (bright as f32 * OPAQUE_FRACTION) as usize;
    percentile(floor, 0.5) as f32 / 65535.0
}

/// Local means: sums over a grid of blocks, box-filtered and interpolated between block
/// centres. Cheap at any resolution, and blocks with nothing in them don't count.
struct Background {
    block: usize,
    gw: usize,
    gh: usize,
    sum: Vec<f32>,
    count: Vec<f32>,
}

impl Background {
    /// Blocks are half the radius; the box filter spans two blocks either side, so the mean
    /// reaches about 1.25 times the radius.
    fn new(
        w: usize,
        h: usize,
        radius: usize,
        sample: impl Fn(usize, usize, usize) -> Option<f32>,
    ) -> Self {
        let block = (radius / 2).max(1);
        let (gw, gh) = (w.div_ceil(block), h.div_ceil(block));
        let mut sum = vec![0f32; gw * gh];
        let mut count = vec![0f32; gw * gh];
        for y in 0..h {
            let row = (y / block) * gw;
            for x in 0..w {
                if let Some(v) = sample(x, y, y * w + x) {
                    let g = row + x / block;
                    sum[g] += v;
                    count[g] += 1.0;
                }
            }
        }
        box_filter(&mut sum, gw, gh, 2);
        box_filter(&mut count, gw, gh, 2);
        Self {
            block,
            gw,
            gh,
            sum,
            count,
        }
    }

    fn at(&self, x: usize, y: usize) -> f32 {
        let coord = |p: usize, n: usize| {
            let f = ((p as f32 + 0.5) / self.block as f32 - 0.5).clamp(0.0, (n - 1) as f32);
            let i = f as usize;
            (i, (i + 1).min(n - 1), f - i as f32)
        };
        let (x0, x1, tx) = coord(x, self.gw);
        let (y0, y1, ty) = coord(y, self.gh);
        let lerp = |v: &[f32]| {
            let top = v[y0 * self.gw + x0] * (1.0 - tx) + v[y0 * self.gw + x1] * tx;
            let bottom = v[y1 * self.gw + x0] * (1.0 - tx) + v[y1 * self.gw + x1] * tx;
            top * (1.0 - ty) + bottom * ty
        };
        let count = lerp(&self.count);
        if count > 0.0 {
            lerp(&self.sum) / count
        } else {
            0.0
        }
    }
}

/// Sum over a (2r+1)² window, in place.
fn box_filter(v: &mut [f32], w: usize, h: usize, r: usize) {
    let mut line = Vec::new();
    for y in 0..h {
        line.clear();
        line.extend_from_slice(&v[y * w..(y + 1) * w]);
        for x in 0..w {
            v[y * w + x] = line[x.saturating_sub(r)..(x + r + 1).min(w)].iter().sum();
        }
    }
    for x in 0..w {
        line.clear();
        line.extend((0..h).map(|y| v[y * w + x]));
        for y in 0..h {
            v[y * w + x] = line[y.saturating_sub(r)..(y + r + 1).min(h)].iter().sum();
        }
    }
}

/// Grow the set pixels of `mask` by `r` pixels in every direction (a square).
pub(crate) fn dilate(mask: &mut [bool], w: usize, h: usize, r: usize) {
    if r == 0 {
        return;
    }
    let mut line = Vec::new();
    let mut out = Vec::new();
    for y in 0..h {
        line.clear();
        line.extend_from_slice(&mask[y * w..(y + 1) * w]);
        dilate_line(&line, &mut out, r);
        mask[y * w..(y + 1) * w].copy_from_slice(&out);
    }
    for x in 0..w {
        line.clear();
        line.extend((0..h).map(|y| mask[y * w + x]));
        dilate_line(&line, &mut out, r);
        for (y, &m) in out.iter().enumerate() {
            mask[y * w + x] = m;
        }
    }
}

/// `out[i]` is whether anything in `line[i - r..=i + r]` is set.
fn dilate_line(line: &[bool], out: &mut Vec<bool>, r: usize) {
    let n = line.len();
    out.clear();
    // Set pixels in the window [x - r, x + r].
    let mut count: usize = line[..r.min(n)].iter().map(|&b| usize::from(b)).sum();
    for x in 0..n {
        if x + r < n {
            count += usize::from(line[x + r]);
        }
        out.push(count > 0);
        if x >= r {
            count -= usize::from(line[x - r]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan_core::image::{ImageBuffer, Luma, Rgb};
    use scan_core::mock::{self, MockBackend};
    use scan_core::{NoProgress, ScanArea, ScanRequest, ScanResult, ScanSource, ScannerBackend};

    const DPI: u32 = 300;

    fn mock_scan(area: ScanArea) -> ScanResult {
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let mut req = ScanRequest::new(ScanSource::Transparency, DPI);
        req.area = Some(area);
        req.infrared = true;
        dev.scan(&req, &mut NoProgress).unwrap()
    }

    /// A stretch of the left strip: two frames, film base and sprocket holes.
    fn strip_area() -> ScanArea {
        ScanArea {
            x_mm: 5.0,
            y_mm: 30.0,
            width_mm: 35.0,
            height_mm: 30.0,
        }
    }

    fn px(area: ScanArea, x_mm: f32, y_mm: f32) -> (u32, u32) {
        let p = |mm: f32| (mm / MM_PER_INCH * DPI as f32) as u32;
        (p(x_mm - area.x_mm), p(y_mm - area.y_mm))
    }

    /// Whether a point lies inside a frame, at least `margin` mm from its edges.
    fn in_frame(x: f32, y: f32, margin: f32) -> bool {
        (0..12).any(|i| {
            let (fx, fy, fw, fh) = mock::frame_rect_mm(i);
            x >= fx + margin && x < fx + fw - margin && y >= fy + margin && y < fy + fh - margin
        })
    }

    #[test]
    fn finds_the_mock_dust_in_frames_and_nothing_else() {
        let area = strip_area();
        let scan = mock_scan(area);
        let ir = scan.infrared.as_ref().unwrap();
        let params = DustParams::for_scan(DPI, 0.5);
        let (mask, report) = defect_mask(scan.primary(), ir, &params).unwrap();
        assert!((report.film_level - 0.85).abs() < 0.1, "{report:?}");

        let specks = mock::dust_specks(area);
        let in_frames: Vec<_> = specks
            .iter()
            .filter(|&&(x, y, _)| in_frame(x, y, 0.5))
            .collect();
        assert!(in_frames.len() >= 10, "only {} specks", in_frames.len());
        for &&(x, y, r) in &in_frames {
            let (cx, cy) = px(area, x, y);
            assert_eq!(mask.get_pixel(cx, cy).0[0], 255, "speck at {x},{y} r {r}");
        }

        // Everything masked within a frame is near a speck.
        let mm_per_px = MM_PER_INCH / DPI as f32;
        let reach = (params.grow as f32 + 2.0) * mm_per_px;
        let mut stray = 0;
        for (px_x, px_y, p) in mask.enumerate_pixels() {
            let x = area.x_mm + (px_x as f32 + 0.5) * mm_per_px;
            let y = area.y_mm + (px_y as f32 + 0.5) * mm_per_px;
            if p.0[0] == 255
                && in_frame(x, y, 0.0)
                && !specks
                    .iter()
                    .any(|&(sx, sy, r)| (x - sx).powi(2) + (y - sy).powi(2) <= (r + reach).powi(2))
            {
                stray += 1;
            }
        }
        assert_eq!(stray, 0, "masked pixels away from dust");
    }

    #[test]
    fn leaves_the_holder_alone() {
        // Across the gap between the strips, where the holder blocks all light.
        let area = ScanArea {
            x_mm: 30.0,
            y_mm: 10.0,
            width_mm: 30.0,
            height_mm: 40.0,
        };
        let scan = mock_scan(area);
        let (mask, _) = defect_mask(
            scan.primary(),
            scan.infrared.as_ref().unwrap(),
            &DustParams::for_scan(DPI, 1.0),
        )
        .unwrap();
        let (x0, _) = px(area, 41.0, 0.0);
        let (x1, _) = px(area, 49.0, 0.0);
        for y in 0..mask.height() {
            for x in x0..x1 {
                assert_eq!(mask.get_pixel(x, y).0[0], 0, "holder pixel {x},{y}");
            }
        }
    }

    #[test]
    fn ignores_a_thin_holder_band_at_the_image_edge() {
        // Film with a speck, then a soft edge into 8 px of holder on the right.
        let (w, h) = (300, 200);
        let ir_at = |x: u32, y: u32| {
            let edge = (x as f32 - 288.0).clamp(0.0, 4.0) / 4.0;
            let speck = if x.abs_diff(100) <= 2 && y.abs_diff(100) <= 2 {
                0.05
            } else {
                1.0
            };
            0.9 * (1.0 - edge * 0.998) * speck
        };
        let infrared = ImageBuffer::from_fn(w, h, |x, y| Luma([(ir_at(x, y) * 65535.0) as u16]));
        let visible = DynamicImage::ImageRgb16(ImageBuffer::from_fn(w, h, |x, y| {
            let v = (ir_at(x, y) * 0.6 * 65535.0) as u16;
            Rgb([v, v, v])
        }));
        let (mask, _) = defect_mask(&visible, &infrared, &DustParams::for_scan(1600, 0.5)).unwrap();
        assert_eq!(mask.get_pixel(100, 100).0[0], 255, "the speck");
        let edge = (250..w).flat_map(|x| (0..h).map(move |y| (x, y)));
        let masked: Vec<_> = edge
            .filter(|&(x, y)| mask.get_pixel(x, y).0[0] != 0)
            .collect();
        assert!(masked.is_empty(), "{} edge pixels masked", masked.len());
    }

    /// A flat negative with fine cyan lines, which also show in infrared, and three specks.
    fn crosstalk_scene() -> (DynamicImage, Gray16Image, Vec<(u32, u32)>) {
        let (w, h) = (200, 120);
        let specks = vec![(40, 30), (100, 60), (160, 95)];
        let speck = |x: u32, y: u32| {
            specks
                .iter()
                .any(|&(sx, sy)| x.abs_diff(sx) <= 1 && y.abs_diff(sy) <= 1)
        };
        // Cyan lines: red transmission falls from 0.8 to 0.2.
        let red = |x: u32, _y: u32| if x % 12 < 2 { 0.2 } else { 0.8 };
        let visible = ImageBuffer::from_fn(w, h, |x, y| {
            let k = if speck(x, y) { 0.1 } else { 1.0 };
            let to16 = |v: f32| (v * k * 65535.0) as u16;
            Rgb([to16(red(x, y)), to16(0.5), to16(0.3)])
        });
        let infrared = ImageBuffer::from_fn(w, h, |x, y| {
            let k = if speck(x, y) { 0.1 } else { 1.0 };
            // 30% of the red lost also goes from the infrared.
            let v = 0.9 - 0.3 * (0.8 - red(x, y));
            Luma([(v * k * 65535.0) as u16])
        });
        (DynamicImage::ImageRgb16(visible), infrared, specks)
    }

    #[test]
    fn crosstalk_correction_spares_cyan_detail() {
        let (visible, infrared, specks) = crosstalk_scene();
        let mut params = DustParams::for_scan(DPI, 1.0);
        params.background_radius = 6;

        params.crosstalk = false;
        let (_, raw) = defect_mask(&visible, &infrared, &params).unwrap();
        params.crosstalk = true;
        let (mask, report) = defect_mask(&visible, &infrared, &params).unwrap();

        assert!((report.crosstalk - 0.3).abs() < 0.05, "{report:?}");
        for &(x, y) in &specks {
            assert_eq!(mask.get_pixel(x, y).0[0], 255, "speck at {x},{y}");
        }
        // Without the correction the lines are taken for scratches.
        let speck_area = specks.len() * (3 + 2 * params.grow as usize).pow(2);
        assert!(raw.defect_pixels > 4 * speck_area, "{raw:?}");
        assert!(report.defect_pixels <= speck_area, "{report:?}");
    }

    #[test]
    fn removes_dust_from_the_mock_scan() {
        let area = strip_area();
        let scan = mock_scan(area);
        let params = DustParams::for_scan(DPI, 0.5);
        let cleaned =
            remove_dust(scan.primary(), scan.infrared.as_ref().unwrap(), &params).unwrap();
        assert!(matches!(cleaned.image, DynamicImage::ImageRgb16(_)));
        let before = scan.primary().to_rgb16();
        let after = cleaned.image.to_rgb16();

        // The mock's dust passes a tenth of the light, so the film under a speck's centre
        // was ten times brighter. Fills next to a sharp edge in the frame may miss it.
        let (mut checked, mut good) = (0, 0);
        for &(x, y, _) in &mock::dust_specks(area) {
            if !in_frame(x, y, 1.0) {
                continue;
            }
            let (cx, cy) = px(area, x, y);
            let (b, a) = (before.get_pixel(cx, cy).0, after.get_pixel(cx, cy).0);
            checked += 1;
            good += usize::from((0..3).all(|c| {
                let expected = b[c] as f32 * 10.0;
                (a[c] as f32 - expected).abs() < 0.15 * expected + 500.0
            }));
        }
        assert!(checked >= 10, "checked {checked}");
        assert!(good * 10 >= checked * 9, "{good} of {checked} filled well");

        // Unmasked pixels are untouched.
        for (p, (q, m)) in before
            .pixels()
            .zip(after.pixels().zip(cleaned.mask.pixels()))
        {
            if m.0[0] == 0 {
                assert_eq!(p, q);
            }
        }
    }

    #[test]
    fn mismatched_sizes_are_an_error() {
        let visible = DynamicImage::new_rgb16(10, 10);
        let infrared = Gray16Image::new(10, 11);
        let params = DustParams::for_scan(DPI, 0.5);
        assert!(matches!(
            defect_mask(&visible, &infrared, &params),
            Err(DustError::SizeMismatch { .. })
        ));
    }

    #[test]
    fn params_scale_with_resolution_and_strength() {
        let low = DustParams::for_scan(75, 0.0);
        let high = DustParams::for_scan(1600, 1.0);
        assert!(high.threshold < low.threshold);
        assert!(high.background_radius > low.background_radius);
        assert_eq!((low.grow, high.grow), (1, 2));
        assert_eq!(DustParams::for_scan(1600, 7.0), high);
    }

    #[test]
    fn dilate_grows_a_square() {
        let (w, h) = (7, 5);
        let mut mask = vec![false; w * h];
        mask[2 * w + 3] = true;
        dilate(&mut mask, w, h, 1);
        let set: Vec<_> = (0..w * h)
            .filter(|&i| mask[i])
            .map(|i| (i % w, i / w))
            .collect();
        let expected: Vec<_> = (1..4).flat_map(|y| (2..5).map(move |x| (x, y))).collect();
        assert_eq!(set, expected);
    }
}
