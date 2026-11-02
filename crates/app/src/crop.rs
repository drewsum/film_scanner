//! The frames drawn over the scan, and editing them with the pointer: drag a frame to move
//! it, its corner and edge handles to resize it, and the handle above it to turn it.

use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Key, Pos2, Shape, Stroke};
use scan_core::ScanArea;
use scan_process::FrameRect;

use crate::state::CropFrame;
use crate::view::Placement;

/// How near a handle the pointer must be to take it, in points.
const GRAB: f32 = 8.0;
/// How far beyond a frame's edge its turning handle is, in points.
const TURN_HANDLE: f32 = 24.0;
/// Frames are no smaller than this, in millimetres.
const MIN_SIZE_MM: f32 = 2.0;

const FRAME_COLOR: Color32 = Color32::from_rgb(80, 220, 120);
const CHECK_COLOR: Color32 = Color32::from_rgb(255, 190, 40);
const SELECTED_COLOR: Color32 = Color32::from_rgb(90, 170, 255);

/// Where on a frame the pointer holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grip {
    Move,
    /// The edges that move along the frame's own x and y axes: -1 the left or top one, 1 the
    /// right or bottom one, 0 neither.
    Resize(i8, i8),
    Turn,
}

/// Maps between millimetres on the scanner bed and the screen.
#[derive(Clone, Copy, Debug)]
pub struct BedView {
    /// The area the image shows.
    pub area: ScanArea,
    pub placement: Placement,
}

impl BedView {
    pub fn to_screen(self, (x, y): (f32, f32)) -> Pos2 {
        let a = self.area;
        self.placement.to_screen(Pos2::new(
            (x - a.x_mm) / a.width_mm,
            (y - a.y_mm) / a.height_mm,
        ))
    }

    pub fn to_bed(self, p: Pos2) -> (f32, f32) {
        let a = self.area;
        let s = self.placement.to_image(p);
        (a.x_mm + s.x * a.width_mm, a.y_mm + s.y * a.height_mm)
    }
}

/// `start` dragged by `grip` from the point `from` to `to`, in millimetres.
pub fn dragged(start: &FrameRect, grip: Grip, from: (f32, f32), to: (f32, f32)) -> FrameRect {
    match grip {
        Grip::Move => FrameRect {
            cx_mm: start.cx_mm + to.0 - from.0,
            cy_mm: start.cy_mm + to.1 - from.1,
            ..*start
        },
        Grip::Resize(du, dv) => {
            let (u, v) = start.to_local(to.0, to.1);
            let (mut l, mut r) = (-start.width_mm / 2.0, start.width_mm / 2.0);
            let (mut t, mut b) = (-start.height_mm / 2.0, start.height_mm / 2.0);
            match du {
                -1 => l = u.min(r - MIN_SIZE_MM),
                1 => r = u.max(l + MIN_SIZE_MM),
                _ => {}
            }
            match dv {
                -1 => t = v.min(b - MIN_SIZE_MM),
                1 => b = v.max(t + MIN_SIZE_MM),
                _ => {}
            }
            let (cx_mm, cy_mm) = start.from_local((l + r) / 2.0, (t + b) / 2.0);
            FrameRect {
                cx_mm,
                cy_mm,
                width_mm: r - l,
                height_mm: b - t,
                angle_deg: start.angle_deg,
            }
        }
        Grip::Turn => {
            let angle = |(x, y): (f32, f32)| (y - start.cy_mm).atan2(x - start.cx_mm);
            let turned = start.angle_deg + (angle(to) - angle(from)).to_degrees();
            FrameRect {
                angle_deg: (turned + 180.0).rem_euclid(360.0) - 180.0,
                ..*start
            }
        }
    }
}

/// A frame's handles on screen: its corners and edge middles, and the turning handle beyond
/// the edge that is highest on screen, with the point on that edge it hangs from.
fn handles(frame: &FrameRect, view: &BedView) -> (Vec<(Grip, Pos2)>, (Pos2, Pos2)) {
    let (w, h) = (frame.width_mm / 2.0, frame.height_mm / 2.0);
    let mut out = Vec::with_capacity(9);
    for dv in -1i8..=1 {
        for du in -1i8..=1 {
            if (du, dv) != (0, 0) {
                let p = frame.from_local(f32::from(du) * w, f32::from(dv) * h);
                out.push((Grip::Resize(du, dv), view.to_screen(p)));
            }
        }
    }
    let centre = view.to_screen((frame.cx_mm, frame.cy_mm));
    let top = out
        .iter()
        .filter(|(g, _)| matches!(g, Grip::Resize(0, _) | Grip::Resize(_, 0)))
        .map(|&(_, p)| p)
        .min_by(|a, b| a.y.total_cmp(&b.y))
        .unwrap_or(centre);
    let turn = top + (top - centre).normalized() * TURN_HANDLE;
    out.push((Grip::Turn, turn));
    (out, (top, turn))
}

/// What the pointer did to the frames this frame.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// The pointer is busy with the frames, so it doesn't pan the view.
    pub took_pointer: bool,
    /// A new selection (`Some(None)` to select nothing).
    pub select: Option<Option<usize>>,
    /// A frame's new rectangle.
    pub edit: Option<(usize, FrameRect)>,
    /// Delete the selected frame.
    pub delete: bool,
}

/// An edit in progress.
#[derive(Clone, Copy, Debug)]
struct Drag {
    frame: usize,
    grip: Grip,
    start: FrameRect,
    from: (f32, f32),
}

#[derive(Default)]
pub struct CropEditor {
    drag: Option<Drag>,
}

impl CropEditor {
    /// What the pointer at `p` would take: the selected frame's handles first, then the
    /// selected frame, then the frame on top.
    fn hit(
        &self,
        p: Pos2,
        view: &BedView,
        frames: &[CropFrame],
        selected: Option<usize>,
    ) -> Option<(usize, Grip)> {
        if let Some(i) = selected.filter(|&i| i < frames.len()) {
            let (handles, _) = handles(&frames[i].rect, view);
            let near = handles
                .into_iter()
                .map(|(g, h)| (g, h.distance(p)))
                .filter(|&(_, d)| d <= GRAB)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((grip, _)) = near {
                return Some((i, grip));
            }
        }
        let (x, y) = view.to_bed(p);
        let inside = |i: &usize| frames[*i].rect.contains(x, y);
        selected
            .filter(|i| *i < frames.len())
            .filter(inside)
            .or_else(|| (0..frames.len()).rev().find(inside))
            .map(|i| (i, Grip::Move))
    }

    /// Draw `frames` over the image and handle the pointer and the Delete key.
    pub fn show(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        view: &BedView,
        frames: &[CropFrame],
        selected: Option<usize>,
    ) -> Outcome {
        let mut out = Outcome::default();
        if self.drag.is_some_and(|d| d.frame >= frames.len()) {
            self.drag = None;
        }

        // Taking hold where the button went down, not where the drag was recognised.
        if response.drag_started()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
            && let Some((frame, grip)) = self.hit(origin, view, frames, selected)
        {
            self.drag = Some(Drag {
                frame,
                grip,
                start: frames[frame].rect,
                from: view.to_bed(origin),
            });
            out.select = Some(Some(frame));
        }
        if let Some(drag) = self.drag {
            out.took_pointer = true;
            if let Some(p) = response.interact_pointer_pos() {
                let rect = dragged(&drag.start, drag.grip, drag.from, view.to_bed(p));
                out.edit = Some((drag.frame, rect));
            }
            if response.drag_stopped() || !response.dragged() {
                self.drag = None;
            }
        } else if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            out.select = Some(self.hit(p, view, frames, selected).map(|(i, _)| i));
        }

        if let Some(p) = response.hover_pos() {
            let held = self.drag.map(|d| (d.frame, d.grip));
            if let Some((i, grip)) = held.or_else(|| self.hit(p, view, frames, selected)) {
                ui.ctx()
                    .set_cursor_icon(cursor(grip, &frames[i].rect, view));
            }
        }
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        if selected.is_some() && !typing && ui.input(|i| i.key_pressed(Key::Delete)) {
            out.delete = true;
        }

        self.paint(ui, response.rect, view, frames, selected, out.edit);
        out
    }

    fn paint(
        &self,
        ui: &egui::Ui,
        clip: egui::Rect,
        view: &BedView,
        frames: &[CropFrame],
        selected: Option<usize>,
        edit: Option<(usize, FrameRect)>,
    ) {
        let painter = ui.painter_at(clip);
        for (i, frame) in frames.iter().enumerate() {
            let rect = match edit {
                Some((e, r)) if e == i => r,
                _ => frame.rect,
            };
            let is_selected = selected == Some(i);
            let color = if is_selected {
                SELECTED_COLOR
            } else if frame.check {
                CHECK_COLOR
            } else {
                FRAME_COLOR
            };
            let corners: Vec<Pos2> = rect.corners().iter().map(|&c| view.to_screen(c)).collect();
            let width = if is_selected { 2.5 } else { 1.5 };
            painter.add(Shape::closed_line(corners, Stroke::new(width, color)));

            let centre = view.to_screen((rect.cx_mm, rect.cy_mm));
            let label = if frame.check {
                format!("{}?", i + 1)
            } else {
                format!("{}", i + 1)
            };
            painter.circle_filled(centre, 12.0, Color32::from_black_alpha(160));
            painter.text(
                centre,
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(14.0),
                color,
            );

            if is_selected {
                let (handles, (edge, turn)) = handles(&rect, view);
                painter.line_segment([edge, turn], Stroke::new(1.5, color));
                for (grip, p) in handles {
                    if grip == Grip::Turn {
                        painter.circle(p, 5.0, Color32::WHITE, Stroke::new(1.5, color));
                    } else {
                        let square = egui::Rect::from_center_size(p, egui::vec2(8.0, 8.0));
                        painter.rect_filled(square, 1.0, Color32::WHITE);
                        painter.rect_stroke(
                            square,
                            1.0,
                            Stroke::new(1.5, color),
                            egui::StrokeKind::Middle,
                        );
                    }
                }
            }
        }
    }
}

/// The pointer's look over `grip` of `frame`: a resize arrow along the handle's direction
/// on screen.
fn cursor(grip: Grip, frame: &FrameRect, view: &BedView) -> CursorIcon {
    let Grip::Resize(du, dv) = grip else {
        return if grip == Grip::Move {
            CursorIcon::Move
        } else {
            CursorIcon::Crosshair
        };
    };
    let p = frame.from_local(
        f32::from(du) * frame.width_mm / 2.0,
        f32::from(dv) * frame.height_mm / 2.0,
    );
    let d = view.to_screen(p) - view.to_screen((frame.cx_mm, frame.cy_mm));
    let angle = d.y.atan2(d.x).to_degrees().rem_euclid(180.0);
    match angle {
        a if !(22.5..157.5).contains(&a) => CursorIcon::ResizeHorizontal,
        a if a < 67.5 => CursorIcon::ResizeNwSe,
        a if a < 112.5 => CursorIcon::ResizeVertical,
        _ => CursorIcon::ResizeNeSw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> FrameRect {
        FrameRect {
            cx_mm: 20.0,
            cy_mm: 40.0,
            width_mm: 24.0,
            height_mm: 36.0,
            angle_deg: 0.0,
        }
    }

    fn close(a: &FrameRect, b: &FrameRect) -> bool {
        [
            a.cx_mm - b.cx_mm,
            a.cy_mm - b.cy_mm,
            a.width_mm - b.width_mm,
            a.height_mm - b.height_mm,
            a.angle_deg - b.angle_deg,
        ]
        .iter()
        .all(|d| d.abs() < 1e-3)
    }

    #[test]
    fn moving_resizing_and_turning() {
        let f = frame();
        let moved = dragged(&f, Grip::Move, (0.0, 0.0), (1.5, -2.0));
        assert!(close(
            &moved,
            &FrameRect {
                cx_mm: 21.5,
                cy_mm: 38.0,
                ..f
            }
        ));

        // The right edge moves out 2 mm; the left edge stays.
        let wider = dragged(&f, Grip::Resize(1, 0), (32.0, 40.0), (34.0, 45.0));
        let expected = FrameRect {
            cx_mm: 21.0,
            width_mm: 26.0,
            ..f
        };
        assert!(close(&wider, &expected), "{wider:?}");
        // The top-left corner can't pass the opposite corner.
        let tiny = dragged(&f, Grip::Resize(-1, -1), (8.0, 22.0), (100.0, 100.0));
        assert!((tiny.width_mm - MIN_SIZE_MM).abs() < 1e-4 && tiny.height_mm >= MIN_SIZE_MM);

        // A quarter turn about the centre.
        let turned = dragged(&f, Grip::Turn, (20.0, 20.0), (40.0, 40.0));
        assert!((turned.angle_deg - 90.0).abs() < 1e-3, "{turned:?}");
        let back = dragged(&turned, Grip::Turn, (40.0, 40.0), (20.0, 20.0));
        assert!(back.angle_deg.abs() < 1e-3);
    }

    #[test]
    fn resizing_a_turned_frame_follows_its_axes() {
        let f = FrameRect {
            angle_deg: 90.0,
            ..frame()
        };
        // Its x axis points down the bed: dragging its right edge 3 mm further down.
        let (x, y) = f.from_local(12.0, 0.0);
        let r = dragged(&f, Grip::Resize(1, 0), (x, y), (x + 5.0, y + 3.0));
        assert!((r.width_mm - 27.0).abs() < 1e-3, "{r:?}");
        assert!((r.cy_mm - 41.5).abs() < 1e-3 && (r.cx_mm - 20.0).abs() < 1e-3);
    }

    #[test]
    fn bed_view_round_trips_when_turned() {
        let view = BedView {
            area: ScanArea {
                x_mm: 10.0,
                ..ScanArea::full(60.0, 200.0)
            },
            placement: Placement {
                rect: egui::Rect::from_min_size(Pos2::new(5.0, 5.0), egui::vec2(400.0, 120.0)),
                quarter_turns: 3,
            },
        };
        let p = view.to_bed(view.to_screen((30.0, 150.0)));
        assert!(
            (p.0 - 30.0).abs() < 1e-3 && (p.1 - 150.0).abs() < 1e-3,
            "{p:?}"
        );
    }

    #[test]
    fn the_turning_handle_is_above_the_frame_on_screen() {
        let view = BedView {
            area: ScanArea::full(100.0, 100.0),
            placement: Placement {
                rect: egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 100.0)),
                quarter_turns: 1,
            },
        };
        let (handles, (edge, turn)) = handles(&frame(), &view);
        assert_eq!(handles.len(), 9);
        assert!(turn.y < edge.y);
        let centre = view.to_screen((20.0, 40.0));
        assert!(edge.y < centre.y);
    }
}
