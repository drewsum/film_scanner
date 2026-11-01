//! Writing scan results to disk.

use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageFormat};

use crate::{ScanError, ScanResult};

/// Save a scan as TIFF files, preserving bit depth.
///
/// The primary pass is written to `path` (with a `.tif` extension). Extra exposure passes
/// become `<stem>_ev+1.0.tif` etc., and an infrared channel becomes `<stem>_ir.tif`.
/// Returns every path written.
pub fn save_tiff(result: &ScanResult, path: &Path) -> Result<Vec<PathBuf>, ScanError> {
    save_tiff_with_primary(result, result.primary(), path)
}

/// Like [`save_tiff`], with `primary` (e.g. the dust-cleaned image) written in place of the
/// first pass. The other passes and the infrared channel are saved as scanned.
pub fn save_tiff_with_primary(
    result: &ScanResult,
    primary_image: &DynamicImage,
    path: &Path,
) -> Result<Vec<PathBuf>, ScanError> {
    let primary = path.with_extension("tif");
    let stem = primary
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("scan")
        .to_owned();
    let sibling = |suffix: &str| primary.with_file_name(format!("{stem}_{suffix}.tif"));

    let mut written = Vec::new();
    for (i, pass) in result.passes.iter().enumerate() {
        let (target, image) = if i == 0 {
            (primary.clone(), primary_image)
        } else {
            (sibling(&format!("ev{:+.1}", pass.exposure.ev)), &pass.image)
        };
        image.save_with_format(&target, ImageFormat::Tiff)?;
        written.push(target);
    }
    if let Some(ir) = &result.infrared {
        let target = sibling("ir");
        DynamicImage::ImageLuma16(ir.clone()).save_with_format(&target, ImageFormat::Tiff)?;
        written.push(target);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;
    use crate::{Exposure, NoProgress, ScanRequest, ScanSource, ScannerBackend};

    #[test]
    fn writes_16_bit_tiffs_for_every_pass() {
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let mut req = ScanRequest::new(ScanSource::Transparency, 75);
        req.infrared = true;
        req.exposures = vec![Exposure { ev: 0.0 }, Exposure { ev: 1.0 }];
        let result = dev.scan(&req, &mut NoProgress).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let written = save_tiff(&result, &dir.path().join("roll1.tiff")).unwrap();
        let names: Vec<_> = written
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["roll1.tif", "roll1_ev+1.0.tif", "roll1_ir.tif"]);

        let back = image::open(&written[0]).unwrap();
        assert!(matches!(back, DynamicImage::ImageRgb16(_)));
        assert_eq!((back.width(), back.height()), result.dimensions());
        let ir = image::open(&written[2]).unwrap();
        assert!(matches!(ir, DynamicImage::ImageLuma16(_)));
    }

    #[test]
    fn a_replacement_primary_is_written_first() {
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let mut req = ScanRequest::new(ScanSource::Transparency, 75);
        req.infrared = true;
        let result = dev.scan(&req, &mut NoProgress).unwrap();
        let (w, h) = result.dimensions();
        let cleaned = DynamicImage::new_rgb16(w, h);

        let dir = tempfile::tempdir().unwrap();
        let written =
            save_tiff_with_primary(&result, &cleaned, &dir.path().join("clean.tif")).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(image::open(&written[0]).unwrap(), cleaned);
        assert!(written[1].ends_with("clean_ir.tif"));
    }
}
