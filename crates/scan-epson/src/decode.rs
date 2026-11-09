//! Raw scan data to an image.

use scan_core::Gray16Image;
use scan_core::image::{ImageBuffer, Pixel, Rgb, RgbImage};

use crate::geometry::{FlatbedPlan, skew_lines};
use crate::tables;
use crate::template::RawLayout;

/// A 16-bit RGB image.
pub type Rgb16Image = ImageBuffer<Rgb<u16>, Vec<u16>>;

/// What a read becomes: the output size at the requested resolution, made from raw data at
/// the hardware resolution: `hw_dpi` lines per inch, and `hw_x_dpi` pixels per inch across
/// (more than the lines in the V500's 3,200 × 2,400 film mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resample {
    pub dpi: u32,
    pub hw_dpi: u32,
    pub hw_x_dpi: u32,
    pub width: u32,
    pub height: u32,
    /// Lines each raw column sees the film later than the earliest, by raw column mod 4: the
    /// CCD's staggered pixels, which the V500 leaves in its film reads above 1,600 dpi.
    pub stagger: [u8; 4],
    /// The raw line of output row 0 (before the color skew): lines read above the area.
    pub first_line: u32,
}

impl From<&FlatbedPlan> for Resample {
    fn from(plan: &FlatbedPlan) -> Self {
        Self {
            dpi: plan.dpi,
            hw_dpi: plan.hw_dpi,
            hw_x_dpi: plan.hw_dpi,
            width: plan.width,
            height: plan.height,
            stagger: [0; 4],
            first_line: 0,
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
    let mut stream = Stream::rgb16(layout, out);
    stream.push(raw);
    stream.into_rgb16()
}

/// Decode an infrared read: all three CCD rows see the infrared, so realign them like color
/// and average them into one linear plane.
pub fn infrared(raw: &[u8], layout: RawLayout, out: Resample) -> Gray16Image {
    let mut stream = Stream::infrared(layout, out);
    stream.push(raw);
    stream.into_infrared()
}

/// A 16-bit read decoded as it arrives, like [`rgb16`] or [`infrared`]: each output row is
/// made once the raw lines behind it are in, and lines no row needs any more are let go,
/// so a read far bigger than memory allows can be decoded.
pub struct Stream {
    grid: Grid,
    kind: Kind,
    /// Raw lines from `base` on, as received.
    buf: Vec<u8>,
    base: usize,
    /// The next output row, and the rows so far: 3 channels, or 1 for infrared.
    row: u32,
    pixels: Vec<u16>,
}

enum Kind {
    /// Color, through the tone curve, as a table of every 16-bit value.
    Tone(Vec<u16>),
    /// The channels averaged, linear.
    Infrared,
}

impl Stream {
    pub fn rgb16(layout: RawLayout, out: Resample) -> Self {
        let curve = (0..=u16::MAX)
            .map(|v| (65535.0 * tables::tone(f64::from(v) / 65535.0)).round() as u16)
            .collect();
        Self::new(layout, out, Kind::Tone(curve))
    }

    pub fn infrared(layout: RawLayout, out: Resample) -> Self {
        Self::new(layout, out, Kind::Infrared)
    }

    fn new(layout: RawLayout, out: Resample, kind: Kind) -> Self {
        let channels = if matches!(kind, Kind::Infrared) { 1 } else { 3 };
        Self {
            grid: Grid::new(layout, out),
            kind,
            buf: Vec::new(),
            base: 0,
            row: 0,
            pixels: Vec::with_capacity(out.width as usize * out.height as usize * channels),
        }
    }

    /// The read's next bytes.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        self.rows(false);
    }

    pub fn into_rgb16(mut self) -> Rgb16Image {
        self.rows(true);
        let out = self.grid.plan;
        ImageBuffer::from_raw(out.width, out.height, self.pixels).expect("whole image")
    }

    pub fn into_infrared(mut self) -> Gray16Image {
        self.rows(true);
        let out = self.grid.plan;
        ImageBuffer::from_raw(out.width, out.height, self.pixels).expect("whole image")
    }

    /// Make the rows whose lines are in (all of them at the `end`, with 0 past the read),
    /// then let go of the lines behind them.
    fn rows(&mut self, end: bool) {
        let line_len = self.grid.width * 6;
        let out = self.grid.plan;
        while self.row < out.height {
            let lines = self.grid.lines(self.row);
            if !end && self.base + self.buf.len() / line_len < lines.end {
                break;
            }
            let (buf, start) = (&self.buf, 2 * self.base * self.grid.width * 3);
            let sample = |i: usize| {
                let at = 2 * i - start;
                buf.get(at..at + 2)
                    .map_or(0, |b| u32::from(u16::from_le_bytes([b[0], b[1]])))
            };
            for x in 0..out.width {
                let mean = self.grid.pixel(x, self.row, sample);
                match &self.kind {
                    Kind::Tone(curve) => {
                        self.pixels.extend(mean.map(|m| curve[m.round() as usize]))
                    }
                    Kind::Infrared => {
                        let m = mean.map(|m| u32::from(m.round() as u16));
                        self.pixels.push(((m[0] + m[1] + m[2] + 1) / 3) as u16);
                    }
                }
            }
            self.row += 1;
        }
        // Rows only go down the read, so lines above the next row's are done with. Let them
        // go a half buffer at a time, not to move the rest for every block.
        if self.row < out.height {
            let done = (self.grid.lines(self.row).start - self.base) * line_len;
            if done > 0 && done >= self.buf.len() / 2 {
                let done = done.min(self.buf.len() / line_len * line_len);
                self.buf.drain(..done);
                self.base += done / line_len;
            }
        }
    }
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
    let grid = Grid::new(layout, plan);
    ImageBuffer::from_fn(plan.width, plan.height, |x, y| {
        Rgb(grid.pixel(x, y, &sample).map(&out))
    })
}

/// Which raw samples make each output pixel.
struct Grid {
    plan: Resample,
    /// Raw pixels per line.
    width: usize,
    crop_x: usize,
    skew: usize,
    scale_x: f64,
    scale_y: f64,
}

impl Grid {
    fn new(layout: RawLayout, plan: Resample) -> Self {
        let scale = |hw: u32| f64::from(hw) / f64::from(plan.dpi);
        Self {
            plan,
            width: layout.width as usize,
            crop_x: layout.crop_x as usize,
            skew: skew_lines(plan.hw_dpi) as usize,
            scale_x: scale(plan.hw_x_dpi),
            scale_y: scale(plan.hw_dpi),
        }
    }

    /// Raw lines or columns covered by output index `i`.
    fn span(i: u32, scale: f64) -> std::ops::Range<usize> {
        let a = (f64::from(i) * scale) as usize;
        let b = ((f64::from(i + 1) * scale) as usize).max(a + 1);
        a..b
    }

    /// The raw lines output row `y` reads, every channel and column's.
    fn lines(&self, y: u32) -> std::ops::Range<usize> {
        let rows = Self::span(y, self.scale_y);
        let first = self.plan.first_line as usize;
        let late = 2 * self.skew + usize::from(*self.plan.stagger.iter().max().unwrap());
        first + rows.start..first + rows.end + late
    }

    /// Pixel (x, y)'s mean in each channel: red from its lines 2 × skew on, green from skew
    /// on and blue from its own, each column from its stagger's lines on.
    fn pixel(&self, x: u32, y: u32, sample: impl Fn(usize) -> u32) -> [f64; 3] {
        let cols = Self::span(x, self.scale_x);
        let rows = Self::span(y, self.scale_y);
        let n = (cols.len() * rows.len()) as f64;
        let first = self.plan.first_line as usize;
        [(0, 2 * self.skew), (1, self.skew), (2, 0)].map(|(ch, offset)| {
            let mut sum = 0u64;
            for line in rows.clone() {
                for col in cols.clone() {
                    let col = col + self.crop_x;
                    let line = first + line + offset + usize::from(self.plan.stagger[col % 4]);
                    sum += u64::from(sample((line * self.width + col) * 3 + ch));
                }
            }
            sum as f64 / n
        })
    }
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

    #[test]
    fn undoes_the_stagger() {
        // 1:1 at 40 dpi (a skew of 1 line). Each raw sample is the film row it sees: its
        // column's stagger lines behind its line. Realigned, every output row is one film row.
        let stagger = [1, 3, 0, 2];
        let mut out = Resample {
            dpi: 40,
            hw_dpi: 40,
            hw_x_dpi: 40,
            width: 8,
            height: 2,
            stagger,
            first_line: 0,
        };
        let layout = RawLayout {
            lines: 8,
            width: 8,
            crop_x: 0,
        };
        let raw: Vec<u8> = (0..8u16)
            .flat_map(|line| {
                (0..8).flat_map(move |col| [10 + line - u16::from(stagger[col % 4]); 3])
            })
            .flat_map(u16::to_le_bytes)
            .collect();
        for first in [0, 1] {
            out.first_line = first;
            let ir = infrared(&raw, layout, out);
            for y in 0..2 {
                let row: Vec<u16> = (0..8).map(|x| ir.get_pixel(x, y).0[0]).collect();
                // Blue, green and red from lines y, y + 1 and y + 2 (after the first): film
                // rows 10 + first + y + 1 on average.
                assert_eq!(row, vec![11 + (first + y) as u16; 8], "row {y}");
            }
        }
    }

    #[test]
    fn streams_in_any_pieces() {
        // 1,200 dpi from a 3,200 × 2,400 read with a stagger: a read in odd-sized pieces
        // decodes to the same image as the whole, while holding only a few lines at a time.
        let out = Resample {
            dpi: 1200,
            hw_dpi: 2400,
            hw_x_dpi: 3200,
            width: 30,
            height: 1000,
            stagger: [0, 6, 0, 6],
            first_line: 3,
        };
        let layout = RawLayout {
            lines: 3 + 2000 + 2 * skew_lines(2400) + 6,
            width: 80,
            crop_x: 0,
        };
        let len = layout.lines as usize * layout.width as usize * 6;
        let raw: Vec<u8> = (0..len).map(|i| (i * 7919 % 251) as u8).collect();
        let whole = rgb16(&raw, layout, out);
        let mut stream = Stream::rgb16(layout, out);
        let mut held = 0;
        for piece in raw.chunks(1234) {
            stream.push(piece);
            held = held.max(stream.buf.len());
        }
        assert_eq!(stream.into_rgb16(), whole);
        assert!(held < len / 4, "held {held} of {len}");
        let mut stream = Stream::infrared(layout, out);
        for piece in raw.chunks(777) {
            stream.push(piece);
        }
        assert_eq!(stream.into_infrared(), infrared(&raw, layout, out));
        // A read cut short decodes with zeros past its end, like the whole decoder.
        let mut stream = Stream::rgb16(layout, out);
        stream.push(&raw[..len / 2]);
        assert_eq!(stream.into_rgb16(), rgb16(&raw[..len / 2], layout, out));
    }
}
