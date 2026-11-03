//! Flic Film Elektra 100: ISO 100 color negative film for C-41, respooled from Kodak's
//! aerial film.
//!
//! # Where the data comes from
//!
//! Flic Film publishes no curves. Elektra 100 is widely reported to be KODAK AEROCOLOR IV
//! Negative Film 2460 (also sold as Santa Color 100 and Luminar 100), and is profiled on
//! Kodak's datasheet for it, publication AS-2652 (October 2009), now only on the Internet
//! Archive. The sheet's curves for its standard C-41 development (3:15, "developed for low
//! contrast", gamma 0.63), Status M, daylight; its spectral sensitivities (at 0.3 above
//! D-min) and dye densities.
//!
//! Aerial film speeds aren't pictorial ones, and the sheet gives no Log H ref: −0.84 is
//! assumed, where Kodak's ISO 100 still films put a grey card.
//!
//! # Known differences to watch
//!
//! - **No orange mask.** The sheet says so: D-min is nearly neutral, and its colors are
//!   less corrected than a masked film's. Inverting against the measured base works the
//!   same way.
//! - **Speed.** Kodak gives this development an aerial film speed of 80; the box says 100.
//!
//! # What is measured instead of assumed
//!
//! The film base (D-min) is measured on every scan; the sheet's D-min is only where the
//! curves start.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheets::kodak_as2652_aerocolor_iv_2460;
use crate::stock::{FilmKind, FilmStock};

static MODEL: OnceLock<FilmModel> = OnceLock::new();

pub const FLIC_FILM_ELEKTRA_100: FilmStock = FilmStock {
    id: "flic-film-elektra-100",
    manufacturer: "Flic Film",
    name: "Elektra 100",
    iso: 100,
    kind: FilmKind::ColorNegative,
    sheet: &kodak_as2652_aerocolor_iv_2460::SHEET,
    provenance: "Flic Film publishes no curves; Elektra 100 is reported to be Kodak \
                 AEROCOLOR IV 2460 aerial film, so its profile uses Kodak's datasheet for it \
                 (AS-2652), at its standard C-41 development. It has no orange mask. The \
                 film base is measured on every scan.",
    model: &MODEL,
};
