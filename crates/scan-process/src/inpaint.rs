//! Filling masked pixels from their surroundings.
//!
//! The fill works inwards from the edge of each masked area, one ring of pixels at a time,
//! each pixel taking the mean of its already-known neighbours. A few relaxation passes then
//! smooth out the rings. This suits dust specks and thin scratches, a few pixels across;
//! larger areas come out smooth, without the film's grain.

use scan_core::image::{DynamicImage, GenericImageView, GrayImage};

/// Relaxation passes after the fill.
const SMOOTHING_PASSES: usize = 8;
const DIAGONAL: f32 = std::f32::consts::FRAC_1_SQRT_2;
/// The 8 neighbours of a pixel and their weights.
const NEIGHBOURS: [(isize, isize, f32); 8] = [
    (-1, 0, 1.0),
    (1, 0, 1.0),
    (0, -1, 1.0),
    (0, 1, 1.0),
    (-1, -1, DIAGONAL),
    (1, -1, DIAGONAL),
    (-1, 1, DIAGONAL),
    (1, 1, DIAGONAL),
];

/// A copy of `image` with every pixel where `mask` is non-zero filled in from the unmasked
/// pixels around it. The image keeps its format. If the sizes differ, the mask is ignored.
pub fn inpaint(image: &DynamicImage, mask: &GrayImage) -> DynamicImage {
    let mut out = image.clone();
    if image.dimensions() != mask.dimensions() {
        return out;
    }
    let (w, h) = (image.width() as usize, image.height() as usize);
    let channels = image.color().channel_count() as usize;
    let mask = mask.as_raw();
    match &mut out {
        DynamicImage::ImageLuma8(b) => fill(b, channels, w, h, mask),
        DynamicImage::ImageLumaA8(b) => fill(b, channels, w, h, mask),
        DynamicImage::ImageRgb8(b) => fill(b, channels, w, h, mask),
        DynamicImage::ImageRgba8(b) => fill(b, channels, w, h, mask),
        DynamicImage::ImageLuma16(b) => fill(b, channels, w, h, mask),
        DynamicImage::ImageLumaA16(b) => fill(b, channels, w, h, mask),
        DynamicImage::ImageRgb16(b) => fill(b, channels, w, h, mask),
        DynamicImage::ImageRgba16(b) => fill(b, channels, w, h, mask),
        other => {
            let mut rgb = other.to_rgb16();
            fill(&mut rgb, 3, w, h, mask);
            *other = DynamicImage::ImageRgb16(rgb);
        }
    }
    out
}

trait Sample: Copy {
    fn to_f32(self) -> f32;
    fn from_f32(v: f32) -> Self;
}

impl Sample for u8 {
    fn to_f32(self) -> f32 {
        self as f32
    }
    fn from_f32(v: f32) -> Self {
        v.round().clamp(0.0, u8::MAX as f32) as u8
    }
}

impl Sample for u16 {
    fn to_f32(self) -> f32 {
        self as f32
    }
    fn from_f32(v: f32) -> Self {
        v.round().clamp(0.0, u16::MAX as f32) as u16
    }
}

/// Fill the masked pixels of interleaved `data`, `channels` samples per pixel.
fn fill<T: Sample>(data: &mut [T], channels: usize, w: usize, h: usize, mask: &[u8]) {
    let holes: Vec<usize> = (0..w * h).filter(|&i| mask[i] != 0).collect();
    if holes.is_empty() || holes.len() == w * h {
        return;
    }
    let neighbours = |i: usize| {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        NEIGHBOURS.iter().filter_map(move |&(dx, dy, weight)| {
            let (nx, ny) = (x + dx, y + dy);
            let inside = (0..w as isize).contains(&nx) && (0..h as isize).contains(&ny);
            inside.then(|| (ny as usize * w + nx as usize, weight))
        })
    };
    let mean = |data: &[T], i: usize, known: &dyn Fn(usize) -> bool, out: &mut Vec<f32>| {
        let start = out.len();
        out.resize(start + channels, 0.0);
        let mut total = 0.0;
        for (j, weight) in neighbours(i).filter(|&(j, _)| known(j)) {
            for c in 0..channels {
                out[start + c] += weight * data[j * channels + c].to_f32();
            }
            total += weight;
        }
        for v in &mut out[start..] {
            *v /= total;
        }
    };

    let mut known: Vec<bool> = mask.iter().map(|&m| m == 0).collect();
    // Pixels that are known or waiting in the current ring.
    let mut reached = known.clone();
    let mut ring: Vec<usize> = holes
        .iter()
        .copied()
        .filter(|&i| neighbours(i).any(|(j, _)| known[j]))
        .collect();
    for &i in &ring {
        reached[i] = true;
    }
    let mut values = Vec::new();
    while !ring.is_empty() {
        values.clear();
        for &i in &ring {
            mean(data, i, &|j| known[j], &mut values);
        }
        for (k, &i) in ring.iter().enumerate() {
            for c in 0..channels {
                data[i * channels + c] = T::from_f32(values[k * channels + c]);
            }
            known[i] = true;
        }
        let mut next = Vec::new();
        for &i in &ring {
            for (j, _) in neighbours(i) {
                if !reached[j] {
                    reached[j] = true;
                    next.push(j);
                }
            }
        }
        ring = next;
    }

    for _ in 0..SMOOTHING_PASSES {
        for &i in &holes {
            values.clear();
            mean(data, i, &|_| true, &mut values);
            for c in 0..channels {
                data[i * channels + c] = T::from_f32(values[c]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan_core::image::{ImageBuffer, Luma, Rgb};

    fn blob_mask(w: u32, h: u32, cx: u32, cy: u32, r: u32) -> GrayImage {
        ImageBuffer::from_fn(w, h, |x, y| {
            let d2 = x.abs_diff(cx).pow(2) + y.abs_diff(cy).pow(2);
            Luma([if d2 <= r * r { 255 } else { 0 }])
        })
    }

    #[test]
    fn fills_a_hole_in_a_gradient() {
        let (w, h) = (60, 40);
        let gradient = |x: u32, y: u32| Rgb([(x * 1000) as u16, (y * 1500) as u16, 30000]);
        let mut img = ImageBuffer::from_fn(w, h, gradient);
        let mask = blob_mask(w, h, 30, 20, 5);
        for (x, y, m) in mask.enumerate_pixels() {
            if m.0[0] != 0 {
                img.put_pixel(x, y, Rgb([0, 65535, 0]));
            }
        }
        let out = inpaint(&DynamicImage::ImageRgb16(img), &mask).to_rgb16();
        for (x, y, p) in out.enumerate_pixels() {
            let expected = gradient(x, y).0;
            for c in 0..3 {
                let err = (p.0[c] as f32 - expected[c] as f32).abs();
                assert!(
                    err < 2000.0,
                    "{x},{y} channel {c}: {:?} vs {expected:?}",
                    p.0
                );
            }
        }
    }

    #[test]
    fn keeps_the_format_and_unmasked_pixels() {
        let img = ImageBuffer::from_fn(20, 20, |x, y| Luma([(x * 10 + y) as u8]));
        let mask = blob_mask(20, 20, 0, 0, 3);
        let out = inpaint(&DynamicImage::ImageLuma8(img.clone()), &mask);
        let DynamicImage::ImageLuma8(out) = out else {
            panic!("format changed");
        };
        for (x, y, m) in mask.enumerate_pixels() {
            if m.0[0] == 0 {
                assert_eq!(out.get_pixel(x, y), img.get_pixel(x, y));
            }
        }
    }

    #[test]
    fn a_fully_masked_or_mismatched_mask_changes_nothing() {
        let img = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(8, 8, Rgb([1, 2, 3])));
        let full = GrayImage::from_pixel(8, 8, Luma([255]));
        assert_eq!(inpaint(&img, &full), img);
        let other = GrayImage::from_pixel(4, 8, Luma([255]));
        assert_eq!(inpaint(&img, &other), img);
    }
}
