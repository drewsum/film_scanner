//! Flatbed scans built from a captured session of Epson's driver.
//!
//! A full-width capture supplies everything we don't generate yet (calibration, lamp, gamma).
//! For a new request only the final read changes: the carriage move before it, its line
//! count and its motor steps. The read keeps the template's full width, and the area is
//! cropped from it when decoding.

use std::time::Duration;

use scan_core::{ScanArea, ScanError};

use crate::data::BlockAssembler;
use crate::geometry::{self, FlatbedPlan, MoveProfile};
use crate::pcap::TransferType;
use crate::replay::replay;
use crate::trace::{Direction, Transfer};
use crate::transport::Transport;

const SET_GEOMETRY: [u8; 2] = [0x1e, 0x41];
const MOTOR: [u8; 2] = [0x1e, 0x01];
const MOVE: [u8; 2] = [0x1e, 0x05];
const READ: [u8; 2] = [0x1e, 0x47];

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
        let after = (read + 1..t.len())
            .find(|&i| t[i].direction == Direction::Out)
            .unwrap_or(t.len());

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
        Ok(Self {
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
    ) -> Result<Vec<u8>, ScanError> {
        let threshold = 512;
        let report = replay(&script.before, dev, threshold, poll_delay);
        if let Some((i, e)) = report.error {
            return Err(ScanError::Backend(format!(
                "scan setup failed at transfer {i}: {e}"
            )));
        }
        for m in &report.mismatches {
            log::debug!(
                "reply {} differs: {:02x?} vs {:02x?}",
                m.index,
                m.actual,
                m.expected
            );
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
