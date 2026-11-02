//! The film stocks with profiles. Each lives in its own module, which documents where its
//! data comes from; see `docs/film-profiles.md` for adding one.

mod kodak_gold_100;
mod kodak_gold_200;
mod kodak_kodacolor_200;

use crate::stock::FilmStock;

pub use kodak_gold_100::KODAK_GOLD_100;
pub use kodak_gold_200::KODAK_GOLD_200;
pub use kodak_kodacolor_200::KODAK_KODACOLOR_200;

/// Offered in this order: the film in the scanner when this was written first, then by
/// maker and name.
pub(crate) const ALL: &[&FilmStock] = &[&KODAK_KODACOLOR_200, &KODAK_GOLD_100, &KODAK_GOLD_200];
