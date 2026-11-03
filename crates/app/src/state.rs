//! UI-independent application state and its transitions, driven by worker events.

use std::sync::Arc;

use scan_core::{
    BitDepth, Capabilities, ColorMode, DeviceId, DeviceInfo, Exposure, ScanError, ScanRequest,
    ScanResult, ScanSource, SourceCapabilities,
};

use scan_core::image::DynamicImage;
use scan_film::{FilmStock, Inverter, Mono, MonoStock, Printing};
use scan_process::adjust::{self, Adjustments, Levels};
use scan_process::{DustRemoval, ExposureMerge, FrameDetection, FrameRect};

use crate::export::{self, ExportSettings};
use crate::history::{self, History, Preset};
use crate::session::{self, Roll};
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
                "Turn the film into positives: color negatives with film-stock profiles, \
                 removing the orange mask, black-and-white negatives and slides."
            }
            WorkflowStep::Adjust => {
                "Edit each frame on its own: color, tone, levels, curves, selective color, \
                 grain reduction, sharpening, straightening and spot healing."
            }
            WorkflowStep::Export => {
                "Save the finished frames of the roll into a folder, with the metadata \
                 film_roll_publisher uploads them to Immich with."
            }
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
    DetectingFrames,
    Saving,
    OpeningRoll,
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

/// A frame to crop out of the scan.
#[derive(Clone, Debug, PartialEq)]
pub struct CropFrame {
    pub rect: FrameRect,
    /// Detected without either end along the strip showing, so placed from the frame
    /// spacing alone: worth checking. Cleared when the frame is edited.
    pub check: bool,
    /// The frame's own edits, from the Adjust step.
    pub adjust: Adjustments,
    /// Left out of exports and saves: a blank, fogged or repeated frame.
    pub skip: bool,
    /// Its edits' undo and redo.
    pub history: History,
}

impl CropFrame {
    pub fn new(rect: FrameRect) -> Self {
        Self {
            rect,
            check: false,
            adjust: Adjustments::default(),
            skip: false,
            history: History::default(),
        }
    }

    /// What is cut from the scan for it: its rectangle with its border and straightening.
    pub fn cut(&self) -> FrameRect {
        adjust::framed(&self.rect, &self.adjust)
    }
}

/// The frames on the scan, as detected and edited, in millimetres on the scanner bed: frames
/// found on a preview stay put on a later scan of the same film.
#[derive(Clone, Debug, Default)]
pub struct Crop {
    pub frames: Vec<CropFrame>,
    pub selected: Option<usize>,
    /// The frames were changed by hand, so a new scan doesn't replace them with a detection.
    pub edited: bool,
    /// The result generation the frames were last detected on.
    pub detected: Option<u64>,
    /// Film strips the last detection found.
    pub strips: usize,
    /// Number the frames from the other end of the film: for cameras that wind the film
    /// the other way. Kept for later detections.
    pub reversed: bool,
}

/// What film is in the holder, which decides how it is turned into a positive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FilmType {
    /// Inverted with the film stock's profile, which removes the orange mask.
    #[default]
    ColorNegative,
    /// Inverted on its film's curve (or a generic black-and-white film's), mixed to grey
    /// and toned.
    BlackAndWhite,
    /// A positive already: printed as it is, with the roll's exposure and balance.
    Slide,
}

impl FilmType {
    pub const ALL: [FilmType; 3] = [
        FilmType::ColorNegative,
        FilmType::BlackAndWhite,
        FilmType::Slide,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FilmType::ColorNegative => "Color negative",
            FilmType::BlackAndWhite => "Black-and-white negative",
            FilmType::Slide => "Slide (positive)",
        }
    }

    /// How it is kept in settings and roll files: never change these once released.
    pub fn key(self) -> &'static str {
        match self {
            FilmType::ColorNegative => "color-negative",
            FilmType::BlackAndWhite => "black-and-white",
            FilmType::Slide => "slide",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }

    /// Whether it needs the film base measured: negatives are inverted against it.
    pub fn is_negative(self) -> bool {
        self != FilmType::Slide
    }
}

/// Turning film scans into positives: color negatives with a film stock's profile,
/// black-and-white negatives with one or without, slides without one.
#[derive(Clone, Copy, Debug)]
pub struct InvertSettings {
    /// Show and save film scans as positives.
    pub enabled: bool,
    pub film_type: FilmType,
    /// The color negative film's profile.
    pub stock: &'static FilmStock,
    /// The black-and-white film's profile; `None` inverts on a generic film's curve.
    pub mono_stock: Option<&'static MonoStock>,
    pub printing: Printing,
    /// How black-and-white film is made grey and toned.
    pub mono: Mono,
}

impl Default for InvertSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            film_type: FilmType::ColorNegative,
            stock: scan_film::stock::default_stock(),
            mono_stock: None,
            printing: Printing::default(),
            mono: Mono::default(),
        }
    }
}

impl InvertSettings {
    /// The film whose profile inverts this film type: the color stock for a color negative,
    /// the black-and-white stock for a black-and-white negative if one is chosen. Slides
    /// have none.
    pub fn profile_film(&self) -> Option<ProfileFilm> {
        match self.film_type {
            FilmType::ColorNegative => {
                let s = self.stock;
                Some(ProfileFilm {
                    id: s.id,
                    manufacturer: s.manufacturer,
                    name: s.name,
                    iso: s.iso,
                })
            }
            FilmType::BlackAndWhite => self.mono_stock.map(|s| ProfileFilm {
                id: s.id,
                manufacturer: s.manufacturer,
                name: s.name,
                iso: s.iso,
            }),
            FilmType::Slide => None,
        }
    }
}

/// A film profile's stock, as the roll's metadata names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileFilm {
    pub id: &'static str,
    pub manufacturer: &'static str,
    pub name: &'static str,
    pub iso: u32,
}

impl ProfileFilm {
    /// "Kodak Gold 200".
    pub fn display_name(&self) -> String {
        format!("{} {}", self.manufacturer, self.name)
    }
}

/// What turns the current result into positives: its film's type and stock, its film base
/// and the roll's printing. Each frame is printed with its own adjustments' exposure and
/// color added.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Film {
    pub film_type: FilmType,
    pub stock: &'static FilmStock,
    pub mono_stock: Option<&'static MonoStock>,
    /// Linear light through the bare film base, per channel (1 for a slide).
    pub base: [f32; 3],
    pub printing: Printing,
    pub mono: Mono,
}

impl Film {
    /// The inverter for an image with `adjust`.
    pub fn inverter(&self, adjust: &Adjustments) -> Inverter {
        self.with_printing(&adjust.printing(&self.printing))
    }

    /// The inverter with no printing exposure or balance, which frames are measured
    /// through for Auto.
    pub fn neutral(&self) -> Inverter {
        self.with_printing(&Printing {
            exposure_ev: 0.0,
            balance_ev: [0.0; 3],
            ..self.printing
        })
    }

    fn with_printing(&self, printing: &Printing) -> Inverter {
        match self.film_type {
            FilmType::ColorNegative => Inverter::new(self.stock.model(), self.base, printing),
            FilmType::BlackAndWhite => Inverter::black_and_white(
                self.base,
                printing,
                &self.mono,
                self.mono_stock.map(MonoStock::model),
            ),
            FilmType::Slide => Inverter::positive(printing),
        }
    }
}

/// The light through the bare film base of the result with some generation, which
/// negatives are inverted against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilmBase {
    pub generation: u64,
    /// Linear, 0-1 of full scale, per channel.
    pub light: [f32; 3],
    /// Measured on the film's rebate by frame detection, rather than guessed from the
    /// clearest film on the scan.
    pub measured: bool,
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
    pub crop: Crop,
    /// How the scan is turned for viewing and saving, in quarter turns clockwise.
    pub quarter_turns: u8,
    pub invert: InvertSettings,
    /// The film base of the current result, once frame detection has looked for it.
    pub film_base: Option<FilmBase>,
    /// The Adjust step's edits of the whole scan, for saving it (each frame has its own).
    pub scan_adjust: Adjustments,
    /// Adjustments copied from a frame, to paste onto others.
    pub clipboard: Option<Adjustments>,
    /// The whole scan's undo and redo (each frame has its own).
    pub scan_history: History,
    /// Saved edits, kept between sessions.
    pub presets: Vec<Preset>,
    pub export: ExportSettings,
    /// The roll file the current result was last saved to or opened from, and that
    /// result's generation: saving to it again needn't write the scan again.
    pub roll_file: Option<(std::path::PathBuf, u64)>,
    /// The device whose capabilities the settings were last fitted to.
    settings_device: Option<DeviceId>,
    /// Every target's edits as they were when last settled (the whole scan's, then each
    /// frame's), which a change is undone back to; `None` until the frames settle.
    settled: Option<Vec<Adjustments>>,
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
            crop: Crop::default(),
            quarter_turns: 0,
            invert: InvertSettings::default(),
            film_base: None,
            scan_adjust: Adjustments::default(),
            clipboard: None,
            scan_history: History::default(),
            presets: Vec::new(),
            export: ExportSettings::default(),
            roll_file: None,
            settings_device: None,
            settled: None,
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

    /// Whether frames are to be detected on the current result: on every film scan, which
    /// also measures its film base; frames edited by hand are kept.
    fn detection_pending(&self) -> bool {
        self.result.as_ref().is_some_and(|r| {
            r.source == ScanSource::Transparency
                && self.crop.detected != Some(self.result_generation)
        })
    }

    /// The image shown and saved: the dust-cleaned one, else the merged exposures, else the
    /// primary pass.
    pub fn processed_image(&self) -> Option<&DynamicImage> {
        let result = self.result.as_ref()?;
        Some(match (self.current_cleaned(), self.current_merged()) {
            (Some(c), _) => &c.removal.image,
            (None, Some(m)) => &m.merge.image,
            (None, None) => result.primary(),
        })
    }

    /// The current result's film base, once it is known.
    pub fn current_film_base(&self) -> Option<FilmBase> {
        self.film_base
            .filter(|b| b.generation == self.result_generation)
    }

    /// Which image [`AppState::processed_image`] is: the dust removal or the merge, told
    /// apart by its address while it is alive, or 0 for the primary pass. With the result
    /// generation, a key for caches of it.
    pub fn processed_id(&self) -> u64 {
        match (self.current_cleaned(), self.current_merged()) {
            (Some(c), _) => Arc::as_ptr(&c.removal) as usize as u64,
            (None, Some(m)) => Arc::as_ptr(&m.merge) as usize as u64,
            (None, None) => 0,
        }
    }

    /// What turns the current result into positives, if it is a film scan to be turned
    /// and (for a negative) its film base is known.
    pub fn film(&self) -> Option<Film> {
        let result = self.result.as_ref()?;
        if !self.invert.enabled || result.source != ScanSource::Transparency {
            return None;
        }
        let film_type = self.invert.film_type;
        let base = if film_type.is_negative() {
            self.current_film_base()?.light
        } else {
            [1.0; 3]
        };
        Some(Film {
            film_type,
            stock: self.invert.stock,
            mono_stock: self.invert.mono_stock,
            base,
            printing: self.invert.printing,
            mono: self.invert.mono,
        })
    }

    /// What turns the current result into a positive with the roll's printing alone.
    pub fn inverter(&self) -> Option<Inverter> {
        self.film().map(|f| f.inverter(&Adjustments::default()))
    }

    /// The adjustments of frame `target`, or of the whole scan for `None` (or a frame that
    /// isn't there).
    pub fn adjustments(&self, target: Option<usize>) -> &Adjustments {
        match target.and_then(|i| self.crop.frames.get(i)) {
            Some(frame) => &frame.adjust,
            None => &self.scan_adjust,
        }
    }

    pub fn adjustments_mut(&mut self, target: Option<usize>) -> &mut Adjustments {
        match target.and_then(|i| self.crop.frames.get_mut(i)) {
            Some(frame) => &mut frame.adjust,
            None => &mut self.scan_adjust,
        }
    }

    fn history_mut(&mut self, target: Option<usize>) -> &mut History {
        match target.and_then(|i| self.crop.frames.get_mut(i)) {
            Some(frame) => &mut frame.history,
            None => &mut self.scan_history,
        }
    }

    pub fn history(&self, target: Option<usize>) -> &History {
        match target.and_then(|i| self.crop.frames.get(i)) {
            Some(frame) => &frame.history,
            None => &self.scan_history,
        }
    }

    /// Record every change of edits since the last call in its target's history, if
    /// `settled` (nothing is being dragged): a slider dragged is one change. Call once per
    /// frame of the UI, after it has made its changes.
    pub fn track_history(&mut self, settled: bool) {
        let now = |state: &Self| {
            std::iter::once(&state.scan_adjust)
                .chain(state.crop.frames.iter().map(|f| &f.adjust))
                .cloned()
                .collect::<Vec<_>>()
        };
        match self.settled.take() {
            Some(before) if before.len() == self.crop.frames.len() + 1 => {
                if !settled {
                    self.settled = Some(before);
                    return;
                }
                for (i, before) in before.into_iter().enumerate() {
                    let target = i.checked_sub(1);
                    if *self.adjustments(target) != before {
                        self.history_mut(target).record(before);
                    }
                }
                self.settled = Some(now(self));
            }
            // The frames changed (or this is the first call): start again from here.
            _ => self.settled = Some(now(self)),
        }
    }

    /// Stop tracking changes until the next [`AppState::track_history`]: the frames were
    /// replaced or moved, so their edits before aren't theirs.
    pub(crate) fn forget_settled(&mut self) {
        self.settled = None;
    }

    /// Undo `target`'s last change of edits; false if there is none.
    pub fn undo(&mut self, target: Option<usize>) -> bool {
        let current = self.adjustments(target).clone();
        match self.history_mut(target).undo(current) {
            Some(previous) => self.set_settled(target, previous),
            None => false,
        }
    }

    /// Redo `target`'s last undone change; false if there is none.
    pub fn redo(&mut self, target: Option<usize>) -> bool {
        let current = self.adjustments(target).clone();
        match self.history_mut(target).redo(current) {
            Some(next) => self.set_settled(target, next),
            None => false,
        }
    }

    /// Give `target` `adjust`, as already recorded, so tracking doesn't record it again.
    fn set_settled(&mut self, target: Option<usize>, adjust: Adjustments) -> bool {
        let i = target
            .filter(|&i| i < self.crop.frames.len())
            .map_or(0, |i| i + 1);
        if let Some(entry) = self.settled.as_mut().and_then(|s| s.get_mut(i)) {
            *entry = adjust.clone();
        }
        *self.adjustments_mut(target) = adjust;
        true
    }

    /// The frames saved and exported, in order, with their indices: those not skipped.
    pub fn exported_frames(&self) -> impl Iterator<Item = (usize, &CropFrame)> {
        self.crop.frames.iter().enumerate().filter(|(_, f)| !f.skip)
    }

    /// Adjust `target` automatically, as SilverFast's auto adjust does:
    ///
    /// - **Color.** A frame of a negative gets the printing a photofinisher's printer
    ///   would give it on its own (its average taken as grey; see
    ///   [`scan_film::auto_printing`]), as offsets from the roll's. A positive is balanced
    ///   to grey the same way, in linear light, keeping its exposure. The whole scan of a
    ///   negative keeps the roll's printing, which the Invert step's Auto sets.
    /// - **Levels.** Master levels stretch the result to full range (see
    ///   [`adjust::auto_levels`]), the same for every channel, so the balance is kept.
    ///
    /// Everything else is left as it is.
    pub fn auto_adjust(&mut self, target: Option<usize>) {
        let (Some(result), Some(image)) = (self.result.clone(), self.processed_image()) else {
            return;
        };
        let rect = target
            .and_then(|i| self.crop.frames.get(i))
            .map(|f| f.cut());
        let film = self.film();
        let mut adj = self.adjustments(target).clone();
        let (preview, scale) = scan_process::preview_frame(
            image,
            result.dpi,
            result.area,
            rect.as_ref(),
            AUTO_PREVIEW_SIDE,
        );
        adj.temperature = 0.0;
        adj.tint = 0.0;
        match (film, rect) {
            (Some(f), Some(rect)) => {
                let levels = scan_process::frame_levels(
                    image,
                    result.gamma,
                    result.dpi,
                    result.area,
                    &[rect],
                    &f.neutral(),
                );
                if let Some(level) = levels.first() {
                    let p = scan_film::auto_printing(&[*level], &f.printing);
                    adj.exposure_ev = p.exposure_ev - f.printing.exposure_ev;
                    adj.balance_ev =
                        std::array::from_fn(|c| p.balance_ev[c] - f.printing.balance_ev[c]);
                }
            }
            (Some(_), None) => {
                adj.exposure_ev = 0.0;
                adj.balance_ev = [0.0; 3];
            }
            (None, _) => {
                let means = adjust::log_means(&preview, result.gamma);
                adj.exposure_ev = 0.0;
                adj.balance_ev = adjust::neutralize(means.map(f32::exp2));
            }
        }
        let color = Adjustments {
            exposure_ev: adj.exposure_ev,
            balance_ev: adj.balance_ev,
            ..Adjustments::default()
        };
        let inverter = film.map(|f| f.inverter(&color));
        let positive = adjust::develop(&preview, result.gamma, inverter.as_ref(), &color, scale);
        adj.levels = [Levels::default(); 4];
        adj.levels[adjust::MASTER] = adjust::auto_levels(&adjust::histogram(&positive), 0.001);
        let l = adj.levels[adjust::MASTER];
        self.status = format!(
            "Auto: exposure {:+.2} EV, red {:+.2}, green {:+.2}, blue {:+.2}; levels {:.0} to {:.0}",
            adj.exposure_ev,
            adj.balance_ev[0],
            adj.balance_ev[1],
            adj.balance_ev[2],
            l.black * 255.0,
            l.white * 255.0
        );
        *self.adjustments_mut(target) = adj;
    }

    /// [`AppState::auto_adjust`] every frame.
    pub fn auto_all_frames(&mut self) {
        let n = self.crop.frames.len();
        for i in 0..n {
            self.auto_adjust(Some(i));
        }
        if n > 0 {
            self.status = format!("Auto: color and levels for each of {n} frames on its own");
        }
    }

    /// Pull every frame's exposure and color balance to the roll's: each frame's average
    /// color (its mean log light per channel, through the film at neutral printing) is
    /// moved to the median frame's, so frames shot in the same light print alike, without
    /// each being made grey on its own as Auto does. Temperature, tint and every other
    /// edit are kept.
    pub fn match_roll(&mut self) {
        let (Some(result), Some(image)) = (self.result.clone(), self.processed_image()) else {
            return;
        };
        let film = self.film();
        // Each frame's level, in stops, if it is on the scan.
        let levels: Vec<Option<[f32; 3]>> = self
            .crop
            .frames
            .iter()
            .map(|frame| {
                let rect = frame.cut();
                match &film {
                    Some(f) => scan_process::frame_levels(
                        image,
                        result.gamma,
                        result.dpi,
                        result.area,
                        &[rect],
                        &f.neutral(),
                    )
                    .first()
                    .map(|l| l.map(|v| v / std::f32::consts::LOG10_2)),
                    None => {
                        let (preview, _) = scan_process::preview_frame(
                            image,
                            result.dpi,
                            result.area,
                            Some(&rect),
                            MATCH_PREVIEW_SIDE,
                        );
                        Some(adjust::log_means(&preview, result.gamma))
                    }
                }
            })
            .collect();
        let known: Vec<[f32; 3]> = levels.iter().flatten().copied().collect();
        if known.len() < 2 {
            self.error = Some("Match roll needs at least two frames on the scan".into());
            return;
        }
        let target: [f32; 3] = std::array::from_fn(|c| {
            let mut v: Vec<f32> = known.iter().map(|l| l[c]).collect();
            v.sort_by(f32::total_cmp);
            v[v.len() / 2]
        });
        for (frame, level) in self.crop.frames.iter_mut().zip(&levels) {
            let Some(level) = level else { continue };
            let offset: [f32; 3] = std::array::from_fn(|c| target[c] - level[c]);
            let exposure = offset.iter().sum::<f32>() / 3.0;
            let adj = &mut frame.adjust;
            // What temperature and tint add, which the balance makes up to the offset.
            let warmth = Adjustments {
                temperature: adj.temperature,
                tint: adj.tint,
                ..Adjustments::default()
            }
            .color_ev();
            adj.exposure_ev = exposure;
            adj.balance_ev = std::array::from_fn(|c| offset[c] - exposure - warmth[c]);
        }
        self.error = None;
        self.status = format!("Matched {} frames to the roll's median frame", known.len());
    }

    /// Save `target`'s edits, without its geometry, as a preset named `name`, replacing
    /// one of that name.
    pub fn save_preset(&mut self, name: &str, target: Option<usize>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let preset = Preset {
            name: name.to_owned(),
            adjust: history::without_geometry(self.adjustments(target)),
        };
        match self.presets.iter_mut().find(|p| p.name == name) {
            Some(p) => *p = preset,
            None => self.presets.push(preset),
        }
        self.status = format!("Saved preset {name}");
    }

    /// Apply preset `index` to `target`, keeping its geometry.
    pub fn apply_preset(&mut self, index: usize, target: Option<usize>) {
        if let Some(p) = self.presets.get(index).cloned() {
            history::paste(self.adjustments_mut(target), p.adjust);
            self.status = format!("Applied preset {}", p.name);
        }
    }

    /// Apply preset `index` to every frame, each keeping its geometry.
    pub fn apply_preset_to_all(&mut self, index: usize) {
        if let Some(p) = self.presets.get(index).cloned() {
            for frame in &mut self.crop.frames {
                history::paste(&mut frame.adjust, p.adjust.clone());
            }
            self.status = format!(
                "Applied preset {} to {} frames",
                p.name,
                self.crop.frames.len()
            );
        }
    }

    /// Balance `target` so the point where its linear light (through the film, for a
    /// negative) is `light` comes out neutral: the grey picker.
    pub fn pick_neutral(&mut self, target: Option<usize>, light: [f32; 3]) {
        let film = self.film();
        let adj = self.adjustments_mut(target);
        let e: [f32; 3] = match film {
            Some(f) => {
                let inverter = f.inverter(adj);
                std::array::from_fn(|c| inverter.scene(c, light[c]))
            }
            None => {
                let g = adj.gains();
                std::array::from_fn(|c| light[c] * g[c])
            }
        };
        let d = adjust::neutralize(e);
        for (b, d) in adj.balance_ev.iter_mut().zip(d) {
            *b += d;
        }
        self.status = format!(
            "Grey picked: red {:+.2}, green {:+.2}, blue {:+.2} EV",
            d[0], d[1], d[2]
        );
    }

    /// Set `target`'s levels so `value` (each channel of the positive, before levels, 0-1)
    /// becomes white, or black: the white- and black-point pickers. Each channel gets its
    /// own, so the point also comes out neutral.
    pub fn pick_level(&mut self, target: Option<usize>, value: [f32; 3], white: bool) {
        let adj = self.adjustments_mut(target);
        for (l, v) in adj.levels[1..].iter_mut().zip(value) {
            if white {
                l.white = v.clamp(l.black + 0.02, 1.0);
            } else {
                l.black = v.clamp(0.0, l.white - 0.02);
            }
        }
    }

    /// Copy `target`'s adjustments.
    pub fn copy_adjustments(&mut self, target: Option<usize>) {
        self.clipboard = Some(self.adjustments(target).clone());
    }

    /// Paste the copied adjustments onto `target`, all but its geometry.
    pub fn paste_adjustments(&mut self, target: Option<usize>) {
        if let Some(copied) = self.clipboard.clone() {
            history::paste(self.adjustments_mut(target), copied);
        }
    }

    /// Paste the copied adjustments onto every frame, all but their geometry.
    pub fn paste_to_all_frames(&mut self) {
        if let Some(copied) = &self.clipboard {
            for frame in &mut self.crop.frames {
                history::paste(&mut frame.adjust, copied.clone());
            }
        }
    }

    /// Undo every edit of `target` but its geometry (its turn, mirroring, straightening and
    /// healed spots).
    pub fn reset_adjustments(&mut self, target: Option<usize>) {
        history::paste(self.adjustments_mut(target), Adjustments::default());
    }

    /// Set the printing's exposure and color balance the way a photofinisher's printer
    /// does, from the frames' average colors (see [`scan_film::auto_printing`]).
    pub fn auto_printing(&mut self) {
        let (Some(result), Some(film), Some(image)) =
            (self.result.clone(), self.film(), self.processed_image())
        else {
            return;
        };
        let frames: Vec<FrameRect> = self.crop.frames.iter().map(|f| f.cut()).collect();
        if frames.is_empty() {
            self.error = Some("Auto needs frames: detect or add them in the Crop step".into());
            return;
        }
        let levels = scan_process::frame_levels(
            image,
            result.gamma,
            result.dpi,
            result.area,
            &frames,
            &film.neutral(),
        );
        let p = scan_film::auto_printing(&levels, &self.invert.printing);
        self.invert.printing = p;
        self.error = None;
        self.status = format!(
            "Auto from {} frames: exposure {:+.2} EV, red {:+.2}, green {:+.2}, blue {:+.2}",
            levels.len(),
            p.exposure_ev,
            p.balance_ev[0],
            p.balance_ev[1],
            p.balance_ev[2]
        );
    }

    /// Start the next processing step the current result needs with the current settings:
    /// detecting its frames, merging its exposures, then removing dust from the merge (or
    /// the primary pass). Call after the result or the settings change.
    pub fn update_processing(&mut self) -> Option<Command> {
        let result = self.result.clone()?;
        if self.is_busy() {
            return None;
        }
        if self.detection_pending() {
            return self.detect(result);
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
            film: self.film(),
            adjust: Box::new(self.scan_adjust.clone()),
            quarter_turns: self.quarter_turns,
            path,
        })
    }

    /// Save every frame not skipped as its own TIFF, numbered after `path`.
    pub fn start_save_frames(&mut self, path: std::path::PathBuf) -> Option<Command> {
        let targets = (0..self.exported_frames().count())
            .map(|n| scan_core::export::numbered(&path, n))
            .collect();
        self.save_frames(targets, export::FrameFormat::Tiff, None)
    }

    /// Why the frames can't be exported with the Export step's settings: nothing stops
    /// them when empty.
    pub fn export_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.result.is_none() {
            problems.push("Scan the film first".into());
        } else if self.crop.frames.is_empty() {
            problems.push("Find the frames in the Crop step first".into());
        } else if self.exported_frames().next().is_none() {
            problems.push("Every frame is skipped".into());
        }
        if self.export.folder.is_none() {
            problems.push("Choose a folder to export to".into());
        }
        problems.extend(self.export.prefix_problem());
        if self.export.write_metadata {
            problems.extend(self.export.metadata.validate());
        }
        problems
    }

    /// Every file an export would write, the frames' then the metadata's.
    pub fn export_paths(&self) -> Vec<std::path::PathBuf> {
        let Some(folder) = &self.export.folder else {
            return Vec::new();
        };
        let count = self.exported_frames().count();
        let mut paths: Vec<_> = (0..count)
            .map(|n| self.export.frame_path(folder, n, count))
            .collect();
        if self.export.write_metadata {
            paths.push(folder.join(export::METADATA_FILE));
        }
        paths
    }

    /// Fill in the roll's film from the Invert step's profile, unless it was already filled
    /// in from that profile (and perhaps edited since). Film not inverted has no profile,
    /// nor does black-and-white film on the generic curve.
    pub fn fill_export_film(&mut self) {
        if self.film().is_none() {
            return;
        }
        if let Some(p) = self.invert.profile_film()
            && self.export.film_from != Some(p.id)
        {
            self.export
                .metadata
                .fill_film(p.manufacturer, p.name, p.iso);
            self.export.film_from = Some(p.id);
        }
    }

    /// Export every frame into the Export step's folder, with the metadata if asked.
    pub fn start_export(&mut self) -> Option<Command> {
        let problems = self.export_problems();
        if !problems.is_empty() {
            self.error = Some(problems.join("; "));
            return None;
        }
        let mut targets = self.export_paths();
        let metadata = self.export.write_metadata.then(|| {
            let path = targets.pop().expect("the metadata's path");
            (path, self.export.metadata.to_yaml())
        });
        self.save_frames(targets, self.export.format, metadata)
    }

    fn save_frames(
        &mut self,
        targets: Vec<std::path::PathBuf>,
        format: export::FrameFormat,
        metadata: Option<(std::path::PathBuf, String)>,
    ) -> Option<Command> {
        if self.is_busy() || self.exported_frames().next().is_none() {
            return None;
        }
        let result = self.result.clone()?;
        let merged = self.current_merged().map(|m| m.merge.clone());
        let cleaned = self.current_cleaned().map(|c| c.removal.clone());
        let frames: Vec<_> = self
            .exported_frames()
            .zip(targets)
            .map(|((_, f), target)| (f.rect, f.adjust.clone(), target))
            .collect();
        self.activity = Activity::Saving;
        self.status = format!("Saving {} frames...", frames.len());
        Some(Command::SaveFrames {
            result,
            merged,
            cleaned,
            film: self.film(),
            frames,
            quarter_turns: self.quarter_turns,
            format,
            metadata,
        })
    }

    /// Save the current result's frames and every edit as the roll file `path` (with the
    /// roll file extension), and the scan beside it, unless it is already there.
    pub fn start_save_roll(&mut self, path: std::path::PathBuf) -> Option<Command> {
        if self.is_busy() {
            return None;
        }
        let path = path.with_extension(session::EXTENSION);
        let roll = Roll::of(self, &path)?;
        let saved = self
            .roll_file
            .as_ref()
            .is_some_and(|(p, g)| *p == path && *g == self.result_generation)
            && roll.scan_path(&path).exists();
        self.activity = Activity::Saving;
        self.status = format!("Saving {}...", path.display());
        Some(Command::SaveRoll {
            scan: if saved { None } else { self.result.clone() },
            roll: Box::new(roll),
            path,
            generation: self.result_generation,
        })
    }

    /// Open the roll file at `path`, in place of the current result.
    pub fn start_open_roll(&mut self, path: std::path::PathBuf) -> Option<Command> {
        if self.is_busy() {
            return None;
        }
        self.activity = Activity::OpeningRoll;
        self.status = format!("Opening {}...", path.display());
        self.error = None;
        Some(Command::OpenRoll(path))
    }

    /// Turn the scan a quarter clockwise, or anticlockwise.
    pub fn turn(&mut self, clockwise: bool) {
        self.quarter_turns = (self.quarter_turns + if clockwise { 1 } else { 3 }) % 4;
    }

    fn detect(&mut self, result: Arc<ScanResult>) -> Option<Command> {
        self.activity = Activity::DetectingFrames;
        self.status = "Finding frames...".into();
        Some(Command::DetectFrames {
            generation: self.result_generation,
            result,
        })
    }

    /// Detect the frames again, replacing any edits.
    pub fn redetect_frames(&mut self) -> Option<Command> {
        let result = self.result.clone()?;
        if self.is_busy() {
            return None;
        }
        self.crop.edited = false;
        self.detect(result)
    }

    /// Replace frame `index`'s rectangle, by hand.
    pub fn edit_frame(&mut self, index: usize, rect: FrameRect) {
        if let Some(frame) = self.crop.frames.get_mut(index)
            && frame.rect != rect
        {
            frame.rect = rect;
            frame.check = false;
            self.crop.edited = true;
        }
    }

    /// Number the frames from the other end of the film, now and for later detections. The
    /// selection and each frame's edits go with their frames.
    pub fn reverse_frames(&mut self) {
        self.forget_settled();
        let crop = &mut self.crop;
        crop.reversed = !crop.reversed;
        crop.frames.reverse();
        let n = crop.frames.len();
        crop.selected = crop.selected.filter(|&i| i < n).map(|i| n - 1 - i);
    }

    pub fn delete_frame(&mut self, index: usize) {
        if index < self.crop.frames.len() {
            self.forget_settled();
            self.crop.frames.remove(index);
            self.crop.edited = true;
            self.crop.selected = None;
        }
    }

    /// Add a frame centred on (x, y) mm and select it. It is shaped like the selected frame,
    /// or the first, or else is a 35 mm frame along the scan's long side.
    pub fn add_frame(&mut self, x_mm: f32, y_mm: f32) {
        let Some(area) = self.result.as_ref().map(|r| r.area) else {
            return;
        };
        let like = self
            .crop
            .selected
            .and_then(|i| self.crop.frames.get(i))
            .or(self.crop.frames.first());
        let rect = match like {
            Some(f) => FrameRect {
                cx_mm: x_mm,
                cy_mm: y_mm,
                ..f.rect
            },
            None => {
                let (w, h) = if area.height_mm >= area.width_mm {
                    (24.0, 36.0)
                } else {
                    (36.0, 24.0)
                };
                FrameRect {
                    cx_mm: x_mm,
                    cy_mm: y_mm,
                    width_mm: w,
                    height_mm: h,
                    angle_deg: 0.0,
                }
            }
        };
        self.forget_settled();
        self.crop.frames.push(CropFrame::new(rect));
        self.crop.edited = true;
        self.crop.selected = Some(self.crop.frames.len() - 1);
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
                        self.crop.selected = None;
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
            Event::FramesDetected {
                generation,
                detection,
                estimated_base,
            } => {
                self.activity = Activity::Idle;
                self.status = detection_summary(&detection);
                if generation == self.result_generation {
                    let base = match (detection.base, estimated_base) {
                        (Some(light), _) => Some((light, true)),
                        (None, Some(light)) => Some((light, false)),
                        (None, None) => None,
                    };
                    self.film_base = base.map(|(light, measured)| FilmBase {
                        generation,
                        light,
                        measured,
                    });
                    self.crop.detected = Some(generation);
                    // Frames edited by hand, before or while it ran, are kept; otherwise each
                    // new frame keeps the edits (and skipping and history) of the old one in
                    // its place.
                    if !self.crop.edited {
                        self.forget_settled();
                        let old = std::mem::take(&mut self.crop.frames);
                        let mut frames: Vec<CropFrame> = detection
                            .frames
                            .iter()
                            .map(|f| CropFrame {
                                check: f.placed_by_spacing,
                                ..carried(&old, &f.rect)
                            })
                            .collect();
                        if self.crop.reversed {
                            frames.reverse();
                        }
                        self.crop = Crop {
                            frames,
                            selected: None,
                            edited: false,
                            detected: Some(generation),
                            strips: detection.strips,
                            reversed: self.crop.reversed,
                        };
                    }
                }
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
            Event::RollSaved {
                path,
                generation,
                result,
            } => {
                self.activity = Activity::Idle;
                match result {
                    Ok(paths) => {
                        self.status = match paths.len() {
                            1 => format!("Saved the roll: {}", path.display()),
                            n => format!(
                                "Saved the roll and its scan ({n} files): {}",
                                path.display()
                            ),
                        };
                        self.roll_file = Some((path, generation));
                    }
                    Err(e) => self.error = Some(format!("Saving the roll failed: {e}")),
                }
                None
            }
            Event::RollOpened { path, result } => {
                self.activity = Activity::Idle;
                match result {
                    Ok((roll, scan)) => {
                        let frames = roll.frames.len();
                        roll.restore(self, scan, path.clone());
                        self.status = format!(
                            "Opened {} with {frames} frame{}",
                            path.display(),
                            if frames == 1 { "" } else { "s" }
                        );
                        return self.update_processing();
                    }
                    Err(e) => {
                        self.status = "Could not open the roll".into();
                        self.error = Some(format!("{}: {e}", path.display()));
                    }
                }
                None
            }
        }
    }
}

/// How big a preview [`AppState::auto_adjust`] measures, on its longer side.
const AUTO_PREVIEW_SIDE: u32 = 1024;

/// How big a preview [`AppState::match_roll`] measures a positive's frames on.
const MATCH_PREVIEW_SIDE: u32 = 512;

/// A frame at `rect`, with the edits, skipping and history of the frame of `old` it
/// replaces: the one whose middle is nearest `rect`'s, within half its shorter side. A
/// preview's frames and a later scan's are the same frames found again, a little more
/// exactly.
fn carried(old: &[CropFrame], rect: &FrameRect) -> CropFrame {
    let reach = rect.width_mm.min(rect.height_mm) / 2.0;
    let replaced = old
        .iter()
        .map(|f| {
            let d = (f.rect.cx_mm - rect.cx_mm).hypot(f.rect.cy_mm - rect.cy_mm);
            (d, f)
        })
        .filter(|&(d, _)| d <= reach)
        .min_by(|a, b| a.0.total_cmp(&b.0));
    match replaced {
        Some((_, f)) => CropFrame {
            rect: *rect,
            ..f.clone()
        },
        None => CropFrame::new(*rect),
    }
}

/// A line on what a frame detection found.
pub fn detection_summary(detection: &FrameDetection) -> String {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut line = match (detection.frames.len(), detection.strips) {
        (_, 0) => "No film strips found: add frames by hand".to_owned(),
        (0, _) => "No frames found on the film: add them by hand".to_owned(),
        (f, s) => format!("Found {f} frame{} on {s} strip{}", plural(f), plural(s)),
    };
    let check = detection
        .frames
        .iter()
        .filter(|f| f.placed_by_spacing)
        .count();
    if check > 0 {
        line += &format!(", {check} placed by their spacing: check them");
    }
    line
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

    /// Apply a finished scan, and the frame detection it starts on film; returns the command
    /// after that.
    fn scan_done(
        state: &mut AppState,
        kind: ScanKind,
        result: Result<ScanResult, ScanError>,
    ) -> Option<Command> {
        let cmd = state.apply(Event::ScanDone { kind, result });
        detected(state, cmd)
    }

    /// Run the frame detection `cmd` asks for, if it is one, and apply its outcome.
    fn detected(state: &mut AppState, cmd: Option<Command>) -> Option<Command> {
        let Some(Command::DetectFrames { generation, result }) = cmd else {
            return cmd;
        };
        let detection = scan_process::detect_frames(
            result.primary(),
            result.gamma,
            result.dpi,
            result.area,
            &Default::default(),
        );
        state.apply(Event::FramesDetected {
            generation,
            detection,
            estimated_base: None,
        })
    }

    /// A preview scan, applied; returns the command that follows it.
    fn preview(state: &mut AppState) -> Option<Command> {
        let Some(Command::Scan { request, kind, .. }) = state.start_scan(ScanKind::Preview) else {
            panic!("expected scan command");
        };
        let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
        let result = dev.scan(&request, &mut NoProgress);
        state.apply(Event::ScanDone { kind, result })
    }

    #[test]
    fn film_scans_find_their_frames() {
        let mut state = with_mock_open();
        let cmd = preview(&mut state);
        assert!(matches!(
            cmd,
            Some(Command::DetectFrames { generation: 1, .. })
        ));
        assert_eq!(state.activity, Activity::DetectingFrames);
        assert!(detected(&mut state, cmd).is_none());
        assert!(!state.is_busy());
        assert_eq!(state.crop.frames.len(), 12);
        assert_eq!((state.crop.detected, state.crop.strips), (Some(1), 2));
        assert_eq!(state.status, "Found 12 frames on 2 strips");
        assert!(state.crop.frames.iter().all(|f| !f.check));

        // A detection of an earlier scan is dropped.
        preview(&mut state);
        state.apply(Event::FramesDetected {
            generation: 1,
            detection: FrameDetection::default(),
            estimated_base: None,
        });
        assert_eq!(state.crop.frames.len(), 12);
        assert_eq!(state.crop.detected, Some(1));
    }

    #[test]
    fn flatbed_scans_are_not_searched_for_frames() {
        let mut state = with_mock_open();
        state.settings.source = ScanSource::Flatbed;
        state.fit_settings();
        assert!(preview(&mut state).is_none());
        assert!(state.crop.frames.is_empty());
        // Asked to, it looks.
        assert!(matches!(
            state.redetect_frames(),
            Some(Command::DetectFrames { .. })
        ));
    }

    #[test]
    fn edited_frames_survive_new_scans_until_detected_again() {
        let mut state = with_mock_open();
        let cmd = preview(&mut state);
        detected(&mut state, cmd);
        let mut rect = state.crop.frames[3].rect;
        rect.angle_deg = 1.5;
        state.edit_frame(3, rect);
        state.delete_frame(11);
        assert!(state.crop.edited);
        assert_eq!(state.crop.frames.len(), 11);

        // The frames are on the bed, so they fit the new scan as they are. The new scan is
        // still searched, for its film base.
        let cmd = preview(&mut state);
        assert!(matches!(cmd, Some(Command::DetectFrames { .. })));
        detected(&mut state, cmd);
        assert_eq!(state.crop.frames.len(), 11);
        assert_eq!(state.current_film_base().map(|b| b.measured), Some(true));
        assert_eq!(state.crop.frames[3].rect.angle_deg, 1.5);

        let cmd = state.redetect_frames();
        assert!(!state.crop.edited);
        detected(&mut state, cmd);
        assert_eq!(state.crop.frames.len(), 12);
        assert!(state.crop.frames[3].rect.angle_deg.abs() < 0.1);
    }

    #[test]
    fn film_scans_are_inverted_against_their_measured_base() {
        let mut state = with_mock_open();
        let cmd = preview(&mut state);
        assert!(
            state.inverter().is_none(),
            "no base until the frames are looked for"
        );
        detected(&mut state, cmd);
        let base = state.current_film_base().unwrap();
        assert!(base.measured);
        for c in 0..3 {
            let want = scan_core::mock::FILM_BASE[c];
            // A preview is 8-bit.
            assert!((base.light[c] / want - 1.0).abs() < 0.03, "{base:?}");
        }
        assert!(state.inverter().is_some());
        let Some(Command::SaveFrames { film: Some(_), .. }) =
            state.start_save_frames("f.tif".into())
        else {
            panic!("expected the frames saved inverted");
        };
        state.activity = Activity::Idle;

        state.invert.enabled = false;
        assert!(state.inverter().is_none());
        state.invert.enabled = true;

        // The mock's frames are lit evenly, so the roll comes out near neutral.
        state.auto_printing();
        assert!(
            state.status.starts_with("Auto from 12 frames"),
            "{}",
            state.status
        );
        let p = state.invert.printing;
        assert!(p.balance_ev.iter().all(|b| b.abs() < 0.5), "{p:?}");

        // A new scan needs its own base: the old one isn't used for it.
        preview(&mut state);
        assert!(state.inverter().is_none());
    }

    #[test]
    fn flatbed_scans_are_not_inverted() {
        let mut state = with_mock_open();
        state.settings.source = ScanSource::Flatbed;
        state.fit_settings();
        preview(&mut state);
        assert!(state.inverter().is_none());
    }

    #[test]
    fn frames_are_added_like_the_others() {
        let mut state = with_mock_open();
        preview(&mut state);
        state.activity = Activity::Idle;
        // With no frames, a 35 mm frame along the film area's long side.
        state.add_frame(30.0, 50.0);
        let first = state.crop.frames[0].clone();
        assert_eq!(
            (first.rect.width_mm, first.rect.height_mm, first.rect.cx_mm),
            (24.0, 36.0, 30.0)
        );
        assert_eq!(state.crop.selected, Some(0));
        assert!(state.crop.edited);

        let mut turned = first.rect;
        turned.width_mm = 30.0;
        turned.angle_deg = 90.0;
        state.edit_frame(0, turned);
        state.add_frame(60.0, 100.0);
        let second = state.crop.frames[1].rect;
        assert_eq!((second.width_mm, second.angle_deg), (30.0, 90.0));
        assert_eq!((second.cx_mm, second.cy_mm), (60.0, 100.0));
        assert_eq!(state.crop.selected, Some(1));
        state.delete_frame(0);
        assert_eq!(state.crop.frames.len(), 1);
        assert_eq!(state.crop.selected, None);
    }

    #[test]
    fn frames_and_scans_are_saved_turned() {
        let mut state = with_mock_open();
        assert!(state.start_save_frames("f.tif".into()).is_none(), "no scan");
        let cmd = preview(&mut state);
        detected(&mut state, cmd);
        state.turn(false);
        assert_eq!(state.quarter_turns, 3);
        state.turn(true);
        state.turn(true);
        assert_eq!(state.quarter_turns, 1);

        let Some(Command::SaveFrames {
            frames,
            quarter_turns,
            ..
        }) = state.start_save_frames("f.tif".into())
        else {
            panic!("expected the frames saved");
        };
        assert_eq!((frames.len(), quarter_turns), (12, 1));
        assert!(state.start_save_frames("f.tif".into()).is_none(), "busy");
        state.activity = Activity::Idle;
        let Some(Command::Save { quarter_turns, .. }) = state.start_save("s.tif".into()) else {
            panic!("expected a save");
        };
        assert_eq!(quarter_turns, 1);
    }

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
        scan_done(&mut state, kind, result);
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
        scan_done(state, kind, result)
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
        scan_done(state, kind, result)
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
        assert!(scan_done(&mut state, kind, result).is_none());
        assert!(state.current_cleaned().is_none());
    }

    /// A film preview with its frames found.
    fn film_with_frames() -> AppState {
        let mut state = with_mock_open();
        let cmd = preview(&mut state);
        detected(&mut state, cmd);
        assert_eq!(state.crop.frames.len(), 12);
        state
    }

    #[test]
    fn each_frame_keeps_its_own_adjustments_through_a_new_scan() {
        let mut state = film_with_frames();
        state.adjustments_mut(Some(3)).exposure_ev = 0.7;
        state.adjustments_mut(Some(3)).quarter_turns = 1;
        state.adjustments_mut(None).saturation = 0.2;
        assert_eq!(state.adjustments(Some(4)), &Adjustments::default());
        assert!(
            !state.crop.edited,
            "editing a frame's colors isn't editing the frames"
        );

        // A new scan of the film finds the frames again, each keeping its edits.
        let cmd = preview(&mut state);
        detected(&mut state, cmd);
        assert_eq!(state.crop.frames.len(), 12);
        assert_eq!(state.adjustments(Some(3)).exposure_ev, 0.7);
        assert_eq!(state.adjustments(Some(3)).quarter_turns, 1);
        assert_eq!(state.adjustments(Some(2)), &Adjustments::default());
        assert_eq!(state.scan_adjust.saturation, 0.2);
        // A frame out of range is the whole scan.
        assert_eq!(state.adjustments(Some(99)).saturation, 0.2);

        // Saving frames saves each with its own.
        let Some(Command::SaveFrames { frames, film, .. }) =
            state.start_save_frames("f.tif".into())
        else {
            panic!("expected save");
        };
        assert!(film.is_some());
        assert_eq!(frames[3].1.exposure_ev, 0.7);
        assert_eq!(frames[0].1, Adjustments::default());
        state.activity = Activity::Idle;
        let Some(Command::Save { adjust, .. }) = state.start_save("s.tif".into()) else {
            panic!("expected save");
        };
        assert_eq!(adjust.saturation, 0.2);
    }

    #[test]
    fn export_saves_every_frame_with_the_publishers_metadata() {
        let mut state = film_with_frames();
        assert!(state.export_problems().iter().any(|p| p.contains("folder")));
        assert!(state.start_export().is_none());
        assert!(
            state
                .error
                .take()
                .unwrap()
                .contains("Roll name is required")
        );

        state.export.folder = Some("out".into());
        state.fill_export_film();
        let stock = state.invert.stock;
        let meta = &mut state.export.metadata;
        assert_eq!(
            (&*meta.manufacturer, &*meta.iso),
            (stock.manufacturer, "200")
        );
        // Filling in again from the same profile keeps what was typed since.
        meta.stock = "Gold".into();
        state.fill_export_film();
        let meta = &mut state.export.metadata;
        assert_eq!(meta.stock, "Gold");
        meta.name = "Halloween".into();
        meta.date_captured = "10/31/2026".into();
        meta.camera_make = "Nikon".into();
        meta.camera_model = "FM2".into();
        assert!(
            state.export_problems().is_empty(),
            "{:?}",
            state.export_problems()
        );

        let out = std::path::Path::new("out");
        assert_eq!(state.export_paths().len(), 13);
        let Some(Command::SaveFrames {
            frames,
            format,
            metadata,
            ..
        }) = state.start_export()
        else {
            panic!("expected an export");
        };
        assert_eq!(format, export::FrameFormat::Jpeg(95));
        assert_eq!(frames.len(), 12);
        assert_eq!(frames[11].2, out.join("roll_12.jpg"));
        let (path, text) = metadata.unwrap();
        assert_eq!(path, out.join(export::METADATA_FILE));
        assert!(
            text.contains(
                "  name: \"Halloween\"
"
            ),
            "{text}"
        );

        // Without metadata, nothing about the roll is needed.
        state.activity = Activity::Idle;
        state.export.write_metadata = false;
        state.export.metadata = Default::default();
        assert_eq!(state.export_paths().len(), 12);
        let Some(Command::SaveFrames { metadata: None, .. }) = state.start_export() else {
            panic!("expected an export without metadata");
        };
    }

    #[test]
    fn frames_can_be_numbered_from_the_other_end() {
        let mut state = film_with_frames();
        let rects: Vec<FrameRect> = state.crop.frames.iter().map(|f| f.rect).collect();
        state.adjustments_mut(Some(0)).exposure_ev = 0.5;
        state.crop.selected = Some(1);
        state.reverse_frames();
        assert!(state.crop.reversed && !state.crop.edited);
        assert_eq!(state.crop.frames[0].rect, rects[11]);
        assert_eq!(
            state.crop.frames[11].adjust.exposure_ev, 0.5,
            "edits go with the frame"
        );
        assert_eq!(state.crop.selected, Some(10));

        // Later detections come out reversed too, and back the other way when turned off.
        let cmd = preview(&mut state);
        detected(&mut state, cmd);
        assert!(state.crop.reversed);
        assert_eq!(state.crop.frames[0].rect, rects[11]);
        assert_eq!(state.crop.frames[11].adjust.exposure_ev, 0.5);
        state.reverse_frames();
        let cmd = state.redetect_frames();
        detected(&mut state, cmd);
        assert_eq!(state.crop.frames[0].rect, rects[0]);
    }

    #[test]
    fn adjustments_copy_paste_and_reset_but_keep_each_frames_turn() {
        let mut state = film_with_frames();
        let a = state.adjustments_mut(Some(0));
        a.contrast = 0.4;
        a.balance_ev = [0.1, 0.0, -0.1];
        a.mirror = true;
        state.adjustments_mut(Some(1)).quarter_turns = 3;
        state.copy_adjustments(Some(0));
        state.paste_adjustments(Some(1));
        let b = state.adjustments(Some(1));
        assert_eq!((b.contrast, b.quarter_turns, b.mirror), (0.4, 3, false));
        state.paste_to_all_frames();
        assert!(state.crop.frames.iter().all(|f| f.adjust.contrast == 0.4));
        assert_eq!(
            state.scan_adjust,
            Adjustments::default(),
            "the whole scan isn't a frame"
        );
        state.reset_adjustments(Some(1));
        let b = state.adjustments(Some(1));
        assert_eq!((b.contrast, b.quarter_turns), (0.0, 3));
        assert!(state.adjustments(Some(0)).mirror);
    }

    #[test]
    fn the_grey_picker_neutralises_the_point_in_scene_light() {
        let mut state = film_with_frames();
        // Some light through the film: an orange-ish negative's.
        let light = [0.30, 0.12, 0.03];
        state.adjustments_mut(Some(2)).exposure_ev = 0.5;
        state.pick_neutral(Some(2), light);
        let adj = state.adjustments(Some(2)).clone();
        assert_eq!(adj.exposure_ev, 0.5, "exposure kept");
        let inverter = state.film().unwrap().inverter(&adj);
        let e: Vec<f32> = (0..3).map(|c| inverter.scene(c, light[c])).collect();
        assert!(
            (e[0] / e[1] - 1.0).abs() < 1e-3 && (e[1] / e[2] - 1.0).abs() < 1e-3,
            "{e:?}"
        );
        assert!(state.status.starts_with("Grey picked"));

        // On a positive, the same in linear light.
        state.invert.enabled = false;
        state.pick_neutral(None, [0.4, 0.2, 0.1]);
        let g = state.scan_adjust.gains();
        assert!((0.4 * g[0] - 0.2 * g[1]).abs() < 1e-4 && (0.2 * g[1] - 0.1 * g[2]).abs() < 1e-4);
    }

    #[test]
    fn level_pickers_set_each_channels_point() {
        let mut state = film_with_frames();
        state.pick_level(Some(0), [0.9, 0.85, 0.8], true);
        state.pick_level(Some(0), [0.05, 0.1, 0.12], false);
        let l = &state.adjustments(Some(0)).levels;
        assert_eq!((l[1].white, l[2].white, l[3].white), (0.9, 0.85, 0.8));
        assert_eq!((l[1].black, l[3].black), (0.05, 0.12));
        assert_eq!(l[adjust::MASTER], Levels::default());
        // Never crossing.
        state.pick_level(Some(0), [0.01; 3], true);
        assert!(state.adjustments(Some(0)).levels[1].white >= 0.07 - 1e-6);
    }

    #[test]
    fn auto_balances_a_frame_on_its_own_and_stretches_it() {
        let mut state = film_with_frames();
        state.auto_adjust(Some(5));
        assert!(state.status.starts_with("Auto:"), "{}", state.status);
        let adj = state.adjustments(Some(5)).clone();
        let l = adj.levels[adjust::MASTER];
        assert!(l.black < l.white);
        // The frame's printing is what the roll's Auto would give it alone.
        let film = state.film().unwrap();
        let result = state.result.clone().unwrap();
        let levels = scan_process::frame_levels(
            state.processed_image().unwrap(),
            result.gamma,
            result.dpi,
            result.area,
            &[state.crop.frames[5].rect],
            &film.neutral(),
        );
        let alone = scan_film::auto_printing(&levels, &film.printing);
        let p = adj.printing(&film.printing);
        assert!((p.exposure_ev - alone.exposure_ev).abs() < 1e-4);
        for c in 0..3 {
            assert!((p.balance_ev[c] - alone.balance_ev[c]).abs() < 1e-4);
        }
        // Other frames are left alone.
        assert_eq!(state.adjustments(Some(4)), &Adjustments::default());

        // The whole scan of a negative keeps the roll's printing.
        state.adjustments_mut(None).exposure_ev = 1.0;
        state.auto_adjust(None);
        assert_eq!(state.scan_adjust.exposure_ev, 0.0);
    }

    #[test]
    fn changes_are_undone_one_gesture_at_a_time() {
        let mut state = film_with_frames();
        state.track_history(true);
        // A slider dragged over several frames of the UI is one change.
        for ev in [0.1, 0.2, 0.3] {
            state.adjustments_mut(Some(1)).exposure_ev = ev;
            state.track_history(false);
        }
        state.track_history(true);
        state.adjustments_mut(Some(1)).contrast = 0.5;
        state.adjustments_mut(None).saturation = 0.2;
        state.track_history(true);
        assert!(state.history(Some(1)).can_undo() && state.history(None).can_undo());
        assert!(!state.history(Some(0)).can_undo());

        assert!(state.undo(Some(1)));
        assert_eq!(state.adjustments(Some(1)).contrast, 0.0);
        assert_eq!(state.adjustments(Some(1)).exposure_ev, 0.3);
        // Undoing isn't itself recorded as a change.
        state.track_history(true);
        assert!(state.undo(Some(1)));
        assert_eq!(state.adjustments(Some(1)), &Adjustments::default());
        assert!(!state.undo(Some(1)));
        assert!(state.redo(Some(1)));
        assert_eq!(state.adjustments(Some(1)).exposure_ev, 0.3);
        assert!(state.undo(None));
        assert_eq!(state.scan_adjust.saturation, 0.0);

        // Pasting onto every frame is undone frame by frame.
        state.adjustments_mut(Some(4)).brightness = 0.3;
        state.copy_adjustments(Some(4));
        state.paste_to_all_frames();
        state.track_history(true);
        assert!(state.undo(Some(7)));
        assert_eq!(state.adjustments(Some(7)).brightness, 0.0);

        // Frames moved around aren't recorded as edits: each keeps its own history.
        let last = state.crop.frames[11].history.clone();
        state.reverse_frames();
        state.track_history(true);
        assert_eq!(state.crop.frames[0].history, last);
    }

    #[test]
    fn skipped_frames_are_left_out_of_saves_and_exports() {
        let mut state = film_with_frames();
        state.crop.frames[0].skip = true;
        state.crop.frames[4].skip = true;
        state.adjustments_mut(Some(1)).exposure_ev = 0.6;
        let Some(Command::SaveFrames { frames, .. }) = state.start_save_frames("f.tif".into())
        else {
            panic!("expected a save");
        };
        assert_eq!(frames.len(), 10);
        // Numbered without gaps, from frame 2.
        assert_eq!(frames[0].1.exposure_ev, 0.6);
        assert_eq!(frames[0].2, std::path::Path::new("f_01.tif"));
        state.activity = Activity::Idle;
        state.export.folder = Some("out".into());
        state.export.write_metadata = false;
        let paths = state.export_paths();
        assert_eq!(paths.len(), 10);
        assert!(paths[9].ends_with("roll_10.jpg"));
        for frame in &mut state.crop.frames {
            frame.skip = true;
        }
        assert!(
            state
                .export_problems()
                .contains(&"Every frame is skipped".to_owned())
        );
        assert!(state.start_export().is_none());
        // Skipping goes with the frame through a new scan.
        state.crop.frames[3].skip = false;
        let cmd = preview(&mut state);
        detected(&mut state, cmd);
        assert!(!state.crop.frames[3].skip && state.crop.frames[2].skip);
    }

    #[test]
    fn auto_all_and_match_roll_set_every_frame() {
        let mut state = film_with_frames();
        state.auto_all_frames();
        assert!(
            state
                .crop
                .frames
                .iter()
                .all(|f| f.adjust.levels[adjust::MASTER] != Levels::default())
        );
        assert!(state.status.contains("12 frames"), "{}", state.status);

        // Matched, every frame measures the same through its printing.
        let mut state = film_with_frames();
        state.adjustments_mut(Some(2)).temperature = 0.5;
        state.adjustments_mut(Some(2)).contrast = 0.3;
        state.match_roll();
        assert!(state.status.starts_with("Matched 12"), "{}", state.status);
        let film = state.film().unwrap();
        let result = state.result.clone().unwrap();
        let image = state.processed_image().unwrap();
        let printed: Vec<[f32; 3]> = state
            .crop
            .frames
            .iter()
            .map(|f| {
                let level = scan_process::frame_levels(
                    image,
                    result.gamma,
                    result.dpi,
                    result.area,
                    &[f.cut()],
                    &film.neutral(),
                )[0];
                let ev = f.adjust.color_ev();
                std::array::from_fn(|c| {
                    level[c] / std::f32::consts::LOG10_2 + f.adjust.exposure_ev + ev[c]
                })
            })
            .collect();
        for p in &printed[1..] {
            for c in 0..3 {
                assert!(
                    (p[c] - printed[0][c]).abs() < 1e-3,
                    "{p:?} {:?}",
                    printed[0]
                );
            }
        }
        let a = state.adjustments(Some(2));
        assert_eq!((a.temperature, a.contrast), (0.5, 0.3), "other edits kept");
    }

    #[test]
    fn presets_hold_edits_without_geometry() {
        let mut state = film_with_frames();
        let a = state.adjustments_mut(Some(0));
        a.vibrance = 0.4;
        a.quarter_turns = 1;
        a.straighten_deg = 2.0;
        state.save_preset("  Gold sunny ", Some(0));
        assert_eq!(state.presets.len(), 1);
        assert_eq!(state.presets[0].name, "Gold sunny");
        assert_eq!(state.presets[0].adjust.straighten_deg, 0.0);
        // The same name replaces it; a blank one saves nothing.
        state.adjustments_mut(Some(0)).vibrance = 0.5;
        state.save_preset("Gold sunny", Some(0));
        state.save_preset(" ", Some(0));
        assert_eq!(state.presets.len(), 1);

        state.adjustments_mut(Some(3)).straighten_deg = -1.0;
        state.apply_preset(0, Some(3));
        let b = state.adjustments(Some(3));
        assert_eq!(
            (b.vibrance, b.straighten_deg, b.quarter_turns),
            (0.5, -1.0, 0)
        );
        state.apply_preset_to_all(0);
        assert!(state.crop.frames.iter().all(|f| f.adjust.vibrance == 0.5));
        assert_eq!(state.adjustments(Some(0)).quarter_turns, 1);
    }

    #[test]
    fn black_and_white_and_slides_print_without_a_profile() {
        let mut state = film_with_frames();
        state.invert.film_type = FilmType::BlackAndWhite;
        let film = state
            .film()
            .expect("a negative needs its base, which is known");
        assert!(film.inverter(&Adjustments::default()).is_mono());
        // The export's film isn't filled in from the color profile.
        state.fill_export_film();
        assert!(state.export.metadata.manufacturer.is_empty());
        // With a black-and-white stock, it inverts on that film's curve, and the export's
        // film comes from it.
        let generic = film.inverter(&Adjustments::default());
        state.invert.mono_stock = scan_film::stock::mono_by_id("ilford-hp5-plus");
        let hp5 = state.film().unwrap().inverter(&Adjustments::default());
        assert!(hp5.is_mono());
        let light = film.base.map(|b| b * 0.05);
        assert_ne!(hp5.pixel(light), generic.pixel(light));
        state.fill_export_film();
        let m = &state.export.metadata;
        assert_eq!(
            (&*m.manufacturer, &*m.stock, &*m.iso),
            ("Ilford", "HP5 Plus", "400")
        );
        state.invert.mono_stock = None;

        state.invert.film_type = FilmType::Slide;
        state.film_base = None;
        let film = state.film().expect("a slide needs no base");
        assert_eq!(film.base, [1.0; 3]);
        state.auto_printing();
        assert!(
            state.status.starts_with("Auto from 12 frames"),
            "{}",
            state.status
        );
        state.invert.film_type = FilmType::ColorNegative;
        assert!(state.film().is_none(), "a color negative does");
        for t in FilmType::ALL {
            assert_eq!(FilmType::from_key(t.key()), Some(t));
        }
    }

    #[test]
    fn frames_are_cut_with_their_border_and_straightening() {
        let mut state = film_with_frames();
        let rect = state.crop.frames[0].rect;
        let a = state.adjustments_mut(Some(0));
        a.border_mm = 1.0;
        let cut = state.crop.frames[0].cut();
        assert_eq!(cut.width_mm, rect.width_mm + 2.0);
        // Pasting and resetting keep a frame's straightening, but not its border.
        state.adjustments_mut(Some(0)).straighten_deg = 1.0;
        state.reset_adjustments(Some(0));
        let a = state.adjustments(Some(0));
        assert_eq!((a.straighten_deg, a.border_mm), (1.0, 0.0));
    }

    #[test]
    fn scan_without_device_is_refused() {
        let mut state = AppState::default();
        assert!(state.start_scan(ScanKind::Preview).is_none());
        assert!(state.build_request(ScanKind::Preview).is_err());
    }
}
