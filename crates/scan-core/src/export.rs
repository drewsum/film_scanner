//! Writing scan results to disk.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageFormat};

use crate::{ScanError, ScanResult};

/// Save a scan as TIFF files, preserving bit depth.
///
/// The primary pass is written to `path` (with a `.tif` extension). Extra exposure passes
/// become `<stem>_ev+1.0.tif` etc., and an infrared channel becomes `<stem>_ir.tif`.
/// Returns every path written.
pub fn save_tiff(result: &ScanResult, path: &Path) -> Result<Vec<PathBuf>, ScanError> {
    save_tiff_with_primary(result, result.primary(), 0, path)
}

/// Like [`save_tiff`], with `primary` (e.g. the dust-cleaned image) written in place of the
/// first pass, and every image turned `quarter_turns` clockwise. The other passes and the
/// infrared channel are saved as scanned.
pub fn save_tiff_with_primary(
    result: &ScanResult,
    primary_image: &DynamicImage,
    quarter_turns: u8,
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
        rotated(image, quarter_turns).save_with_format(&target, ImageFormat::Tiff)?;
        written.push(target);
    }
    if let Some(ir) = &result.infrared {
        let target = sibling("ir");
        let ir = DynamicImage::ImageLuma16(ir.clone());
        rotated(&ir, quarter_turns).save_with_format(&target, ImageFormat::Tiff)?;
        written.push(target);
    }
    Ok(written)
}

/// `image` turned `quarter_turns` clockwise.
pub fn rotated(image: &DynamicImage, quarter_turns: u8) -> Cow<'_, DynamicImage> {
    match quarter_turns % 4 {
        0 => Cow::Borrowed(image),
        1 => Cow::Owned(image.rotate90()),
        2 => Cow::Owned(image.rotate180()),
        _ => Cow::Owned(image.rotate270()),
    }
}

/// `path` with `_NN` (from 01) added to its name, and a `.tif` extension: for the `n`th of
/// several images saved together.
pub fn numbered(path: &Path, n: usize) -> PathBuf {
    let path = path.with_extension("tif");
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("frame")
        .to_owned();
    path.with_file_name(format!("{stem}_{:02}.tif", n + 1))
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
            save_tiff_with_primary(&result, &cleaned, 0, &dir.path().join("clean.tif")).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(image::open(&written[0]).unwrap(), cleaned);
        assert!(written[1].ends_with("clean_ir.tif"));
    }

    #[test]
    fn saved_images_are_turned() {
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let mut req = ScanRequest::new(ScanSource::Transparency, 75);
        req.infrared = true;
        let result = dev.scan(&req, &mut NoProgress).unwrap();
        let (w, h) = result.dimensions();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turned.tif");
        let written = save_tiff_with_primary(&result, result.primary(), 1, &path).unwrap();
        for file in &written {
            let back = image::open(file).unwrap();
            assert_eq!((back.width(), back.height()), (h, w));
        }
        // A quarter clockwise: the top-left pixel ends up top-right.
        let back = image::open(&written[0]).unwrap().to_rgb16();
        let original = result.primary().to_rgb16();
        assert_eq!(back.get_pixel(h - 1, 0), original.get_pixel(0, 0));
        assert_eq!(back.get_pixel(0, 0), original.get_pixel(0, h - 1));
    }

    #[test]
    fn numbered_paths() {
        let path = Path::new("dir/roll 7.tiff");
        assert_eq!(numbered(path, 0), Path::new("dir/roll 7_01.tif"));
        assert_eq!(numbered(path, 11), Path::new("dir/roll 7_12.tif"));
    }
}
