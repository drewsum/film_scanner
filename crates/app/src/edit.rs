//! The Adjust step: the selected frame (or the whole scan) shown developed with its own
//! edits, the controls that edit it, and the tools that work on the view.
//!
//! The frame is cut out of the scan at preview size once ([`scan_process::preview_frame`]),
//! with its border and straightening ([`adjust::framed`]), its spots healed, inverted with
//! its color ("base", cached until its exposure, color, spots or the roll's inversion
//! change), then edited ("output"), so moving a slider redoes only what it affects.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use eframe::egui::{
    self, Color32, ColorImage, Key, Modifiers, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2,
};
use scan_core::image::DynamicImage;
use scan_core::{MM_PER_INCH, ScanArea};
use scan_process::adjust::{
    self, Adjustments, CHANNELS, Curve, HUE_RANGES, Histogram, Levels, MASTER, Spot,
};
use scan_process::{FrameRect, Rgb16Image};

use crate::state::{AppState, Film};
use crate::tooltip_art::{ArtTip, Tip, art};
use crate::view::{ImageView, Placement};

/// Longest side of the preview a frame is edited on.
const PREVIEW_SIDE: u32 = 2048;
/// How the channels are drawn in histograms and curves: brightness, red, green, blue.
const CHANNEL_COLORS: [Color32; 4] = [
    Color32::from_gray(200),
    Color32::from_rgb(235, 80, 70),
    Color32::from_rgb(90, 200, 90),
    Color32::from_rgb(90, 140, 245),
];
/// Clipped pixels, when clipping is shown: to white in some channel, and to black.
const CLIPPED_WHITE: Color32 = Color32::from_rgb(255, 0, 0);
const CLIPPED_BLACK: Color32 = Color32::from_rgb(0, 80, 255);

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
    /// Heals a spot where clicked; right-click removes one.
    Heal,
    /// Straightens the frame along a line dragged over it.
    Level,
}

#[derive(Clone, Debug, PartialEq)]
struct SourceKey {
    generation: u64,
    image: u64,
    /// What is cut: the frame with its border and straightening.
    rect: Option<FrameRect>,
}

/// The frame cut out of the scan at preview size, 16-bit, not inverted.
struct Source {
    key: SourceKey,
    image: DynamicImage,
    /// Its pixels per pixel of the saved frame.
    scale: f32,
    /// Its pixels per millimetre on the bed.
    px_per_mm: f32,
    area: ScanArea,
    /// `image` with these spots healed.
    spots: Vec<Spot>,
    healed: DynamicImage,
}

impl Source {
    /// Heal `spots` on it, unless they are healed already.
    fn heal(&mut self, spots: &[Spot]) {
        if self.spots != spots {
            let px = adjust::spot_pixels(spots, self.key.rect.as_ref(), &self.area, self.px_per_mm);
            self.healed = adjust::heal(&self.image, &px).into_owned();
            self.spots = spots.to_vec();
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct BaseKey {
    source: SourceKey,
    spots: Vec<Spot>,
    film: Option<Film>,
    gamma: f32,
    exposure_ev: f32,
    /// With the temperature and tint: see [`Adjustments::color_ev`].
    balance_ev: [f32; 3],
}

struct Developed<K> {
    key: K,
    image: Arc<Rgb16Image>,
    histogram: Histogram,
}

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
    /// Show it before and after, divided where `split_at` is across the view.
    pub split: bool,
    split_at: f32,
    dragging_split: bool,
    /// Mark pixels clipped to black or white.
    pub clipping: bool,
    /// The healing brush's radius, in millimetres on the film.
    heal_radius_mm: f32,
    /// The line being dragged with the level tool, on screen.
    level_line: Option<(Pos2, Pos2)>,
    /// The channel levels and curves show.
    channel: usize,
    /// The hue range selective color shows.
    hue: usize,
    /// The curve point being dragged.
    curve_drag: Option<usize>,
    /// The name a preset is saved as.
    preset_name: String,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            view: ImageView::default(),
            source: None,
            base: None,
            output: None,
            before: None,
            shown: 0,
            version: 0,
            tool: Tool::None,
            compare: false,
            split: false,
            split_at: 0.5,
            dragging_split: false,
            clipping: false,
            heal_radius_mm: 0.5,
            level_line: None,
            channel: MASTER,
            hue: 0,
            curve_drag: None,
            preset_name: String::new(),
        }
    }
}

/// The frame the Adjust step edits: the selected one, or the whole scan.
pub fn target(state: &AppState) -> Option<usize> {
    state.crop.selected.filter(|&i| i < state.crop.frames.len())
}

fn target_name(target: Option<usize>) -> String {
    match target {
        Some(i) => format!("Frame {}", i + 1),
        None => "Whole scan".to_owned(),
    }
}

/// `source` developed with `key`'s exposure and color alone, healed if `key` has spots.
fn develop_base(source: &Source, key: BaseKey) -> Developed<BaseKey> {
    let color = Adjustments {
        exposure_ev: key.exposure_ev,
        balance_ev: key.balance_ev,
        ..Adjustments::default()
    };
    let inverter = key.film.map(|f| f.inverter(&color));
    let image = if key.spots.is_empty() {
        &source.image
    } else {
        &source.healed
    };
    let image = adjust::develop(image, key.gamma, inverter.as_ref(), &color, source.scale);
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
            rect: target.map(|i| state.crop.frames[i].cut()),
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
                healed: preview.clone(),
                image: preview,
                scale,
                px_per_mm: result.dpi as f32 / MM_PER_INCH * scale,
                area: result.area,
                spots: Vec::new(),
            });
            self.shown += 1;
        }
        let adj = state.adjustments(target);
        let Some(source) = &mut self.source else {
            return false;
        };
        source.heal(&adj.spots);
        let film = state.film();
        let base_key = BaseKey {
            source: key.clone(),
            spots: adj.spots.clone(),
            film,
            gamma: result.gamma,
            exposure_ev: adj.exposure_ev,
            balance_ev: adj.color_ev(),
        };
        if self.base.as_ref().is_none_or(|b| b.key != base_key) {
            self.base = Some(develop_base(source, base_key.clone()));
        }
        // Orientation is the view's, not the pixels'; the geometry and spots are the base's.
        let edits = Adjustments {
            quarter_turns: 0,
            mirror: false,
            straighten_deg: 0.0,
            border_mm: 0.0,
            spots: Vec::new(),
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
        if self.compare || self.split {
            let before_key = BaseKey {
                exposure_ev: 0.0,
                balance_ev: [0.0; 3],
                spots: Vec::new(),
                ..self.base.as_ref().expect("made above").key.clone()
            };
            if self.before.as_ref().is_none_or(|b| b.key != before_key) {
                self.before = Some(develop_base(source, before_key));
            }
        }
        true
    }

    /// The view of the frame being edited, with the tools.
    pub fn central(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        if !self.prepare(state) {
            return;
        }
        let target = target(state);
        ui.horizontal(|ui| {
            ui.label(RichText::new(target_name(target)).strong());
            ui.checkbox(&mut self.compare, "Before").on_hover_art(
                "Shows it without any of its Adjust edits, to compare (B)",
                "compare",
                &["before"],
            );
            ui.add_enabled(!self.compare, egui::Checkbox::new(&mut self.split, "Split"))
                .on_hover_art(
                    "Before on the left, after on the right: drag the line (S)",
                    "compare",
                    &["split"],
                );
            ui.checkbox(&mut self.clipping, "Clipping").on_hover_art(
                "Marks pixels clipped to white in any channel in red, and to black in blue (C)",
                "clipping",
                &[],
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Fit").clicked() {
                    self.view.reset();
                }
                let hint = match self.tool {
                    Tool::None => "Scroll to zoom, drag to pan, double-click to fit",
                    Tool::Grey => "Click something that should be grey",
                    Tool::Black => "Click what should be black",
                    Tool::White => "Click what should be white",
                    Tool::Heal => "Click a speck to heal it, right-click a healed one to undo it",
                    Tool::Level => "Drag along something that should be level or upright",
                };
                ui.label(RichText::new(hint).weak());
            });
        });
        // `prepare` again: "Before" or "Split" may have just been turned on.
        if !self.prepare(state) {
            return;
        }
        let adj = state.adjustments(target).clone();
        let turns = (state.quarter_turns + adj.quarter_turns) % 4;
        let mirror = adj.mirror;
        let split = self.split && !self.compare;
        let (Some(output), Some(source)) = (&self.output, &self.source) else {
            return;
        };
        let before = self.before.as_ref().map(|b| b.image.clone());
        let after = output.image.clone();
        let mut h = std::hash::DefaultHasher::new();
        (
            self.version,
            self.compare,
            split,
            self.clipping,
            mirror,
            turns,
        )
            .hash(&mut h);
        if split {
            ((self.split_at * 1000.0) as u32).hash(&mut h);
        }
        let variant = h.finish();
        let (compare, clipping, split_at) = (self.compare, self.clipping, self.split_at);
        let image = move || match (compare, split, &before) {
            (true, _, Some(b)) => color_image(b, mirror, false),
            (_, true, Some(b)) => split_image(
                &color_image(b, mirror, false),
                &color_image(&after, mirror, clipping),
                split_at,
                turns,
            ),
            _ => color_image(&after, mirror, clipping),
        };

        let tool = self.tool;
        let cut = source.key.rect;
        let area = source.area;
        let frame_mm = match &cut {
            Some(r) => (r.width_mm, r.height_mm),
            None => (area.width_mm, area.height_mm),
        };
        let spots = &adj.spots;
        let heal_radius = self.heal_radius_mm;
        let level_line = &mut self.level_line;
        let split_at = &mut self.split_at;
        let dragging_split = &mut self.dragging_split;
        let mut picked = None;
        let mut heal_at = None;
        let mut unheal_at = None;
        let mut level = None;
        self.view.show(
            ui,
            self.shown,
            variant,
            turns,
            image,
            |ui, response, placement| {
                let geometry = Geometry {
                    placement: *placement,
                    mirror,
                    cut,
                    area,
                    frame_mm,
                };
                let painter = ui.painter_at(response.rect);
                let mut took = false;
                if split {
                    let r = placement.rect;
                    let x = r.left() + *split_at * r.width();
                    painter.line_segment(
                        [Pos2::new(x, r.top()), Pos2::new(x, r.bottom())],
                        Stroke::new(1.5, Color32::WHITE),
                    );
                    let press = ui.input(|i| i.pointer.press_origin());
                    if response.drag_started()
                        && press.is_some_and(|p| (p.x - x).abs() < 8.0 && r.contains(p))
                    {
                        *dragging_split = true;
                    }
                    if *dragging_split {
                        took = true;
                        if let Some(p) = response.interact_pointer_pos() {
                            *split_at = ((p.x - r.left()) / r.width()).clamp(0.0, 1.0);
                        }
                        if response.drag_stopped() {
                            *dragging_split = false;
                        }
                    }
                    if response
                        .hover_pos()
                        .is_some_and(|p| (p.x - x).abs() < 8.0 && r.contains(p))
                    {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                }
                match tool {
                    Tool::None => {}
                    Tool::Grey | Tool::Black | Tool::White => {
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                        }
                        if response.clicked()
                            && let Some(p) = response.interact_pointer_pos()
                        {
                            let at = geometry.to_unmirrored(p);
                            if (0.0..=1.0).contains(&at.x) && (0.0..=1.0).contains(&at.y) {
                                picked = Some(at);
                            }
                        }
                    }
                    Tool::Heal => {
                        let ppm = geometry.screen_px_per_mm();
                        for s in spots {
                            painter.circle_stroke(
                                geometry.bed_to_screen(s.x_mm, s.y_mm),
                                s.radius_mm * ppm,
                                Stroke::new(1.0, Color32::from_rgb(255, 220, 60)),
                            );
                        }
                        if let Some(p) = response.hover_pos() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                            painter.circle_stroke(
                                p,
                                heal_radius * ppm,
                                Stroke::new(1.5, Color32::WHITE),
                            );
                        }
                        if let Some(p) = response.interact_pointer_pos() {
                            if response.clicked() {
                                heal_at = Some(geometry.screen_to_bed(p));
                            } else if response.secondary_clicked() {
                                unheal_at = Some(geometry.screen_to_bed(p));
                            }
                        }
                    }
                    Tool::Level => {
                        if response.drag_started()
                            && let Some(p) = ui.input(|i| i.pointer.press_origin())
                        {
                            *level_line = Some((p, p));
                        }
                        if let (Some(line), Some(p)) =
                            (level_line.as_mut(), response.interact_pointer_pos())
                        {
                            took = true;
                            line.1 = p;
                            if response.drag_stopped() {
                                level = Some(geometry.line_angle(line.0, line.1));
                                *level_line = None;
                            }
                        }
                        if let Some((a, b)) = *level_line {
                            painter.line_segment([a, b], Stroke::new(2.0, Color32::YELLOW));
                        }
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                        }
                    }
                }
                took
            },
        );
        if let Some(at) = picked {
            self.pick(state, target, at);
        }
        if let Some((x, y)) = heal_at {
            state.adjustments_mut(target).spots.push(Spot {
                x_mm: x,
                y_mm: y,
                radius_mm: self.heal_radius_mm,
            });
        }
        if let Some((x, y)) = unheal_at {
            let spots = &mut state.adjustments_mut(target).spots;
            let nearest = spots
                .iter()
                .enumerate()
                .map(|(i, s)| (i, (s.x_mm - x).hypot(s.y_mm - y) - s.radius_mm))
                .filter(|&(_, d)| d < 0.3)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((i, _)) = nearest {
                spots.remove(i);
            }
        }
        if let Some(Some(angle)) = level {
            let adj = state.adjustments_mut(target);
            adj.straighten_deg = (adj.straighten_deg - angle).clamp(-STRAIGHTEN, STRAIGHTEN);
            state.status = format!("Straightened by {:+.2}°", -angle);
            self.tool = Tool::None;
        }
    }

    /// Use the picker at `at` (0–1 across and down the frame, as it is cut, not mirrored).
    fn pick(&mut self, state: &mut AppState, target: Option<usize>, at: Pos2) {
        match self.tool {
            Tool::Grey => {
                let (Some(source), Some(result)) = (&self.source, &state.result) else {
                    return;
                };
                let DynamicImage::ImageRgb16(image) = &source.healed else {
                    return;
                };
                let gamma = result.gamma;
                let value = mean_at(image, at.x, at.y);
                state.pick_neutral(target, value.map(|v| v.powf(gamma)));
            }
            Tool::Black | Tool::White => {
                let Some(base) = &self.base else {
                    return;
                };
                let value = mean_at(&base.image, at.x, at.y);
                state.pick_level(target, value, self.tool == Tool::White);
            }
            _ => return,
        }
        self.tool = Tool::None;
    }

    /// Select `selected` (a frame, or the whole scan), putting the tools down.
    pub fn select(&mut self, state: &mut AppState, selected: Option<usize>) {
        if selected != target(state) {
            state.crop.selected = selected;
            self.tool = Tool::None;
            self.curve_drag = None;
            self.level_line = None;
        }
    }

    /// The Adjust step's keyboard shortcuts, unless typing.
    pub fn shortcuts(&mut self, ctx: &egui::Context, state: &mut AppState) {
        if ctx.text_edit_focused() {
            return;
        }
        let target = target(state);
        let command = |key| ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, key));
        let shift_command =
            |key| ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, key));
        if shift_command(Key::Z) || command(Key::Y) {
            state.redo(target);
        } else if command(Key::Z) {
            state.undo(target);
        }
        // Copy and paste arrive as their own events, not as key presses.
        let (copy, paste) = ctx.input(|i| {
            let copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy));
            let paste = i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)));
            (copy, paste)
        });
        if copy {
            state.copy_adjustments(target);
            state.status = format!("Copied the edits of {}", target_name(target).to_lowercase());
        }
        if paste {
            state.paste_adjustments(target);
        }
        let key = |key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
        if key(Key::A) {
            state.auto_adjust(target);
        }
        if key(Key::B) {
            self.compare = !self.compare;
        }
        if key(Key::S) {
            self.split = !self.split;
        }
        if key(Key::C) {
            self.clipping = !self.clipping;
        }
        if key(Key::H) {
            self.toggle(Tool::Heal);
        }
        if key(Key::L) && target.is_some() {
            self.toggle(Tool::Level);
        }
        if key(Key::G) {
            self.toggle(Tool::Grey);
        }
        if key(Key::Escape) {
            self.tool = Tool::None;
        }
        if let Some(i) = target {
            let adj = state.adjustments_mut(Some(i));
            if key(Key::OpenBracket) {
                adj.quarter_turns = (adj.quarter_turns + 3) % 4;
            }
            if key(Key::CloseBracket) {
                adj.quarter_turns = (adj.quarter_turns + 1) % 4;
            }
            if key(Key::X) {
                let frame = &mut state.crop.frames[i];
                frame.skip = !frame.skip;
            }
        }
    }

    fn toggle(&mut self, tool: Tool) {
        self.tool = if self.tool == tool { Tool::None } else { tool };
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
            for (i, frame) in state.crop.frames.iter().enumerate() {
                let edited = !frame.adjust.is_identity();
                let label = format!(
                    "{}{}",
                    i + 1,
                    match (edited, frame.skip) {
                        (_, true) => " -",
                        (true, false) => "*",
                        (false, false) => "",
                    }
                );
                ui.selectable_value(&mut selected, Some(i), label)
                    .on_hover_text(format!(
                        "Frame {}{}{}",
                        i + 1,
                        if edited { ", edited" } else { "" },
                        if frame.skip { ", skipped" } else { "" }
                    ));
            }
        });
        self.select(state, selected);
        if state.crop.frames.is_empty() {
            ui.label(
                RichText::new("No frames: detect or add them in the Crop step to edit each one.")
                    .weak(),
            );
        }
        let target = target(state);
        self.prepare(state);
        let mono = state.inverter().is_some_and(|i| i.is_mono());

        ui.add_space(4.0);
        self.frame_buttons(ui, state, target);
        ui.horizontal(|ui| {
            ui.label("Pick");
            for (tool, label, hover) in [
                (
                    Tool::Grey,
                    "Grey",
                    "Click a grey in the frame to balance it (G)",
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
                    .add_enabled(
                        !mono || tool != Tool::Grey,
                        egui::Button::selectable(on, label),
                    )
                    .on_hover_text(hover)
                    .clicked()
                {
                    self.toggle(tool);
                }
            }
        });
        if let Some(i) = target {
            let frame = &mut state.crop.frames[i];
            let adj = &mut frame.adjust;
            ui.horizontal(|ui| {
                ui.label("Turn");
                if ui
                    .button("⟲")
                    .on_hover_text("This frame a quarter anticlockwise ([)")
                    .clicked()
                {
                    adj.quarter_turns = (adj.quarter_turns + 3) % 4;
                }
                if ui
                    .button("⟳")
                    .on_hover_text("This frame a quarter clockwise (])")
                    .clicked()
                {
                    adj.quarter_turns = (adj.quarter_turns + 1) % 4;
                }
                ui.checkbox(&mut adj.mirror, "Mirror")
                    .on_hover_text("Flips it left to right: film scanned the wrong way up");
                let mut export = !frame.skip;
                if ui
                    .checkbox(&mut export, "Export")
                    .on_hover_text("Off leaves the frame out of exports and saves (X)")
                    .changed()
                {
                    frame.skip = !export;
                }
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
            .show(ui, |ui| color_controls(ui, adj, film, mono));
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
        egui::CollapsingHeader::new("Tone").show(ui, |ui| {
            grid(ui, "tone", |ui| {
                signed(
                    ui,
                    "Brightness",
                    &mut adj.brightness,
                    art(
                        "Midtones lighter or darker; black and white stay",
                        "brightness",
                        &["brightness"],
                    ),
                );
                signed(
                    ui,
                    "Contrast",
                    &mut adj.contrast,
                    art(
                        "An S-curve about the middle, or flatter",
                        "contrast",
                        &["contrast"],
                    ),
                );
                signed(
                    ui,
                    "Highlights",
                    &mut adj.highlights,
                    art(
                        "Light areas darker (-) to recover them, or lighter, keeping their detail",
                        "tone_range",
                        &["highlights"],
                    ),
                );
                signed(
                    ui,
                    "Shadows",
                    &mut adj.shadows,
                    art(
                        "Dark areas lighter (+) to open them up, or darker, keeping their detail",
                        "tone_range",
                        &["shadows"],
                    ),
                );
                signed(
                    ui,
                    "Clarity",
                    &mut adj.clarity,
                    art(
                        "Local contrast in the midtones: crisper (+) or softer, without halos",
                        "clarity",
                        &["clarity"],
                    ),
                );
            });
        });
        egui::CollapsingHeader::new("Saturation").show(ui, |ui| {
            grid(ui, "saturation", |ui| {
                signed(
                    ui,
                    "Saturation",
                    &mut adj.saturation,
                    "More or less color; greys stay grey",
                );
                signed(
                    ui,
                    "Vibrance",
                    &mut adj.vibrance,
                    "More or less color where it is muted, sparing skin and colors already strong",
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
                .on_hover_art(
                    "Towards the next hue round (+: reds towards yellow) or back",
                    "hue_shift",
                    &["hue"],
                );
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
                    .on_hover_art(
                        "Smooths film grain in flat areas, keeping edges",
                        "noise",
                        &["grain"],
                    );
                ui.end_row();
                ui.label("Color noise");
                ui.add(egui::Slider::new(&mut n.color, 0.0..=1.0).step_by(0.01))
                    .on_hover_art(
                        "Smooths blotches of color, keeping detail in brightness",
                        "noise",
                        &["color"],
                    );
                ui.end_row();
                ui.label("Size");
                ui.add(
                    egui::Slider::new(&mut n.radius_px, 0.5..=8.0)
                        .step_by(0.1)
                        .suffix(" px"),
                )
                .on_hover_art(
                    "How big the grain is, in pixels of the saved frame",
                    "noise",
                    &["size"],
                );
                ui.end_row();
            });
        });
        egui::CollapsingHeader::new("Sharpening").show(ui, |ui| {
            let s = &mut adj.sharpen;
            grid(ui, "sharpen", |ui| {
                ui.label("Amount");
                ui.add(egui::Slider::new(&mut s.amount, 0.0..=3.0).step_by(0.01))
                    .on_hover_art(
                        "Unsharp mask: how much the edges are steepened",
                        "sharpen",
                        &["amount"],
                    );
                ui.end_row();
                ui.label("Radius");
                ui.add(
                    egui::Slider::new(&mut s.radius_px, 0.3..=5.0)
                        .step_by(0.05)
                        .suffix(" px"),
                )
                .on_hover_art(
                    "How wide the edges are, in pixels of the saved frame",
                    "sharpen",
                    &["radius"],
                );
                ui.end_row();
                ui.label("Threshold");
                ui.add(egui::Slider::new(&mut s.threshold, 0.0..=0.2).step_by(0.002))
                    .on_hover_art(
                        "Differences smaller than this are left, so grain isn't sharpened",
                        "sharpen",
                        &["threshold"],
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
        egui::CollapsingHeader::new("Light falloff and vignette").show(ui, |ui| {
            grid(ui, "edges", |ui| {
                ui.label("Light falloff");
                let mut percent = adj.falloff * 100.0;
                if ui
                    .add(
                        egui::Slider::new(&mut percent, 0.0..=100.0)
                            .step_by(1.0)
                            .suffix("%"),
                    )
                    .on_hover_art(
                        "Brightens the edges and corners, for a scanner's or lens's light \
                         falling off towards them",
                        "falloff",
                        &["falloff"],
                    )
                    .changed()
                {
                    adj.falloff = percent / 100.0;
                }
                ui.end_row();
                signed(
                    ui,
                    "Vignette",
                    &mut adj.vignette,
                    art(
                        "Darker (-) or lighter corners, for effect",
                        "falloff",
                        &["vignette"],
                    ),
                );
            });
        });
        if target.is_some() {
            egui::CollapsingHeader::new("Straighten and border").show(ui, |ui| {
                // Shown as the picture turns on screen: a mirrored frame turns the other way.
                let sign = if adj.mirror { -1.0 } else { 1.0 };
                grid(ui, "geometry", |ui| {
                    ui.label("Straighten");
                    let mut deg = adj.straighten_deg * sign;
                    if ui
                        .add(
                            egui::Slider::new(&mut deg, -STRAIGHTEN..=STRAIGHTEN)
                                .step_by(0.05)
                                .suffix("°"),
                        )
                        .on_hover_art(
                            "Turns the picture clockwise (+), cutting it just smaller so no \
                             corner is empty",
                            "straighten",
                            &["straighten"],
                        )
                        .changed()
                    {
                        adj.straighten_deg = deg * sign;
                    }
                    ui.end_row();
                    ui.label("Border");
                    ui.add(
                        egui::Slider::new(&mut adj.border_mm, 0.0..=4.0)
                            .step_by(0.05)
                            .suffix(" mm"),
                    )
                    .on_hover_art(
                        "Keeps this much film around the frame: its edges, rebate and \
                         sprocket holes",
                        "border",
                        &["border"],
                    );
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    let on = self.tool == Tool::Level;
                    if ui
                        .add(egui::Button::selectable(on, "Level"))
                        .on_hover_art(
                            "Drag along a horizon or upright to straighten along it (L)",
                            "straighten",
                            &["level"],
                        )
                        .clicked()
                    {
                        self.toggle(Tool::Level);
                    }
                    if ui.small_button("Reset").clicked() {
                        adj.straighten_deg = 0.0;
                        adj.border_mm = 0.0;
                    }
                });
            });
        }
        egui::CollapsingHeader::new("Spot healing").show(ui, |ui| {
            ui.horizontal(|ui| {
                let on = self.tool == Tool::Heal;
                if ui
                    .add(egui::Button::selectable(on, "Heal"))
                    .on_hover_art(
                        "Click dust, hairs and scratches to fill them in from around them (H): \
                         for what the infrared pass misses, and for black-and-white film and \
                         Kodachrome, which infrared can't see through",
                        "heal",
                        &["heal"],
                    )
                    .clicked()
                {
                    self.toggle(Tool::Heal);
                }
                ui.add(
                    egui::Slider::new(&mut self.heal_radius_mm, 0.05..=3.0)
                        .logarithmic(true)
                        .suffix(" mm"),
                )
                .on_hover_art(
                    "The brush's radius, on the film: cover the whole speck, as it is filled \
                     in from what is just outside the brush",
                    "heal",
                    &["brush"],
                );
            });
            let n = adj.spots.len();
            ui.horizontal(|ui| {
                ui.label(format!("{n} spot{} healed", if n == 1 { "" } else { "s" }));
                if ui
                    .add_enabled(n > 0, egui::Button::new("Remove last").small())
                    .clicked()
                {
                    adj.spots.pop();
                }
                if ui
                    .add_enabled(n > 0, egui::Button::new("Clear").small())
                    .clicked()
                {
                    adj.spots.clear();
                }
            });
        });
        self.presets(ui, state, target);
        egui::CollapsingHeader::new("Keyboard shortcuts").show(ui, |ui| {
            grid(ui, "keys", |ui| {
                for (keys, what) in SHORTCUTS {
                    ui.label(RichText::new(*keys).monospace());
                    ui.label(*what);
                    ui.end_row();
                }
            });
        });
    }

    fn frame_buttons(&mut self, ui: &mut egui::Ui, state: &mut AppState, target: Option<usize>) {
        let frames = !state.crop.frames.is_empty();
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Auto")
                .on_hover_text(
                    "Color and levels for this frame alone: a negative's frame is balanced as \
                     a photofinisher's printer would on its own, then stretched to full range (A)",
                )
                .clicked()
            {
                state.auto_adjust(target);
            }
            if ui
                .add_enabled(frames, egui::Button::new("Auto all"))
                .on_hover_text("Auto for every frame, each on its own")
                .clicked()
            {
                state.auto_all_frames();
            }
            if ui
                .add_enabled(state.crop.frames.len() > 1, egui::Button::new("Match roll"))
                .on_hover_text(
                    "Brings every frame's exposure and color to the roll's typical frame, so \
                     frames shot in the same light look alike, rather than each made neutral \
                     on its own",
                )
                .clicked()
            {
                state.match_roll();
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Reset")
                .on_hover_text(
                    "Undoes this frame's edits, but not its turn, straightening or spots",
                )
                .clicked()
            {
                state.reset_adjustments(target);
            }
            if ui.button("Copy").on_hover_text("Ctrl+C").clicked() {
                state.copy_adjustments(target);
            }
            let can_paste = state.clipboard.is_some();
            if ui
                .add_enabled(can_paste, egui::Button::new("Paste"))
                .on_hover_text("Everything but the turn, straightening and spots (Ctrl+V)")
                .clicked()
            {
                state.paste_adjustments(target);
            }
            if ui
                .add_enabled(can_paste && frames, egui::Button::new("Paste to all"))
                .on_hover_text(
                    "Onto every frame, everything but their turns, straightening and spots",
                )
                .clicked()
            {
                state.paste_to_all_frames();
            }
        });
        ui.horizontal(|ui| {
            let history = state.history(target);
            let (undo, redo) = (history.can_undo(), history.can_redo());
            if ui
                .add_enabled(undo, egui::Button::new("Undo"))
                .on_hover_text("This frame's last change (Ctrl+Z)")
                .clicked()
            {
                state.undo(target);
            }
            if ui
                .add_enabled(redo, egui::Button::new("Redo"))
                .on_hover_text("Ctrl+Y")
                .clicked()
            {
                state.redo(target);
            }
        });
    }

    fn presets(&mut self, ui: &mut egui::Ui, state: &mut AppState, target: Option<usize>) {
        egui::CollapsingHeader::new("Presets").show(ui, |ui| {
            let mut apply = None;
            let mut apply_all = None;
            let mut delete = None;
            for (i, preset) in state.presets.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(&preset.name);
                    if ui
                        .small_button("Apply")
                        .on_hover_text("Onto this frame, keeping its turn, straightening and spots")
                        .clicked()
                    {
                        apply = Some(i);
                    }
                    if ui
                        .add_enabled(
                            !state.crop.frames.is_empty(),
                            egui::Button::new("All").small(),
                        )
                        .on_hover_text("Onto every frame")
                        .clicked()
                    {
                        apply_all = Some(i);
                    }
                    if ui.small_button("Delete").clicked() {
                        delete = Some(i);
                    }
                });
            }
            if state.presets.is_empty() {
                ui.label(RichText::new("None yet.").weak());
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.preset_name)
                        .hint_text("Gold 200 sunny")
                        .desired_width(140.0),
                );
                if ui
                    .add_enabled(
                        !self.preset_name.trim().is_empty(),
                        egui::Button::new("Save"),
                    )
                    .on_hover_text(
                        "Saves this frame's edits, but not its geometry, under this name: kept \
                         for later rolls",
                    )
                    .clicked()
                {
                    state.save_preset(&self.preset_name, target);
                    self.preset_name.clear();
                }
            });
            if let Some(i) = apply {
                state.apply_preset(i, target);
            }
            if let Some(i) = apply_all {
                state.apply_preset_to_all(i);
            }
            if let Some(i) = delete {
                state.presets.remove(i);
            }
        });
    }
}

/// How far the Adjust step straightens, either way, in degrees.
const STRAIGHTEN: f32 = 10.0;

/// The Adjust step's shortcuts, as listed for the user.
const SHORTCUTS: &[(&str, &str)] = &[
    ("PgUp PgDn", "Previous and next frame (also ← →)"),
    ("Ctrl+Z", "Undo"),
    ("Ctrl+Y", "Redo (also Ctrl+Shift+Z)"),
    ("Ctrl+C Ctrl+V", "Copy and paste edits"),
    ("A", "Auto"),
    ("B", "Before"),
    ("S", "Split before and after"),
    ("C", "Clipping"),
    ("G", "Grey picker"),
    ("H", "Heal"),
    ("L", "Level"),
    ("[ ]", "Turn anticlockwise, clockwise"),
    ("X", "Export the frame or skip it"),
    ("Esc", "Put the tool down"),
    ("Ctrl+S", "Save the roll"),
    ("Ctrl+O", "Open a roll"),
];

/// Where the frame being edited is on screen, and on the bed.
struct Geometry {
    placement: Placement,
    mirror: bool,
    /// What is cut for the frame, or `None` for the whole scan of `area`.
    cut: Option<FrameRect>,
    area: ScanArea,
    /// The cut's width and height, in millimetres.
    frame_mm: (f32, f32),
}

impl Geometry {
    /// A point on screen as 0–1 across and down the frame as it is cut (not mirrored).
    fn to_unmirrored(&self, p: Pos2) -> Pos2 {
        let at = self.placement.to_image(p);
        Pos2::new(if self.mirror { 1.0 - at.x } else { at.x }, at.y)
    }

    fn screen_to_bed(&self, p: Pos2) -> (f32, f32) {
        let at = self.to_unmirrored(p);
        let (w, h) = self.frame_mm;
        match &self.cut {
            Some(r) => r.from_local((at.x - 0.5) * w, (at.y - 0.5) * h),
            None => (self.area.x_mm + at.x * w, self.area.y_mm + at.y * h),
        }
    }

    fn bed_to_screen(&self, x: f32, y: f32) -> Pos2 {
        let (w, h) = self.frame_mm;
        let (u, v) = match &self.cut {
            Some(r) => {
                let (u, v) = r.to_local(x, y);
                (u / w + 0.5, v / h + 0.5)
            }
            None => ((x - self.area.x_mm) / w, (y - self.area.y_mm) / h),
        };
        let u = if self.mirror { 1.0 - u } else { u };
        self.placement.to_screen(Pos2::new(u, v))
    }

    fn screen_px_per_mm(&self) -> f32 {
        let (w, h) = self.frame_mm;
        let across = if self.placement.quarter_turns.is_multiple_of(2) {
            w
        } else {
            h
        };
        self.placement.rect.width() / across.max(1e-3)
    }

    /// The angle, clockwise in degrees, of the line from `a` to `b` on screen in the frame
    /// as it is cut, from level or from upright, whichever is nearer: what straightening
    /// takes away to level it. `None` for a line too short to tell.
    fn line_angle(&self, a: Pos2, b: Pos2) -> Option<f32> {
        if a.distance(b) < 10.0 {
            return None;
        }
        let (p, q) = (self.to_unmirrored(a), self.to_unmirrored(b));
        let (w, h) = self.frame_mm;
        let (dx, dy) = ((q.x - p.x) * w, (q.y - p.y) * h);
        // Either direction along the line is the same line: -90 to 90.
        let mut angle = dy.atan2(dx).to_degrees();
        if angle > 90.0 {
            angle -= 180.0;
        } else if angle <= -90.0 {
            angle += 180.0;
        }
        Some(if angle > 45.0 {
            angle - 90.0
        } else if angle < -45.0 {
            angle + 90.0
        } else {
            angle
        })
    }
}

fn grid(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, add);
}

/// A −1 to 1 slider, shown as a percentage, in a grid row.
fn signed<'a>(ui: &mut egui::Ui, label: &str, value: &mut f32, hover: impl Into<Tip<'a>>) {
    signed_if(ui, true, label, value, hover);
}

/// [`signed`], greyed out unless `enabled`.
fn signed_if<'a>(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut f32,
    hover: impl Into<Tip<'a>>,
) {
    ui.add_enabled(enabled, egui::Label::new(label));
    let mut percent = *value * 100.0;
    let slider = ui.add_enabled(
        enabled,
        egui::Slider::new(&mut percent, -100.0..=100.0)
            .step_by(1.0)
            .suffix("%"),
    );
    if hover.into().on(slider).changed() {
        *value = percent / 100.0;
    }
    ui.end_row();
}

fn color_controls(ui: &mut egui::Ui, adj: &mut Adjustments, film: bool, mono: bool) {
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
        signed_if(
            ui,
            !mono,
            "Temperature",
            &mut adj.temperature,
            art(
                "Warmer (+, more amber) or cooler (more blue)",
                "white_balance",
                &["temperature"],
            ),
        );
        signed_if(
            ui,
            !mono,
            "Tint",
            &mut adj.tint,
            art("More magenta (+) or more green", "white_balance", &["tint"]),
        );
        for (c, (label, part)) in [
            ("Cyan - Red", "cr"),
            ("Magenta - Green", "mg"),
            ("Yellow - Blue", "yb"),
        ]
        .into_iter()
        .enumerate()
        {
            ui.add_enabled(!mono, egui::Label::new(label));
            ui.add_enabled(
                !mono,
                egui::Slider::new(&mut adj.balance_ev[c], -1.5..=1.5)
                    .step_by(0.01)
                    .suffix(" EV"),
            )
            .on_hover_art(format!("In stops, {how}"), "balance", &[part]);
            ui.end_row();
        }
    });
    if mono {
        ui.label(
            RichText::new("Black-and-white film: its color doesn't apply.")
                .weak()
                .small(),
        );
    }
    if ui.small_button("Reset color").clicked() {
        adj.exposure_ev = 0.0;
        adj.balance_ev = [0.0; 3];
        adj.temperature = 0.0;
        adj.tint = 0.0;
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
    let level = |ui: &mut egui::Ui, label: &str, v: &mut f32, hover: &str, part: &str| {
        ui.label(label);
        let mut x = *v * 255.0;
        if ui
            .add(egui::Slider::new(&mut x, 0.0..=255.0).step_by(1.0))
            .on_hover_art(hover, "levels", &[part])
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
            "black",
        );
        ui.label("Midtones");
        ui.add(egui::Slider::new(&mut l.gamma, 0.2..=5.0).logarithmic(true))
            .on_hover_art(
                "Gamma: above 1 lightens the midtones, below darkens them",
                "levels",
                &["gamma"],
            );
        ui.end_row();
        level(
            ui,
            "White",
            &mut l.white,
            "Input that becomes the output white",
            "white",
        );
        level(
            ui,
            "Output black",
            &mut l.out_black,
            "The darkest output: above 0 for a softer black",
            "out_black",
        );
        level(
            ui,
            "Output white",
            &mut l.out_white,
            "The lightest output",
            "out_white",
        );
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

/// `image` as an 8-bit texture image, flipped left to right if `mirror`, with clipped
/// pixels marked if `clipping`.
pub fn color_image(image: &Rgb16Image, mirror: bool, clipping: bool) -> ColorImage {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let src = image.as_raw();
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let sx = if mirror { w - 1 - x } else { x };
            let i = (y * w + sx) * 3;
            let p = [src[i] >> 8, src[i + 1] >> 8, src[i + 2] >> 8].map(|v| v as u8);
            let p = if !clipping {
                p
            } else if p.contains(&255) {
                [CLIPPED_WHITE.r(), CLIPPED_WHITE.g(), CLIPPED_WHITE.b()]
            } else if p.contains(&0) {
                [CLIPPED_BLACK.r(), CLIPPED_BLACK.g(), CLIPPED_BLACK.b()]
            } else {
                p
            };
            rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
    }
    ColorImage::from_rgba_unmultiplied([w, h], &rgba)
}

/// `before` where it shows left of `at` (0–1 across the view) once turned `quarter_turns`
/// clockwise, and `after` right of it. Both are the same size.
fn split_image(before: &ColorImage, after: &ColorImage, at: f32, quarter_turns: u8) -> ColorImage {
    let [w, h] = after.size;
    let mut out = after.clone();
    for y in 0..h {
        for x in 0..w {
            let (u, v) = ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
            // Where across the view this texel lands: see `Placement::to_screen`.
            let across = match quarter_turns % 4 {
                0 => u,
                1 => 1.0 - v,
                2 => 1.0 - u,
                _ => v,
            };
            if across < at {
                out.pixels[y * w + x] = before.pixels[y * w + x];
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan_core::image::{ImageBuffer, Rgb};

    #[test]
    fn color_images_mirror() {
        let img: Rgb16Image = ImageBuffer::from_fn(2, 1, |x, _| Rgb([x as u16 * 65535, 0, 0]));
        assert_eq!(
            color_image(&img, false, false).pixels[1],
            Color32::from_rgb(255, 0, 0)
        );
        assert_eq!(
            color_image(&img, true, false).pixels[0],
            Color32::from_rgb(255, 0, 0)
        );
    }

    #[test]
    fn clipping_is_marked() {
        let img: Rgb16Image = ImageBuffer::from_fn(3, 1, |x, _| match x {
            0 => Rgb([65535, 30000, 30000]),
            1 => Rgb([30000, 0, 30000]),
            _ => Rgb([30000, 30000, 30000]),
        });
        let c = color_image(&img, false, true);
        assert_eq!(c.pixels[0], CLIPPED_WHITE);
        assert_eq!(c.pixels[1], CLIPPED_BLACK);
        assert_eq!(c.pixels[2], Color32::from_gray(117));
    }

    #[test]
    fn splits_follow_the_view() {
        let before = ColorImage::filled([4, 2], Color32::BLACK);
        let after = ColorImage::filled([4, 2], Color32::WHITE);
        // Upright, the left half is before.
        let s = split_image(&before, &after, 0.5, 0);
        assert_eq!(
            &s.pixels[..4],
            &[
                Color32::BLACK,
                Color32::BLACK,
                Color32::WHITE,
                Color32::WHITE
            ]
        );
        // A quarter clockwise, the image's bottom row is on the view's left.
        let s = split_image(&before, &after, 0.5, 1);
        assert_eq!((s.pixels[0], s.pixels[4]), (Color32::WHITE, Color32::BLACK));
    }

    #[test]
    fn picks_average_around_the_point() {
        let img: Rgb16Image = ImageBuffer::from_pixel(20, 20, Rgb([32768, 0, 65535]));
        let m = mean_at(&img, 0.0, 1.0);
        assert!((m[0] - 0.5).abs() < 1e-3 && m[1] == 0.0 && m[2] == 1.0);
    }

    fn geometry(mirror: bool, quarter_turns: u8) -> Geometry {
        Geometry {
            placement: Placement {
                rect: Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(360.0, 240.0)),
                quarter_turns,
            },
            mirror,
            cut: Some(FrameRect {
                cx_mm: 30.0,
                cy_mm: 20.0,
                width_mm: 36.0,
                height_mm: 24.0,
                angle_deg: 0.0,
            }),
            area: ScanArea::full(100.0, 100.0),
            frame_mm: (36.0, 24.0),
        }
    }

    #[test]
    fn spots_go_to_the_bed_and_back() {
        for mirror in [false, true] {
            let g = geometry(mirror, 0);
            let p = Pos2::new(130.0, 80.0);
            let (x, y) = g.screen_to_bed(p);
            assert!(g.bed_to_screen(x, y).distance(p) < 1e-3);
            // 10 screen pixels per millimetre.
            assert!((g.screen_px_per_mm() - 10.0).abs() < 1e-4);
        }
        // Unmirrored, the view's top-left is the frame's.
        let (x, y) = geometry(false, 0).screen_to_bed(Pos2::new(100.0, 50.0));
        assert!((x - 12.0).abs() < 1e-3 && (y - 8.0).abs() < 1e-3);
    }

    #[test]
    fn level_lines_measure_from_level_or_upright() {
        let g = geometry(false, 0);
        let a = Pos2::new(120.0, 100.0);
        // Down 10 px over 200 px across: about 2.9° clockwise.
        let angle = g.line_angle(a, a + Vec2::new(200.0, 10.0)).unwrap();
        assert!((angle - 2.862).abs() < 0.01, "{angle}");
        // The same line drawn the other way is the same line.
        let back = g.line_angle(a + Vec2::new(200.0, 10.0), a).unwrap();
        assert!((back - angle).abs() < 1e-4);
        // Nearly upright, leaning right at the bottom: anticlockwise of upright.
        let up = g.line_angle(a, a + Vec2::new(5.0, 100.0)).unwrap();
        assert!((up + 2.862).abs() < 0.01, "{up}");
        // Mirrored, the frame as cut leans the other way.
        let m = geometry(true, 0)
            .line_angle(a, a + Vec2::new(200.0, 10.0))
            .unwrap();
        assert!((m + angle).abs() < 1e-4);
        assert!(g.line_angle(a, a + Vec2::new(3.0, 0.0)).is_none());
    }
}
