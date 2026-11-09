//! Writing scan results to disk.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageFormat};

use crate::{ScanError, ScanResult, ScanResultExt};

/// Read an image [`save_tiff`] wrote. Without the decoder's memory limit: a 16-bit scan of the
/// V500's whole film area at 1,600 dpi is about 390 MB, past the default 512 MB allowance
/// once decoding needs its own buffers.
pub fn open_scan(path: &Path) -> image::ImageResult<DynamicImage> {
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    reader.no_limits();
    reader.decode()
}

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
        write_tiff(&rotated(image, quarter_turns), &target)?;
        written.push(target);
    }
    if let Some(ir) = &result.infrared {
        let target = sibling("ir");
        let ir = DynamicImage::ImageLuma16(ir.clone());
        write_tiff(&rotated(&ir, quarter_turns), &target)?;
        written.push(target);
    }
    Ok(written)
}

/// Images at least this big are written as BigTIFF: classic TIFF's offsets are 32-bit, and
/// a 16-bit scan of the whole film area at 6,400 dpi is 6 GB.
const BIG_TIFF: usize = 0xf000_0000;

/// Write `image` to `path` as TIFF, as BigTIFF when it is too big for classic TIFF.
pub fn write_tiff(image: &DynamicImage, path: &Path) -> Result<(), ScanError> {
    write_tiff_from(image, path, BIG_TIFF)
}

/// [`write_tiff`], writing BigTIFF from `big` bytes on.
fn write_tiff_from(image: &DynamicImage, path: &Path, big: usize) -> Result<(), ScanError> {
    if image.as_bytes().len() < big {
        return Ok(image.save_with_format(path, ImageFormat::Tiff)?);
    }
    use tiff::encoder::{TiffEncoder, colortype};
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut tiff = TiffEncoder::new_big(file).backend_context("BigTIFF")?;
    let (w, h) = (image.width(), image.height());
    match image {
        DynamicImage::ImageRgb8(i) => tiff.write_image::<colortype::RGB8>(w, h, i),
        DynamicImage::ImageLuma8(i) => tiff.write_image::<colortype::Gray8>(w, h, i),
        DynamicImage::ImageLuma16(i) => tiff.write_image::<colortype::Gray16>(w, h, i),
        DynamicImage::ImageRgb16(i) => tiff.write_image::<colortype::RGB16>(w, h, i),
        other => tiff.write_image::<colortype::RGB16>(w, h, &other.to_rgb16()),
    }
    .backend_context(&path.display().to_string())
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

    #[test]
    fn writes_big_images_as_bigtiff() {
        let dir = tempfile::tempdir().unwrap();
        let rgb = DynamicImage::ImageRgb16(image::ImageBuffer::from_fn(5, 3, |x, y| {
            image::Rgb([x as u16 * 1000, y as u16 * 2000, 65535])
        }));
        let gray = DynamicImage::ImageLuma16(image::ImageBuffer::from_fn(4, 2, |x, y| {
            image::Luma([(x + 10 * y) as u16])
        }));
        for (name, image) in [("rgb.tif", rgb), ("gray.tif", gray)] {
            let path = dir.path().join(name);
            write_tiff_from(&image, &path, 0).unwrap();
            // BigTIFF's header: version 43 rather than 42.
            assert_eq!(std::fs::read(&path).unwrap()[2], 43);
            assert_eq!(open_scan(&path).unwrap(), image);
        }
    }
}
