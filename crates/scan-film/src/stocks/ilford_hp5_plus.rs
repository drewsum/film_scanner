//! Ilford HP5 Plus: ISO 400 black-and-white negative film.
//!
//! # Where the data comes from
//!
//! Ilford's technical information sheet, April 2010 edition: the characteristic curve for
//! 35 mm developed in ILFOTEC HC (1+31), 6 1/2 minutes at 20 °C, which Ilford says stands
//! for roll and sheet film too. Ilford's current edition (November 2018) prints the same
//! curve for the same development as a picture; this edition draws it as a vector path.
//!
//! Ilford plots relative log exposure and prints no grey-card exposure. Log H ref is taken
//! 1.01 above the speed point (0.1 above base plus fog), where ISO 6 puts a metered midtone;
//! Ilford rates HP5 Plus by practical evaluation rather than foot speed, so this is
//! approximate, and Auto printing sets the exposure from the frames anyway.
//!
//! # Known differences to watch
//!
//! The curve is for one developer and time. Pushed, or developed in something else, the
//! film has another curve: more or less contrast than this profile expects.
//!
//! # What is measured instead of assumed
//!
//! The film base (base plus fog) is measured on every scan.

use std::sync::OnceLock;

use crate::model::MonoModel;
use crate::sheets::ilford_hp5_plus_2010;
use crate::stock::MonoStock;

static MODEL: OnceLock<MonoModel> = OnceLock::new();

pub const ILFORD_HP5_PLUS: MonoStock = MonoStock {
    id: "ilford-hp5-plus",
    manufacturer: "Ilford",
    name: "HP5 Plus",
    iso: 400,
    sheet: &ilford_hp5_plus_2010::SHEET,
    provenance: "Ilford's own datasheet for HP5 Plus (2010), developed in ILFOTEC HC 1+31. \
                 Its grey-card exposure is estimated from the curve's speed point. The film \
                 base is measured on every scan.",
    model: &MODEL,
};
