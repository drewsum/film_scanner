//! Fujifilm 200: ISO 200 daylight color negative film for C-41, sold by Fujifilm since
//! 2022 (35 mm, made in the USA).
//!
//! # Where the data comes from
//!
//! Its own datasheet: FUJIFILM Ref. No. AF3-0261E, 9 February 2022: characteristic curves
//! (Status M, daylight, Process CN-16, Fujifilm's equivalent of C-41), spectral
//! sensitivities and spectral dye densities, read from the PDF's vector paths. The sheet
//! says its photographic characteristics are "almost the same as those for the current
//! C200".
//!
//! It is widely reported to be made by Kodak, and reviewers comparing its datasheet with
//! Kodak Gold 200's found the spectral sensitivities remarkably alike. This profile uses
//! Fujifilm's own published curves, not Gold 200's.
//!
//! Two things the sheet doesn't give, and how they're filled:
//!
//! - **Log H ref.** Fujifilm prints no grey-card exposure. −1.14 is assumed: where Kodak's
//!   ISO 200 sheets put it (log10(0.8 / 200) + 1.26). It puts a grey card at a red density
//!   of 0.92 here, as Gold 200's curves do at its own Log H ref.
//! - **Absolute spectral sensitivity.** Fujifilm plots it relative, with a scale bar but no
//!   zero. Only the curves' shapes and offsets are used: each layer is normalised to
//!   daylight on white when the colorimetric matrix is fitted.
//!
//! # What is measured instead of assumed
//!
//! The film base (D-min) is measured on every scan; the sheet's D-min is only where the
//! curves start.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::fujifilm_af3_0261e_fujifilm_200;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

pub const FUJIFILM_200: FilmStock = FilmStock {
    id: "fujifilm-200",
    manufacturer: "Fujifilm",
    name: "200",
    iso: 200,
    kind: FilmKind::ColorNegative,
    sheet: &fujifilm_af3_0261e_fujifilm_200::SHEET,
    provenance: "Fujifilm's own datasheet for Fujifilm 200 (AF3-0261E, 2022). It prints no \
                 grey-card exposure, so Kodak's for an ISO 200 film is assumed. The film \
                 base is measured on every scan.",
    model: &MODEL,
};
