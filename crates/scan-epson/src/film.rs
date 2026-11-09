//! Film scans through the transparency unit, generated from scratch: the session SilverFast
//! runs (see `docs/epson-v500`), with exposure, gain and offset searches of our own that land
//! where SilverFast's do. The 1,200, 2,400 and 6,400 dpi modes come from Epson Scan's
//! sessions, which SilverFast doesn't use.
//!
//! A session:
//!
//! 1. opens the scanner, switches the film lamp on and moves the carriage under the holder;
//! 2. finds the holder's calibration window in a 200 dpi read and parks the carriage under it;
//! 3. searches the exposure and per-channel gains through the window, and the analog offsets
//!    against dark reads of the CCD's masked strip;
//! 4. takes white and dark reads at the area's x range for the shading table;
//! 5. moves to the area and reads it at 400, 800, 1,200, 1,600, 2,400 or 6,400 dpi (3,200 ×
//!    2,400 at 2,400: the CCD samples finer across than the motor steps).
//!
//! An infrared pass ([`FilmSession::infrared`]) is a session of its own, as in SilverFast's
//! iSRD: the infrared lamp, an exposure fixed by the line time, and gains searched for it.
//!
//! Reads are 16-bit and linear, with flat tone curves; [`crate::decode`] applies the curve, and
//! makes 8-bit images from them.

use std::time::Duration;

use scan_core::image::Luma;
use scan_core::{Gray16Image, MM_PER_INCH, NoProgress, ProgressSink, ScanArea, ScanError};

use crate::calibration::{self, Shading};
use crate::decode::{self, Resample};
use crate::geometry::{STEPS_PER_INCH, skew_lines};
use crate::protocol::Protocol;
use crate::session::{
    AUTO_OFFSETS, CALIBRATED, DEVICE_DATA, EXPOSURE, EXPOSURE_21, GAIN, GAIN_NEUTRAL, GEOMETRY,
    IDENTIFY, INIT, LAST_SCAN, LEVELS, LIGHT, MAX_BLOCK, MOTOR, MOVE, OFFSETS, OPEN, PASS,
    POSITION, RECORD_SCAN, SETUP, STATE_65, STATE_A1, WHITE_LINES, WHITE_MOTOR, area_params,
    cancelled, upload,
};
use crate::tables;
use crate::template::RawLayout;
use crate::transport::Transport;

/// What the motor drives: `00` the CCD carriage, `02` the transparency unit's lamp, `03` both
/// (`1e 86` reads the two positions). The window read sweeps the lamp past the parked CCD.
const STAGE: [u8; 2] = [0x1e, 0x25];
const STAGE_CCD: u8 = 0x00;
const STAGE_LAMP: u8 = 0x02;
const STAGE_BOTH: u8 = 0x03;
/// The calibration window's position: `0x400` plus the move back to it, then `00 77`.
const WINDOW: [u8; 2] = [0x1e, 0x8a];

const FILM_LAMP: u8 = 0x02;
const FILM_LAMP_OFF: u8 = 0x03;
/// The infrared lamp: `06` for the calibration reads, `05` off, `04` for the image read.
const IR_CALIBRATION_LAMP: u8 = 0x06;
const IR_LAMP_OFF: u8 = 0x05;
const IR_LAMP: u8 = 0x04;
/// `1e e1` parameters in SilverFast's film sessions.
const SETUP_PARAMS: [u8; 6] = [0x0b, 0x00, 0x31, 0x61, 0x00, 0x00];
/// `1e 11` (the CCD's horizontal mode): 6,400 (`00`), 3,200 (`01`), 1,600 (`02`) or 800
/// (`03`) pixels per inch.
const CCD_800: u8 = 0x03;
const CCD_1600: u8 = 0x02;

/// The film area: 2.7 × 9.33 in, SilverFast's preview.
pub const FILM_AREA: ScanArea = ScanArea {
    x_mm: 0.0,
    y_mm: 0.0,
    width_mm: 68.58,
    height_mm: 236.98,
};

/// Carriage steps from home to just above the holder's calibration window.
const FIRST_MOVE: u32 = 932;
/// The infrared LED sits this far along the lamp unit from the visible lamp: SilverFast's
/// infrared pass moves the lamp alone this much first.
const IR_LAMP_MOVE: u32 = 535;
/// The infrared plane comes out this much wider than the color one, about the point
/// [`IR_CENTRE_MM`] across the film area: the lens seems to magnify the infrared a little
/// more. Measured on a V500 from the dust on frames in both strips at 1,600 dpi (to within a
/// pixel), and 800 dpi agrees. A fixed offset can't line the planes up: at 1,600 dpi a frame
/// in the left strip is 13 px off, one in the right strip 5 px the other way, and each frame
/// 12 px wider in infrared. [`FilmSession::align_infrared`] undoes it.
const IR_STRETCH: f64 = 0.0079;
const IR_CENTRE_MM: f64 = 39.2;
/// The infrared read reaches this many pixels (at hardware resolution) past the color one
/// on each side, so the stretch can be undone right to the area's edges: it moves pixels at
/// most 20 px at 1,600 dpi, at the far edges of the film area.
const IR_MARGIN: u32 = 32;
/// The infrared plane comes out this many lines (at hardware resolution) above the color one:
/// measured on a V500 at 1,600 and 2,400 dpi (5.6 and 4.8 lines, across the frame). The
/// infrared read starts [`IR_ABOVE_STEPS`] higher so it can be moved down.
const IR_LAG: u32 = 5;
/// Carriage steps the infrared read starts above the area: whole lines at every resolution.
const IR_ABOVE_STEPS: u32 = 48;
/// The read that finds the window: 150 lines at 200 dpi of the gap between the strips.
const WINDOW_READ: (u16, u16, u16) = (0x0c8f, 0x0e0f, 150);
const WINDOW_READ_STEPS: u32 = 1920;
const WINDOW_READ_RAMP: (u16, u16) = (1250, 336);
const WINDOW_READ_MOTOR: [u8; 8] = [0x78, 0x00, 0xff, 0x00, 0xff, 0x00, 0x02, 0x00];
/// Exposure and gain of the window reads.
const WINDOW_EXPOSURE: u16 = 0x1f80;
const WINDOW_GAIN: u8 = 0x1e;
/// The move back from the end of the window read when the window's lower edge is at
/// [`WINDOW_EDGE`]; a line (12 steps) less per line later.
const BACK_MOVE: u32 = 276;
const WINDOW_EDGE: f64 = 91.0;
/// A 4-line 16-bit read at minimum gain before the searches; SilverFast ignores it as far as
/// can be seen (it is saturated).
const LAMP_CHECK: (u16, u16, u16) = (0x0a51, 0x10a1, 4);

/// Gains the exposure search starts from, and the level it aims for in each channel (8-bit
/// means through the window).
const INITIAL_GAINS: [u8; 3] = [0x1c, 0x17, 0x13];
const TARGET: f64 = 206.0;
/// Gain is proportional to 1 / (POLE − code), measured from SilverFast's searches.
const GAIN_POLE: f64 = 72.0;
const MAX_GAIN: u8 = 0x3f;
/// SilverFast's exposure ends 0.85 × the one that brings the brightest channel to 243.
const EXPOSURE_FRACTION: f64 = 0.85 * 243.0;
/// Offsets are searched until each channel's dark level lies in this range.
const DARK_RANGE: (f64, f64) = (1024.0, 1536.0);
const FIRST_OFFSET: u16 = 0x80;
const OFFSET_STEP: u16 = 0x40;
const OFFSET_TRIES: usize = 8;
const DARK_LINES: u16 = 64;

/// Moves under the holder: a 512-entry acceleration from 2,500 to 222.
const MOVE_RAMP: (u16, u16, usize) = (2500, 222, 512);
const MOVE_MOTOR: [u8; 8] = [0, 0, 0xff, 0x01, 0xff, 0x01, 0x10, 0x00];
const BACK_MOTOR: [u8; 8] = [0, 0, 0xff, 0x01, 0xff, 0x01, 0x30, 0x00];
/// Short moves (to the top of the film area) run at one speed, with this period.
const SLOW_MOVE_PERIOD: u16 = 96;
const SLOW_MOVE_MOTOR: [u8; 8] = [0, 0, 0, 0, 0, 0, 0x18, 0x00];
const SLOW_BACK_MOTOR: [u8; 8] = [0, 0, 0, 0, 0, 0, 0x38, 0x00];
/// The slow move's lead-in is this much shorter than the fast one's: a fast move goes its
/// steps plus twice its 512-step ramp (as `1e 86` counts it).
const SLOW_MOVE_OFFSET: i64 = 1024;
const MIN_FAST_MOVE: i64 = 512;

/// Image reads at one motor speed (`1e 01` mode `08`): a line takes the motor period × 16 ×
/// steps per line, which is the line time `1e 21` sets. Byte 0 is the read's lead-in in steps,
/// as in every read's `1e 01`.
const fn steady_motor(lead_in: u8) -> [u8; 8] {
    [lead_in, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00]
}

/// The move to a steady read lands short by this: 368 steps more than the read's lead-in, in
/// SilverFast's sessions and Epson Scan's alike.
const fn steady_lead(lead_in: u8) -> i64 {
    368 + lead_in as i64
}

/// `1e 21` with only the line time set.
fn line_time(t: u16) -> [u8; 26] {
    let mut p = [0u8; 26];
    p[..2].copy_from_slice(&t.to_le_bytes());
    p
}

const STEADY_SCAN_MOTOR: [u8; 8] = steady_motor(0x7e);
const STEADY_LEAD: i64 = steady_lead(0x7e);
/// Infrared window reads run at this gain, unexposed (`1e 22` is 0 throughout an infrared
/// pass; the line time sets the exposure).
const IR_WINDOW_GAIN: u8 = 0x32;
/// The infrared gain search starts from 0 and aims for this mean.
const IR_TARGET: f64 = 210.0;

/// Constants of one hardware resolution, from SilverFast's captures and Epson Scan's.
#[derive(Clone, Copy, Debug)]
struct Mode {
    /// Lines per inch.
    hw_dpi: u32,
    /// Pixels per inch across: as many as the lines, except 3,200 in the 2,400 dpi mode.
    x_dpi: u32,
    /// `1e 41` bytes 9–10.
    resolution: [u8; 2],
    /// `1e 41` byte 8 of the image read (16-bit).
    subsampling: u8,
    /// `1e 11` during the searches and reads.
    ccd: u8,
    /// x units per inch of the area's left edge (`1e 41` bytes 0–1).
    units_per_inch: u32,
    /// x units per pixel of the read's width, as a fraction: 2 at 400 dpi and 4/3 at 1,200
    /// (both subsampled), otherwise 1. Above 1,600 dpi the width counts pixels while the left
    /// edge stays at 1,600 units per inch, as Epson Scan sends them.
    units_per_px: (u32, u32),
    /// x of the film area's left edge.
    origin: u16,
    /// x range of the exposure search through the window.
    search_x: (u16, u16),
    /// The CCD's masked strip, read for the offsets.
    strip: (u16, u16),
    /// `1e 22` of the first search read.
    first_exposure: u16,
    /// `1e 21`: the line time and longest exposure. A normal one is at most half of it.
    max_exposure: u16,
    /// `1e 21` during the exposure search. At 6,400 dpi the window read (17,280 px standing
    /// still) stalls after two blocks at the scan's line time; Epson Scan searches at a longer
    /// one.
    search_line_time: u16,
    /// The white read's motor period: the line time over 24 (it reads at 1,600 dpi, 1.5 steps
    /// a line).
    white_step: u16,
    scan_ramp: (u16, u16, usize),
    /// `1e 01` bytes 4–11 of the image read.
    scan_motor: [u8; 8],
    /// Steps the fast move lands short of 2,400 per inch.
    lead: i64,
    /// `1e 41` byte 11 of the image read: `80` at 6,400 dpi, as Epson Scan sends it.
    image_kind: u8,
    /// Lines each column sees the film after the earliest, by its x (`x0` + column) mod 4 (see
    /// [`Resample::stagger`]). Measured on a V500: the CCD's rows of pixels lie 16 lines
    /// apart at 6,400 dpi (0.064 mm, 6 lines at 2,400) and its halves 80 further, and Epson
    /// Scan reads as many lines more than the color skew needs (2 × 63 at 2,400, 2 × 211 at
    /// 6,400). At 2,400 dpi a read starting one unit further along swaps the columns over,
    /// though a unit is two pixels.
    stagger: [u8; 4],
    /// `1e 57` field 7, low byte.
    record_code: u32,
    /// The infrared pass: line time (`1e 21`), the image read's motor period, and the white
    /// read's.
    ir_line_time: u16,
    ir_period: u16,
    ir_white_step: u16,
}

const MODES: [Mode; 6] = [
    Mode {
        hw_dpi: 400,
        x_dpi: 400,
        resolution: [1, 6],
        subsampling: 0x10,
        ccd: CCD_800,
        units_per_inch: 800,
        units_per_px: (2, 1),
        origin: 2365,
        search_x: (0x0943, 0x11b3),
        strip: (0x20, 0x30),
        first_exposure: 1500,
        max_exposure: 0x0f00,
        search_line_time: 0x0f00,
        white_step: 160,
        scan_ramp: (1249, 320, 512),
        scan_motor: [0x7e, 0x00, 0xff, 0x00, 0xff, 0x00, 0x02, 0x00],
        lead: 750,
        image_kind: 0x83,
        stagger: [0; 4],
        record_code: 0x12,
        // Not captured; 800 dpi's line time at 6 steps a line.
        ir_line_time: 0x1860,
        ir_period: 65,
        ir_white_step: 260,
    },
    Mode {
        hw_dpi: 800,
        x_dpi: 800,
        resolution: [1, 3],
        subsampling: 0x00,
        ccd: CCD_800,
        units_per_inch: 800,
        units_per_px: (1, 1),
        origin: 2365,
        search_x: (0x0943, 0x11b3),
        strip: (0x20, 0x30),
        first_exposure: 1500,
        max_exposure: 0x0f00,
        search_line_time: 0x0f00,
        white_step: 160,
        scan_ramp: (1250, 640, 512),
        scan_motor: [0x7e, 0x00, 0xff, 0x00, 0xff, 0x00, 0x02, 0x00],
        lead: 750,
        image_kind: 0x83,
        stagger: [0; 4],
        record_code: 0x24,
        // Capture 38.
        ir_line_time: 0x1860,
        ir_period: 130,
        ir_white_step: 260,
    },
    // Capture 40 (Epson Scan): 1,600 dpi's CCD mode, subsampled.
    Mode {
        hw_dpi: 1200,
        x_dpi: 1200,
        resolution: [1, 2],
        subsampling: 0xa0,
        ccd: CCD_1600,
        units_per_inch: 1600,
        units_per_px: (4, 3),
        origin: 4730,
        search_x: (0x1287, 0x2367),
        strip: (0x3d, 0x6d),
        first_exposure: 4000,
        max_exposure: 0x1d40,
        search_line_time: 0x1d40,
        white_step: 312,
        scan_ramp: (234, 234, 1),
        scan_motor: steady_motor(0x80),
        lead: steady_lead(0x80),
        image_kind: 0x83,
        stagger: [0; 4],
        record_code: 0x20,
        // Not captured; about 1,600 dpi's line time, in whole periods at 2 steps a line.
        ir_line_time: 0x21c0,
        ir_period: 270,
        ir_white_step: 360,
    },
    Mode {
        hw_dpi: 1600,
        x_dpi: 1600,
        resolution: [2, 3],
        subsampling: 0x00,
        ccd: CCD_1600,
        units_per_inch: 1600,
        units_per_px: (1, 1),
        origin: 4730,
        search_x: (0x1287, 0x2367),
        strip: (0x3d, 0x6d),
        first_exposure: 4000,
        max_exposure: 0x1cf8,
        search_line_time: 0x1cf8,
        white_step: 309,
        scan_ramp: (309, 309, 1),
        scan_motor: STEADY_SCAN_MOTOR,
        lead: STEADY_LEAD,
        image_kind: 0x83,
        stagger: [0; 4],
        record_code: 0x45,
        // Capture 13.
        ir_line_time: 0x21a8,
        ir_period: 359,
        ir_white_step: 359,
    },
    // Captures 41–44 (Epson Scan's 2,400 and 3,200 dpi, the same read). The origin is
    // capture 41's: a frame scanned at it lines up with Epson Scan's to 0.05 mm, as at 1,200
    // and 6,400 dpi. Capture 44, of a frame in the other strip, puts it 19 units left.
    Mode {
        hw_dpi: 2400,
        x_dpi: 3200,
        resolution: [1, 1],
        subsampling: 0x00,
        ccd: 0x01,
        units_per_inch: 1600,
        units_per_px: (1, 1),
        origin: 4783,
        search_x: (0x12b2, 0x3472),
        strip: (0x64, 0xc4),
        first_exposure: 4000,
        max_exposure: 0x1fe0,
        search_line_time: 0x1fe0,
        white_step: 340,
        scan_ramp: (510, 510, 1),
        scan_motor: steady_motor(0x20),
        lead: steady_lead(0x20),
        image_kind: 0x83,
        stagger: [0, 6, 0, 6],
        record_code: 0x0f,
        // Capture 42.
        ir_line_time: 0x2ee0,
        ir_period: 750,
        ir_white_step: 500,
    },
    // Capture 45 (Epson Scan's 6,400 dpi). Its origin is 49 units right of 2,400 dpi's: Epson
    // Scan's images of one area in both line up.
    Mode {
        hw_dpi: 6400,
        x_dpi: 6400,
        resolution: [8, 3],
        subsampling: 0x00,
        ccd: 0x00,
        units_per_inch: 1600,
        units_per_px: (1, 1),
        origin: 4819,
        search_x: (0x12d4, 0x5654),
        strip: (0xc8, 0x188),
        first_exposure: 4000,
        max_exposure: 0x2550,
        search_line_time: 0x3690,
        white_step: 398,
        scan_ramp: (1592, 1592, 1),
        scan_motor: steady_motor(0x1e),
        lead: steady_lead(0x1e),
        image_kind: 0x80,
        stagger: [0, 80, 16, 96],
        record_code: 0x07,
        // Not captured; 2,400 dpi's infrared line time over its color one, at 6 periods a line.
        ir_line_time: 0x36d8,
        ir_period: 2340,
        ir_white_step: 585,
    },
];

impl Mode {
    /// Read widths are whole 16 px and whole x units.
    fn width_align(&self) -> u32 {
        16 * self.units_per_px.1
    }

    /// [`Mode::stagger`] by column of a read starting at `x0`.
    fn stagger_at(&self, x0: u16) -> [u8; 4] {
        std::array::from_fn(|c| self.stagger[(usize::from(x0) + c) % 4])
    }

    /// Pixels the infrared read reaches past the color one on each side: [`IR_MARGIN`],
    /// rounded up so the read stays aligned.
    fn ir_margin(&self) -> u32 {
        let half = self.width_align() / 2;
        IR_MARGIN.div_ceil(half) * half
    }

    /// x units of `px` pixels of width.
    fn width_units(&self, px: u32) -> u32 {
        px * self.units_per_px.0 / self.units_per_px.1
    }
}

/// What differs between a color pass and an infrared one.
#[derive(Clone, Copy, Debug)]
struct Light {
    infrared: bool,
    /// `1e a2` for the calibration reads, with the lamp off, and for the image read.
    calibrate: u8,
    off: u8,
    image: u8,
    window_exposure: u16,
    window_gain: u8,
    white_step: u16,
    /// The image read's motor table and `1e 01` mode, and the move's lead.
    scan_ramp: (u16, u16, usize),
    scan_motor: [u8; 8],
    lead: i64,
}

impl Light {
    fn new(mode: &Mode, infrared: bool) -> Self {
        if infrared {
            Self {
                infrared,
                calibrate: IR_CALIBRATION_LAMP,
                off: IR_LAMP_OFF,
                image: IR_LAMP,
                window_exposure: 0,
                window_gain: IR_WINDOW_GAIN,
                white_step: mode.ir_white_step,
                scan_ramp: (mode.ir_period, mode.ir_period, 1),
                // At one speed, with the color read's lead-in where that is steady too.
                scan_motor: if mode.scan_ramp.2 == 1 {
                    mode.scan_motor
                } else {
                    STEADY_SCAN_MOTOR
                },
                lead: if mode.scan_ramp.2 == 1 {
                    mode.lead
                } else {
                    STEADY_LEAD
                },
            }
        } else {
            Self {
                infrared,
                calibrate: FILM_LAMP,
                off: FILM_LAMP_OFF,
                image: FILM_LAMP,
                window_exposure: WINDOW_EXPOSURE,
                window_gain: WINDOW_GAIN,
                white_step: mode.white_step,
                scan_ramp: mode.scan_ramp,
                scan_motor: mode.scan_motor,
                lead: mode.lead,
            }
        }
    }

    /// `1e 22`: the exposure at bytes 6–7; byte 9 is 4 for reads with the lamp off. All zero
    /// in an infrared pass.
    fn exposure(&self, e: u16, dark: bool) -> [u8; 12] {
        if self.infrared {
            return [0; 12];
        }
        let e = e.to_le_bytes();
        [
            0,
            0,
            0,
            0,
            0,
            0,
            e[0],
            e[1],
            0,
            if dark { 4 } else { 0 },
            0,
            0,
        ]
    }
}

/// A carriage move under the holder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carriage {
    /// Accelerating; most moves.
    Fast(u32),
    /// At one speed, for areas near the top of the film area.
    Slow(u32),
}

/// A film read for one request.
#[derive(Clone, Debug, PartialEq)]
pub struct FilmPlan {
    pub output: Resample,
    /// Lines to read, including the color-skew margin.
    pub lines: u32,
    /// x range of the read, and its pixels per line.
    pub x_units: (u16, u16),
    pub read_width: u32,
    /// The area's position and size in pixels at hardware resolution (the `1e 57` record).
    pub pixels: [u32; 4],
    /// From the calibration window to the area.
    pub carriage: Carriage,
    pub scan_steps: u32,
}

impl FilmPlan {
    /// `lead` and `lead_in` are the move's and the read's (see [`steady_lead`]); the read
    /// starts `above` lines higher (whole steps), the output still at the area's top.
    fn new(
        area: &ScanArea,
        dpi: u32,
        mode: &Mode,
        lead: i64,
        lead_in: u8,
        above: u32,
    ) -> Result<Self, ScanError> {
        let (hw, hw_x) = (mode.hw_dpi, mode.x_dpi);
        let (width, height) = area.pixel_size(dpi);
        let scale = |hw: u32| f64::from(hw) / f64::from(dpi);
        let px = |mm: f32, hw: u32| {
            (f64::from(mm) / f64::from(MM_PER_INCH) * f64::from(hw)).round() as u32
        };
        let (x, y) = (px(area.x_mm, hw_x), px(area.y_mm, hw));
        let w = (f64::from(width) * scale(hw_x)).ceil() as u32;
        let h = (f64::from(height) * scale(hw)).floor() as u32;
        let read_width = w.div_ceil(mode.width_align()) * mode.width_align();
        let x0 = u32::from(mode.origin) + (x * mode.units_per_inch + hw_x / 2) / hw_x;
        let x1 = x0 + mode.width_units(read_width);
        let end = |a: f32, b: f32| f64::from(a + b) - 0.01;
        if end(area.x_mm, area.width_mm) > f64::from(FILM_AREA.width_mm)
            || end(area.y_mm, area.height_mm) > f64::from(FILM_AREA.height_mm)
            || area.x_mm < 0.0
            || area.y_mm < 0.0
        {
            return Err(ScanError::InvalidRequest(format!(
                "area {area:?} is outside the film area"
            )));
        }
        let y_steps = i64::from(y) * STEPS_PER_INCH as i64 / i64::from(hw);
        let lead = lead + i64::from(above * STEPS_PER_INCH as u32 / hw);
        let carriage = if y_steps - lead >= MIN_FAST_MOVE {
            Carriage::Fast((y_steps - lead) as u32)
        } else {
            Carriage::Slow((y_steps - lead + SLOW_MOVE_OFFSET) as u32)
        };
        let lines = above + h + 2 * skew_lines(hw) + u32::from(*mode.stagger.iter().max().unwrap());
        Ok(Self {
            output: Resample {
                dpi,
                hw_dpi: hw,
                hw_x_dpi: hw_x,
                width,
                height,
                stagger: mode.stagger_at(x0 as u16),
                first_line: above,
            },
            lines,
            x_units: (x0 as u16, x1 as u16),
            read_width,
            pixels: [x, y, w, h],
            carriage,
            scan_steps: (lines * STEPS_PER_INCH as u32).div_ceil(hw) + u32::from(lead_in),
        })
    }

    /// Width of the white and dark reads, in x units (one pixel per unit: they aren't
    /// subsampled).
    fn calibration_width(&self) -> usize {
        usize::from(self.x_units.1 - self.x_units.0)
    }
}

/// `1e 31`: per-channel offsets (u16), then gains.
fn analog(offsets: [u16; 3], gains: [u8; 3]) -> [u8; 12] {
    let mut a = [0u8; 12];
    for c in 0..3 {
        a[2 * c..2 * c + 2].copy_from_slice(&offsets[c].to_le_bytes());
        a[8 + c] = gains[c];
    }
    a
}

fn motor(steps: u32, mode: [u8; 8]) -> [u8; 12] {
    let mut m = [0u8; 12];
    m[..4].copy_from_slice(&steps.to_le_bytes());
    m[4..].copy_from_slice(&mode);
    m
}

/// `1e 43` with neutral values for both halves.
fn neutral_gain() -> [u8; 12] {
    let mut g = [0u8; 12];
    g[..6].copy_from_slice(&GAIN_NEUTRAL);
    g[6..].copy_from_slice(&GAIN_NEUTRAL);
    g
}

/// `1e 43` of the window reads: neutral, then the flatbed image read's values.
fn window_gain() -> [u8; 12] {
    let mut g = [0u8; 12];
    g[..6].copy_from_slice(&GAIN_NEUTRAL);
    g[6..].copy_from_slice(&[0x84, 0x7d, 0x77, 0x7e, 0xdf, 0x7f]);
    g
}

/// `1e 41` bytes 6–21 for a read: sample format, resolution, kind (byte 11), byte 16 and
/// lines per block.
fn read_params(format: u8, resolution: [u8; 2], kind: u8, flags: [u8; 2], block: u8) -> [u8; 16] {
    [
        0,
        0,
        format,
        resolution[0],
        resolution[1],
        kind,
        0,
        0,
        0,
        0,
        flags[0],
        flags[1],
        block,
        0,
        0,
        0,
    ]
}

/// Per-channel means of pixel-interleaved RGB, 8- or 16-bit.
fn channel_means(data: &[u8], sixteen: bool) -> [f64; 3] {
    let mut sum = [0.0; 3];
    let size = if sixteen { 2 } else { 1 };
    let n = data.len() / (3 * size);
    for px in data.chunks_exact(3 * size) {
        for (c, s) in sum.iter_mut().enumerate() {
            *s += if sixteen {
                f64::from(u16::from_le_bytes([px[2 * c], px[2 * c + 1]]))
            } else {
                f64::from(px[c])
            };
        }
    }
    sum.map(|s| s / n.max(1) as f64)
}

/// Gains that bring each channel's mean to `target`, from `means` read at `gains` and an
/// exposure `ratio` times shorter than the next read's: gain is proportional to
/// 1 / ([`GAIN_POLE`] − code).
fn solve_gains(gains: [u8; 3], means: [f64; 3], ratio: f64, target: f64) -> [u8; 3] {
    std::array::from_fn(|c| {
        let g = GAIN_POLE - (GAIN_POLE - f64::from(gains[c])) * means[c] * ratio / target;
        g.round().clamp(0.0, f64::from(MAX_GAIN)) as u8
    })
}

/// The move back from the end of the window read, from where the window's lower edge falls
/// in it. `None` if no window is visible.
fn back_move(window: &[u8], width: usize) -> Option<u32> {
    let rows: Vec<f64> = window
        .chunks_exact(width * 3)
        .map(|line| line.iter().map(|&v| f64::from(v)).sum::<f64>() / line.len() as f64)
        .collect();
    // The longest run of lit rows.
    let (mut best, mut start) = ((0, 0), None);
    for (i, &v) in rows.iter().enumerate() {
        match (v >= 128.0, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if i - s > best.1 - best.0 {
                    best = (s, i);
                }
                start = None;
            }
            _ => {}
        }
    }
    let (first, end) = best;
    if !(30..=100).contains(&(end - first)) || end >= rows.len() {
        return None;
    }
    let last = end - 1;
    let edge = last as f64 + (128.0 - rows[last]) / (rows[end] - rows[last]);
    let lines = (edge.round() - WINDOW_EDGE) as i64;
    u32::try_from(i64::from(BACK_MOVE) - 12 * lines).ok()
}

/// A film scan of one area, ready to run.
#[derive(Clone, Debug)]
pub struct FilmSession {
    mode: Mode,
    pub plan: FilmPlan,
    pub layout: RawLayout,
    /// Multiplies the exposure of the image read (multi-exposure passes).
    exposure_scale: f64,
    light: Light,
}

impl FilmSession {
    /// A color pass.
    pub fn new(area: &ScanArea, dpi: u32) -> Result<Self, ScanError> {
        Self::with_light(area, dpi, false)
    }

    /// An infrared pass of the same area: three channels that all see the infrared, at the
    /// color pass's geometry.
    pub fn infrared(area: &ScanArea, dpi: u32) -> Result<Self, ScanError> {
        Self::with_light(area, dpi, true)
    }

    fn with_light(area: &ScanArea, dpi: u32, infrared: bool) -> Result<Self, ScanError> {
        let mode = *MODES
            .iter()
            .find(|m| m.hw_dpi.min(m.x_dpi) >= dpi)
            .ok_or_else(|| {
                ScanError::Unsupported(format!("{dpi} dpi: native film scans go up to 6,400"))
            })?;
        let light = Light::new(&mode, infrared);
        let above = if infrared {
            IR_ABOVE_STEPS * mode.hw_dpi / STEPS_PER_INCH as u32
        } else {
            0
        };
        let mut plan = FilmPlan::new(area, dpi, &mode, light.lead, light.scan_motor[0], above)?;
        if infrared {
            let margin = mode.ir_margin();
            plan.read_width += 2 * margin;
            let x0 = plan.x_units.0 - (margin * mode.units_per_inch / mode.x_dpi) as u16;
            plan.x_units = (x0, x0 + mode.width_units(plan.read_width) as u16);
            plan.output.stagger = mode.stagger_at(x0);
            plan.output.first_line -= IR_LAG;
        }
        // `1e 41` counts the lines in 16 bits: a whole holder at 6,400 dpi just fits.
        if plan.lines > u32::from(u16::MAX) {
            return Err(ScanError::InvalidRequest(format!(
                "{} lines at {dpi} dpi: scan a shorter area",
                plan.lines
            )));
        }
        Ok(Self {
            mode,
            layout: RawLayout {
                lines: plan.lines,
                width: plan.read_width,
                crop_x: 0,
            },
            plan,
            exposure_scale: 1.0,
            light,
        })
    }

    /// A decoder for this session's read as it arrives ([`Self::run_into`]): the color
    /// pass's image, or for infrared the plane read, to be lined up with
    /// [`Self::align_infrared_plane`].
    pub fn decoder(&self) -> decode::Stream {
        let out = self.plan.output;
        if !self.light.infrared {
            return decode::Stream::rgb16(self.layout, out);
        }
        let scale = f64::from(out.hw_x_dpi) / f64::from(out.dpi);
        let wide = Resample {
            width: (f64::from(self.plan.read_width) / scale) as u32,
            ..out
        };
        decode::Stream::infrared(self.layout, wide)
    }

    /// Decode this infrared session's read onto the color pass's pixels, undoing the
    /// infrared's stretch ([`IR_STRETCH`]) with linear interpolation.
    pub fn decode_infrared(&self, raw: &[u8], area: &ScanArea) -> Gray16Image {
        let mut stream = self.decoder();
        stream.push(raw);
        self.align_infrared_plane(stream, area)
    }

    /// The infrared plane of [`Self::decoder`]'s stream, lined up like [`Self::decode_infrared`].
    pub fn align_infrared_plane(&self, stream: decode::Stream, area: &ScanArea) -> Gray16Image {
        let out = self.plan.output;
        let scale = f64::from(out.hw_x_dpi) / f64::from(out.dpi);
        let margin = f64::from(self.mode.ir_margin()) / scale;
        Self::align_infrared(&stream.into_infrared(), margin, area.x_mm, out)
    }

    /// The `out`-sized plane from `wide`, an infrared plane whose column `margin` is the
    /// area's left edge at `x_mm` across the film area.
    fn align_infrared(wide: &Gray16Image, margin: f64, x_mm: f32, out: Resample) -> Gray16Image {
        let px_per_mm = f64::from(out.dpi) / f64::from(MM_PER_INCH);
        let left = f64::from(x_mm) * px_per_mm;
        let centre = IR_CENTRE_MM * px_per_mm;
        let last = f64::from(wide.width().saturating_sub(1));
        Gray16Image::from_fn(out.width, out.height, |x, y| {
            // Where the color pixel's centre lands in the infrared.
            let film_x = left + f64::from(x) + 0.5;
            let src = (margin + f64::from(x) + IR_STRETCH * (film_x - centre)).clamp(0.0, last);
            let (x0, t) = (src.floor() as u32, src.fract());
            let x1 = (x0 + 1).min(wide.width() - 1);
            let (a, b) = (wide.get_pixel(x0, y).0[0], wide.get_pixel(x1, y).0[0]);
            Luma([(f64::from(a) * (1.0 - t) + f64::from(b) * t).round() as u16])
        })
    }

    /// Expose the image read `ev` stops more than the calibrated exposure (up to the scanner's
    /// longest), as SilverFast's multi-exposure does for its second pass.
    pub fn with_exposure(mut self, ev: f32) -> Self {
        self.exposure_scale = 2f64.powf(f64::from(ev));
        self
    }

    fn line_len(&self) -> usize {
        self.plan.read_width as usize * 6
    }

    /// Bytes the image read returns: 16-bit RGB.
    pub fn data_len(&self) -> usize {
        self.plan.lines as usize * self.line_len()
    }

    /// The `1e 57` record: resolution, the area in pixels, then SilverFast's constants.
    fn record(&self) -> [u8; 42] {
        let [x, y, w, h] = self.plan.pixels;
        let fields = [
            self.mode.x_dpi,
            self.mode.hw_dpi,
            x,
            y,
            w,
            h,
            0x0001_1012,
            0x0300 | self.mode.record_code,
            0x8000,
        ];
        let mut r = [0u8; 42];
        for (chunk, v) in r.as_chunks_mut::<4>().0.iter_mut().zip(fields) {
            *chunk = v.to_le_bytes();
        }
        r
    }

    fn move_carriage(
        p: &mut Protocol<'_>,
        carriage: Carriage,
        back: bool,
    ) -> Result<(), ScanError> {
        let params = match carriage {
            Carriage::Fast(steps) => {
                let (start, end, len) = MOVE_RAMP;
                upload(
                    p,
                    tables::MOTOR,
                    tables::MOTOR_ADDRESS,
                    &tables::motor_ramp(start, end, len),
                )?;
                motor(steps, if back { BACK_MOTOR } else { MOVE_MOTOR })
            }
            Carriage::Slow(steps) => {
                upload(
                    p,
                    tables::MOTOR,
                    tables::MOTOR_ADDRESS,
                    &SLOW_MOVE_PERIOD.to_le_bytes(),
                )?;
                motor(
                    steps,
                    if back {
                        SLOW_BACK_MOTOR
                    } else {
                        SLOW_MOVE_MOTOR
                    },
                )
            }
        };
        p.command(MOTOR, &[&params])?;
        p.command(MOVE, &[])?;
        p.wait_ready()
    }

    /// Take the CCD and the lamp home if a session stopped partway left them out (the moves
    /// are relative, and only a session's image read returns them).
    fn home(p: &mut Protocol<'_>, position: &[u8]) -> Result<(), ScanError> {
        for (stage, at) in [(STAGE_CCD, &position[..2]), (STAGE_LAMP, &position[2..])] {
            let at = i64::from(u16::from_le_bytes([at[0], at[1]]));
            if at == 0 {
                continue;
            }
            log::warn!(
                "{} is {at} steps from home; taking it back",
                if stage == STAGE_CCD {
                    "the CCD"
                } else {
                    "the lamp"
                }
            );
            let carriage = if at - SLOW_MOVE_OFFSET >= MIN_FAST_MOVE {
                Carriage::Fast((at - SLOW_MOVE_OFFSET) as u32)
            } else {
                Carriage::Slow(at as u32)
            };
            p.command(STAGE, &[&[stage]])?;
            Self::move_carriage(p, carriage, true)?;
            let left = p.query(POSITION, 4)?;
            log::debug!("position after homing: {left:02x?}");
        }
        Ok(())
    }

    /// Run the session on a scanner and return the raw image data (see [`Self::layout`]).
    pub fn run(&self, dev: &mut dyn Transport, poll_delay: Duration) -> Result<Vec<u8>, ScanError> {
        self.run_with(dev, poll_delay, &mut NoProgress)
    }

    /// [`Self::run`], reporting progress. A cancel is honoured only before the carriage first
    /// moves; after that the session runs to the end.
    pub fn run_with(
        &self,
        dev: &mut dyn Transport,
        poll_delay: Duration,
        progress: &mut dyn ProgressSink,
    ) -> Result<Vec<u8>, ScanError> {
        let mut data = Vec::with_capacity(self.data_len());
        self.run_into(dev, poll_delay, progress, &mut |block| {
            data.extend_from_slice(block)
        })?;
        Ok(data)
    }

    /// [`Self::run_with`], handing `received` the image read's blocks as they come, for
    /// decoding them as they arrive ([`decode::Stream`]) instead of keeping the read.
    pub fn run_into(
        &self,
        dev: &mut dyn Transport,
        poll_delay: Duration,
        progress: &mut dyn ProgressSink,
        received: &mut dyn FnMut(&[u8]),
    ) -> Result<(), ScanError> {
        let (m, l) = (self.mode, self.light);
        let mut p = Protocol::new(dev, poll_delay);

        // Open the device, with flat tone curves.
        p.query(OPEN, 1)?;
        p.wait_ready()?;
        p.query(LAST_SCAN, 42)?;
        p.command(SETUP, &[&SETUP_PARAMS])?;
        p.query(STATE_65, 1)?;
        let id = p.query(IDENTIFY, 28)?;
        log::info!("scanner: {}", String::from_utf8_lossy(&id).trim());
        p.command(INIT, &[])?;
        p.query(LAST_SCAN, 42)?;
        p.query(LAST_SCAN, 42)?;
        for address in tables::GAMMA8_ADDRESS {
            upload(&mut p, tables::GAMMA8, address, &tables::linear8())?;
        }
        p.query(LAST_SCAN, 42)?;
        p.command(RECORD_SCAN, &[&self.record()])?;
        p.query(STATE_A1, 1)?;
        p.command(LIGHT, &[&[l.calibrate]])?;
        let position = p.query(POSITION, 4)?;
        cancelled(progress)?;
        Self::home(&mut p, &position)?;

        // Under the holder, find the calibration window and park under it.
        if l.infrared {
            p.command(STAGE, &[&[STAGE_LAMP]])?;
            Self::move_carriage(&mut p, Carriage::Fast(IR_LAMP_MOVE), false)?;
            p.command(LIGHT, &[&[l.calibrate]])?;
            p.query(POSITION, 4)?;
        }
        p.command(STAGE, &[&[STAGE_CCD]])?;
        Self::move_carriage(&mut p, Carriage::Fast(FIRST_MOVE), false)?;
        p.query(POSITION, 4)?;
        p.command(LIGHT, &[&[l.calibrate]])?;
        p.command(STAGE, &[&[STAGE_LAMP]])?;
        p.command(PASS, &[&[CCD_800]])?;
        p.command(OFFSETS, &[&analog([FIRST_OFFSET; 3], [l.window_gain; 3])])?;
        p.command(EXPOSURE_21, &[&line_time(WINDOW_EXPOSURE)])?;
        upload(
            &mut p,
            tables::TYPE7,
            tables::TYPE7_ADDRESS,
            &tables::type7(),
        )?;
        let (x0, x1, lines) = WINDOW_READ;
        p.command(EXPOSURE, &[&l.exposure(l.window_exposure, false)])?;
        p.command(
            GEOMETRY,
            &[&area_params(
                x0,
                x1,
                lines,
                read_params(0x01, [1, 12], 0x00, [0x80, 0x80], lines as u8),
            )],
        )?;
        p.command(LEVELS, &[&[0u8; 18]])?;
        p.command(GAIN, &[&window_gain()])?;
        let (start, end) = WINDOW_READ_RAMP;
        upload(
            &mut p,
            tables::MOTOR,
            tables::MOTOR_ADDRESS,
            &tables::motor_ramp(start, end, 512),
        )?;
        p.command(MOTOR, &[&motor(WINDOW_READ_STEPS, WINDOW_READ_MOTOR)])?;
        let width = usize::from(x1 - x0);
        let window = p.read_data(width * usize::from(lines) * 3)?;
        p.wait_ready()?;
        let back = back_move(&window, width).unwrap_or_else(|| {
            log::warn!("no calibration window in the holder read; assuming the usual place");
            BACK_MOVE
        });
        log::debug!("calibration window: {back} steps back");
        Self::move_carriage(&mut p, Carriage::Fast(back), true)?;
        // SilverFast's infrared pass follows a color pass and leaves these out.
        if !l.infrared {
            let at = (0x400 + back as u16).to_le_bytes();
            p.command(WINDOW, &[&[at[0], at[1], 0x00, 0x77]])?;

            // A read at minimum gain.
            p.command(OFFSETS, &[&analog([FIRST_OFFSET; 3], [1; 3])])?;
            p.query(AUTO_OFFSETS, 12)?;
            let (x0, x1, lines) = LAMP_CHECK;
            p.command(EXPOSURE, &[&l.exposure(WINDOW_EXPOSURE, false)])?;
            p.command(
                GEOMETRY,
                &[&area_params(
                    x0,
                    x1,
                    lines,
                    read_params(0x00, [1, 3], 0x00, [0x80, 0x00], lines as u8),
                )],
            )?;
            p.command(LEVELS, &[&[0u8; 18]])?;
            p.command(GAIN, &[&window_gain()])?;
            p.read_data(usize::from(x1 - x0) * usize::from(lines) * 6)?;
            p.wait_ready()?;
        }
        p.command(STAGE, &[&[STAGE_BOTH]])?;
        p.request(DEVICE_DATA, &[0x00], 10)?;
        p.query(POSITION, 4)?;
        progress.progress(0.05);

        let (base_exposure, gains) = if l.infrared {
            (0, self.search_infrared_gains(&mut p)?)
        } else {
            self.search_exposure(&mut p)?
        };
        let offsets = self.search_offsets(&mut p, base_exposure, gains)?;
        let image_exposure = ((f64::from(base_exposure) * self.exposure_scale).round() as u16)
            .clamp(1, m.max_exposure);

        // Black level of the masked strip, for the white read.
        let (s0, s1) = m.strip;
        if !l.infrared {
            p.command(LIGHT, &[&[FILM_LAMP]])?;
            p.command(LIGHT, &[&[FILM_LAMP_OFF]])?;
        }
        p.command(EXPOSURE, &[&l.exposure(base_exposure, true)])?;
        p.command(
            GEOMETRY,
            &[&area_params(
                s0,
                s1,
                8,
                read_params(0x00, m.resolution, 0x00, [0x80, 0x00], 8),
            )],
        )?;
        p.command(LEVELS, &[&[0u8; 18]])?;
        p.command(GAIN, &[&neutral_gain()])?;
        let strip = p.read_data(usize::from(s1 - s0) * 8 * 6)?;
        p.wait_ready()?;
        let darks = calibration::white_read_darks(&strip);

        // White read through the window, then a dark read, at the area's x range.
        let (x0, x1) = self.plan.x_units;
        let cal_width = self.plan.calibration_width();
        if !l.infrared {
            p.command(LIGHT, &[&[l.calibrate]])?;
        }
        self.read_window(&mut p, (x0, x1), self.read_line_time())?;
        p.command(EXPOSURE, &[&l.exposure(base_exposure, false)])?;
        p.command(
            GEOMETRY,
            &[&area_params(
                x0,
                x1,
                WHITE_LINES,
                read_params(0x00, [2, 3], 0x40, [0x80, 0x80], WHITE_LINES as u8),
            )],
        )?;
        p.command(LEVELS, &[&calibration::white_read_levels(darks)])?;
        p.command(GAIN, &[&neutral_gain()])?;
        upload(
            &mut p,
            tables::MOTOR,
            tables::MOTOR_ADDRESS,
            &l.white_step.to_le_bytes(),
        )?;
        p.command(MOTOR, &[&WHITE_MOTOR])?;
        let white = p.read_data(cal_width * usize::from(WHITE_LINES) * 6)?;
        p.wait_ready()?;

        p.command(LIGHT, &[&[l.off]])?;
        let dark_geometry = area_params(
            x0,
            x1,
            DARK_LINES,
            read_params(0x00, m.resolution, 0x00, [0x80, 0x00], DARK_LINES as u8),
        );
        p.command(EXPOSURE, &[&l.exposure(image_exposure, true)])?;
        p.command(GEOMETRY, &[&dark_geometry])?;
        p.command(LEVELS, &[&[0u8; 18]])?;
        p.command(GAIN, &[&neutral_gain()])?;
        p.command(GEOMETRY, &[&dark_geometry])?;
        let dark = p.read_data(cal_width * usize::from(DARK_LINES) * 6)?;
        p.wait_ready()?;
        p.command(LIGHT, &[&[l.image]])?;
        p.query(POSITION, 4)?;

        let shading =
            Shading::compute(&white, &dark, cal_width, darks, cal_width).ok_or_else(|| {
                ScanError::Backend(format!(
                    "calibration reads of {} and {} bytes don't fit {cal_width} pixels",
                    white.len(),
                    dark.len()
                ))
            })?;
        log::info!(
            "film calibration{}: exposure {base_exposure} (image {image_exposure}), gains \
             {gains:02x?}, offsets {offsets:02x?}, white base {:?}, dark base {:?}",
            if l.infrared { " (infrared)" } else { "" },
            shading.white_base,
            shading.dark_base
        );
        progress.progress(CALIBRATED);

        // Flat tone curves, then move to the area.
        for address in tables::GAMMA12_ADDRESS {
            upload(&mut p, tables::GAMMA12, address, &tables::linear12())?;
        }
        Self::move_carriage(&mut p, self.plan.carriage, false)?;

        // The image read.
        let lines = u16::try_from(self.plan.lines)
            .map_err(|_| ScanError::InvalidRequest("area too tall".into()))?;
        let block = (MAX_BLOCK / self.line_len()).clamp(1, 255) as u8;
        let mut params = read_params(
            m.subsampling,
            m.resolution,
            m.image_kind,
            [0x80, 0xa0],
            block,
        );
        params[6] = 0x80;
        params[9] = 0x20;
        p.command(EXPOSURE, &[&l.exposure(image_exposure, false)])?;
        p.command(GEOMETRY, &[&area_params(x0, x1, lines, params)])?;
        p.command(LEVELS, &[&shading.levels()])?;
        p.command(GAIN, &[&neutral_gain()])?;
        upload(
            &mut p,
            tables::SHADING,
            tables::SHADING_ADDRESS,
            &shading.table,
        )?;
        let (start, end, len) = l.scan_ramp;
        upload(
            &mut p,
            tables::MOTOR,
            tables::MOTOR_ADDRESS,
            &tables::motor_ramp(start, end, len),
        )?;
        p.command(MOTOR, &[&motor(self.plan.scan_steps, l.scan_motor)])?;
        let len = self.data_len();
        let mut done = 0;
        p.read_data_into(len, &mut |block| {
            done += block.len();
            progress.progress(CALIBRATED + (1.0 - CALIBRATED) * done as f32 / len as f32);
            received(block);
        })?;
        p.wait_ready()?;
        p.query(OPEN, 1)?;
        Ok(())
    }

    /// Above 1,600 dpi, Epson Scan's type-7 table for a read of x `x0..x1` at line time `lt`
    /// (see [`tables::type7_window`]); the constant one is right below.
    fn read_window(
        &self,
        p: &mut Protocol<'_>,
        (x0, x1): (u16, u16),
        lt: u16,
    ) -> Result<(), ScanError> {
        if self.mode.hw_dpi <= 1600 {
            return Ok(());
        }
        upload(
            p,
            tables::TYPE7,
            tables::TYPE7_ADDRESS,
            &tables::type7_window(x0, x1, lt),
        )
    }

    /// The line time of the calibration and image reads.
    fn read_line_time(&self) -> u16 {
        if self.light.infrared {
            self.mode.ir_line_time
        } else {
            self.mode.max_exposure
        }
    }

    /// Mean 8-bit levels through the calibration window at an exposure.
    fn window_means(&self, p: &mut Protocol<'_>, e: u16) -> Result<[f64; 3], ScanError> {
        let m = self.mode;
        let (x0, x1) = m.search_x;
        p.command(EXPOSURE, &[&self.light.exposure(e, false)])?;
        p.command(
            GEOMETRY,
            &[&area_params(
                x0,
                x1,
                4,
                read_params(0x01, m.resolution, 0x00, [0x80, 0x00], 4),
            )],
        )?;
        p.command(LEVELS, &[&[0u8; 18]])?;
        p.command(GAIN, &[&neutral_gain()])?;
        let data = p.read_data(usize::from(x1 - x0) * 4 * 3)?;
        p.wait_ready()?;
        Ok(channel_means(&data, false))
    }

    /// Pick the exposure and per-channel gains that bring the window to [`TARGET`]: one read
    /// to scale the exposure as SilverFast ends up with it, then gains from the gain model,
    /// refined once.
    fn search_exposure(&self, p: &mut Protocol<'_>) -> Result<(u16, [u8; 3]), ScanError> {
        let m = self.mode;
        let mut gains = INITIAL_GAINS;
        p.command(OFFSETS, &[&analog([FIRST_OFFSET; 3], gains)])?;
        if m.ccd != CCD_800 {
            p.command(PASS, &[&[m.ccd]])?;
        }
        p.command(EXPOSURE_21, &[&line_time(m.search_line_time)])?;
        self.read_window(p, m.search_x, m.search_line_time)?;

        let mut e = m.first_exposure;
        let mut means = self.window_means(p, e)?;
        // Saturated: halve until the levels mean something.
        for _ in 0..4 {
            if means.iter().cloned().fold(0.0, f64::max) < 250.0 {
                break;
            }
            e /= 2;
            means = self.window_means(p, e)?;
        }
        let brightest = means.iter().cloned().fold(0.0, f64::max);
        if brightest < 8.0 {
            return Err(ScanError::Backend(format!(
                "the film lamp gives no light through the holder's window (levels {means:.0?})"
            )));
        }
        let new_e = (f64::from(e) * EXPOSURE_FRACTION / brightest)
            .round()
            .clamp(1.0, f64::from(m.max_exposure / 2)) as u16;
        gains = solve_gains(gains, means, f64::from(new_e) / f64::from(e), TARGET);
        e = new_e;
        for _ in 0..2 {
            p.command(OFFSETS, &[&analog([FIRST_OFFSET; 3], gains)])?;
            means = self.window_means(p, e)?;
            log::debug!("exposure {e}, gains {gains:02x?}: window {means:.1?}");
            let next = solve_gains(gains, means, 1.0, TARGET);
            if next == gains {
                break;
            }
            gains = next;
        }
        if m.search_line_time != m.max_exposure {
            p.command(EXPOSURE_21, &[&line_time(m.max_exposure)])?;
        }
        Ok((e, gains))
    }

    /// The infrared pass's gains: its exposure is the line time, so only the gains are
    /// searched, from 0, as SilverFast does.
    fn search_infrared_gains(&self, p: &mut Protocol<'_>) -> Result<[u8; 3], ScanError> {
        let m = self.mode;
        let mut gains = [0; 3];
        p.command(OFFSETS, &[&analog([FIRST_OFFSET; 3], gains)])?;
        p.command(PASS, &[&[m.ccd]])?;
        p.command(EXPOSURE_21, &[&line_time(m.ir_line_time)])?;
        upload(p, tables::TYPE7, tables::TYPE7_ADDRESS, &tables::type7())?;
        self.read_window(p, m.search_x, m.ir_line_time)?;
        let mut means = self.window_means(p, 0)?;
        if means.iter().cloned().fold(0.0, f64::max) < 8.0 {
            return Err(ScanError::Backend(format!(
                "the infrared lamp gives no light through the holder's window (levels \
                 {means:.0?})"
            )));
        }
        for _ in 0..2 {
            let next = solve_gains(gains, means, 1.0, IR_TARGET);
            if next == gains {
                break;
            }
            gains = next;
            p.command(OFFSETS, &[&analog([FIRST_OFFSET; 3], gains)])?;
            means = self.window_means(p, 0)?;
            log::debug!("infrared gains {gains:02x?}: window {means:.1?}");
        }
        Ok(gains)
    }

    /// Search the analog offsets that put each channel's dark level in [`DARK_RANGE`], halving
    /// the step each time, as SilverFast does.
    fn search_offsets(
        &self,
        p: &mut Protocol<'_>,
        e: u16,
        gains: [u8; 3],
    ) -> Result<[u16; 3], ScanError> {
        let (s0, s1) = self.mode.strip;
        self.read_window(p, self.mode.strip, self.read_line_time())?;
        let geometry = area_params(
            s0,
            s1,
            4,
            read_params(0x00, self.mode.resolution, 0x00, [0x80, 0x00], 4),
        );
        p.command(EXPOSURE, &[&self.light.exposure(e, true)])?;
        p.command(GEOMETRY, &[&geometry])?;
        p.command(LEVELS, &[&[0u8; 18]])?;
        p.command(GAIN, &[&neutral_gain()])?;
        let mut offsets = [FIRST_OFFSET; 3];
        let mut step = OFFSET_STEP;
        for _ in 0..OFFSET_TRIES {
            p.command(OFFSETS, &[&analog(offsets, gains)])?;
            // The strip is masked; the infrared pass leaves its lamp on.
            if !self.light.infrared {
                p.command(LIGHT, &[&[FILM_LAMP_OFF]])?;
            }
            p.command(GEOMETRY, &[&geometry])?;
            let data = p.read_data(usize::from(s1 - s0) * 4 * 6)?;
            p.wait_ready()?;
            let means = channel_means(&data, true);
            let mut settled = true;
            for (o, &level) in offsets.iter_mut().zip(&means) {
                if level < DARK_RANGE.0 {
                    *o += step;
                    settled = false;
                } else if level > DARK_RANGE.1 {
                    *o = o.saturating_sub(step);
                    settled = false;
                }
            }
            log::debug!("dark levels {means:.0?}");
            if settled {
                return Ok(offsets);
            }
            step = (step / 2).max(1);
        }
        log::warn!("offsets {offsets:02x?} leave the dark level outside {DARK_RANGE:?}");
        p.command(OFFSETS, &[&analog(offsets, gains)])?;
        Ok(offsets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::SimulatedScanner;

    fn area(x: f32, y: f32, w: f32, h: f32) -> ScanArea {
        ScanArea {
            x_mm: x,
            y_mm: y,
            width_mm: w,
            height_mm: h,
        }
    }

    fn inch(v: f32) -> f32 {
        v * MM_PER_INCH
    }

    /// A whole holder plans at every resolution, in color and infrared: the read is decoded
    /// as it arrives, and its lines just fit `1e 41`'s 16 bits at 6,400 dpi.
    #[test]
    fn whole_holder_at_every_resolution() {
        for dpi in [400, 800, 1200, 1600, 2400, 3200, 6400] {
            for session in [
                FilmSession::new(&FILM_AREA, dpi),
                FilmSession::infrared(&FILM_AREA, dpi),
            ] {
                let s = session.unwrap();
                assert!(s.plan.lines <= u32::from(u16::MAX), "{dpi} dpi");
            }
        }
        let taller = ScanArea {
            height_mm: 300.0,
            ..FILM_AREA
        };
        assert!(FilmSession::new(&taller, 6400).is_err());
    }

    /// The geometry SilverFast sent in captures 30, 31, 33, 35 and 36, from their `1e 57`
    /// records (the area in pixels at hardware resolution).
    #[test]
    fn matches_silverfast() {
        // 30: 400 ppi, x 48, y 3,142, 373 × 567 px.
        let a = area(
            inch(48.0 / 400.0),
            inch(3142.0 / 400.0),
            inch(373.0 / 400.0),
            inch(567.0 / 400.0),
        );
        let s = FilmSession::new(&a, 400).unwrap();
        assert_eq!(s.plan.x_units, (0x099d, 0x0c9d));
        assert_eq!((s.plan.read_width, s.plan.lines), (384, 0x24b));
        assert_eq!(s.plan.carriage, Carriage::Fast(0x46b6));
        assert_eq!(s.plan.scan_steps, 0x0e40);
        assert_eq!(s.plan.pixels, [48, 3142, 373, 567]);

        // 31: 800 ppi, x 97, y 6,284, 746 × 1,134 px.
        let a = area(
            inch(97.0 / 800.0),
            inch(6284.0 / 800.0),
            inch(746.0 / 800.0),
            inch(1134.0 / 800.0),
        );
        let s = FilmSession::new(&a, 800).unwrap();
        assert_eq!(s.plan.x_units, (0x099e, 0x0c8e));
        assert_eq!(s.plan.lines, 0x496);
        assert_eq!(s.plan.carriage, Carriage::Fast(0x46b6));
        assert_eq!(s.plan.scan_steps, 0x0e40);
        assert_eq!(s.data_len(), 0x496 * 752 * 6);

        // 33, 35, 36: 1,600 ppi.
        for (x, y, w, h, x_units, steps, scan) in [
            (194, 12569, 1491, 2268, (0x133c, 0x191c), 0x47b7, 0x0e40),
            (2480, 10397, 1499, 2268, (0x1c2a, 0x220a), 0x3afd, 0x0e40),
            (182, 5377, 1477, 2270, (0x1330, 0x1900), 0x1d93, 0x0e43),
        ] {
            let px = |v: u32| inch(v as f32 / 1600.0);
            let s = FilmSession::new(&area(px(x), px(y), px(w), px(h)), 1600).unwrap();
            assert_eq!(s.plan.x_units, x_units, "x {x}");
            assert_eq!(s.plan.carriage, Carriage::Fast(steps), "y {y}");
            assert_eq!(s.plan.scan_steps, scan, "y {y}");
            assert_eq!(s.plan.pixels, [x, y, w, h]);
        }
    }

    #[test]
    fn records_the_scan() {
        let a = area(
            inch(97.0 / 800.0),
            inch(6284.0 / 800.0),
            inch(746.0 / 800.0),
            inch(1134.0 / 800.0),
        );
        let r = FilmSession::new(&a, 800).unwrap().record();
        // Capture 31's record.
        assert_eq!(
            r[..36],
            [
                0x20, 0x03, 0, 0, 0x20, 0x03, 0, 0, 0x61, 0, 0, 0, 0x8c, 0x18, 0, 0, 0xea, 0x02, 0,
                0, 0x6e, 0x04, 0, 0, 0x12, 0x10, 0x01, 0, 0x24, 0x03, 0, 0, 0, 0x80, 0, 0
            ]
        );
    }

    #[test]
    fn top_of_the_film_moves_slowly() {
        // Capture 11: SilverFast's preview at y = 0 moves 274 steps at one speed.
        let s = FilmSession::new(&FILM_AREA, 400).unwrap();
        assert_eq!(s.plan.carriage, Carriage::Slow(274));
        assert!(FilmSession::new(&area(60.0, 0.0, 10.0, 10.0), 800).is_err());
        assert!(FilmSession::new(&area(0.0, 230.0, 10.0, 10.0), 800).is_err());
        assert!(FilmSession::new(&area(0.0, 0.0, 10.0, 10.0), 9600).is_err());
    }

    /// The geometry Epson Scan sent in captures 40, 41, 44 and 45, from their `1e 57` records
    /// (the area in pixels: x at the CCD's resolution, y at the lines'), `1e 41` and `1e 01`.
    #[test]
    fn matches_epson_scan() {
        // (dpi, record x and y resolution, x, y, w, h, the read's x range, its move)
        for (dpi, (rx, ry), [x, y, w, h], (x0, x1), steps) in [
            (
                1200,
                (1200, 1200),
                [1904, 9627, 1027, 1483],
                (0x1c6b, 0x226b),
                0x4946,
            ),
            (
                2400,
                (3200, 2400),
                [5077, 19255, 2741, 2967],
                (0x1c9a, 0x275a),
                0x49a7,
            ),
            (
                2400,
                (3200, 2400),
                [287, 8119, 2901, 3344],
                (0x132c, 0x1e8c),
                0x1e27,
            ),
            (
                3200,
                (6400, 6400),
                [10154, 51349, 5482, 7914],
                (0x1ccb, 0x323b),
                0x49a9,
            ),
        ] {
            let a = area(
                inch(x as f32 / rx as f32),
                inch(y as f32 / ry as f32),
                inch(w as f32 / rx as f32),
                inch(h as f32 / ry as f32),
            );
            let s = FilmSession::new(&a, dpi).unwrap();
            assert_eq!((s.mode.x_dpi, s.mode.hw_dpi), (rx, ry), "{dpi} dpi");
            assert_eq!(s.plan.carriage, Carriage::Fast(steps), "{dpi} dpi, y {y}");
            // Epson Scan reads 1,200 dpi wider than asked; the others to whole 16 px.
            if dpi != 1200 {
                assert_eq!(s.plan.x_units.1 - s.plan.x_units.0, x1 - x0, "{dpi} dpi");
            }
            // The left edge, within the 19 units captures 41 and 44 disagree by.
            assert!(
                s.plan.x_units.0.abs_diff(x0) <= 19,
                "{dpi} dpi, x {x}: {:#x} vs {x0:#x}",
                s.plan.x_units.0
            );
        }
    }

    #[test]
    fn finds_the_window_edge() {
        // Dark rows, then the window from row 27 to 91, then dark again.
        let width = 4;
        let rows = |fall: usize| -> Vec<u8> {
            (0..150)
                .flat_map(|r| {
                    let v = if (27..fall).contains(&r) { 250 } else { 60 };
                    vec![v; width * 3]
                })
                .collect()
        };
        assert_eq!(back_move(&rows(91), width), Some(276));
        assert_eq!(back_move(&rows(92), width), Some(264));
        assert_eq!(back_move(&vec![200; 150 * width * 3], width), None);
    }

    /// The commands a session sends, against SilverFast's in captures 30, 31, 33, 35 and 36,
    /// outside the searches (whose reads differ). Needs `target/captures` (not committed):
    /// `cargo test -p scan-epson -- --ignored`.
    #[test]
    #[ignore]
    fn matches_silverfast_captures() {
        use crate::trace::Direction;
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/captures/");
        for file in [
            "30_film_400ppi.pcap",
            "31_film_800ppi.pcap",
            "33_film_3200ppi.pcap",
            "35_film_1600ppi_other_strip.pcap",
            "36_film_1600ppi_same_strip.pcap",
        ] {
            let packets =
                crate::pcap::parse(&std::fs::read(format!("{dir}{file}")).unwrap()).unwrap();
            let device = crate::trace::busiest_device(&packets).unwrap();
            // Status polls, and state queries SilverFast sends only in some sessions.
            let skip = |w: &[u8]| {
                [[0x1b, 0x03], OPEN, STATE_A1, [0x1e, 0x77]]
                    .iter()
                    .any(|c| w == c)
            };
            let theirs: Vec<Vec<u8>> = crate::trace::transfers(&packets, device)
                .into_iter()
                .filter(|t| t.kind == crate::pcap::TransferType::Bulk)
                .filter(|t| t.direction == Direction::Out && !skip(&t.data))
                .map(|t| t.data)
                .collect();
            // The area from SilverFast's scan record.
            let record = theirs[theirs.iter().position(|w| w[..] == RECORD_SCAN).unwrap() + 1]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect::<Vec<_>>();
            let hw = record[0];
            let mm = |px: u32| px as f32 / hw as f32 * MM_PER_INCH;
            let a = area(mm(record[2]), mm(record[3]), mm(record[4]), mm(record[5]));
            let session = FilmSession::new(&a, hw).unwrap();
            let mut sim = SimulatedScanner::default();
            session.run(&mut sim, Duration::ZERO).unwrap();
            let ours: Vec<Vec<u8>> = sim.writes.into_iter().filter(|w| !skip(w)).collect();

            // From opening to the end of the read at minimum gain, and from the masked-strip
            // read before the white read to the end.
            let opening = |w: &[Vec<u8>]| {
                let end = w
                    .windows(2)
                    .position(|p| p[0] == STAGE && p[1] == [0x03])
                    .unwrap();
                w[..end].to_vec()
            };
            let closing = |w: &[Vec<u8>]| {
                let strip = w
                    .iter()
                    .position(|c| c.len() == 22 && c[4..6] == [8, 0])
                    .unwrap();
                w[strip - 3..].to_vec()
            };
            for (part, t, o) in [
                ("opening", opening(&theirs), opening(&ours)),
                ("closing", closing(&theirs), closing(&ours)),
            ] {
                assert_eq!(t.len(), o.len(), "{file} {part}: command count");
                for (i, (tw, ow)) in t.iter().zip(&o).enumerate() {
                    if tw == ow {
                        continue;
                    }
                    let cmd = t[..i]
                        .iter()
                        .rev()
                        .find(|c| c.len() == 2 && matches!(c[0], 0x1b | 0x1e))
                        .unwrap();
                    let header = &t[i - 1];
                    match &cmd[..] {
                        // Calibration values differ per session.
                        c if c == OFFSETS || c == LEVELS => {}
                        // Lines per block: SilverFast's blocks grow to 256 KiB at 1,600 dpi;
                        // ours stay at 64 KiB.
                        c if c == GEOMETRY && tw[11] == 0x83 => {
                            assert_eq!([&tw[..18], &tw[19..]], [&ow[..18], &ow[19..]]);
                        }
                        c if c == EXPOSURE => assert_eq!(
                            [&tw[..6], &tw[8..]],
                            [&ow[..6], &ow[8..]],
                            "{file} {part}: exposure {i}"
                        ),
                        [0x1e, 0x84] if header[0] == tables::SHADING => {}
                        [0x1e, 0x84] if header[0] == tables::MOTOR => {
                            let worst = tw
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .zip(ow.as_chunks::<2>().0)
                                .map(|(a, b)| {
                                    u16::from_le_bytes(*a).abs_diff(u16::from_le_bytes(*b))
                                })
                                .max();
                            assert_eq!(tw.len(), ow.len(), "{file} {part}: motor table {i}");
                            assert!(
                                worst <= Some(1),
                                "{file} {part}: motor table {i}: {worst:?}"
                            );
                        }
                        _ => {
                            let at = tw.iter().zip(ow).position(|(a, b)| a != b);
                            panic!(
                                "{file} {part}: write {i} after {cmd:02x?} ({} vs {} bytes),                                  first difference at {at:?}: {:02x?} vs {:02x?}",
                                tw.len(),
                                ow.len(),
                                &tw[..tw.len().min(24)],
                                &ow[..ow.len().min(24)]
                            )
                        }
                    }
                }
            }
        }
    }

    /// The gain model against SilverFast's searches: from the levels it read, the gains it
    /// chose next (captures 33 and 38).
    #[test]
    fn solves_gains_like_silverfast() {
        // Color, 1,600 dpi: 1c 17 13 read 205.1, 243.6, 229.1; SilverFast balanced G and B
        // down to R in two steps, reaching 1c 0e 0d.
        assert_eq!(
            solve_gains([0x1c, 0x17, 0x13], [205.1, 243.6, 229.1], 1.0, 205.1),
            [0x1c, 0x0e, 0x0d]
        );
        // Infrared, 800 dpi: from 0, 166.0, 146.2, 153.6 led to 0f 16 14 (and 210.4, 211.9,
        // 213.7); the model lands within a step.
        let ir = solve_gains([0; 3], [166.0, 146.2, 153.6], 1.0, IR_TARGET);
        for (got, want) in ir.into_iter().zip([0x0f, 0x16, 0x14]) {
            assert!(got.abs_diff(want) <= 1, "{ir:02x?}");
        }
    }

    #[test]
    fn infrared_alignment_undoes_the_stretch() {
        // A wide plane whose values are their own column, ×10.
        let wide = Gray16Image::from_fn(1700, 2, |x, _| Luma([(x * 10) as u16]));
        let out = Resample {
            dpi: 1600,
            hw_dpi: 1600,
            hw_x_dpi: 1600,
            width: 1600,
            height: 2,
            stagger: [0; 4],
            first_line: 0,
        };
        let px_per_mm = 1600.0 / f64::from(MM_PER_INCH);
        // A frame in the left strip: its infrared lands left of the color, increasingly so
        // further from the centre.
        let aligned = FilmSession::align_infrared(&wide, 32.0, 3.0, out);
        let at = |x: u32| f64::from(aligned.get_pixel(x, 0).0[0]) / 10.0 - 32.0 - f64::from(x);
        for x in [0, 800, 1599] {
            let film_x = 3.0 * px_per_mm + f64::from(x) + 0.5;
            let expected = IR_STRETCH * (film_x - IR_CENTRE_MM * px_per_mm);
            assert!(
                (at(x) - expected).abs() < 0.1,
                "x {x}: {} vs {expected}",
                at(x)
            );
        }
        assert!(at(0) < -12.0 && at(1599) > at(0) + 12.0);
    }

    #[test]
    fn infrared_pass_on_simulator() {
        let a = area(5.0, 40.0, 24.0, 36.0);
        for (dpi, a) in [
            (800, a),
            (1200, a),
            (1600, a),
            (2400, area(5.0, 40.0, 6.0, 6.0)),
            (3200, area(5.0, 40.0, 3.0, 3.0)),
        ] {
            let session = FilmSession::infrared(&a, dpi).unwrap();
            // The same read as the color pass, moved with the steady motor's lead, and wider.
            let color = FilmSession::new(&a, dpi).unwrap();
            let m = session.mode;
            let margin = m.ir_margin();
            let (x0, x1) = color.plan.x_units;
            let left = (margin * m.units_per_inch / m.x_dpi) as u16;
            let right = m.width_units(2 * margin) as u16 - left;
            assert_eq!(session.plan.x_units, (x0 - left, x1 + right), "{dpi} dpi");
            assert_eq!(session.plan.read_width, color.plan.read_width + 2 * margin);
            assert_eq!(session.plan.read_width % m.width_align(), 0);
            let mut sim = SimulatedScanner::default();
            let data = session.run(&mut sim, Duration::ZERO).unwrap();
            assert_eq!(data.len(), session.data_len());
            let lamps: Vec<u8> = sim
                .writes
                .windows(2)
                .filter(|w| w[0] == LIGHT)
                .map(|w| w[1][0])
                .collect();
            assert!(
                lamps.contains(&IR_LAMP) && !lamps.contains(&FILM_LAMP),
                "{lamps:02x?}"
            );
            // Unexposed throughout; the image read at one motor speed.
            let exposures = sim.writes.windows(2).filter(|w| w[0] == EXPOSURE);
            assert!(exposures.into_iter().all(|w| w[1] == [0; 12]));
            let motor = sim.writes.iter().rev().find(|w| w.len() == 12).unwrap();
            assert_eq!(motor[4..], session.light.scan_motor);
            assert_eq!(motor[10], 0x08);
        }
        let ir = FilmSession::infrared(&a, 800).unwrap();
        let color = FilmSession::new(&a, 800).unwrap();
        let (Carriage::Fast(i), Carriage::Fast(c)) = (ir.plan.carriage, color.plan.carriage) else {
            panic!("slow move");
        };
        // The steady motor's lead, less the lines read above the area.
        assert_eq!(i - c, 256 - IR_ABOVE_STEPS);
        assert_eq!(ir.plan.lines, color.plan.lines + IR_ABOVE_STEPS / 3);
        assert_eq!(ir.plan.output.first_line, IR_ABOVE_STEPS / 3 - IR_LAG);
    }

    /// A session that finds the CCD and the lamp out (as one stopped partway leaves them)
    /// takes each back on its own before starting.
    #[test]
    fn starts_by_going_home() {
        let session = FilmSession::new(&area(5.0, 40.0, 24.0, 36.0), 800).unwrap();
        let mut sim = SimulatedScanner::default();
        sim.position = [1956, 1300];
        session.run(&mut sim, Duration::ZERO).unwrap();
        // Each motor command with the stage selected before it.
        let mut stage = None;
        let mut moves = Vec::new();
        for w in sim.writes.windows(2) {
            if w[0] == STAGE {
                stage = Some(w[1][0]);
            } else if w[0] == MOTOR {
                moves.push((stage.unwrap(), w[1].clone()));
            }
        }
        assert_eq!(moves[0], (STAGE_CCD, motor(932, BACK_MOTOR).to_vec()));
        assert_eq!(
            moves[1],
            (STAGE_LAMP, motor(1300, SLOW_BACK_MOTOR).to_vec())
        );
        assert_eq!(sim.position, [0; 2]);
    }

    #[test]
    fn runs_on_simulator() {
        let frame = area(5.0, 40.0, 24.0, 36.0);
        for (dpi, a) in [
            (300, frame),
            (800, frame),
            (1200, frame),
            (1600, frame),
            (2400, area(5.0, 40.0, 6.0, 6.0)),
            (3200, area(5.0, 40.0, 3.0, 3.0)),
            (6400, area(5.0, 40.0, 3.0, 3.0)),
        ] {
            let session = FilmSession::new(&a, dpi).unwrap();
            let mut sim = SimulatedScanner::default();
            let data = session.run(&mut sim, Duration::ZERO).unwrap();
            assert_eq!(data.len(), session.data_len(), "{dpi} dpi");
            // Decodes to the requested size.
            let image = decode::rgb16(&data, session.layout, session.plan.output);
            let (w, h) = a.pixel_size(dpi);
            assert_eq!(image.dimensions(), (w, h), "{dpi} dpi");
            // The film lamp, then 16-bit linear data.
            assert!(sim.writes.contains(&vec![FILM_LAMP]));
            assert!(sim.writes.contains(&tables::linear12()));
            let geometry = sim.writes.iter().rev().find(|w| w.len() == 22).unwrap();
            assert_eq!(geometry[11], session.mode.image_kind);
            assert_eq!(geometry[8] & 0x01, 0);
        }
    }
}
