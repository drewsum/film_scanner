//! UI-independent application state and its transitions, driven by worker events.

use std::sync::Arc;

use scan_core::{
    BitDepth, Capabilities, ColorMode, DeviceId, DeviceInfo, ScanError, ScanRequest, ScanResult,
    ScanSource, SourceCapabilities,
};

use crate::worker::{Command, Event, ScanKind};

/// The stages of the film scanning workflow, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowStep {
    Scan,
    Crop,
    Invert,
    Adjust,
    Export,
}

impl WorkflowStep {
    pub const ALL: [WorkflowStep; 5] = [
        WorkflowStep::Scan,
        WorkflowStep::Crop,
        WorkflowStep::Invert,
        WorkflowStep::Adjust,
        WorkflowStep::Export,
    ];

    pub fn label(self) -> &'static str {
        match self {
            WorkflowStep::Scan => "Scan",
            WorkflowStep::Crop => "Crop",
            WorkflowStep::Invert => "Invert",
            WorkflowStep::Adjust => "Adjust",
            WorkflowStep::Export => "Export",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            WorkflowStep::Scan => {
                "Select a scanner, preview the bed and capture high-resolution scans."
            }
            WorkflowStep::Crop => {
                "Automatically detect frames on the strip, remove borders and straighten each frame."
            }
            WorkflowStep::Invert => {
                "Invert negatives using film-stock profiles, removing the orange mask of colour film."
            }
            WorkflowStep::Adjust => {
                "Colour correction: white balance, levels, curves and per-channel tweaks."
            }
            WorkflowStep::Export => "Batch export of finished frames with metadata.",
        }
    }
}

/// Which image of a scan result is displayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewChannel {
    Visible,
    Infrared,
}

/// What the background worker is doing on our behalf.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Activity {
    Idle,
    ListingDevices,
    OpeningDevice,
    Scanning { kind: ScanKind, progress: f32 },
    Saving,
}

/// User-chosen scan settings, kept valid for the selected device and source.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanSettings {
    pub source: ScanSource,
    pub dpi: u32,
    pub bit_depth: BitDepth,
    pub color_mode: ColorMode,
    pub infrared: bool,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            source: ScanSource::Transparency,
            dpi: 1200,
            bit_depth: BitDepth::Sixteen,
            color_mode: ColorMode::Color,
            infrared: false,
        }
    }
}

/// Resolution used for preview scans (the lowest supported at or above this).
pub const PREVIEW_DPI: u32 = 75;

pub struct AppState {
    pub step: WorkflowStep,
    pub devices: Vec<DeviceInfo>,
    pub selected: Option<usize>,
    pub capabilities: Option<Capabilities>,
    pub settings: ScanSettings,
    pub activity: Activity,
    pub status: String,
    pub error: Option<String>,
    pub result: Option<Arc<ScanResult>>,
    pub result_kind: Option<ScanKind>,
    /// Incremented whenever `result` changes, so views can refresh cached textures.
    pub result_generation: u64,
    pub view_channel: ViewChannel,
    /// The device whose capabilities the settings were last fitted to.
    settings_device: Option<DeviceId>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            step: WorkflowStep::Scan,
            devices: Vec::new(),
            selected: None,
            capabilities: None,
            settings: ScanSettings::default(),
            activity: Activity::Idle,
            status: "Ready".into(),
            error: None,
            result: None,
            result_kind: None,
            result_generation: 0,
            view_channel: ViewChannel::Visible,
            settings_device: None,
        }
    }
}

impl AppState {
    pub fn is_busy(&self) -> bool {
        self.activity != Activity::Idle
    }

    pub fn selected_device(&self) -> Option<&DeviceInfo> {
        self.selected.and_then(|i| self.devices.get(i))
    }

    pub fn source_caps(&self) -> Option<&SourceCapabilities> {
        self.capabilities.as_ref()?.source(self.settings.source)
    }

    pub fn refresh_devices(&mut self) -> Command {
        self.activity = Activity::ListingDevices;
        self.status = "Looking for scanners...".into();
        self.error = None;
        Command::ListDevices
    }

    pub fn select_device(&mut self, index: usize) -> Option<Command> {
        let device = self.devices.get(index)?.clone();
        self.selected = Some(index);
        self.capabilities = None;
        self.activity = Activity::OpeningDevice;
        self.status = format!("Opening {}...", device.display_name());
        self.error = None;
        Some(Command::Open(device))
    }

    /// Clamp the settings to what the current device and source support.
    pub fn fit_settings(&mut self) {
        let Some(caps) = &self.capabilities else {
            return;
        };
        if caps.source(self.settings.source).is_none() {
            // Prefer film scanning when the device supports it.
            let preferred = caps
                .source(ScanSource::Transparency)
                .or(caps.sources.first());
            if let Some(src) = preferred {
                self.settings.source = src.source;
            }
        }
        let Some(src) = caps.source(self.settings.source) else {
            return;
        };
        let s = &mut self.settings;
        if !src.resolutions.contains(&s.dpi) {
            // Closest supported resolution.
            s.dpi = src
                .resolutions
                .iter()
                .copied()
                .min_by_key(|r| r.abs_diff(s.dpi))
                .unwrap_or(s.dpi);
        }
        if !src.bit_depths.contains(&s.bit_depth) {
            s.bit_depth = src
                .bit_depths
                .iter()
                .copied()
                .max()
                .unwrap_or(BitDepth::Eight);
        }
        if !src.color_modes.contains(&s.color_mode) {
            s.color_mode = src.color_modes.first().copied().unwrap_or(ColorMode::Color);
        }
        s.infrared &= src.infrared;
    }

    /// The request for a preview or final scan with the current settings.
    pub fn build_request(&self, kind: ScanKind) -> Result<ScanRequest, String> {
        let src = self.source_caps().ok_or("No scanner selected")?;
        let mut req = ScanRequest::new(self.settings.source, self.settings.dpi);
        req.bit_depth = self.settings.bit_depth;
        req.color_mode = self.settings.color_mode;
        req.infrared = self.settings.infrared;
        if kind == ScanKind::Preview {
            req.dpi = src.preview_resolution(PREVIEW_DPI).unwrap_or(req.dpi);
            req.bit_depth = src
                .bit_depths
                .iter()
                .copied()
                .min()
                .unwrap_or(req.bit_depth);
            req.infrared = false;
        }
        req.validate(self.capabilities.as_ref().unwrap())
            .map_err(|e| e.to_string())?;
        Ok(req)
    }

    pub fn start_scan(&mut self, kind: ScanKind) -> Option<Command> {
        if self.is_busy() {
            return None;
        }
        let device = self.selected_device()?.clone();
        match self.build_request(kind) {
            Ok(request) => {
                self.activity = Activity::Scanning {
                    kind,
                    progress: 0.0,
                };
                self.status = format!("{} at {} dpi...", kind.label(), request.dpi);
                self.error = None;
                Some(Command::Scan {
                    device,
                    request,
                    kind,
                })
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    pub fn start_save(&mut self, path: std::path::PathBuf) -> Option<Command> {
        let result = self.result.clone()?;
        self.activity = Activity::Saving;
        self.status = format!("Saving {}...", path.display());
        Some(Command::Save { result, path })
    }

    /// Apply a worker event, returning a follow-up command if one is needed.
    pub fn apply(&mut self, event: Event) -> Option<Command> {
        match event {
            Event::Devices { devices, errors } => {
                self.activity = Activity::Idle;
                let previous = self.selected_device().map(|d| d.id.clone());
                self.devices = devices;
                self.error = (!errors.is_empty()).then(|| errors.join("; "));
                self.status = format!("Found {} scanner(s)", self.devices.len());
                let index = previous
                    .and_then(|id| self.devices.iter().position(|d| d.id == id))
                    .or((!self.devices.is_empty()).then_some(0));
                match index {
                    Some(i) => self.select_device(i),
                    None => {
                        self.selected = None;
                        self.capabilities = None;
                        None
                    }
                }
            }
            Event::Opened(Ok((device, caps))) => {
                if self.selected_device() == Some(&device) {
                    self.capabilities = Some(caps);
                    // Settings carry over when a device is re-opened, not to a different one.
                    if self.settings_device.as_ref() != Some(&device.id) {
                        self.settings = ScanSettings::default();
                        self.settings_device = Some(device.id.clone());
                    }
                    self.fit_settings();
                    self.status = format!("{} ready", device.display_name());
                }
                self.activity = Activity::Idle;
                None
            }
            Event::Opened(Err(e)) => {
                self.activity = Activity::Idle;
                self.capabilities = None;
                self.status = "Could not open scanner".into();
                self.error = Some(e);
                None
            }
            Event::Progress(p) => {
                if let Activity::Scanning { progress, .. } = &mut self.activity {
                    *progress = p;
                }
                None
            }
            Event::ScanDone { kind, result } => {
                self.activity = Activity::Idle;
                match result {
                    Ok(result) => {
                        let (w, h) = result.dimensions();
                        self.status = format!(
                            "{} complete: {w} x {h} px at {} dpi",
                            kind.label(),
                            result.dpi
                        );
                        if result.infrared.is_none() {
                            self.view_channel = ViewChannel::Visible;
                        }
                        self.result = Some(Arc::new(result));
                        self.result_kind = Some(kind);
                        self.result_generation += 1;
                    }
                    Err(ScanError::Cancelled) => self.status = "Scan cancelled".into(),
                    Err(e) => {
                        self.status = "Scan failed".into();
                        self.error = Some(e.to_string());
                    }
                }
                None
            }
            Event::Saved(result) => {
                self.activity = Activity::Idle;
                match result {
                    Ok(paths) => {
                        self.status =
                            format!("Saved {} file(s): {}", paths.len(), paths[0].display())
                    }
                    Err(e) => self.error = Some(format!("Save failed: {e}")),
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan_core::mock::MockBackend;
    use scan_core::{NoProgress, ScannerBackend};

    fn with_mock_open() -> AppState {
        let mut state = AppState::default();
        let cmd = state.apply(Event::Devices {
            devices: vec![MockBackend::device_info()],
            errors: vec![],
        });
        assert!(matches!(cmd, Some(Command::Open(_))));
        state.apply(Event::Opened(Ok((
            MockBackend::device_info(),
            MockBackend::capabilities(),
        ))));
        state
    }

    #[test]
    fn device_listing_selects_and_opens_first() {
        let mut state = AppState::default();
        assert!(matches!(state.refresh_devices(), Command::ListDevices));
        assert!(state.is_busy());
        let cmd = state.apply(Event::Devices {
            devices: vec![MockBackend::device_info()],
            errors: vec![],
        });
        assert!(matches!(cmd, Some(Command::Open(d)) if d == MockBackend::device_info()));
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.activity, Activity::OpeningDevice);
    }

    #[test]
    fn empty_listing_clears_selection_and_reports_errors() {
        let mut state = with_mock_open();
        let cmd = state.apply(Event::Devices {
            devices: vec![],
            errors: vec!["wia: offline".into()],
        });
        assert!(cmd.is_none());
        assert_eq!(state.selected, None);
        assert!(state.capabilities.is_none());
        assert_eq!(state.error.as_deref(), Some("wia: offline"));
    }

    #[test]
    fn opening_fits_settings_to_capabilities() {
        let state = with_mock_open();
        assert!(!state.is_busy());
        // Mock tops out at 600 dpi for film; the 1200 dpi default is clamped.
        assert_eq!(state.settings.source, ScanSource::Transparency);
        assert_eq!(state.settings.dpi, 600);
        assert_eq!(state.settings.bit_depth, BitDepth::Sixteen);
    }

    #[test]
    fn settings_reset_for_a_different_device_but_not_on_reopen() {
        // A flatbed-only, 8-bit scanner (like the V500 over WIA) alongside the mock.
        let mut flatbed_only = MockBackend::capabilities();
        flatbed_only
            .sources
            .retain(|s| s.source == ScanSource::Flatbed);
        flatbed_only.sources[0].bit_depths = vec![BitDepth::Eight];
        let mut other = MockBackend::device_info();
        other.id = DeviceId("wia:v500".into());
        other.backend = "wia";

        let mut state = AppState::default();
        let devices = vec![other.clone(), MockBackend::device_info()];
        state.apply(Event::Devices {
            devices,
            errors: vec![],
        });
        state.apply(Event::Opened(Ok((other, flatbed_only))));
        assert_eq!(state.settings.source, ScanSource::Flatbed);
        assert_eq!(state.settings.bit_depth, BitDepth::Eight);

        state.select_device(1);
        let mock = (MockBackend::device_info(), MockBackend::capabilities());
        state.apply(Event::Opened(Ok(mock.clone())));
        assert_eq!(state.settings.source, ScanSource::Transparency);
        assert_eq!(state.settings.bit_depth, BitDepth::Sixteen);

        // User choices survive re-opening the same device (e.g. after Refresh).
        state.settings.dpi = 150;
        state.select_device(1);
        state.apply(Event::Opened(Ok(mock)));
        assert_eq!(state.settings.dpi, 150);
    }

    #[test]
    fn switching_source_disables_unsupported_infrared() {
        let mut state = with_mock_open();
        state.settings.infrared = true;
        state.settings.source = ScanSource::Flatbed;
        state.fit_settings();
        assert!(!state.settings.infrared);
        assert_eq!(state.settings.dpi, 300);
    }

    #[test]
    fn preview_request_uses_low_resolution_without_infrared() {
        let mut state = with_mock_open();
        state.settings.infrared = true;
        let preview = state.build_request(ScanKind::Preview).unwrap();
        assert_eq!(
            (preview.dpi, preview.bit_depth, preview.infrared),
            (75, BitDepth::Eight, false)
        );
        let full = state.build_request(ScanKind::Final).unwrap();
        assert_eq!(
            (full.dpi, full.bit_depth, full.infrared),
            (600, BitDepth::Sixteen, true)
        );
    }

    #[test]
    fn scan_lifecycle_stores_result() {
        let mut state = with_mock_open();
        let Some(Command::Scan { request, kind, .. }) = state.start_scan(ScanKind::Preview) else {
            panic!("expected scan command");
        };
        assert!(
            state.start_scan(ScanKind::Final).is_none(),
            "no second scan while busy"
        );
        state.apply(Event::Progress(0.5));
        assert_eq!(
            state.activity,
            Activity::Scanning {
                kind,
                progress: 0.5
            }
        );

        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let result = dev.scan(&request, &mut NoProgress);
        state.apply(Event::ScanDone { kind, result });
        assert!(!state.is_busy());
        assert_eq!(state.result_generation, 1);
        assert_eq!(state.result_kind, Some(ScanKind::Preview));
        assert!(state.error.is_none());
    }

    #[test]
    fn cancelled_and_failed_scans() {
        let mut state = with_mock_open();
        state.start_scan(ScanKind::Final);
        state.apply(Event::ScanDone {
            kind: ScanKind::Final,
            result: Err(ScanError::Cancelled),
        });
        assert_eq!(state.status, "Scan cancelled");
        assert!(state.error.is_none() && state.result.is_none());

        state.start_scan(ScanKind::Final);
        let err = ScanError::Backend("lamp failure".into());
        state.apply(Event::ScanDone {
            kind: ScanKind::Final,
            result: Err(err),
        });
        assert!(state.error.as_deref().unwrap().contains("lamp failure"));
    }

    #[test]
    fn scan_without_device_is_refused() {
        let mut state = AppState::default();
        assert!(state.start_scan(ScanKind::Preview).is_none());
        assert!(state.build_request(ScanKind::Preview).is_err());
    }
}
