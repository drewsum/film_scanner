//! Kodak UltraMax 400: ISO 400 daylight consumer color negative film for C-41 (Kodak film
//! code GC).
//!
//! # Where the data comes from
//!
//! Its own datasheet: KODAK publication E-7023, February 2016 edition. Characteristic
//! curves (Status M, daylight, Log H ref −1.44), spectral sensitivities and spectral dye
//! densities, all read from the PDF's vector paths.
//!
//! The extracted red curve agrees with the sheet's own exposure check: a grey card at Log
//! H ref reads 0.96 (Kodak: 0.80 to 1.00), the lightest step of a paper grey scale 1.32
//! (Kodak: 1.20 to 1.40).
//!
//! # What is measured instead of assumed
//!
//! The film base (D-min) is measured on every scan; the sheet's D-min (0.29, 0.69, 0.98) is
//! only where the curves start.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::kodak_e7023_ultramax_400;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

pub const KODAK_ULTRAMAX_400: FilmStock = FilmStock {
    id: "kodak-ultramax-400",
    manufacturer: "Kodak",
    name: "UltraMax 400",
    iso: 400,
    kind: FilmKind::ColorNegative,
    sheet: &kodak_e7023_ultramax_400::SHEET,
    provenance: "Kodak's own datasheet for UltraMax 400 (publication E-7023, 2016). The \
                 film base is measured on every scan.",
    model: &MODEL,
};
