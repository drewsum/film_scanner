//! Kodak Gold 200: ISO 200 daylight color negative film for C-41 (Kodak film code GB).
//!
//! # Where the data comes from
//!
//! Its own datasheet, KODAK publication E-7022 (February 2016): characteristic curves
//! (Status M, daylight, Log H ref −1.14), spectral sensitivities and spectral dye
//! densities. The 2007 edition's Gold 200 curves are the same to within 0.003 density, read
//! independently from its own page: the published data hasn't changed in that time.
//!
//! The extracted red curve agrees with the sheet's own exposure check: a grey card at Log
//! H ref reads 0.95 (Kodak: 0.85 to 1.05), the lightest step of a paper grey scale 1.31
//! (Kodak: 1.25 to 1.45).
//!
//! # What is measured instead of assumed
//!
//! The film base (D-min) is measured on every scan; the sheet's D-min (0.25, 0.66, 0.99) is
//! only where the curves start.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::kodak_e7022;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

pub const KODAK_GOLD_200: FilmStock = FilmStock {
    id: "kodak-gold-200",
    manufacturer: "Kodak",
    name: "Gold 200",
    iso: 200,
    kind: FilmKind::ColorNegative,
    sheet: &kodak_e7022::SHEET,
    provenance: "Kodak's own datasheet for Gold 200 (publication E-7022, 2016). The film \
                 base is measured on every scan.",
    model: &MODEL,
};
