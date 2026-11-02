//! The filmstrip: every frame as a small picture with its edits, along the bottom of the
//! Adjust and Export steps, to pick one and to tick which are exported.

use eframe::egui::{self, Color32, RichText, TextureHandle, TextureOptions, Vec2};
use scan_core::MM_PER_INCH;
use scan_process::FrameRect;
use scan_process::adjust::{self, Adjustments};

use crate::edit;
use crate::state::{AppState, Film};

/// Longest side of a frame's picture, in pixels.
const THUMB_SIDE: u32 = 192;
/// How tall the pictures are shown.
const THUMB_HEIGHT: f32 = 84.0;
/// How many pictures are made in one frame of the UI, so it stays responsive.
const PER_FRAME: usize = 2;

/// What a frame's picture shows.
#[derive(Clone, Debug, PartialEq)]
struct Key {
    generation: u64,
    image: u64,
    cut: FrameRect,
    film: Option<Film>,
    adjust: Adjustments,
    scan_turns: u8,
}

struct Thumb {
    key: Key,
    texture: TextureHandle,
}

#[derive(Default)]
pub struct Filmstrip {
    thumbs: Vec<Option<Thumb>>,
}

impl Filmstrip {
    /// The strip, selecting a frame clicked (through `editor`, which puts its tools down).
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState, editor: &mut edit::Editor) {
        let n = state.crop.frames.len();
        if n == 0 {
            ui.label(RichText::new("No frames yet: find them in the Crop step.").weak());
            return;
        }
        self.thumbs.resize_with(n, || None);
        let mut budget = PER_FRAME;
        let mut waiting = false;
        for i in 0..n {
            let Some(key) = key(state, i) else { continue };
            if self.thumbs[i].as_ref().is_some_and(|t| t.key == key) {
                continue;
            }
            if budget == 0 {
                waiting = true;
                continue;
            }
            budget -= 1;
            if let Some(image) = render(state, &key) {
                let texture = match self.thumbs[i].take() {
                    Some(mut t) => {
                        t.texture.set(image, TextureOptions::LINEAR);
                        t.texture
                    }
                    None => {
                        ui.ctx()
                            .load_texture(format!("frame {i}"), image, TextureOptions::LINEAR)
                    }
                };
                self.thumbs[i] = Some(Thumb { key, texture });
            }
        }
        if waiting {
            ui.ctx().request_repaint();
        }

        let selected = edit::target(state);
        let mut select = None;
        egui::ScrollArea::horizontal()
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for i in 0..n {
                        ui.vertical(|ui| {
                            let frame = &mut state.crop.frames[i];
                            let tint = if frame.skip {
                                Color32::from_gray(70)
                            } else {
                                Color32::WHITE
                            };
                            let clicked = match &self.thumbs[i] {
                                Some(t) => {
                                    let size = t.texture.size_vec2();
                                    let size = size * (THUMB_HEIGHT / size.y.max(1.0));
                                    let image = egui::Image::new((t.texture.id(), size)).tint(tint);
                                    ui.add(egui::Button::image(image).selected(selected == Some(i)))
                                        .clicked()
                                }
                                None => ui
                                    .add_sized(
                                        Vec2::new(THUMB_HEIGHT * 1.5, THUMB_HEIGHT),
                                        egui::Button::selectable(selected == Some(i), "..."),
                                    )
                                    .clicked(),
                            };
                            if clicked {
                                select = Some(i);
                            }
                            ui.horizontal(|ui| {
                                let mut export = !frame.skip;
                                if ui
                                    .checkbox(&mut export, format!("{}", i + 1))
                                    .on_hover_text("Exported; off leaves it out (X)")
                                    .changed()
                                {
                                    frame.skip = !export;
                                }
                                if !frame.adjust.is_identity() {
                                    ui.label(RichText::new("edited").weak().small());
                                }
                            });
                        });
                    }
                });
                // Room for the scroll bar, which floats over the bottom when hovered.
                ui.add_space(12.0);
            });
        if let Some(i) = select {
            editor.select(state, Some(i));
        }
    }
}

/// What frame `i`'s picture shows now.
fn key(state: &AppState, i: usize) -> Option<Key> {
    state.result.as_ref()?;
    let frame = state.crop.frames.get(i)?;
    Some(Key {
        generation: state.result_generation,
        image: state.processed_id(),
        cut: frame.cut(),
        film: state.film(),
        adjust: frame.adjust.clone(),
        scan_turns: state.quarter_turns,
    })
}

/// Frame `key`'s picture: cut small, healed, developed with its edits and turned.
fn render(state: &AppState, key: &Key) -> Option<egui::ColorImage> {
    let (result, image) = (state.result.as_ref()?, state.processed_image()?);
    let (small, scale) =
        scan_process::preview_frame(image, result.dpi, result.area, Some(&key.cut), THUMB_SIDE);
    let px_per_mm = result.dpi as f32 / MM_PER_INCH * scale;
    let spots = adjust::spot_pixels(&key.adjust.spots, Some(&key.cut), &result.area, px_per_mm);
    let small = adjust::heal(&small, &spots);
    let inverter = key.film.map(|f| f.inverter(&key.adjust));
    let developed = adjust::develop(&small, result.gamma, inverter.as_ref(), &key.adjust, scale);
    let turns = (key.scan_turns + key.adjust.quarter_turns) % 4;
    let developed = scan_core::image::DynamicImage::ImageRgb16(developed);
    let turned = adjust::orient(&developed, turns, key.adjust.mirror);
    let rgb = turned.to_rgb16();
    Some(edit::color_image(&rgb, false, false))
}
