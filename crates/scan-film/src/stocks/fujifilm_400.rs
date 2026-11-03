//! Fujifilm 400: ISO 400 daylight color negative film for C-41, sold by Fujifilm since
//! 2023 in place of Superia X-TRA 400 (35 mm, made in the USA).
//!
//! # Where the data comes from
//!
//! Its own datasheet: FUJIFILM Ref. No. AF3-0262E, 1 April 2023, linked from Fujifilm's US
//! product page: characteristic curves (Status M, daylight, Process CN-16, Fujifilm's
//! equivalent of C-41), spectral sensitivities and spectral dye densities, read from the
//! PDF's vector paths.
//!
//! It is widely reported to be Kodak UltraMax 400 (by its DX code, and side-by-side
//! reviews that found the two nearly identical), but its published characteristic curves
//! are not UltraMax's. This profile uses Fujifilm's own.
//!
//! Its spectral sensitivity and dye density plots are the same drawings as on Fujifilm
//! 200's sheet, path for path: Fujifilm publishes one set of spectral data for both.
//!
//! Two things the sheet doesn't give, and how they're filled:
//!
//! - **Log H ref.** Fujifilm prints no grey-card exposure. −1.44 is assumed: where Kodak's
//!   ISO 400 sheets put it (log10(0.8 / 400) + 1.26). It puts a grey card at a red density
//!   of 0.96 here, just above the sheet's own exposure check (0.75 to 0.95).
//! - **Absolute spectral sensitivity.** Plotted relative, with a scale bar but no zero.
//!   Only the curves' shapes and offsets are used.
//!
//! # What is measured instead of assumed
//!
//! The film base (D-min) is measured on every scan; the sheet's D-min is only where the
//! curves start.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::fujifilm_af3_0262e_fujifilm_400;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

pub const FUJIFILM_400: FilmStock = FilmStock {
    id: "fujifilm-400",
    manufacturer: "Fujifilm",
    name: "400",
    iso: 400,
    kind: FilmKind::ColorNegative,
    sheet: &fujifilm_af3_0262e_fujifilm_400::SHEET,
    provenance: "Fujifilm's own datasheet for Fujifilm 400 (AF3-0262E, 2023). It prints no \
                 grey-card exposure, so Kodak's for an ISO 400 film is assumed. The film \
                 base is measured on every scan.",
    model: &MODEL,
};
