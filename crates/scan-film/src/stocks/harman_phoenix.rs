//! Harman Phoenix 200 and Phoenix II 200: ISO 200 color negative films for C-41, Harman's
//! own emulsions (2023 and 2025), without an orange mask.
//!
//! # Where the data comes from
//!
//! **Nowhere specific: this is a generic profile.** Harman publishes no characteristic
//! curves for either film; the Phoenix II sheet (July 2025) has only a spectral wedge
//! spectrogram, printed as a picture. Both are profiled on a typical C-41 film averaged
//! from four Kodak datasheets (Gold 200, Ektar 100, Portra 400 and UltraMax 400; see
//! [`crate::sheets::generic_c41`]): one tone curve for every layer, placed for ISO 200,
//! and the sheets' mean spectral sensitivities.
//!
//! # Known differences to watch
//!
//! Phoenix is known for high contrast, strong grain and saturated, shifting colors,
//! Phoenix II for being more normal; neither looks like the average Kodak film. Expect to
//! set contrast and color by eye, more than with a film that has its own datasheet. If
//! Harman publishes curves, this should be replaced.
//!
//! # What is measured instead of assumed
//!
//! The film base is measured on every scan.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::generic_c41;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

const PROVENANCE: &str = "Generic: Harman publishes no curves for Phoenix, so its profile is \
                          a typical C-41 film averaged from four Kodak datasheets. Expect to \
                          adjust contrast and color by eye. The film base is measured on \
                          every scan.";

pub const HARMAN_PHOENIX_200: FilmStock = FilmStock {
    id: "harman-phoenix-200",
    manufacturer: "Harman",
    name: "Phoenix 200",
    iso: 200,
    kind: FilmKind::ColorNegative,
    sheet: &generic_c41::SHEET,
    provenance: PROVENANCE,
    model: &MODEL,
};

pub const HARMAN_PHOENIX_II_200: FilmStock = FilmStock {
    id: "harman-phoenix-ii-200",
    manufacturer: "Harman",
    name: "Phoenix II 200",
    iso: 200,
    kind: FilmKind::ColorNegative,
    sheet: &generic_c41::SHEET,
    provenance: PROVENANCE,
    model: &MODEL,
};
