//! Kodak Professional Pro Image 100: ISO 100 daylight color negative film for C-41, made for
//! portrait and social photography in hot climates (it keeps at room temperature), and
//! printed like Kodak Gold.
//!
//! # Where the data comes from
//!
//! Its own datasheet: KODAK publication E-4L, July 1997, the film's launch edition.
//! Characteristic curves (Status M, daylight), spectral sensitivities and spectral dye
//! densities, all read from the PDF's vector paths. Kodak no longer hosts it and has
//! declined to publish a newer one; the copy used is an archive (issue #31).
//!
//! The sheet prints no Log H ref. −0.84 is assumed, where Kodak's ISO 100 sheets (Ektar 100,
//! Gold 100) put a grey card: log10(0.8 / 100) + 1.26. It agrees with the sheet's own
//! exposure check: the red curve reads 1.16 for a grey card (the check says 1.03 to 1.23)
//! and 1.53 for the lightest step of a paper grey scale (1.43 to 1.63).
//!
//! # Known differences to watch
//!
//! The curves are from 1997. Kodak has kept making the film since, and may have changed it
//! without a new datasheet; a roll from today could differ from these curves.
//!
//! # What is measured instead of assumed
//!
//! The film base (D-min) is measured on every scan; the sheet's D-min (0.25, 0.70, 0.96) is
//! only where the curves start.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::kodak_e4l_pro_image_100;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

pub const KODAK_PRO_IMAGE_100: FilmStock = FilmStock {
    id: "kodak-pro-image-100",
    manufacturer: "Kodak",
    name: "Pro Image 100",
    iso: 100,
    kind: FilmKind::ColorNegative,
    sheet: &kodak_e4l_pro_image_100::SHEET,
    provenance: "Kodak's own datasheet for Pro Image 100 (publication E-4L, 1997, from an \
                 archive; its Log H ref is assumed from the film's speed). The film base is \
                 measured on every scan.",
    model: &MODEL,
};
