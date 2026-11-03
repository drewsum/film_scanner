//! Kodak Professional Ektar 100: ISO 100 daylight color negative film for C-41, Kodak's
//! finest-grained and most saturated.
//!
//! # Where the data comes from
//!
//! Its own datasheet: KODAK publication E-4046, January 2025 edition. Characteristic curves
//! (Status M, daylight, Log H ref −0.84), spectral sensitivities and spectral dye
//! densities, all read from the PDF's vector paths.
//!
//! # Known differences to watch
//!
//! The sheet's curves and its own exposure check disagree a little. At the printed Log H
//! ref the red curve puts a grey card at 0.93 and the lightest step of a paper grey scale
//! at 1.32, where the check says 0.77 to 0.87 and 1.13 to 1.23: 0.06 to 0.09 denser, about
//! a third of a stop. The extraction matches the printed curve (checked by overlaying it),
//! and Portra 400's sheet prints the same check word for word, so it looks like a table
//! shared across Kodak's professional films. The printed Log H ref is kept; Auto printing
//! sets the exposure from the frames either way.
//!
//! # What is measured instead of assumed
//!
//! The film base (D-min) is measured on every scan; the sheet's D-min (0.21, 0.64, 0.85) is
//! only where the curves start.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::kodak_e4046_ektar_100;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

pub const KODAK_EKTAR_100: FilmStock = FilmStock {
    id: "kodak-ektar-100",
    manufacturer: "Kodak",
    name: "Ektar 100",
    iso: 100,
    kind: FilmKind::ColorNegative,
    sheet: &kodak_e4046_ektar_100::SHEET,
    provenance: "Kodak's own datasheet for Ektar 100 (publication E-4046, 2025). The film \
                 base is measured on every scan.",
    model: &MODEL,
};
