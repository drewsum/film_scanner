//! Platform-independent conversions between WIA property values and `scan-core` types.

use scan_core::{BitDepth, MM_PER_INCH, ScanArea};

/// Resolutions offered to the user when a driver reports a continuous range.
pub const STANDARD_RESOLUTIONS: [u32; 18] = [
    50, 72, 75, 100, 150, 200, 240, 300, 400, 600, 800, 1200, 1600, 2400, 3200, 4800, 6400, 9600,
];

/// Valid values of a WIA property, as reported by `GetPropertyAttributes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidValues {
    Range {
        min: i32,
        nominal: i32,
        max: i32,
        step: i32,
    },
    List(Vec<i32>),
    /// Flag sets, or properties without constraints.
    Other,
}

impl ValidValues {
    /// Parse the raw element vector of a `WIA_PROP_RANGE` (`[min, nom, max, step]`) or
    /// `WIA_PROP_LIST` (`[count, nom, values...]`) attribute.
    pub fn from_raw(is_range: bool, is_list: bool, elems: &[i32]) -> Self {
        if is_range && elems.len() >= 4 {
            ValidValues::Range {
                min: elems[0],
                nominal: elems[1],
                max: elems[2],
                step: elems[3],
            }
        } else if is_list && elems.len() >= 2 {
            let count = (elems[0].max(0) as usize).min(elems.len() - 2);
            ValidValues::List(elems[2..2 + count].to_vec())
        } else {
            ValidValues::Other
        }
    }

    pub fn contains(&self, v: i32) -> bool {
        match self {
            ValidValues::Range { min, max, step, .. } => {
                (*min..=*max).contains(&v) && (*step <= 1 || (v - min) % step == 0)
            }
            ValidValues::List(values) => values.contains(&v),
            ValidValues::Other => true,
        }
    }
}

/// Resolutions to offer for a `WIA_IPS_XRES` constraint, ascending.
pub fn resolutions(valid: &ValidValues) -> Vec<u32> {
    let mut out: Vec<u32> = match valid {
        ValidValues::List(values) => values
            .iter()
            .filter(|&&v| v > 0)
            .map(|&v| v as u32)
            .collect(),
        ValidValues::Range { .. } => STANDARD_RESOLUTIONS
            .iter()
            .copied()
            .filter(|&r| valid.contains(r as i32))
            .collect(),
        ValidValues::Other => Vec::new(),
    };
    out.sort_unstable();
    out.dedup();
    out
}

/// Bit depths (per channel) available from a colour `WIA_IPA_DEPTH` constraint
/// (24 = 8-bit RGB, 48 = 16-bit RGB). Unconstrained properties yield nothing, since the
/// driver then only supports its current depth.
pub fn color_bit_depths(valid: &ValidValues) -> Vec<BitDepth> {
    let mut out = Vec::new();
    if *valid == ValidValues::Other {
        return out;
    }
    if valid.contains(24) {
        out.push(BitDepth::Eight);
    }
    if valid.contains(48) {
        out.push(BitDepth::Sixteen);
    }
    out
}

/// WIA bits-per-pixel for a colour or grey scan.
pub fn wia_depth(bit_depth: BitDepth, color: bool) -> i32 {
    let per_channel = bit_depth.bits() as i32;
    if color { per_channel * 3 } else { per_channel }
}

/// WIA 2.0 sizes are in thousandths of an inch.
pub fn thousandths_to_mm(v: i32) -> f32 {
    v as f32 / 1000.0 * MM_PER_INCH
}

/// Scan window in pixels (`x, y, width, height`) for `WIA_IPS_XPOS/YPOS/XEXTENT/YEXTENT`,
/// clamped to the bed size `max` (also in millimetres).
pub fn area_to_pixels(area: ScanArea, max: ScanArea, dpi: u32) -> (i32, i32, i32, i32) {
    let px = |mm: f32| (mm / MM_PER_INCH * dpi as f32).round() as i32;
    let (max_w, max_h) = (px(max.width_mm), px(max.height_mm));
    let x = px(area.x_mm).clamp(0, max_w - 1);
    let y = px(area.y_mm).clamp(0, max_h - 1);
    let w = px(area.width_mm).clamp(1, max_w - x);
    let h = px(area.height_mm).clamp(1, max_h - y);
    (x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_range_and_list() {
        assert_eq!(
            ValidValues::from_raw(true, false, &[50, 300, 6400, 1]),
            ValidValues::Range {
                min: 50,
                nominal: 300,
                max: 6400,
                step: 1
            }
        );
        assert_eq!(
            ValidValues::from_raw(false, true, &[3, 24, 8, 24, 48]),
            ValidValues::List(vec![8, 24, 48])
        );
        // A malformed count never reads past the end.
        assert_eq!(
            ValidValues::from_raw(false, true, &[9, 24, 24]),
            ValidValues::List(vec![24])
        );
        assert_eq!(
            ValidValues::from_raw(false, false, &[1, 2]),
            ValidValues::Other
        );
    }

    #[test]
    fn range_resolutions_respect_step() {
        let range = ValidValues::Range {
            min: 100,
            nominal: 300,
            max: 1200,
            step: 100,
        };
        assert_eq!(resolutions(&range), [100, 200, 300, 400, 600, 800, 1200]);
        let list = ValidValues::List(vec![600, 300, 0, 300]);
        assert_eq!(resolutions(&list), [300, 600]);
    }

    #[test]
    fn bit_depth_mapping() {
        let list = ValidValues::List(vec![1, 8, 24, 48]);
        assert_eq!(
            color_bit_depths(&list),
            [BitDepth::Eight, BitDepth::Sixteen]
        );
        assert_eq!(
            color_bit_depths(&ValidValues::List(vec![24])),
            [BitDepth::Eight]
        );
        assert!(color_bit_depths(&ValidValues::Other).is_empty());
        assert_eq!(wia_depth(BitDepth::Sixteen, true), 48);
        assert_eq!(wia_depth(BitDepth::Eight, false), 8);
    }

    #[test]
    fn units() {
        assert!((thousandths_to_mm(8500) - 215.9).abs() < 0.01);
        let max = ScanArea::full(25.4, 25.4);
        assert_eq!(area_to_pixels(max, max, 300), (0, 0, 300, 300));
        let off = ScanArea {
            x_mm: 12.7,
            y_mm: 0.0,
            width_mm: 25.4,
            height_mm: 1.0,
        };
        assert_eq!(area_to_pixels(off, max, 300), (150, 0, 150, 12));
    }
}
