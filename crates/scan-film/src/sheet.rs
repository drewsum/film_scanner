//! The curves of a film's technical datasheet, as published.
//!
//! The tables themselves are generated from the datasheets' PDFs by
//! `tools/film-curves/extract_curves.py` (see [`crate::sheets`]). Channels are always in the
//! order red, green, blue: the red-sensitive (cyan dye forming) layer first.

/// A curve sampled on an even grid: `values[i]` is the curve at `start + i * step`.
#[derive(Clone, Copy, Debug)]
pub struct Curve {
    pub start: f32,
    pub step: f32,
    pub values: &'static [f32],
}

impl Curve {
    /// The last x sampled.
    pub fn end(&self) -> f32 {
        self.start + (self.values.len() - 1) as f32 * self.step
    }

    /// The x of sample `i`.
    pub fn x(&self, i: usize) -> f32 {
        self.start + i as f32 * self.step
    }

    /// The curve at `x`, interpolated linearly; the end values outside the samples.
    pub fn at(&self, x: f32) -> f32 {
        let t = ((x - self.start) / self.step).clamp(0.0, (self.values.len() - 1) as f32);
        let i = (t.floor() as usize).min(self.values.len() - 2);
        let f = t - i as f32;
        self.values[i] * (1.0 - f) + self.values[i + 1] * f
    }
}

/// A spectral curve sampled every `step_nm` from `start_nm`. `None` where the datasheet's
/// plot shows nothing: below its axis, or past the ends of the curve.
#[derive(Clone, Copy, Debug)]
pub struct Spectrum {
    pub start_nm: f32,
    pub step_nm: f32,
    pub values: &'static [Option<f32>],
}

impl Spectrum {
    /// The curve at `nm`, interpolated linearly between samples; `None` unless both samples
    /// around it are known (or `nm` is a known sample).
    pub fn at(&self, nm: f32) -> Option<f32> {
        let t = (nm - self.start_nm) / self.step_nm;
        if t < -1e-4 || t > (self.values.len() - 1) as f32 + 1e-4 {
            return None;
        }
        let i = t.round() as usize;
        if (t - i as f32).abs() < 1e-4 {
            return self.values[i];
        }
        let i = t.floor() as usize;
        let f = t - i as f32;
        Some(self.values[i]? * (1.0 - f) + self.values[i + 1]? * f)
    }
}

/// What a film manufacturer's technical datasheet tells about a color negative film.
#[derive(Clone, Copy, Debug)]
pub struct Datasheet {
    /// The film the sheet is for, as titled.
    pub film: &'static str,
    /// The publication number, e.g. `E-7022` for Kodak.
    pub publication: &'static str,
    pub edition: &'static str,
    pub url: &'static str,
    /// The exposure, in log lux-seconds, of a normally exposed grey card: where the
    /// characteristic curves put a properly exposed midtone.
    pub log_h_ref: f32,
    /// Status M density against log exposure (lux-seconds) to daylight, per channel. Status
    /// M is the densitometry made to see a negative as color print paper does.
    pub characteristic: [Curve; 3],
    /// Log spectral sensitivity of the red-, green- and blue-sensitive layers: the log of
    /// the reciprocal of the exposure (erg/cm²) that gives a density 0.2 above D-min.
    pub log_sensitivity: [Spectrum; 3],
    /// Diffuse spectral density of the processed film's minimum density (base, fog and the
    /// orange mask), and of a midscale neutral.
    pub dye_density_min: Spectrum,
    pub dye_density_midscale: Spectrum,
}

impl Datasheet {
    /// The density of unexposed film per channel: each characteristic curve at its lowest
    /// exposure.
    pub fn d_min(&self) -> [f32; 3] {
        self.characteristic.map(|c| c.values[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_interpolate_and_hold_their_ends() {
        let c = Curve {
            start: -1.0,
            step: 0.5,
            values: &[0.0, 1.0, 3.0],
        };
        assert_eq!(c.end(), 0.0);
        assert_eq!(c.at(-0.75), 0.5);
        assert_eq!(c.at(-0.25), 2.0);
        assert_eq!(c.at(-5.0), 0.0);
        assert_eq!(c.at(5.0), 3.0);
    }

    #[test]
    fn spectra_are_unknown_where_the_plot_shows_nothing() {
        let s = Spectrum {
            start_nm: 400.0,
            step_nm: 10.0,
            values: &[None, Some(1.0), Some(2.0), None],
        };
        assert_eq!(s.at(410.0), Some(1.0));
        assert_eq!(s.at(415.0), Some(1.5));
        assert_eq!(s.at(405.0), None);
        assert_eq!(s.at(425.0), None);
        assert_eq!(s.at(390.0), None);
        assert_eq!(s.at(440.0), None);
    }
}
