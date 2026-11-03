//! CineStill 50D, 400D and 800T: color negative films for C-41, made from Kodak motion
//! picture film (VISION3) for still cameras, without its rem-jet backing.
//!
//! # Where the data comes from
//!
//! CineStill publishes no curves. Each film is profiled on the Kodak VISION3 datasheet of
//! the emulsion it is made from:
//!
//! - **50D**: KODAK VISION3 50D 5203 (H-1-5203t, July 2015). 50D is widely reported to be
//!   VISION3 50D with the rem-jet removed.
//! - **800T**: KODAK VISION3 500T 5219 (H-1-5219t, July 2015). 800T is widely reported to
//!   be VISION3 500T with the rem-jet removed, rated 800 for C-41.
//! - **400D**: KODAK VISION3 250D 5207 (H-1-5207t, April 2009). CineStill calls 400D a
//!   new film made with its manufacturing partner, not a repackaged motion picture stock;
//!   it is widely reported to be built on VISION3 250D, the nearest documented emulsion.
//!   This is the weakest of the three.
//!
//! Kodak's VISION3 sheets give characteristic curves (Status M, ECN-2), spectral
//! sensitivities and spectral dye densities; Log H ref is each sheet's camera-stop zero,
//! the grey card at the film's exposure index (50, 250, 500).
//!
//! # Known differences to watch
//!
//! - **Process.** Kodak's curves are for ECN-2; CineStill films are developed in C-41,
//!   whose developer is more active. Expect more contrast than the curves predict, and a
//!   different color balance, which Auto printing takes out.
//! - **Speed.** 800T is rated 800 against VISION3 500T's 500, and 400D 400 against 250D's
//!   250: frames exposed at the box speed sit up to two-thirds of a stop under the sheet's
//!   grey card. Auto printing sets the exposure from the frames themselves.
//! - **Halation.** Without rem-jet, bright lights glow red. That is in the image, not the
//!   curves, and the profile leaves it there.
//!
//! # What is measured instead of assumed
//!
//! The film base (D-min) is measured on every scan; the sheet's D-min is only where the
//! curves start.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::{
    kodak_h1_5203_vision3_50d, kodak_h1_5207_vision3_250d, kodak_h1_5219_vision3_500t,
};
use crate::stock::{FilmKind, FilmStock};

static MODEL_50D: OnceLock<FilmModel> = OnceLock::new();
static MODEL_400D: OnceLock<FilmModel> = OnceLock::new();
static MODEL_800T: OnceLock<FilmModel> = OnceLock::new();

pub const CINESTILL_50D: FilmStock = FilmStock {
    id: "cinestill-50d",
    manufacturer: "CineStill",
    name: "50D",
    iso: 50,
    kind: FilmKind::ColorNegative,
    sheet: &kodak_h1_5203_vision3_50d::SHEET,
    provenance: "CineStill publishes no curves. 50D is reported to be Kodak VISION3 50D \
                 without rem-jet, so its profile uses Kodak's VISION3 50D datasheet \
                 (H-1-5203t), for ECN-2: developed in C-41 it has more contrast. The film \
                 base is measured on every scan.",
    model: &MODEL_50D,
};

pub const CINESTILL_400D: FilmStock = FilmStock {
    id: "cinestill-400d",
    manufacturer: "CineStill",
    name: "400D",
    iso: 400,
    kind: FilmKind::ColorNegative,
    sheet: &kodak_h1_5207_vision3_250d::SHEET,
    provenance: "CineStill publishes no curves. 400D is widely reported to be built on \
                 Kodak VISION3 250D, so its profile uses Kodak's VISION3 250D datasheet \
                 (H-1-5207t), for ECN-2: developed in C-41 it has more contrast. The film \
                 base is measured on every scan.",
    model: &MODEL_400D,
};

pub const CINESTILL_800T: FilmStock = FilmStock {
    id: "cinestill-800t",
    manufacturer: "CineStill",
    name: "800T",
    iso: 800,
    kind: FilmKind::ColorNegative,
    sheet: &kodak_h1_5219_vision3_500t::SHEET,
    provenance: "CineStill publishes no curves. 800T is reported to be Kodak VISION3 500T \
                 without rem-jet, so its profile uses Kodak's VISION3 500T datasheet \
                 (H-1-5219t), for tungsten light and ECN-2: developed in C-41 it has more \
                 contrast. The film base is measured on every scan.",
    model: &MODEL_800T,
};
