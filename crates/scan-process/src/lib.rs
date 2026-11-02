//! Processing of scan results, independent of the scanner that made them.
//!
//! [`hdr`] merges a scan's exposure passes, [`dust`] finds dust and scratches in a film
//! scan's infrared channel, [`inpaint`] fills them in the visible image, and [`frames`]
//! finds the frames on film strips and cuts them out.

pub mod dust;
mod fit;
pub mod frames;
pub mod hdr;
pub mod inpaint;

pub use dust::{DustError, DustParams, DustRemoval, DustReport, defect_mask, remove_dust};
pub use frames::{
    DetectParams, DetectedFrame, FilmFormat, FrameDetection, FrameRect, crop_frame, detect_frames,
};
pub use hdr::{ExposureMerge, MergeError, PassReport, merge_exposures};
