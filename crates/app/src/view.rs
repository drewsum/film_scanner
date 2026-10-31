//! Zoomable, pannable display of a scan result.

use eframe::egui::{
    self, Color32, ColorImage, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2,
};
use scan_core::ScanResult;
use scan_core::image::DynamicImage;

use crate::state::ViewChannel;

/// Longest side of the texture uploaded for display; full-resolution data stays in the result.
pub const MAX_TEXTURE_SIDE: u32 = 4096;

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

#[derive(Default)]
pub struct ImageView {
    texture: Option<TextureHandle>,
    shown: Option<(u64, ViewChannel)>,
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
        result: &ScanResult,
        generation: u64,
        channel: ViewChannel,
    ) {
        if self.shown == Some((generation, channel)) {
            return;
        }
        let ir;
        let img = match (channel, &result.infrared) {
            (ViewChannel::Infrared, Some(gray)) => {
                ir = DynamicImage::ImageLuma16(gray.clone());
                &ir
            }
            _ => result.primary(),
        };
        let image = to_color_image(img, MAX_TEXTURE_SIDE);
        match &mut self.texture {
            Some(tex) => tex.set(image, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("scan", image, TextureOptions::LINEAR)),
        }
        if self.shown.map(|(g, _)| g) != Some(generation) {
            self.reset();
        }
        self.shown = Some((generation, channel));
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        result: &ScanResult,
        generation: u64,
        channel: ViewChannel,
    ) {
        self.update_texture(ui.ctx(), result, generation, channel);
        let Some((tex_id, tex_size)) = self.texture.as_ref().map(|t| (t.id(), t.size_vec2()))
        else {
            return;
        };
        if self.zoom <= 0.0 {
            self.reset();
        }

        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let fit = (rect.width() / tex_size.x).min(rect.height() / tex_size.y);

        if response.double_clicked() {
            self.reset();
        }
        if response.dragged() {
            self.offset += response.drag_delta();
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
        let image_rect = Rect::from_center_size(rect.center() + self.offset, size);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        painter.image(tex_id, image_rect, uv, Color32::WHITE);
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
