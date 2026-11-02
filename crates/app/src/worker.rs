//! Background thread that owns scanner access, so device I/O never blocks the UI.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use scan_core::image::{DynamicImage, ImageFormat};
use scan_core::{
    CancelToken, Capabilities, DeviceInfo, DeviceRegistry, ProgressSink, ScanError, ScanRequest,
    ScanResult, ScannerDevice, export,
};
use scan_film::Inverter;
use scan_process::{
    DetectParams, DustParams, DustRemoval, ExposureMerge, FrameDetection, FrameRect,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanKind {
    Preview,
    Final,
}

impl ScanKind {
    pub fn label(self) -> &'static str {
        match self {
            ScanKind::Preview => "Preview",
            ScanKind::Final => "Scan",
        }
    }
}

#[derive(Debug)]
pub enum Command {
    ListDevices,
    Open(DeviceInfo),
    Scan {
        device: DeviceInfo,
        request: ScanRequest,
        kind: ScanKind,
    },
    /// Merge the result's exposure passes.
    MergeExposures {
        /// The result's generation, handed back with the outcome.
        generation: u64,
        result: Arc<ScanResult>,
    },
    /// Find and fill dust in the result's primary image, or in `merged` when given, using its
    /// infrared channel.
    RemoveDust {
        /// The result's generation, handed back with the outcome.
        generation: u64,
        strength: f32,
        merged: Option<Arc<ExposureMerge>>,
        result: Arc<ScanResult>,
    },
    /// Find the frames on the result's primary pass, and its film base.
    DetectFrames {
        /// The result's generation, handed back with the outcome.
        generation: u64,
        result: Arc<ScanResult>,
    },
    Save {
        result: Arc<ScanResult>,
        /// Saved in place of the primary pass when present, unless `cleaned` is.
        merged: Option<Arc<ExposureMerge>>,
        /// Saved in place of the primary pass when present.
        cleaned: Option<Arc<DustRemoval>>,
        /// Turns the image saved for the primary pass into a positive, when given.
        invert: Option<Arc<Inverter>>,
        /// Clockwise, applied to every image saved.
        quarter_turns: u8,
        path: PathBuf,
    },
    /// Save `frames` cut out of the primary image (or of `merged` or `cleaned`, as for
    /// [`Command::Save`]), straightened, inverted (when `invert` is given) and turned, each
    /// to a file numbered after `path`.
    SaveFrames {
        result: Arc<ScanResult>,
        merged: Option<Arc<ExposureMerge>>,
        cleaned: Option<Arc<DustRemoval>>,
        invert: Option<Arc<Inverter>>,
        frames: Vec<FrameRect>,
        quarter_turns: u8,
        path: PathBuf,
    },
}

#[derive(Debug)]
pub enum Event {
    Devices {
        devices: Vec<DeviceInfo>,
        errors: Vec<String>,
    },
    Opened(Result<(DeviceInfo, Capabilities), String>),
    Progress(f32),
    ScanDone {
        kind: ScanKind,
        result: Result<ScanResult, ScanError>,
    },
    ExposuresMerged {
        generation: u64,
        result: Result<ExposureMerge, String>,
    },
    DustRemoved {
        generation: u64,
        strength: f32,
        /// Whether the merged exposures were cleaned.
        merged: bool,
        result: Result<DustRemoval, String>,
    },
    FramesDetected {
        generation: u64,
        detection: FrameDetection,
        /// A guess at the film base from the clearest film, when the detection found no
        /// strips to measure it on.
        estimated_base: Option<[f32; 3]>,
    },
    Saved(Result<Vec<PathBuf>, String>),
}

/// Handle to the worker thread. Dropping it stops the thread once its current job finishes.
pub struct Worker {
    commands: Sender<Command>,
    events: Receiver<Event>,
    cancel: CancelToken,
}

impl Worker {
    /// Start the worker. `notify` is called after every event is sent, e.g. to wake the UI.
    pub fn spawn(registry: DeviceRegistry, notify: impl Fn() + Send + 'static) -> Self {
        let (commands, command_rx) = channel();
        let (event_tx, events) = channel();
        let cancel = CancelToken::new();
        let token = cancel.clone();
        thread::Builder::new()
            .name("scanner-worker".into())
            .spawn(move || run(registry, command_rx, event_tx, token, notify))
            .expect("spawn scanner worker");
        Self {
            commands,
            events,
            cancel,
        }
    }

    pub fn send(&self, command: Command) {
        // If the worker died, events stop arriving; the UI stays usable.
        let _ = self.commands.send(command);
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Ask the running scan to stop.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

struct ChannelProgress<'a> {
    events: &'a Sender<Event>,
    notify: &'a dyn Fn(),
    cancel: &'a CancelToken,
    last: f32,
}

impl ProgressSink for ChannelProgress<'_> {
    fn progress(&mut self, fraction: f32) {
        // Throttle to whole percents to avoid flooding the UI.
        if (fraction - self.last).abs() >= 0.01 || fraction >= 1.0 {
            self.last = fraction;
            let _ = self.events.send(Event::Progress(fraction));
            (self.notify)();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

fn run(
    registry: DeviceRegistry,
    commands: Receiver<Command>,
    events: Sender<Event>,
    cancel: CancelToken,
    notify: impl Fn(),
) {
    let mut device: Option<Box<dyn ScannerDevice>> = None;
    let send = |event: Event| {
        let _ = events.send(event);
        notify();
    };

    for command in commands {
        match command {
            Command::ListDevices => {
                let listing = registry.list_devices();
                let errors = listing
                    .errors
                    .iter()
                    .map(|(b, e)| format!("{b}: {e}"))
                    .collect();
                send(Event::Devices {
                    devices: listing.devices,
                    errors,
                });
            }
            Command::Open(info) => {
                device = None;
                let result = registry.open(&info).map(|dev| {
                    let caps = dev.capabilities().clone();
                    device = Some(dev);
                    (info, caps)
                });
                send(Event::Opened(result.map_err(|e| e.to_string())));
            }
            Command::Scan {
                device: info,
                request,
                kind,
            } => {
                cancel.reset();
                let result = match device.as_mut().filter(|d| *d.info() == info) {
                    Some(dev) => {
                        let mut progress = ChannelProgress {
                            events: &events,
                            notify: &notify,
                            cancel: &cancel,
                            last: -1.0,
                        };
                        dev.scan(&request, &mut progress)
                    }
                    None => Err(ScanError::DeviceNotFound(format!(
                        "{} is not open",
                        info.model
                    ))),
                };
                send(Event::ScanDone { kind, result });
            }
            Command::MergeExposures { generation, result } => {
                let merged = scan_process::merge_exposures(&result.passes, result.gamma)
                    .map_err(|e| e.to_string());
                send(Event::ExposuresMerged {
                    generation,
                    result: merged,
                });
            }
            Command::RemoveDust {
                generation,
                strength,
                merged,
                result,
            } => {
                let image = merged.as_ref().map_or(result.primary(), |m| &m.image);
                let removed = match &result.infrared {
                    Some(ir) => {
                        let params = DustParams::for_scan(result.dpi, strength);
                        scan_process::remove_dust(image, ir, &params).map_err(|e| e.to_string())
                    }
                    None => Err("the scan has no infrared channel".into()),
                };
                send(Event::DustRemoved {
                    generation,
                    strength,
                    merged: merged.is_some(),
                    result: removed,
                });
            }
            Command::DetectFrames { generation, result } => {
                let detection = scan_process::detect_frames(
                    result.primary(),
                    result.gamma,
                    result.dpi,
                    result.area,
                    &DetectParams::default(),
                );
                let estimated_base = match detection.base {
                    Some(_) => None,
                    None => scan_process::estimate_base(result.primary(), result.gamma),
                };
                send(Event::FramesDetected {
                    generation,
                    detection,
                    estimated_base,
                });
            }
            Command::Save {
                result,
                merged,
                cleaned,
                invert,
                quarter_turns,
                path,
            } => {
                let primary = processed(&result, &merged, &cleaned);
                let positive = invert.map(|inv| positive(primary, result.gamma, &inv));
                let primary = positive.as_ref().unwrap_or(primary);
                let saved = export::save_tiff_with_primary(&result, primary, quarter_turns, &path)
                    .map_err(|e| e.to_string());
                send(Event::Saved(saved));
            }
            Command::SaveFrames {
                result,
                merged,
                cleaned,
                invert,
                frames,
                quarter_turns,
                path,
            } => {
                let image = processed(&result, &merged, &cleaned);
                let saved = frames
                    .iter()
                    .enumerate()
                    .map(|(n, frame)| {
                        let mut crop =
                            scan_process::crop_frame(image, result.dpi, result.area, frame);
                        if let Some(inv) = &invert {
                            crop = positive(&crop, result.gamma, inv);
                        }
                        let target = export::numbered(&path, n);
                        export::rotated(&crop, quarter_turns)
                            .save_with_format(&target, ImageFormat::Tiff)
                            .map(|()| target)
                            .map_err(|e| e.to_string())
                    })
                    .collect();
                send(Event::Saved(saved));
            }
        }
    }
}

/// The positive of `image`, a scan of a negative encoding light with `gamma`.
fn positive(image: &DynamicImage, gamma: f32, inverter: &Inverter) -> DynamicImage {
    DynamicImage::ImageRgb16(scan_process::invert(image, gamma, inverter))
}

/// The image to save: the dust-cleaned one, else the merged exposures, else the scan's
/// primary pass.
fn processed<'a>(
    result: &'a ScanResult,
    merged: &'a Option<Arc<ExposureMerge>>,
    cleaned: &'a Option<Arc<DustRemoval>>,
) -> &'a DynamicImage {
    match (cleaned, merged) {
        (Some(c), _) => &c.image,
        (None, Some(m)) => &m.image,
        (None, None) => result.primary(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use scan_core::mock::MockBackend;
    use scan_core::{Exposure, NoProgress, ScanSource, ScannerBackend};

    fn next(worker: &Worker) -> Event {
        // Skip progress events; wait for the response to the command.
        loop {
            let event = worker
                .events
                .recv_timeout(Duration::from_secs(30))
                .expect("worker event");
            if !matches!(event, Event::Progress(_)) {
                return event;
            }
        }
    }

    #[test]
    fn lists_opens_scans_and_saves_with_mock_backend() {
        let worker = Worker::spawn(DeviceRegistry::new().with_backend(MockBackend), || {});

        worker.send(Command::ListDevices);
        let Event::Devices { devices, errors } = next(&worker) else {
            panic!("expected devices")
        };
        assert_eq!(devices, vec![MockBackend::device_info()]);
        assert!(errors.is_empty());

        worker.send(Command::Open(devices[0].clone()));
        let Event::Opened(Ok((_, caps))) = next(&worker) else {
            panic!("expected open")
        };
        assert_eq!(caps, MockBackend::capabilities());

        let request = ScanRequest::new(ScanSource::Transparency, 75);
        worker.send(Command::Scan {
            device: devices[0].clone(),
            request,
            kind: ScanKind::Final,
        });
        let Event::ScanDone {
            result: Ok(result), ..
        } = next(&worker)
        else {
            panic!("expected scan")
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("strip.tif");
        worker.send(Command::Save {
            result: Arc::new(result),
            merged: None,
            cleaned: None,
            invert: None,
            quarter_turns: 0,
            path: path.clone(),
        });
        let Event::Saved(Ok(paths)) = next(&worker) else {
            panic!("expected save")
        };
        assert_eq!(paths, vec![path.clone()]);
        assert!(path.exists());
    }

    #[test]
    fn detects_and_saves_frames() {
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let request = ScanRequest::new(ScanSource::Transparency, 75);
        let result = Arc::new(dev.scan(&request, &mut NoProgress).unwrap());

        let worker = Worker::spawn(DeviceRegistry::new(), || {});
        worker.send(Command::DetectFrames {
            generation: 5,
            result: result.clone(),
        });
        let Event::FramesDetected {
            generation: 5,
            detection,
            estimated_base: None,
        } = next(&worker)
        else {
            panic!("expected frames")
        };
        assert_eq!((detection.frames.len(), detection.strips), (12, 2));
        let base = detection.base.expect("the film base");

        let dir = tempfile::tempdir().unwrap();
        let frames: Vec<_> = detection.frames[..2].iter().map(|f| f.rect).collect();
        let stock = scan_film::stock::default_stock();
        let inverter = Inverter::new(stock.model(), base, &Default::default());
        worker.send(Command::SaveFrames {
            result,
            merged: None,
            cleaned: None,
            invert: Some(Arc::new(inverter)),
            frames: frames.clone(),
            quarter_turns: 1,
            path: dir.path().join("roll.tif"),
        });
        let Event::Saved(Ok(paths)) = next(&worker) else {
            panic!("expected frames saved")
        };
        let names: Vec<_> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["roll_01.tif", "roll_02.tif"]);
        // Frames down the strip are portrait; turned a quarter, landscape.
        let saved = scan_core::image::open(&paths[0]).unwrap();
        assert!(matches!(saved, DynamicImage::ImageRgb16(_)));
        let (w, h) = (frames[0].width_mm, frames[0].height_mm);
        let px = |mm: f32| (mm / scan_core::MM_PER_INCH * 75.0).round() as u32;
        assert_eq!((saved.width(), saved.height()), (px(h), px(w)));
    }

    #[test]
    fn removes_dust_using_the_infrared_channel() {
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let mut request = ScanRequest::new(ScanSource::Transparency, 150);
        request.infrared = true;
        let with_ir = Arc::new(dev.scan(&request, &mut NoProgress).unwrap());
        request.infrared = false;
        let without = Arc::new(dev.scan(&request, &mut NoProgress).unwrap());

        let worker = Worker::spawn(DeviceRegistry::new(), || {});
        for (generation, result) in [(7, with_ir), (8, without)] {
            worker.send(Command::RemoveDust {
                generation,
                strength: 0.5,
                merged: None,
                result,
            });
        }
        let Event::DustRemoved {
            generation: 7,
            result: Ok(removal),
            ..
        } = next(&worker)
        else {
            panic!("expected dust removal")
        };
        assert!(removal.report.defect_pixels > 0);
        let Event::DustRemoved {
            generation: 8,
            result: Err(_),
            ..
        } = next(&worker)
        else {
            panic!("expected an error without infrared")
        };
    }

    #[test]
    fn merges_exposure_passes() {
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let mut request = ScanRequest::new(ScanSource::Transparency, 75);
        let single = Arc::new(dev.scan(&request, &mut NoProgress).unwrap());
        request.exposures = vec![Exposure::default(), Exposure { ev: 1.0 }];
        let double = Arc::new(dev.scan(&request, &mut NoProgress).unwrap());

        let worker = Worker::spawn(DeviceRegistry::new(), || {});
        for (generation, result) in [(3, double), (4, single)] {
            worker.send(Command::MergeExposures { generation, result });
        }
        let Event::ExposuresMerged {
            generation: 3,
            result: Ok(merge),
        } = next(&worker)
        else {
            panic!("expected a merge")
        };
        assert_eq!(merge.passes.len(), 1);
        let Event::ExposuresMerged {
            generation: 4,
            result: Err(_),
        } = next(&worker)
        else {
            panic!("expected an error with one pass")
        };
    }

    #[test]
    fn scanning_an_unopened_device_fails() {
        let worker = Worker::spawn(DeviceRegistry::new().with_backend(MockBackend), || {});
        let request = ScanRequest::new(ScanSource::Transparency, 75);
        worker.send(Command::Scan {
            device: MockBackend::device_info(),
            request,
            kind: ScanKind::Preview,
        });
        let Event::ScanDone { result, .. } = next(&worker) else {
            panic!("expected scan result")
        };
        assert!(matches!(result, Err(ScanError::DeviceNotFound(_))));
    }
}
