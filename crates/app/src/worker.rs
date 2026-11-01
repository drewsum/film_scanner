//! Background thread that owns scanner access, so device I/O never blocks the UI.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use scan_core::{
    CancelToken, Capabilities, DeviceInfo, DeviceRegistry, ProgressSink, ScanError, ScanRequest,
    ScanResult, ScannerDevice, export,
};
use scan_process::{DustParams, DustRemoval};

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
    /// Find and fill dust in the result's primary image, using its infrared channel.
    RemoveDust {
        /// The result's generation, handed back with the outcome.
        generation: u64,
        strength: f32,
        result: Arc<ScanResult>,
    },
    Save {
        result: Arc<ScanResult>,
        /// Saved in place of the primary pass when present.
        cleaned: Option<Arc<DustRemoval>>,
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
    DustRemoved {
        generation: u64,
        strength: f32,
        result: Result<DustRemoval, String>,
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
            Command::RemoveDust {
                generation,
                strength,
                result,
            } => {
                let removed = match &result.infrared {
                    Some(ir) => {
                        let params = DustParams::for_scan(result.dpi, strength);
                        scan_process::remove_dust(result.primary(), ir, &params)
                            .map_err(|e| e.to_string())
                    }
                    None => Err("the scan has no infrared channel".into()),
                };
                send(Event::DustRemoved {
                    generation,
                    strength,
                    result: removed,
                });
            }
            Command::Save {
                result,
                cleaned,
                path,
            } => {
                let primary = cleaned.as_ref().map_or(result.primary(), |c| &c.image);
                let saved = export::save_tiff_with_primary(&result, primary, &path)
                    .map_err(|e| e.to_string());
                send(Event::Saved(saved));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use scan_core::mock::MockBackend;
    use scan_core::{NoProgress, ScanSource, ScannerBackend};

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
            cleaned: None,
            path: path.clone(),
        });
        let Event::Saved(Ok(paths)) = next(&worker) else {
            panic!("expected save")
        };
        assert_eq!(paths, vec![path.clone()]);
        assert!(path.exists());
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
