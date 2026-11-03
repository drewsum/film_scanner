//! egui front end: workflow stepper, scan controls, image view and status bar.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use eframe::egui::{self, Align, Layout, RichText};
use scan_core::image::DynamicImage;
use scan_core::mock::MockBackend;
use scan_core::{BitDepth, ColorMode, DeviceRegistry, ScanSource};
use scan_film::render::CONTRAST_RANGE;
use scan_film::{Mono, Printing, Rendering, Toning};

use crate::crop::{BedView, CropEditor};
use crate::edit::{self, Editor};
use crate::export::{self, ExportSettings, FrameFormat};
use crate::history::Preset;
use crate::session;
use crate::state::{self, Activity, AppState, EXTRA_EV, FilmType, ViewChannel, WorkflowStep};
use crate::strip::Filmstrip;
use crate::view::{self, ImageView};
use crate::worker::{Event, ScanKind, Worker};

/// Where the scan's turn is remembered between sessions.
const TURNS_KEY: &str = "quarter_turns";
/// Where the film stock and type are remembered: the roll in the scanner is likely the
/// same next time.
const STOCK_KEY: &str = "film_stock";
const FILM_TYPE_KEY: &str = "film_type";
/// The black-and-white stock's id; empty for the generic curve.
const MONO_STOCK_KEY: &str = "mono_stock";
/// The Adjust step's presets, as RON.
const PRESETS_KEY: &str = "presets";

/// Where the Export step's settings are remembered, with the camera fields under
/// `export.<field>`: the same camera and folder are likely next time, not the same roll.
const EXPORT_FOLDER_KEY: &str = "export.folder";
const EXPORT_PREFIX_KEY: &str = "export.prefix";
/// `jpeg <quality>`, `png` or `tiff`.
const EXPORT_FORMAT_KEY: &str = "export.format";
const EXPORT_METADATA_KEY: &str = "export.write_metadata";

/// All backends compiled into this build, real hardware first.
pub fn default_registry() -> DeviceRegistry {
    let mut registry = DeviceRegistry::new();
    scan_epson::register(&mut registry);
    scan_wia::register(&mut registry);
    scan_sane::register(&mut registry);
    registry.register(MockBackend);
    registry
}

pub struct ScannerApp {
    state: AppState,
    worker: Worker,
    view: ImageView,
    crop_editor: CropEditor,
    editor: Editor,
    filmstrip: Filmstrip,
    /// The middle of the image view on the bed, in millimetres, as last shown.
    view_centre: Option<(f32, f32)>,
    /// The dust strength slider, applied when it is let go.
    dust_strength: f32,
    /// The processed image scaled to the view, and which result and image it is of.
    thumbnail: Option<((u64, u64), Arc<DynamicImage>)>,
}

impl ScannerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let worker = Worker::spawn(default_registry(), move || ctx.request_repaint());
        let mut state = AppState::default();
        // Film goes in the holder the same way every time, so the turn carries over.
        state.quarter_turns = cc
            .storage
            .and_then(|s| s.get_string(TURNS_KEY))
            .and_then(|s| s.parse::<u8>().ok())
            .map_or(0, |t| t % 4);
        if let Some(stock) = cc
            .storage
            .and_then(|s| s.get_string(STOCK_KEY))
            .and_then(|id| scan_film::stock::by_id(&id))
        {
            state.invert.stock = stock;
        }
        state.invert.mono_stock = cc
            .storage
            .and_then(|s| s.get_string(MONO_STOCK_KEY))
            .and_then(|id| scan_film::stock::mono_by_id(&id));
        if let Some(film_type) = cc
            .storage
            .and_then(|s| s.get_string(FILM_TYPE_KEY))
            .and_then(|key| FilmType::from_key(&key))
        {
            state.invert.film_type = film_type;
        }
        if let Some(storage) = cc.storage {
            restore_export(&mut state.export, storage);
            state.presets =
                eframe::get_value::<Vec<Preset>>(storage, PRESETS_KEY).unwrap_or_default();
        }
        worker.send(state.refresh_devices());
        Self {
            dust_strength: state.dust.strength,
            state,
            worker,
            view: ImageView::default(),
            crop_editor: CropEditor::default(),
            editor: Editor::default(),
            filmstrip: Filmstrip::default(),
            view_centre: None,
            thumbnail: None,
        }
    }

    fn process_events(&mut self) {
        while let Some(event) = self.worker.try_recv() {
            let opened = matches!(event, Event::RollOpened { result: Ok(_), .. });
            if let Some(command) = self.state.apply(event) {
                self.worker.send(command);
            }
            if opened {
                self.dust_strength = self.state.dust.strength;
                self.thumbnail = None;
            }
        }
    }

    fn workflow_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Film Scanner");
            ui.separator();
            for (i, step) in WorkflowStep::ALL.into_iter().enumerate() {
                if i > 0 {
                    ui.label("›");
                }
                let label = format!("{} {}", i + 1, step.label());
                ui.selectable_value(&mut self.state.step, step, label)
                    .on_hover_text(step.description());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let busy = self.state.is_busy();
                if ui
                    .add_enabled(
                        !busy && self.state.result.is_some(),
                        egui::Button::new("Save roll..."),
                    )
                    .on_hover_text(
                        "Saves the scan, its frames and every edit as a roll file, to open \
                         again later (Ctrl+S)",
                    )
                    .clicked()
                {
                    self.save_roll(true);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Open roll..."))
                    .on_hover_text("Opens a roll saved earlier, with its frames and edits (Ctrl+O)")
                    .clicked()
                {
                    self.open_roll();
                }
            });
        });
    }

    /// Save the roll: to its file, if it has one and `ask` is false, else where the user
    /// chooses.
    fn save_roll(&mut self, ask: bool) {
        let known = self
            .state
            .roll_file
            .as_ref()
            .filter(|(_, g)| *g == self.state.result_generation)
            .map(|(p, _)| p.clone());
        let path = match known {
            Some(path) if !ask => Some(path),
            known => {
                let mut dialog = rfd::FileDialog::new()
                    .add_filter("Film Scanner roll", &[session::EXTENSION])
                    .set_file_name(format!(
                        "{}.{}",
                        self.state.export.prefix.trim(),
                        session::EXTENSION
                    ));
                let folder = known
                    .as_ref()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    .or(self.state.export.folder.clone());
                if let Some(folder) = folder {
                    dialog = dialog.set_directory(folder);
                }
                dialog.save_file()
            }
        };
        if let Some(cmd) = path.and_then(|p| self.state.start_save_roll(p)) {
            self.worker.send(cmd);
        }
    }

    fn open_roll(&mut self) {
        if let Some(cmd) = rfd::FileDialog::new()
            .add_filter("Film Scanner roll", &[session::EXTENSION])
            .pick_file()
            .and_then(|p| self.state.start_open_roll(p))
        {
            self.worker.send(cmd);
        }
    }

    /// Shortcuts for every step, then the Adjust step's.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() || self.state.is_busy() {
            return;
        }
        let command = |key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, key));
        if command(egui::Key::S) && self.state.result.is_some() {
            self.save_roll(false);
        }
        if command(egui::Key::O) {
            self.open_roll();
        }
        let step = self.state.step;
        if !matches!(step, WorkflowStep::Adjust | WorkflowStep::Export) {
            return;
        }
        // Arrows move a focused slider, so they step through frames only when none is.
        let free = ctx.memory(|m| m.focused().is_none());
        let key = |key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
        let n = self.state.crop.frames.len();
        let current = edit::target(&self.state);
        let step_by = if key(egui::Key::PageDown) || (free && key(egui::Key::ArrowRight)) {
            1
        } else if key(egui::Key::PageUp) || (free && key(egui::Key::ArrowLeft)) {
            -1
        } else {
            0
        };
        if step_by != 0 && n > 0 {
            let next = match current {
                Some(i) => (i as isize + step_by).clamp(0, n as isize - 1) as usize,
                None if step_by > 0 => 0,
                None => n - 1,
            };
            self.editor.select(&mut self.state, Some(next));
        }
        if step == WorkflowStep::Adjust {
            self.editor.shortcuts(ctx, &mut self.state);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match self.state.activity {
                Activity::Idle => {}
                Activity::Scanning { progress, .. } => {
                    if ui.button("Cancel").clicked() {
                        self.worker.cancel();
                    }
                    ui.add(
                        egui::ProgressBar::new(progress)
                            .desired_width(200.0)
                            .show_percentage(),
                    );
                }
                _ => {
                    ui.spinner();
                }
            }
            ui.label(&self.state.status);
            if let Some(err) = &self.state.error {
                ui.separator();
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
        });
    }

    fn scan_controls(&mut self, ui: &mut egui::Ui) {
        let busy = self.state.is_busy();
        ui.heading("Scanner");
        ui.horizontal(|ui| {
            let selected = self.state.selected_device().map(|d| d.display_name());
            let mut choice = self.state.selected;
            ui.add_enabled_ui(!busy, |ui| {
                egui::ComboBox::from_id_salt("device")
                    .width(200.0)
                    .selected_text(selected.unwrap_or_else(|| "No scanner".into()))
                    .show_ui(ui, |ui| {
                        for (i, device) in self.state.devices.iter().enumerate() {
                            ui.selectable_value(&mut choice, Some(i), device.display_name());
                        }
                    });
            });
            if choice != self.state.selected
                && let Some(cmd) = choice.and_then(|i| self.state.select_device(i))
            {
                self.worker.send(cmd);
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Refresh"))
                .clicked()
            {
                let cmd = self.state.refresh_devices();
                self.worker.send(cmd);
            }
        });

        let Some(caps) = self.state.capabilities.clone() else {
            ui.label("Select a scanner to see its options.");
            return;
        };
        ui.add_space(8.0);
        ui.heading("Settings");
        let before = self.state.settings.clone();
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("settings")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    let s = &mut self.state.settings;
                    ui.label("Source");
                    egui::ComboBox::from_id_salt("source")
                        .selected_text(s.source.label())
                        .show_ui(ui, |ui| {
                            for src in &caps.sources {
                                ui.selectable_value(&mut s.source, src.source, src.source.label());
                            }
                        });
                    ui.end_row();

                    let Some(src) = caps.source(s.source) else {
                        return;
                    };
                    ui.label("Resolution");
                    egui::ComboBox::from_id_salt("dpi")
                        .selected_text(format!("{} dpi", s.dpi))
                        .show_ui(ui, |ui| {
                            for &dpi in &src.resolutions {
                                ui.selectable_value(&mut s.dpi, dpi, format!("{dpi} dpi"));
                            }
                        });
                    ui.end_row();

                    ui.label("Bit depth");
                    egui::ComboBox::from_id_salt("depth")
                        .selected_text(format!("{}-bit", s.bit_depth.bits()))
                        .show_ui(ui, |ui| {
                            for &depth in &src.bit_depths {
                                ui.selectable_value(
                                    &mut s.bit_depth,
                                    depth,
                                    format!("{}-bit", depth.bits()),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label("Color");
                    let mode_label = |m: ColorMode| match m {
                        ColorMode::Color => "Color",
                        ColorMode::Gray => "Greyscale",
                    };
                    egui::ComboBox::from_id_salt("mode")
                        .selected_text(mode_label(s.color_mode))
                        .show_ui(ui, |ui| {
                            for &mode in &src.color_modes {
                                ui.selectable_value(&mut s.color_mode, mode, mode_label(mode));
                            }
                        });
                    ui.end_row();

                    ui.label("Infrared");
                    ui.add_enabled(
                        src.infrared,
                        egui::Checkbox::new(&mut s.infrared, "Dust removal channel"),
                    )
                    .on_disabled_hover_text("Not supported by this scanner/source");
                    ui.end_row();

                    ui.label("Exposures");
                    ui.add_enabled(
                        src.max_exposure_passes >= 2,
                        egui::Checkbox::new(
                            &mut s.multi_exposure,
                            format!("Extra {EXTRA_EV:+} EV pass"),
                        ),
                    )
                    .on_hover_text(
                        "Scans again, brighter, and merges the two: less noise in the densest \
                         film (a negative's highlights, a slide's shadows). Takes about twice \
                         as long. Not used for previews.",
                    )
                    .on_disabled_hover_text("Not supported by this scanner/source");
                    ui.end_row();

                    let (w, h) = src.max_area.pixel_size(s.dpi);
                    let bytes = w as u64
                        * h as u64
                        * 3
                        * if s.bit_depth == BitDepth::Sixteen {
                            2
                        } else {
                            1
                        };
                    ui.label("Full area");
                    ui.label(format!(
                        "{:.0} x {:.0} mm, {w} x {h} px (~{} MB)",
                        src.max_area.width_mm,
                        src.max_area.height_mm,
                        bytes / 1_000_000
                    ));
                    ui.end_row();
                });
        });
        if self.state.settings != before {
            self.state.fit_settings();
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            for kind in [ScanKind::Preview, ScanKind::Final] {
                if ui
                    .add_enabled(!busy, egui::Button::new(kind.label()))
                    .clicked()
                    && let Some(cmd) = self.state.start_scan(kind)
                {
                    self.worker.send(cmd);
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.heading("Result");
        let Some(result) = self.state.result.clone() else {
            ui.label("No scan yet.");
            return;
        };
        let (w, h) = result.dimensions();
        ui.label(format!(
            "{}: {w} x {h} px, {} dpi, {} pass(es){}",
            self.state.result_kind.map_or("Scan", |k| k.label()),
            result.dpi,
            result.passes.len(),
            if result.infrared.is_some() {
                " + IR"
            } else {
                ""
            }
        ));
        if result.passes.len() > 1 {
            self.merge_controls(ui, busy);
        }
        if result.infrared.is_some() {
            self.dust_controls(ui, busy);
        } else if self.state.source_caps().is_some_and(|s| s.infrared) {
            ui.label(RichText::new("Scan with Infrared on to remove dust.").weak());
        }
        self.save_scan_button(ui, busy);
    }

    fn save_scan_button(&mut self, ui: &mut egui::Ui, busy: bool) {
        if ui
            .add_enabled(!busy, egui::Button::new("Save as TIFF..."))
            .on_hover_text("Saves the whole scan, with the Adjust step's whole-scan edits")
            .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("TIFF image", &["tif", "tiff"])
                .set_file_name("scan.tif")
                .save_file()
            && let Some(cmd) = self.state.start_save(path)
        {
            self.worker.send(cmd);
        }
    }

    fn merge_controls(&mut self, ui: &mut egui::Ui, busy: bool) {
        ui.add_space(4.0);
        let before = self.state.merge_exposures;
        ui.add_enabled(
            !busy,
            egui::Checkbox::new(&mut self.state.merge_exposures, "Merge exposures"),
        )
        .on_hover_text("Off shows and saves the normal exposure alone, to compare");
        if let Some(merged) = self.state.current_merged() {
            let details: Vec<String> = merged
                .merge
                .passes
                .iter()
                .map(|p| {
                    format!(
                        "{:+.1} EV pass: gains {:.2} / {:.2} / {:.2}, shifted {:.2}, {:.2} px",
                        p.ev, p.gain[0], p.gain[1], p.gain[2], p.shift.0, p.shift.1
                    )
                })
                .collect();
            let summary = state::merge_summary(&merged.merge);
            ui.label(summary.trim_start_matches("Exposures merged: "))
                .on_hover_text(details.join("\n"));
        }
        if self.state.merge_exposures != before
            && let Some(cmd) = self.state.update_processing()
        {
            self.worker.send(cmd);
        }
    }

    fn dust_controls(&mut self, ui: &mut egui::Ui, busy: bool) {
        ui.add_space(4.0);
        let before = self.state.dust;
        ui.add_enabled_ui(!busy, |ui| {
            let dust = &mut self.state.dust;
            ui.checkbox(&mut dust.enabled, "Remove dust and scratches")
                .on_hover_text(
                    "Finds defects in the infrared channel and fills them in. Not for \
                     black-and-white film or Kodachrome, which block infrared.",
                );
            ui.add_enabled_ui(dust.enabled, |ui| {
                let slider = ui
                    .add(egui::Slider::new(&mut self.dust_strength, 0.0..=1.0).text("Strength"))
                    .on_hover_text("Higher also repairs fainter defects");
                if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                    dust.strength = self.dust_strength;
                }
                ui.checkbox(&mut dust.show_mask, "Show repaired areas in red");
            });
        });
        if let Some(cleaned) = self.state.current_cleaned() {
            let report = cleaned.removal.report;
            ui.label(format!("{} px repaired", report.defect_pixels))
                .on_hover_text(format!(
                    "Film infrared level {:.2}, cyan dye crosstalk {:.2}",
                    report.film_level, report.crosstalk
                ));
        }
        if self.state.dust != before
            && let Some(cmd) = self.state.update_processing()
        {
            self.worker.send(cmd);
        }
    }

    fn crop_controls(&mut self, ui: &mut egui::Ui) {
        let busy = self.state.is_busy();
        ui.heading("Frames");
        let Some(result) = self.state.result.clone() else {
            ui.label("Scan some film first: its frames are found on the scan.");
            return;
        };
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("Detect frames"))
                .on_hover_text("Finds the frames again, replacing any changes")
                .clicked()
                && let Some(cmd) = self.state.redetect_frames()
            {
                self.worker.send(cmd);
            }
            if ui
                .button("Add frame")
                .on_hover_text("Adds a frame in the middle of the view")
                .clicked()
            {
                let a = result.area;
                let (x, y) = self
                    .view_centre
                    .unwrap_or((a.x_mm + a.width_mm / 2.0, a.y_mm + a.height_mm / 2.0));
                self.state.add_frame(x, y);
            }
            if ui
                .add_enabled(
                    self.state.crop.frames.len() > 1,
                    egui::Button::selectable(
                        self.state.crop.order != state::FrameOrder::default(),
                        self.state.crop.order.label(),
                    ),
                )
                .on_hover_text(
                    "Click to go through the four orders: as scanned, each strip's frames \
                     backwards, the strips backwards, and both (the other end of the film). \
                     For cameras that wind the film the other way, or strips put in the \
                     holder the other way round. Kept for later scans; each frame keeps its \
                     edits.",
                )
                .clicked()
            {
                self.state.cycle_frame_order();
            }
        });

        let crop = &self.state.crop;
        let checks = crop.frames.iter().filter(|f| f.check).count();
        let count = match crop.frames.len() {
            0 => "No frames".to_owned(),
            1 => "1 frame".to_owned(),
            n => format!("{n} frames"),
        };
        ui.label(match (crop.edited, crop.strips) {
            (true, _) => format!("{count}, edited"),
            (false, 0) => format!("{count}: no film strips found"),
            (false, 1) => format!("{count} on 1 strip"),
            (false, s) => format!("{count} on {s} strips"),
        });
        if checks > 0 {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{checks} frame(s) marked ? were placed by the spacing of the others, as \
                     their edges couldn't be seen: check them."
                ),
            );
        }
        ui.label(
            RichText::new(
                "Drag a frame to move it, its handles to resize it, or the round handle to \
                 turn it. Delete removes the selected frame.",
            )
            .weak(),
        );

        ui.add_space(4.0);
        let mut select = None;
        for (i, frame) in self.state.crop.frames.iter().enumerate() {
            let label = format!("Frame {}{}", i + 1, if frame.check { "  ?" } else { "" });
            if ui
                .selectable_label(self.state.crop.selected == Some(i), label)
                .clicked()
            {
                select = Some(i);
            }
        }
        if select.is_some() {
            self.state.crop.selected = select;
        }

        if let Some(i) = self.state.crop.selected
            && let Some(frame) = self.state.crop.frames.get(i)
        {
            ui.add_space(4.0);
            let mut rect = frame.rect;
            egui::Grid::new("frame")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    fn mm(v: &mut f32) -> egui::DragValue<'_> {
                        egui::DragValue::new(v)
                            .speed(0.05)
                            .range(2.0..=300.0)
                            .fixed_decimals(2)
                            .suffix(" mm")
                    }
                    ui.label("Width");
                    ui.add(mm(&mut rect.width_mm));
                    ui.end_row();
                    ui.label("Height");
                    ui.add(mm(&mut rect.height_mm));
                    ui.end_row();
                    ui.label("Angle");
                    ui.add(
                        egui::DragValue::new(&mut rect.angle_deg)
                            .speed(0.02)
                            .range(-180.0..=180.0)
                            .fixed_decimals(2)
                            .suffix("°"),
                    )
                    .on_hover_text("Clockwise, to straighten the frame");
                    ui.end_row();
                });
            self.state.edit_frame(i, rect);
            if ui.button(format!("Delete frame {}", i + 1)).clicked() {
                self.state.delete_frame(i);
            }
        }

        ui.add_space(8.0);
        ui.separator();
        self.save_frames_button(ui, busy);
    }

    fn save_frames_button(&mut self, ui: &mut egui::Ui, busy: bool) {
        let can_save = !busy && !self.state.crop.frames.is_empty();
        let positive = self.state.inverter().is_some();
        if ui
            .add_enabled(can_save, egui::Button::new("Save frames as TIFF..."))
            .on_hover_text(format!(
                "Saves each frame straightened{}, with its own edits from the Adjust step, \
                 turned like the view, as its own 16-bit file: name_01.tif, name_02.tif, ...",
                if positive { " and inverted" } else { "" }
            ))
            .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("TIFF image", &["tif", "tiff"])
                .set_file_name("frame.tif")
                .save_file()
            && let Some(cmd) = self.state.start_save_frames(path)
        {
            self.worker.send(cmd);
        }
    }

    fn invert_controls(&mut self, ui: &mut egui::Ui) {
        let busy = self.state.is_busy();
        ui.heading("Film");
        let Some(result) = self.state.result.clone() else {
            ui.label("Scan some film first.");
            return;
        };
        if result.source != ScanSource::Transparency {
            ui.label("Inversion is for film scans: scan with the film source.");
            return;
        }
        let invert = &mut self.state.invert;
        ui.checkbox(&mut invert.enabled, "Make positives")
            .on_hover_text("Off shows and saves the scan as it is");
        ui.add_enabled_ui(invert.enabled, |ui| {
            egui::Grid::new("film")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Film");
                    egui::ComboBox::from_id_salt("film_type")
                        .width(200.0)
                        .selected_text(invert.film_type.label())
                        .show_ui(ui, |ui| {
                            for t in FilmType::ALL {
                                ui.selectable_value(&mut invert.film_type, t, t.label());
                            }
                        });
                    ui.end_row();
                    if invert.film_type == FilmType::ColorNegative {
                        ui.label("Film stock");
                        egui::ComboBox::from_id_salt("stock")
                            .width(200.0)
                            .selected_text(invert.stock.display_name())
                            .show_ui(ui, |ui| {
                                for &stock in scan_film::stock::all() {
                                    ui.selectable_value(
                                        &mut invert.stock,
                                        stock,
                                        stock.display_name(),
                                    )
                                    .on_hover_text(stock.provenance);
                                }
                            });
                        ui.end_row();
                    }
                    if invert.film_type == FilmType::BlackAndWhite {
                        ui.label("Film stock");
                        egui::ComboBox::from_id_salt("mono_stock")
                            .width(200.0)
                            .selected_text(
                                invert
                                    .mono_stock
                                    .map_or(GENERIC_BW.into(), |s| s.display_name()),
                            )
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut invert.mono_stock, None, GENERIC_BW)
                                    .on_hover_text(GENERIC_BW_NOTE);
                                for &stock in scan_film::stock::mono_all() {
                                    ui.selectable_value(
                                        &mut invert.mono_stock,
                                        Some(stock),
                                        stock.display_name(),
                                    )
                                    .on_hover_text(stock.provenance);
                                }
                            });
                        ui.end_row();
                    }
                });
            match invert.film_type {
                FilmType::ColorNegative => {
                    ui.label(RichText::new(invert.stock.provenance).weak().small());
                }
                FilmType::BlackAndWhite => {
                    let note = invert.mono_stock.map_or(GENERIC_BW_NOTE, |s| s.provenance);
                    ui.label(RichText::new(note).weak().small());
                    mono_controls(ui, &mut invert.mono);
                }
                FilmType::Slide => {
                    ui.label(
                        RichText::new(
                            "No slide film profiles yet: the slide is shown as it is, with the \
                             printing's exposure, balance and contrast.",
                        )
                        .weak()
                        .small(),
                    );
                }
            }
        });
        if !self.state.invert.enabled {
            ui.add_space(8.0);
            ui.separator();
            self.save_frames_button(ui, busy);
            return;
        }

        ui.add_space(4.0);
        let film_type = self.state.invert.film_type;
        let d_min = self.state.invert.stock.model().d_min();
        match self.state.current_film_base() {
            _ if !film_type.is_negative() => {}
            Some(base) => {
                let d = base.light.map(|l| -l.max(1e-6).log10());
                ui.label(format!(
                    "Film base density: {:.2} / {:.2} / {:.2}",
                    d[0], d[1], d[2]
                ))
                .on_hover_text(format!(
                    "Red / green / blue, {}. The negative is inverted against it{}",
                    if base.measured {
                        "measured on the unexposed film between the frames and perforations"
                    } else {
                        "estimated from the clearest film on the scan, as no film strips \
                         were found: check the result"
                    },
                    if film_type == FilmType::ColorNegative {
                        format!(
                            ", which removes the orange mask. The datasheet's D-min is \
                             {:.2} / {:.2} / {:.2} (Status M); a scanner's channels see the \
                             mask a little differently.",
                            d_min[0], d_min[1], d_min[2]
                        )
                    } else {
                        ".".to_owned()
                    }
                ));
                if !base.measured {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Film base estimated: no film strips were found to measure it on.",
                    );
                }
            }
            None if busy => {
                ui.label("Measuring the film base...");
            }
            None => {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "No film base found on this scan: it can't be inverted.",
                );
            }
        }

        ui.add_space(8.0);
        ui.heading("Printing");
        let color = film_type != FilmType::BlackAndWhite;
        let printing = &mut self.state.invert.printing;
        egui::Grid::new("printing")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Exposure");
                ui.add(
                    egui::Slider::new(&mut printing.exposure_ev, -3.0..=3.0)
                        .step_by(0.05)
                        .suffix(" EV"),
                )
                .on_hover_text("Brighter or darker, in stops, as a printer's exposure");
                ui.end_row();
                for (c, (label, hover)) in [
                    ("Cyan - Red", "More red (+) or cyan (-), in stops"),
                    ("Magenta - Green", "More green (+) or magenta (-), in stops"),
                    ("Yellow - Blue", "More blue (+) or yellow (-), in stops"),
                ]
                .into_iter()
                .enumerate()
                {
                    ui.add_enabled(color, egui::Label::new(label));
                    ui.add_enabled(
                        color,
                        egui::Slider::new(&mut printing.balance_ev[c], -1.5..=1.5)
                            .step_by(0.01)
                            .suffix(" EV"),
                    )
                    .on_hover_text(hover);
                    ui.end_row();
                }
                ui.label("Contrast");
                ui.add(egui::Slider::new(&mut printing.contrast, CONTRAST_RANGE).step_by(0.05))
                    .on_hover_text("The print curve's contrast; grey stays grey");
                ui.end_row();
                if film_type != FilmType::ColorNegative {
                    return;
                }
                ui.label("Color");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut printing.rendering, Rendering::Print, "Print")
                        .on_hover_text(
                            "Each of the film's layers makes its own color, as when the \
                             negative is printed on color paper: the film's own look",
                        );
                    ui.radio_value(
                        &mut printing.rendering,
                        Rendering::Colorimetric,
                        "Colorimetric",
                    )
                    .on_hover_text(
                        "Mixes the layers towards the scene's colors, from the film's \
                         spectral sensitivities. Can look more saturated than a print.",
                    );
                });
                ui.end_row();
            });
        ui.horizontal(|ui| {
            let has_frames = !self.state.crop.frames.is_empty();
            if ui
                .add_enabled(!busy && has_frames, egui::Button::new("Auto"))
                .on_hover_text(
                    "Sets exposure and color balance from the frames, as a photofinisher's \
                     printer does: each frame's average taken as grey, and the roll's \
                     typical frame used, so one frame of lawn or sea doesn't tint the rest",
                )
                .on_disabled_hover_text("Needs frames: detect or add them in the Crop step")
                .clicked()
            {
                self.state.auto_printing();
            }
            if ui
                .button("Reset")
                .on_hover_text("No exposure or color change: the film as its datasheet prints it")
                .clicked()
            {
                self.state.invert.printing = Printing {
                    rendering: self.state.invert.printing.rendering,
                    ..Printing::default()
                };
            }
        });

        ui.add_space(8.0);
        ui.separator();
        self.save_frames_button(ui, busy);
    }

    /// The frames along the bottom, in the Adjust and Export steps.
    fn filmstrip(&mut self, ui: &mut egui::Ui) {
        self.filmstrip.show(ui, &mut self.state, &mut self.editor);
    }

    fn adjust_controls(&mut self, ui: &mut egui::Ui) {
        let busy = self.state.is_busy();
        self.editor.controls(ui, &mut self.state);
        if self.state.result.is_none() {
            return;
        }
        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            self.save_frames_button(ui, busy);
            self.save_scan_button(ui, busy);
        });
    }

    fn export_controls(&mut self, ui: &mut egui::Ui) {
        let busy = self.state.is_busy();
        self.state.fill_export_film();
        ui.heading("Export");
        let frames = self.state.exported_frames().count();
        let skipped = self.state.crop.frames.len() - frames;
        let settings = &mut self.state.export;
        egui::Grid::new("export")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Folder");
                ui.horizontal(|ui| {
                    if ui.button("Choose...").clicked() {
                        let mut dialog = rfd::FileDialog::new();
                        if let Some(folder) = &settings.folder {
                            dialog = dialog.set_directory(folder);
                        }
                        if let Some(folder) = dialog.pick_folder() {
                            settings.folder = Some(folder);
                        }
                    }
                    let shown = settings
                        .folder
                        .as_ref()
                        .map_or("None chosen".into(), |f| f.display().to_string());
                    ui.add(egui::Label::new(RichText::new(shown).weak()).truncate());
                });
                ui.end_row();

                ui.label("File names");
                ui.vertical(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut settings.prefix)
                            .hint_text("roll180")
                            .desired_width(160.0),
                    )
                    .on_hover_text(
                        "The start of every frame's file name: frames are saved as \
                         <this>_01, <this>_02, ... in frame order",
                    );
                    let count = frames.max(1);
                    let folder = std::path::Path::new("");
                    let name = |n| settings.frame_path(folder, n, count).display().to_string();
                    let shown = if count > 1 {
                        format!("{}, {} ... {}", name(0), name(1), name(count - 1))
                    } else {
                        name(0)
                    };
                    ui.label(RichText::new(shown).weak().small());
                });
                ui.end_row();

                ui.label("Format");
                ui.horizontal(|ui| {
                    let quality = match settings.format {
                        FrameFormat::Jpeg(q) => q,
                        _ => 95,
                    };
                    egui::ComboBox::from_id_salt("export_format")
                        .selected_text(settings.format.label())
                        .show_ui(ui, |ui| {
                            for format in [
                                FrameFormat::Jpeg(quality),
                                FrameFormat::Png,
                                FrameFormat::Tiff,
                            ] {
                                ui.selectable_value(&mut settings.format, format, format.label());
                            }
                        });
                    if let FrameFormat::Jpeg(q) = &mut settings.format {
                        ui.add(egui::Slider::new(q, 50..=100).text("quality"));
                    }
                });
                ui.end_row();
            });
        if !settings.format.publishable() {
            ui.label(
                RichText::new("film_roll_publisher uploads JPEG and PNG files only.")
                    .weak()
                    .small(),
            );
        }

        ui.add_space(8.0);
        ui.separator();
        ui.checkbox(
            &mut settings.write_metadata,
            format!("Write {} for film_roll_publisher", export::METADATA_FILE),
        )
        .on_hover_text(
            "Saves the roll's details next to the frames, for rollpublisher to upload \
             them to Immich with: run it in the export folder",
        );
        if settings.write_metadata {
            let profile = self.state.invert.profile_film();
            metadata_fields(ui, &mut self.state.export, profile);
        }

        ui.add_space(8.0);
        ui.separator();
        let problems = self.state.export_problems();
        for problem in &problems {
            ui.label(RichText::new(problem).color(ui.visuals().warn_fg_color));
        }
        if skipped > 0 {
            ui.label(
                RichText::new(format!(
                    "{skipped} frame{} skipped: tick {} in the strip below to export {}.",
                    if skipped == 1 { "" } else { "s" },
                    if skipped == 1 { "it" } else { "them" },
                    if skipped == 1 { "it" } else { "them" }
                ))
                .weak(),
            );
        }
        let paths = self.state.export_paths();
        let existing: Vec<_> = paths.iter().filter(|p| p.exists()).collect();
        if let Some(first) = existing.first() {
            ui.label(
                RichText::new(format!(
                    "{} file(s) in the folder will be replaced, {} first",
                    existing.len(),
                    first.file_name().unwrap_or_default().to_string_lossy()
                ))
                .color(ui.visuals().warn_fg_color),
            );
        }
        if ui
            .add_enabled(
                !busy && problems.is_empty(),
                egui::Button::new(format!("Export {frames} frames")),
            )
            .on_hover_text(
                "Saves each frame straightened, inverted and with its own edits from the \
                 Adjust step, turned like the view, tagged sRGB",
            )
            .clicked()
            && let Some(cmd) = self.state.start_export()
        {
            self.worker.send(cmd);
        }
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let Some(result) = self.state.result.clone() else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Run a preview scan to see the scanner bed here.").weak());
            });
            return;
        };
        if self.state.step == WorkflowStep::Adjust {
            self.editor.central(ui, &mut self.state);
            return;
        }
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.state.view_channel,
                ViewChannel::Visible,
                "Visible",
            );
            ui.add_enabled_ui(result.infrared.is_some(), |ui| {
                ui.selectable_value(
                    &mut self.state.view_channel,
                    ViewChannel::Infrared,
                    "Infrared",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Fit").clicked() {
                    self.view.reset();
                }
                if ui
                    .button("⟳")
                    .on_hover_text("Turn the scan 90° clockwise, for viewing and saving")
                    .clicked()
                {
                    self.state.turn(true);
                }
                if ui
                    .button("⟲")
                    .on_hover_text("Turn the scan 90° anticlockwise, for viewing and saving")
                    .clicked()
                {
                    self.state.turn(false);
                }
                ui.label(RichText::new("Scroll to zoom, drag to pan, double-click to fit").weak());
            });
        });
        let channel = self.state.view_channel;
        let cleaned = self.state.current_cleaned().map(|c| c.removal.clone());
        let show_mask = self.state.dust.show_mask;
        let image_id = self.state.processed_id();
        let key = (self.state.result_generation, image_id);
        let visible = match &self.thumbnail {
            Some((k, image)) if *k == key => image.clone(),
            _ => {
                let image = Arc::new(view::thumbnail(
                    self.state.processed_image().unwrap_or(result.primary()),
                ));
                self.thumbnail = Some((key, image.clone()));
                image
            }
        };
        let inverter = self.state.inverter();
        // Which view of the result: the channel, the image, its mask, and its inversion.
        let variant = match channel {
            ViewChannel::Infrared => 1,
            ViewChannel::Visible => {
                let mut h = std::hash::DefaultHasher::new();
                (image_id, show_mask && cleaned.is_some()).hash(&mut h);
                if inverter.is_some() {
                    let invert = &self.state.invert;
                    let p = &invert.printing;
                    invert.stock.id.hash(&mut h);
                    invert.mono_stock.map(|s| s.id).hash(&mut h);
                    invert.film_type.key().hash(&mut h);
                    let m = &invert.mono;
                    (m.toning.label(), m.toning_amount.to_bits()).hash(&mut h);
                    for v in m.mix {
                        v.to_bits().hash(&mut h);
                    }
                    let base = self.state.current_film_base().map(|b| b.light);
                    for v in base.into_iter().flatten().chain(p.balance_ev) {
                        v.to_bits().hash(&mut h);
                    }
                    (
                        p.exposure_ev.to_bits(),
                        p.contrast.to_bits(),
                        p.rendering as u8,
                    )
                        .hash(&mut h);
                }
                h.finish() | 2
            }
        };
        let cropping = self.state.step == WorkflowStep::Crop;
        let state = &mut self.state;
        let editor = &mut self.crop_editor;
        let view_centre = &mut self.view_centre;
        self.view.show(
            ui,
            state.result_generation,
            variant,
            state.quarter_turns,
            || {
                view::render(
                    &result,
                    channel,
                    &visible,
                    cleaned.as_deref(),
                    show_mask,
                    inverter.as_ref(),
                )
            },
            |ui, response, placement| {
                let bed = BedView {
                    area: result.area,
                    placement: *placement,
                };
                let a = result.area;
                let (x, y) = bed.to_bed(response.rect.center());
                *view_centre = Some((
                    x.clamp(a.x_mm, a.x_mm + a.width_mm),
                    y.clamp(a.y_mm, a.y_mm + a.height_mm),
                ));
                if !cropping {
                    return false;
                }
                let crop = &state.crop;
                let out = editor.show(ui, response, &bed, &crop.frames, crop.selected);
                if let Some(selected) = out.select {
                    state.crop.selected = selected;
                }
                if let Some((i, rect)) = out.edit {
                    state.edit_frame(i, rect);
                }
                if out.delete
                    && let Some(i) = state.crop.selected
                {
                    state.delete_frame(i);
                }
                out.took_pointer
            },
        );
    }
}

impl eframe::App for ScannerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.process_events();
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);

        egui::Panel::top("workflow").show(ui, |ui| {
            ui.add_space(4.0);
            self.workflow_bar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(2.0);
            self.status_bar(ui);
            ui.add_space(2.0);
        });
        if matches!(self.state.step, WorkflowStep::Adjust | WorkflowStep::Export)
            && self.state.result.is_some()
        {
            egui::Panel::bottom("filmstrip").show(ui, |ui| {
                ui.add_space(4.0);
                self.filmstrip(ui);
                ui.add_space(2.0);
            });
        }
        egui::Panel::left("controls")
            .default_size(340.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(4.0);
                    match self.state.step {
                        WorkflowStep::Scan => self.scan_controls(ui),
                        WorkflowStep::Crop => self.crop_controls(ui),
                        WorkflowStep::Invert => self.invert_controls(ui),
                        WorkflowStep::Adjust => self.adjust_controls(ui),
                        WorkflowStep::Export => self.export_controls(ui),
                    }
                });
            });
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));
        // A slider dragged, or a curve point, is one change once let go.
        let settled = !ctx.input(|i| i.pointer.any_down());
        self.state.track_history(settled);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(TURNS_KEY, self.state.quarter_turns.to_string());
        storage.set_string(STOCK_KEY, self.state.invert.stock.id.to_owned());
        storage.set_string(FILM_TYPE_KEY, self.state.invert.film_type.key().to_owned());
        let mono_stock = self.state.invert.mono_stock.map_or("", |s| s.id);
        storage.set_string(MONO_STOCK_KEY, mono_stock.to_owned());
        eframe::set_value(storage, PRESETS_KEY, &self.state.presets);
        remember_export(&mut self.state.export, storage);
    }
}

/// The black-and-white stock choice without a profile, and what it does.
const GENERIC_BW: &str = "Generic black-and-white";
const GENERIC_BW_NOTE: &str = "No film profile: inverted on a generic black-and-white film's \
                               curve. Exposure and contrast set the print.";

/// How black-and-white film is mixed to grey and toned.
fn mono_controls(ui: &mut egui::Ui, mono: &mut Mono) {
    egui::Grid::new("mono")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Grey from");
            ui.horizontal(|ui| {
                for (label, mix, hover) in [
                    (
                        "All",
                        [1.0, 1.0, 1.0],
                        "Every channel equally: the least noise",
                    ),
                    ("Red", [1.0, 0.0, 0.0], "The red channel alone"),
                    ("Green", [0.0, 1.0, 0.0], "The green channel alone"),
                    ("Blue", [0.0, 0.0, 1.0], "The blue channel alone"),
                ] {
                    let total: f32 = mix.iter().sum();
                    let mix = mix.map(|m| m / total);
                    let on = mono.mix.iter().zip(mix).all(|(a, b)| (a - b).abs() < 1e-3);
                    if ui
                        .add(egui::Button::selectable(on, label))
                        .on_hover_text(hover)
                        .clicked()
                    {
                        mono.mix = mix;
                    }
                }
            })
            .response
            .on_hover_text(
                "Which of the scan's channels make the grey: silver film is near neutral, so \
                 this mostly picks the cleanest; chromogenic (C-41) black-and-white film differs \
                 more",
            );
            ui.end_row();
            ui.label("Toning");
            egui::ComboBox::from_id_salt("toning")
                .selected_text(mono.toning.label())
                .show_ui(ui, |ui| {
                    for t in Toning::ALL {
                        ui.selectable_value(&mut mono.toning, t, t.label());
                    }
                });
            ui.end_row();
            ui.label("Strength");
            ui.add_enabled(
                mono.toning != Toning::None,
                egui::Slider::new(&mut mono.toning_amount, 0.0..=1.0)
                    .step_by(0.01)
                    .fixed_decimals(2),
            );
            ui.end_row();
        });
}

/// The roll, film and camera fields of the publisher's metadata. Required ones are marked.
fn metadata_fields(
    ui: &mut egui::Ui,
    settings: &mut ExportSettings,
    profile: Option<state::ProfileFilm>,
) {
    let text = |ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str| {
        ui.label(label);
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .desired_width(190.0),
        );
        ui.end_row();
    };
    let date = |ui: &mut egui::Ui, label: &str, value: &mut String| {
        ui.label(label);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(value)
                    .hint_text("MM/DD/YYYY")
                    .desired_width(110.0),
            );
            if ui.small_button("Today").clicked() {
                *value = export::today();
            }
        });
        ui.end_row();
    };
    let m = &mut settings.metadata;
    egui::CollapsingHeader::new("Roll")
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("meta_roll").num_columns(2).show(ui, |ui| {
                text(ui, "Name *", &mut m.name, "Album name in Immich");
                ui.label("Description");
                ui.add(
                    egui::TextEdit::multiline(&mut m.description)
                        .desired_rows(2)
                        .desired_width(190.0),
                );
                ui.end_row();
                date(ui, "Date captured *", &mut m.date_captured);
                date(ui, "Date scanned", &mut m.date_scanned);
                ui.label("Public");
                ui.checkbox(&mut m.public, "");
                ui.end_row();
                text(ui, "GPS latitude", &mut m.gps_latitude, "41.8781");
                text(ui, "GPS longitude", &mut m.gps_longitude, "-87.6298");
            });
        });
    egui::CollapsingHeader::new("Film")
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("meta_film").num_columns(2).show(ui, |ui| {
                text(ui, "Manufacturer *", &mut m.manufacturer, "Kodak");
                text(ui, "Stock *", &mut m.stock, "Gold");
                text(ui, "ISO *", &mut m.iso, "200");
                text(ui, "Development", &mut m.development_notes, "Lab, C-41");
            });
            if let Some(p) = profile
                && ui
                    .small_button("From the film profile")
                    .on_hover_text(format!("Fill in {}", p.display_name()))
                    .clicked()
            {
                m.fill_film(p.manufacturer, p.name, p.iso);
            }
        });
    egui::CollapsingHeader::new("Camera")
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("meta_camera")
                .num_columns(2)
                .show(ui, |ui| {
                    text(ui, "Make *", &mut m.camera_make, "Nikon");
                    text(ui, "Model *", &mut m.camera_model, "FM2");
                    text(ui, "Lens make", &mut m.lens_make, "Nikon");
                    text(ui, "Lens model", &mut m.lens_model, "50mm f/1.8");
                    text(ui, "F-number", &mut m.f_number, "f/8");
                    text(ui, "Focal length", &mut m.focal_length, "50mm");
                    text(ui, "Flash", &mut m.flash, "No");
                    text(ui, "Filters", &mut m.filters, "UV");
                });
            ui.label(RichText::new("Kept for the next roll.").weak().small());
        });
}

fn restore_export(export: &mut ExportSettings, storage: &dyn eframe::Storage) {
    if let Some(folder) = storage
        .get_string(EXPORT_FOLDER_KEY)
        .filter(|f| !f.is_empty())
    {
        export.folder = Some(folder.into());
    }
    if let Some(prefix) = storage.get_string(EXPORT_PREFIX_KEY) {
        export.prefix = prefix;
    }
    match storage.get_string(EXPORT_FORMAT_KEY).as_deref() {
        Some("png") => export.format = FrameFormat::Png,
        Some("tiff") => export.format = FrameFormat::Tiff,
        Some(jpeg) => {
            if let Some(q) = jpeg
                .strip_prefix("jpeg ")
                .and_then(|q| q.parse::<u8>().ok())
            {
                export.format = FrameFormat::Jpeg(q.clamp(1, 100));
            }
        }
        None => {}
    }
    if let Some(on) = storage.get_string(EXPORT_METADATA_KEY) {
        export.write_metadata = on == "true";
    }
    for (key, value) in export.metadata.camera_fields() {
        if let Some(saved) = storage.get_string(&format!("export.{key}")) {
            *value = saved;
        }
    }
}

fn remember_export(export: &mut ExportSettings, storage: &mut dyn eframe::Storage) {
    let folder = export
        .folder
        .as_ref()
        .map_or(String::new(), |f| f.display().to_string());
    storage.set_string(EXPORT_FOLDER_KEY, folder);
    storage.set_string(EXPORT_PREFIX_KEY, export.prefix.clone());
    let format = match export.format {
        FrameFormat::Jpeg(q) => format!("jpeg {q}"),
        FrameFormat::Png => "png".into(),
        FrameFormat::Tiff => "tiff".into(),
    };
    storage.set_string(EXPORT_FORMAT_KEY, format);
    storage.set_string(EXPORT_METADATA_KEY, export.write_metadata.to_string());
    for (key, value) in export.metadata.camera_fields() {
        storage.set_string(&format!("export.{key}"), value.clone());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn registry_has_native_backend_first() {
        let names = super::default_registry().backend_names();
        assert_eq!(names.first(), Some(&"epson"));
        assert_eq!(names.last(), Some(&"mock"));
    }
}
