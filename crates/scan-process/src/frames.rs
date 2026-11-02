//! Finding the frames on a scan of film strips, and cutting them out straightened.
//!
//! [`detect_frames`] works on a transparency scan of negative strips in a holder:
//!
//! 1. **Strips.** Film lets light through and the holder doesn't, so the strips are the large
//!    bright regions of the scan. Their shape gives each one's direction, down or across the
//!    scan. Its two long edges are then found every millimetre and fitted with lines, which
//!    give the strip's exact angle (the deskew) and middle.
//! 2. **Film base.** The strip is sampled straight along its length. The unexposed film
//!    between the perforations and the frames (the rebate) gives the colour of the bare film
//!    base, which is also what lies between frames.
//! 3. **Frames.** A camera advances the film by a nearly fixed distance (38 mm, eight
//!    perforations, on 35 mm film), so a row of frames at one spacing is fitted to the whole
//!    strip: the one whose gaps look most like bare film and whose frames least. A dark or
//!    underexposed frame, whose edges look just like the film around it, still gets its
//!    place from the frames around it. Each edge is then moved to where the scan shows it
//!    nearby, if it does. Frames whose ends couldn't be seen are flagged for checking, and
//!    frames as bare as the film base (the unexposed end of a roll) are left out.
//!
//! Frames are [`FrameRect`]s in millimetres on the scanner bed, so frames found on a
//! low-resolution preview fit a later full-resolution scan of the same film.
//! [`crop_frame`] cuts one out of a scan, straightened.

use scan_core::image::{DynamicImage, ImageBuffer, Luma, Rgb};
use scan_core::{MM_PER_INCH, ScanArea};

use crate::fit;

/// Resolution the detection works at; finer scans are averaged down to about this.
const WORK_DPI: f32 = 200.0;
/// How different film is from the bare film base is measured in linear light, as the share
/// of the light through the base that it holds back (negative where it lets more through),
/// averaged over the channels. Linear, so that the middle of a blurred edge is its place.
/// A difference counts for no more than this, so a speck of dust or a perforation doesn't
/// outweigh a frame's worth of image.
const MAX_DIFFERENCE: f32 = 1.5;
/// Film letting through this much more than the base is no film at all: clear glass past
/// the end of a strip, or a perforation.
const CLEAR: f32 = 0.2;
/// A frame whose average difference from the film base is less than its gaps' plus this is
/// bare film, and is left out.
const BLANK: f32 = 0.05;
/// An edge must step up by at least this difference to be taken as seen.
const MIN_EDGE: f32 = 0.08;
/// How far from where the spacing puts it an edge is looked for, in millimetres.
const EDGE_WINDOW_MM: f32 = 1.2;
/// How much each side of an edge is averaged, in millimetres.
const EDGE_SIDE_MM: f32 = 0.5;
/// Steps of the spacing search, in millimetres.
const SEARCH_STEP_MM: f32 = 0.1;

/// A rectangle on the scanner bed in millimetres, turned about its centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRect {
    pub cx_mm: f32,
    pub cy_mm: f32,
    /// Size along the rectangle's own axes: across and down when `angle_deg` is 0.
    pub width_mm: f32,
    pub height_mm: f32,
    /// Clockwise on screen, where y points down.
    pub angle_deg: f32,
}

impl FrameRect {
    /// The rectangle's x and y axes, as unit vectors on the bed.
    fn axes(&self) -> ([f32; 2], [f32; 2]) {
        let (s, c) = self.angle_deg.to_radians().sin_cos();
        ([c, s], [-s, c])
    }

    /// A point on the bed in the rectangle's own coordinates, from its centre.
    pub fn to_local(&self, x: f32, y: f32) -> (f32, f32) {
        let (ex, ey) = self.axes();
        let (dx, dy) = (x - self.cx_mm, y - self.cy_mm);
        (dx * ex[0] + dy * ex[1], dx * ey[0] + dy * ey[1])
    }

    /// A point in the rectangle's own coordinates, on the bed.
    pub fn from_local(&self, u: f32, v: f32) -> (f32, f32) {
        let (ex, ey) = self.axes();
        (
            self.cx_mm + u * ex[0] + v * ey[0],
            self.cy_mm + u * ex[1] + v * ey[1],
        )
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        let (u, v) = self.to_local(x, y);
        u.abs() <= self.width_mm / 2.0 && v.abs() <= self.height_mm / 2.0
    }

    /// Top-left, top-right, bottom-right and bottom-left, in the rectangle's own orientation.
    pub fn corners(&self) -> [(f32, f32); 4] {
        let (w, h) = (self.width_mm / 2.0, self.height_mm / 2.0);
        [(-w, -h), (w, -h), (w, h), (-w, h)].map(|(u, v)| self.from_local(u, v))
    }
}

/// The size of film and of the frames on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilmFormat {
    pub film_width_mm: f32,
    /// Across the film.
    pub frame_width_mm: f32,
    /// Along the film.
    pub frame_length_mm: f32,
    /// From one frame to the next, as the camera advances the film.
    pub pitch_mm: f32,
}

impl FilmFormat {
    /// 35 mm film: 24 x 36 mm frames, eight perforations apart.
    pub const FILM_135: FilmFormat = FilmFormat {
        film_width_mm: 35.0,
        frame_width_mm: 24.0,
        frame_length_mm: 36.0,
        pitch_mm: 38.0,
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectParams {
    pub format: FilmFormat,
    /// Trimmed off every side of a detected frame, so no border shows along its edges.
    pub inset_mm: f32,
}

impl Default for DetectParams {
    fn default() -> Self {
        Self {
            format: FilmFormat::FILM_135,
            inset_mm: 0.3,
        }
    }
}

/// A frame [`detect_frames`] found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectedFrame {
    pub rect: FrameRect,
    /// How many of the frame's four edges the scan showed; the others are where the frame
    /// spacing and the film format put them.
    pub edges_seen: u8,
    /// Neither end of the frame along the strip could be seen, so its place along the strip
    /// comes from the frames around it alone: worth checking.
    pub placed_by_spacing: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameDetection {
    /// Strip by strip (left to right, or top to bottom), each from its top or left end.
    pub frames: Vec<DetectedFrame>,
    /// Strips of film found, including any without frames.
    pub strips: usize,
    /// The linear light (0–1 of full scale) through the bare film base, per channel: the
    /// median of the strips' rebates (or, where the holder covers those, of their clearest
    /// film). What a negative is inverted against.
    pub base: Option<[f32; 3]>,
}

/// Find the frames on a transparency scan of negative film strips. `image` is the scan of
/// `area` at `dpi`, its values encoding light with `gamma` (see [`scan_core::ScanResult`]).
pub fn detect_frames(
    image: &DynamicImage,
    gamma: f32,
    dpi: u32,
    area: ScanArea,
    params: &DetectParams,
) -> FrameDetection {
    let work = Work::new(image, gamma, dpi);
    let mut detection = FrameDetection::default();
    let Some(levels) = Levels::measure(&work) else {
        return detection;
    };
    let format = Format::new(&params.format, work.mm);
    let inset = params.inset_mm / work.mm;
    let mut bases = Vec::new();
    for strip in find_strips(&work, &levels, &format) {
        let Some(sampled) = Sampled::new(&work, &levels, &strip, &format) else {
            continue;
        };
        detection.strips += 1;
        bases.push(sampled.base);
        for frame in sampled.frames(&format) {
            let along = (frame.start + inset, frame.end - inset);
            let across = (frame.left + inset, frame.right - inset);
            if along.1 <= along.0 || across.1 <= across.0 {
                continue;
            }
            let v = sampled.v_start + (along.0 + along.1) / 2.0;
            let u = sampled.u_start + (across.0 + across.1) / 2.0;
            let [x, y] = strip.point(v, u);
            let (length, width) = (along.1 - along.0, across.1 - across.0);
            let a = strip.along;
            let (angle, w, h) = if strip.vertical() {
                ((-a[0]).atan2(a[1]), width, length)
            } else {
                (a[1].atan2(a[0]), length, width)
            };
            detection.frames.push(DetectedFrame {
                rect: FrameRect {
                    cx_mm: area.x_mm + x * work.mm,
                    cy_mm: area.y_mm + y * work.mm,
                    width_mm: w * work.mm,
                    height_mm: h * work.mm,
                    angle_deg: angle.to_degrees(),
                },
                edges_seen: frame.edges_seen,
                placed_by_spacing: frame.placed_by_spacing,
            });
        }
    }
    if !bases.is_empty() {
        detection.base = Some(std::array::from_fn(|c| {
            let mut v: Vec<f32> = bases.iter().map(|b| b[c]).collect();
            percentile(&mut v, 0.5)
        }));
    }
    detection
}

/// The film format in work pixels.
struct Format {
    film_width: f32,
    frame_width: f32,
    frame_length: f32,
    pitch: f32,
    /// One millimetre.
    mm: f32,
}

impl Format {
    fn new(format: &FilmFormat, mm_per_px: f32) -> Self {
        let px = |mm: f32| mm / mm_per_px;
        Self {
            film_width: px(format.film_width_mm),
            frame_width: px(format.frame_width_mm),
            frame_length: px(format.frame_length_mm),
            pitch: px(format.pitch_mm),
            mm: px(1.0),
        }
    }
}

/// The scan in linear light, averaged down to about [`WORK_DPI`]. Coordinates are in work
/// pixels from the top-left corner, with pixel `i` covering `i..i + 1`.
struct Work {
    w: usize,
    h: usize,
    /// Millimetres per work pixel.
    mm: f32,
    lin: Vec<[f32; 3]>,
}

impl Work {
    fn new(image: &DynamicImage, gamma: f32, dpi: u32) -> Self {
        let factor = ((dpi as f32 / WORK_DPI).round() as usize).max(1);
        let (w, h) = (image.width() as usize, image.height() as usize);
        let lut8: Vec<f32> = (0..=255u32)
            .map(|v| (v as f32 / 255.0).powf(gamma))
            .collect();
        let lut16 = || -> Vec<f32> {
            (0..=65535u32)
                .map(|v| (v as f32 / 65535.0).powf(gamma))
                .collect()
        };
        let (ww, wh, lin) = match image {
            DynamicImage::ImageRgb8(b) => {
                downsample(b.as_raw(), w, h, 3, factor, |v| lut8[v as usize])
            }
            DynamicImage::ImageLuma8(b) => {
                downsample(b.as_raw(), w, h, 1, factor, |v| lut8[v as usize])
            }
            DynamicImage::ImageLuma16(b) => {
                let lut = lut16();
                downsample(b.as_raw(), w, h, 1, factor, |v| lut[v as usize])
            }
            DynamicImage::ImageRgb16(b) => {
                let lut = lut16();
                downsample(b.as_raw(), w, h, 3, factor, |v| lut[v as usize])
            }
            other => {
                let lut = lut16();
                downsample(other.to_rgb16().as_raw(), w, h, 3, factor, |v| {
                    lut[v as usize]
                })
            }
        };
        Self {
            w: ww,
            h: wh,
            mm: MM_PER_INCH / dpi as f32 * factor as f32,
            lin,
        }
    }

    fn inside(&self, [x, y]: [f32; 2]) -> bool {
        (0.0..=self.w as f32).contains(&x) && (0.0..=self.h as f32).contains(&y)
    }

    /// Bilinear interpolation, clamped at the borders.
    fn at(&self, [x, y]: [f32; 2]) -> [f32; 3] {
        let fx = (x - 0.5).clamp(0.0, (self.w - 1) as f32);
        let fy = (y - 0.5).clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let p = |x: usize, y: usize| self.lin[y * self.w + x];
        let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
        std::array::from_fn(|i| {
            let top = a[i] + (b[i] - a[i]) * tx;
            let bottom = c[i] + (d[i] - c[i]) * tx;
            top + (bottom - top) * ty
        })
    }
}

/// Average `factor` x `factor` blocks of an image with 1 or 3 channels into linear RGB.
/// Large blocks are averaged from a grid of samples in them, plenty to smooth the noise.
fn downsample<T: Copy>(
    raw: &[T],
    w: usize,
    h: usize,
    channels: usize,
    factor: usize,
    linear: impl Fn(T) -> f32,
) -> (usize, usize, Vec<[f32; 3]>) {
    let (ww, wh) = ((w / factor).max(1), (h / factor).max(1));
    let stride = (factor / 4).max(1);
    let mut sums = vec![[0f32; 3]; ww * wh];
    let mut counts = vec![0u32; ww * wh];
    for y in (0..(wh * factor).min(h)).step_by(stride) {
        let row = (y / factor) * ww;
        for x in (0..(ww * factor).min(w)).step_by(stride) {
            let p = &raw[(y * w + x) * channels..][..channels];
            let i = row + x / factor;
            counts[i] += 1;
            for c in 0..3 {
                sums[i][c] += linear(p[c.min(channels - 1)]);
            }
        }
    }
    for (s, &n) in sums.iter_mut().zip(&counts) {
        *s = s.map(|v| v / n.max(1) as f32);
    }
    (ww, wh, sums)
}

/// The brightest channel, which tells film from holder best: even a dense negative lets
/// through a fair amount of red.
fn bright(p: [f32; 3]) -> f32 {
    p[0].max(p[1]).max(p[2])
}

/// The value at fraction `q` of the way through `values` in order (which it reorders).
fn percentile(values: &mut [f32], q: f32) -> f32 {
    let i = ((values.len() - 1) as f32 * q).round() as usize;
    *values.select_nth_unstable_by(i, f32::total_cmp).1
}

struct Levels {
    /// Brighter than this is film (or a perforation, or empty glass).
    film: f32,
    /// Darker than this is the holder, even within a strip's outline.
    holder: f32,
}

impl Levels {
    /// The levels between the holder (the darkest part of the scan) and the light (the
    /// brightest: perforations and empty glass). `None` without that contrast: no holder.
    fn measure(work: &Work) -> Option<Self> {
        let mut values: Vec<f32> = work.lin.iter().map(|&p| bright(p)).collect();
        let holder = percentile(&mut values, 0.01).max(1e-5);
        let light = percentile(&mut values, 0.998);
        if light < holder * 20.0 {
            return None;
        }
        let film = (holder * light).sqrt();
        Some(Self {
            film,
            holder: (holder * film).sqrt(),
        })
    }
}

/// A strip's axes, in work pixels.
#[derive(Clone, Debug)]
struct Strip {
    /// A point on the middle line of the strip.
    centre: [f32; 2],
    /// Unit vector along the strip, pointing down for strips down the scan and right for
    /// strips across it.
    along: [f32; 2],
    /// Half the strip's width.
    half: f32,
    /// The strip's ends, along it from `centre`.
    start: f32,
    end: f32,
}

impl Strip {
    /// Unit vector across the strip.
    fn across(&self) -> [f32; 2] {
        [-self.along[1], self.along[0]]
    }

    fn vertical(&self) -> bool {
        self.along[1].abs() >= self.along[0].abs()
    }

    /// The point `v` along and `u` across the strip from its centre.
    fn point(&self, v: f32, u: f32) -> [f32; 2] {
        let (a, n) = (self.along, self.across());
        [
            self.centre[0] + a[0] * v + n[0] * u,
            self.centre[1] + a[1] * v + n[1] * u,
        ]
    }

    fn set_along(&mut self, a: [f32; 2]) {
        let len = (a[0] * a[0] + a[1] * a[1]).sqrt();
        let mut a = [a[0] / len, a[1] / len];
        let flip = if a[1].abs() >= a[0].abs() {
            a[1] < 0.0
        } else {
            a[0] < 0.0
        };
        if flip {
            a = [-a[0], -a[1]];
            (self.start, self.end) = (-self.end, -self.start);
        }
        self.along = a;
    }

    /// Fit lines to the strip's long edges, and put its axes and width on them.
    fn fit_edges(&mut self, work: &Work, levels: &Levels, format: &Format) {
        // Edges are looked for from outside, coming in from the holder, so whatever is on
        // the film near its edges doesn't matter.
        // The holder between strips can be narrow, so the search starts just outside the
        // strip's outline, or a little further out, where that is holder.
        let outside = [1.0, 0.5, 2.0].map(|mm| mm * format.mm);
        let step = format.mm.max(1.0);
        let mut sides: [Vec<(f64, f64)>; 2] = [Vec::new(), Vec::new()];
        let mut v = self.start + 2.0 * format.mm;
        while v < self.end - 2.0 * format.mm {
            for (side, sign) in [(0, -1.0f32), (1, 1.0)] {
                let start = outside.iter().find_map(|&o| {
                    let u = sign * (self.half + o);
                    let p = self.point(v, u);
                    let value = bright(work.at(p));
                    (work.inside(p) && value <= levels.film).then_some((u, value))
                });
                let Some((mut u, mut before)) = start else {
                    continue;
                };
                while u * sign > 0.0 {
                    let next = u - sign * 0.25;
                    let value = bright(work.at(self.point(v, next)));
                    if value > levels.film {
                        let f = (levels.film - before) / (value - before);
                        sides[side].push((v as f64, (u + (next - u) * f) as f64));
                        break;
                    }
                    (u, before) = (next, value);
                }
            }
            v += step;
        }
        let fits = sides.map(|points| fit::trimmed_line_of(&points, 0.5, 8.0));
        let (slope, middle, half) = match fits {
            [Some(l), Some(r)] => (
                (l.slope + r.slope) / 2.0,
                (l.offset + r.offset) / 2.0,
                (r.offset - l.offset) / 2.0,
            ),
            [Some(l), None] => (l.slope, l.offset + self.half as f64, self.half as f64),
            [None, Some(r)] => (r.slope, r.offset - self.half as f64, self.half as f64),
            [None, None] => return,
        };
        // A line u = middle + slope · v runs along a + slope · n from the point `middle`
        // across from the centre.
        self.centre = self.point(0.0, middle as f32);
        let (a, n) = (self.along, self.across());
        let slope = slope as f32;
        self.set_along([a[0] + n[0] * slope, a[1] + n[1] * slope]);
        self.half = half as f32;
    }
}

/// The strips of film on the scan: bright regions about as wide as the film and at least a
/// frame long, with straightened axes.
fn find_strips(work: &Work, levels: &Levels, format: &Format) -> Vec<Strip> {
    let mask: Vec<bool> = work.lin.iter().map(|&p| bright(p) > levels.film).collect();
    let mut seen = vec![false; mask.len()];
    let min_area = 0.5 * format.frame_width * format.frame_length;
    // Strips, and whether their shape left their direction open (a strip of one frame is
    // nearly square).
    let mut found: Vec<(Strip, bool)> = Vec::new();
    let mut stack = Vec::new();
    let mut pixels = Vec::new();
    for first in 0..mask.len() {
        if !mask[first] || seen[first] {
            continue;
        }
        seen[first] = true;
        stack.push(first);
        pixels.clear();
        while let Some(i) = stack.pop() {
            pixels.push(i);
            let (x, y) = (i % work.w, i / work.w);
            let mut visit = |j: usize| {
                if mask[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < work.w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - work.w);
            }
            if y + 1 < work.h {
                visit(i + work.w);
            }
        }
        if (pixels.len() as f32) < min_area {
            continue;
        }
        if let Some(strip) = strip_of(&pixels, work.w, format) {
            found.push(strip);
        }
    }

    // Strips whose shape doesn't say which way they run go the way the others do, or the
    // way the scan's long side does.
    let (down, across) = found
        .iter()
        .filter(|(_, open)| !open)
        .fold(
            (0, 0),
            |(d, a), (s, _)| {
                if s.vertical() { (d + 1, a) } else { (d, a + 1) }
            },
        );
    let vertical = if down + across > 0 {
        down >= across
    } else {
        work.h >= work.w
    };
    let mut strips: Vec<Strip> = found
        .into_iter()
        .filter_map(|(mut strip, open)| {
            if open && strip.vertical() != vertical {
                // Turn it a quarter: its length and width swap.
                let half = (strip.end - strip.start) / 2.0;
                let middle = (strip.end + strip.start) / 2.0;
                strip.centre = strip.point(middle, 0.0);
                (strip.start, strip.end) = (-strip.half, strip.half);
                strip.half = half;
                let n = strip.across();
                strip.set_along(n);
            }
            for _ in 0..2 {
                strip.fit_edges(work, levels, format);
            }
            let width = 2.0 * strip.half;
            (0.6 * format.film_width..=1.4 * format.film_width)
                .contains(&width)
                .then_some(strip)
        })
        .collect();
    strips.sort_by(|a, b| {
        let key = |s: &Strip| {
            let c = s.point((s.start + s.end) / 2.0, 0.0);
            (!s.vertical(), if s.vertical() { c[0] } else { c[1] })
        };
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
    });
    strips
}

/// The strip a region of pixels makes, from its principal axes, if it is shaped like one.
/// Also whether it is so nearly square that its direction is open.
fn strip_of(pixels: &[usize], w: usize, format: &Format) -> Option<(Strip, bool)> {
    let coords = || {
        pixels
            .iter()
            .map(move |&i| ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5))
    };
    let n = pixels.len() as f64;
    let (sx, sy) = coords().fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
    let (mx, my) = (sx / n, sy / n);
    let (mut cxx, mut cyy, mut cxy) = (0.0, 0.0, 0.0);
    for (x, y) in coords() {
        let (dx, dy) = (x - mx, y - my);
        cxx += dx * dx;
        cyy += dy * dy;
        cxy += dx * dy;
    }
    let theta = 0.5 * (2.0 * cxy).atan2(cxx - cyy);
    let (a, b) = (theta.cos(), theta.sin());
    let (mut v0, mut v1, mut u0, mut u1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for (x, y) in coords() {
        let (dx, dy) = (x - mx, y - my);
        let (v, u) = (dx * a + dy * b, -dx * b + dy * a);
        (v0, v1, u0, u1) = (v0.min(v), v1.max(v), u0.min(u), u1.max(u));
    }
    let (length, width) = ((v1 - v0) as f32, (u1 - u0) as f32);
    let wide_enough = (0.6 * format.film_width..=1.4 * format.film_width).contains(&width);
    if !wide_enough || length < 0.75 * format.frame_length {
        return None;
    }
    let mut strip = Strip {
        centre: [mx as f32, my as f32],
        along: [a as f32, b as f32],
        half: width / 2.0,
        start: -length / 2.0,
        end: length / 2.0,
    };
    // Centre the outline's middle.
    strip.centre = strip.point(((v0 + v1) / 2.0) as f32, ((u0 + u1) / 2.0) as f32);
    let along = strip.along;
    strip.set_along(along);
    Some((strip, length < 1.3 * width))
}

/// A strip sampled straight: one sample per work pixel along and across it, `u` across
/// from its left edge and `v` along from its start.
struct Sampled {
    nu: usize,
    nv: usize,
    /// Where sample 0 starts, across and along from the strip's centre.
    u_start: f32,
    v_start: f32,
    /// Linear light per channel.
    light: Vec<[f32; 3]>,
    /// Holder, or clear glass: no film.
    no_film: Vec<bool>,
    /// The frames' band across the strip, and the strip's middle, in samples.
    band: (usize, usize),
    middle: f32,
    /// The light through unexposed film.
    base: [f32; 3],
}

/// A frame on a sampled strip, in samples.
#[derive(Debug)]
struct Found {
    start: f32,
    end: f32,
    left: f32,
    right: f32,
    edges_seen: u8,
    placed_by_spacing: bool,
}

impl Sampled {
    fn new(work: &Work, levels: &Levels, strip: &Strip, format: &Format) -> Option<Self> {
        let half = strip.half.max(format.film_width / 2.0) + format.mm;
        let nu = (2.0 * half).ceil() as usize;
        let nv = (strip.end - strip.start).floor() as usize;
        if nv < 2 {
            return None;
        }
        let (u_start, v_start) = (-(nu as f32) / 2.0, strip.start);
        let mut light = Vec::with_capacity(nu * nv);
        let mut holder = Vec::with_capacity(nu * nv);
        for j in 0..nv {
            for i in 0..nu {
                let p = strip.point(v_start + j as f32 + 0.5, u_start + i as f32 + 0.5);
                let lin = work.at(p);
                holder.push(!work.inside(p) || bright(lin) < levels.holder);
                light.push(lin);
            }
        }
        let middle = -u_start;
        let column = |u: f32| (middle + u).clamp(0.0, nu as f32) as usize;
        let edge = format.frame_width / 2.0;
        let band = (column(-edge + format.mm), column(edge - format.mm));

        // The rebate between the perforations and the frames, on both sides, unless the
        // holder covers it.
        let rebate = (
            edge + 0.4 * format.mm,
            (edge + 1.4 * format.mm).min(strip.half - 0.3 * format.mm),
        );
        let mut base_samples: [Vec<f32>; 3] = Default::default();
        if rebate.1 > rebate.0 {
            for j in 0..nv {
                for (lo, hi) in [(-rebate.1, -rebate.0), (rebate.0, rebate.1)] {
                    for i in column(lo)..column(hi) {
                        let k = j * nu + i;
                        if !holder[k] {
                            for c in 0..3 {
                                base_samples[c].push(light[k][c]);
                            }
                        }
                    }
                }
            }
        }
        let base = if base_samples[0].len() >= 20 {
            base_samples.map(|mut s| percentile(&mut s, 0.5))
        } else {
            // The clearest film in the frames' band: the bare film between frames.
            let mut s: [Vec<f32>; 3] = Default::default();
            for j in 0..nv {
                for i in band.0..band.1 {
                    let k = j * nu + i;
                    if !holder[k] {
                        for c in 0..3 {
                            s[c].push(light[k][c]);
                        }
                    }
                }
            }
            if s[0].is_empty() {
                return None;
            }
            s.map(|mut s| percentile(&mut s, 0.97))
        };
        let base = base.map(|b| b.max(1e-6));
        let no_film = holder
            .iter()
            .zip(&light)
            .map(|(&h, l)| h || held_back(*l, base).iter().sum::<f32>() / 3.0 < -CLEAR)
            .collect();
        Some(Self {
            nu,
            nv,
            u_start,
            v_start,
            light,
            no_film,
            band,
            middle,
            base,
        })
    }

    /// How different a sample is from bare film.
    fn difference(&self, k: usize) -> f32 {
        let held = held_back(self.light[k], self.base);
        (held.iter().map(|h| h.abs()).sum::<f32>() / 3.0).min(MAX_DIFFERENCE)
    }

    /// The longest run of rows with film across most of the frames' band, bridging breaks
    /// of up to a millimetre. In rows, end exclusive.
    fn film_rows(&self, format: &Format) -> Option<(usize, usize)> {
        let width = (self.band.1 - self.band.0).max(1);
        let film: Vec<bool> = (0..self.nv)
            .map(|j| {
                let k = j * self.nu;
                let n = (self.band.0..self.band.1)
                    .filter(|&i| !self.no_film[k + i])
                    .count();
                n * 2 >= width
            })
            .collect();
        let bridge = format.mm.ceil() as usize;
        let mut best: Option<(usize, usize)> = None;
        let mut run: Option<(usize, usize)> = None;
        for (j, &f) in film.iter().enumerate() {
            if !f {
                continue;
            }
            run = match run {
                Some((s, e)) if j - e <= bridge => Some((s, j + 1)),
                _ => Some((j, j + 1)),
            };
            let (s, e) = run.unwrap();
            if best.is_none_or(|(bs, be)| e - s > be - bs) {
                best = Some((s, e));
            }
        }
        best
    }

    /// The frames along the strip.
    fn frames(&self, format: &Format) -> Vec<Found> {
        let Some((first, last)) = self.film_rows(format) else {
            return Vec::new();
        };
        if ((last - first) as f32) < 0.5 * format.frame_length {
            return Vec::new();
        }
        // How different each row of the frames' band is from bare film.
        let rows: Vec<f32> = (0..self.nv)
            .map(|j| {
                let (mut sum, mut n) = (0.0, 0);
                for i in self.band.0..self.band.1 {
                    let k = j * self.nu + i;
                    if !self.no_film[k] {
                        sum += self.difference(k);
                        n += 1;
                    }
                }
                if n > 0 { sum / n as f32 } else { 0.0 }
            })
            .collect();
        let rows = Profile::new(&rows);
        let (e0, e1) = (first as f32, last as f32);
        let fit = fit_spacing(&rows, e0, e1, format);

        let mut frames = Vec::new();
        for (s, e) in fit.frames(e0, e1) {
            let mean = rows.mean(s, e);
            if mean - fit.gap < BLANK {
                continue;
            }
            let contrast = mean - fit.gap;
            let at_start = s <= e0 + 0.5;
            let at_end = e >= e1 - 0.5;
            let start = (!at_start)
                .then(|| find_edge(&rows, s, fit.gap, contrast, format, true))
                .flatten();
            let end = (!at_end)
                .then(|| find_edge(&rows, e, fit.gap, contrast, format, false))
                .flatten();
            let (s, e) = (start.unwrap_or(s), end.unwrap_or(e));
            let along_seen = [at_start || start.is_some(), at_end || end.is_some()];

            // Across: the profile of the frame's rows, sample by sample across the strip.
            let inner = (s + format.mm, e - format.mm);
            let (j0, j1) = if inner.1 > inner.0 {
                (
                    inner.0 as usize,
                    (inner.1 as usize).max(inner.0 as usize + 1),
                )
            } else {
                (s as usize, e as usize)
            };
            let j1 = j1.min(self.nv);
            let columns: Vec<f32> = (0..self.nu)
                .map(|i| {
                    let n = (j1 - j0).max(1) as f32;
                    (j0..j1)
                        .map(|j| self.difference(j * self.nu + i))
                        .sum::<f32>()
                        / n
                })
                .collect();
            let columns = Profile::new(&columns);
            let half = format.frame_width / 2.0;
            let left = find_edge(
                &columns,
                self.middle - half,
                fit.gap,
                contrast,
                format,
                true,
            );
            let right = find_edge(
                &columns,
                self.middle + half,
                fit.gap,
                contrast,
                format,
                false,
            );
            let edges_seen = along_seen.iter().filter(|&&s| s).count() as u8
                + u8::from(left.is_some())
                + u8::from(right.is_some());
            frames.push(Found {
                start: s,
                end: e,
                left: left.unwrap_or(self.middle - half).max(0.0),
                right: right.unwrap_or(self.middle + half).min(self.nu as f32),
                edges_seen,
                placed_by_spacing: !along_seen[0] && !along_seen[1],
            });
        }
        frames
    }
}

/// The share of the light through `base` that `light` holds back, per channel.
fn held_back(light: [f32; 3], base: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|c| 1.0 - light[c] / base[c])
}

/// Running sums of a profile, for averages over any stretch of it, fractions of a sample
/// included. Sample `i` covers `i..i + 1`.
struct Profile {
    sums: Vec<f32>,
}

impl Profile {
    fn new(values: &[f32]) -> Self {
        let mut sums = Vec::with_capacity(values.len() + 1);
        let mut total = 0.0;
        sums.push(0.0);
        for &v in values {
            total += v;
            sums.push(total);
        }
        Self { sums }
    }

    fn len(&self) -> f32 {
        (self.sums.len() - 1) as f32
    }

    /// The sum of the profile from its start to `x`.
    fn cumulative(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, self.len());
        let i = (x as usize).min(self.sums.len() - 2);
        let f = x - i as f32;
        self.sums[i] + (self.sums[i + 1] - self.sums[i]) * f
    }

    /// Sample `i`, if there is one.
    fn value(&self, i: usize) -> Option<f32> {
        Some(self.sums.get(i + 1)? - self.sums[i])
    }

    fn sum(&self, a: f32, b: f32) -> f32 {
        self.cumulative(b) - self.cumulative(a)
    }

    fn mean(&self, a: f32, b: f32) -> f32 {
        if b - a <= 1e-3 {
            return 0.0;
        }
        self.sum(a, b) / (b - a)
    }
}

/// The edge near `at` on `profile`: where it steps up from about `gap` (bare film) going
/// forwards when `rising`, backwards otherwise. `None` unless the step is a good part of
/// `contrast`, the frame's average difference from the gap.
fn find_edge(
    profile: &Profile,
    at: f32,
    gap: f32,
    contrast: f32,
    format: &Format,
    rising: bool,
) -> Option<f32> {
    let side = EDGE_SIDE_MM * format.mm;
    let window = EDGE_WINDOW_MM * format.mm;
    let lo = (at - window).max(side);
    let hi = (at + window).min(profile.len() - side);
    let step = 0.25f32.min(side / 2.0);
    let mut best: Option<(f32, f32)> = None;
    let mut x = lo;
    while x <= hi {
        let (before, after) = (profile.mean(x - side, x), profile.mean(x, x + side));
        let (outside, inside) = if rising {
            (before, after)
        } else {
            (after, before)
        };
        let step_up = inside - outside;
        if outside < gap + 0.4 * contrast && best.is_none_or(|(_, b)| step_up > b) {
            best = Some((x, step_up));
        }
        x += step;
    }
    let (x, _) = best.filter(|&(_, step_up)| step_up > MIN_EDGE && step_up > 0.5 * contrast)?;
    // The step's middle, from sample to sample: averaging places it where the frame's
    // content grows strong rather than at its edge.
    let (before, after) = (profile.mean(x - side, x), profile.mean(x, x + side));
    let half_way = (before + after) / 2.0;
    let (first, last) = (
        (x - side).floor() as usize,
        ((x + side).ceil() as usize).min(profile.sums.len() - 1),
    );
    let crossing = (first..last).find_map(|i| {
        let i = if rising { i } else { first + last - 1 - i };
        let next = if rising { i.checked_sub(1)? } else { i + 1 };
        let (v, prev) = (profile.value(i)?, profile.value(next)?);
        // Sample centres are at i + 0.5.
        (v >= half_way && prev < half_way).then(|| {
            let f = (half_way - prev) / (v - prev);
            next as f32 + 0.5 + (i as f32 - next as f32) * f
        })
    });
    Some(crossing.unwrap_or(x))
}

/// A row of frames at one spacing along a strip, in rows.
#[derive(Debug)]
struct Spacing {
    pitch: f32,
    length: f32,
    /// Where the first frame starts, at most a pitch before the strip's film starts.
    first: f32,
    /// The average difference from bare film between the frames.
    gap: f32,
}

impl Spacing {
    /// The frames that are mostly on the film from `e0` to `e1`, cut to it.
    fn frames(&self, e0: f32, e1: f32) -> Vec<(f32, f32)> {
        let mut out = Vec::new();
        let mut s = self.first;
        while s < e1 {
            let (a, b) = (s.max(e0), (s + self.length).min(e1));
            if b - a >= 0.6 * self.length {
                out.push((a, b));
            }
            s += self.pitch;
        }
        out
    }
}

/// The spacing of frames that best fits the strip: whose frames differ most from bare film
/// and whose gaps least, preferring the format's own spacing a little.
fn fit_spacing(rows: &Profile, e0: f32, e1: f32, format: &Format) -> Spacing {
    let step = SEARCH_STEP_MM * format.mm;
    let total = rows.sum(e0, e1);
    let min_gap = 0.3 * format.mm;
    let mut best: Option<(f32, Spacing)> = None;
    let mut pitch = format.pitch - 1.5 * format.mm;
    while pitch <= format.pitch + 1.5 * format.mm {
        let mut length = format.frame_length - 2.0 * format.mm;
        let max_length = (format.frame_length + format.mm).min(pitch - 0.4 * format.mm);
        while length <= max_length {
            let mut offset = 0.0;
            while offset < pitch {
                let first = e0 - pitch + offset;
                let (mut frame_sum, mut frame_len) = (0.0, 0.0);
                let mut s = first;
                while s < e1 {
                    let (a, b) = (s.max(e0), (s + length).min(e1));
                    if b > a {
                        frame_sum += rows.sum(a, b);
                        frame_len += b - a;
                    }
                    s += pitch;
                }
                let gap_len = (e1 - e0) - frame_len;
                if gap_len >= min_gap && frame_len > 0.0 {
                    let gap = (total - frame_sum) / gap_len;
                    let mut score = frame_sum / frame_len - gap;
                    let off_mm = ((pitch - format.pitch).abs()
                        + (length - format.frame_length).abs())
                        / format.mm;
                    score -= 0.003 * off_mm;
                    if best.as_ref().is_none_or(|(b, _)| score > *b) {
                        best = Some((
                            score,
                            Spacing {
                                pitch,
                                length,
                                first,
                                gap,
                            },
                        ));
                    }
                }
                offset += step;
            }
            length += step;
        }
        pitch += step;
    }
    best.map(|(_, s)| s).unwrap_or_else(|| {
        // No room for a gap: one frame, centred.
        let length = format.frame_length.min(e1 - e0);
        Spacing {
            pitch: e1 - e0 + format.pitch,
            length,
            first: (e0 + e1 - length) / 2.0,
            gap: 0.0,
        }
    })
}

/// Cut `frame` out of `image`, a scan of `area` at `dpi`, straightened: the result is
/// `frame`'s width by its height at `dpi`, with the scan's pixel type.
pub fn crop_frame(
    image: &DynamicImage,
    dpi: u32,
    area: ScanArea,
    frame: &FrameRect,
) -> DynamicImage {
    let px_per_mm = dpi as f32 / MM_PER_INCH;
    let out_w = ((frame.width_mm * px_per_mm).round() as u32).max(1);
    let out_h = ((frame.height_mm * px_per_mm).round() as u32).max(1);
    // Output pixel (i, j)'s centre is at the scan pixel position origin + i·di + j·dj.
    let (ex, ey) = frame.axes();
    let (sx, sy) = (
        frame.width_mm / out_w as f32,
        frame.height_mm / out_h as f32,
    );
    let (x0, y0) = frame.from_local(
        -frame.width_mm / 2.0 + sx / 2.0,
        -frame.height_mm / 2.0 + sy / 2.0,
    );
    let map = Affine {
        origin: [(x0 - area.x_mm) * px_per_mm, (y0 - area.y_mm) * px_per_mm],
        di: [ex[0] * sx * px_per_mm, ex[1] * sx * px_per_mm],
        dj: [ey[0] * sy * px_per_mm, ey[1] * sy * px_per_mm],
    };
    let (w, h) = (image.width() as usize, image.height() as usize);
    let size = (out_w as usize, out_h as usize);
    match image {
        DynamicImage::ImageRgb8(b) => DynamicImage::ImageRgb8(
            ImageBuffer::<Rgb<u8>, _>::from_raw(
                out_w,
                out_h,
                resample(b.as_raw(), w, h, 3, size, &map),
            )
            .expect("size matches"),
        ),
        DynamicImage::ImageLuma8(b) => DynamicImage::ImageLuma8(
            ImageBuffer::<Luma<u8>, _>::from_raw(
                out_w,
                out_h,
                resample(b.as_raw(), w, h, 1, size, &map),
            )
            .expect("size matches"),
        ),
        DynamicImage::ImageLuma16(b) => DynamicImage::ImageLuma16(
            ImageBuffer::<Luma<u16>, _>::from_raw(
                out_w,
                out_h,
                resample(b.as_raw(), w, h, 1, size, &map),
            )
            .expect("size matches"),
        ),
        DynamicImage::ImageRgb16(b) => DynamicImage::ImageRgb16(
            ImageBuffer::<Rgb<u16>, _>::from_raw(
                out_w,
                out_h,
                resample(b.as_raw(), w, h, 3, size, &map),
            )
            .expect("size matches"),
        ),
        other => {
            let rgb = other.to_rgb16();
            DynamicImage::ImageRgb16(
                ImageBuffer::<Rgb<u16>, _>::from_raw(
                    out_w,
                    out_h,
                    resample(rgb.as_raw(), w, h, 3, size, &map),
                )
                .expect("size matches"),
            )
        }
    }
}

struct Affine {
    origin: [f32; 2],
    di: [f32; 2],
    dj: [f32; 2],
}

/// A sample type of an image: 8 or 16 bits.
trait Sample: Copy {
    fn to_f32(self) -> f32;
    fn from_f32(v: f32) -> Self;
}

impl Sample for u8 {
    fn to_f32(self) -> f32 {
        f32::from(self)
    }
    fn from_f32(v: f32) -> Self {
        v.round().clamp(0.0, 255.0) as u8
    }
}

impl Sample for u16 {
    fn to_f32(self) -> f32 {
        f32::from(self)
    }
    fn from_f32(v: f32) -> Self {
        v.round().clamp(0.0, 65535.0) as u16
    }
}

/// Bilinear resampling of an image with `channels` per pixel at the positions `map` gives
/// each output pixel, clamped at the image's borders.
fn resample<T: Sample>(
    raw: &[T],
    w: usize,
    h: usize,
    channels: usize,
    (out_w, out_h): (usize, usize),
    map: &Affine,
) -> Vec<T> {
    let mut out = Vec::with_capacity(out_w * out_h * channels);
    for j in 0..out_h {
        for i in 0..out_w {
            let (i, j) = (i as f32, j as f32);
            let x = map.origin[0] + map.di[0] * i + map.dj[0] * j - 0.5;
            let y = map.origin[1] + map.di[1] * i + map.dj[1] * j - 0.5;
            let fx = x.clamp(0.0, (w - 1) as f32);
            let fy = y.clamp(0.0, (h - 1) as f32);
            let (x0, y0) = (fx as usize, fy as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            for c in 0..channels {
                let p = |x: usize, y: usize| raw[(y * w + x) * channels + c].to_f32();
                let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * tx;
                let bottom = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * tx;
                out.push(T::from_f32(top + (bottom - top) * ty));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
