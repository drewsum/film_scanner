//! From a scan of a negative to a positive: what a photofinisher's printer does, with the
//! film's own curves.
//!
//! 1. **Base.** Every channel is divided by the light through the bare film base (measured
//!    on the same scan), which removes the orange mask and the scanner's own color: what's
//!    left is each dye's density above D-min.
//! 2. **Film.** Each layer's inverse characteristic curve turns its density back into the
//!    scene light that exposed it ([`FilmModel::exposure`]). This is what keeps a neutral
//!    neutral through the toe, the straight line and the shoulder: each layer has its own
//!    gamma and its own curve.
//! 3. **Printing.** Exposure and color balance are offsets in log exposure, as a printer's
//!    lights are.
//! 4. **Color.** [`Rendering::Print`] lets each layer drive its own color, as an optical
//!    print does; [`Rendering::Colorimetric`] mixes them with the matrix fitted from the
//!    film's spectral sensitivities.
//! 5. **Tone.** A print-like curve (see [`Tone`]) for display, then sRGB encoding.
//!
//! Black-and-white negatives ([`Inverter::black_and_white`]) go through the same steps on
//! their film's one curve (see [`MonoModel`]), or a generic film's straight line when there
//! is no profile, their channels mixed to grey (see [`Mono`]). Slides
//! ([`Inverter::positive`]) have no profile: the light through one is taken as the scene's.

use crate::model::{FilmModel, GREY, MonoModel};
use crate::spectral::{self, Mat3};

/// How the film's layers become color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Rendering {
    /// Each layer drives its own channel, as when the negative is printed on color paper,
    /// whose layers see one dye each: the film's own color, as its maker designed it to
    /// print.
    #[default]
    Print,
    /// The layers mixed by [`FilmModel::colorimetric`], towards the scene's colors as the
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
    /// Per channel, red, green, blue: more (+) or less (−) of that color, in stops.
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

/// A toning of black-and-white prints, as the chemistry tints the silver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Toning {
    #[default]
    None,
    /// Warm brown, strongest in the highlights.
    Sepia,
    /// Cool purple-brown in the shadows.
    Selenium,
    /// Blue, as a cyanotype.
    Blue,
}

impl Toning {
    pub const ALL: [Toning; 4] = [Toning::None, Toning::Sepia, Toning::Selenium, Toning::Blue];

    pub fn label(self) -> &'static str {
        match self {
            Toning::None => "None",
            Toning::Sepia => "Sepia",
            Toning::Selenium => "Selenium",
            Toning::Blue => "Blue",
        }
    }

    /// The power each channel's display value (0–1) is raised to at full strength: above 1
    /// takes that color out of the midtones, below 1 adds it. Black and white stay.
    fn powers(self) -> [f32; 3] {
        match self {
            Toning::None => [1.0; 3],
            Toning::Sepia => [0.88, 1.0, 1.3],
            Toning::Selenium => [1.04, 1.12, 1.0],
            Toning::Blue => [1.3, 1.08, 0.85],
        }
    }
}

/// How black-and-white film is turned grey: for silver film scanned in color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mono {
    /// How much each of red, green and blue counts towards the grey, scaled to add up to 1.
    /// Silver is near neutral, so this mostly trades one channel's noise for another's; a
    /// chromogenic (C-41) black-and-white film's dye differs more.
    pub mix: [f32; 3],
    pub toning: Toning,
    /// 0 (none) to 1.
    pub toning_amount: f32,
}

impl Default for Mono {
    fn default() -> Self {
        Self {
            mix: [1.0 / 3.0; 3],
            toning: Toning::None,
            toning_amount: 0.5,
        }
    }
}

/// A generic black-and-white negative's density above its base where a grey card exposed
/// as the film's speed asks lands: about where the published curves of such films put it.
pub const BW_GREY_DENSITY: f32 = 0.75;
/// A generic black-and-white negative's gamma (contrast index), developed normally.
pub const BW_GAMMA: f32 = 0.62;

#[derive(Clone, Debug)]
enum Kind {
    /// A color negative, with its film's curves.
    Color {
        model: Box<FilmModel>,
        matrix: Option<[[f32; 3]; 3]>,
    },
    /// A black-and-white negative, on its film's curve, or a generic straight line.
    Mono {
        mono: Mono,
        film: Option<Box<MonoModel>>,
    },
    /// A slide: the light through it is the scene's. `power` is its contrast about grey.
    Positive { power: f32 },
}

/// Turns the light through a negative (or a slide) into a positive, for one film, base and
/// printing.
#[derive(Clone, Debug)]
pub struct Inverter {
    kind: Kind,
    /// Linear light through the bare film base, per channel, on the scan's scale.
    base: [f32; 3],
    /// Printing exposure per channel, as a factor on scene light.
    gain: [f32; 3],
    tone: Tone,
}

impl Inverter {
    /// For a color negative of the film `model`.
    pub fn new(model: &FilmModel, base: [f32; 3], printing: &Printing) -> Self {
        let matrix = match printing.rendering {
            Rendering::Print => None,
            Rendering::Colorimetric => Some(to_f32(model.colorimetric())),
        };
        Self {
            kind: Kind::Color {
                model: Box::new(model.clone()),
                matrix,
            },
            base: base.map(|b| b.max(1e-6)),
            gain: printing
                .balance_ev
                .map(|b| 2f32.powf(printing.exposure_ev + b)),
            tone: Tone::new(printing.contrast),
        }
    }

    /// For a black-and-white negative: each channel turned back into light on the film's
    /// curve, or without a profile on a generic film's straight line ([`BW_GREY_DENSITY`],
    /// [`BW_GAMMA`]), mixed to grey, printed, then toned. The printing's color balance and
    /// rendering don't apply.
    pub fn black_and_white(
        base: [f32; 3],
        printing: &Printing,
        mono: &Mono,
        film: Option<&MonoModel>,
    ) -> Self {
        let total: f32 = mono.mix.iter().map(|m| m.max(0.0)).sum();
        let mix = if total > 0.0 {
            mono.mix.map(|m| m.max(0.0) / total)
        } else {
            [1.0 / 3.0; 3]
        };
        Self {
            kind: Kind::Mono {
                mono: Mono { mix, ..*mono },
                film: film.map(|f| Box::new(f.clone())),
            },
            base: base.map(|b| b.max(1e-6)),
            gain: [2f32.powf(printing.exposure_ev); 3],
            tone: Tone::new(printing.contrast),
        }
    }

    /// For a slide (or any positive film), without a profile: the light through it, with
    /// the printing's exposure and balance, is the scene's, its contrast moved about grey
    /// by the printing's (unchanged at [`DEFAULT_CONTRAST`]).
    pub fn positive(printing: &Printing) -> Self {
        Self {
            kind: Kind::Positive {
                power: printing.contrast / DEFAULT_CONTRAST,
            },
            base: [1.0; 3],
            gain: printing
                .balance_ev
                .map(|b| 2f32.powf(printing.exposure_ev + b)),
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
        let e = match &self.kind {
            Kind::Color { model, .. } => model.exposure(c, density),
            Kind::Mono {
                film: Some(film), ..
            } => film.exposure(density),
            Kind::Mono { film: None, .. } => {
                GREY * 10f32.powf((density - BW_GREY_DENSITY) / BW_GAMMA)
            }
            Kind::Positive { .. } => light.max(0.0),
        };
        e * self.gain[c]
    }

    /// Whether each output channel depends on its own input channel alone, so a channel
    /// can be rendered with [`Inverter::display_channel`].
    pub fn separable(&self) -> bool {
        match &self.kind {
            Kind::Color { matrix, .. } => matrix.is_none(),
            Kind::Mono { .. } => false,
            Kind::Positive { .. } => true,
        }
    }

    /// Whether this makes a black-and-white image, so color balance means nothing.
    pub fn is_mono(&self) -> bool {
        matches!(self.kind, Kind::Mono { .. })
    }

    /// The display value (sRGB encoded, 0–1) of scene light `e`, for a separable inverter.
    pub fn display_channel(&self, e: f32) -> f32 {
        let light = match self.kind {
            Kind::Positive { power } if e > 0.0 => GREY * (e / GREY).powf(power),
            Kind::Positive { .. } => 0.0,
            _ => self.tone.apply(e),
        };
        spectral::srgb_encode(f64::from(light.min(1.0))) as f32
    }

    /// The display color (sRGB encoded, 0–1) of scene light `e`.
    pub fn display(&self, e: [f32; 3]) -> [f32; 3] {
        match &self.kind {
            Kind::Color {
                matrix: Some(m), ..
            } => {
                let e: [f32; 3] = std::array::from_fn(|r| (0..3).map(|k| m[r][k] * e[k]).sum());
                e.map(|v| self.display_channel(v))
            }
            Kind::Mono { mono, .. } => {
                let grey: f32 = (0..3).map(|c| mono.mix[c] * e[c]).sum();
                let v = self.display_channel(grey);
                let amount = mono.toning_amount.clamp(0.0, 1.0);
                mono.toning
                    .powers()
                    .map(|p| v.powf(1.0 + (p - 1.0) * amount))
            }
            _ => e.map(|v| self.display_channel(v)),
        }
    }

    /// The display color of a pixel whose linear light through the film is `light`.
    pub fn pixel(&self, light: [f32; 3]) -> [f32; 3] {
        self.display(std::array::from_fn(|c| self.scene(c, light[c])))
    }
}

fn to_f32(m: &Mat3) -> [[f32; 3]; 3] {
    m.map(|row| row.map(|v| v as f32))
}

/// Printing that balances a roll the way a photofinishing printer does ("integrate to
/// grey"): each frame's average color, in log exposure, is taken to be grey, and the roll
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
