//! Film stock profiles, built from the manufacturers' published data, and the inversion of
//! colour negatives with them (film_scanner's answer to SilverFast's NegaFix).
//!
//! - [`sheet`] holds a datasheet's curves as published, and [`sheets`] the datasheets,
//!   generated from their PDFs.
//! - [`stock`] is the films offered, each pointing at the data that describes it (see
//!   [`stocks`] for their provenance).
//! - [`model`] runs a film forwards and backwards through its characteristic curves, and
//!   fits its colour from its spectral sensitivities ([`spectral`] has the colour science).
//! - [`render`] turns a scan of a negative into a positive.
//!
//! How a profile is built, and how to add one, is in `docs/film-profiles.md`.
//!
//! This crate has no dependencies, so the mock scanner in `scan-core` can shoot its
//! negatives on a real film's curves.

pub mod model;
pub mod render;
pub mod sheet;
pub mod sheets;
pub mod spectral;
pub mod stock;
pub mod stocks;

pub use model::FilmModel;
pub use render::{Inverter, Printing, Rendering, Tone, auto_printing};
pub use stock::{FilmKind, FilmStock};

#[cfg(test)]
mod tests;
