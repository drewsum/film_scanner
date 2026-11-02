//! From a scan of a negative to a positive: what a photofinisher's printer does, with the
//! film's own curves.
//!
//! 1. **Base.** Every channel is divided by the light through the bare film base (measured
//!    on the same scan), which removes the orange mask and the scanner's own colour: what's
//!    left is each dye's density above D-min.
//! 2. **Film.** Each layer's inverse characteristic curve turns its density back into the
//!    scene light that exposed it ([`FilmModel::exposure`]). This is what keeps a neutral
//!    neutral through the toe, the straight line and the shoulder: each layer has its own
//!    gamma and its own curve.
//! 3. **Printing.** Exposure and colour balance are offsets in log exposure, as a printer's
//!    lights are.
//! 4. **Colour.** [`Rendering::Print`] lets each layer drive its own colour, as an optical
//!    print does; [`Rendering::Colorimetric`] mixes them with the matrix fitted from the
//!    film's spectral sensitivities.
//! 5. **Tone.** A print-like curve (see [`Tone`]) for display, then sRGB encoding.

use crate::model::{FilmModel, GREY};
use crate::spectral::{self, Mat3};

/// How the film's layers become colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Rendering {
    /// Each layer drives its own channel, as when the negative is printed on colour paper,
    /// whose layers see one dye each: the film's own colour, as its maker designed it to
    /// print.
    #[default]
    Print,
    /// The layers mixed by [`FilmModel::colorimetric`], towards the scene's colours as the
    /// CIE observer saw them through the film's spectral sensitivities. The film's
    /// chemistry already boosts saturation (interimage effects), so this can look more
    /// saturated than a print.
    Colorimetric,
}

/// The printer's settings: what a photofinisher sets per roll or frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Printing {
    /// Brighter (+) or darker (−), in stops.
    pub exposure_ev: f32,
    /// Per channel, red, green, blue: more (+) or less (−) of that colour, in stops.
    pub balance_ev: [f32; 3],
    /// The tone curve's contrast; see [`Tone`].
    pub contrast: f32,
    pub rendering: Rendering,
}

pub const DEFAULT_CONTRAST: f32 = 1.4;
pub const CONTRAST_RANGE: std::ops::RangeInclusive<f32> = 0.8..=2.2;

impl Default for Printing {
    fn default() -> Self {
        Self {
            exposure_ev: 0.0,
            balance_ev: [0.0; 3],
            contrast: DEFAULT_CONTRAST,
            rendering: Rendering::Print,
        }
    }
}

/// The display tone curve: scene light `e` to display light `e^p / (e^p + k^p)`, with `k`
/// chosen so a grey card (0.18) stays 0.18. Like a print it compresses the highlights
/// smoothly towards white and the shadows towards black; `p` is the contrast, the curve's
/// slope (in log–log) at black, and `p · 0.82` at grey.
#[derive(Clone, Copy, Debug)]
pub struct Tone {
    p: f32,
    k_p: f32,
}

impl Tone {
    pub fn new(contrast: f32) -> Self {
        let p = contrast.clamp(*CONTRAST_RANGE.start(), *CONTRAST_RANGE.end());
        // GREY^p / (GREY^p + k^p) = GREY  =>  k^p = GREY^p (1 / GREY - 1).
        Self {
            p,
            k_p: GREY.powf(p) * (1.0 / GREY - 1.0),
        }
    }

    /// Display light (0–1) of scene light `e`.
    pub fn apply(&self, e: f32) -> f32 {
        if e <= 0.0 {
            return 0.0;
        }
        let x = e.powf(self.p);
        x / (x + self.k_p)
    }
}

/// Turns the light through a negative into a positive, for one film, base and printing.
#[derive(Clone, Debug)]
pub struct Inverter {
    model: FilmModel,
    /// Linear light through the bare film base, per channel, on the scan's scale.
    base: [f32; 3],
    /// Printing exposure per channel, as a factor on scene light.
    gain: [f32; 3],
    matrix: Option<[[f32; 3]; 3]>,
    tone: Tone,
}

impl Inverter {
    pub fn new(model: &FilmModel, base: [f32; 3], printing: &Printing) -> Self {
        let matrix = match printing.rendering {
            Rendering::Print => None,
            Rendering::Colorimetric => Some(to_f32(model.colorimetric())),
        };
        Self {
            model: model.clone(),
            base: base.map(|b| b.max(1e-6)),
            gain: printing
                .balance_ev
                .map(|b| 2f32.powf(printing.exposure_ev + b)),
            matrix,
            tone: Tone::new(printing.contrast),
        }
    }

    pub fn base(&self) -> [f32; 3] {
        self.base
    }

    /// Scene light (0.18 for a normally exposed grey card, after printing) of channel `c`
    /// from `light`, the linear light through the film on the scan's scale.
    pub fn scene(&self, c: usize, light: f32) -> f32 {
        let base = self.base[c];
        let density = (base / light.max(base * 1e-5)).log10();
        self.model.exposure(c, density) * self.gain[c]
    }

    /// Whether each output channel depends on its own input channel alone, so a channel
    /// can be rendered with [`Inverter::display_channel`].
    pub fn separable(&self) -> bool {
        self.matrix.is_none()
    }

    /// The display value (sRGB encoded, 0–1) of scene light `e`, for a separable inverter.
    pub fn display_channel(&self, e: f32) -> f32 {
        spectral::srgb_encode(f64::from(self.tone.apply(e))) as f32
    }

    /// The display colour (sRGB encoded, 0–1) of scene light `e`.
    pub fn display(&self, e: [f32; 3]) -> [f32; 3] {
        let e = match &self.matrix {
            Some(m) => std::array::from_fn(|r| (0..3).map(|k| m[r][k] * e[k]).sum()),
            None => e,
        };
        e.map(|v| self.display_channel(v))
    }

    /// The display colour of a pixel whose linear light through the film is `light`.
    pub fn pixel(&self, light: [f32; 3]) -> [f32; 3] {
        self.display(std::array::from_fn(|c| self.scene(c, light[c])))
    }
}

fn to_f32(m: &Mat3) -> [[f32; 3]; 3] {
    m.map(|row| row.map(|v| v as f32))
}

/// Printing that balances a roll the way a photofinishing printer does ("integrate to
/// grey"): each frame's average colour, in log exposure, is taken to be grey, and the roll
/// gets the median of its frames' corrections, so one frame of green lawn or blue sea
/// doesn't tint the rest. The exposure brings the roll's typical frame's log-average to a
/// grey card.
///
/// `frames` holds each frame's mean log10 scene light per channel at neutral printing
/// (no exposure or balance). Contrast and rendering are kept from `printing`.
pub fn auto_printing(frames: &[[f32; 3]], printing: &Printing) -> Printing {
    if frames.is_empty() {
        return *printing;
    }
    let log2 = std::f32::consts::LOG10_2;
    let mut balance = [0.0; 3];
    for (c, b) in balance.iter_mut().enumerate() {
        let mut offsets: Vec<f32> = frames
            .iter()
            .map(|f| f[c] - (f[0] + f[1] + f[2]) / 3.0)
            .collect();
        *b = -median(&mut offsets) / log2;
    }
    let mean = balance.iter().sum::<f32>() / 3.0;
    let balance = balance.map(|b| b - mean);
    let mut levels: Vec<f32> = frames.iter().map(|f| (f[0] + f[1] + f[2]) / 3.0).collect();
    let exposure = (GREY.log10() - median(&mut levels)) / log2;
    Printing {
        exposure_ev: exposure,
        balance_ev: balance,
        ..*printing
    }
}

fn median(values: &mut [f32]) -> f32 {
    values.sort_by(f32::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}
