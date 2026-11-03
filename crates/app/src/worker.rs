//! Background thread that owns scanner access, so device I/O never blocks the UI.

use std::borrow::Cow;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use scan_core::image::codecs::jpeg::JpegEncoder;
use scan_core::image::codecs::png::PngEncoder;
use scan_core::image::codecs::tiff::TiffEncoder;
use scan_core::image::{DynamicImage, ImageEncoder, ImageError};
use scan_core::{
    CancelToken, Capabilities, DeviceInfo, DeviceRegistry, MM_PER_INCH, ProgressSink, ScanError,
    ScanRequest, ScanResult, ScannerDevice, export,
};
use scan_process::adjust::{self, Adjustments};
use scan_process::{
    DetectParams, DustParams, DustRemoval, ExposureMerge, FrameDetection, FrameRect,
};

use crate::export::FrameFormat;
use crate::session::Roll;
use crate::state::Film;

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
        /// Inverts the image saved for the primary pass, when given.
        film: Option<Film>,
        /// The whole scan's edits, applied to the image saved for the primary pass (its
        /// orientation is not: the other passes and infrared are turned like it).
        adjust: Box<Adjustments>,
        /// Clockwise, applied to every image saved.
        quarter_turns: u8,
        path: PathBuf,
    },
    /// Save `frames` cut out of the primary image (or of `merged` or `cleaned`, as for
    /// [`Command::Save`]), straightened (with their border and straightening), healed,
    /// inverted (when `film` is given) and edited with their own adjustments, then turned
    /// (`quarter_turns`, then each frame's own), each to its own file in `format`, tagged
    /// sRGB. Then write `metadata`'s text to its file, when given.
    SaveFrames {
        result: Arc<ScanResult>,
        merged: Option<Arc<ExposureMerge>>,
        cleaned: Option<Arc<DustRemoval>>,
        film: Option<Film>,
        frames: Vec<(FrameRect, Adjustments, PathBuf)>,
        quarter_turns: u8,
        format: FrameFormat,
        metadata: Option<(PathBuf, String)>,
    },
    /// Write `roll` to `path`, and `scan` beside it (as scanned) when given.
    SaveRoll {
        scan: Option<Arc<ScanResult>>,
        roll: Box<Roll>,
        path: PathBuf,
        /// The result's generation, handed back with the outcome.
        generation: u64,
    },
    /// Read the roll file at the path, and its scan.
    OpenRoll(PathBuf),
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
    RollSaved {
        path: PathBuf,
        generation: u64,
        result: Result<Vec<PathBuf>, String>,
    },
    RollOpened {
        path: PathBuf,
        result: Result<(Box<Roll>, ScanResult), String>,
    },
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
                film,
                adjust,
                quarter_turns,
                path,
            } => {
                let primary = processed(&result, &merged, &cleaned);
                let px_per_mm = result.dpi as f32 / MM_PER_INCH;
                let spots = adjust::spot_pixels(&adjust.spots, None, &result.area, px_per_mm);
                let primary = adjust::heal(primary, &spots);
                let primary = developed(&primary, result.gamma, film.as_ref(), &adjust);
                let saved = export::save_tiff_with_primary(&result, &primary, quarter_turns, &path)
                    .map_err(|e| e.to_string());
                send(Event::Saved(saved));
            }
            Command::SaveFrames {
                result,
                merged,
                cleaned,
                film,
                frames,
                quarter_turns,
                format,
                metadata,
            } => {
                let image = processed(&result, &merged, &cleaned);
                let px_per_mm = result.dpi as f32 / MM_PER_INCH;
                let saved = frames
                    .into_iter()
                    .map(|(frame, adj, target)| {
                        let cut = adjust::framed(&frame, &adj);
                        let shifts =
                            scan_process::channel_shifts(image, result.dpi, result.area, &cut);
                        let crop = scan_process::crop_frame_aligned(
                            image,
                            result.dpi,
                            result.area,
                            &cut,
                            &shifts,
                        );
                        let spots =
                            adjust::spot_pixels(&adj.spots, Some(&cut), &result.area, px_per_mm);
                        let crop = adjust::heal(&crop, &spots);
                        let crop = developed(&crop, result.gamma, film.as_ref(), &adj);
                        let turns = (quarter_turns + adj.quarter_turns) % 4;
                        save_frame(&adjust::orient(&crop, turns, adj.mirror), format, &target)
                            .map_err(|e| format!("{}: {e}", target.display()))
                            .map(|()| target)
                    })
                    .collect::<Result<Vec<_>, _>>();
                let saved = saved.and_then(|mut written| match metadata {
                    None => Ok(written),
                    Some((path, text)) => match std::fs::write(&path, text) {
                        Ok(()) => {
                            written.push(path);
                            Ok(written)
                        }
                        Err(e) => Err(format!("{}: {e}", path.display())),
                    },
                });
                send(Event::Saved(saved));
            }
            Command::SaveRoll {
                scan,
                roll,
                path,
                generation,
            } => {
                let mut written = Vec::new();
                let saved = scan
                    .map_or(Ok(()), |scan| {
                        export::save_tiff(&scan, &roll.scan_path(&path))
                            .map(|files| written = files)
                            .map_err(|e| e.to_string())
                    })
                    .and_then(|()| {
                        std::fs::write(&path, roll.to_text())
                            .map_err(|e| format!("{}: {e}", path.display()))
                    })
                    .map(|()| {
                        written.insert(0, path.clone());
                        written
                    });
                send(Event::RollSaved {
                    path,
                    generation,
                    result: saved,
                });
            }
            Command::OpenRoll(path) => {
                let result = Roll::open(&path).map(|(roll, scan)| (Box::new(roll), scan));
                send(Event::RollOpened { path, result });
            }
        }
    }
}

/// `image`, a scan encoding light with `gamma`, inverted with `film` (when given) and
/// edited with `adjust` (but not turned): as it is, if that changes nothing.
fn developed<'a>(
    image: &'a DynamicImage,
    gamma: f32,
    film: Option<&Film>,
    adjust: &Adjustments,
) -> Cow<'a, DynamicImage> {
    let inverter = film.map(|f| f.inverter(adjust));
    if inverter.is_none() && adjust.gains() == [1.0; 3] && !adjust.edits_positive() {
        return Cow::Borrowed(image);
    }
    let positive = adjust::develop(image, gamma, inverter.as_ref(), adjust, 1.0);
    Cow::Owned(DynamicImage::ImageRgb16(positive))
}

/// Write `image` (16-bit) to `path` in `format`, as 8 bits for JPEG and PNG, with an sRGB
/// profile.
fn save_frame(image: &DynamicImage, format: FrameFormat, path: &Path) -> Result<(), ImageError> {
    let file = BufWriter::new(File::create(path)?);
    let eight = || DynamicImage::ImageRgb8(image.to_rgb8());
    match format {
        FrameFormat::Jpeg(quality) => {
            write_tagged(&eight(), JpegEncoder::new_with_quality(file, quality))
        }
        FrameFormat::Png => write_tagged(&eight(), PngEncoder::new(file)),
        FrameFormat::Tiff => write_tagged(image, TiffEncoder::new(file)),
    }
}

/// Write `image` with `encoder`, tagged with an sRGB profile.
fn write_tagged(image: &DynamicImage, mut encoder: impl ImageEncoder) -> Result<(), ImageError> {
    encoder
        .set_icc_profile(crate::icc::srgb().to_vec())
        .map_err(ImageError::Unsupported)?;
    image.write_with_encoder(encoder)
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
            film: None,
            adjust: Box::default(),
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
        // The second frame is turned back upright by its own adjustments, and edited.
        let turned = Adjustments {
            quarter_turns: 3,
            mirror: true,
            contrast: 0.3,
            sharpen: adjust::Sharpen {
                amount: 0.5,
                ..Default::default()
            },
            ..Adjustments::default()
        };
        let film = Film {
            film_type: crate::state::FilmType::ColorNegative,
            stock: scan_film::stock::default_stock(),
            mono_stock: None,
            base,
            printing: Default::default(),
            mono: Default::default(),
        };
        worker.send(Command::SaveFrames {
            result: result.clone(),
            merged: None,
            cleaned: None,
            film: Some(film),
            frames: vec![
                (
                    frames[0],
                    Adjustments::default(),
                    dir.path().join("roll_01.tif"),
                ),
                (frames[1], turned, dir.path().join("roll_02.tif")),
            ],
            quarter_turns: 1,
            format: FrameFormat::Tiff,
            metadata: None,
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
        let upright = scan_core::image::open(&paths[1]).unwrap();
        assert_eq!((upright.width(), upright.height()), (px(w), px(h)));

        // Exported for the publisher: 8-bit JPEG, then the metadata.
        let (jpeg, yaml) = (dir.path().join("r_1.jpg"), dir.path().join("metadata.yml"));
        worker.send(Command::SaveFrames {
            result,
            merged: None,
            cleaned: None,
            film: Some(film),
            frames: vec![(frames[0], Adjustments::default(), jpeg.clone())],
            quarter_turns: 0,
            format: FrameFormat::Jpeg(90),
            metadata: Some((
                yaml.clone(),
                "format: 0
"
                .into(),
            )),
        });
        let Event::Saved(Ok(paths)) = next(&worker) else {
            panic!("expected the export")
        };
        assert_eq!(paths, [jpeg.clone(), yaml.clone()]);
        let saved = scan_core::image::open(&jpeg).unwrap();
        assert!(matches!(saved, DynamicImage::ImageRgb8(_)));
        assert_eq!((saved.width(), saved.height()), (px(w), px(h)));
        assert_eq!(
            std::fs::read_to_string(&yaml).unwrap(),
            "format: 0
"
        );
    }

    #[test]
    fn exports_are_cut_with_their_geometry_healed_and_tagged_srgb() {
        use scan_core::image::ImageDecoder;

        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let request = ScanRequest::new(ScanSource::Transparency, 150);
        let result = Arc::new(dev.scan(&request, &mut NoProgress).unwrap());
        let detection = scan_process::detect_frames(
            result.primary(),
            result.gamma,
            result.dpi,
            result.area,
            &DetectParams::default(),
        );
        let rect = detection.frames[0].rect;
        let bordered = Adjustments {
            border_mm: 2.0,
            spots: vec![adjust::Spot {
                x_mm: rect.cx_mm,
                y_mm: rect.cy_mm,
                radius_mm: 0.5,
            }],
            ..Adjustments::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let worker = Worker::spawn(DeviceRegistry::new(), || {});
        let paths: Vec<_> = ["a.jpg", "b.png", "c.tif"]
            .iter()
            .map(|n| dir.path().join(n))
            .collect();
        for (path, format) in
            paths
                .iter()
                .zip([FrameFormat::Jpeg(90), FrameFormat::Png, FrameFormat::Tiff])
        {
            worker.send(Command::SaveFrames {
                result: result.clone(),
                merged: None,
                cleaned: None,
                film: None,
                frames: vec![(rect, bordered.clone(), path.clone())],
                quarter_turns: 0,
                format,
                metadata: None,
            });
            let Event::Saved(Ok(_)) = next(&worker) else {
                panic!("expected {path:?} saved")
            };
        }
        let px = |mm: f32| (mm / scan_core::MM_PER_INCH * 150.0).round() as u32;
        let saved = scan_core::image::open(&paths[0]).unwrap();
        assert_eq!(
            (saved.width(), saved.height()),
            (px(rect.width_mm + 4.0), px(rect.height_mm + 4.0))
        );
        let jpeg = std::io::BufReader::new(File::open(&paths[0]).unwrap());
        let mut decoder = scan_core::image::codecs::jpeg::JpegDecoder::new(jpeg).unwrap();
        assert_eq!(
            decoder.icc_profile().unwrap().as_deref(),
            Some(crate::icc::srgb())
        );
        let png = std::io::BufReader::new(File::open(&paths[1]).unwrap());
        let mut decoder = scan_core::image::codecs::png::PngDecoder::new(png).unwrap();
        assert_eq!(
            decoder.icc_profile().unwrap().as_deref(),
            Some(crate::icc::srgb())
        );
        let tiff = std::io::BufReader::new(File::open(&paths[2]).unwrap());
        let mut decoder = scan_core::image::codecs::tiff::TiffDecoder::new(tiff).unwrap();
        assert_eq!(
            decoder.icc_profile().unwrap().as_deref(),
            Some(crate::icc::srgb())
        );
        assert!(matches!(
            scan_core::image::open(&paths[2]).unwrap(),
            DynamicImage::ImageRgb16(_)
        ));
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
