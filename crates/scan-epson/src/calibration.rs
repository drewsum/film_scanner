//! Flatbed calibration computed from the scanner's own reads, as Epson's driver does before
//! every scan (see `docs/epson-v500`):
//!
//! 1. A small read of a masked strip of the CCD gives the black level the scanner subtracts
//!    during the white read (`1e 42`).
//! 2. A 32-line white read of the calibration strip gives each pixel's white level.
//! 3. A 16-line read with the lamp off gives each pixel's dark level.
//! 4. The final `1e 42` carries per-channel white and dark bases, and the shading table
//!    (`1e 84` type 6) per pixel and channel: the white level above its base in 64-count
//!    steps (upper 10 bits) and the smoothed dark level above its base (lower 6 bits).
//!
//! All reads are 16-bit little-endian, pixel-interleaved RGB.

const CHANNELS: usize = 3;
/// Width of the box filter for dark levels.
const DARK_SMOOTHING: usize = 65;
/// Pixels below this fraction of the brightest are outside the calibration strip.
const LIT_FRACTION: f64 = 0.1;
const WHITE_STEP: f64 = 64.0;
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
    pub dark_base: [u16; 3],
    /// `width` × 3 little-endian u16 entries, pixel-interleaved.
    pub table: Vec<u8>,
}

impl Shading {
    /// Compute from the white read (taken with `white_darks` subtracted) and the dark read.
    /// Returns `None` if the reads don't split into `width`-pixel lines.
    pub fn compute(white: &[u8], dark: &[u8], width: usize, white_darks: [u16; 3]) -> Option<Self> {
        let line = width * CHANNELS * 2;
        if width == 0
            || dark.is_empty()
            || !white.len().is_multiple_of(line)
            || !dark.len().is_multiple_of(line)
        {
            return None;
        }
        let white = column_means(white, white.len() / line);
        let dark = smooth(&column_means(dark, dark.len() / line), DARK_SMOOTHING);

        let mut white_base = [0u16; 3];
        let mut dark_base = [0u16; 3];
        let mut amplitude = vec![[0.0; 3]; width];
        for c in 0..CHANNELS {
            // The white read had `white_darks` subtracted; measure white against each
            // pixel's own dark level instead.
            for (a, (w, d)) in amplitude.iter_mut().zip(white.iter().zip(&dark)) {
                a[c] = w[c] + f64::from(white_darks[c]) - d[c];
            }
            let max = amplitude.iter().map(|a| a[c]).fold(0.0, f64::max);
            let lit_min = amplitude
                .iter()
                .map(|a| a[c])
                .filter(|&a| a >= LIT_FRACTION * max)
                .fold(f64::INFINITY, f64::min);
            let base = lit_min.min(max).max(max - WHITE_STEP * WHITE_MAX).max(0.0);
            white_base[c] = base.floor().min(65535.0) as u16;
            let dark_min = dark.iter().map(|d| d[c]).fold(f64::INFINITY, f64::min);
            dark_base[c] = dark_min.floor().clamp(0.0, 65535.0) as u16;
        }

        let mut table = Vec::with_capacity(line);
        for (a, d) in amplitude.iter().zip(&dark) {
            for c in 0..CHANNELS {
                let hi = ((a[c] - f64::from(white_base[c])) / WHITE_STEP)
                    .round()
                    .clamp(0.0, WHITE_MAX) as u16;
                let lo = (d[c] - f64::from(dark_base[c]))
                    .round()
                    .clamp(0.0, DARK_MAX) as u16;
                table.extend_from_slice(&(hi << 6 | lo).to_le_bytes());
            }
        }
        Some(Self {
            white_base,
            dark_base,
            table,
        })
    }

    /// The `1e 42` parameters: white bases, then each dark base twice.
    pub fn levels(&self) -> [u8; 18] {
        let mut out = [0u8; 18];
        for c in 0..CHANNELS {
            out[2 * c..2 * c + 2].copy_from_slice(&self.white_base[c].to_le_bytes());
            for k in 0..2 {
                let i = 6 + 4 * c + 2 * k;
                out[i..i + 2].copy_from_slice(&self.dark_base[c].to_le_bytes());
            }
        }
        out
    }
}

/// `1e 42` parameters for the white read: no white bases, `darks` subtracted.
pub fn white_read_levels(darks: [u16; 3]) -> [u8; 18] {
    Shading {
        white_base: [0; 3],
        dark_base: darks,
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

/// Box filter of `n` pixels, with the edges repeated.
fn smooth(values: &[[f64; 3]], n: usize) -> Vec<[f64; 3]> {
    let half = (n / 2) as isize;
    let len = values.len() as isize;
    (0..len)
        .map(|i| {
            let mut sum = [0.0; 3];
            for j in i - half..=i + half {
                let v = values[j.clamp(0, len - 1) as usize];
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
        // Vignetted white (already dark-subtracted), unlit beyond x = 180.
        let white = rgb16(
            32,
            |x| {
                let v = if x < 180 { 24000 + 100 * x as u16 } else { 30 };
                [v, v + 1000, v]
            },
            width,
        );
        let dark = rgb16(16, |x| [1300 + (x / 50) as u16 * 10, 1250, 1100], width);
        let s = Shading::compute(&white, &dark, width, [1300, 1250, 1100]).unwrap();
        assert_eq!(s.table.len(), width * 6);
        // White base: the dimmest lit pixel, measured against its own dark level.
        assert_eq!(s.white_base[1], 25000);
        assert_eq!(s.dark_base, [1300, 1250, 1100]);
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
    fn rejects_mismatched_width() {
        assert!(Shading::compute(&[0; 12], &[0; 12], 3, [0; 3]).is_none());
    }
}
