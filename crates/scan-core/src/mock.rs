//! A virtual scanner that renders synthetic scans, for CI and hardware-free development.
//!
//! The transparency source simulates a holder with two vertical 35mm strips of six color
//! negative frames each: an orange-masked film base, clear sprocket holes, an opaque holder
//! around the strips, and a handful of dust specks that show up in both the visible and the
//! infrared channel (as on a real scanner, where film dyes are nearly transparent to infrared).
//! The frames are exposed on Kodak Kodacolor 200, through its datasheet curves, so they
//! invert back to their scenes with its profile.
//! As with some real films, the cyan dye absorbs a little infrared, so frame detail shows
//! faintly in the infrared channel too.

use image::{DynamicImage, ImageBuffer, Luma, Rgb};

use crate::{
    BitDepth, Capabilities, ColorMode, DeviceId, DeviceInfo, Exposure, Gray16Image, MM_PER_INCH,
    ProgressSink, ScanArea, ScanError, ScanPass, ScanRequest, ScanResult, ScanSource,
    ScannerBackend, ScannerDevice, SourceCapabilities,
};

pub const MOCK_BACKEND: &str = "mock";

/// Width of each film strip (35mm film).
pub const STRIP_WIDTH_MM: f32 = 35.0;
/// Left edges of the two strips in the holder.
pub const STRIP_X_MM: [f32; 2] = [5.0, 50.0];
/// Frame size: 24mm across the strip, 36mm along it.
pub const FRAME_W_MM: f32 = 24.0;
pub const FRAME_H_MM: f32 = 36.0;
pub const FRAME_GAP_MM: f32 = 2.0;
pub const FRAMES_PER_STRIP: usize = 6;
pub const FIRST_FRAME_Y_MM: f32 = 3.0;

/// Transmittance of the orange mask of unexposed color negative film base (R, G, B).
pub const FILM_BASE: [f32; 3] = [0.85, 0.55, 0.33];
/// The film the frames are shot on.
pub const FILM: scan_film::FilmStock = scan_film::stocks::KODAK_KODACOLOR_200;
const HOLDER: f32 = 0.002;
const FILM_IR: f32 = 0.9;
/// Fraction of the infrared that the densest cyan dye absorbs.
pub const CYAN_IR_CROSSTALK: f32 = 0.2;

/// Centres and radii (x, y, r) in millimetres of the dust specks on a scan of `area`.
pub fn dust_specks(area: ScanArea) -> Vec<(f32, f32, f32)> {
    Dust::generate(area).specks
}

/// Rectangle (x, y, w, h) in millimetres of frame `index` (0..12) on the holder.
pub fn frame_rect_mm(index: usize) -> (f32, f32, f32, f32) {
    let strip = index / FRAMES_PER_STRIP;
    let n = index % FRAMES_PER_STRIP;
    let x = STRIP_X_MM[strip] + (STRIP_WIDTH_MM - FRAME_W_MM) / 2.0;
    let y = FIRST_FRAME_Y_MM + n as f32 * (FRAME_H_MM + FRAME_GAP_MM);
    (x, y, FRAME_W_MM, FRAME_H_MM)
}

/// A backend exposing a single virtual scanner.
#[derive(Default)]
pub struct MockBackend;

impl MockBackend {
    pub fn device_info() -> DeviceInfo {
        DeviceInfo {
            id: DeviceId("mock:virtual-film-scanner".into()),
            manufacturer: "film_scanner".into(),
            model: "Virtual Film Scanner".into(),
            backend: MOCK_BACKEND,
        }
    }

    pub fn capabilities() -> Capabilities {
        let common = |source, resolutions: Vec<u32>, infrared, max_area| SourceCapabilities {
            source,
            resolutions,
            bit_depths: vec![BitDepth::Eight, BitDepth::Sixteen],
            color_modes: vec![ColorMode::Color, ColorMode::Gray],
            infrared,
            max_exposure_passes: 4,
            max_area,
        };
        Capabilities {
            sources: vec![
                common(
                    ScanSource::Flatbed,
                    vec![75, 150, 300],
                    false,
                    ScanArea::full(216.0, 297.0),
                ),
                common(
                    ScanSource::Transparency,
                    vec![75, 150, 300, 600],
                    true,
                    ScanArea::full(90.0, 235.0),
                ),
            ],
        }
    }
}

impl ScannerBackend for MockBackend {
    fn name(&self) -> &'static str {
        MOCK_BACKEND
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>, ScanError> {
        Ok(vec![Self::device_info()])
    }

    fn open(&self, device: &DeviceInfo) -> Result<Box<dyn ScannerDevice>, ScanError> {
        if device.id != Self::device_info().id {
            return Err(ScanError::DeviceNotFound(device.id.to_string()));
        }
        Ok(Box::new(MockDevice {
            info: Self::device_info(),
            caps: Self::capabilities(),
        }))
    }
}

pub struct MockDevice {
    info: DeviceInfo,
    caps: Capabilities,
}

impl ScannerDevice for MockDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn scan(
        &mut self,
        request: &ScanRequest,
        progress: &mut dyn ProgressSink,
    ) -> Result<ScanResult, ScanError> {
        let (_, area) = request.validate(&self.caps)?;
        let exposures = request.effective_exposures();
        let total_passes = exposures.len() + usize::from(request.infrared);
        let dust = Dust::generate(area);

        let mut passes = Vec::with_capacity(exposures.len());
        for (i, exposure) in exposures.iter().enumerate() {
            let mut pass_progress = |f: f32| (i as f32 + f) / total_passes as f32;
            let image = render_visible(
                request,
                area,
                *exposure,
                &dust,
                progress,
                &mut pass_progress,
            )?;
            passes.push(ScanPass {
                exposure: *exposure,
                image,
            });
        }
        let infrared = if request.infrared {
            let done = exposures.len() as f32;
            Some(render_infrared(request, area, &dust, progress, |f| {
                (done + f) / total_passes as f32
            })?)
        } else {
            None
        };
        progress.progress(1.0);
        Ok(ScanResult {
            source: request.source,
            dpi: request.dpi,
            area,
            passes,
            infrared,
            gamma: 1.0,
        })
    }
}

/// Deterministic dust specks in bed coordinates, so they land in the same place at any DPI.
struct Dust {
    specks: Vec<(f32, f32, f32)>,
}

impl Dust {
    fn generate(area: ScanArea) -> Self {
        let mut seed: u32 = 0x5EED_F11F;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let specks = (0..40)
            .map(|_| {
                let x = area.x_mm + next() * area.width_mm;
                let y = area.y_mm + next() * area.height_mm;
                let r = 0.1 + next() * 0.3;
                (x, y, r)
            })
            .collect();
        Self { specks }
    }

    /// Light transmission factor at a point: 1.0 is clean, ~0.1 under a speck.
    fn factor(&self, x: f32, y: f32) -> f32 {
        for &(sx, sy, r) in &self.specks {
            let (dx, dy) = (x - sx, y - sy);
            if dx * dx + dy * dy <= r * r {
                return 0.1;
            }
        }
        1.0
    }
}

/// What lies under a point of the transparency holder.
enum Region {
    Holder,
    SprocketHole,
    FilmBase,
    Frame { index: usize, u: f32, v: f32 },
}

fn region_at(x: f32, y: f32) -> Region {
    for (strip, &sx) in STRIP_X_MM.iter().enumerate() {
        let lx = x - sx;
        if !(0.0..STRIP_WIDTH_MM).contains(&lx) {
            continue;
        }
        // Sprocket holes: 2.0 x 2.8mm, 4.75mm pitch, 1mm from each strip edge.
        let in_edge =
            (1.0..3.0).contains(&lx) || (STRIP_WIDTH_MM - 3.0..STRIP_WIDTH_MM - 1.0).contains(&lx);
        if in_edge && y.rem_euclid(4.75) < 2.8 {
            return Region::SprocketHole;
        }
        for n in 0..FRAMES_PER_STRIP {
            let index = strip * FRAMES_PER_STRIP + n;
            let (fx, fy, fw, fh) = frame_rect_mm(index);
            if (fx..fx + fw).contains(&x) && (fy..fy + fh).contains(&y) {
                return Region::Frame {
                    index,
                    u: (x - fx) / fw,
                    v: (y - fy) / fh,
                };
            }
        }
        return Region::FilmBase;
    }
    Region::Holder
}

/// Scene color (0..1 per channel, 1 a white surface) of the positive image in frame
/// `index`, at (`u`, `v`) across and down it (0..1). The frame is lit so that a surface of
/// `s` exposes the film to scene light `0.9 * s`: a white surface reflects 90%.
pub fn frame_scene(index: usize, u: f32, v: f32) -> [f32; 3] {
    let hue = index as f32 / 12.0;
    let (cu, cv) = (0.5 + 0.2 * (hue * std::f32::consts::TAU).cos(), 0.5);
    let d = ((u - cu).powi(2) + (v - cv).powi(2)).sqrt();
    let disc = if d < 0.25 { 1.0 } else { 0.0 };
    let sky = 1.0 - v;
    [
        (0.2 + 0.6 * sky * hue + 0.6 * disc).min(1.0),
        (0.3 + 0.5 * u + 0.2 * disc).min(1.0),
        (0.2 + 0.7 * sky * (1.0 - hue)).min(1.0),
    ]
}

fn transmittance_transparency(x: f32, y: f32) -> [f32; 3] {
    match region_at(x, y) {
        Region::Holder => [HOLDER; 3],
        Region::SprocketHole => [1.0; 3],
        Region::FilmBase => FILM_BASE,
        Region::Frame { index, u, v } => {
            // A white surface reflects 90%; a grey card (18%) is exposed normally.
            let e = frame_scene(index, u, v).map(|s| 0.9 * s);
            let density = FILM.model().negative(e);
            std::array::from_fn(|c| FILM_BASE[c] * 10f32.powf(-density[c]))
        }
    }
}

fn reflectance_flatbed(area: ScanArea, x: f32, y: f32) -> [f32; 3] {
    let u = (x - area.x_mm) / area.width_mm.max(1e-3);
    let v = (y - area.y_mm) / area.height_mm.max(1e-3);
    [u, v, 1.0 - 0.5 * (u + v)]
}

fn bed_coord(origin_mm: f32, pixel: u32, dpi: u32) -> f32 {
    origin_mm + (pixel as f32 + 0.5) / dpi as f32 * MM_PER_INCH
}

fn render_visible(
    request: &ScanRequest,
    area: ScanArea,
    exposure: Exposure,
    dust: &Dust,
    progress: &mut dyn ProgressSink,
    map_progress: &mut dyn FnMut(f32) -> f32,
) -> Result<DynamicImage, ScanError> {
    let (w, h) = area.pixel_size(request.dpi);
    let gain = 2f32.powf(exposure.ev);
    let gray = request.color_mode == ColorMode::Gray;
    let channels = if gray { 1 } else { 3 };
    let mut data = vec![0u16; (w * h) as usize * channels];

    for py in 0..h {
        if py % 64 == 0 {
            if progress.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            progress.progress(map_progress(py as f32 / h as f32));
        }
        let y = bed_coord(area.y_mm, py, request.dpi);
        for px in 0..w {
            let x = bed_coord(area.x_mm, px, request.dpi);
            let t = match request.source {
                ScanSource::Transparency => transmittance_transparency(x, y),
                ScanSource::Flatbed => reflectance_flatbed(area, x, y),
            };
            let k = dust.factor(x, y) * gain;
            let to16 = |v: f32| ((v * k).clamp(0.0, 1.0) * 65535.0).round() as u16;
            let i = (py * w + px) as usize * channels;
            if gray {
                data[i] = to16(0.2126 * t[0] + 0.7152 * t[1] + 0.0722 * t[2]);
            } else {
                for c in 0..3 {
                    data[i + c] = to16(t[c]);
                }
            }
        }
    }

    let image = if gray {
        DynamicImage::ImageLuma16(ImageBuffer::<Luma<u16>, _>::from_raw(w, h, data).unwrap())
    } else {
        DynamicImage::ImageRgb16(ImageBuffer::<Rgb<u16>, _>::from_raw(w, h, data).unwrap())
    };
    Ok(match (request.bit_depth, gray) {
        (BitDepth::Sixteen, _) => image,
        (BitDepth::Eight, true) => DynamicImage::ImageLuma8(image.to_luma8()),
        (BitDepth::Eight, false) => DynamicImage::ImageRgb8(image.to_rgb8()),
    })
}

fn render_infrared(
    request: &ScanRequest,
    area: ScanArea,
    dust: &Dust,
    progress: &mut dyn ProgressSink,
    map_progress: impl Fn(f32) -> f32,
) -> Result<Gray16Image, ScanError> {
    let (w, h) = area.pixel_size(request.dpi);
    let mut img = Gray16Image::new(w, h);
    for py in 0..h {
        if py % 64 == 0 {
            if progress.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            progress.progress(map_progress(py as f32 / h as f32));
        }
        let y = bed_coord(area.y_mm, py, request.dpi);
        for px in 0..w {
            let x = bed_coord(area.x_mm, px, request.dpi);
            let base = match region_at(x, y) {
                Region::Holder => HOLDER,
                Region::SprocketHole => 1.0,
                Region::FilmBase => FILM_IR,
                Region::Frame { .. } => {
                    // Cyan dye density follows how much red the negative blocks.
                    let cyan = 1.0 - transmittance_transparency(x, y)[0] / FILM_BASE[0];
                    FILM_IR * (1.0 - CYAN_IR_CROSSTALK * cyan)
                }
            };
            let v = base * dust.factor(x, y);
            img.put_pixel(px, py, Luma([(v * 65535.0).round() as u16]));
        }
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancelToken, NoProgress};

    fn open() -> Box<dyn ScannerDevice> {
        MockBackend.open(&MockBackend::device_info()).unwrap()
    }

    #[test]
    fn lists_one_device() {
        let devices = MockBackend.list_devices().unwrap();
        assert_eq!(devices, vec![MockBackend::device_info()]);
    }

    #[test]
    fn rejects_unknown_device() {
        let mut info = MockBackend::device_info();
        info.id = DeviceId("mock:other".into());
        assert!(matches!(
            MockBackend.open(&info),
            Err(ScanError::DeviceNotFound(_))
        ));
    }

    #[test]
    fn film_scan_dimensions_and_channels() {
        let mut dev = open();
        let mut req = ScanRequest::new(ScanSource::Transparency, 75);
        req.infrared = true;
        req.exposures = vec![Exposure { ev: 0.0 }, Exposure { ev: 1.0 }];
        let result = dev.scan(&req, &mut NoProgress).unwrap();

        let expected = ScanArea::full(90.0, 235.0).pixel_size(75);
        assert_eq!(result.dimensions(), expected);
        assert_eq!(result.passes.len(), 2);
        assert!(matches!(result.primary(), DynamicImage::ImageRgb16(_)));
        let ir = result.infrared.as_ref().expect("infrared pass");
        assert_eq!(ir.dimensions(), expected);
    }

    #[test]
    fn frames_show_orange_mask_and_holder_is_dark() {
        let mut dev = open();
        let result = dev
            .scan(
                &ScanRequest::new(ScanSource::Transparency, 150),
                &mut NoProgress,
            )
            .unwrap();
        let img = result.primary().to_rgb16();
        let px = |x_mm: f32, y_mm: f32| {
            let p = img.get_pixel(
                (x_mm / MM_PER_INCH * 150.0) as u32,
                (y_mm / MM_PER_INCH * 150.0) as u32,
            );
            p.0
        };
        // Holder between the strips blocks light.
        let holder = px(45.0, 100.0);
        assert!(holder.iter().all(|&c| c < 1000), "holder {holder:?}");
        // Film base between frames is orange: R > G > B.
        let base = px(22.5, FIRST_FRAME_Y_MM + FRAME_H_MM + FRAME_GAP_MM / 2.0);
        assert!(base[0] > base[1] && base[1] > base[2], "film base {base:?}");
    }

    #[test]
    fn higher_exposure_is_brighter() {
        let mut dev = open();
        let mut req = ScanRequest::new(ScanSource::Transparency, 75);
        req.exposures = vec![Exposure { ev: -1.0 }, Exposure { ev: 1.0 }];
        let result = dev.scan(&req, &mut NoProgress).unwrap();
        let mean = |img: &DynamicImage| {
            let v = img.to_rgb16().into_raw();
            v.iter().map(|&x| x as u64).sum::<u64>() / v.len() as u64
        };
        assert!(mean(&result.passes[1].image) > mean(&result.passes[0].image));
    }

    #[test]
    fn eight_bit_gray_flatbed() {
        let mut dev = open();
        let mut req = ScanRequest::new(ScanSource::Flatbed, 75);
        req.bit_depth = BitDepth::Eight;
        req.color_mode = ColorMode::Gray;
        let result = dev.scan(&req, &mut NoProgress).unwrap();
        assert!(matches!(result.primary(), DynamicImage::ImageLuma8(_)));
        assert!(result.infrared.is_none());
    }

    #[test]
    fn infrared_rejected_on_flatbed() {
        let mut dev = open();
        let mut req = ScanRequest::new(ScanSource::Flatbed, 75);
        req.infrared = true;
        assert!(matches!(
            dev.scan(&req, &mut NoProgress),
            Err(ScanError::InvalidRequest(_))
        ));
    }

    struct Recorder {
        values: Vec<f32>,
        cancel: CancelToken,
    }

    impl ProgressSink for Recorder {
        fn progress(&mut self, fraction: f32) {
            self.values.push(fraction);
        }
        fn is_cancelled(&self) -> bool {
            self.cancel.is_cancelled()
        }
    }

    #[test]
    fn progress_is_monotonic_and_finishes() {
        let mut dev = open();
        let mut req = ScanRequest::new(ScanSource::Transparency, 150);
        req.infrared = true;
        let mut rec = Recorder {
            values: vec![],
            cancel: CancelToken::new(),
        };
        dev.scan(&req, &mut rec).unwrap();
        assert!(
            rec.values.windows(2).all(|w| w[0] <= w[1]),
            "{:?}",
            rec.values
        );
        assert_eq!(rec.values.last(), Some(&1.0));
    }

    #[test]
    fn cancelled_scan_returns_cancelled() {
        let mut dev = open();
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut rec = Recorder {
            values: vec![],
            cancel,
        };
        let res = dev.scan(&ScanRequest::new(ScanSource::Transparency, 75), &mut rec);
        assert!(matches!(res, Err(ScanError::Cancelled)));
    }
}
