//! Undo and redo of a frame's edits, and named presets of edits.

use scan_process::adjust::Adjustments;
use serde::{Deserialize, Serialize};

/// How many edits back each frame can be undone.
const LIMIT: usize = 200;

/// The edits a frame (or the whole scan) had before each change, to undo, and those undone,
/// to redo.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    undo: Vec<Adjustments>,
    redo: Vec<Adjustments>,
}

impl History {
    /// Remember `before`, the edits as they were before a change. Anything undone is
    /// forgotten.
    pub fn record(&mut self, before: Adjustments) {
        self.undo.push(before);
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// The edits before the last change, given the `current` ones (which redo brings back).
    pub fn undo(&mut self, current: Adjustments) -> Option<Adjustments> {
        let previous = self.undo.pop()?;
        self.redo.push(current);
        Some(previous)
    }

    /// The edits the last undo undid, given the `current` ones.
    pub fn redo(&mut self, current: Adjustments) -> Option<Adjustments> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// Edits saved under a name, to apply to other frames and rolls. They hold no geometry: a
/// frame keeps its own turn, mirroring, straightening and spots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub adjust: Adjustments,
}

/// `from` pasted onto `to`: everything but `to`'s own geometry (its turn, mirroring,
/// straightening and healed spots), which belongs to its frame.
pub fn paste(to: &mut Adjustments, from: Adjustments) {
    *to = Adjustments {
        quarter_turns: to.quarter_turns,
        mirror: to.mirror,
        straighten_deg: to.straighten_deg,
        spots: std::mem::take(&mut to.spots),
        ..from
    };
}

/// `adjust` without its frame's geometry, as a preset holds it.
pub fn without_geometry(adjust: &Adjustments) -> Adjustments {
    let mut plain = Adjustments::default();
    paste(&mut plain, adjust.clone());
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exposure(ev: f32) -> Adjustments {
        Adjustments {
            exposure_ev: ev,
            ..Adjustments::default()
        }
    }

    #[test]
    fn undoes_and_redoes_in_order() {
        let mut h = History::default();
        assert!(!h.can_undo() && h.undo(exposure(0.0)).is_none());
        h.record(exposure(0.0));
        h.record(exposure(1.0));
        // Now at 2.
        assert_eq!(h.undo(exposure(2.0)), Some(exposure(1.0)));
        assert_eq!(h.undo(exposure(1.0)), Some(exposure(0.0)));
        assert!(!h.can_undo() && h.can_redo());
        assert_eq!(h.redo(exposure(0.0)), Some(exposure(1.0)));
        // A new change forgets what was undone.
        h.record(exposure(1.0));
        assert!(!h.can_redo());
        assert_eq!(h.undo(exposure(5.0)), Some(exposure(1.0)));
    }

    #[test]
    fn history_is_limited() {
        let mut h = History::default();
        for i in 0..LIMIT + 10 {
            h.record(exposure(i as f32));
        }
        let mut n = 0;
        while h.undo(exposure(0.0)).is_some() {
            n += 1;
        }
        assert_eq!(n, LIMIT);
    }

    #[test]
    fn pasting_keeps_the_frames_geometry() {
        let spot = scan_process::adjust::Spot {
            x_mm: 1.0,
            y_mm: 2.0,
            radius_mm: 0.3,
        };
        let mut to = Adjustments {
            quarter_turns: 1,
            straighten_deg: 1.5,
            spots: vec![spot],
            ..Adjustments::default()
        };
        let from = Adjustments {
            contrast: 0.3,
            border_mm: 1.0,
            mirror: true,
            straighten_deg: -2.0,
            ..Adjustments::default()
        };
        paste(&mut to, from.clone());
        assert_eq!((to.contrast, to.border_mm), (0.3, 1.0));
        assert_eq!(
            (to.quarter_turns, to.mirror, to.straighten_deg),
            (1, false, 1.5)
        );
        assert_eq!(to.spots, [spot]);
        let plain = without_geometry(&from);
        assert_eq!(
            (plain.contrast, plain.mirror, plain.straighten_deg),
            (0.3, false, 0.0)
        );
    }
}
