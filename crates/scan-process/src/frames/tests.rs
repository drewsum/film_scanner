use super::*;
use scan_core::mock::{self, MockBackend};
use scan_core::{NoProgress, ScanRequest, ScanSource, ScannerBackend};

const GAMMA: f32 = 2.2;
const DPI: u32 = 100;
const BASE: [f32; 3] = [0.85, 0.55, 0.33];
const HOLDER: f32 = 0.002;
/// Two strips don't fit across the V500's film area, so its holder covers the film's edges:
/// this much is visible either side of the middle.
const VISIBLE: f32 = 16.0;
/// The V500's film area.
const V500: ScanArea = ScanArea {
    x_mm: 0.0,
    y_mm: 0.0,
    width_mm: 68.58,
    height_mm: 236.98,
};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Shot {
    Normal,
    /// A night shot: bare film but for lights in the middle, so its edges can't be seen.
    Night,
    /// Unexposed film.
    Blank,
}

struct Strip {
    /// The middle of the film's start end, in mm on the bed.
    start: (f32, f32),
    /// The frames' angle: clockwise from straight down (or across) the scan.
    angle_deg: f32,
    vertical: bool,
    length: f32,
    /// How much of the film's width the holder leaves visible, either side of its middle.
    visible: f32,
    /// How far the image reaches either side of the middle.
    image: f32,
    /// Where each frame starts along the strip, and what is on it.
    frames: Vec<(f32, Shot)>,
}

impl Strip {
    fn new(start: (f32, f32), frames: &[(f32, Shot)]) -> Self {
        let end = frames.last().map_or(0.0, |f| f.0) + 36.0 + 1.5;
        Self {
            start,
            angle_deg: 0.0,
            vertical: true,
            length: end,
            visible: VISIBLE,
            image: 12.0,
            frames: frames.to_vec(),
        }
    }

    fn evenly(start: (f32, f32), shots: &[Shot]) -> Self {
        let frames: Vec<_> = shots
            .iter()
            .enumerate()
            .map(|(i, &s)| (1.5 + i as f32 * 38.0, s))
            .collect();
        Self::new(start, &frames)
    }

    /// Unit vectors along and across.
    fn axes(&self) -> ([f32; 2], [f32; 2]) {
        let (s, c) = self.angle_deg.to_radians().sin_cos();
        if self.vertical {
            ([-s, c], [c, s])
        } else {
            ([c, s], [-s, c])
        }
    }

    /// Where (`v` along, `u` across) a point is on the strip.
    fn local(&self, x: f32, y: f32) -> (f32, f32) {
        let (a, n) = self.axes();
        let (dx, dy) = (x - self.start.0, y - self.start.1);
        (dx * a[0] + dy * a[1], dx * n[0] + dy * n[1])
    }

    /// The frames as they should be found, trimmed as the detection does, and kept off the
    /// holder.
    fn truth(&self) -> Vec<(FrameRect, Shot)> {
        let (a, _) = self.axes();
        let inset = DetectParams::default().inset_mm;
        let on_film = self.visible - HOLDER_MARGIN_MM;
        let across = 2.0 * (self.image.min(on_film) - inset);
        let along = 36.0 - 2.0 * inset;
        self.frames
            .iter()
            .filter(|f| f.1 != Shot::Blank)
            .map(|&(v, shot)| {
                let c = v + 18.0;
                let (w, h) = if self.vertical {
                    (across, along)
                } else {
                    (along, across)
                };
                let rect = FrameRect {
                    cx_mm: self.start.0 + a[0] * c,
                    cy_mm: self.start.1 + a[1] * c,
                    width_mm: w,
                    height_mm: h,
                    angle_deg: self.angle_deg,
                };
                (rect, shot)
            })
            .collect()
    }

    /// Light through the strip at (x, y), if the point is on it.
    fn light(&self, x: f32, y: f32) -> Option<[f32; 3]> {
        let (v, u) = self.local(x, y);
        if !(0.0..self.length).contains(&v) || u.abs() > self.visible {
            return None;
        }
        if (13.5..15.5).contains(&u.abs()) && v.rem_euclid(4.75) < 2.8 {
            return Some([1.0; 3]);
        }
        for &(start, shot) in &self.frames {
            let fv = v - start;
            if u.abs() <= self.image && (0.0..36.0).contains(&fv) {
                let t: [f32; 3] = match shot {
                    Shot::Normal => std::array::from_fn(|c| {
                        let ph = c as f32 * 1.7 + start;
                        0.1 + 0.75 * (0.5 + 0.5 * (u * 0.31 + ph).sin() * (fv * 0.23).cos())
                    }),
                    Shot::Night => {
                        let r2 = u * u + (fv - 18.0).powi(2);
                        [1.0 - 0.6 * (-r2 / 50.0).exp(); 3]
                    }
                    Shot::Blank => [1.0; 3],
                };
                return Some(std::array::from_fn(|c| BASE[c] * t[c]));
            }
        }
        Some(BASE)
    }
}

/// A 16-bit scan of `strips` on `area` at `dpi`, with a little noise.
fn scan(area: ScanArea, dpi: u32, strips: &[Strip]) -> DynamicImage {
    let (w, h) = area.pixel_size(dpi);
    let mm = MM_PER_INCH / dpi as f32;
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for py in 0..h {
        for px in 0..w {
            let (x, y) = (
                area.x_mm + (px as f32 + 0.5) * mm,
                area.y_mm + (py as f32 + 0.5) * mm,
            );
            let light = strips
                .iter()
                .find_map(|s| s.light(x, y))
                .unwrap_or([HOLDER; 3]);
            for (c, l) in light.into_iter().enumerate() {
                let hash = (px.wrapping_mul(73_856_093) ^ py.wrapping_mul(19_349_663))
                    .wrapping_add(c as u32 * 83_492_791);
                let noise = 1.0 + 0.03 * ((hash % 1000) as f32 / 1000.0 - 0.5);
                let v = (l * noise).clamp(0.0, 1.0).powf(1.0 / GAMMA);
                data.push((v * 65535.0).round() as u16);
            }
        }
    }
    DynamicImage::ImageRgb16(ImageBuffer::from_raw(w, h, data).unwrap())
}

fn detect(area: ScanArea, strips: &[Strip]) -> FrameDetection {
    let image = scan(area, DPI, strips);
    detect_frames(&image, GAMMA, DPI, area, &DetectParams::default())
}

/// Check every expected frame was found, in order, to within `tolerance` mm.
fn check(found: &FrameDetection, strips: &[Strip], tolerance: f32) {
    let expected: Vec<_> = strips.iter().flat_map(Strip::truth).collect();
    assert_eq!(found.frames.len(), expected.len(), "{found:#?}");
    for (i, (f, (e, _))) in found.frames.iter().zip(&expected).enumerate() {
        let r = f.rect;
        let off = [
            r.cx_mm - e.cx_mm,
            r.cy_mm - e.cy_mm,
            r.width_mm - e.width_mm,
            r.height_mm - e.height_mm,
        ];
        assert!(
            off.iter().all(|d| d.abs() <= tolerance),
            "frame {i}: found {r:?}, expected {e:?}"
        );
        assert!(
            (r.angle_deg - e.angle_deg).abs() < 0.1,
            "frame {i}: angle {} expected {}",
            r.angle_deg,
            e.angle_deg
        );
    }
}

fn six() -> [Shot; 6] {
    [Shot::Normal; 6]
}

#[test]
fn twelve_frames_on_two_strips() {
    let strips = [
        Strip::evenly((17.0, 2.0), &six()),
        Strip::evenly((51.5, 2.0), &six()),
    ];
    let found = detect(V500, &strips);
    assert_eq!(found.strips, 2);
    check(&found, &strips, 0.35);
    assert!(
        found
            .frames
            .iter()
            .all(|f| f.edges_seen == 4 && !f.placed_by_spacing),
        "{found:#?}"
    );
    // The bare film between the frames and perforations, measured for inverting.
    let base = found.base.unwrap();
    for c in 0..3 {
        assert!((base[c] / BASE[c] - 1.0).abs() < 0.02, "{base:?}");
    }
}

#[test]
fn skewed_strips_are_straightened() {
    // Narrower windows than the V500's leave room to skew.
    let mut strips = [
        Strip::evenly((19.0, 2.0), &six()),
        Strip::evenly((51.0, 2.0), &six()),
    ];
    strips[0].angle_deg = 0.8;
    strips[1].angle_deg = -0.5;
    for s in &mut strips {
        s.visible = 15.0;
    }
    check(&detect(V500, &strips), &strips, 0.35);
}

#[test]
fn strips_across_the_scan() {
    let area = ScanArea::full(V500.height_mm, V500.width_mm);
    let mut strips = [
        Strip::evenly((2.0, 17.0), &six()),
        Strip::evenly((3.0, 51.5), &six()),
    ];
    for s in &mut strips {
        s.vertical = false;
    }
    strips[1].angle_deg = 0.5;
    check(&detect(area, &strips), &strips, 0.35);
}

#[test]
fn short_strips_and_blank_film() {
    use Shot::{Blank, Normal};
    // Four frames, and six with the last two unexposed.
    let strips = [
        Strip::evenly((17.0, 2.0), &[Normal; 4]),
        Strip::evenly((51.5, 2.0), &[Normal, Normal, Normal, Normal, Blank, Blank]),
    ];
    check(&detect(V500, &strips), &strips, 0.35);

    // A single frame, and a single strip.
    let strips = [
        Strip::evenly((17.0, 60.0), &[Normal]),
        Strip::evenly((51.5, 2.0), &[Normal; 3]),
    ];
    check(&detect(V500, &strips), &strips, 0.35);
    let strips = [Strip::evenly((34.0, 5.0), &six())];
    check(&detect(V500, &strips), &strips, 0.35);
}

#[test]
fn dark_frames_are_placed_by_the_spacing() {
    use Shot::{Night, Normal};
    let strips = [
        Strip::evenly((17.0, 2.0), &[Normal, Night, Night, Normal, Night, Normal]),
        Strip::evenly((51.5, 2.0), &[Night, Night, Normal, Night, Night, Night]),
    ];
    let found = detect(V500, &strips);
    check(&found, &strips, 0.8);
    let expected: Vec<_> = strips.iter().flat_map(Strip::truth).collect();
    for (f, (_, shot)) in found.frames.iter().zip(&expected) {
        assert_eq!(f.placed_by_spacing, *shot == Night, "{f:?}");
    }
}

#[test]
fn frames_run_under_a_holder_covering_the_rebate() {
    // The holder covers the film just past the image, which is a little wider than 24 mm
    // and runs on under it: the frames reach the holder's edge, but not onto it.
    let mut strips = [
        Strip::evenly((17.0, 2.0), &six()),
        Strip::evenly((51.5, 2.0), &six()),
    ];
    for s in &mut strips {
        (s.visible, s.image) = (12.2, 12.4);
    }
    let found = detect(V500, &strips);
    check(&found, &strips, 0.35);
    assert!(found.frames.iter().all(|f| f.edges_seen == 4), "{found:#?}");
}

#[test]
fn uneven_spacing_follows_the_edges() {
    let starts = [1.5, 40.1, 77.6, 116.3, 153.9, 192.4];
    let frames: Vec<_> = starts.iter().map(|&s| (s, Shot::Normal)).collect();
    let strips = [
        Strip::new((17.0, 2.0), &frames),
        Strip::evenly((51.5, 2.0), &six()),
    ];
    check(&detect(V500, &strips), &strips, 0.35);
}

#[test]
fn frames_of_the_mock_scanner() {
    let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
    let request = ScanRequest::new(ScanSource::Transparency, 75);
    let result = dev.scan(&request, &mut NoProgress).unwrap();
    let found = detect_frames(
        result.primary(),
        result.gamma,
        result.dpi,
        result.area,
        &DetectParams::default(),
    );
    assert_eq!(found.frames.len(), 12, "{found:#?}");
    for (i, f) in found.frames.iter().enumerate() {
        let (x, y, w, h) = mock::frame_rect_mm(i);
        let r = f.rect;
        let off = [
            r.cx_mm - (x + w / 2.0),
            r.cy_mm - (y + h / 2.0),
            r.width_mm - (w - 0.2),
            r.height_mm - (h - 0.2),
        ];
        assert!(off.iter().all(|d| d.abs() < 0.5), "frame {i}: {r:?}");
    }
}

#[test]
fn no_film_no_frames() {
    let image = DynamicImage::ImageRgb16(ImageBuffer::from_pixel(270, 930, Rgb([30000; 3])));
    let found = detect_frames(&image, GAMMA, DPI, V500, &DetectParams::default());
    assert_eq!(found, FrameDetection::default());
}

#[test]
fn frame_rect_geometry() {
    let r = FrameRect {
        cx_mm: 10.0,
        cy_mm: 20.0,
        width_mm: 4.0,
        height_mm: 2.0,
        angle_deg: 90.0,
    };
    // Turned a quarter clockwise, its width runs down the bed.
    let [tl, tr, ..] = r.corners();
    assert!(
        (tl.0 - 11.0).abs() < 1e-5 && (tl.1 - 18.0).abs() < 1e-5,
        "{tl:?}"
    );
    assert!(
        (tr.0 - 11.0).abs() < 1e-5 && (tr.1 - 22.0).abs() < 1e-5,
        "{tr:?}"
    );
    assert!(r.contains(10.5, 21.9) && !r.contains(11.5, 20.0));
    let (u, v) = r.to_local(12.0, 23.0);
    let (x, y) = r.from_local(u, v);
    assert!((x - 12.0).abs() < 1e-5 && (y - 23.0).abs() < 1e-5);
}

#[test]
fn crops_are_straightened() {
    // 10 pixels per millimetre; red and green are 1000 x the pixel centres' x and y.
    let dpi = 254;
    let image: ImageBuffer<Rgb<u16>, _> = ImageBuffer::from_fn(300, 300, |x, y| {
        Rgb([(x * 100 + 50) as u16, (y * 100 + 50) as u16, 7])
    });
    let image = DynamicImage::ImageRgb16(image);
    let area = ScanArea {
        x_mm: 5.0,
        ..ScanArea::full(30.0, 30.0)
    };
    let frame = FrameRect {
        cx_mm: 20.0,
        cy_mm: 15.0,
        width_mm: 6.0,
        height_mm: 4.0,
        angle_deg: 30.0,
    };
    let crop = crop_frame(&image, dpi, area, &frame).to_rgb16();
    assert_eq!(crop.dimensions(), (60, 40));
    for (i, j) in [(0, 0), (59, 0), (30, 20), (12, 39)] {
        let (x, y) = frame.from_local((i as f32 + 0.5) / 10.0 - 3.0, (j as f32 + 0.5) / 10.0 - 2.0);
        let p = crop.get_pixel(i, j).0;
        // Within 0.03 px: the Lanczos kernel follows a ramp closely, not exactly.
        assert!(
            (p[0] as f32 - (x - area.x_mm) * 1000.0).abs() <= 3.0,
            "{p:?} at {x}"
        );
        assert!((p[1] as f32 - y * 1000.0).abs() <= 3.0, "{p:?} at {y}");
        assert_eq!(p[2], 7);
    }

    let gray = DynamicImage::ImageLuma8(image.to_luma8());
    assert!(matches!(
        crop_frame(&gray, dpi, area, &frame),
        DynamicImage::ImageLuma8(_)
    ));
}

/// Grain: independent noise, 16-bit, about mid-grey.
fn grain(w: u32, h: u32) -> DynamicImage {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    DynamicImage::ImageLuma16(ImageBuffer::from_fn(w, h, |_, _| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        Luma([30_000 + (state % 4_000) as u16])
    }))
}

fn std_dev(v: &[u16]) -> f64 {
    let mean = v.iter().map(|&p| f64::from(p)).sum::<f64>() / v.len() as f64;
    (v.iter()
        .map(|&p| (f64::from(p) - mean).powi(2))
        .sum::<f64>()
        / v.len() as f64)
        .sqrt()
}

#[test]
fn crops_on_whole_pixels_are_copies() {
    // 10 pixels per millimetre; the frame's edges on pixel edges.
    let image = grain(200, 200);
    let area = ScanArea::full(20.0, 20.0);
    let frame = FrameRect {
        cx_mm: 8.0,
        cy_mm: 11.0,
        width_mm: 6.0,
        height_mm: 4.0,
        angle_deg: 0.0,
    };
    let crop = crop_frame(&image, 254, area, &frame).to_luma16();
    let source = image.to_luma16();
    for (i, j, p) in crop.enumerate_pixels() {
        assert_eq!(p, source.get_pixel(50 + i, 90 + j), "at ({i}, {j})");
    }
}

#[test]
fn unturned_crops_move_onto_whole_pixels() {
    // 0.3 px right and 0.4 px down of the pixel grid: copied from the nearest whole pixels.
    let image = grain(200, 200);
    let area = ScanArea::full(20.0, 20.0);
    let frame = FrameRect {
        cx_mm: 8.03,
        cy_mm: 11.04,
        width_mm: 6.0,
        height_mm: 4.0,
        angle_deg: 0.0,
    };
    let crop = crop_frame(&image, 254, area, &frame).to_luma16();
    let source = image.to_luma16();
    for (i, j, p) in crop.enumerate_pixels() {
        assert_eq!(p, source.get_pixel(50 + i, 90 + j), "at ({i}, {j})");
    }
}

#[test]
fn straightening_keeps_the_grain() {
    // Half a pixel off and turned a little: a bilinear cut left about half the grain's
    // deviation where it read halfway between pixels.
    let image = grain(200, 200);
    let area = ScanArea::full(20.0, 20.0);
    let frame = FrameRect {
        cx_mm: 10.05,
        cy_mm: 10.05,
        width_mm: 12.0,
        height_mm: 12.0,
        angle_deg: 0.3,
    };
    let crop = crop_frame(&image, 254, area, &frame).to_luma16();
    let kept = std_dev(crop.as_raw()) / std_dev(image.to_luma16().as_raw());
    assert!(kept > 0.8, "{kept}");
}

#[test]
fn previews_are_crops_scaled_down() {
    // As in `crops_are_straightened`: red and green are 1000 x the pixel centres' x and y.
    let image: ImageBuffer<Rgb<u16>, _> = ImageBuffer::from_fn(300, 300, |x, y| {
        Rgb([(x * 100 + 50) as u16, (y * 100 + 50) as u16, 7])
    });
    let image = DynamicImage::ImageRgb16(image);
    let area = ScanArea {
        x_mm: 5.0,
        ..ScanArea::full(30.0, 30.0)
    };
    let frame = FrameRect {
        cx_mm: 20.0,
        cy_mm: 15.0,
        width_mm: 6.0,
        height_mm: 4.0,
        angle_deg: 30.0,
    };
    // Half size: 127 dpi, 5 pixels per millimetre.
    let (preview, scale) = preview_frame(&image, 254, area, Some(&frame), 30);
    assert_eq!(scale, 0.5);
    let preview = preview.to_rgb16();
    assert_eq!(preview.dimensions(), (30, 20));
    for (i, j) in [(2, 2), (27, 2), (15, 10), (6, 17)] {
        let (x, y) = frame.from_local((i as f32 + 0.5) / 5.0 - 3.0, (j as f32 + 0.5) / 5.0 - 2.0);
        let p = preview.get_pixel(i, j).0;
        assert!(
            (p[0] as f32 - (x - area.x_mm) * 1000.0).abs() < 120.0,
            "{p:?} at {x}"
        );
        assert!((p[1] as f32 - y * 1000.0).abs() < 120.0, "{p:?} at {y}");
    }
    // Small enough already: the crop itself.
    let (full, scale) = preview_frame(&image, 254, area, Some(&frame), 100);
    assert_eq!((scale, full.width()), (1.0, 60));
    // The whole scan.
    let (whole, scale) = preview_frame(&image, 254, area, None, 150);
    assert_eq!((scale, whole.width()), (0.5, 150));
}
