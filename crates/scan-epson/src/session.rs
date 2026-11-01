//! Flatbed scans generated from scratch: the whole session Epson's driver runs, with its
//! constants and tables (see `docs/epson-v500`), so no capture is needed.
//!
//! Like the driver, a session reads the bed's full width at the hardware resolution and the
//! requested area is cropped from it when decoding. Supported: 8- and 16-bit RGB at hardware
//! resolutions 200 and 400 dpi (requests up to 400 dpi). 16-bit reads are linear, with flat
//! tone curves as SilverFast loads them; [`crate::decode::flatbed_rgb16`] applies the curve.

use std::time::Duration;

use scan_core::{BitDepth, MM_PER_INCH, NoProgress, ProgressSink, ScanArea, ScanError};

use crate::calibration::{self, Shading};
use crate::geometry::{self, FlatbedPlan, MoveProfile};
use crate::protocol::Protocol;
use crate::tables;
use crate::template::RawLayout;
use crate::transport::Transport;

pub(crate) const INIT: [u8; 2] = [0x1b, 0x40];
pub(crate) const IDENTIFY: [u8; 2] = [0x1b, 0x13];
pub(crate) const OPEN: [u8; 2] = [0x1e, 0x85];
pub(crate) const LAST_SCAN: [u8; 2] = [0x1e, 0x53];
pub(crate) const RECORD_SCAN: [u8; 2] = [0x1e, 0x57];
pub(crate) const SETUP: [u8; 2] = [0x1e, 0xe1];
pub(crate) const STATE_A1: [u8; 2] = [0x1e, 0xa1];
pub(crate) const STATE_65: [u8; 2] = [0x1e, 0x65];
pub(crate) const LIGHT: [u8; 2] = [0x1e, 0xa2];
pub(crate) const DEVICE_DATA: [u8; 2] = [0x1e, 0x87];
pub(crate) const POSITION: [u8; 2] = [0x1e, 0x86];
pub(crate) const OFFSETS: [u8; 2] = [0x1e, 0x31];
pub(crate) const START_OFFSETS: [u8; 2] = [0x1e, 0x46];
pub(crate) const OFFSETS_BUSY: [u8; 2] = [0x1e, 0x04];
pub(crate) const AUTO_OFFSETS: [u8; 2] = [0x1e, 0x92];
pub(crate) const PASS: [u8; 2] = [0x1e, 0x11];
pub(crate) const EXPOSURE_21: [u8; 2] = [0x1e, 0x21];
pub(crate) const EXPOSURE: [u8; 2] = [0x1e, 0x22];
pub(crate) const GEOMETRY: [u8; 2] = [0x1e, 0x41];
pub(crate) const LEVELS: [u8; 2] = [0x1e, 0x42];
pub(crate) const GAIN: [u8; 2] = [0x1e, 0x43];
pub(crate) const TABLE: [u8; 2] = [0x1e, 0x84];
pub(crate) const MOTOR: [u8; 2] = [0x1e, 0x01];
pub(crate) const MOVE: [u8; 2] = [0x1e, 0x05];

const LAMP_OFF: u8 = 0x00;
const FLATBED_LAMP: u8 = 0x01;
/// `1e 11` for a flatbed colour pass.
const FLATBED_PASS: u8 = 0x04;
/// The starting analog offsets the driver sends before the scanner calibrates its own.
const INITIAL_OFFSETS: [u8; 12] = [0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0, 0, 0, 0, 0, 0];
/// `1e e1` parameters, the same in every session captured.
const SETUP_PARAMS: [u8; 6] = [0x0b, 0x00, 0x25, 0x61, 0x00, 0x00];
/// `1e 43` per channel; the same in Epson's and SilverFast's sessions.
const GAIN_VALUES: [u8; 6] = [0x84, 0x7d, 0x77, 0x7e, 0xdf, 0x7f];
pub(crate) const GAIN_NEUTRAL: [u8; 6] = [0x00, 0x80, 0x00, 0x80, 0x00, 0x80];
/// x of the bed's left edge, in units of 533⅓ per inch; full-width reads start here.
const X_START: u16 = 0x8c;
/// The masked strip of the CCD read for its black level.
const MASKED_STRIP: (u16, u16, u16) = (0x15, 0x25, 4);
pub(crate) const WHITE_LINES: u16 = 32;
const DARK_LINES: u16 = 16;
/// The white read moves the carriage over the calibration strip at this period.
pub(crate) const WHITE_MOTOR: [u8; 12] = [0x33, 0, 0, 0, 0x03, 0, 0, 0, 0, 0, 0x08, 0];
pub(crate) const MAX_BLOCK: usize = 65536;
/// `1e 41` byte 8 bit 0: 8-bit samples. WIA's reads all set it; calibration reads and
/// SilverFast's 48-bit scans clear it and get 16-bit samples.
const EIGHT_BIT: u8 = 0x01;

/// Constants of one hardware resolution, from captures of Epson's driver.
#[derive(Clone, Copy, Debug)]
struct Mode {
    hw_dpi: u32,
    /// `1e 41` bytes 9–10 give 2400 × 1 / divisor dpi.
    divisor: u8,
    /// `1e 41` byte 8 of the 8-bit image read: the horizontal subsampling, bit 0 set.
    subsampling: u8,
    /// Pixels per line of the image read, after subsampling.
    width: u32,
    /// End of the full-width read in units of 533⅓ per inch (it starts at [`X_START`]).
    x_end: u16,
    /// `1e 22`, `1e 21` and `1e 46` lead with this; probably the exposure per line.
    exposure: u16,
    /// Step period during the white read.
    white_step: u16,
    /// Acceleration of the image read: first and final step periods.
    scan_ramp: (u16, u16),
    /// `1e 01` bytes 4–11 of the image read: the lead-in and the motor mode.
    scan_motor: [u8; 8],
    /// `1e 41` byte 16 (and `1e 57` byte 33) for a short move; 0 otherwise.
    short_move_flag: u8,
}

const MODES: [Mode; 2] = [
    Mode {
        hw_dpi: 200,
        divisor: 12,
        subsampling: 0xb1,
        width: 1920,
        x_end: 0x148c,
        exposure: 0x0f00,
        white_step: 160,
        scan_ramp: (2500, 320),
        scan_motor: [0x78, 0x00, 0xff, 0x01, 0xff, 0x01, 0x00, 0x00],
        short_move_flag: 0,
    },
    Mode {
        hw_dpi: 400,
        divisor: 6,
        subsampling: 0xa1,
        width: 3456,
        x_end: 0x128c,
        exposure: 0x0cc0,
        white_step: 136,
        scan_ramp: (1249, 272),
        scan_motor: [0x7e, 0x00, 0xff, 0x00, 0xff, 0x00, 0x02, 0x00],
        short_move_flag: 0x1e,
    },
];

/// The carriage move's acceleration (first and final periods, length) and `1e 01` bytes 4–11.
fn move_motor(profile: MoveProfile) -> ((u16, u16, usize), [u8; 8]) {
    match profile {
        MoveProfile::Short => ((2500, 250, 256), [0, 0, 0xff, 0x00, 0xff, 0x00, 0x10, 0x00]),
        MoveProfile::Long => ((2500, 160, 512), [0, 0, 0xff, 0x01, 0xff, 0x01, 0x10, 0x00]),
    }
}

/// `1e 41` parameters: x range, lines, then bytes 6–21.
pub(crate) fn area_params(x0: u16, x1: u16, lines: u16, rest: [u8; 16]) -> Vec<u8> {
    let mut g = Vec::with_capacity(22);
    for v in [x0, x1, lines] {
        g.extend_from_slice(&v.to_le_bytes());
    }
    g.extend_from_slice(&rest);
    g
}

fn exposure_params(exposure: u16, dark: bool) -> [u8; 12] {
    let e = exposure.to_le_bytes();
    [e[0], e[1], 0, 0, 0, 0, 0, 0, 0, u8::from(dark), 0, 0]
}

fn gain(first: [u8; 6]) -> [u8; 12] {
    let mut g = [0u8; 12];
    g[..6].copy_from_slice(&first);
    g[6..].copy_from_slice(&GAIN_VALUES);
    g
}

/// Progress once calibration is done; the image read takes the rest.
pub(crate) const CALIBRATED: f32 = 0.15;

pub(crate) fn cancelled(progress: &dyn ProgressSink) -> Result<(), ScanError> {
    if progress.is_cancelled() {
        Err(ScanError::Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) fn upload(
    p: &mut Protocol<'_>,
    kind: u8,
    address: [u8; 4],
    data: &[u8],
) -> Result<(), ScanError> {
    p.command(TABLE, &[&tables::header(kind, address, data.len()), data])
}

/// A flatbed scan of one area, ready to run.
#[derive(Clone, Debug)]
pub struct FlatbedSession {
    mode: Mode,
    pub plan: FlatbedPlan,
    pub layout: RawLayout,
    pub bit_depth: BitDepth,
    area: ScanArea,
}

impl FlatbedSession {
    pub fn new(area: &ScanArea, dpi: u32, bit_depth: BitDepth) -> Result<Self, ScanError> {
        let plan = FlatbedPlan::new(area, dpi, u32::from(X_START))?;
        let mode = *MODES
            .iter()
            .find(|m| m.hw_dpi == plan.hw_dpi)
            .ok_or_else(|| {
                ScanError::Unsupported(format!(
                    "{dpi} dpi needs {} dpi hardware resolution; native scans support up to 400",
                    plan.hw_dpi
                ))
            })?;
        let units = f64::from(mode.x_end - X_START);
        let px_per_unit = f64::from(mode.width) / units;
        let crop_x = (f64::from(plan.x_units.saturating_sub(u32::from(X_START))) * px_per_unit)
            .round() as u32;
        let used = (f64::from(plan.width) * f64::from(plan.hw_dpi) / f64::from(dpi)).ceil() as u32;
        if plan.x_units < u32::from(X_START) || crop_x + used > mode.width {
            return Err(ScanError::InvalidRequest(format!(
                "area {area:?} is outside the bed"
            )));
        }
        Ok(Self {
            mode,
            layout: RawLayout {
                lines: plan.lines,
                width: mode.width,
                crop_x,
            },
            plan,
            bit_depth,
            area: *area,
        })
    }

    fn sixteen(&self) -> bool {
        self.bit_depth == BitDepth::Sixteen
    }

    /// Bytes per line of the image read.
    fn line_len(&self) -> usize {
        self.mode.width as usize * 3 * if self.sixteen() { 2 } else { 1 }
    }

    /// Bytes the image read returns.
    pub fn data_len(&self) -> usize {
        self.plan.lines as usize * self.line_len()
    }

    /// The `1e 57` record of the scan (read back by the next session's `1e 53`): resolution,
    /// then the area's position and size in pixels at that resolution.
    fn record(&self) -> [u8; 42] {
        let m = &self.mode;
        let px = |mm: f32| f64::from(mm) / f64::from(MM_PER_INCH) * f64::from(m.hw_dpi);
        let fields = [
            m.hw_dpi,
            m.hw_dpi,
            px(self.area.x_mm).round() as u32,
            px(self.area.y_mm).round() as u32,
            px(self.area.width_mm).round() as u32,
            px(self.area.height_mm) as u32,
            0x0813,
            0x0300 | u32::from(m.divisor),
            u32::from(self.short_flag()) << 8,
        ];
        let mut r = [0u8; 42];
        for (chunk, v) in r.as_chunks_mut::<4>().0.iter_mut().zip(fields) {
            *chunk = v.to_le_bytes();
        }
        r
    }

    fn short_flag(&self) -> u8 {
        match self.plan.move_profile {
            MoveProfile::Short => self.mode.short_move_flag,
            MoveProfile::Long => 0,
        }
    }

    /// Run the session on a scanner and return the raw image data (see [`Self::layout`]).
    pub fn run(&self, dev: &mut dyn Transport, poll_delay: Duration) -> Result<Vec<u8>, ScanError> {
        self.run_with(dev, poll_delay, &mut NoProgress)
    }

    /// [`Self::run`], reporting progress. A cancel before calibration stops the session there,
    /// with the carriage still home. Later, the session runs to the end (no way to abort a
    /// read is known, and the carriage must get home) and the data is dropped.
    pub fn run_with(
        &self,
        dev: &mut dyn Transport,
        poll_delay: Duration,
        progress: &mut dyn ProgressSink,
    ) -> Result<Vec<u8>, ScanError> {
        let m = self.mode;
        let mut p = Protocol::new(dev, poll_delay);

        // Open the device and identify it.
        p.query(OPEN, 1)?;
        p.wait_ready()?;
        p.query(LAST_SCAN, 42)?;
        p.command(SETUP, &[&SETUP_PARAMS])?;
        p.command(INIT, &[])?;
        let id = p.query(IDENTIFY, 28)?;
        log::info!("scanner: {}", String::from_utf8_lossy(&id).trim());
        p.query(STATE_A1, 1)?;
        p.query(STATE_65, 1)?;
        let gamma8 = if self.sixteen() {
            tables::linear8()
        } else {
            tables::gamma8()
        };
        for address in tables::GAMMA8_ADDRESS {
            upload(&mut p, tables::GAMMA8, address, &gamma8)?;
        }
        p.wait_ready()?;
        p.query(STATE_A1, 1)?;
        p.query(STATE_65, 1)?;
        p.query(LAST_SCAN, 42)?;
        p.command(RECORD_SCAN, &[&self.record()])?;
        p.query(STATE_A1, 1)?;
        p.command(LIGHT, &[&[FLATBED_LAMP]])?;
        let device = p.request(DEVICE_DATA, &[0x00], 10)?;
        log::debug!("device data {device:02x?}");
        p.query(POSITION, 4)?;

        // The scanner calibrates its analog offsets; apply the second result.
        p.command(OFFSETS, &[&INITIAL_OFFSETS])?;
        let e = m.exposure.to_le_bytes();
        let start = [0x04, 0x00, e[0], e[1], 0, 0, 0, 0, 0, 0, 0, 0];
        let mut offsets = Vec::new();
        for _ in 0..2 {
            p.command(START_OFFSETS, &[&start])?;
            p.poll(OFFSETS_BUSY, 1, |r| r == [0x01])?;
            offsets = p.query(AUTO_OFFSETS, 12)?;
        }
        if offsets.len() != 12 {
            return Err(ScanError::Backend(format!(
                "offset calibration replied {offsets:02x?}"
            )));
        }
        p.command(PASS, &[&[FLATBED_PASS]])?;
        p.command(OFFSETS, &[&offsets])?;
        let mut exposure_21 = [0u8; 26];
        exposure_21[..2].copy_from_slice(&e);
        p.command(EXPOSURE_21, &[&exposure_21])?;
        upload(
            &mut p,
            tables::TYPE7,
            tables::TYPE7_ADDRESS,
            &tables::type7(),
        )?;

        progress.progress(0.05);
        cancelled(progress)?;

        // Black level of the masked strip.
        let (x0, x1, lines) = MASKED_STRIP;
        let strip = area_params(
            x0,
            x1,
            lines,
            [
                0,
                0,
                0x00,
                0x01,
                m.divisor,
                0x00,
                0,
                0,
                0,
                0,
                0x80,
                0x00,
                lines as u8,
                0,
                0,
                0,
            ],
        );
        p.command(EXPOSURE, &[&exposure_params(m.exposure, false)])?;
        p.command(GEOMETRY, &[&strip])?;
        p.command(LEVELS, &[&[0u8; 18]])?;
        p.command(GAIN, &[&gain(GAIN_NEUTRAL)])?;
        let strip = p.read_data(usize::from(x1 - x0) * usize::from(lines) * 6)?;
        let darks = calibration::white_read_darks(&strip);

        // White read of the calibration strip at 1,600 dpi.
        let cal_width = usize::from(m.x_end - X_START);
        p.wait_ready()?;
        p.command(EXPOSURE, &[&exposure_params(m.exposure, false)])?;
        p.command(
            GEOMETRY,
            &[&area_params(
                X_START,
                m.x_end,
                WHITE_LINES,
                [
                    0,
                    0,
                    0x00,
                    0x02,
                    0x03,
                    0x40,
                    0,
                    0,
                    0,
                    0,
                    0x80,
                    0x80,
                    WHITE_LINES as u8,
                    0,
                    0,
                    0,
                ],
            )],
        )?;
        p.command(LEVELS, &[&calibration::white_read_levels(darks)])?;
        p.command(GAIN, &[&gain(GAIN_NEUTRAL)])?;
        upload(
            &mut p,
            tables::MOTOR,
            tables::MOTOR_ADDRESS,
            &m.white_step.to_le_bytes(),
        )?;
        p.command(MOTOR, &[&WHITE_MOTOR])?;
        let white = p.read_data(cal_width * usize::from(WHITE_LINES) * 6)?;

        // Dark read with the lamp off.
        p.wait_ready()?;
        p.command(LIGHT, &[&[LAMP_OFF]])?;
        let dark_geometry = area_params(
            X_START,
            m.x_end,
            DARK_LINES,
            [
                0,
                0,
                0x00,
                0x01,
                m.divisor,
                0x00,
                0,
                0,
                0,
                0,
                0x80,
                0x00,
                DARK_LINES as u8,
                0,
                0,
                0,
            ],
        );
        p.command(EXPOSURE, &[&exposure_params(m.exposure, true)])?;
        p.command(GEOMETRY, &[&dark_geometry])?;
        p.command(LEVELS, &[&[0u8; 18]])?;
        p.command(GAIN, &[&gain(GAIN_NEUTRAL)])?;
        p.command(GEOMETRY, &[&dark_geometry])?;
        let dark = p.read_data(cal_width * usize::from(DARK_LINES) * 6)?;
        p.wait_ready()?;
        p.command(LIGHT, &[&[FLATBED_LAMP]])?;
        p.query(POSITION, 4)?;

        let shading = Shading::compute(&white, &dark, cal_width, darks, geometry::bed_units())
            .ok_or_else(|| {
                ScanError::Backend(format!(
                    "calibration reads of {} and {} bytes don't fit {cal_width} pixels",
                    white.len(),
                    dark.len()
                ))
            })?;
        log::info!(
            "calibration: offsets {offsets:02x?}, white-read darks {darks:?}, \
             white base {:?}, dark base {:?}",
            shading.white_base,
            shading.dark_base
        );

        progress.progress(CALIBRATED);

        // Tone curves, then move the carriage to the area.
        let gamma12 = if self.sixteen() {
            tables::linear12()
        } else {
            tables::gamma12()
        };
        for address in tables::GAMMA12_ADDRESS {
            upload(&mut p, tables::GAMMA12, address, &gamma12)?;
        }
        let ((start, end, len), motor) = move_motor(self.plan.move_profile);
        upload(
            &mut p,
            tables::MOTOR,
            tables::MOTOR_ADDRESS,
            &tables::motor_ramp(start, end, len),
        )?;
        let mut params = [0u8; 12];
        params[..4].copy_from_slice(&self.plan.move_steps.to_le_bytes());
        params[4..].copy_from_slice(&motor);
        p.command(MOTOR, &[&params])?;
        p.command(MOVE, &[])?;
        p.wait_ready()?;

        // The image read.
        let lines = u16::try_from(self.plan.lines)
            .map_err(|_| ScanError::InvalidRequest("area too tall".into()))?;
        let lines_per_block = (MAX_BLOCK / self.line_len()).min(255) as u8;
        let subsampling = if self.sixteen() {
            m.subsampling & !EIGHT_BIT
        } else {
            m.subsampling
        };
        p.command(EXPOSURE, &[&exposure_params(m.exposure, false)])?;
        p.command(
            GEOMETRY,
            &[&area_params(
                X_START,
                m.x_end,
                lines,
                [
                    0,
                    0,
                    subsampling,
                    0x01,
                    m.divisor,
                    0x82,
                    0x80,
                    0,
                    0,
                    0x60,
                    self.short_flag(),
                    0xa0,
                    lines_per_block,
                    0,
                    0,
                    0,
                ],
            )],
        )?;
        p.command(LEVELS, &[&shading.levels()])?;
        p.command(GAIN, &[&gain(GAIN_VALUES)])?;
        upload(
            &mut p,
            tables::SHADING,
            tables::SHADING_ADDRESS,
            &shading.table,
        )?;
        let (start, end) = m.scan_ramp;
        upload(
            &mut p,
            tables::MOTOR,
            tables::MOTOR_ADDRESS,
            &tables::motor_ramp(start, end, 512),
        )?;
        let mut params = [0u8; 12];
        params[..4].copy_from_slice(&self.plan.scan_steps.to_le_bytes());
        params[4..].copy_from_slice(&m.scan_motor);
        p.command(MOTOR, &[&params])?;
        let len = self.data_len();
        let data = p.read_data_with(len, &mut |n| {
            progress.progress(CALIBRATED + (1.0 - CALIBRATED) * n as f32 / len as f32);
        })?;

        p.wait_ready()?;
        p.query(STATE_A1, 1)?;
        p.query(STATE_65, 1)?;
        p.query(OPEN, 1)?;
        cancelled(progress)?;
        Ok(data)
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

    #[test]
    fn runs_on_simulator() {
        for (dpi, y) in [(75, 0.0), (75, 60.0), (300, 0.0), (300, 60.0)] {
            let session =
                FlatbedSession::new(&area(40.0, y, 30.0, 20.0), dpi, BitDepth::Eight).unwrap();
            let mut sim = SimulatedScanner::default();
            let data = session.run(&mut sim, Duration::ZERO).unwrap();
            assert_eq!(data.len(), session.data_len());
            // The simulator's lit flatbed reads back as a bright, flat image.
            assert!(data.iter().all(|&v| v > 100), "{dpi} dpi, y {y}");
        }
        let session =
            FlatbedSession::new(&area(40.0, 60.0, 30.0, 20.0), 300, BitDepth::Sixteen).unwrap();
        let mut sim = SimulatedScanner::default();
        let data = session.run(&mut sim, Duration::ZERO).unwrap();
        assert_eq!(data.len(), 2 * session.plan.lines as usize * 3456 * 3);
        assert!(
            data.as_chunks::<2>()
                .0
                .iter()
                .all(|v| u16::from_le_bytes(*v) > 20000)
        );
        // The image read asks for 16 bits, with flat tone curves.
        let geometry = sim.writes.iter().rev().find(|w| w.len() == 22).unwrap();
        assert_eq!(geometry[8], 0xa0);
        assert!(sim.writes.contains(&tables::linear12()));
    }

    #[test]
    fn rejects_unsupported_requests() {
        let new = |a, dpi| FlatbedSession::new(&a, dpi, BitDepth::Eight);
        assert!(new(area(0.0, 0.0, 10.0, 10.0), 600).is_err());
        // The full-width read is 243 mm wide at 200 dpi, 219 mm at 400.
        assert!(new(area(230.0, 0.0, 30.0, 10.0), 75).is_err());
        assert!(new(area(200.0, 0.0, 30.0, 10.0), 300).is_err());
    }

    #[test]
    fn records_the_scan() {
        let s = FlatbedSession::new(&area(0.0, 30.0, 215.9, 20.0), 300, BitDepth::Eight).unwrap();
        let r = s.record();
        // Capture 29: 400 dpi, y 472, 3,400 × 314 px.
        assert_eq!(r[..8], [0x90, 0x01, 0, 0, 0x90, 0x01, 0, 0]);
        assert_eq!(
            r[12..24],
            [0xd8, 0x01, 0, 0, 0x48, 0x0d, 0, 0, 0x3a, 0x01, 0, 0]
        );
        assert_eq!(r[24..34], [0x13, 0x08, 0, 0, 0x06, 0x03, 0, 0, 0, 0]);
    }

    /// The commands a session sends, against Epson's driver in the full-width captures. Needs
    /// `target/captures` (not committed): `cargo test -p scan-epson -- --ignored`.
    #[test]
    #[ignore]
    fn matches_driver_captures() {
        use crate::trace::Direction;
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/captures/");
        for (file, dpi, y) in [
            ("24_75dpi_fullwidth.pcap", 75, 0.0),
            ("28_75dpi_fullwidth_y30.pcap", 75, 30.0),
            ("25_300dpi_fullwidth.pcap", 300, 0.0),
            ("29_300dpi_fullwidth_y30.pcap", 300, 30.0),
        ] {
            let packets =
                crate::pcap::parse(&std::fs::read(format!("{dir}{file}")).unwrap()).unwrap();
            let device = crate::trace::busiest_device(&packets).unwrap();
            let skip = |w: &[u8]| w == [0x1b, 0x03] || w == [0x1e, 0x04];
            let captured: Vec<Vec<u8>> = crate::trace::transfers(&packets, device)
                .into_iter()
                .filter(|t| t.kind == crate::pcap::TransferType::Bulk)
                .filter(|t| t.direction == Direction::Out && !skip(&t.data))
                .map(|t| t.data)
                .collect();
            let session =
                FlatbedSession::new(&area(0.0, y, 215.9, 20.0), dpi, BitDepth::Eight).unwrap();
            let mut sim = SimulatedScanner::default();
            session.run(&mut sim, Duration::ZERO).unwrap();
            let ours: Vec<Vec<u8>> = sim.writes.into_iter().filter(|w| !skip(w)).collect();

            // Compare from the scan record on; the driver repeats its opening commands.
            let from = |w: &[Vec<u8>]| w.iter().position(|c| c[..] == RECORD_SCAN).unwrap();
            let (theirs, ours) = (&captured[from(&captured)..], &ours[from(&ours)..]);
            assert_eq!(theirs.len(), ours.len(), "{file}: command count");
            for (i, (t, o)) in theirs.iter().zip(ours).enumerate() {
                if t == o {
                    continue;
                }
                let cmd = theirs[..i]
                    .iter()
                    .rev()
                    .find(|c| c.len() == 2 && matches!(c[0], 0x1b | 0x1e))
                    .unwrap();
                let header = &theirs[i - 1];
                match &cmd[..] {
                    // Calibration differs per session; the record and moves are checked by the
                    // geometry tests.
                    c if c == RECORD_SCAN || c == OFFSETS || c == LEVELS => {}
                    c if c == MOTOR => assert_eq!(t[4..], o[4..], "{file}: motor mode {i}"),
                    c if c == TABLE && header[0] == tables::SHADING => {}
                    c if c == TABLE && header[0] == tables::MOTOR => {
                        let worst = t
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .zip(o.as_chunks::<2>().0)
                            .map(|(a, b)| u16::from_le_bytes(*a).abs_diff(u16::from_le_bytes(*b)))
                            .max();
                        assert!(worst <= Some(1), "{file}: motor table {i} off by {worst:?}");
                    }
                    c if c == TABLE && header[0] == tables::GAMMA12 => {
                        assert_eq!(t.len(), o.len());
                    }
                    _ => panic!("{file}: write {i} after {cmd:02x?}: {t:02x?} vs {o:02x?}"),
                }
            }
        }
    }
}
