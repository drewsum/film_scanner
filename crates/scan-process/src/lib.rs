//! Processing of scan results, independent of the scanner that made them.
//!
//! [`dust`] finds dust and scratches in a film scan's infrared channel, and [`inpaint`] fills
//! them in the visible image.

pub mod dust;
pub mod inpaint;

pub use dust::{DustError, DustParams, DustRemoval, DustReport, defect_mask, remove_dust};
