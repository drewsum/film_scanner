//! The [`scan_core`] backend: SANE devices, their capabilities, and scanning.

use scan_core::image::{DynamicImage, ImageBuffer, Luma, Rgb};
use scan_core::{
    BitDepth, Capabilities, ColorMode, DeviceId, DeviceInfo, Exposure, ProgressSink, ScanArea,
    ScanError, ScanPass, ScanRequest, ScanResult, ScanSource, ScannerBackend, ScannerDevice,
    SourceCapabilities,
};

use crate::handle::{self, SaneHandle};
use crate::options::{Driver, Frame, Kind, OptionDesc, Options, Unit, Value, names};

pub const BACKEND_NAME: &str = "sane";

/// How the scans' values encode light. SANE doesn't say; drivers mostly deliver data ready
/// for display, as WIA does.
const GAMMA: f32 = 2.2;

/// Resolutions offered when a driver accepts a continuous range.
const STANDARD_RESOLUTIONS: [u32; 17] = [
    75, 100, 150, 200, 240, 300, 400, 600, 800, 1200, 1600, 2400, 3200, 4800, 6400, 9600, 12800,
];

/// Bed size assumed when a driver has no geometry options in millimetres (US letter); the
/// whole bed is then scanned.
const DEFAULT_BED: ScanArea = ScanArea {
    x_mm: 0.0,
    y_mm: 0.0,
    width_mm: 215.9,
    height_mm: 279.4,
};

/// Bytes asked for per `sane_read`.
const CHUNK: usize = 64 * 1024;

/// Scanners reachable through SANE (Linux, macOS, BSD).
pub struct SaneBackend;

impl ScannerBackend for SaneBackend {
    fn name(&self) -> &'static str {
        BACKEND_NAME
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>, ScanError> {
        // Most macOS machines have no SANE; that isn't an error worth showing.
        if let Some(why) = handle::unavailable() {
            log::info!("SANE backend disabled: {why}");
            return Ok(Vec::new());
        }
        Ok(handle::devices()?
            .into_iter()
            .map(|d| DeviceInfo {
                id: DeviceId(d.name),
                manufacturer: d.vendor,
                model: d.model,
                backend: BACKEND_NAME,
            })
            .collect())
    }

    fn open(&self, device: &DeviceInfo) -> Result<Box<dyn ScannerDevice>, ScanError> {
        let handle = SaneHandle::open(&device.id.0)?;
        Ok(Box::new(SaneDevice::new(device.clone(), Box::new(handle))?))
    }
}

/// How a source is selected and laid out in the driver's options.
#[derive(Clone, Debug, PartialEq)]
struct SourceSetup {
    source: ScanSource,
    /// The `source` option's value, if the driver has one.
    name: Option<String>,
    /// The `mode` option's value for each color mode.
    color: Option<String>,
    gray: Option<String>,
    /// The geometry options' minimum corner, in mm, if the driver takes an area in mm.
    origin: Option<(f64, f64)>,
}

impl SourceSetup {
    fn mode(&self, mode: ColorMode) -> Option<&String> {
        match mode {
            ColorMode::Color => self.color.as_ref(),
            ColorMode::Gray => self.gray.as_ref(),
        }
    }
}

/// Classify a `source` option value. Document feeders, and infrared sources (until the
/// backend reads them), aren't film or flatbed sources.
fn classify_source(name: &str) -> Option<ScanSource> {
    let name = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if has(&["adf", "feeder", "duplex", "infrared"]) {
        None
    } else if has(&[
        "transparen",
        "tpu",
        "tma",
        "xpa",
        "film",
        "slide",
        "negative",
        "positive",
    ]) {
        Some(ScanSource::Transparency)
    } else {
        Some(ScanSource::Flatbed)
    }
}

fn classify_mode(name: &str) -> Option<ColorMode> {
    let name = name.to_ascii_lowercase();
    if name.contains("infrared") {
        None
    } else if name.contains("color") || name.contains("colour") {
        Some(ColorMode::Color)
    } else if name.contains("gray") || name.contains("grey") {
        Some(ColorMode::Gray)
    } else {
        None
    }
}

fn resolutions(option: &OptionDesc) -> Vec<u32> {
    let mut out: Vec<u32> = match &option.constraint {
        crate::options::Constraint::Numbers(n) => n
            .iter()
            .filter(|&&v| v >= 1.0)
            .map(|&v| v.round() as u32)
            .collect(),
        crate::options::Constraint::Range { min, max, .. } => {
            let mut r: Vec<u32> = STANDARD_RESOLUTIONS
                .iter()
                .copied()
                .filter(|&v| (option.nearest(v as f64) - v as f64).abs() < 0.01)
                .collect();
            if r.is_empty() && *max >= 1.0 {
                r.push(min.max(1.0).round() as u32);
                r.push(max.round() as u32);
            }
            r
        }
        _ => STANDARD_RESOLUTIONS
            .iter()
            .copied()
            .filter(|&v| v <= 1200)
            .collect(),
    };
    out.sort_unstable();
    out.dedup();
    out
}

/// Capabilities of the selected source, from the driver's current options.
fn source_capabilities(
    options: &Options,
    source: ScanSource,
    name: Option<String>,
) -> Option<(SourceCapabilities, SourceSetup)> {
    let Some(resolution) = options.get(names::RESOLUTION) else {
        log::warn!("SANE source {name:?} has no resolution option");
        return None;
    };
    let resolutions = resolutions(resolution);
    if resolutions.is_empty() {
        return None;
    }

    let bit_depths = match options.get(names::DEPTH) {
        Some(depth) => [BitDepth::Eight, BitDepth::Sixteen]
            .into_iter()
            .filter(|d| (depth.nearest(d.bits() as f64) - d.bits() as f64).abs() < 0.01)
            .collect(),
        None => vec![BitDepth::Eight],
    };
    if bit_depths.is_empty() {
        return None;
    }

    let mut setup = SourceSetup {
        source,
        name,
        color: None,
        gray: None,
        origin: None,
    };
    let color_modes = match options.get(names::MODE) {
        Some(mode) => {
            for value in mode.strings() {
                match classify_mode(value) {
                    Some(ColorMode::Color) if setup.color.is_none() => {
                        setup.color = Some(value.clone())
                    }
                    Some(ColorMode::Gray) if setup.gray.is_none() => {
                        setup.gray = Some(value.clone())
                    }
                    _ => {}
                }
            }
            [ColorMode::Color, ColorMode::Gray]
                .into_iter()
                .filter(|&m| setup.mode(m).is_some())
                .collect()
        }
        // Without a mode option the driver decides; parameters tell which it chose.
        None => vec![ColorMode::Color],
    };
    if color_modes.is_empty() {
        return None;
    }

    let bounds = |name| {
        options
            .get(name)
            .filter(|o| o.unit == Unit::Mm)
            .and_then(OptionDesc::bounds)
    };
    let max_area = match (
        bounds(names::TL_X),
        bounds(names::TL_Y),
        bounds(names::BR_X),
        bounds(names::BR_Y),
    ) {
        (Some((x0, _)), Some((y0, _)), Some((_, x1)), Some((_, y1))) if x1 > x0 && y1 > y0 => {
            setup.origin = Some((x0, y0));
            ScanArea::full((x1 - x0) as f32, (y1 - y0) as f32)
        }
        _ => {
            log::warn!("SANE source {:?} has no scan area in mm", setup.name);
            DEFAULT_BED
        }
    };

    let caps = SourceCapabilities {
        source,
        resolutions,
        bit_depths,
        color_modes,
        infrared: false,
        max_exposure_passes: 1,
        max_area,
    };
    Some((caps, setup))
}

/// An open SANE device.
pub struct SaneDevice {
    info: DeviceInfo,
    driver: Box<dyn Driver>,
    caps: Capabilities,
    setups: Vec<SourceSetup>,
    options: Options,
}

impl SaneDevice {
    /// Read the device's capabilities, selecting each of its sources in turn.
    pub fn new(info: DeviceInfo, driver: Box<dyn Driver>) -> Result<Self, ScanError> {
        let options = Options(Vec::new());
        let mut device = Self {
            info,
            driver,
            caps: Capabilities::default(),
            setups: Vec::new(),
            options,
        };
        device.reload()?;

        let mut sources: Vec<(ScanSource, Option<String>)> = match device.options.get(names::SOURCE)
        {
            Some(option) => option
                .strings()
                .iter()
                .filter_map(|s| classify_source(s).map(|source| (source, Some(s.clone()))))
                .collect(),
            None => vec![(ScanSource::Flatbed, None)],
        };
        // The first value for each source wins (e.g. "Transparency" before "Negative").
        let mut seen = Vec::new();
        sources.retain(|(source, _)| {
            let new = !seen.contains(source);
            seen.push(*source);
            new
        });

        for (source, name) in sources {
            if let Some(name) = &name {
                device.set(names::SOURCE, Value::String(name.clone()))?;
            }
            // Depths can depend on the mode; color is the one that matters for film.
            if let Some(color) = device
                .options
                .get(names::MODE)
                .and_then(|m| {
                    m.strings()
                        .iter()
                        .find(|s| classify_mode(s) == Some(ColorMode::Color))
                })
                .cloned()
            {
                device.set(names::MODE, Value::String(color))?;
            }
            if let Some((caps, setup)) = source_capabilities(&device.options, source, name) {
                device.caps.sources.push(caps);
                device.setups.push(setup);
            }
        }
        if device.caps.sources.is_empty() {
            return Err(ScanError::Unsupported(format!(
                "{} has no flatbed or film source this backend can use",
                device.info.model
            )));
        }
        Ok(device)
    }

    fn reload(&mut self) -> Result<(), ScanError> {
        self.options = Options(self.driver.options()?);
        Ok(())
    }

    /// Set option `name` if the driver has it, moving numbers onto an accepted value.
    /// Returns the value set, if any.
    fn set(&mut self, name: &str, value: Value) -> Result<Option<Value>, ScanError> {
        let Some(option) = self.options.get(name).cloned() else {
            return Ok(None);
        };
        let value = match (option.kind, value) {
            (Kind::Int | Kind::Fixed, Value::Number(n)) => Value::Number(option.nearest(n)),
            (_, value) => value,
        };
        if self.driver.set(&option, &value)? {
            self.reload()?;
        }
        Ok(Some(value))
    }

    /// Set the driver up for `request`. Returns the area it will scan, which drivers may
    /// round (SANE's test driver to whole millimetres).
    fn configure(&mut self, request: &ScanRequest, area: ScanArea) -> Result<ScanArea, ScanError> {
        let setup = self
            .setups
            .iter()
            .find(|s| s.source == request.source)
            .cloned()
            .ok_or_else(|| ScanError::InvalidRequest("source not available".into()))?;
        if let Some(name) = &setup.name {
            self.set(names::SOURCE, Value::String(name.clone()))?;
        }
        if let Some(mode) = setup.mode(request.color_mode) {
            self.set(names::MODE, Value::String(mode.clone()))?;
        }
        self.set(names::DEPTH, Value::Number(request.bit_depth.bits() as f64))?;
        self.set(names::RESOLUTION, Value::Number(request.dpi as f64))?;
        self.set(names::PREVIEW, Value::Bool(false))?;
        if let Some((x0, y0)) = setup.origin {
            let left = x0 + area.x_mm as f64;
            let top = y0 + area.y_mm as f64;
            let right = left + area.width_mm as f64;
            let bottom = top + area.height_mm as f64;
            // Widen first, so the top-left corner is never set past the bottom-right one.
            for name in [names::BR_X, names::BR_Y] {
                if let Some(max) = self.options.get(name).and_then(OptionDesc::bounds) {
                    self.set(name, Value::Number(max.1))?;
                }
            }
            let mut set = |name, v| match self.set(name, Value::Number(v))? {
                Some(Value::Number(set)) => Ok::<_, ScanError>(set),
                _ => Ok(v),
            };
            let left = set(names::TL_X, left)?;
            let top = set(names::TL_Y, top)?;
            let right = set(names::BR_X, right)?;
            let bottom = set(names::BR_Y, bottom)?;
            return Ok(ScanArea {
                x_mm: (left - x0) as f32,
                y_mm: (top - y0) as f32,
                width_mm: (right - left) as f32,
                height_mm: (bottom - top) as f32,
            });
        }
        Ok(area)
    }
}

impl ScannerDevice for SaneDevice {
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
        let (src, area) = request.validate(&self.caps)?;
        let area = if self
            .setups
            .iter()
            .any(|s| s.source == request.source && s.origin.is_some())
        {
            area
        } else {
            src.max_area
        };
        let area = self.configure(request, area)?;
        let expected_lines = area.pixel_size(request.dpi).1 as usize;
        let image = read_image(self.driver.as_mut(), expected_lines, progress);
        // SANE wants a cancel after every scan, finished or not.
        self.driver.cancel();
        progress.progress(1.0);
        Ok(ScanResult {
            source: request.source,
            dpi: request.dpi,
            area,
            passes: vec![ScanPass {
                exposure: Exposure::default(),
                image: image?,
            }],
            infrared: None,
            gamma: GAMMA,
        })
    }
}

/// One frame's samples, without row padding.
struct Plane {
    format: Frame,
    width: usize,
    height: usize,
    data: Vec<u8>,
}

/// Read every frame of a started scan and assemble them into one image.
fn read_image(
    driver: &mut dyn Driver,
    expected_lines: usize,
    progress: &mut dyn ProgressSink,
) -> Result<DynamicImage, ScanError> {
    let mut planes = Vec::new();
    let mut depth = 0;
    let mut buf = vec![0; CHUNK];
    loop {
        driver.start()?;
        let p = driver.parameters()?;
        if p.depth != 8 && p.depth != 16 {
            return Err(ScanError::Unsupported(format!(
                "{}-bit SANE frames",
                p.depth
            )));
        }
        if depth != 0 && p.depth != depth {
            return Err(ScanError::Backend("SANE frames differ in depth".into()));
        }
        depth = p.depth;
        let channels = if p.format == Frame::Rgb { 3 } else { 1 };
        let row = p.pixels_per_line * channels * depth as usize / 8;
        if row == 0 || p.bytes_per_line < row {
            return Err(ScanError::Backend(format!(
                "SANE frame of {} bytes per line can't hold {} pixels",
                p.bytes_per_line, p.pixels_per_line
            )));
        }
        let frames = if matches!(p.format, Frame::Gray | Frame::Rgb) {
            1
        } else {
            3
        };
        let lines = p.lines.unwrap_or(expected_lines).max(1);
        let total = (p.bytes_per_line * lines) as f32;

        let mut data = Vec::with_capacity(p.bytes_per_line * lines);
        while let Some(n) = driver.read(&mut buf)? {
            if progress.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            data.extend_from_slice(&buf[..n]);
            let frame = (data.len() as f32 / total).min(1.0);
            progress.progress((planes.len() as f32 + frame) / frames as f32 * 0.99);
        }

        let mut height = data.len() / p.bytes_per_line;
        if let Some(lines) = p.lines {
            height = height.min(lines);
        }
        let packed = data
            .chunks_exact(p.bytes_per_line)
            .take(height)
            .flat_map(|line| &line[..row])
            .copied()
            .collect();
        planes.push(Plane {
            format: p.format,
            width: p.pixels_per_line,
            height,
            data: packed,
        });
        if p.last_frame || frames == 1 || planes.len() == 3 {
            break;
        }
    }
    assemble(planes, depth)
}

fn assemble(planes: Vec<Plane>, depth: u32) -> Result<DynamicImage, ScanError> {
    let no_data = || ScanError::Backend("SANE sent no image data".into());
    let first = planes.first().ok_or_else(no_data)?;
    let (width, mut height) = (first.width, first.height);
    let (channels, data) = match first.format {
        Frame::Gray | Frame::Rgb => {
            let channels = if first.format == Frame::Rgb { 3 } else { 1 };
            (
                channels,
                planes.into_iter().next().ok_or_else(no_data)?.data,
            )
        }
        _ => {
            let plane = |format| {
                planes
                    .iter()
                    .find(|p| p.format == format && p.width == width)
                    .ok_or_else(|| ScanError::Backend(format!("SANE sent no {format:?} frame")))
            };
            let rgb = [
                plane(Frame::Red)?,
                plane(Frame::Green)?,
                plane(Frame::Blue)?,
            ];
            height = rgb.iter().map(|p| p.height).min().unwrap_or(0);
            let bytes = depth as usize / 8;
            let mut data = Vec::with_capacity(width * height * 3 * bytes);
            for i in 0..width * height {
                for p in rgb {
                    data.extend_from_slice(&p.data[i * bytes..(i + 1) * bytes]);
                }
            }
            (3, data)
        }
    };
    if height == 0 {
        return Err(no_data());
    }
    let (w, h) = (width as u32, height as u32);
    let bad = || ScanError::Backend("SANE frame size doesn't match its data".into());
    Ok(match (depth, channels) {
        (8, 1) => DynamicImage::ImageLuma8(
            ImageBuffer::<Luma<u8>, _>::from_raw(w, h, data).ok_or_else(bad)?,
        ),
        (8, _) => DynamicImage::ImageRgb8(
            ImageBuffer::<Rgb<u8>, _>::from_raw(w, h, data).ok_or_else(bad)?,
        ),
        // 16-bit samples arrive in the host's byte order.
        (_, c) => {
            let samples = data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| u16::from_ne_bytes(b))
                .collect();
            if c == 1 {
                DynamicImage::ImageLuma16(ImageBuffer::from_raw(w, h, samples).ok_or_else(bad)?)
            } else {
                DynamicImage::ImageRgb16(ImageBuffer::from_raw(w, h, samples).ok_or_else(bad)?)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{Fake, expected_pixel};
    use scan_core::NoProgress;

    fn info() -> DeviceInfo {
        DeviceInfo {
            id: DeviceId("fake:0".into()),
            manufacturer: "Fake".into(),
            model: "Film Scanner".into(),
            backend: BACKEND_NAME,
        }
    }

    fn open(fake: Fake) -> SaneDevice {
        SaneDevice::new(info(), Box::new(fake)).unwrap()
    }

    #[test]
    fn classifies_sources_and_modes() {
        let sources = [
            ("Flatbed", Some(ScanSource::Flatbed)),
            ("Normal", Some(ScanSource::Flatbed)),
            ("Transparency Unit", Some(ScanSource::Transparency)),
            ("TPU8x10", Some(ScanSource::Transparency)),
            ("Film", Some(ScanSource::Transparency)),
            ("Negative", Some(ScanSource::Transparency)),
            ("Automatic Document Feeder", None),
            ("ADF Duplex", None),
            ("Transparency Adapter Infrared", None),
        ];
        for (name, source) in sources {
            assert_eq!(classify_source(name), source, "{name}");
        }
        assert_eq!(classify_mode("Color"), Some(ColorMode::Color));
        assert_eq!(classify_mode("Gray"), Some(ColorMode::Gray));
        assert_eq!(classify_mode("Lineart"), None);
        assert_eq!(classify_mode("Color+Infrared"), None);
    }

    #[test]
    fn capabilities_per_source() {
        let device = open(Fake::new());
        let caps = device.capabilities();
        assert_eq!(caps.sources.len(), 2, "the feeder is skipped");

        let flatbed = caps.source(ScanSource::Flatbed).unwrap();
        assert_eq!(flatbed.resolutions, [75, 150, 300, 600, 1200]);
        assert_eq!(flatbed.bit_depths, [BitDepth::Eight, BitDepth::Sixteen]);
        assert_eq!(flatbed.color_modes, [ColorMode::Color, ColorMode::Gray]);
        assert_eq!(flatbed.max_area, ScanArea::full(216.0, 297.0));
        assert!(!flatbed.infrared);
        assert_eq!(flatbed.max_exposure_passes, 1);

        // The film source reloads the options with its own range and geometry.
        let film = caps.source(ScanSource::Transparency).unwrap();
        assert_eq!(
            film.resolutions,
            [400, 800, 1200, 1600, 2400, 3200, 4800, 6400]
        );
        assert_eq!(film.max_area, ScanArea::full(50.0, 220.0));
        assert_eq!(device.setups[1].origin, Some((80.0, 20.0)));
    }

    #[test]
    fn scans_an_area_of_film() {
        let fake = Fake::new();
        let log = fake.log();
        let mut device = open(fake);
        let area = ScanArea {
            x_mm: 5.0,
            y_mm: 10.0,
            width_mm: 25.4,
            height_mm: 12.7,
        };
        let request = ScanRequest {
            area: Some(area),
            ..ScanRequest::new(ScanSource::Transparency, 400)
        };
        let result = device.scan(&request, &mut NoProgress).unwrap();
        assert_eq!(result.dimensions(), (400, 200));
        assert_eq!(result.area, area);
        assert_eq!(result.gamma, GAMMA);

        let image = result.primary().as_rgb16().unwrap();
        for (x, y, px) in image.enumerate_pixels().step_by(97) {
            for c in 0..3 {
                assert_eq!(px[c], expected_pixel(x, y, c, 16));
            }
        }
        let log = log.lock().unwrap();
        for set in [
            "source=Transparency Unit",
            "mode=Color",
            "depth=16",
            "resolution=400",
            "tl-x=85",
            "tl-y=30",
            "br-x=110.4",
            "br-y=42.7",
        ] {
            assert!(log.iter().any(|l| l == set), "{set} not in {log:?}");
        }
        assert_eq!(log.last().map(String::as_str), Some("cancel"));
    }

    #[test]
    fn three_pass_frames_with_unknown_length_and_padding() {
        let mut fake = Fake::new();
        fake.three_pass = true;
        fake.unknown_lines = true;
        fake.padding = 3;
        let mut device = open(fake);
        let request = ScanRequest {
            bit_depth: BitDepth::Eight,
            area: Some(ScanArea::full(10.0, 20.0)),
            ..ScanRequest::new(ScanSource::Flatbed, 150)
        };
        let result = device.scan(&request, &mut NoProgress).unwrap();
        assert_eq!(result.dimensions(), (59, 118));
        let image = result.primary().as_rgb8().unwrap();
        for (x, y, px) in image.enumerate_pixels() {
            for c in 0..3 {
                assert_eq!(px[c] as u16, expected_pixel(x, y, c, 8));
            }
        }
    }

    #[test]
    fn greyscale_scan() {
        let mut device = open(Fake::new());
        let request = ScanRequest {
            color_mode: ColorMode::Gray,
            area: Some(ScanArea::full(10.0, 10.0)),
            ..ScanRequest::new(ScanSource::Flatbed, 300)
        };
        let result = device.scan(&request, &mut NoProgress).unwrap();
        let image = result.primary().as_luma16().unwrap();
        assert_eq!(image.dimensions(), (118, 118));
        assert_eq!(image.get_pixel(17, 33)[0], expected_pixel(17, 33, 0, 16));
    }

    #[test]
    fn cancelling_stops_the_scan() {
        struct CancelSoon(usize);
        impl ProgressSink for CancelSoon {
            fn progress(&mut self, _: f32) {
                self.0 += 1;
            }
            fn is_cancelled(&self) -> bool {
                self.0 >= 2
            }
        }
        let fake = Fake::new();
        let log = fake.log();
        let mut device = open(fake);
        let request = ScanRequest {
            area: Some(ScanArea::full(50.0, 50.0)),
            ..ScanRequest::new(ScanSource::Flatbed, 300)
        };
        let result = device.scan(&request, &mut CancelSoon(0));
        assert!(matches!(result, Err(ScanError::Cancelled)));
        assert_eq!(
            log.lock().unwrap().last().map(String::as_str),
            Some("cancel")
        );
    }

    #[test]
    fn rejects_unusable_devices() {
        let mut fake = Fake::new();
        fake.sources = vec!["Automatic Document Feeder".into()];
        let err = SaneDevice::new(info(), Box::new(fake)).err().unwrap();
        assert!(matches!(err, ScanError::Unsupported(_)));
    }
}
