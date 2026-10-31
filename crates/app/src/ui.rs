//! egui front end: workflow stepper, scan controls, image view and status bar.

use eframe::egui::{self, Align, Layout, RichText};
use scan_core::mock::MockBackend;
use scan_core::{BitDepth, ColorMode, DeviceRegistry};

use crate::state::{Activity, AppState, ViewChannel, WorkflowStep};
use crate::view::ImageView;
use crate::worker::{ScanKind, Worker};

/// All backends compiled into this build, real hardware first.
pub fn default_registry() -> DeviceRegistry {
    let mut registry = DeviceRegistry::new();
    scan_wia::register(&mut registry);
    registry.register(MockBackend);
    registry
}

pub struct ScannerApp {
    state: AppState,
    worker: Worker,
    view: ImageView,
}

impl ScannerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let worker = Worker::spawn(default_registry(), move || ctx.request_repaint());
        let mut state = AppState::default();
        worker.send(state.refresh_devices());
        Self {
            state,
            worker,
            view: ImageView::default(),
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
                ui.label(RichText::new("Scroll to zoom, drag to pan, double-click to fit").weak());
            });
        });
        self.view.show(
            ui,
            &result,
            self.state.result_generation,
            self.state.view_channel,
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
                        step => self.placeholder(ui, step),
                    }
                });
            });
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));
    }
}
