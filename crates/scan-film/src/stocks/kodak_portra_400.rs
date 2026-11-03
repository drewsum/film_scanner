//! Kodak Professional Portra 400: ISO 400 daylight color negative film for C-41, made for
//! skin tones and wide exposure latitude.
//!
//! # Where the data comes from
//!
//! Its own datasheet: KODAK publication E-4050, January 2025 edition. Characteristic curves
//! (Status M, daylight, Log H ref −1.44), spectral sensitivities and spectral dye
//! densities, all read from the PDF's vector paths.
//!
//! # Known differences to watch
//!
//! The sheet's curves sit a little denser than its own exposure check. At the printed Log
//! H ref the red curve puts a grey card at 0.89 and the lightest step of a paper grey scale
//! at 1.27, where the check says 0.77 to 0.87 and 1.13 to 1.23. The check is word for word
//! the one on Ektar 100's sheet (whose curves sit further off), so it looks like a table
//! shared across Kodak's professional films. The printed Log H ref is kept.
//!
//! # What is measured instead of assumed
//!
//! The film base (D-min) is measured on every scan; the sheet's D-min (0.22, 0.65, 0.87) is
//! only where the curves start.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::kodak_e4050_portra_400;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

pub const KODAK_PORTRA_400: FilmStock = FilmStock {
    id: "kodak-portra-400",
    manufacturer: "Kodak",
    name: "Portra 400",
    iso: 400,
    kind: FilmKind::ColorNegative,
    sheet: &kodak_e4050_portra_400::SHEET,
    provenance: "Kodak's own datasheet for Portra 400 (publication E-4050, 2025). The film \
                 base is measured on every scan.",
    model: &MODEL,
};
