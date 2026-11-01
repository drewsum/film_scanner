//! Replay a captured session against a scanner: every bulk OUT transfer is written as
//! captured, and every bulk IN transfer is read with the captured length.
//!
//! Short replies (acks, status) are compared with the capture and differences are reported.
//! They can be legitimate: for example, the lamp may warm up in a different number of polls.

use scan_core::ScanError;

use crate::pcap::TransferType;
use crate::trace::{Direction, Transfer};
use crate::transport::Transport;

/// A reply that differed from the capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    /// Index into the replayed transfers.
    pub index: usize,
    pub expected: Vec<u8>,
    pub actual: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct ReplayReport {
    pub writes: usize,
    pub reads: usize,
    pub mismatches: Vec<Mismatch>,
    /// Concatenated payload of IN transfers of at least the data threshold.
    pub data: Vec<u8>,
    /// The transfer the replay stopped at, and why.
    pub error: Option<(usize, ScanError)>,
}

/// Status poll; bit 0x40 of the first reply byte is set while the scanner is busy.
const STATUS: [u8; 2] = [0x1b, 0x03];
const STATUS_BUSY: u8 = 0x40;
/// Lamp poll; replies 02 while warming up and 01 when ready.
const LAMP: [u8; 2] = [0x1e, 0x04];
const LAMP_READY: [u8; 1] = [0x01];
const MAX_POLLS: usize = 400;

/// Whether a poll reply means the scanner has reached the state seen in the capture.
fn poll_reached(command: &[u8], expected: &[u8], actual: &[u8]) -> bool {
    match command {
        c if c == STATUS => {
            let busy = |r: &[u8]| r.first().is_some_and(|b| b & STATUS_BUSY != 0);
            !actual.is_empty() && busy(expected) == busy(actual)
        }
        // A lamp that is already warm satisfies the capture's warm-up polls too.
        c if c == LAMP => actual == expected || actual == LAMP_READY,
        _ => expected == actual,
    }
}

/// Replay the bulk transfers of `transfers`. IN transfers of `data_threshold` bytes or more
/// are image data: they are collected rather than compared.
///
/// Status and lamp polls are repeated, `poll_delay` apart, until the scanner reaches the
/// captured state, since the replay runs at a different pace from the original session.
pub fn replay(
    transfers: &[Transfer],
    transport: &mut dyn Transport,
    data_threshold: usize,
    poll_delay: std::time::Duration,
) -> ReplayReport {
    let bulk: Vec<(usize, &Transfer)> = transfers
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == TransferType::Bulk)
        .collect();
    let mut report = ReplayReport::default();
    let mut i = 0;
    while i < bulk.len() {
        let (index, t) = bulk[i];
        let reply = bulk
            .get(i + 1)
            .filter(|(_, r)| r.direction == Direction::In && r.data.len() < data_threshold);
        let result = match reply {
            Some(&(_, r))
                if t.direction == Direction::Out && (t.data == STATUS || t.data == LAMP) =>
            {
                i += 1;
                poll(t, r, transport, poll_delay, &mut report)
            }
            _ => replay_one(index, t, transport, data_threshold, &mut report),
        };
        if let Err(e) = result {
            report.error = Some((index, e));
            break;
        }
        i += 1;
    }
    report
}

fn poll(
    command: &Transfer,
    expected: &Transfer,
    transport: &mut dyn Transport,
    delay: std::time::Duration,
    report: &mut ReplayReport,
) -> Result<(), ScanError> {
    let mut buf = vec![0u8; expected.data.len().max(2)];
    for attempt in 0..MAX_POLLS {
        if attempt > 0 {
            std::thread::sleep(delay);
        }
        transport.write(&command.data)?;
        report.writes += 1;
        let n = transport.read(&mut buf)?;
        report.reads += 1;
        if poll_reached(&command.data, &expected.data, &buf[..n]) {
            return Ok(());
        }
    }
    Err(ScanError::Backend(format!(
        "scanner never reached {:02x?} for poll {:02x?}",
        expected.data, command.data
    )))
}

fn replay_one(
    index: usize,
    t: &Transfer,
    transport: &mut dyn Transport,
    data_threshold: usize,
    report: &mut ReplayReport,
) -> Result<(), ScanError> {
    match t.direction {
        Direction::Out => {
            transport.write(&t.data)?;
            report.writes += 1;
        }
        Direction::In => {
            let mut buf = vec![0u8; t.data.len()];
            let n = transport.read(&mut buf)?;
            buf.truncate(n);
            report.reads += 1;
            if t.data.len() >= data_threshold {
                if n < t.data.len() {
                    // Keep reading so later transfers stay in step.
                    let mut rest = vec![0u8; t.data.len() - n];
                    transport.read_exact(&mut rest)?;
                    buf.extend_from_slice(&rest);
                }
                report.data.extend_from_slice(&buf);
            } else if buf != t.data {
                report.mismatches.push(Mismatch {
                    index,
                    expected: t.data.clone(),
                    actual: buf,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Exchange, ScriptedTransport};

    fn t(direction: Direction, data: &[u8]) -> Transfer {
        Transfer {
            timestamp: 0.0,
            direction,
            endpoint: if direction == Direction::In {
                0x81
            } else {
                0x02
            },
            kind: TransferType::Bulk,
            status: 0,
            data: data.to_vec(),
        }
    }

    #[test]
    fn replays_and_collects_data() {
        let capture = [
            t(Direction::Out, &[0x1e, 0x53]),
            t(Direction::In, &[0x64]),
            t(Direction::Out, &[0x1e, 0x47]),
            t(Direction::In, &[7; 4]),
            t(Direction::In, &[0x02, 0x00]),
        ];
        let mut dev = ScriptedTransport::new([
            Exchange::new(&[0x1e, 0x53], &[&[0x65]]),
            Exchange::new(&[0x1e, 0x47], &[&[9, 9], &[9, 9], &[0x02, 0x00]]),
        ]);
        let report = replay(&capture, &mut dev, 4, std::time::Duration::ZERO);
        assert!(report.error.is_none());
        assert_eq!((report.writes, report.reads), (2, 3));
        assert_eq!(report.data, [9; 4]);
        assert_eq!(
            report.mismatches,
            [Mismatch {
                index: 1,
                expected: vec![0x64],
                actual: vec![0x65]
            }]
        );
        assert!(dev.finished());
    }

    #[test]
    fn polls_until_scanner_catches_up() {
        let capture = [
            t(Direction::Out, &[0x1e, 0x04]),
            t(Direction::In, &[0x01]),
            // Warming in the capture, already warm on replay.
            t(Direction::Out, &[0x1e, 0x04]),
            t(Direction::In, &[0x02]),
            t(Direction::Out, &[0x1b, 0x03]),
            t(Direction::In, &[0x18, 0x81]),
        ];
        let mut dev = ScriptedTransport::new([
            Exchange::new(&[0x1e, 0x04], &[&[0x02]]),
            Exchange::new(&[0x1e, 0x04], &[&[0x01]]),
            Exchange::new(&[0x1e, 0x04], &[&[0x01]]),
            Exchange::new(&[0x1b, 0x03], &[&[0x58, 0x81]]),
            // Not busy, other bits different: good enough.
            Exchange::new(&[0x1b, 0x03], &[&[0x18, 0x01]]),
        ]);
        let report = replay(&capture, &mut dev, 4, std::time::Duration::ZERO);
        assert!(report.error.is_none(), "{:?}", report.error);
        assert!(report.mismatches.is_empty());
        assert!(dev.finished());
    }
}
