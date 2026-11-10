//! Flatbed calibration computed from the scanner's own reads, as Epson's driver does before
//! every scan (see `docs/epson-v500`):
//!
//! 1. A small read of a masked strip of the CCD gives the black level the scanner subtracts
//!    during the white read (`1e 42`).
//! 2. A 32-line white read of the calibration strip gives each pixel's white level.
//! 3. A 16-line read with the lamp off gives each pixel's dark level.
//! 4. The final `1e 42` carries per-channel white and dark bases (the dimmest pixel under the
//!    glass, and the smoothed dark minimum of the even and of the odd pixels), and the shading
//!    table (`1e 84` type 6) per pixel and channel: the white level above its base in steps of
//!    a power of two (upper 10 bits) and the smoothed dark level above its base (lower 6 bits).
//!    The image read's `1e 41` names the step ([`Shading::step_code`]): Epson's driver takes
//!    the finest whose 1,023 steps span the white levels, 64 for the whole flatbed's width and
//!    4 or 8 for film.
//!
//! At 6,400 dpi the dark level differs between pixels: the even and odd ones of the read (from
//! its first) by some 300–650 counts, the CCD's four staggered lines by up to ~100 more. So
//! dark levels are smoothed among pixels a multiple of [`DARK_PHASES`] apart, and each of the
//! two is measured against its own base.
//!
//! All reads are 16-bit little-endian, pixel-interleaved RGB.

use std::ops::RangeInclusive;

const CHANNELS: usize = 3;
/// Width of the box filter for dark levels, in pixels.
const DARK_SMOOTHING: usize = 65;
/// Pixels whose dark levels may differ: smoothed only among those this many apart.
const DARK_PHASES: usize = 4;
/// Pixels below this fraction of the brightest are outside the calibration strip.
const LIT_FRACTION: f64 = 0.1;
const WHITE_MAX: f64 = 1023.0;
const DARK_MAX: f64 = 63.0;

/// Per-channel black levels subtracted during the white read, from the masked-strip read.
pub fn white_read_darks(dark_ref: &[u8]) -> [u16; 3] {
    let means = column_means(dark_ref, 1)
        .into_iter()
        .fold([(0.0, 0usize); 3], |mut acc, px| {
            for c in 0..CHANNELS {
                acc[c].0 += px[c];
                acc[c].1 += 1;
            }
            acc
        });
    means.map(|(sum, n)| (sum / n.max(1) as f64).round() as u16)
}

/// The levels and shading table for a read `width` pixels wide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shading {
    pub white_base: [u16; 3],
    /// Per channel, the even pixels' dark base and the odd ones'.
    pub dark_base: [[u16; 2]; 3],
    /// The white field's step is `1 << white_shift` counts.
    pub white_shift: u8,
    /// `width` × 3 little-endian u16 entries, pixel-interleaved.
    pub table: Vec<u8>,
}

impl Shading {
    /// Compute from the white read (taken with `white_darks` subtracted) and the dark read.
    /// The white base comes from the first `bed` pixels, which lie under the glass; beyond
    /// them the read falls off the calibration strip. The white field's step is the finest
    /// power of two among `shifts` that spans the white levels. Returns `None` if the reads
    /// don't split into `width`-pixel lines.
    pub fn compute(
        white: &[u8],
        dark: &[u8],
        width: usize,
        white_darks: [u16; 3],
        bed: usize,
        shifts: RangeInclusive<u8>,
    ) -> Option<Self> {
        let line = width * CHANNELS * 2;
        if width == 0
            || dark.is_empty()
            || !white.len().is_multiple_of(line)
            || !dark.len().is_multiple_of(line)
        {
            return None;
        }
        let white = column_means(white, white.len() / line);
        let dark = smooth(
            &column_means(dark, dark.len() / line),
            DARK_SMOOTHING,
            DARK_PHASES,
        );

        let mut white_base = [0u16; 3];
        let mut dark_base = [[0u16; 2]; 3];
        // The white read had `white_darks` subtracted; measure white against each pixel's own
        // dark level instead.
        let amplitude: Vec<[f64; 3]> = white
            .iter()
            .zip(&dark)
            .map(|(w, d)| std::array::from_fn(|c| w[c] + f64::from(white_darks[c]) - d[c]))
            .collect();
        // Per channel, the brightest pixel and the dimmest lit one under the glass.
        let span: [(f64, f64); 3] = std::array::from_fn(|c| {
            let max = amplitude.iter().map(|a| a[c]).fold(0.0, f64::max);
            let lit_min = amplitude[..bed.min(width)]
                .iter()
                .map(|a| a[c])
                .filter(|&a| a >= LIT_FRACTION * max)
                .fold(f64::INFINITY, f64::min);
            (lit_min.min(max), max)
        });
        let range = span.iter().map(|(lo, hi)| hi - lo).fold(0.0, f64::max);
        let white_shift = shifts
            .clone()
            .find(|&s| range <= f64::from(1u32 << s) * WHITE_MAX)
            .unwrap_or(*shifts.end());
        let step = f64::from(1u32 << white_shift);
        for c in 0..CHANNELS {
            let (lit_min, max) = span[c];
            let base = lit_min.max(max - step * WHITE_MAX).max(0.0);
            white_base[c] = base.floor().min(65535.0) as u16;
            for (parity, base) in dark_base[c].iter_mut().enumerate() {
                let levels = || dark.iter().skip(parity).step_by(2).map(|d| d[c]);
                let min = levels().fold(f64::INFINITY, f64::min);
                let max = levels().fold(f64::NEG_INFINITY, f64::max);
                // The lowest, unless the highest would then be out of the table's reach: then
                // halfway, missing both ends by as little.
                let b = min.max((min + max - DARK_MAX) / 2.0);
                *base = b.floor().clamp(0.0, 65535.0) as u16;
            }
        }

        let mut table = Vec::with_capacity(line);
        for (x, (a, d)) in amplitude.iter().zip(&dark).enumerate() {
            for c in 0..CHANNELS {
                let hi = ((a[c] - f64::from(white_base[c])) / step)
                    .round()
                    .clamp(0.0, WHITE_MAX) as u16;
                let lo = (d[c] - f64::from(dark_base[c][x % 2]))
                    .round()
                    .clamp(0.0, DARK_MAX) as u16;
                table.extend_from_slice(&(hi << 6 | lo).to_le_bytes());
            }
        }
        Some(Self {
            white_base,
            dark_base,
            white_shift,
            table,
        })
    }

    /// The image read's `1e 41` byte 15 for this table's white step, where byte 11 is `kind`.
    /// The step is 2 ^ (upper nibble + (`kind` & 3) − 2) counts, in all of the captures: `60`
    /// with `82` is 64 (the flatbed's width), `50` with `82` and `40` with `83` 32, `30` with
    /// `83` 16, `20` with `83` 8 (film up to 3,200 dpi), `50` with `80` 8 (6,400 dpi).
    pub fn step_code(&self, kind: u8) -> u8 {
        (self.white_shift + 2).saturating_sub(kind & 3) << 4
    }

    /// The `1e 42` parameters: white bases, then each channel's even and odd dark bases.
    pub fn levels(&self) -> [u8; 18] {
        let mut out = [0u8; 18];
        for c in 0..CHANNELS {
            out[2 * c..2 * c + 2].copy_from_slice(&self.white_base[c].to_le_bytes());
            for k in 0..2 {
                let i = 6 + 4 * c + 2 * k;
                out[i..i + 2].copy_from_slice(&self.dark_base[c][k].to_le_bytes());
            }
        }
        out
    }
}

/// `1e 42` parameters for the white read: no white bases, `darks` subtracted.
pub fn white_read_levels(darks: [u16; 3]) -> [u8; 18] {
    Shading {
        white_base: [0; 3],
        dark_base: darks.map(|d| [d; 2]),
        white_shift: 0,
        table: Vec::new(),
    }
    .levels()
}

/// Mean of each pixel and channel over `lines` lines of 16-bit RGB.
fn column_means(data: &[u8], lines: usize) -> Vec<[f64; 3]> {
    let lines = lines.max(1);
    let width = data.len() / (lines * CHANNELS * 2);
    let mut sums = vec![[0.0; 3]; width];
    for line in data.chunks_exact(width * CHANNELS * 2).take(lines) {
        for (sum, px) in sums.iter_mut().zip(line.as_chunks::<{ CHANNELS * 2 }>().0) {
            for c in 0..CHANNELS {
                sum[c] += f64::from(u16::from_le_bytes([px[2 * c], px[2 * c + 1]]));
            }
        }
    }
    for sum in &mut sums {
        for v in sum.iter_mut() {
            *v /= lines as f64;
        }
    }
    sums
}

/// Box filter about `n` pixels wide among pixels `phases` apart, with each phase's edges
/// repeated.
fn smooth(values: &[[f64; 3]], n: usize, phases: usize) -> Vec<[f64; 3]> {
    let half = (n / phases / 2) as isize;
    let len = values.len() as isize;
    let phases = phases as isize;
    (0..len)
        .map(|i| {
            let phase = i % phases;
            let last = (len - 1 - phase) / phases;
            let mut sum = [0.0; 3];
            for j in i / phases - half..=i / phases + half {
                let v = values[(j.clamp(0, last) * phases + phase) as usize];
                for c in 0..CHANNELS {
                    sum[c] += v[c];
                }
            }
            sum.map(|s| s / (2 * half + 1) as f64)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb16(lines: usize, px: impl Fn(usize) -> [u16; 3], width: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..lines {
            for x in 0..width {
                for v in px(x) {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        out
    }

    fn entry(table: &[u8], x: usize, c: usize) -> (u16, u16) {
        let i = 2 * (3 * x + c);
        let v = u16::from_le_bytes([table[i], table[i + 1]]);
        (v >> 6, v & 63)
    }

    #[test]
    fn darks_from_masked_strip() {
        let data = rgb16(4, |x| [1340 + x as u16 % 2 * 2, 1250, 1090], 16);
        assert_eq!(white_read_darks(&data), [1341, 1250, 1090]);
        assert_eq!(
            white_read_levels([0x544, 0x4d2, 0x446])[6..],
            [0x44, 5, 0x44, 5, 0xd2, 4, 0xd2, 4, 0x46, 4, 0x46, 4]
        );
    }

    #[test]
    fn shading_encodes_white_and_dark() {
        let width = 200;
        // Vignetted white (already dark-subtracted), falling off past the bed at x = 170 and
        // unlit beyond x = 180.
        let white = rgb16(
            32,
            |x| {
                let v = match x {
                    0..170 => 24000 + 100 * x as u16,
                    170..180 => 3000 * (180 - x as u16),
                    _ => 30,
                };
                [v, v + 1000, v]
            },
            width,
        );
        let dark = rgb16(16, |x| [1300 + (x / 50) as u16 * 10, 1250, 1100], width);
        let s = Shading::compute(&white, &dark, width, [1300, 1250, 1100], 170, 6..=6).unwrap();
        assert_eq!(s.table.len(), width * 6);
        // White base: the dimmest lit pixel, measured against its own dark level.
        assert_eq!(s.white_base[1], 25000);
        assert_eq!(s.dark_base, [[1300; 2], [1250; 2], [1100; 2]]);
        assert_eq!(entry(&s.table, 0, 1), (0, 0));
        assert_eq!(entry(&s.table, 64, 1), (100, 0));
        // Smoothed dark steps from 1300 to 1330 across the read.
        let (_, lo) = entry(&s.table, 199, 0);
        assert!((25..=30).contains(&lo), "{lo}");
        // Unlit pixels get no white.
        assert_eq!(entry(&s.table, 190, 1).0, 0);
        assert_eq!(s.levels()[..2], s.white_base[0].to_le_bytes());
    }

    #[test]
    fn dark_levels_by_phase() {
        // As at 6,400 dpi: odd pixels some 300 counts below even ones, and the four phases
        // apart; even ones further apart than the table's 63 counts reach.
        let width = 400;
        let phase = [0u16, 300, 90, 290];
        let level = |x: usize| 1400 - phase[x % 4] + (x / 100) as u16 * 3;
        let white = rgb16(32, |_| [30000; 3], width);
        let dark = rgb16(16, |x| [level(x); 3], width);
        let s = Shading::compute(&white, &dark, width, [1300; 3], width, 6..=6).unwrap();
        // Even: 1,310 to 1,409 is 99 apart, so halfway (1,328), 18 off at both ends; odd: the
        // lowest, 1,100.
        assert_eq!(s.dark_base[0], [1328, 1100]);
        assert_eq!(s.levels()[6..10], [0x30, 5, 0x4c, 4]);
        for x in 0..width {
            let (_, lo) = entry(&s.table, x, 0);
            let got = i32::from(s.dark_base[0][x % 2] + lo);
            let err = (got - i32::from(level(x))).abs();
            assert!(
                err <= if x % 2 == 0 { 19 } else { 2 },
                "x {x}: {got} vs {}",
                level(x)
            );
        }
    }

    #[test]
    fn white_step_spans_the_levels() {
        // Film's white levels: a few thousand counts of vignetting over the base.
        let width = 400;
        let dark = rgb16(16, |_| [1300; 3], width);
        let film = |spread: u16| {
            let white = rgb16(
                32,
                |x| [48000 + (u32::from(spread) * x as u32 / 399) as u16; 3],
                width,
            );
            Shading::compute(&white, &dark, width, [1300; 3], width, 2..=6).unwrap()
        };
        let s = film(5360);
        // 5,360 counts take 1,340 steps of 4, 670 of 8.
        assert_eq!(s.white_shift, 3);
        assert_eq!(entry(&s.table, 399, 0).0, 670);
        // Byte 15 as Epson Scan sends it at 6,400 dpi (`80`) and up to 3,200 (`83`).
        assert_eq!(s.step_code(0x80), 0x50);
        assert_eq!(s.step_code(0x83), 0x20);
        let s = film(3000);
        assert_eq!(s.white_shift, 2);
        assert_eq!(entry(&s.table, 399, 0).0, 750);
        assert_eq!(s.step_code(0x83), 0x10);
        // The flatbed keeps 64, `60` with `82`.
        let white = rgb16(32, |x| [24000 + 80 * x as u16; 3], width);
        let s = Shading::compute(&white, &dark, width, [1300; 3], width, 6..=6).unwrap();
        assert_eq!((s.white_shift, s.step_code(0x82)), (6, 0x60));
        // Beyond 1,023 steps of the coarsest, the base rises to keep the brightest in reach.
        let white = rgb16(32, |x| [1000 + 150 * x as u16; 3], width);
        let s = Shading::compute(&white, &dark, width, [1300; 3], width, 2..=5).unwrap();
        assert_eq!(s.white_shift, 5);
        assert_eq!(s.white_base[0], 1000 + 150 * 399 - 32 * 1023);
    }

    #[test]
    fn rejects_mismatched_width() {
        assert!(Shading::compute(&[0; 12], &[0; 12], 3, [0; 3], 3, 6..=6).is_none());
    }
}
