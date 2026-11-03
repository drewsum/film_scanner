//! The film stocks with profiles. Each lives in its own module, which documents where its
//! data comes from; see `docs/film-profiles.md` for adding one.

mod cinestill;
mod flic_film_elektra_100;
mod fujifilm_200;
mod fujifilm_400;
mod harman_phoenix;
mod ilford_hp5_plus;
mod kodak_ektar_100;
mod kodak_gold_100;
mod kodak_gold_200;
mod kodak_kodacolor_200;
mod kodak_portra_400;
mod kodak_ultramax_400;
mod reto_amber_d100;

use crate::stock::{FilmStock, MonoStock};

pub use cinestill::{CINESTILL_50D, CINESTILL_400D, CINESTILL_800T};
pub use flic_film_elektra_100::FLIC_FILM_ELEKTRA_100;
pub use fujifilm_200::FUJIFILM_200;
pub use fujifilm_400::FUJIFILM_400;
pub use harman_phoenix::{HARMAN_PHOENIX_200, HARMAN_PHOENIX_II_200};
pub use ilford_hp5_plus::ILFORD_HP5_PLUS;
pub use kodak_ektar_100::KODAK_EKTAR_100;
pub use kodak_gold_100::KODAK_GOLD_100;
pub use kodak_gold_200::KODAK_GOLD_200;
pub use kodak_kodacolor_200::KODAK_KODACOLOR_200;
pub use kodak_portra_400::KODAK_PORTRA_400;
pub use kodak_ultramax_400::KODAK_ULTRAMAX_400;
pub use reto_amber_d100::RETO_AMBER_D100;

/// Offered in this order: the film in the scanner when this was written first, then by
/// maker and name.
pub(crate) const ALL: &[&FilmStock] = &[
    &KODAK_KODACOLOR_200,
    &CINESTILL_50D,
    &CINESTILL_400D,
    &CINESTILL_800T,
    &FLIC_FILM_ELEKTRA_100,
    &FUJIFILM_200,
    &FUJIFILM_400,
    &HARMAN_PHOENIX_200,
    &HARMAN_PHOENIX_II_200,
    &KODAK_EKTAR_100,
    &KODAK_GOLD_100,
    &KODAK_GOLD_200,
    &KODAK_PORTRA_400,
    &KODAK_ULTRAMAX_400,
    &RETO_AMBER_D100,
];

/// Black-and-white stocks, by maker and name.
pub(crate) const MONO: &[&MonoStock] = &[&ILFORD_HP5_PLUS];
