//! Turn raw USB packets into the host/device conversation of one scanner, and print it.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::pcap::{TransferType, UsbPacket};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
}

/// One completed exchange of data on an endpoint.
#[derive(Clone, Debug, PartialEq)]
pub struct Transfer {
    pub timestamp: f64,
    pub direction: Direction,
    pub endpoint: u8,
    pub kind: TransferType,
    pub status: u32,
    pub data: Vec<u8>,
}

/// `(bus, device)` address of a USB device in a capture.
pub type Address = (u16, u16);

/// The device that moved the most bulk data, which in a scanner capture is the scanner.
pub fn busiest_device(packets: &[UsbPacket]) -> Option<Address> {
    let mut bytes: HashMap<Address, usize> = HashMap::new();
    for p in packets.iter().filter(|p| p.transfer == TransferType::Bulk) {
        *bytes.entry((p.bus, p.device)).or_default() += p.data.len();
    }
    bytes.into_iter().max_by_key(|&(_, n)| n).map(|(a, _)| a)
}

/// Data transfers of one device. OUT data is taken from submissions and IN data from
/// completions; control setup packets are kept as OUT transfers on endpoint 0.
pub fn transfers(packets: &[UsbPacket], device: Address) -> Vec<Transfer> {
    packets
        .iter()
        .filter(|p| (p.bus, p.device) == device && !p.data.is_empty())
        .filter_map(|p| {
            let direction = match (p.transfer, p.completion, p.is_in_endpoint()) {
                (TransferType::Control, false, _) => Direction::Out,
                (TransferType::Control, true, _) => Direction::In,
                (_, false, false) => Direction::Out,
                (_, true, true) => Direction::In,
                _ => return None,
            };
            Some(Transfer {
                timestamp: p.timestamp,
                direction,
                endpoint: p.endpoint,
                kind: p.transfer,
                status: p.status,
                data: p.data.clone(),
            })
        })
        .collect()
}

/// Hex bytes followed by their printable ASCII, cut to `max` bytes.
pub fn hex_ascii(data: &[u8], max: usize) -> String {
    let shown = &data[..data.len().min(max)];
    let hex: Vec<String> = shown.iter().map(|b| format!("{b:02x}")).collect();
    let ascii: String = shown
        .iter()
        .map(|&b| {
            if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    let more = if data.len() > max {
        format!(" ... (+{} bytes)", data.len() - max)
    } else {
        String::new()
    };
    format!("{}  |{ascii}|{more}", hex.join(" "))
}

/// Options for [`format_log`].
#[derive(Clone, Copy, Debug)]
pub struct LogOptions {
    /// Bytes of each transfer to print.
    pub max_bytes: usize,
    /// IN transfers at least this long are bulk image data; consecutive ones are collapsed.
    pub data_threshold: usize,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            max_bytes: 48,
            data_threshold: 512,
        }
    }
}

/// One line per command and reply, with runs of large IN transfers summarised.
pub fn format_log(transfers: &[Transfer], opts: LogOptions) -> String {
    let mut out = String::new();
    let Some(start) = transfers.first().map(|t| t.timestamp) else {
        return out;
    };
    let mut i = 0;
    while i < transfers.len() {
        let t = &transfers[i];
        let dir = match t.direction {
            Direction::Out => "OUT",
            Direction::In => " IN",
        };
        let kind = match t.kind {
            TransferType::Control => "ctl",
            TransferType::Interrupt => "int",
            TransferType::Bulk => "blk",
            TransferType::Isochronous => "iso",
            TransferType::Other(_) => "???",
        };
        let status = if t.status != 0 {
            format!(" status={:#x}", t.status)
        } else {
            String::new()
        };
        let is_data =
            |t: &Transfer| t.direction == Direction::In && t.data.len() >= opts.data_threshold;
        if is_data(t) {
            let run_end = transfers[i..]
                .iter()
                .position(|n| !is_data(n) || n.endpoint != t.endpoint)
                .map_or(transfers.len(), |p| i + p);
            let total: usize = transfers[i..run_end].iter().map(|n| n.data.len()).sum();
            let count = run_end - i;
            let _ = writeln!(
                out,
                "{:>10.6} {dir} {kind} ep{:02x} DATA {count} transfer(s), {total} bytes \
                 (first {}){status}",
                t.timestamp - start,
                t.endpoint,
                t.data.len(),
            );
            let _ = writeln!(
                out,
                "{:>21}{}",
                "",
                hex_ascii(&t.data, opts.max_bytes.min(32))
            );
            i = run_end;
            continue;
        }
        let _ = writeln!(
            out,
            "{:>10.6} {dir} {kind} ep{:02x} {:>5}  {}{status}",
            t.timestamp - start,
            t.endpoint,
            t.data.len(),
            hex_ascii(&t.data, opts.max_bytes),
        );
        i += 1;
    }
    out
}

/// Concatenate the payload of every IN bulk transfer of at least `threshold` bytes, i.e. the
/// raw image stream of a scan, for offline decoding.
pub fn bulk_in_stream(transfers: &[Transfer], threshold: usize) -> Vec<u8> {
    transfers
        .iter()
        .filter(|t| t.direction == Direction::In && t.kind == TransferType::Bulk)
        .filter(|t| t.data.len() >= threshold)
        .flat_map(|t| t.data.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcap::{self, build};

    fn sample() -> Vec<UsbPacket> {
        let mut records = vec![
            build::bulk(false, 0x02, &[0x1b, 0x40]),
            build::bulk(true, 0x02, &[]),
            build::bulk(false, 0x81, &[]),
            build::bulk(true, 0x81, &[0x06]),
        ];
        for _ in 0..3 {
            records.push(build::bulk(true, 0x81, &[0xaa; 1024]));
        }
        records.push(build::bulk(true, 0x81, &[0x06]));
        let mut other = build::bulk(false, 0x02, &[0; 4]);
        other.device = 9;
        records.push(other);
        pcap::parse(&build::file(&records)).unwrap()
    }

    #[test]
    fn picks_scanner_and_pairs_directions() {
        let p = sample();
        assert_eq!(busiest_device(&p), Some((1, 3)));
        let t = transfers(&p, (1, 3));
        assert_eq!(t.len(), 6);
        assert_eq!(t[0].direction, Direction::Out);
        assert_eq!(t[0].data, [0x1b, 0x40]);
        assert_eq!(t[1].direction, Direction::In);
        assert_eq!(t[1].data, [0x06]);
        assert_eq!(bulk_in_stream(&t, 512).len(), 3072);
    }

    #[test]
    fn log_collapses_image_data() {
        let t = transfers(&sample(), (1, 3));
        let log = format_log(&t, LogOptions::default());
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 5, "{log}");
        assert!(
            lines[0].contains("OUT blk ep02     2  1b 40  |.@|"),
            "{log}"
        );
        assert!(lines[2].contains("DATA 3 transfer(s), 3072 bytes"), "{log}");
    }

    #[test]
    fn hex_ascii_truncates() {
        assert_eq!(hex_ascii(b"AB\x01", 2), "41 42  |AB| ... (+1 bytes)");
    }
}
