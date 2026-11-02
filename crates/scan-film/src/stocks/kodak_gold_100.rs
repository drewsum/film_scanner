//! Kodak Gold 100: ISO 100 daylight colour negative film for C-41 (Kodak film code GA),
//! discontinued; profiled for scanning old negatives.
//!
//! # Where the data comes from
//!
//! Its own datasheet: KODAK publication E-7022, February 2007 edition, "KODAK GOLD 100 and
//! 200 Films". Gold 100 has characteristic curves of its own (Status M, daylight); the
//! spectral sensitivity and dye density plots are printed once for both films, so Gold 100
//! shares Gold 200's.
//!
//! The sheet prints Gold 100's Log H ref as "0.84" without its minus sign; it is −0.84:
//! one stop (0.30) more exposure than Gold 200's −1.14, as ISO 100 against ISO 200 needs,
//! and it puts a grey card at the red density the sheet's exposure check gives for Gold
//! 100 (0.90 to 1.10; the extracted curve reads 1.06).
//!
//! # What is measured instead of assumed
//!
//! The film base (D-min) is measured on every scan; the sheet's curves start at −2.8 log
//! H, the lowest exposure plotted, where the toe has very nearly reached D-min.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::kodak_e7022_2007_gold_100;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

pub const KODAK_GOLD_100: FilmStock = FilmStock {
    id: "kodak-gold-100",
    manufacturer: "Kodak",
    name: "Gold 100",
    iso: 100,
    kind: FilmKind::ColourNegative,
    sheet: &kodak_e7022_2007_gold_100::SHEET,
    provenance: "Kodak's own datasheet for Gold 100 (publication E-7022, 2007 edition, which \
                 covers Gold 100 and 200; the two share its spectral data). Discontinued \
                 film. The film base is measured on every scan.",
    model: &MODEL,
};
