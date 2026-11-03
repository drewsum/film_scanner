//! Reto Amber D100: ISO 100 daylight color negative film for C-41 (35 mm, 27 exposures),
//! sold by RETO Project, made from Kodak motion picture film without its rem-jet backing.
//!
//! # Where the data comes from
//!
//! Reto publishes no curves. Retailers' listings describe Amber D100 as Kodak VISION3 50D
//! with the rem-jet removed, made by Kodak; it is profiled on Kodak's datasheet for that
//! film, KODAK VISION3 50D 5203 (H-1-5203t, July 2015): characteristic curves (Status M,
//! ECN-2, Log H ref −0.515 at EI 50), spectral sensitivities and dye densities.
//!
//! # Known differences to watch
//!
//! - **Speed.** Rated 100 against VISION3 50D's 50: frames exposed at the box speed sit a
//!   stop under the sheet's grey card. Auto printing sets the exposure from the frames.
//! - **Process.** Kodak's curves are for ECN-2; Amber D100 is sold for C-41, whose more
//!   active developer gives more contrast than the curves predict.
//! - **Halation.** Without rem-jet, bright lights glow red, which the profile leaves in.
//!
//! # What is measured instead of assumed
//!
//! The film base (D-min) is measured on every scan; the sheet's D-min is only where the
//! curves start.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::kodak_h1_5203_vision3_50d;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

pub const RETO_AMBER_D100: FilmStock = FilmStock {
    id: "reto-amber-d100",
    manufacturer: "Reto",
    name: "Amber D100",
    iso: 100,
    kind: FilmKind::ColorNegative,
    sheet: &kodak_h1_5203_vision3_50d::SHEET,
    provenance: "Reto publishes no curves; Amber D100 is reported to be Kodak VISION3 50D \
                 without rem-jet, so its profile uses Kodak's VISION3 50D datasheet \
                 (H-1-5203t), for ECN-2: developed in C-41 it has more contrast. The film \
                 base is measured on every scan.",
    model: &MODEL,
};
