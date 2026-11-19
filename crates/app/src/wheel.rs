//! Sliders the mouse wheel moves, a step a notch, for fine changes without dragging.

use std::ops::RangeInclusive;

use eframe::egui::{self, Event, MouseWheelUnit, Response, Ui, Vec2, Widget};

/// Points of trackpad scrolling that count as one notch of a mouse wheel.
const POINTS_PER_NOTCH: f32 = 40.0;

/// An [`egui::Slider`] of `value` in steps of `step`, which turning the mouse wheel over it
/// moves a step a notch (up for more) instead of scrolling the panel.
pub struct WheelSlider<'a> {
    value: &'a mut f32,
    range: RangeInclusive<f32>,
    step: f32,
    suffix: &'static str,
    text: &'static str,
    logarithmic: bool,
    decimals: Option<usize>,
}

impl<'a> WheelSlider<'a> {
    pub fn new(value: &'a mut f32, range: RangeInclusive<f32>, step: f32) -> Self {
        Self {
            value,
            range,
            step,
            suffix: "",
            text: "",
            logarithmic: false,
            decimals: None,
        }
    }

    pub fn suffix(self, suffix: &'static str) -> Self {
        Self { suffix, ..self }
    }

    pub fn text(self, text: &'static str) -> Self {
        Self { text, ..self }
    }

    /// Dragged on a logarithmic scale; the wheel still moves it in steps.
    pub fn logarithmic(self) -> Self {
        Self {
            logarithmic: true,
            ..self
        }
    }

    pub fn fixed_decimals(self, decimals: usize) -> Self {
        Self {
            decimals: Some(decimals),
            ..self
        }
    }
}

impl Widget for WheelSlider<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let mut slider = egui::Slider::new(&mut *self.value, self.range.clone())
            .suffix(self.suffix)
            .text(self.text)
            .logarithmic(self.logarithmic);
        // A logarithmic slider dragged in steps would stick at the small end.
        if !self.logarithmic {
            slider = slider.step_by(f64::from(self.step));
        }
        if let Some(decimals) = self.decimals {
            slider = slider.fixed_decimals(decimals);
        }
        let mut response = ui.add(slider);
        let notches = wheel_notches(ui, &response);
        if notches != 0 {
            let moved = nudged(*self.value, notches, self.step, &self.range);
            if moved != *self.value {
                *self.value = moved;
                response.mark_changed();
            }
        }
        response
    }
}

/// Whole notches the mouse wheel turned over `response` this frame, up positive. While the
/// pointer is over it, the panel isn't scrolled. Trackpad scrolling adds up across frames.
fn wheel_notches(ui: &Ui, response: &Response) -> i32 {
    if !response.hovered() || !response.enabled() {
        return 0;
    }
    let turned: f32 = ui.input_mut(|i| {
        i.smooth_scroll_delta = Vec2::ZERO;
        i.events
            .iter()
            .map(|e| match e {
                // Ctrl and the wheel zooms.
                Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                    ..
                } if !modifiers.command => {
                    // Shift turns the wheel's scrolling sideways.
                    let along = delta.x + delta.y;
                    match unit {
                        MouseWheelUnit::Point => along / POINTS_PER_NOTCH,
                        MouseWheelUnit::Line | MouseWheelUnit::Page => along,
                    }
                }
                _ => 0.0,
            })
            .sum()
    });
    let id = response.id.with("wheel");
    ui.data_mut(|d| {
        let total = d.get_temp_mut_or(id, 0.0f32);
        *total += turned;
        let whole = total.trunc();
        *total -= whole;
        whole as i32
    })
}

/// `value` moved `notches` steps of `step`, onto a whole step, within `range`.
fn nudged(value: f32, notches: i32, step: f32, range: &RangeInclusive<f32>) -> f32 {
    let steps = (value / step).round() + notches as f32;
    (steps * step).clamp(*range.start(), *range.end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notch_moves_a_step_and_stays_in_range() {
        let range = -3.0..=3.0;
        assert!((nudged(0.0, 1, 0.05, &range) - 0.05).abs() < 1e-6);
        assert!((nudged(0.5, -3, 0.05, &range) - 0.35).abs() < 1e-6);
        // Off a step (typed in), it lands on one.
        assert!((nudged(0.52, 1, 0.05, &range) - 0.55).abs() < 1e-6);
        assert_eq!(nudged(2.98, 2, 0.05, &range), 3.0);
        assert_eq!(nudged(-3.0, -1, 0.05, &range), -3.0);
    }
}
