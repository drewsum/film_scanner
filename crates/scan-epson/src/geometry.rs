//! Flatbed scan geometry: turning a requested area and resolution into the scanner's units,
//! as measured from Epson's driver (see `docs/epson-v500`).
//!
//! - The scanner reads at a hardware resolution of 2400 × m / d dpi; requests are scanned at
//!   the next hardware step up and downsampled.
//! - x positions are in pixels at max(hardware dpi, 533⅓) per inch.
//! - The carriage moves 2,400 steps per inch.
//! - The CCD's red, green and blue rows are 0.025 in apart, so a read needs that many extra
//!   lines above and below the area.

use scan_core::{MM_PER_INCH, ScanArea, ScanError};

/// Carriage steps per inch.
pub const STEPS_PER_INCH: f64 = 2400.0;
/// x units per inch at hardware resolutions up to 400 dpi.
const LOW_RES_UNITS_PER_INCH: f64 = 1600.0 / 3.0;
/// Width of the bed (8.5 in).
const BED_WIDTH_INCH: f64 = 8.5;
/// Distance between the CCD's color rows.
const ROW_PITCH_INCH: f64 = 0.025;

/// Hardware resolutions, as (dpi, multiplier, divisor) for `1e 41` bytes 9 and 10.
pub const HARDWARE_RESOLUTIONS: [(u32, u8, u8); 5] = [
    (200, 1, 12),
    (400, 1, 6),
    (800, 1, 3),
    (1600, 2, 3),
    (2400, 1, 1),
];

/// The hardware resolution a request is scanned at.
pub fn hardware_dpi(dpi: u32) -> Option<u32> {
    HARDWARE_RESOLUTIONS
        .iter()
        .map(|&(hw, ..)| hw)
        .find(|&hw| hw >= dpi)
}

/// x units per inch at a hardware resolution.
pub fn units_per_inch(hw_dpi: u32) -> f64 {
    f64::from(hw_dpi).max(LOW_RES_UNITS_PER_INCH)
}

/// Width of the bed in x units at 533⅓ per inch: the pixels of a calibration read that lie
/// under the glass, counted from the bed's edge.
pub fn bed_units() -> usize {
    (BED_WIDTH_INCH * LOW_RES_UNITS_PER_INCH).round() as usize
}

/// Lines between the CCD's blue and green (and green and red) rows.
pub fn skew_lines(hw_dpi: u32) -> u32 {
    (ROW_PITCH_INCH * f64::from(hw_dpi)).round() as u32
}

/// The motor profile of the carriage move before a read. Epson's driver uses a slow, short
/// profile near the top of the bed and a fast one (with a longer internal lead-in) elsewhere;
/// the motor table, `1e 01` fields and step count differ between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveProfile {
    Short,
    Long,
}

/// Measured per hardware resolution: (short-profile steps at y = 0, long-profile lead-in).
fn move_constants(hw_dpi: u32) -> Option<(f64, f64)> {
    match hw_dpi {
        200 => Some((194.0, 315.0)),
        400 => Some((444.0, 69.0)),
        _ => None,
    }
}

/// Carriage steps from home, and the profile, for a read of an area starting at `y_mm`.
pub fn carriage_move(y_mm: f32, hw_dpi: u32) -> Option<(u32, MoveProfile)> {
    let (short_base, long_lead) = move_constants(hw_dpi)?;
    let y = f64::from(y_mm) / f64::from(MM_PER_INCH) * STEPS_PER_INCH;
    Some(if y - long_lead > short_base {
        ((y - long_lead).round() as u32, MoveProfile::Long)
    } else {
        ((y + short_base).round() as u32, MoveProfile::Short)
    })
}

/// Motor steps for a read of `lines` at `hw_dpi`, including the lead-in.
pub fn scan_steps(hw_dpi: u32, lines: u32) -> u32 {
    // Measured: 120 steps at 200 dpi, 126 at 400.
    let lead_in = if hw_dpi <= 200 { 120 } else { 126 };
    (STEPS_PER_INCH as u32 / hw_dpi) * lines + lead_in
}

/// A flatbed read for one request.
#[derive(Clone, Debug, PartialEq)]
pub struct FlatbedPlan {
    pub dpi: u32,
    pub hw_dpi: u32,
    /// Output size at `dpi`.
    pub width: u32,
    pub height: u32,
    /// Lines to read at `hw_dpi`, including the color-skew margin.
    pub lines: u32,
    pub move_steps: u32,
    pub move_profile: MoveProfile,
    pub scan_steps: u32,
    /// x of the area's left edge in the scanner's units, given the session's x origin.
    pub x_units: u32,
}

impl FlatbedPlan {
    /// Plan a read of `area` at `dpi`. `x_origin` is the x unit of the bed's left edge for this
    /// session (Epson's driver used 140–143 at 533⅓ units per inch).
    pub fn new(area: &ScanArea, dpi: u32, x_origin: u32) -> Result<Self, ScanError> {
        let hw_dpi = hardware_dpi(dpi).ok_or_else(|| {
            ScanError::InvalidRequest(format!("{dpi} dpi is above the hardware maximum"))
        })?;
        let (width, height) = area.pixel_size(dpi);
        let scale = f64::from(hw_dpi) / f64::from(dpi);
        let lines = (f64::from(height) * scale).floor() as u32 + 2 * skew_lines(hw_dpi);
        let (move_steps, move_profile) = carriage_move(area.y_mm, hw_dpi).ok_or_else(|| {
            ScanError::Unsupported(format!(
                "carriage moves at {hw_dpi} dpi hardware resolution"
            ))
        })?;
        let inch = |mm: f32| f64::from(mm) / f64::from(MM_PER_INCH);
        let x_units = f64::from(x_origin) * units_per_inch(hw_dpi) / LOW_RES_UNITS_PER_INCH
            + inch(area.x_mm) * units_per_inch(hw_dpi);
        Ok(Self {
            dpi,
            hw_dpi,
            width,
            height,
            lines,
            move_steps,
            move_profile,
            scan_steps: scan_steps(hw_dpi, lines),
            x_units: x_units.round() as u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: f32, y: f32, w: f32, h: f32) -> ScanArea {
        ScanArea {
            x_mm: x,
            y_mm: y,
            width_mm: w,
            height_mm: h,
        }
    }

    #[test]
    fn hardware_steps() {
        assert_eq!(hardware_dpi(50), Some(200));
        assert_eq!(hardware_dpi(240), Some(400));
        assert_eq!(hardware_dpi(1200), Some(1600));
        assert_eq!(hardware_dpi(4800), None);
        assert_eq!(skew_lines(200), 5);
        assert_eq!(skew_lines(1600), 40);
    }

    /// Values Epson's driver sent in captures 02, 26 and 27.
    #[test]
    fn matches_driver() {
        let p = FlatbedPlan::new(&area(0.0, 0.0, 50.0, 50.0), 75, 143).unwrap();
        assert_eq!((p.hw_dpi, p.lines, p.scan_steps), (200, 404, 4968));
        assert_eq!((p.move_steps, p.x_units), (194, 143));

        let p = FlatbedPlan::new(&area(40.0, 60.0, 80.0, 50.0), 75, 140).unwrap();
        assert_eq!((p.width, p.height, p.lines), (236, 148, 404));
        assert_eq!(p.x_units, 980); // driver: 981

        let p = FlatbedPlan::new(&area(40.0, 60.0, 30.0, 20.0), 300, 140).unwrap();
        assert_eq!((p.hw_dpi, p.width, p.height, p.lines), (400, 354, 236, 334));
        assert_eq!(p.scan_steps, 2130);
    }

    #[test]
    fn carriage_moves() {
        use MoveProfile::*;
        assert_eq!(carriage_move(0.0, 200), Some((194, Short)));
        assert_eq!(carriage_move(0.0, 400), Some((444, Short)));
        // Driver, 200 dpi: 2,526, 5,346 and 13,854 for y = 30, 60 and 150 mm.
        for (y, steps) in [(30.0, 2526), (60.0, 5346), (150.0, 13854)] {
            let (s, p) = carriage_move(y, 200).unwrap();
            assert_eq!(p, Long);
            assert!(s.abs_diff(steps) <= 10, "{y} mm: {s} vs {steps}");
        }
        // 400 dpi: 2,764 and 5,602 for y = 30 and 60 mm.
        for (y, steps) in [(30.0, 2764), (60.0, 5602)] {
            assert!(carriage_move(y, 400).unwrap().0.abs_diff(steps) <= 3);
        }
        assert_eq!(carriage_move(10.0, 800), None);
    }
}
