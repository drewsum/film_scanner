//! UI-independent application state and its transitions, driven by worker events.

use std::sync::Arc;

use scan_core::{
    BitDepth, Capabilities, ColorMode, DeviceId, DeviceInfo, Exposure, ScanError, ScanRequest,
    ScanResult, ScanSource, SourceCapabilities,
};

use scan_process::{DustRemoval, ExposureMerge};

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
    MergingExposures,
    RemovingDust,
    Saving,
}

/// Infrared dust and scratch removal of scans that have an infrared channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DustSettings {
    pub enabled: bool,
    /// 0 removes only obvious defects, 1 faint ones too.
    pub strength: f32,
    /// Show the defects found, in red over the image.
    pub show_mask: bool,
}

impl Default for DustSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            strength: 0.5,
            show_mask: false,
        }
    }
}

/// A dust removal of the result with some generation and strength.
#[derive(Clone, Debug)]
pub struct Cleaned {
    pub generation: u64,
    pub strength: f32,
    /// Whether it cleaned the merged exposures rather than the primary pass.
    pub merged: bool,
    pub removal: Arc<DustRemoval>,
}

/// The merged exposure passes of the result with some generation.
#[derive(Clone, Debug)]
pub struct Merged {
    pub generation: u64,
    pub merge: Arc<ExposureMerge>,
}

/// User-chosen scan settings, kept valid for the selected device and source.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanSettings {
    pub source: ScanSource,
    pub dpi: u32,
    pub bit_depth: BitDepth,
    pub color_mode: ColorMode,
    pub infrared: bool,
    /// Scan a second pass [`EXTRA_EV`] brighter, to merge with the first.
    pub multi_exposure: bool,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            source: ScanSource::Transparency,
            dpi: 1200,
            bit_depth: BitDepth::Sixteen,
            color_mode: ColorMode::Color,
            infrared: false,
            multi_exposure: false,
        }
    }
}

/// Resolution used for preview scans (the lowest supported at or above this).
pub const PREVIEW_DPI: u32 = 75;
/// How much brighter a multi-exposure scan's second pass is, in stops: as much as the V500
/// takes, and enough to lift the densest film well clear of the noise.
pub const EXTRA_EV: f32 = 1.0;

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
    /// Show, clean and save the result's exposure passes merged, when it has several.
    pub merge_exposures: bool,
    /// The latest merge; see [`AppState::current_merged`].
    pub merged: Option<Merged>,
    pub dust: DustSettings,
    /// The latest dust removal; see [`AppState::current_cleaned`].
    pub cleaned: Option<Cleaned>,
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
            merge_exposures: true,
            merged: None,
            dust: DustSettings::default(),
            cleaned: None,
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
        s.multi_exposure &= src.max_exposure_passes >= 2;
    }

    /// The request for a preview or final scan with the current settings.
    pub fn build_request(&self, kind: ScanKind) -> Result<ScanRequest, String> {
        let src = self.source_caps().ok_or("No scanner selected")?;
        let mut req = ScanRequest::new(self.settings.source, self.settings.dpi);
        req.bit_depth = self.settings.bit_depth;
        req.color_mode = self.settings.color_mode;
        req.infrared = self.settings.infrared;
        if self.settings.multi_exposure {
            req.exposures = vec![Exposure::default(), Exposure { ev: EXTRA_EV }];
        }
        if kind == ScanKind::Preview {
            req.dpi = src.preview_resolution(PREVIEW_DPI).unwrap_or(req.dpi);
            req.bit_depth = src
                .bit_depths
                .iter()
                .copied()
                .min()
                .unwrap_or(req.bit_depth);
            req.infrared = false;
            req.exposures.clear();
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

    /// The merge of the current result's exposures, if it has several, merging is on and it
    /// is done.
    pub fn current_merged(&self) -> Option<&Merged> {
        self.merged
            .as_ref()
            .filter(|m| self.merge_exposures && m.generation == self.result_generation)
    }

    /// Whether the current result's exposures are to be merged but aren't yet.
    fn merge_pending(&self) -> bool {
        self.merge_exposures
            && self.result.as_ref().is_some_and(|r| r.passes.len() > 1)
            && self.current_merged().is_none()
    }

    /// The dust removal of the current result with the current settings, if it is done.
    pub fn current_cleaned(&self) -> Option<&Cleaned> {
        self.cleaned.as_ref().filter(|c| {
            self.dust.enabled
                && c.generation == self.result_generation
                && c.strength == self.dust.strength
                && c.merged == self.current_merged().is_some()
                && !self.merge_pending()
        })
    }

    /// Start the next processing step the current result needs with the current settings:
    /// merging its exposures, then removing dust from the merge (or the primary pass). Call
    /// after the result or the settings change.
    pub fn update_processing(&mut self) -> Option<Command> {
        let result = self.result.clone()?;
        if self.is_busy() {
            return None;
        }
        if self.merge_pending() {
            self.activity = Activity::MergingExposures;
            self.status = "Merging exposures...".into();
            return Some(Command::MergeExposures {
                generation: self.result_generation,
                result,
            });
        }
        if !self.dust.enabled || result.infrared.is_none() || self.current_cleaned().is_some() {
            return None;
        }
        self.activity = Activity::RemovingDust;
        self.status = "Removing dust and scratches...".into();
        Some(Command::RemoveDust {
            generation: self.result_generation,
            strength: self.dust.strength,
            merged: self.current_merged().map(|m| m.merge.clone()),
            result,
        })
    }

    pub fn start_save(&mut self, path: std::path::PathBuf) -> Option<Command> {
        let result = self.result.clone()?;
        let merged = self.current_merged().map(|m| m.merge.clone());
        let cleaned = self.current_cleaned().map(|c| c.removal.clone());
        self.activity = Activity::Saving;
        self.status = format!("Saving {}...", path.display());
        Some(Command::Save {
            result,
            merged,
            cleaned,
            path,
        })
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
                        self.merged = None;
                        self.cleaned = None;
                        return self.update_processing();
                    }
                    Err(ScanError::Cancelled) => self.status = "Scan cancelled".into(),
                    Err(e) => {
                        self.status = "Scan failed".into();
                        self.error = Some(e.to_string());
                    }
                }
                None
            }
            Event::ExposuresMerged { generation, result } => {
                self.activity = Activity::Idle;
                match result {
                    Ok(merge) => {
                        self.status = merge_summary(&merge);
                        if generation == self.result_generation {
                            self.merged = Some(Merged {
                                generation,
                                merge: Arc::new(merge),
                            });
                        }
                    }
                    Err(e) => {
                        self.status = "Merging exposures failed".into();
                        self.error = Some(e);
                        // Carry on with the primary pass.
                        self.merge_exposures = false;
                    }
                }
                self.update_processing()
            }
            Event::DustRemoved {
                generation,
                strength,
                merged,
                result,
            } => {
                self.activity = Activity::Idle;
                match result {
                    Ok(removal) => {
                        let (w, h) = removal.mask.dimensions();
                        let share = removal.report.defect_pixels as f64 / (w as f64 * h as f64);
                        self.status = format!(
                            "Dust removal: {} px repaired ({:.2}% of the scan)",
                            removal.report.defect_pixels,
                            share * 100.0
                        );
                        if generation == self.result_generation {
                            self.cleaned = Some(Cleaned {
                                generation,
                                strength,
                                merged,
                                removal: Arc::new(removal),
                            });
                        }
                    }
                    Err(e) => {
                        self.status = "Dust removal failed".into();
                        self.error = Some(e);
                        // Don't retry the same thing straight away.
                        self.dust.enabled = false;
                    }
                }
                // The settings or the result may have changed in the meantime.
                self.update_processing()
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

/// A line on how a merge went: each longer pass's measured exposure and share of the image.
pub fn merge_summary(merge: &ExposureMerge) -> String {
    let passes: Vec<String> = merge
        .passes
        .iter()
        .map(|p| {
            format!(
                "{:+.1} EV pass measured {:+.2} EV, used for {:.0}%",
                p.ev,
                p.measured_ev(),
                p.share * 100.0
            )
        })
        .collect();
    format!("Exposures merged: {}", passes.join("; "))
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

    /// A final scan with infrared, applied to the state; returns the follow-up command.
    fn final_ir_scan(state: &mut AppState) -> Option<Command> {
        state.settings.infrared = true;
        let Some(Command::Scan { request, kind, .. }) = state.start_scan(ScanKind::Final) else {
            panic!("expected scan command");
        };
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let result = dev.scan(&request, &mut NoProgress);
        state.apply(Event::ScanDone { kind, result })
    }

    /// Run the dust removal `cmd` asks for and apply its outcome.
    fn removed(state: &mut AppState, cmd: Option<Command>) -> Option<Command> {
        let Some(Command::RemoveDust {
            generation,
            strength,
            merged,
            result,
        }) = cmd
        else {
            panic!("expected dust removal, got {cmd:?}");
        };
        let params = scan_process::DustParams::for_scan(result.dpi, strength);
        let image = merged.as_ref().map_or(result.primary(), |m| &m.image);
        let removal = scan_process::remove_dust(image, result.infrared.as_ref().unwrap(), &params);
        state.apply(Event::DustRemoved {
            generation,
            strength,
            merged: merged.is_some(),
            result: removal.map_err(|e| e.to_string()),
        })
    }

    /// Run the merge `cmd` asks for and apply its outcome.
    fn merged(state: &mut AppState, cmd: Option<Command>) -> Option<Command> {
        let Some(Command::MergeExposures { generation, result }) = cmd else {
            panic!("expected a merge, got {cmd:?}");
        };
        let merge = scan_process::merge_exposures(&result.passes, result.gamma);
        state.apply(Event::ExposuresMerged {
            generation,
            result: merge.map_err(|e| e.to_string()),
        })
    }

    #[test]
    fn infrared_scans_are_cleaned_and_saved_clean() {
        let mut state = with_mock_open();
        state.settings.dpi = 150;
        let cmd = final_ir_scan(&mut state);
        assert_eq!(state.activity, Activity::RemovingDust);
        assert!(removed(&mut state, cmd).is_none());
        assert!(!state.is_busy());
        let cleaned = state.current_cleaned().expect("cleaned").removal.clone();
        assert!(cleaned.report.defect_pixels > 0);

        let Some(Command::Save { cleaned: saved, .. }) = state.start_save("x.tif".into()) else {
            panic!("expected save");
        };
        assert!(Arc::ptr_eq(&saved.unwrap(), &cleaned));

        // Turned off, the raw scan is saved again, and nothing is recomputed.
        state.activity = Activity::Idle;
        state.dust.enabled = false;
        assert!(state.update_processing().is_none());
        let Some(Command::Save { cleaned, .. }) = state.start_save("x.tif".into()) else {
            panic!("expected save");
        };
        assert!(cleaned.is_none());
        state.activity = Activity::Idle;
        state.dust.enabled = true;
        assert!(
            state.update_processing().is_none(),
            "the earlier removal still fits"
        );
    }

    #[test]
    fn changing_strength_reruns_and_stale_results_are_dropped() {
        let mut state = with_mock_open();
        state.settings.dpi = 150;
        let first = final_ir_scan(&mut state);
        // The strength changes while the first removal runs.
        state.dust.strength = 0.9;
        assert!(state.update_processing().is_none(), "busy");
        let second = removed(&mut state, first);
        assert!(state.current_cleaned().is_none());
        assert!(matches!(
            second,
            Some(Command::RemoveDust { strength, .. }) if strength == 0.9
        ));
        assert!(removed(&mut state, second).is_none());
        assert_eq!(state.current_cleaned().unwrap().strength, 0.9);

        // A removal that finishes after a new scan doesn't apply to it.
        let generation = state.result_generation;
        state.result_generation += 1;
        let removal = state.cleaned.as_ref().unwrap().removal.as_ref().clone();
        state.dust.enabled = false;
        state.apply(Event::DustRemoved {
            generation,
            strength: 0.9,
            merged: false,
            result: Ok(removal),
        });
        assert_eq!(state.cleaned.as_ref().unwrap().generation, generation);
        state.dust.enabled = true;
        assert!(state.current_cleaned().is_none());
    }

    /// A final multi-exposure scan with infrared, applied to the state; returns the
    /// follow-up command.
    fn final_multi_exposure_scan(state: &mut AppState) -> Option<Command> {
        state.settings.multi_exposure = true;
        state.settings.infrared = true;
        let Some(Command::Scan { request, kind, .. }) = state.start_scan(ScanKind::Final) else {
            panic!("expected scan command");
        };
        assert_eq!(request.exposures.len(), 2);
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let result = dev.scan(&request, &mut NoProgress);
        state.apply(Event::ScanDone { kind, result })
    }

    #[test]
    fn multi_exposure_scans_are_merged_then_cleaned() {
        let mut state = with_mock_open();
        state.settings.dpi = 150;
        state.settings.multi_exposure = true;
        let preview = state.build_request(ScanKind::Preview).unwrap();
        assert!(preview.exposures.is_empty(), "previews are single pass");
        let cmd = final_multi_exposure_scan(&mut state);
        assert_eq!(state.activity, Activity::MergingExposures);
        assert!(state.current_cleaned().is_none());

        // Dust removal follows the merge, on the merged image.
        let cmd = merged(&mut state, cmd);
        let summary = merge_summary(&state.current_merged().unwrap().merge);
        assert!(
            summary.starts_with("Exposures merged: +1.0 EV pass measured +1.00 EV"),
            "{summary}"
        );
        assert!(matches!(
            &cmd,
            Some(Command::RemoveDust {
                merged: Some(_),
                ..
            })
        ));
        assert!(removed(&mut state, cmd).is_none());
        let merge = state.current_merged().unwrap().merge.clone();
        let cleaned = state.current_cleaned().unwrap().removal.clone();
        let Some(Command::Save {
            merged: Some(m),
            cleaned: Some(c),
            ..
        }) = state.start_save("x.tif".into())
        else {
            panic!("expected a save of the merged, cleaned image");
        };
        assert!(Arc::ptr_eq(&m, &merge) && Arc::ptr_eq(&c, &cleaned));

        // Without the merge, the primary pass is cleaned instead.
        state.activity = Activity::Idle;
        state.merge_exposures = false;
        assert!(state.current_cleaned().is_none());
        let cmd = state.update_processing();
        assert!(matches!(
            &cmd,
            Some(Command::RemoveDust { merged: None, .. })
        ));
        removed(&mut state, cmd);
        assert!(!state.current_cleaned().unwrap().merged);
        // Turned back on, the earlier merge still fits, but its cleaning was replaced.
        state.merge_exposures = true;
        assert!(state.current_merged().is_some());
        assert!(matches!(
            state.update_processing(),
            Some(Command::RemoveDust {
                merged: Some(_),
                ..
            })
        ));
    }

    #[test]
    fn a_failed_merge_falls_back_to_the_primary_pass() {
        let mut state = with_mock_open();
        state.settings.dpi = 150;
        let Some(Command::MergeExposures { generation, .. }) =
            final_multi_exposure_scan(&mut state)
        else {
            panic!("expected a merge");
        };
        let cmd = state.apply(Event::ExposuresMerged {
            generation,
            result: Err("no overlap".into()),
        });
        assert_eq!(state.error.as_deref(), Some("no overlap"));
        assert!(!state.merge_exposures);
        assert!(matches!(
            cmd,
            Some(Command::RemoveDust { merged: None, .. })
        ));
    }

    #[test]
    fn multi_exposure_needs_a_source_that_takes_two_passes() {
        let mut state = with_mock_open();
        state.settings.multi_exposure = true;
        state.fit_settings();
        assert!(state.settings.multi_exposure);
        let source = state.settings.source;
        let caps = state.capabilities.as_mut().unwrap();
        caps.sources
            .iter_mut()
            .find(|s| s.source == source)
            .unwrap()
            .max_exposure_passes = 1;
        state.fit_settings();
        assert!(!state.settings.multi_exposure);
    }

    #[test]
    fn scans_without_infrared_are_not_cleaned() {
        let mut state = with_mock_open();
        let Some(Command::Scan { request, kind, .. }) = state.start_scan(ScanKind::Preview) else {
            panic!("expected scan command");
        };
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let result = dev.scan(&request, &mut NoProgress);
        assert!(state.apply(Event::ScanDone { kind, result }).is_none());
        assert!(state.current_cleaned().is_none());
    }

    #[test]
    fn scan_without_device_is_refused() {
        let mut state = AppState::default();
        assert!(state.start_scan(ScanKind::Preview).is_none());
        assert!(state.build_request(ScanKind::Preview).is_err());
    }
}
