//! A color negative film as its datasheet describes it, run forwards (scene to negative)
//! and backwards (negative to scene); and a black-and-white one ([`MonoModel`]).
//!
//! Exposure is measured as `e`, scene-linear light relative to a grey card: a grey card
//! (18% reflectance) exposed as the film's speed asks for is `e = 0.18`, and lands at the
//! datasheet's Log H ref. Each layer then turns its log exposure into dye, whose density
//! the characteristic curves give in Status M: the densitometry that sees a negative as
//! color print paper does, one dye per channel.

use crate::sheet::{Curve, Datasheet, MonoDatasheet};
use crate::spectral::{self, Mat3, SAMPLES, wavelengths};

/// A grey card's reflectance: `e` of a normally exposed midtone.
pub const GREY: f32 = 0.18;
/// The least gamma (density per unit of log exposure) a layer is given anywhere: in its
/// toe, and past the ends of its datasheet curve. Where the film itself is flatter (deep in
/// the toe) the inverse would multiply scanner noise without bound; this caps that at
/// 1 / MIN_GAMMA = 20 times.
pub const MIN_GAMMA: f32 = 0.05;

/// One layer's characteristic curve, made strictly increasing so it can be inverted.
#[derive(Clone, Debug)]
pub struct Layer {
    start: f32,
    step: f32,
    density: Vec<f32>,
    /// Gamma past the curve's high end: the shoulder's own, at least [`MIN_GAMMA`].
    end_gamma: f32,
}

impl Layer {
    /// The datasheet curve, raised where needed so it climbs at least [`MIN_GAMMA`]. A
    /// curve that dips (the blue curve of many negatives does, just above D-min, from the
    /// masking couplers) is held level instead: unexposed film is the lowest density the
    /// layer gives.
    pub fn new(curve: &Curve) -> Self {
        let rise = MIN_GAMMA * curve.step;
        let mut density: Vec<f32> = Vec::with_capacity(curve.values.len());
        for &v in curve.values {
            let v = match density.last() {
                Some(&prev) => v.max(prev + rise),
                None => v,
            };
            density.push(v);
        }
        let n = density.len();
        let tail = 4.min(n - 1);
        let end_gamma =
            ((density[n - 1] - density[n - 1 - tail]) / (tail as f32 * curve.step)).max(MIN_GAMMA);
        Self {
            start: curve.start,
            step: curve.step,
            density,
            end_gamma,
        }
    }

    fn end(&self) -> f32 {
        self.start + (self.density.len() - 1) as f32 * self.step
    }

    /// The density of unexposed film.
    pub fn d_min(&self) -> f32 {
        self.density[0]
    }

    /// Density at `log_h` (log lux-seconds).
    pub fn density(&self, log_h: f32) -> f32 {
        let d = &self.density;
        if log_h <= self.start {
            return d[0] - (self.start - log_h) * MIN_GAMMA;
        }
        let end = self.end();
        if log_h >= end {
            return d[d.len() - 1] + (log_h - end) * self.end_gamma;
        }
        let t = (log_h - self.start) / self.step;
        let i = (t as usize).min(d.len() - 2);
        let f = t - i as f32;
        d[i] + (d[i + 1] - d[i]) * f
    }

    /// The log exposure that gives `density`: the inverse of [`Layer::density`].
    pub fn log_exposure(&self, density: f32) -> f32 {
        let d = &self.density;
        if density <= d[0] {
            return self.start - (d[0] - density) / MIN_GAMMA;
        }
        let last = d[d.len() - 1];
        if density >= last {
            return self.end() + (density - last) / self.end_gamma;
        }
        // The first sample above it; d[0] < density < last, so 1 <= i < len.
        let i = d.partition_point(|&v| v <= density);
        let f = (density - d[i - 1]) / (d[i] - d[i - 1]);
        self.start + (i as f32 - 1.0 + f) * self.step
    }
}

/// A color negative film: its three layers, and how its layers' exposures relate to
/// color.
#[derive(Clone, Debug)]
pub struct FilmModel {
    layers: [Layer; 3],
    log_h_ref: f32,
    colorimetric: Mat3,
}

impl FilmModel {
    pub fn new(sheet: &Datasheet) -> Self {
        Self {
            layers: sheet.characteristic.each_ref().map(Layer::new),
            log_h_ref: sheet.log_h_ref,
            colorimetric: colorimetric_matrix(sheet),
        }
    }

    pub fn layer(&self, c: usize) -> &Layer {
        &self.layers[c]
    }

    /// Status M density of unexposed, processed film: base, fog and orange mask.
    pub fn d_min(&self) -> [f32; 3] {
        self.layers.each_ref().map(Layer::d_min)
    }

    /// The density above D-min of a negative exposed to scene light `e`, per layer. Film
    /// gets no clearer than unexposed: below the datasheet's lowest exposure it is D-min.
    pub fn negative(&self, e: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|c| {
            let log_h = self.log_h_ref + (e[c].max(1e-12) / GREY).log10();
            (self.layers[c].density(log_h) - self.layers[c].d_min()).max(0.0)
        })
    }

    /// The scene light `e` of layer `c` from its density above D-min: [`FilmModel::negative`]
    /// backwards.
    pub fn exposure(&self, c: usize, density_above_d_min: f32) -> f32 {
        let layer = &self.layers[c];
        let log_h = layer.log_exposure(layer.d_min() + density_above_d_min);
        GREY * 10f32.powf(log_h - self.log_h_ref)
    }

    /// The matrix from layer exposures to linear sRGB that best matches what the CIE
    /// observer would see, fitted from the film's spectral sensitivities (see
    /// [`colorimetric_matrix`]). White stays white.
    pub fn colorimetric(&self) -> &Mat3 {
        &self.colorimetric
    }
}

/// A black-and-white film: one curve of silver, which every channel of a scan sees alike.
#[derive(Clone, Debug)]
pub struct MonoModel {
    layer: Layer,
    log_h_ref: f32,
}

impl MonoModel {
    pub fn new(sheet: &MonoDatasheet) -> Self {
        Self {
            layer: Layer::new(&sheet.characteristic),
            log_h_ref: sheet.log_h_ref,
        }
    }

    pub fn layer(&self) -> &Layer {
        &self.layer
    }

    /// Density of unexposed, processed film: base plus fog.
    pub fn d_min(&self) -> f32 {
        self.layer.d_min()
    }

    /// The density above base plus fog of a negative exposed to scene light `e`.
    pub fn negative(&self, e: f32) -> f32 {
        let log_h = self.log_h_ref + (e.max(1e-12) / GREY).log10();
        (self.layer.density(log_h) - self.layer.d_min()).max(0.0)
    }

    /// The scene light `e` from a density above base plus fog: [`MonoModel::negative`]
    /// backwards.
    pub fn exposure(&self, density_above_d_min: f32) -> f32 {
        let log_h = self
            .layer
            .log_exposure(self.layer.d_min() + density_above_d_min);
        GREY * 10f32.powf(log_h - self.log_h_ref)
    }
}

/// Each layer's spectral sensitivity on the [`spectral::wavelengths`] grid, linear. Where
/// the datasheet plots nothing (below its axis: under a thousandth of the peak) it is 0.
pub fn sensitivities(sheet: &Datasheet) -> [[f64; SAMPLES]; 3] {
    sheet.log_sensitivity.map(|s| {
        let mut out = [0.0; SAMPLES];
        for (o, nm) in out.iter_mut().zip(wavelengths()) {
            *o = s.at(nm as f32).map_or(0.0, |l| 10f64.powf(f64::from(l)));
        }
        out
    })
}

/// The 3 × 3 matrix taking a film's layer exposures to linear sRGB, fitted by least
/// squares over a set of surface colors lit by daylight, with white held to white.
///
/// For each surface the layer exposures come from the datasheet's spectral sensitivities
/// (normalised so that daylight on a white surface exposes every layer equally, as the
/// characteristic curves assume) and the target from the CIE 1931 observer, adapted from
/// daylight to sRGB's D65 white with Bradford. The surfaces are smooth reflectance spectra
/// of every hue and purity: bumps, dips and edges across the spectrum (see
/// [`training_surfaces`]), the shapes natural and painted surfaces have.
pub fn colorimetric_matrix(sheet: &Datasheet) -> Mat3 {
    let sens = sensitivities(sheet);
    let light: Vec<f64> = wavelengths().map(spectral::daylight).collect();
    let response = |reflectance: &[f64; SAMPLES]| -> ([f64; 3], [f64; 3]) {
        let mut layers = [0.0; 3];
        let mut power = [0.0; SAMPLES];
        for i in 0..SAMPLES {
            power[i] = light[i] * reflectance[i];
            for c in 0..3 {
                layers[c] += sens[c][i] * power[i];
            }
        }
        (layers, spectral::xyz_of(&power))
    };
    // Both sides relative to a white surface: layer exposures of 1, and Y = 1.
    let (white_layers, white_xyz) = response(&[1.0; SAMPLES]);
    let white_y = white_xyz[1];
    let white_xyz = white_xyz.map(|v| v / white_y);
    let to_srgb = spectral::mul(
        &spectral::XYZ_TO_SRGB,
        &spectral::bradford(white_xyz, spectral::D65),
    );
    let samples: Vec<([f64; 3], [f64; 3])> = training_surfaces()
        .iter()
        .map(|r| {
            let (layers, xyz) = response(r);
            let e = std::array::from_fn(|c| layers[c] / white_layers[c]);
            (e, spectral::mul_vec(&to_srgb, xyz.map(|v| v / white_y)))
        })
        .collect();
    // White's target is white, to the precision of the conversions.
    let white = spectral::mul_vec(&to_srgb, white_xyz);
    std::array::from_fn(|row| fit_row(&samples, row, white[row]))
}

/// The row `m` minimising Σ (m · e − target[row])² with m · (1, 1, 1) = `sum`: eliminating
/// the third coefficient leaves a two-variable least-squares problem.
fn fit_row(samples: &[([f64; 3], [f64; 3])], row: usize, sum: f64) -> [f64; 3] {
    let (mut a, mut b) = ([[0.0; 2]; 2], [0.0; 2]);
    for (e, t) in samples {
        let f = [e[0] - e[2], e[1] - e[2]];
        let y = t[row] - sum * e[2];
        for i in 0..2 {
            b[i] += f[i] * y;
            for j in 0..2 {
                a[i][j] += f[i] * f[j];
            }
        }
    }
    let det = a[0][0] * a[1][1] - a[0][1] * a[1][0];
    let m0 = (b[0] * a[1][1] - b[1] * a[0][1]) / det;
    let m1 = (a[0][0] * b[1] - a[1][0] * b[0]) / det;
    [m0, m1, sum - m0 - m1]
}

/// Smooth surface reflectances of every hue: Gaussian bumps (saturated colors) and dips
/// (their complements, the purples among them) across the spectrum at three widths, edges
/// rising and falling (yellows, oranges, cyans, blues) at two sharpnesses, and greys.
pub fn training_surfaces() -> Vec<[f64; SAMPLES]> {
    let spectrum = |f: &dyn Fn(f64) -> f64| -> [f64; SAMPLES] {
        let mut out = [0.0; SAMPLES];
        for (o, nm) in out.iter_mut().zip(wavelengths()) {
            *o = f(nm);
        }
        out
    };
    let mut surfaces = Vec::new();
    for width in [20.0, 40.0, 70.0] {
        for centre in (400..=700).step_by(15).map(f64::from) {
            let bump = move |nm: f64| (-0.5 * ((nm - centre) / width).powi(2)).exp();
            surfaces.push(spectrum(&|nm| 0.05 + 0.85 * bump(nm)));
            surfaces.push(spectrum(&|nm| 0.9 - 0.85 * bump(nm)));
        }
    }
    for sharpness in [10.0, 25.0] {
        for edge in (420..=680).step_by(20).map(f64::from) {
            let rise = move |nm: f64| 1.0 / (1.0 + (-(nm - edge) / sharpness).exp());
            surfaces.push(spectrum(&|nm| 0.05 + 0.85 * rise(nm)));
            surfaces.push(spectrum(&|nm| 0.9 - 0.85 * rise(nm)));
        }
    }
    for grey in [0.05, 0.2, 0.5, 0.9] {
        surfaces.push([grey; SAMPLES]);
    }
    surfaces
}
