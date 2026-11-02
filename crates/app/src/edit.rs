//! The Adjust step: the selected frame (or the whole scan) shown developed with its own
//! edits, the controls that edit it, and the pickers that work on the view.
//!
//! The frame is cut out of the scan at preview size once ([`scan_process::preview_frame`]),
//! inverted with its color ("base", cached until its exposure, balance or the roll's
//! inversion change), then edited ("output"), so moving a slider redoes only what it
//! affects.

use std::sync::Arc;

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};
use scan_core::image::DynamicImage;
use scan_process::adjust::{
    self, Adjustments, CHANNELS, Curve, HUE_RANGES, Histogram, Levels, MASTER,
};
use scan_process::{FrameRect, Rgb16Image};

use crate::state::{AppState, Film};
use crate::view::ImageView;

/// Longest side of the preview a frame is edited on.
const PREVIEW_SIDE: u32 = 2048;
/// How the channels are drawn in histograms and curves: brightness, red, green, blue.
const CHANNEL_COLORS: [Color32; 4] = [
    Color32::from_gray(200),
    Color32::from_rgb(235, 80, 70),
    Color32::from_rgb(90, 200, 90),
    Color32::from_rgb(90, 140, 245),
];

/// What a click on the view does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    None,
    /// Balances the frame so the point clicked is neutral.
    Grey,
    /// Sets the levels' black point, per channel, to the point clicked.
    Black,
    /// Sets the levels' white point, per channel, to the point clicked.
    White,
}

#[derive(Clone, Debug, PartialEq)]
struct SourceKey {
    generation: u64,
    image: u64,
    rect: Option<FrameRect>,
}

/// The frame cut out of the scan at preview size, 16-bit, not inverted.
struct Source {
    key: SourceKey,
    image: DynamicImage,
    /// Its pixels per pixel of the saved frame.
    scale: f32,
}

#[derive(Clone, Debug, PartialEq)]
struct BaseKey {
    source: SourceKey,
    film: Option<Film>,
    gamma: f32,
    exposure_ev: f32,
    balance_ev: [f32; 3],
}

struct Developed<K> {
    key: K,
    image: Arc<Rgb16Image>,
    histogram: Histogram,
}

#[derive(Default)]
pub struct Editor {
    view: ImageView,
    source: Option<Source>,
    /// The frame inverted with its color, before its other edits: what levels and the
    /// black and white pickers work on.
    base: Option<Developed<BaseKey>>,
    /// The frame with every edit.
    output: Option<Developed<(BaseKey, Adjustments)>>,
    /// The frame with no edits at all, for comparing; made when asked for.
    before: Option<Developed<BaseKey>>,
    /// Counts changes of what is shown: the frame (the view's generation), and its pixels.
    shown: u64,
    version: u64,
    pub tool: Tool,
    /// Show the frame without its edits.
    pub compare: bool,
    /// The channel levels and curves show.
    channel: usize,
    /// The hue range selective color shows.
    hue: usize,
    /// The curve point being dragged.
    curve_drag: Option<usize>,
}

/// The frame the Adjust step edits: the selected one, or the whole scan.
fn target(state: &AppState) -> Option<usize> {
    state.crop.selected.filter(|&i| i < state.crop.frames.len())
}

fn target_name(target: Option<usize>) -> String {
    match target {
        Some(i) => format!("Frame {}", i + 1),
        None => "Whole scan".to_owned(),
    }
}

/// `source` developed with `adj`'s exposure and balance alone.
fn develop_base(source: &Source, key: BaseKey) -> Developed<BaseKey> {
    let color = Adjustments {
        exposure_ev: key.exposure_ev,
        balance_ev: key.balance_ev,
        ..Adjustments::default()
    };
    let inverter = key.film.map(|f| f.inverter(&color));
    let image = adjust::develop(
        &source.image,
        key.gamma,
        inverter.as_ref(),
        &color,
        source.scale,
    );
    Developed {
        histogram: adjust::histogram(&image),
        image: Arc::new(image),
        key,
    }
}

impl Editor {
    /// Bring the preview up to date with the state; false if there is nothing to show.
    fn prepare(&mut self, state: &AppState) -> bool {
        let (Some(result), Some(image)) = (state.result.as_ref(), state.processed_image()) else {
            return false;
        };
        let target = target(state);
        let key = SourceKey {
            generation: state.result_generation,
            image: state.processed_id(),
            rect: target.map(|i| state.crop.frames[i].rect),
        };
        if self.source.as_ref().is_none_or(|s| s.key != key) {
            let (preview, scale) = scan_process::preview_frame(
                image,
                result.dpi,
                result.area,
                key.rect.as_ref(),
                PREVIEW_SIDE,
            );
            let preview = match preview {
                DynamicImage::ImageRgb16(_) => preview,
                other => DynamicImage::ImageRgb16(other.to_rgb16()),
            };
            self.source = Some(Source {
                key: key.clone(),
                image: preview,
                scale,
            });
            self.shown += 1;
        }
        let Some(source) = &self.source else {
            return false;
        };
        let adj = state.adjustments(target);
        let film = state.film();
        let base_key = BaseKey {
            source: key.clone(),
            film,
            gamma: result.gamma,
            exposure_ev: adj.exposure_ev,
            balance_ev: adj.balance_ev,
        };
        if self.base.as_ref().is_none_or(|b| b.key != base_key) {
            self.base = Some(develop_base(source, base_key.clone()));
        }
        // Orientation is the view's, not the pixels'.
        let edits = Adjustments {
            quarter_turns: 0,
            mirror: false,
            ..adj.clone()
        };
        let out_key = (base_key, edits);
        if self.output.as_ref().is_none_or(|o| o.key != out_key)
            && let Some(base) = &self.base
        {
            let image = if out_key.1.edits_positive() {
                Arc::new(adjust::apply(
                    (*base.image).clone(),
                    &out_key.1,
                    source.scale,
                ))
            } else {
                base.image.clone()
            };
            self.output = Some(Developed {
                histogram: adjust::histogram(&image),
                image,
                key: out_key,
            });
            self.version += 1;
        }
        if self.compare {
            let before_key = BaseKey {
                exposure_ev: 0.0,
                balance_ev: [0.0; 3],
                ..self.base.as_ref().expect("made above").key.clone()
            };
            if self.before.as_ref().is_none_or(|b| b.key != before_key) {
                self.before = Some(develop_base(source, before_key));
            }
        }
        true
    }

    /// The view of the frame being edited, with the pickers.
    pub fn central(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        if !self.prepare(state) {
            return;
        }
        let target = target(state);
        ui.horizontal(|ui| {
            ui.label(RichText::new(target_name(target)).strong());
            ui.checkbox(&mut self.compare, "Before")
                .on_hover_text("Shows it without any of its Adjust edits, to compare");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Fit").clicked() {
                    self.view.reset();
                }
                let hint = match self.tool {
                    Tool::None => "Scroll to zoom, drag to pan, double-click to fit",
                    Tool::Grey => "Click something that should be grey",
                    Tool::Black => "Click what should be black",
                    Tool::White => "Click what should be white",
                };
                ui.label(RichText::new(hint).weak());
            });
        });
        // `prepare` again: "Before" may have just been turned on.
        if !self.prepare(state) {
            return;
        }
        let adj = state.adjustments(target);
        let turns = (state.quarter_turns + adj.quarter_turns) % 4;
        let mirror = adj.mirror;
        let shown = match (self.compare, &self.before, &self.output) {
            (true, Some(b), _) => b.image.clone(),
            (_, _, Some(o)) => o.image.clone(),
            _ => return,
        };
        let variant = self.version * 4 + u64::from(self.compare) * 2 + u64::from(mirror);
        let tool = self.tool;
        let mut picked = None;
        self.view.show(
            ui,
            self.shown,
            variant,
            turns,
            || color_image(&shown, mirror),
            |ui, response, placement| {
                if tool == Tool::None {
                    return false;
                }
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if response.clicked()
                    && let Some(p) = response.interact_pointer_pos()
                {
                    let at = placement.to_image(p);
                    if (0.0..=1.0).contains(&at.x) && (0.0..=1.0).contains(&at.y) {
                        picked = Some(at);
                    }
                }
                false
            },
        );
        if let Some(at) = picked {
            self.pick(state, target, at, mirror);
        }
    }

    /// Use the picker at `at` (0–1 across and down the shown, possibly mirrored, frame).
    fn pick(&mut self, state: &mut AppState, target: Option<usize>, at: Pos2, mirror: bool) {
        let u = if mirror { 1.0 - at.x } else { at.x };
        match self.tool {
            Tool::None => {}
            Tool::Grey => {
                let (Some(source), Some(result)) = (&self.source, &state.result) else {
                    return;
                };
                let DynamicImage::ImageRgb16(image) = &source.image else {
                    return;
                };
                let gamma = result.gamma;
                let value = mean_at(image, u, at.y);
                state.pick_neutral(target, value.map(|v| v.powf(gamma)));
            }
            Tool::Black | Tool::White => {
                let Some(base) = &self.base else {
                    return;
                };
                let value = mean_at(&base.image, u, at.y);
                state.pick_level(target, value, self.tool == Tool::White);
            }
        }
        self.tool = Tool::None;
    }

    /// The left panel's controls.
    pub fn controls(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        ui.heading("Frames");
        if state.result.is_none() {
            ui.label("Scan some film first.");
            return;
        }
        let before = target(state);
        let mut selected = before;
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut selected, None, "Whole scan")
                .on_hover_text("The whole scan, for Save as TIFF");
            for i in 0..state.crop.frames.len() {
                let edited = !state.crop.frames[i].adjust.is_identity();
                let label = format!("{}{}", i + 1, if edited { "*" } else { "" });
                ui.selectable_value(&mut selected, Some(i), label)
                    .on_hover_text(format!(
                        "Frame {}{}",
                        i + 1,
                        if edited { ", edited" } else { "" }
                    ));
            }
        });
        if selected != before {
            state.crop.selected = selected;
            self.tool = Tool::None;
            self.curve_drag = None;
        }
        if state.crop.frames.is_empty() {
            ui.label(
                RichText::new("No frames: detect or add them in the Crop step to edit each one.")
                    .weak(),
            );
        }
        let target = target(state);
        self.prepare(state);

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Auto")
                .on_hover_text(
                    "Color and levels for this frame alone: a negative's frame is balanced as \
                     a photofinisher's printer would on its own, then stretched to full range",
                )
                .clicked()
            {
                state.auto_adjust(target);
            }
            if ui
                .button("Reset")
                .on_hover_text("Undoes this frame's edits, but not its turn")
                .clicked()
            {
                state.reset_adjustments(target);
            }
            if ui.button("Copy").clicked() {
                state.copy_adjustments(target);
            }
            let can_paste = state.clipboard.is_some();
            if ui
                .add_enabled(can_paste, egui::Button::new("Paste"))
                .on_hover_text("Everything but the turn")
                .clicked()
            {
                state.paste_adjustments(target);
            }
            if ui
                .add_enabled(
                    can_paste && !state.crop.frames.is_empty(),
                    egui::Button::new("Paste to all"),
                )
                .on_hover_text("Onto every frame, everything but their turns")
                .clicked()
            {
                state.paste_to_all_frames();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Pick");
            for (tool, label, hover) in [
                (
                    Tool::Grey,
                    "Grey",
                    "Click a grey in the frame to balance it",
                ),
                (
                    Tool::Black,
                    "Black",
                    "Click the frame's black to set each channel's black point",
                ),
                (
                    Tool::White,
                    "White",
                    "Click the frame's white to set each channel's white point",
                ),
            ] {
                let on = self.tool == tool;
                if ui
                    .selectable_label(on, label)
                    .on_hover_text(hover)
                    .clicked()
                {
                    self.tool = if on { Tool::None } else { tool };
                }
            }
        });
        if target.is_some() {
            let adj = state.adjustments_mut(target);
            ui.horizontal(|ui| {
                ui.label("Turn");
                if ui
                    .button("⟲")
                    .on_hover_text("This frame a quarter anticlockwise")
                    .clicked()
                {
                    adj.quarter_turns = (adj.quarter_turns + 3) % 4;
                }
                if ui
                    .button("⟳")
                    .on_hover_text("This frame a quarter clockwise")
                    .clicked()
                {
                    adj.quarter_turns = (adj.quarter_turns + 1) % 4;
                }
                ui.checkbox(&mut adj.mirror, "Mirror")
                    .on_hover_text("Flips it left to right: film scanned the wrong way up");
            });
        }
        if let Some(output) = &self.output {
            ui.add_space(4.0);
            histogram_plot(ui, &output.histogram, MASTER, None);
        }

        let film = state.film().is_some();
        let base_histogram = self.base.as_ref().map(|b| b.histogram.clone());
        let adj = state.adjustments_mut(target);
        ui.add_space(4.0);
        egui::CollapsingHeader::new("Color")
            .default_open(true)
            .show(ui, |ui| color_controls(ui, adj, film));
        egui::CollapsingHeader::new("Levels")
            .default_open(true)
            .show(ui, |ui| {
                channel_picker(ui, &mut self.channel, "levels_channel");
                if let Some(h) = &base_histogram {
                    histogram_plot(ui, h, self.channel, Some(&adj.levels[self.channel]));
                }
                levels_controls(ui, &mut adj.levels[self.channel]);
            });
        egui::CollapsingHeader::new("Curves").show(ui, |ui| {
            channel_picker(ui, &mut self.channel, "curves_channel");
            let bins = base_histogram.as_ref().map(|h| &h.bins[self.channel]);
            let ch = self.channel;
            curve_editor(
                ui,
                &mut adj.curves[ch],
                CHANNEL_COLORS[ch],
                bins,
                &mut self.curve_drag,
            );
            ui.label(
                RichText::new("Click to add a point, drag to move it, double-click to remove it.")
                    .weak()
                    .small(),
            );
            if ui.button("Reset curve").clicked() {
                adj.curves[ch] = Curve::default();
            }
        });
        egui::CollapsingHeader::new("Brightness, contrast, saturation").show(ui, |ui| {
            grid(ui, "tone", |ui| {
                signed(
                    ui,
                    "Brightness",
                    &mut adj.brightness,
                    "Midtones lighter or darker; black and white stay",
                );
                signed(
                    ui,
                    "Contrast",
                    &mut adj.contrast,
                    "An S-curve about the middle, or flatter",
                );
                signed(
                    ui,
                    "Saturation",
                    &mut adj.saturation,
                    "More or less color; greys stay grey",
                );
            });
        });
        egui::CollapsingHeader::new("Selective color").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (k, (name, _)) in HUE_RANGES.iter().enumerate() {
                    let edited = !adj.selective[k].is_identity();
                    let label = format!("{name}{}", if edited { "*" } else { "" });
                    ui.selectable_value(&mut self.hue, k, label);
                }
            });
            let s = &mut adj.selective[self.hue];
            grid(ui, "selective", |ui| {
                ui.label("Hue");
                ui.add(
                    egui::Slider::new(&mut s.hue_deg, -60.0..=60.0)
                        .step_by(0.5)
                        .suffix("°"),
                )
                .on_hover_text("Towards the next hue round (+: reds towards yellow) or back");
                ui.end_row();
                signed(
                    ui,
                    "Saturation",
                    &mut s.saturation,
                    "More or less of this color",
                );
                signed(
                    ui,
                    "Lightness",
                    &mut s.lightness,
                    "This color lighter or darker",
                );
            });
            if ui.button("Reset color").clicked() {
                *s = Default::default();
            }
        });
        egui::CollapsingHeader::new("Grain and noise reduction").show(ui, |ui| {
            let n = &mut adj.noise;
            grid(ui, "noise", |ui| {
                ui.label("Grain");
                ui.add(egui::Slider::new(&mut n.grain, 0.0..=1.0).step_by(0.01))
                    .on_hover_text("Smooths film grain in flat areas, keeping edges");
                ui.end_row();
                ui.label("Color noise");
                ui.add(egui::Slider::new(&mut n.color, 0.0..=1.0).step_by(0.01))
                    .on_hover_text("Smooths blotches of color, keeping detail in brightness");
                ui.end_row();
                ui.label("Size");
                ui.add(
                    egui::Slider::new(&mut n.radius_px, 0.5..=8.0)
                        .step_by(0.1)
                        .suffix(" px"),
                )
                .on_hover_text("How big the grain is, in pixels of the saved frame");
                ui.end_row();
            });
        });
        egui::CollapsingHeader::new("Sharpening").show(ui, |ui| {
            let s = &mut adj.sharpen;
            grid(ui, "sharpen", |ui| {
                ui.label("Amount");
                ui.add(egui::Slider::new(&mut s.amount, 0.0..=3.0).step_by(0.01))
                    .on_hover_text("Unsharp mask: how much the edges are steepened");
                ui.end_row();
                ui.label("Radius");
                ui.add(
                    egui::Slider::new(&mut s.radius_px, 0.3..=5.0)
                        .step_by(0.05)
                        .suffix(" px"),
                )
                .on_hover_text("How wide the edges are, in pixels of the saved frame");
                ui.end_row();
                ui.label("Threshold");
                ui.add(egui::Slider::new(&mut s.threshold, 0.0..=0.2).step_by(0.002))
                    .on_hover_text(
                        "Differences smaller than this are left, so grain isn't sharpened",
                    );
                ui.end_row();
            });
            if let Some(source) = &self.source
                && source.scale < 0.999
            {
                ui.label(
                    RichText::new(format!(
                        "The view is {:.0}% of the saved size: zoom in to judge sharpening.",
                        source.scale * 100.0
                    ))
                    .weak()
                    .small(),
                );
            }
        });
    }
}

fn grid(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, add);
}

/// A −1 to 1 slider, shown as a percentage, in a grid row.
fn signed(ui: &mut egui::Ui, label: &str, value: &mut f32, hover: &str) {
    ui.label(label);
    let mut percent = *value * 100.0;
    if ui
        .add(
            egui::Slider::new(&mut percent, -100.0..=100.0)
                .step_by(1.0)
                .suffix("%"),
        )
        .on_hover_text(hover)
        .changed()
    {
        *value = percent / 100.0;
    }
    ui.end_row();
}

fn color_controls(ui: &mut egui::Ui, adj: &mut Adjustments, film: bool) {
    let how = if film {
        "on top of the roll's printing (the Invert step), in the film's own scene light, \
         before the print curve: as a photofinisher corrects each frame"
    } else {
        "on the scan's linear light"
    };
    grid(ui, "frame_color", |ui| {
        ui.label("Exposure");
        ui.add(
            egui::Slider::new(&mut adj.exposure_ev, -3.0..=3.0)
                .step_by(0.05)
                .suffix(" EV"),
        )
        .on_hover_text(format!("Brighter or darker, in stops, {how}"));
        ui.end_row();
        for (c, label) in ["Cyan - Red", "Magenta - Green", "Yellow - Blue"]
            .into_iter()
            .enumerate()
        {
            ui.label(label);
            ui.add(
                egui::Slider::new(&mut adj.balance_ev[c], -1.5..=1.5)
                    .step_by(0.01)
                    .suffix(" EV"),
            )
            .on_hover_text(format!("In stops, {how}"));
            ui.end_row();
        }
    });
    if ui.small_button("Reset color").clicked() {
        adj.exposure_ev = 0.0;
        adj.balance_ev = [0.0; 3];
    }
}

fn channel_picker(ui: &mut egui::Ui, channel: &mut usize, id: &str) {
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            for (i, name) in CHANNELS.iter().enumerate() {
                ui.selectable_value(channel, i, *name);
            }
        });
    });
}

fn levels_controls(ui: &mut egui::Ui, l: &mut Levels) {
    // Shown 0–255, as levels usually are.
    let level = |ui: &mut egui::Ui, label: &str, v: &mut f32, hover: &str| {
        ui.label(label);
        let mut x = *v * 255.0;
        if ui
            .add(egui::Slider::new(&mut x, 0.0..=255.0).step_by(1.0))
            .on_hover_text(hover)
            .changed()
        {
            *v = x / 255.0;
        }
        ui.end_row();
    };
    grid(ui, "levels", |ui| {
        level(
            ui,
            "Black",
            &mut l.black,
            "Input that becomes the output black",
        );
        ui.label("Midtones");
        ui.add(egui::Slider::new(&mut l.gamma, 0.2..=5.0).logarithmic(true))
            .on_hover_text("Gamma: above 1 lightens the midtones, below darkens them");
        ui.end_row();
        level(
            ui,
            "White",
            &mut l.white,
            "Input that becomes the output white",
        );
        level(
            ui,
            "Output black",
            &mut l.out_black,
            "The darkest output: above 0 for a softer black",
        );
        level(ui, "Output white", &mut l.out_white, "The lightest output");
    });
    l.black = l.black.min(l.white - 0.02);
    if ui.small_button("Reset levels").clicked() {
        *l = Levels::default();
    }
}

/// A histogram of `channel` ([`MASTER`]: brightness, with red, green and blue over it), with
/// `levels`' black, midtone and white marked under it.
fn histogram_plot(ui: &mut egui::Ui, h: &Histogram, channel: usize, levels: Option<&Levels>) {
    let width = ui.available_width().min(300.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 70.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    // Scaled to the inner bins' peak: clipped ends would flatten the rest.
    let peak = |bins: &[u32; 256]| bins[1..255].iter().copied().max().unwrap_or(1).max(1) as f32;
    let points = |bins: &[u32; 256], scale: f32| -> Vec<Pos2> {
        (0..256)
            .map(|i| {
                let x = rect.left() + (i as f32 + 0.5) / 256.0 * rect.width();
                let y = rect.bottom() - (bins[i] as f32 / scale).min(1.0) * (rect.height() - 2.0);
                Pos2::new(x, y)
            })
            .collect()
    };
    let main = &h.bins[channel];
    let scale = peak(main);
    for (i, &count) in main.iter().enumerate() {
        let x = rect.left() + i as f32 / 256.0 * rect.width();
        let top = rect.bottom() - (count as f32 / scale).min(1.0) * (rect.height() - 2.0);
        let bar = Rect::from_min_max(
            Pos2::new(x, top),
            Pos2::new(x + rect.width() / 256.0 + 0.5, rect.bottom()),
        );
        painter.rect_filled(bar, 0.0, CHANNEL_COLORS[channel].gamma_multiply(0.45));
    }
    if channel == MASTER {
        for (bins, color) in h.bins.iter().zip(CHANNEL_COLORS).skip(1) {
            painter.add(Shape::line(
                points(bins, peak(bins)),
                Stroke::new(1.0, color),
            ));
        }
    }
    if let Some(l) = levels {
        let x = |v: f32| rect.left() + v.clamp(0.0, 1.0) * rect.width();
        let mid = l.black + (l.white - l.black) * 0.5f32.powf(l.gamma);
        for (v, color) in [
            (l.black, Color32::BLACK),
            (mid, Color32::GRAY),
            (l.white, Color32::WHITE),
        ] {
            let p = Pos2::new(x(v), rect.bottom());
            painter.add(Shape::convex_polygon(
                vec![p, p + Vec2::new(-5.0, -8.0), p + Vec2::new(5.0, -8.0)],
                color,
                Stroke::new(1.0, Color32::DARK_GRAY),
            ));
        }
    }
}

/// An editable tone curve over the histogram `bins`: click to add a point, drag to move it,
/// double-click (or right-click) to remove it.
fn curve_editor(
    ui: &mut egui::Ui,
    curve: &mut Curve,
    color: Color32,
    bins: Option<&[u32; 256]>,
    drag: &mut Option<usize>,
) {
    let side = ui.available_width().min(260.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click_and_drag());
    let to_screen = |p: [f32; 2]| {
        Pos2::new(
            rect.left() + p[0] * rect.width(),
            rect.bottom() - p[1] * rect.height(),
        )
    };
    let to_curve = |s: Pos2| {
        [
            ((s.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
            ((rect.bottom() - s.y) / rect.height()).clamp(0.0, 1.0),
        ]
    };
    let near = |curve: &Curve, s: Pos2| {
        curve
            .points()
            .iter()
            .enumerate()
            .map(|(i, &p)| (i, to_screen(p).distance(s)))
            .filter(|&(_, d)| d < 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };

    if response.drag_started()
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
    {
        *drag = near(curve, origin).or_else(|| {
            let [x, y] = to_curve(origin);
            curve.insert(x, y)
        });
    }
    if let (Some(i), Some(p)) = (*drag, response.interact_pointer_pos())
        && response.dragged()
    {
        let [x, y] = to_curve(p);
        curve.set(i, x, y);
    }
    if response.drag_stopped() {
        *drag = None;
    }
    if let Some(p) = response.interact_pointer_pos() {
        if response.double_clicked() || response.secondary_clicked() {
            if let Some(i) = near(curve, p) {
                curve.remove(i);
            }
        } else if response.clicked() && near(curve, p).is_none() {
            let [x, y] = to_curve(p);
            curve.insert(x, y);
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    if let Some(bins) = bins {
        let peak = bins[1..255].iter().copied().max().unwrap_or(1).max(1) as f32;
        for (i, &b) in bins.iter().enumerate() {
            let x = rect.left() + i as f32 / 256.0 * rect.width();
            let top = rect.bottom() - (b as f32 / peak).min(1.0) * rect.height() * 0.6;
            let bar = Rect::from_min_max(
                Pos2::new(x, top),
                Pos2::new(x + rect.width() / 256.0 + 0.5, rect.bottom()),
            );
            painter.rect_filled(bar, 0.0, color.gamma_multiply(0.2));
        }
    }
    let grid = Stroke::new(0.5, ui.visuals().weak_text_color());
    for k in 1..4 {
        let f = k as f32 / 4.0;
        painter.line_segment([to_screen([f, 0.0]), to_screen([f, 1.0])], grid);
        painter.line_segment([to_screen([0.0, f]), to_screen([1.0, f])], grid);
    }
    painter.line_segment([to_screen([0.0, 0.0]), to_screen([1.0, 1.0])], grid);
    let line: Vec<Pos2> = (0..=128)
        .map(|k| {
            let x = k as f32 / 128.0;
            to_screen([x, curve.eval(x)])
        })
        .collect();
    painter.add(Shape::line(line, Stroke::new(2.0, color)));
    for (i, &p) in curve.points().iter().enumerate() {
        let held = *drag == Some(i);
        painter.circle(
            to_screen(p),
            if held { 5.0 } else { 4.0 },
            Color32::WHITE,
            Stroke::new(1.5, color),
        );
    }
}

/// The mean of `image` (0–1) over a few pixels around (u, v), 0–1 across and down.
fn mean_at(image: &Rgb16Image, u: f32, v: f32) -> [f32; 3] {
    let (w, h) = (image.width() as i64, image.height() as i64);
    let (cx, cy) = ((u * w as f32) as i64, (v * h as f32) as i64);
    let r = 2;
    let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
    for y in (cy - r).max(0)..=(cy + r).min(h - 1) {
        for x in (cx - r).max(0)..=(cx + r).min(w - 1) {
            let p = image.get_pixel(x as u32, y as u32).0;
            for c in 0..3 {
                sum[c] += f32::from(p[c]) / 65535.0;
            }
            n += 1.0;
        }
    }
    sum.map(|s| s / n.max(1.0))
}

/// `image` as an 8-bit texture image, flipped left to right if `mirror`.
fn color_image(image: &Rgb16Image, mirror: bool) -> ColorImage {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let src = image.as_raw();
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let sx = if mirror { w - 1 - x } else { x };
            let i = (y * w + sx) * 3;
            rgba.extend_from_slice(&[
                (src[i] >> 8) as u8,
                (src[i + 1] >> 8) as u8,
                (src[i + 2] >> 8) as u8,
                255,
            ]);
        }
    }
    ColorImage::from_rgba_unmultiplied([w, h], &rgba)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan_core::image::{ImageBuffer, Rgb};

    #[test]
    fn color_images_mirror() {
        let img: Rgb16Image = ImageBuffer::from_fn(2, 1, |x, _| Rgb([x as u16 * 65535, 0, 0]));
        assert_eq!(
            color_image(&img, false).pixels[1],
            Color32::from_rgb(255, 0, 0)
        );
        assert_eq!(
            color_image(&img, true).pixels[0],
            Color32::from_rgb(255, 0, 0)
        );
    }

    #[test]
    fn picks_average_around_the_point() {
        let img: Rgb16Image = ImageBuffer::from_pixel(20, 20, Rgb([32768, 0, 65535]));
        let m = mean_at(&img, 0.0, 1.0);
        assert!((m[0] - 0.5).abs() < 1e-3 && m[1] == 0.0 && m[2] == 1.0);
    }
}
