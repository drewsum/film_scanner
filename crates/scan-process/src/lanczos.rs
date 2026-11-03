//! The Lanczos kernel that frames are cut and channels registered with.
//!
//! Any kernel softens grain where it reads between pixels; 6 taps a side keep about half of
//! what 3 lose, at about 0.2 s for a 1,600 dpi frame. Weights come from a table of phases,
//! a thousandth of a pixel apart.

use std::sync::OnceLock;

/// Taps on each side of a sample position.
pub const A: usize = 6;
/// Samples weighed for one position.
pub const TAPS: usize = 2 * A;
/// Phases in the table, per pixel.
const PHASES: usize = 1024;

/// The weights of the [`TAPS`] samples about a position `t` (0–1) past the sample left of
/// it, from the sample `A − 1` left of that: normalized to sum to 1, and exact (a single 1)
/// on a whole pixel.
pub fn weights(t: f32) -> &'static [f32; TAPS] {
    static TABLE: OnceLock<Vec<[f32; TAPS]>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..=PHASES)
            .map(|i| exact(i as f32 / PHASES as f32))
            .collect()
    });
    &table[((t * PHASES as f32).round() as usize).min(PHASES)]
}

/// The first of the [`TAPS`] samples for position `p`, and the weights' phase.
pub fn split(p: f32) -> (isize, f32) {
    let f = p.floor();
    (f as isize - (A as isize - 1), p - f)
}

fn exact(t: f32) -> [f32; TAPS] {
    let a = A as f32;
    let mut w = std::array::from_fn(|k| {
        let x = t + (A - 1) as f32 - k as f32;
        if x.abs() < 1e-6 {
            1.0
        } else if x.abs() >= a {
            0.0
        } else {
            let px = std::f32::consts::PI * x;
            a * px.sin() * (px / a).sin() / (px * px)
        }
    });
    let sum: f32 = w.iter().sum();
    w.iter_mut().for_each(|v| *v /= sum);
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_pixels_are_single_samples() {
        for t in [0.0, 1.0] {
            let w = weights(t);
            let one = if t == 0.0 { A - 1 } else { A };
            for (k, &v) in w.iter().enumerate() {
                assert!((v - f32::from(k == one)).abs() < 1e-6, "t {t}: {w:?}");
            }
        }
    }

    #[test]
    fn halfway_is_symmetric() {
        let w = weights(0.5);
        for k in 0..A {
            assert!((w[k] - w[TAPS - 1 - k]).abs() < 1e-6, "{w:?}");
        }
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-5);
    }
}
