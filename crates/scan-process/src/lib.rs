//! Processing of scan results, independent of the scanner that made them.
//!
//! [`hdr`] merges a scan's exposure passes, [`dust`] finds dust and scratches in a film
//! scan's infrared channel, and [`inpaint`] fills them in the visible image.

pub mod dust;
mod fit;
pub mod hdr;
pub mod inpaint;

pub use dust::{DustError, DustParams, DustRemoval, DustReport, defect_mask, remove_dust};
pub use hdr::{ExposureMerge, MergeError, PassReport, merge_exposures};
