//! Flatbed scans built from a captured session of Epson's driver.
//!
//! A full-width capture supplies everything we don't generate yet (lamp, gamma, motor tables).
//! For a new request only the final read changes: the carriage move before it, its line
//! count and its motor steps. The read keeps the template's full width, and the area is
//! cropped from it when decoding. Calibration is redone from the scanner's own reads
//! (see [`crate::calibration`]) unless the recorded values are asked for.

use std::ops::Range;
use std::time::Duration;

use scan_core::{ScanArea, ScanError};

use crate::calibration::{self, Shading};
use crate::data::BlockAssembler;
use crate::geometry::{self, FlatbedPlan, MoveProfile};
use crate::pcap::TransferType;
use crate::replay::{ReplayReport, replay};
use crate::trace::{Direction, Transfer};
use crate::transport::Transport;

const SET_GEOMETRY: [u8; 2] = [0x1e, 0x41];
const MOTOR: [u8; 2] = [0x1e, 0x01];
const MOVE: [u8; 2] = [0x1e, 0x05];
const READ: [u8; 2] = [0x1e, 0x47];
const OFFSETS: [u8; 2] = [0x1e, 0x31];
const AUTO_OFFSETS: [u8; 2] = [0x1e, 0x92];
const LEVELS: [u8; 2] = [0x1e, 0x42];
const TABLE: [u8; 2] = [0x1e, 0x84];
const SHADING_TABLE: u8 = 6;

/// A captured flatbed session, with the positions of the commands a new request patches.
#[derive(Clone, Debug)]
pub struct Template {
    transfers: Vec<Transfer>,
    /// The final `1e 47`.
    read: usize,
    /// Payloads of the final read's `1e 41` and `1e 01`, and of the carriage move's `1e 01`.
    geometry: usize,
    scan_length: usize,
    carriage_move: usize,
    /// Where the session's data read resumes with the driver's post-scan commands.
    after: usize,
    pub hw_dpi: u32,
    pub move_profile: MoveProfile,
    /// x range of the read in the scanner's units.
    pub x_start: u32,
    pub x_end: u32,
    /// Pixels per line of the read (8-bit RGB).
    pub width: u32,
    calibration: Option<CalibrationPoints>,
}

/// Where the driver calibrates in a template, so a scan can redo it. Indices are transfers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibrationPoints {
    /// The scanner's reply to the last `1e 92` (its own analog offset calibration), and the
    /// `1e 31` parameters that apply it.
    offsets_reply: usize,
    offsets: usize,
    /// The ends of the masked-strip, white and dark reads (the next OUT after each).
    dark_ref_end: usize,
    white_end: usize,
    dark_end: usize,
    /// `1e 42` parameters for the white read and for the image read.
    white_levels: usize,
    levels: usize,
    /// The shading table's data.
    table: usize,
    /// Pixels per line of the calibration reads.
    width: usize,
}

/// Where a scan's calibration comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Calibration {
    /// Computed from this session's reads.
    #[default]
    Live,
    /// The template's recorded values.
    Recorded,
    /// No shading correction: the recorded table, flattened to each channel's mean.
    Flat,
}

/// The transfers to replay for one request, split around the image read.
#[derive(Clone, Debug)]
pub struct Script {
    /// Up to and including the final `1e 47`.
    pub before: Vec<Transfer>,
    /// Image bytes the read returns.
    pub data_len: usize,
    /// The driver's commands after the read.
    pub after: Vec<Transfer>,
    pub plan: FlatbedPlan,
    pub layout: RawLayout,
    pub calibration: Option<CalibrationPoints>,
}

/// Where the requested area sits in the raw read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawLayout {
    pub lines: u32,
    pub width: u32,
    /// First raw column of the area.
    pub crop_x: u32,
}

fn bad(why: impl std::fmt::Display) -> ScanError {
    ScanError::Backend(format!("unusable template: {why}"))
}

/// Find the driver's calibration before the image read at `read`: three reads (masked strip,
/// white, dark), the offsets and levels that follow from them, and the shading table.
fn calibration_points(t: &[Transfer], read: usize) -> Option<CalibrationPoints> {
    let is_out = |i: usize, cmd: &[u8]| t[i].direction == Direction::Out && t[i].data == cmd;
    let next_out = |i: usize| {
        (i + 1..t.len())
            .find(|&j| t[j].direction == Direction::Out)
            .unwrap_or(t.len())
    };
    let last_before = |end: usize, cmd: &[u8]| (0..end).rev().find(|&i| is_out(i, cmd));
    let reads: Vec<usize> = (0..read).filter(|&i| is_out(i, &READ)).collect();
    let [dark_ref, white, dark] = reads[..] else {
        return None;
    };
    let auto = last_before(dark_ref, &AUTO_OFFSETS)?;
    let offsets_reply =
        Some(auto + 1).filter(|&i| t.get(i).is_some_and(|r| r.direction == Direction::In))?;
    let offsets = next_out((auto..dark_ref).find(|&i| is_out(i, &OFFSETS))?);
    let white_levels = next_out(last_before(white, &LEVELS)?);
    let levels = next_out(last_before(read, &LEVELS)?);
    let table = (dark..read)
        .rev()
        .filter(|&i| is_out(i, &TABLE))
        .map(next_out)
        .find(|&h| {
            t.get(h)
                .is_some_and(|h| h.data.first() == Some(&SHADING_TABLE))
        })
        .map(next_out)?;
    let geometry = next_out(last_before(white, &SET_GEOMETRY)?);
    let lines = t
        .get(geometry)
        .filter(|g| g.data.len() == 22)
        .map(|g| u16_at(&g.data, 4))?;
    let mut blocks = BlockAssembler::default();
    let white_bytes: usize = t[white + 1..next_out(white)]
        .iter()
        .filter_map(|x| blocks.push(&x.data))
        .map(|b| b.len())
        .sum();
    let width = white_bytes / (lines.max(1) as usize * 6);
    let sizes_ok = width > 0
        && t[offsets_reply].data.len() == t.get(offsets)?.data.len()
        && t.get(white_levels)?.data.len() == 18
        && t.get(levels)?.data.len() == 18
        && t.get(table)?.data.len() == width * 6;
    sizes_ok.then_some(CalibrationPoints {
        offsets_reply,
        offsets,
        dark_ref_end: next_out(dark_ref),
        white_end: next_out(white),
        dark_end: next_out(dark),
        white_levels,
        levels,
        table,
        width,
    })
}

/// The reply the scanner gave at transfer `index` of a replay starting at 0.
fn reply_at(report: &ReplayReport, transfers: &[Transfer], index: usize) -> Vec<u8> {
    report
        .mismatches
        .iter()
        .find(|m| m.index == index)
        .map_or_else(|| transfers[index].data.clone(), |m| m.actual.clone())
}

/// Replace each entry of a shading table with its channel's mean.
fn flatten(table: &mut [u8]) {
    let mut sum = [0u64; 3];
    for (i, b) in table.as_chunks::<2>().0.iter().enumerate() {
        sum[i % 3] += u64::from(u16::from_le_bytes(*b));
    }
    let n = (table.len() as u64 / 6).max(1);
    for (i, b) in table.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        b.copy_from_slice(&((sum[i % 3] / n) as u16).to_le_bytes());
    }
}

fn u16_at(b: &[u8], i: usize) -> u32 {
    u32::from(b[i]) | u32::from(b[i + 1]) << 8
}

impl Template {
    pub fn from_transfers(transfers: &[Transfer]) -> Result<Self, ScanError> {
        let t: Vec<Transfer> = transfers
            .iter()
            .filter(|t| t.kind == TransferType::Bulk)
            .cloned()
            .collect();
        let is_out = |i: usize, cmd: &[u8]| t[i].direction == Direction::Out && t[i].data == cmd;
        let last_before = |end: usize, cmd: &[u8]| (0..end).rev().find(|&i| is_out(i, cmd));
        // A command's parameters are the next OUT transfer after it.
        let payload = |cmd: usize| {
            (cmd + 1..t.len())
                .find(|&i| t[i].direction == Direction::Out)
                .ok_or_else(|| bad("command without parameters"))
        };

        let read = last_before(t.len(), &READ).ok_or_else(|| bad("no image read"))?;
        let geometry = payload(last_before(read, &SET_GEOMETRY).ok_or_else(|| bad("no 1e 41"))?)?;
        let scan_length = payload(last_before(read, &MOTOR).ok_or_else(|| bad("no 1e 01"))?)?;
        let moved = last_before(read, &MOVE).ok_or_else(|| bad("no carriage move"))?;
        let carriage_move = payload(last_before(moved, &MOTOR).ok_or_else(|| bad("no move"))?)?;
        let next_out = |i: usize| {
            (i + 1..t.len())
                .find(|&j| t[j].direction == Direction::Out)
                .unwrap_or(t.len())
        };
        let after = next_out(read);

        let g = &t[geometry].data;
        if g.len() != 22 || t[scan_length].data.len() != 12 || t[carriage_move].data.len() != 12 {
            return Err(bad("unexpected parameter sizes"));
        }
        let hw_dpi = 2400 * u32::from(g[9]) / u32::from(g[10].max(1));
        let move_profile = match t[carriage_move].data[7] {
            0 => MoveProfile::Short,
            _ => MoveProfile::Long,
        };
        let (x_start, x_end, lines) = (u16_at(g, 0), u16_at(g, 2), u16_at(g, 4));

        let mut blocks = BlockAssembler::default();
        let data: usize = t[read + 1..after]
            .iter()
            .filter_map(|x| blocks.push(&x.data))
            .map(|b| b.len())
            .sum();
        let line_bytes = data / lines.max(1) as usize;
        if data == 0 || !data.is_multiple_of(lines as usize) || !line_bytes.is_multiple_of(3) {
            return Err(bad(format!(
                "{data} bytes don't split into {lines} RGB lines"
            )));
        }
        let calibration = calibration_points(&t, read);
        if calibration.is_none() {
            log::warn!("template: no calibration found; its recorded values will be used");
        }
        Ok(Self {
            calibration,
            read,
            geometry,
            scan_length,
            carriage_move,
            after,
            hw_dpi,
            move_profile,
            x_start,
            x_end,
            width: (line_bytes / 3) as u32,
            transfers: t,
        })
    }

    /// The transfers for a scan of `area` at `dpi`. `area` must lie within the template's
    /// width, and `dpi` must use the template's hardware resolution.
    pub fn script(&self, area: &ScanArea, dpi: u32) -> Result<Script, ScanError> {
        let x_origin = self.x_origin();
        let plan = FlatbedPlan::new(area, dpi, x_origin)?;
        if plan.hw_dpi != self.hw_dpi {
            return Err(ScanError::InvalidRequest(format!(
                "{dpi} dpi needs a {} dpi template; this one is {} dpi",
                plan.hw_dpi, self.hw_dpi
            )));
        }
        if plan.move_profile != self.move_profile {
            return Err(ScanError::InvalidRequest(format!(
                "y = {} mm needs a template with a {:?} carriage move; this one is {:?}",
                area.y_mm, plan.move_profile, self.move_profile
            )));
        }
        let px_per_unit = f64::from(self.width) / f64::from(self.x_end - self.x_start);
        let crop_x = (f64::from(plan.x_units.saturating_sub(self.x_start)) * px_per_unit).round();
        let used = (f64::from(plan.width) * f64::from(plan.hw_dpi) / f64::from(dpi)).ceil();
        if plan.x_units < self.x_start || crop_x + used > f64::from(self.width) {
            return Err(ScanError::InvalidRequest(format!(
                "area {area:?} is outside the template's width"
            )));
        }

        let mut before = self.transfers[..=self.read].to_vec();
        let g = &mut before[self.geometry].data;
        g[4..6].copy_from_slice(&(plan.lines as u16).to_le_bytes());
        before[self.scan_length].data[..4].copy_from_slice(&plan.scan_steps.to_le_bytes());
        before[self.carriage_move].data[..4].copy_from_slice(&plan.move_steps.to_le_bytes());
        Ok(Script {
            before,
            data_len: plan.lines as usize * self.width as usize * 3,
            after: self.transfers[self.after..].to_vec(),
            layout: RawLayout {
                lines: plan.lines,
                width: self.width,
                crop_x: crop_x as u32,
            },
            plan,
            calibration: self.calibration,
        })
    }

    /// Lines of the template's own image read.
    pub fn lines(&self) -> u32 {
        u16_at(&self.transfers[self.geometry].data, 4)
    }

    /// The template's own image data, as captured.
    pub fn captured_data(&self) -> Vec<u8> {
        let mut blocks = BlockAssembler::default();
        self.transfers[self.read + 1..self.after]
            .iter()
            .filter_map(|x| blocks.push(&x.data))
            .flatten()
            .collect()
    }

    /// Run `script` on a scanner and return the raw image data.
    pub fn run(
        script: &Script,
        dev: &mut dyn Transport,
        poll_delay: Duration,
        calibration: Calibration,
    ) -> Result<Vec<u8>, ScanError> {
        let threshold = 512;
        let mut stage = |before: &[Transfer], range: Range<usize>| {
            let report = replay(&before[range.clone()], dev, threshold, poll_delay);
            if let Some((i, e)) = report.error {
                return Err(ScanError::Backend(format!(
                    "scan setup failed at transfer {}: {e}",
                    range.start + i
                )));
            }
            for m in &report.mismatches {
                log::debug!(
                    "reply {} differs: {:02x?} vs {:02x?}",
                    range.start + m.index,
                    m.actual,
                    m.expected
                );
            }
            Ok(report)
        };
        let mut before = script.before.clone();
        match (calibration, script.calibration) {
            (Calibration::Live, Some(p)) => {
                let report = stage(&before, 0..p.offsets_reply + 1)?;
                before[p.offsets].data = reply_at(&report, &before, p.offsets_reply);
                let report = stage(&before, p.offsets_reply + 1..p.dark_ref_end)?;
                let darks = calibration::white_read_darks(&report.data);
                before[p.white_levels].data = calibration::white_read_levels(darks).to_vec();
                let white = stage(&before, p.dark_ref_end..p.white_end)?.data;
                let dark = stage(&before, p.white_end..p.dark_end)?.data;
                let shading = Shading::compute(&white, &dark, p.width, darks).ok_or_else(|| {
                    ScanError::Backend(format!(
                        "calibration reads of {} and {} bytes don't fit {} pixels",
                        white.len(),
                        dark.len(),
                        p.width
                    ))
                })?;
                log::info!(
                    "calibration: offsets {:02x?}, white-read darks {darks:?}, \
                     white base {:?}, dark base {:?}",
                    before[p.offsets].data,
                    shading.white_base,
                    shading.dark_base
                );
                before[p.levels].data = shading.levels().to_vec();
                before[p.table].data = shading.table;
                stage(&before, p.dark_end..before.len())?;
            }
            (Calibration::Flat, Some(p)) => {
                flatten(&mut before[p.table].data);
                stage(&before, 0..before.len())?;
            }
            (Calibration::Recorded, _) | (_, None) => {
                stage(&before, 0..before.len())?;
            }
        }

        let mut data = Vec::with_capacity(script.data_len);
        let mut blocks = BlockAssembler::default();
        let mut buf = vec![0u8; 65536];
        while data.len() < script.data_len {
            let n = dev.read(&mut buf)?;
            if n == 0 {
                return Err(ScanError::Backend("scanner sent an empty read".into()));
            }
            if let Some(block) = blocks.push(&buf[..n]) {
                data.extend_from_slice(&block);
            }
        }
        if data.len() != script.data_len {
            log::warn!(
                "read {} image bytes, expected {}",
                data.len(),
                script.data_len
            );
            data.truncate(script.data_len);
        }

        let report = replay(&script.after, dev, threshold, poll_delay);
        if let Some((i, e)) = report.error {
            log::warn!("post-scan command {i} failed: {e}");
        }
        Ok(data)
    }

    /// x unit of the bed's left edge at 533⅓ units per inch. The full-width captures start
    /// their read at the edge.
    fn x_origin(&self) -> u32 {
        (f64::from(self.x_start) * 1600.0 / 3.0 / geometry::units_per_inch(self.hw_dpi)).round()
            as u32
    }
}
