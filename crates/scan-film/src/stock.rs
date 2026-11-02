//! Film stocks: a film as sold, and the published data its profile is built from.
//!
//! Adding a stock is described in `docs/film-profiles.md`.

use std::sync::OnceLock;

use crate::model::FilmModel;
use crate::sheet::Datasheet;
use crate::stocks;

/// What kind of film a stock is, which decides how it is inverted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilmKind {
    /// Color negative film for process C-41, with an orange mask.
    ColorNegative,
}

/// A film stock the inversion has a profile for.
#[derive(Debug)]
pub struct FilmStock {
    /// Stable identifier, kept in settings: never change it once released.
    pub id: &'static str,
    pub manufacturer: &'static str,
    /// The film's name as sold, without the manufacturer.
    pub name: &'static str,
    pub iso: u32,
    pub kind: FilmKind,
    /// The datasheet whose curves describe this film. Not always the film's own: see
    /// `provenance`.
    pub sheet: &'static Datasheet,
    /// Where the profile's data comes from and what it assumes, for the user.
    pub provenance: &'static str,
    /// The film's model, built on first use.
    pub(crate) model: &'static OnceLock<FilmModel>,
}

impl FilmStock {
    /// "Kodak Kodacolor 200".
    pub fn display_name(&self) -> String {
        format!("{} {}", self.manufacturer, self.name)
    }

    pub fn model(&self) -> &FilmModel {
        self.model.get_or_init(|| FilmModel::new(self.sheet))
    }
}

/// Stocks are the same stock when their ids are.
impl PartialEq for FilmStock {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for FilmStock {}

/// Every stock with a profile, in the order offered.
pub fn all() -> &'static [&'static FilmStock] {
    stocks::ALL
}

pub fn by_id(id: &str) -> Option<&'static FilmStock> {
    all().iter().copied().find(|s| s.id == id)
}

/// The stock offered first.
pub fn default_stock() -> &'static FilmStock {
    all()[0]
}
