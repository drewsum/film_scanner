//! A simulated V500 that answers the protocol the way the scanner does (see
//! `docs/epson-v500`), for tests and dry runs without hardware.
//!
//! It acks commands, answers queries with replies seen in captures, and returns image data
//! for `1e 47` sized by the last `1e 41`: flat levels, bright with a lamp on and dark with it
//! off. Every write is logged.

use std::collections::VecDeque;

use scan_core::ScanError;

use crate::protocol::MAX_WRITE;
use crate::transport::Transport;

const ACK: u8 = 0x06;
const MAX_TRANSFER: usize = 65536;
/// 16-bit levels of the simulated CCD.
const DARK_LEVEL: u16 = 1300;
const WHITE_LEVEL: u16 = 30000;
const IMAGE_LEVEL: u8 = 200;

#[derive(Debug, Default)]
pub struct SimulatedScanner {
    /// Every write, in order.
    pub writes: Vec<Vec<u8>>,
    replies: VecDeque<Vec<u8>>,
    /// The command whose parameters are expected next, and how many writes they take.
    awaiting: Option<([u8; 2], usize)>,
    geometry: Vec<u8>,
    lamp_off: bool,
    /// What `1e 86` reports: the CCD's and the lamp's positions. A reverse move (`1e 01` mode
    /// bit `0x20`, then `1e 05`) takes the units `1e 25` selects home.
    pub position: [u16; 2],
    stage: u8,
    reverse: bool,
    /// The image read under way, made a block at a time as it is read.
    feed: Option<Feed>,
    /// Bytes of the table being uploaded (`1e 84`) still to come, in writes of their own.
    table_left: usize,
}

/// An image read's blocks still to come: each `per_block` lines of `line` and a trailer.
#[derive(Debug)]
struct Feed {
    line: Vec<u8>,
    lines: usize,
    per_block: usize,
}

impl Feed {
    /// The next block's transfers, if any are left.
    fn next(&mut self) -> Option<Vec<Vec<u8>>> {
        if self.lines == 0 {
            return None;
        }
        let n = self.per_block.min(self.lines);
        self.lines -= n;
        let block = self.line.repeat(n);
        let mut trailer = vec![0x02];
        trailer.extend_from_slice(&(block.len() as u32).to_le_bytes());
        trailer.extend_from_slice(&[0x00, 0x80, 0x00]);
        let mut chunks: Vec<Vec<u8>> = block.chunks(MAX_TRANSFER).map(<[u8]>::to_vec).collect();
        match chunks.last_mut() {
            Some(last) if last.len() + trailer.len() <= MAX_TRANSFER => last.extend(trailer),
            _ => chunks.push(trailer),
        }
        Some(chunks)
    }
}

/// Parameter writes a command takes; `None` for queries, which reply instead of acking.
fn param_writes(cmd: [u8; 2]) -> Option<usize> {
    match cmd {
        [0x1e, 0x84] => Some(2),
        [
            0x1e,
            0xe1 | 0x57 | 0xa2 | 0x87 | 0x31 | 0x46 | 0x11 | 0x21 | 0x22 | 0x41 | 0x25 | 0x8a,
        ]
        | [0x1e, 0x42 | 0x43 | 0x01] => Some(1),
        [0x1b, 0x40] | [0x1e, 0x05] => Some(0),
        _ => None,
    }
}

fn query_reply(cmd: [u8; 2]) -> Vec<u8> {
    match cmd {
        [0x1b, 0x03] => vec![0x18, 0x81],
        [0x1b, 0x13] => {
            let mut id = b"EPSON   GT-X770         1.00".to_vec();
            id.resize(28, b' ');
            id
        }
        [0x1e, 0x53] => vec![0; 42],
        [0x1e, 0x86] => vec![0; 4],
        [0x1e, 0x04] => vec![0x01],
        [0x1e, 0x92] => vec![0x86, 0, 0x86, 0, 0x8c, 0, 0, 0, 0x34, 0x30, 0x31, 0],
        _ => vec![0x00],
    }
}

impl SimulatedScanner {
    /// The data blocks for a read with the current geometry.
    fn image(&self) -> Option<Feed> {
        let g = &self.geometry;
        if g.len() != 22 {
            return None;
        }
        let u16_at = |i: usize| usize::from(u16::from_le_bytes([g[i], g[i + 1]]));
        let units = u16_at(2).saturating_sub(u16_at(0));
        // Horizontal subsampling (`1e 41` byte 8, upper bits): 8/3, 4/3 or 2.
        let width = match g[8] & !0x01 {
            0xb0 => units * 3 / 8,
            0xa0 => units * 3 / 4,
            0x10 => units / 2,
            _ => units,
        };
        let (lines, per_block) = (u16_at(4), usize::from(g[18]).max(1));
        // `1e 41` byte 8 bit 0 selects 8-bit samples; calibration reads are 16-bit.
        let sixteen_bit = g[8] & 0x01 == 0;
        let sample: Vec<u8> = match (sixteen_bit, self.lamp_off) {
            (false, _) => vec![IMAGE_LEVEL],
            (true, true) => DARK_LEVEL.to_le_bytes().to_vec(),
            (true, false) => WHITE_LEVEL.to_le_bytes().to_vec(),
        };
        Some(Feed {
            line: sample.repeat(width * 3),
            lines,
            per_block,
        })
    }
}

impl Transport for SimulatedScanner {
    fn write(&mut self, data: &[u8]) -> Result<(), ScanError> {
        if !self.replies.is_empty() || self.feed.as_ref().is_some_and(|f| f.lines > 0) {
            return Err(ScanError::Backend(format!(
                "write {data:02x?} while {} replies are unread",
                self.replies.len()
            )));
        }
        if data.len() > MAX_WRITE {
            return Err(ScanError::Backend(format!(
                "simulator: a write of {} bytes, past the {MAX_WRITE} the scanner takes",
                data.len()
            )));
        }
        self.writes.push(data.to_vec());
        if let Some((cmd, left)) = self.awaiting.take() {
            // A table: its header gives its length, which may come in several writes.
            if cmd == [0x1e, 0x84] {
                match (left, data) {
                    (2, [_, _, _, _, _, a, b, c]) => {
                        self.table_left =
                            usize::from(*a) | usize::from(*b) << 8 | usize::from(*c) << 16;
                        self.awaiting = Some((cmd, 1));
                        return Ok(());
                    }
                    (1, _) if data.len() < self.table_left => {
                        self.table_left -= data.len();
                        self.awaiting = Some((cmd, 1));
                        return Ok(());
                    }
                    _ => self.table_left = 0,
                }
            }
            match cmd {
                [0x1e, 0x41] => self.geometry = data.to_vec(),
                [0x1e, 0xa2] => self.lamp_off = matches!(data, [0x00 | 0x03 | 0x05]),
                [0x1e, 0x25] => self.stage = data.first().copied().unwrap_or(0),
                [0x1e, 0x01] => self.reverse = data.get(10).is_some_and(|m| m & 0x20 != 0),
                _ => {}
            }
            if left > 1 {
                self.awaiting = Some((cmd, left - 1));
            } else if cmd == [0x1e, 0x87] {
                self.replies
                    .push_back(vec![0x01, 0x54, 0xca, 0xff, 0xb4, 0, 0xf8, 0, 0xc0, 0]);
            } else {
                self.replies.push_back(vec![ACK]);
            }
            return Ok(());
        }
        let cmd: [u8; 2] = data
            .try_into()
            .map_err(|_| ScanError::Backend(format!("simulator: {data:02x?} is not a command")))?;
        if cmd == [0x1e, 0x47] {
            self.feed = self.image();
            return Ok(());
        }
        if cmd == [0x1e, 0x05] && self.reverse {
            match self.stage {
                0x00 => self.position[0] = 0,
                0x02 => self.position[1] = 0,
                _ => self.position = [0; 2],
            }
        }
        if cmd == [0x1e, 0x86] {
            let [ccd, lamp] = self.position.map(u16::to_le_bytes);
            self.replies.push_back([ccd, lamp].concat());
            return Ok(());
        }
        match param_writes(cmd) {
            Some(n) => {
                self.replies.push_back(vec![ACK]);
                if n > 0 {
                    self.awaiting = Some((cmd, n));
                }
            }
            None => self.replies.push_back(query_reply(cmd)),
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, ScanError> {
        if self.replies.is_empty()
            && let Some(block) = self.feed.as_mut().and_then(Feed::next)
        {
            self.replies.extend(block);
        }
        let Some(mut reply) = self.replies.pop_front() else {
            return Err(ScanError::Backend(
                "simulator: read with nothing to send".into(),
            ));
        };
        let n = reply.len().min(buf.len());
        buf[..n].copy_from_slice(&reply[..n]);
        if n < reply.len() {
            self.replies.push_front(reply.split_off(n));
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Protocol;
    use std::time::Duration;

    #[test]
    fn acks_parameters_and_answers_queries() {
        let mut sim = SimulatedScanner::default();
        let mut p = Protocol::new(&mut sim, Duration::ZERO);
        p.command([0x1e, 0xa2], &[&[0x01]]).unwrap();
        assert_eq!(p.query([0x1e, 0x92], 12).unwrap().len(), 12);
        assert_eq!(p.request([0x1e, 0x87], &[0], 10).unwrap().len(), 10);
        p.wait_ready().unwrap();
        assert!(p.command([0x1e, 0x99], &[&[1]]).is_err());
    }

    #[test]
    fn reads_blocks_sized_by_geometry() {
        let mut sim = SimulatedScanner::default();
        let mut p = Protocol::new(&mut sim, Duration::ZERO);
        // 16 px, 4 lines, 16-bit, one block.
        let g = [
            0x15, 0, 0x25, 0, 4, 0, 0, 0, 0, 1, 12, 0, 0, 0, 0, 0, 0x80, 0, 4, 0, 0, 0,
        ];
        p.command([0x1e, 0x41], &[&g]).unwrap();
        let data = p.read_data(16 * 4 * 6).unwrap();
        assert_eq!(&data[..2], &WHITE_LEVEL.to_le_bytes());
        // 1,920 px of 8-bit RGB, 11 lines a block.
        let mut g = [0u8; 22];
        g[..6].copy_from_slice(&[0x8c, 0, 0x8c, 0x14, 30, 0]);
        g[8..12].copy_from_slice(&[0xb1, 1, 12, 0x82]);
        g[18] = 11;
        p.command([0x1e, 0x41], &[&g]).unwrap();
        assert_eq!(p.read_data(30 * 1920 * 3).unwrap().len(), 30 * 1920 * 3);
    }

    #[test]
    fn takes_long_tables_in_pieces() {
        // A whole film area's shading table at 6,400 dpi: 17,280 px × 6 bytes.
        let mut sim = SimulatedScanner::default();
        let table = vec![7u8; 17280 * 6];
        let mut p = Protocol::new(&mut sim, Duration::ZERO);
        crate::session::upload(&mut p, 6, [0, 8, 1, 2], &table).unwrap();
        p.wait_ready().unwrap();
        let pieces: Vec<usize> = sim.writes[2..].iter().map(Vec::len).collect();
        assert_eq!(pieces[..2], [MAX_WRITE, table.len() - MAX_WRITE]);
        // Whole, it is refused.
        let mut sim = SimulatedScanner::default();
        assert!(sim.write(&table).is_err());
    }
}
