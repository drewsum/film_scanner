//! Zoomable, pannable display of a scan result.

use eframe::egui::epaint::{Mesh, Vertex};
use eframe::egui::{
    self, Color32, ColorImage, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2,
};
use scan_core::ScanResult;
use scan_core::image::{DynamicImage, GrayImage};
use scan_film::Inverter;
use scan_process::DustRemoval;

use crate::state::ViewChannel;

/// Longest side of the texture uploaded for display; full-resolution data stays in the result.
pub const MAX_TEXTURE_SIDE: u32 = 4096;
/// Repaired defects, when the mask is shown.
const MASK_COLOR: Color32 = Color32::from_rgb(255, 40, 40);

/// The image to display: a channel of `result`, or `visible` (the processed image, at most
/// [`MAX_TEXTURE_SIDE`] on a side: see [`thumbnail`]) as it is or turned positive by
/// `positive`, with the pixels `cleaned` repaired marked when `show_mask` is set.
pub fn render(
    result: &ScanResult,
    channel: ViewChannel,
    visible: &DynamicImage,
    cleaned: Option<&DustRemoval>,
    show_mask: bool,
    positive: Option<&Inverter>,
) -> ColorImage {
    match (channel, &result.infrared) {
        (ViewChannel::Infrared, Some(ir)) => {
            to_color_image(&DynamicImage::ImageLuma16(ir.clone()), MAX_TEXTURE_SIDE)
        }
        _ => {
            let mut color = match positive {
                Some(inverter) => {
                    let image = scan_process::invert(visible, result.gamma, inverter);
                    to_color_image(&DynamicImage::ImageRgb16(image), MAX_TEXTURE_SIDE)
                }
                None => to_color_image(visible, MAX_TEXTURE_SIDE),
            };
            if let Some(c) = cleaned.filter(|_| show_mask) {
                overlay_mask(&mut color, &c.mask);
            }
            color
        }
    }
}

/// `image` scaled down (if needed) to fit a texture, keeping its bit depth: what the view
/// works from, so a change of inversion doesn't go through the whole scan.
pub fn thumbnail(image: &DynamicImage) -> DynamicImage {
    if image.width() > MAX_TEXTURE_SIDE || image.height() > MAX_TEXTURE_SIDE {
        image.thumbnail(MAX_TEXTURE_SIDE, MAX_TEXTURE_SIDE)
    } else {
        image.clone()
    }
}

/// Mark every texel of `image` that covers a set pixel of `mask`, which may be larger.
pub fn overlay_mask(image: &mut ColorImage, mask: &GrayImage) {
    let [w, h] = image.size;
    let (mw, mh) = (mask.width() as usize, mask.height() as usize);
    for (i, _) in mask.as_raw().iter().enumerate().filter(|&(_, &m)| m != 0) {
        let (x, y) = (i % mw, i / mw);
        image.pixels[(y * h / mh) * w + x * w / mw] = MASK_COLOR;
    }
}

/// Downscale (if needed) and convert any 8/16-bit image to an 8-bit RGBA texture image.
pub fn to_color_image(img: &DynamicImage, max_side: u32) -> ColorImage {
    let scaled;
    let img = if img.width() > max_side || img.height() > max_side {
        scaled = img.thumbnail(max_side, max_side);
        &scaled
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

/// Where the image is on screen, turned: maps between the screen and the unturned image's
/// own coordinates, as fractions (0–1) of its width and height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// The turned image's rectangle on screen.
    pub rect: Rect,
    /// Clockwise.
    pub quarter_turns: u8,
}

impl Placement {
    pub fn to_screen(self, p: Pos2) -> Pos2 {
        let r = match self.quarter_turns % 4 {
            0 => p,
            1 => Pos2::new(1.0 - p.y, p.x),
            2 => Pos2::new(1.0 - p.x, 1.0 - p.y),
            _ => Pos2::new(p.y, 1.0 - p.x),
        };
        self.rect.min + r.to_vec2() * self.rect.size()
    }

    pub fn to_image(self, s: Pos2) -> Pos2 {
        let r = ((s - self.rect.min) / self.rect.size()).to_pos2();
        match self.quarter_turns % 4 {
            0 => r,
            1 => Pos2::new(r.y, 1.0 - r.x),
            2 => Pos2::new(1.0 - r.x, 1.0 - r.y),
            _ => Pos2::new(1.0 - r.y, r.x),
        }
    }
}

#[derive(Default)]
pub struct ImageView {
    texture: Option<TextureHandle>,
    /// The result generation and variant on the texture.
    shown: Option<(u64, u64)>,
    /// The quarter turns last shown.
    turns: u8,
    /// Zoom relative to fit-to-window.
    zoom: f32,
    /// Pan offset of the image centre from the view centre, in points.
    offset: Vec2,
}

impl ImageView {
    pub fn reset(&mut self) {
        self.zoom = 1.0;
        self.offset = Vec2::ZERO;
    }

    fn update_texture(
        &mut self,
        ctx: &egui::Context,
        generation: u64,
        variant: u64,
        image: impl FnOnce() -> ColorImage,
    ) {
        if self.shown == Some((generation, variant)) {
            return;
        }
        let image = image();
        match &mut self.texture {
            Some(tex) => tex.set(image, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("scan", image, TextureOptions::LINEAR)),
        }
        if self.shown.map(|(g, _)| g) != Some(generation) {
            self.reset();
        }
        self.shown = Some((generation, variant));
    }

    /// Show the image of result `generation` turned `quarter_turns` clockwise, calling
    /// `image` for it only when `generation` or `variant` (which view of the result it is)
    /// changed. A new generation or turn also resets the zoom.
    ///
    /// `overlay` then draws over the image and handles the pointer; when it returns true it
    /// took the pointer, and a drag doesn't pan.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        generation: u64,
        variant: u64,
        quarter_turns: u8,
        image: impl FnOnce() -> ColorImage,
        overlay: impl FnOnce(&egui::Ui, &egui::Response, &Placement) -> bool,
    ) {
        self.update_texture(ui.ctx(), generation, variant, image);
        let Some((tex_id, tex_size)) = self.texture.as_ref().map(|t| (t.id(), t.size_vec2()))
        else {
            return;
        };
        if self.zoom <= 0.0 || self.turns != quarter_turns {
            self.reset();
            self.turns = quarter_turns;
        }
        let tex_size = if quarter_turns % 2 == 1 {
            Vec2::new(tex_size.y, tex_size.x)
        } else {
            tex_size
        };

        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let fit = (rect.width() / tex_size.x).min(rect.height() / tex_size.y);

        if response.double_clicked() {
            self.reset();
        }
        if let Some(pointer) = response.hover_pos() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = pinch * (scroll * 0.002).exp();
            if factor != 1.0 {
                let new_zoom = (self.zoom * factor).clamp(0.1, 64.0);
                // Keep the point under the cursor fixed while zooming.
                let anchor = pointer - rect.center() - self.offset;
                self.offset -= anchor * (new_zoom / self.zoom - 1.0);
                self.zoom = new_zoom;
            }
        }

        let size = tex_size * fit * self.zoom;
        let placement = Placement {
            rect: Rect::from_center_size(rect.center() + self.offset, size),
            quarter_turns,
        };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        // Each corner of the turned image shows the texture corner that turned there.
        let mut mesh = Mesh::with_texture(tex_id);
        let r = placement.rect;
        for corner in [
            r.left_top(),
            r.right_top(),
            r.right_bottom(),
            r.left_bottom(),
        ] {
            mesh.vertices.push(Vertex {
                pos: corner,
                uv: placement.to_image(corner),
                color: Color32::WHITE,
            });
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(mesh);

        let took_pointer = overlay(ui, &response, &placement);
        if response.dragged() && !took_pointer {
            self.offset += response.drag_delta();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan_core::image::{ImageBuffer, Rgb};

    #[test]
    fn converts_16_bit_and_downscales() {
        let img: ImageBuffer<Rgb<u16>, Vec<u16>> =
            ImageBuffer::from_fn(400, 100, |x, _| Rgb([u16::MAX, (x * 100) as u16, 0]));
        let color = to_color_image(&DynamicImage::ImageRgb16(img), 200);
        assert_eq!(color.size, [200, 50]);
        assert_eq!(color.pixels[0], Color32::from_rgb(255, 0, 0));
    }

    #[test]
    fn mask_overlay_marks_downscaled_defects() {
        let mut color = ColorImage::filled([4, 2], Color32::BLACK);
        let mut mask = GrayImage::new(40, 20);
        mask.put_pixel(39, 0, scan_core::image::Luma([255]));
        mask.put_pixel(12, 15, scan_core::image::Luma([255]));
        overlay_mask(&mut color, &mask);
        let marked: Vec<_> = (0..8).filter(|&i| color.pixels[i] == MASK_COLOR).collect();
        assert_eq!(marked, [3, 5]);
    }

    #[test]
    fn placements_turn_both_ways() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(200.0, 100.0));
        for quarter_turns in 0..4 {
            let placement = Placement {
                rect,
                quarter_turns,
            };
            let p = Pos2::new(0.25, 0.75);
            let back = placement.to_image(placement.to_screen(p));
            assert!((back - p).length() < 1e-5, "{quarter_turns}: {back:?}");
        }
        // A quarter clockwise, the image's top-left corner is at the top right.
        let placement = Placement {
            rect,
            quarter_turns: 1,
        };
        assert_eq!(placement.to_screen(Pos2::ZERO), rect.right_top());
        assert_eq!(placement.to_screen(Pos2::new(0.0, 1.0)), rect.left_top());
    }

    #[test]
    fn small_images_keep_their_size() {
        let img = DynamicImage::ImageLuma8(ImageBuffer::from_pixel(
            3,
            2,
            scan_core::image::Luma([128u8]),
        ));
        let color = to_color_image(&img, MAX_TEXTURE_SIDE);
        assert_eq!(color.size, [3, 2]);
        assert_eq!(color.pixels[5], Color32::from_gray(128));
    }
}
