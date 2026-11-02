//! egui front end: workflow stepper, scan controls, image view and status bar.

use std::sync::Arc;

use eframe::egui::{self, Align, Layout, RichText};
use scan_core::mock::MockBackend;
use scan_core::{BitDepth, ColorMode, DeviceRegistry};

use crate::crop::{BedView, CropEditor};
use crate::state::{self, Activity, AppState, EXTRA_EV, ViewChannel, WorkflowStep};
use crate::view::{self, ImageView};
use crate::worker::{ScanKind, Worker};

/// Where the scan's turn is remembered between sessions.
const TURNS_KEY: &str = "quarter_turns";

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
    /// The middle of the image view on the bed, in millimetres, as last shown.
    view_centre: Option<(f32, f32)>,
    /// The dust strength slider, applied when it is let go.
    dust_strength: f32,
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
        worker.send(state.refresh_devices());
        Self {
            dust_strength: state.dust.strength,
            state,
            worker,
            view: ImageView::default(),
            crop_editor: CropEditor::default(),
            view_centre: None,
        }
    }

    fn process_events(&mut self) {
        while let Some(event) = self.worker.try_recv() {
            if let Some(command) = self.state.apply(event) {
                self.worker.send(command);
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
        });
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

                    ui.label("Colour");
                    let mode_label = |m: ColorMode| match m {
                        ColorMode::Color => "Colour",
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
        if ui
            .add_enabled(!busy, egui::Button::new("Save as TIFF..."))
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
        let can_save = !busy && !self.state.crop.frames.is_empty();
        if ui
            .add_enabled(can_save, egui::Button::new("Save frames as TIFF..."))
            .on_hover_text(
                "Saves each frame straightened, turned like the view, as its own file: \
                 name_01.tif, name_02.tif, ...",
            )
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

    fn placeholder(&self, ui: &mut egui::Ui, step: WorkflowStep) {
        ui.heading(step.label());
        ui.label(step.description());
        ui.add_space(8.0);
        ui.label(RichText::new("Coming soon.").italics().weak());
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let Some(result) = self.state.result.clone() else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Run a preview scan to see the scanner bed here.").weak());
            });
            return;
        };
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
        let merged = self.state.current_merged().map(|m| m.merge.clone());
        let cleaned = self.state.current_cleaned().map(|c| c.removal.clone());
        let show_mask = self.state.dust.show_mask;
        // Which view of the result: the channel, and which dust removal (with or without its
        // mask) or merge, told apart by its address while it is alive.
        let variant = match (channel, &cleaned, &merged) {
            (ViewChannel::Infrared, ..) => 1,
            (ViewChannel::Visible, Some(c), _) => {
                ((Arc::as_ptr(c) as usize as u64) << 2) | (u64::from(show_mask) << 1)
            }
            (ViewChannel::Visible, None, Some(m)) => (Arc::as_ptr(m) as usize as u64) << 2,
            (ViewChannel::Visible, None, None) => 0,
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
                    merged.as_deref(),
                    cleaned.as_deref(),
                    show_mask,
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
        egui::Panel::left("controls")
            .default_size(340.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(4.0);
                    match self.state.step {
                        WorkflowStep::Scan => self.scan_controls(ui),
                        WorkflowStep::Crop => self.crop_controls(ui),
                        step => self.placeholder(ui, step),
                    }
                });
            });
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(TURNS_KEY, self.state.quarter_turns.to_string());
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
