//! The V500 as a [`scan_core`] backend: native flatbed and film scans over usbscan, generated
//! by [`crate::session`] and [`crate::film`].
//!
//! Each scan opens the scanner, loads Epson's firmware if it was just powered on (from the
//! user's Epson Scan installation, see [`crate::firmware`]) and, on Windows, keeps the WIA
//! service stopped for the scan's length ([`crate::system`]).

use std::time::Duration;

use scan_core::image::DynamicImage;
use scan_core::{
    BitDepth, Capabilities, ColorMode, DeviceId, DeviceInfo, Exposure, ProgressSink, ScanArea,
    ScanError, ScanPass, ScanRequest, ScanResult, ScanSource, ScannerBackend, ScannerDevice,
    SourceCapabilities,
};

use crate::decode;
use crate::film::{self, FilmSession};
use crate::session::FlatbedSession;
use crate::system::WiaPause;
use crate::transport::Transport;

pub const BACKEND_NAME: &str = "epson";
/// Resolutions offered; native flatbed sessions go up to 400 dpi so far.
const RESOLUTIONS: [u32; 7] = [50, 75, 100, 150, 200, 300, 400];
/// Film resolutions: read at 400, 800 or 1,600 dpi and downsampled.
const FILM_RESOLUTIONS: [u32; 8] = [100, 200, 300, 400, 600, 800, 1200, 1600];
/// Film exposures beyond the calibrated one reach at most twice it.
const MAX_FILM_EV: f32 = 1.0;
/// The bed: 8.5 × 11.7 in, as Epson's WIA driver reports it.
const BED: ScanArea = ScanArea {
    x_mm: 0.0,
    y_mm: 0.0,
    width_mm: 215.9,
    height_mm: 297.18,
};
const POLL_DELAY: Duration = Duration::from_millis(25);

/// Opens a connection to the scanner for one scan.
pub type Connect = Box<dyn FnMut() -> Result<Box<dyn Transport>, ScanError> + Send>;

/// Native backend for the Perfection V500.
#[derive(Debug, Default)]
pub struct EpsonBackend;

impl ScannerBackend for EpsonBackend {
    fn name(&self) -> &'static str {
        BACKEND_NAME
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>, ScanError> {
        Ok(usb::find_v500s()
            .into_iter()
            .map(|path| DeviceInfo {
                id: DeviceId(path),
                manufacturer: "EPSON".into(),
                model: "Perfection V500".into(),
                backend: BACKEND_NAME,
            })
            .collect())
    }

    fn open(&self, device: &DeviceInfo) -> Result<Box<dyn ScannerDevice>, ScanError> {
        let path = device.id.0.clone();
        Ok(Box::new(V500Device::new(
            device.clone(),
            Box::new(move || usb::connect(&path)),
            true,
        )))
    }
}

/// An opened V500.
pub struct V500Device {
    info: DeviceInfo,
    caps: Capabilities,
    connect: Connect,
    pause_wia: bool,
    poll_delay: Duration,
}

impl V500Device {
    /// A device reached through `connect`. With `pause_wia`, the WIA service is stopped while
    /// each scan runs.
    pub fn new(info: DeviceInfo, connect: Connect, pause_wia: bool) -> Self {
        Self {
            info,
            caps: capabilities(),
            connect,
            pause_wia,
            poll_delay: POLL_DELAY,
        }
    }

    /// Don't wait between status polls (for the simulator).
    pub fn without_poll_delay(mut self) -> Self {
        self.poll_delay = Duration::ZERO;
        self
    }
}

fn capabilities() -> Capabilities {
    Capabilities {
        sources: vec![
            SourceCapabilities {
                source: ScanSource::Flatbed,
                resolutions: RESOLUTIONS.to_vec(),
                bit_depths: vec![BitDepth::Eight, BitDepth::Sixteen],
                color_modes: vec![ColorMode::Color],
                infrared: false,
                max_exposure_passes: 1,
                max_area: BED,
            },
            SourceCapabilities {
                source: ScanSource::Transparency,
                resolutions: FILM_RESOLUTIONS.to_vec(),
                bit_depths: vec![BitDepth::Eight, BitDepth::Sixteen],
                color_modes: vec![ColorMode::Color],
                infrared: true,
                max_exposure_passes: 2,
                max_area: film::FILM_AREA,
            },
        ],
    }
}

/// A 16-bit read as an image of the requested depth.
fn image(rgb16: decode::Rgb16Image, bit_depth: BitDepth) -> DynamicImage {
    let image = DynamicImage::ImageRgb16(rgb16);
    match bit_depth {
        BitDepth::Eight => DynamicImage::ImageRgb8(image.to_rgb8()),
        BitDepth::Sixteen => image,
    }
}

impl ScannerDevice for V500Device {
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
        if request.source == ScanSource::Transparency {
            return self.scan_film(request, area, progress);
        }
        let session = FlatbedSession::new(&area, request.dpi, request.bit_depth)?;
        progress.progress(0.0);
        let _wia = self.pause_wia.then(WiaPause::new).transpose()?;
        let mut dev = (self.connect)()?;
        let raw = session.run_with(dev.as_mut(), self.poll_delay, progress)?;
        drop(dev);
        let (layout, plan) = (session.layout, &session.plan);
        let image = match request.bit_depth {
            BitDepth::Eight => DynamicImage::ImageRgb8(decode::flatbed_rgb8(&raw, layout, plan)),
            BitDepth::Sixteen => {
                DynamicImage::ImageRgb16(decode::flatbed_rgb16(&raw, layout, plan))
            }
        };
        progress.progress(1.0);
        Ok(ScanResult {
            source: ScanSource::Flatbed,
            dpi: request.dpi,
            area,
            passes: vec![ScanPass {
                exposure: Exposure::default(),
                image,
            }],
            infrared: None,
        })
    }
}

impl V500Device {
    /// One film session per exposure pass, as SilverFast runs them, then one for infrared if
    /// asked.
    fn scan_film(
        &mut self,
        request: &ScanRequest,
        area: ScanArea,
        progress: &mut dyn ProgressSink,
    ) -> Result<ScanResult, ScanError> {
        let exposures = request.effective_exposures();
        if let Some(e) = exposures
            .iter()
            .find(|e| !(0.0..=MAX_FILM_EV).contains(&e.ev))
        {
            return Err(ScanError::InvalidRequest(format!(
                "film exposures go from 0 to +{MAX_FILM_EV} EV, not {:+} EV",
                e.ev
            )));
        }
        let mut sessions = exposures
            .iter()
            .map(|e| Ok(FilmSession::new(&area, request.dpi)?.with_exposure(e.ev)))
            .collect::<Result<Vec<_>, ScanError>>()?;
        if request.infrared {
            sessions.push(FilmSession::infrared(&area, request.dpi)?);
        }
        progress.progress(0.0);
        let _wia = self.pause_wia.then(WiaPause::new).transpose()?;
        let mut passes = Vec::new();
        let mut infrared = None;
        let n = sessions.len() as f32;
        for (i, session) in sessions.iter().enumerate() {
            let mut dev = (self.connect)()?;
            let mut part = Part {
                inner: &mut *progress,
                offset: i as f32 / n,
                scale: 1.0 / n,
            };
            let raw = session.run_with(dev.as_mut(), self.poll_delay, &mut part)?;
            drop(dev);
            let output = session.plan.output;
            match exposures.get(i) {
                Some(exposure) => passes.push(ScanPass {
                    exposure: *exposure,
                    image: image(
                        decode::rgb16(&raw, session.layout, output),
                        request.bit_depth,
                    ),
                }),
                None => infrared = Some(session.decode_infrared(&raw, &area)),
            }
        }
        progress.progress(1.0);
        Ok(ScanResult {
            source: ScanSource::Transparency,
            dpi: request.dpi,
            area,
            passes,
            infrared,
        })
    }
}

/// Progress of one part of a scan, as a share of the whole.
struct Part<'a> {
    inner: &'a mut dyn ProgressSink,
    offset: f32,
    scale: f32,
}

impl ProgressSink for Part<'_> {
    fn progress(&mut self, fraction: f32) {
        self.inner.progress(self.offset + self.scale * fraction);
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}

#[cfg(windows)]
mod usb {
    use std::time::Duration;

    use scan_core::ScanError;

    use crate::firmware::{self, Firmware};
    use crate::transport::{Transport, UsbScan, find_usbscan_devices};
    use crate::{EPSON_VID, V500_PID};

    pub fn find_v500s() -> Vec<String> {
        find_usbscan_devices()
            .into_iter()
            .filter(|d| d.usb_id() == Some((EPSON_VID, V500_PID)))
            .map(|d| d.path)
            .collect()
    }

    /// Open the scanner, loading its firmware first if it was just powered on.
    pub fn connect(path: &str) -> Result<Box<dyn Transport>, ScanError> {
        let mut dev = UsbScan::open(path)?;
        dev.set_timeouts(5, 5)?;
        if firmware::needs_firmware(&mut dev)? {
            let file = firmware::V500_FIRMWARE_PATH;
            let image = std::fs::read(file).map_err(|e| {
                ScanError::Backend(format!(
                    "the scanner needs Epson's firmware after power-on, from Epson Scan's \
                     {file}: {e}"
                ))
            })?;
            log::info!("loading the scanner's firmware from {file}");
            firmware::upload(&mut dev, &Firmware::parse(&image)?, Duration::from_secs(60))?;
        }
        dev.set_timeouts(30, 30)?;
        Ok(Box::new(dev))
    }
}

#[cfg(not(windows))]
mod usb {
    use scan_core::ScanError;

    use crate::transport::Transport;

    pub fn find_v500s() -> Vec<String> {
        Vec::new()
    }

    pub fn connect(_: &str) -> Result<Box<dyn Transport>, ScanError> {
        Err(ScanError::Unsupported(
            "native Epson scanning supports only Windows so far".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::SimulatedScanner;
    use scan_core::{CancelToken, NoProgress};

    fn simulated() -> V500Device {
        let info = DeviceInfo {
            id: DeviceId("sim".into()),
            manufacturer: "EPSON".into(),
            model: "Perfection V500".into(),
            backend: BACKEND_NAME,
        };
        V500Device::new(
            info,
            Box::new(|| Ok(Box::new(SimulatedScanner::default()))),
            false,
        )
        .without_poll_delay()
    }

    fn request(dpi: u32) -> ScanRequest {
        ScanRequest {
            bit_depth: BitDepth::Eight,
            area: Some(ScanArea {
                x_mm: 10.0,
                y_mm: 40.0,
                width_mm: 50.8,
                height_mm: 25.4,
            }),
            ..ScanRequest::new(ScanSource::Flatbed, dpi)
        }
    }

    struct Recorder {
        fractions: Vec<f32>,
        cancel: CancelToken,
    }

    impl ProgressSink for Recorder {
        fn progress(&mut self, fraction: f32) {
            self.fractions.push(fraction);
        }

        fn is_cancelled(&self) -> bool {
            self.cancel.is_cancelled()
        }
    }

    #[test]
    fn scans_on_simulator() {
        let mut dev = simulated();
        let mut progress = Recorder {
            fractions: Vec::new(),
            cancel: CancelToken::new(),
        };
        let result = dev.scan(&request(300), &mut progress).unwrap();
        assert_eq!(result.dimensions(), (600, 300));
        assert!(matches!(result.primary(), DynamicImage::ImageRgb8(_)));
        assert!(progress.fractions.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(progress.fractions.last(), Some(&1.0));
        // The read reports progress as it goes.
        assert!(progress.fractions.iter().any(|&f| f > 0.2 && f < 0.9));

        let request = ScanRequest {
            bit_depth: BitDepth::Sixteen,
            ..request(150)
        };
        let result = dev.scan(&request, &mut NoProgress).unwrap();
        assert_eq!(result.dimensions(), (300, 150));
        assert!(matches!(result.primary(), DynamicImage::ImageRgb16(_)));
    }

    #[test]
    fn scans_film_on_simulator() {
        let mut dev = simulated();
        let request = ScanRequest {
            area: Some(ScanArea {
                x_mm: 5.0,
                y_mm: 40.0,
                width_mm: 24.0,
                height_mm: 36.0,
            }),
            exposures: vec![Exposure::default(), Exposure { ev: 1.0 }],
            ..ScanRequest::new(ScanSource::Transparency, 800)
        };
        let mut progress = Recorder {
            fractions: Vec::new(),
            cancel: CancelToken::new(),
        };
        let result = dev.scan(&request, &mut progress).unwrap();
        assert_eq!(result.passes.len(), 2);
        assert_eq!(result.dimensions(), (756, 1134));
        assert!(matches!(result.primary(), DynamicImage::ImageRgb16(_)));
        assert!(result.infrared.is_none());
        assert!(progress.fractions.windows(2).all(|w| w[0] <= w[1]));

        // With infrared: a plane the size of the image.
        let request = ScanRequest {
            infrared: true,
            exposures: Vec::new(),
            ..request
        };
        let result = dev.scan(&request, &mut NoProgress).unwrap();
        assert_eq!(result.passes.len(), 1);
        let ir = result.infrared.expect("infrared plane");
        assert_eq!(ir.dimensions(), (756, 1134));

        // A preview of the whole film area, 8-bit.
        let request = ScanRequest {
            bit_depth: BitDepth::Eight,
            ..ScanRequest::new(ScanSource::Transparency, 100)
        };
        let result = dev.scan(&request, &mut NoProgress).unwrap();
        assert_eq!(result.dimensions(), (270, 933));
        assert!(matches!(result.primary(), DynamicImage::ImageRgb8(_)));
    }

    #[test]
    fn cancels_before_calibration() {
        let mut dev = simulated();
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut progress = Recorder {
            fractions: Vec::new(),
            cancel,
        };
        assert!(matches!(
            dev.scan(&request(150), &mut progress),
            Err(ScanError::Cancelled)
        ));
    }

    #[test]
    fn rejects_what_it_cant_scan() {
        let mut dev = simulated();
        for req in [
            // Wider than the film area.
            ScanRequest {
                source: ScanSource::Transparency,
                area: Some(ScanArea {
                    x_mm: 30.0,
                    y_mm: 40.0,
                    width_mm: 50.0,
                    height_mm: 25.0,
                }),
                ..request(300)
            },
            ScanRequest {
                source: ScanSource::Transparency,
                area: None,
                exposures: vec![Exposure { ev: -1.0 }],
                ..request(300)
            },
            request(600),
        ] {
            assert!(matches!(
                dev.scan(&req, &mut NoProgress),
                Err(ScanError::InvalidRequest(_))
            ));
        }
    }
}
