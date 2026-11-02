//! Kodak Kodacolor 200: ISO 200 daylight color negative film for C-41, sold by Eastman
//! Kodak since October 2025 (35 mm; edge print `KODAK 200-8`).
//!
//! # Where the data comes from
//!
//! Kodak publishes no datasheet for Kodacolor 200. It is widely reported (by reviewers who
//! shot both, and from its packaging and edge print) to be **Kodak ColorPlus 200** under a
//! new name, and Kodak has never published a datasheet for ColorPlus either; Kodak Alaris
//! reportedly said none exists. Kodak's own product page says only "medium-speed daylight
//! balanced color negative film" with "fine grain" and "saturated colors".
//!
//! The nearest film Kodak does document is **KODAK GOLD 200**, publication E-7022 (2016):
//! the same maker's 200-speed daylight consumer C-41 film, made for the same Kodak papers.
//! Its curves are taken as Kodacolor 200's:
//!
//! - **Characteristic curves** (Status M, daylight, Log H ref −1.14): each layer's tone
//!   response, which the inversion runs backwards. They agree with the sheet's own check of
//!   exposure: a grey card at Log H ref reads 0.95 in red (Kodak: 0.85 to 1.05), the lightest
//!   step of a paper grey scale 1.31 (Kodak: 1.25 to 1.45).
//! - **Spectral sensitivities**: what colors each layer records; the colorimetric
//!   rendering is fitted from them.
//! - **Spectral dye densities** of D-min and a midscale neutral: kept for scanner
//!   calibration (not used by the inversion yet).
//!
//! # What is measured instead of assumed
//!
//! The **film base** (D-min: base, fog and the orange mask) differs from roll to roll with
//! processing, and is never taken from the sheet: it is measured on every scan, from the
//! unexposed film between the frames and the perforations (see `scan_process::frames`).
//! The sheet's D-min (0.25, 0.66, 1.00 in red, green, blue) is only where the curves start.
//!
//! # Known differences to watch
//!
//! Reviewers who shot Kodacolor 200 against ColorPlus found them nearly identical, with
//! Kodacolor perhaps a little cooler, and describe its warmth as "like Gold 200, but not so
//! much". So against these Gold 200 curves, real Kodacolor may print slightly cooler: a
//! difference of film, not of the model, and what the color balance is for.
//!
//! # Measured on a real roll
//!
//! A roll of 12 frames (mostly indoors, two in daylight), scanned on an Epson V500 at 400
//! dpi, 16-bit, November 2026, measured with `scan-process`'s `negative_report` example:
//!
//! - **Film base** density 0.264 / 0.461 / 0.976 against the sheet's D-min of 0.251 / 0.657
//!   / 0.991. Red and blue agree to 0.015. Green reads 0.2 lower: the scanner's green
//!   channel sees the orange mask, whose density falls steeply across green, differently
//!   from Status M. The base is divided out, so this doesn't shift colors by itself.
//! - **The roll is underexposed**, not read low by the scanner: the thinnest 1% of nearly
//!   every frame is at the bare base (unexposed shadows), frame medians are 0.13–0.42 above
//!   base where a normally exposed grey card is 0.67–0.75, and the two daylight frames are
//!   the densest. Auto exposure gives +2.6 EV.
//! - **Neutral balance** from the profile alone is close: Auto's roll correction is red
//!   +0.08, green +0.08, blue −0.16 EV. Frames' color offsets drift by about 0.1 stop with
//!   their level across the roll (blue up in the darkest), within what the scenes'
//!   own color explains; no scanner correction is applied without a target to measure it.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::kodak_e7022;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

pub const KODAK_KODACOLOR_200: FilmStock = FilmStock {
    id: "kodak-kodacolor-200",
    manufacturer: "Kodak",
    name: "Kodacolor 200",
    iso: 200,
    kind: FilmKind::ColorNegative,
    sheet: &kodak_e7022::SHEET,
    provenance: "Kodak publishes no datasheet for Kodacolor 200, which is reported to be \
                 ColorPlus 200 renamed (no datasheet either). Its profile uses the curves \
                 of Kodak Gold 200 (publication E-7022), the same maker's 200-speed consumer \
                 film. The film base is measured on every scan.",
    model: &MODEL,
};
