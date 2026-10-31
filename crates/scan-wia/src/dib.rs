//! Decoder for in-memory device-independent bitmaps (`WiaImgFmt_MEMORYBMP`).
//!
//! A WIA memory bitmap is a `BITMAPINFOHEADER`, an optional palette and the pixel rows, with
//! no `BITMAPFILEHEADER`. Unlike BMP files it may carry 48-bit colour, and drivers sometimes
//! leave the height or image size as zero, so we decode it ourselves.

use image::{DynamicImage, ImageBuffer, Luma, Rgb};

use scan_core::ScanError;

const INFO_HEADER_LEN: usize = 40;
const BI_RGB: u32 = 0;

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn bad(msg: impl Into<String>) -> ScanError {
    ScanError::Backend(format!("invalid memory bitmap: {}", msg.into()))
}

/// Decode a memory bitmap of 8 or 16-bit grey, or 24 or 48-bit colour.
pub fn decode_memory_bmp(data: &[u8]) -> Result<DynamicImage, ScanError> {
    if data.len() < INFO_HEADER_LEN {
        return Err(bad("truncated header"));
    }
    let header_len = u32_at(data, 0) as usize;
    let width = u32_at(data, 4) as i32;
    let height = u32_at(data, 8) as i32;
    let bpp = u16_at(data, 14) as usize;
    let compression = u32_at(data, 16);
    let colors_used = u32_at(data, 32) as usize;
    if header_len < INFO_HEADER_LEN || width <= 0 {
        return Err(bad(format!("header size {header_len}, width {width}")));
    }
    if compression != BI_RGB {
        return Err(bad(format!("unsupported compression {compression}")));
    }
    let palette_len = if bpp <= 8 {
        if colors_used > 0 {
            colors_used
        } else {
            1 << bpp
        }
    } else {
        0
    };
    let pixels_at = header_len + palette_len * 4;
    if data.len() < pixels_at {
        return Err(bad("truncated palette"));
    }
    let palette = &data[header_len..pixels_at];
    let width = width as usize;
    let stride = (width * bpp).div_ceil(32) * 4;
    let available_rows = (data.len() - pixels_at) / stride;
    let rows = if height == 0 {
        available_rows
    } else {
        (height.unsigned_abs() as usize).min(available_rows)
    };
    if rows == 0 {
        return Err(bad("no pixel data"));
    }
    // Positive heights are stored bottom-up.
    let bottom_up = height > 0;
    let row = |y: usize| {
        let src = if bottom_up { rows - 1 - y } else { y };
        let start = pixels_at + src * stride;
        &data[start..start + stride]
    };
    let (w, h) = (width as u32, rows as u32);

    let image = match bpp {
        8 => {
            let mut out = Vec::with_capacity(width * rows);
            for y in 0..rows {
                for &index in &row(y)[..width] {
                    // Greyscale palettes have R = G = B; take green, or the index if missing.
                    let i = index as usize * 4 + 1;
                    out.push(palette.get(i).copied().unwrap_or(index));
                }
            }
            DynamicImage::ImageLuma8(ImageBuffer::<Luma<u8>, _>::from_raw(w, h, out).unwrap())
        }
        16 => {
            let mut out = Vec::with_capacity(width * rows);
            for y in 0..rows {
                out.extend(
                    row(y)[..width * 2]
                        .chunks_exact(2)
                        .map(|p| u16::from_le_bytes([p[0], p[1]])),
                );
            }
            DynamicImage::ImageLuma16(ImageBuffer::<Luma<u16>, _>::from_raw(w, h, out).unwrap())
        }
        24 => {
            let mut out = Vec::with_capacity(width * rows * 3);
            for y in 0..rows {
                for bgr in row(y)[..width * 3].chunks_exact(3) {
                    out.extend_from_slice(&[bgr[2], bgr[1], bgr[0]]);
                }
            }
            DynamicImage::ImageRgb8(ImageBuffer::<Rgb<u8>, _>::from_raw(w, h, out).unwrap())
        }
        48 => {
            let mut out = Vec::with_capacity(width * rows * 3);
            for y in 0..rows {
                for bgr in row(y)[..width * 6].chunks_exact(6) {
                    let c = |i: usize| u16::from_le_bytes([bgr[i], bgr[i + 1]]);
                    out.extend_from_slice(&[c(4), c(2), c(0)]);
                }
            }
            DynamicImage::ImageRgb16(ImageBuffer::<Rgb<u16>, _>::from_raw(w, h, out).unwrap())
        }
        other => return Err(bad(format!("unsupported bit depth {other}"))),
    };
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width: i32, height: i32, bpp: u16, colors: u32) -> Vec<u8> {
        let mut h = vec![0u8; INFO_HEADER_LEN];
        h[0..4].copy_from_slice(&40u32.to_le_bytes());
        h[4..8].copy_from_slice(&width.to_le_bytes());
        h[8..12].copy_from_slice(&height.to_le_bytes());
        h[12..14].copy_from_slice(&1u16.to_le_bytes());
        h[14..16].copy_from_slice(&bpp.to_le_bytes());
        h[32..36].copy_from_slice(&colors.to_le_bytes());
        h
    }

    #[test]
    fn decodes_bottom_up_24_bit_with_padding() {
        // 2x2 image, stride 8 (6 bytes + 2 padding). Stored bottom row first.
        let mut d = header(2, 2, 24, 0);
        d.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // bottom: red, green
        d.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]); // top: blue, white
        let img = decode_memory_bmp(&d).unwrap().to_rgb8();
        assert_eq!(img.get_pixel(0, 0).0, [0, 0, 255]);
        assert_eq!(img.get_pixel(1, 0).0, [255, 255, 255]);
        assert_eq!(img.get_pixel(0, 1).0, [255, 0, 0]);
        assert_eq!(img.get_pixel(1, 1).0, [0, 255, 0]);
    }

    #[test]
    fn decodes_top_down_48_bit() {
        let mut d = header(1, -1, 48, 0);
        d.extend_from_slice(&[0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0, 0]); // B=1, G=2, R=65535
        let img = decode_memory_bmp(&d).unwrap();
        assert!(matches!(img, DynamicImage::ImageRgb16(_)));
        assert_eq!(img.to_rgb16().get_pixel(0, 0).0, [65535, 2, 1]);
    }

    #[test]
    fn derives_height_when_driver_reports_zero() {
        let mut d = header(4, 0, 8, 256);
        for i in 0..=255u8 {
            d.extend_from_slice(&[i, i, i, 0]);
        }
        d.extend_from_slice(&[10, 20, 30, 40]);
        d.extend_from_slice(&[50, 60, 70, 80]);
        let img = decode_memory_bmp(&d).unwrap().to_luma8();
        assert_eq!(img.dimensions(), (4, 2));
        assert_eq!(img.into_raw(), [10, 20, 30, 40, 50, 60, 70, 80]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode_memory_bmp(&[0; 10]).is_err());
        let mut d = header(1, 1, 32, 0);
        d.extend_from_slice(&[0; 4]);
        assert!(decode_memory_bmp(&d).is_err());
    }
}
