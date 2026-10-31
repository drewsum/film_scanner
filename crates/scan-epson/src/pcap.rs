//! Reader for USBPcap captures (classic libpcap files with `LINKTYPE_USBPCAP`).
//!
//! `USBPCAP_BUFFER_PACKET_HEADER` is packed little-endian:
//!
//! | offset | field      | size |
//! |--------|------------|------|
//! | 0      | headerLen  | 2    |
//! | 2      | irpId      | 8    |
//! | 10     | status     | 4    |
//! | 14     | function   | 2    |
//! | 16     | info       | 1    | bit 0 set: completion (device to host)
//! | 17     | bus        | 2    |
//! | 19     | device     | 2    |
//! | 21     | endpoint   | 1    | bit 7 set: IN endpoint
//! | 22     | transfer   | 1    | 0 isochronous, 1 interrupt, 2 control, 3 bulk
//! | 23     | dataLength | 4    |
//!
//! Control transfers carry one more byte, the stage. The payload follows `headerLen` bytes.

use scan_core::ScanError;

pub const LINKTYPE_USBPCAP: u32 = 249;
const BASE_HEADER_LEN: usize = 27;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferType {
    Isochronous,
    Interrupt,
    Control,
    Bulk,
    Other(u8),
}

impl TransferType {
    fn from_raw(v: u8) -> Self {
        match v {
            0 => TransferType::Isochronous,
            1 => TransferType::Interrupt,
            2 => TransferType::Control,
            3 => TransferType::Bulk,
            other => TransferType::Other(other),
        }
    }
}

/// One captured URB, either its submission (host to driver) or its completion.
#[derive(Clone, Debug, PartialEq)]
pub struct UsbPacket {
    /// Seconds since the Unix epoch.
    pub timestamp: f64,
    pub irp_id: u64,
    pub status: u32,
    pub function: u16,
    /// Set for completions travelling back from the device.
    pub completion: bool,
    pub bus: u16,
    pub device: u16,
    pub endpoint: u8,
    pub transfer: TransferType,
    /// Control transfer stage (0 setup, 1 data, 2 status, 3 complete).
    pub control_stage: Option<u8>,
    pub data: Vec<u8>,
}

impl UsbPacket {
    pub fn is_in_endpoint(&self) -> bool {
        self.endpoint & 0x80 != 0
    }
}

fn bad(msg: impl Into<String>) -> ScanError {
    ScanError::Backend(format!("invalid capture: {}", msg.into()))
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

/// Parse a whole `.pcap` file written by USBPcapCMD (or saved from Wireshark as pcap).
pub fn parse(file: &[u8]) -> Result<Vec<UsbPacket>, ScanError> {
    if file.len() < 24 {
        return Err(bad("shorter than a pcap header"));
    }
    let (big_endian, nanos) = match &file[..4] {
        [0xd4, 0xc3, 0xb2, 0xa1] => (false, false),
        [0x4d, 0x3c, 0xb2, 0xa1] => (false, true),
        [0xa1, 0xb2, 0xc3, 0xd4] => (true, false),
        [0xa1, 0xb2, 0x3c, 0x4d] => (true, true),
        [0x0a, 0x0d, 0x0d, 0x0a] => {
            return Err(bad(
                "pcapng is not supported; save the capture as libpcap (.pcap)",
            ));
        }
        _ => return Err(bad("not a pcap file")),
    };
    let rd32 = |i: usize| {
        let b: [u8; 4] = file[i..i + 4].try_into().unwrap();
        if big_endian {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        }
    };
    let linktype = rd32(20);
    if linktype != LINKTYPE_USBPCAP {
        return Err(bad(format!(
            "link type {linktype}, expected USBPcap ({LINKTYPE_USBPCAP})"
        )));
    }
    let mut packets = Vec::new();
    let mut at = 24;
    while at + 16 <= file.len() {
        let secs = rd32(at) as f64;
        let frac = rd32(at + 4) as f64 / if nanos { 1e9 } else { 1e6 };
        let len = rd32(at + 8) as usize;
        let start = at + 16;
        let Some(record) = file.get(start..start + len) else {
            log::warn!("capture truncated at byte {at}; ignoring the partial record");
            break;
        };
        packets.push(parse_record(record, secs + frac)?);
        at = start + len;
    }
    Ok(packets)
}

fn parse_record(r: &[u8], timestamp: f64) -> Result<UsbPacket, ScanError> {
    if r.len() < BASE_HEADER_LEN {
        return Err(bad("record shorter than the USBPcap header"));
    }
    let header_len = u16_at(r, 0) as usize;
    if header_len < BASE_HEADER_LEN || header_len > r.len() {
        return Err(bad(format!("USBPcap header length {header_len}")));
    }
    let transfer = TransferType::from_raw(r[22]);
    let control_stage = (transfer == TransferType::Control && header_len > BASE_HEADER_LEN)
        .then(|| r[BASE_HEADER_LEN]);
    let data_len = u32_at(r, 23) as usize;
    let data_end = (header_len + data_len).min(r.len());
    Ok(UsbPacket {
        timestamp,
        irp_id: u64_at(r, 2),
        status: u32_at(r, 10),
        function: u16_at(r, 14),
        completion: r[16] & 1 != 0,
        bus: u16_at(r, 17),
        device: u16_at(r, 19),
        endpoint: r[21],
        transfer,
        control_stage,
        data: r[header_len..data_end].to_vec(),
    })
}

/// Build pcap files in tests.
#[cfg(test)]
pub(crate) mod build {
    use super::*;

    pub struct Record {
        pub completion: bool,
        pub device: u16,
        pub endpoint: u8,
        pub transfer: u8,
        pub data: Vec<u8>,
    }

    pub fn bulk(completion: bool, endpoint: u8, data: &[u8]) -> Record {
        Record {
            completion,
            device: 3,
            endpoint,
            transfer: 3,
            data: data.to_vec(),
        }
    }

    pub fn file(records: &[Record]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&0xa1b2c3d4u32.to_le_bytes());
        f.extend_from_slice(&2u16.to_le_bytes());
        f.extend_from_slice(&4u16.to_le_bytes());
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(&65535u32.to_le_bytes());
        f.extend_from_slice(&LINKTYPE_USBPCAP.to_le_bytes());
        for (i, r) in records.iter().enumerate() {
            let header_len = if r.transfer == 2 { 28 } else { 27 };
            let mut rec = vec![0u8; header_len];
            rec[0..2].copy_from_slice(&(header_len as u16).to_le_bytes());
            rec[2..10].copy_from_slice(&(i as u64 / 2).to_le_bytes());
            rec[16] = r.completion as u8;
            rec[17..19].copy_from_slice(&1u16.to_le_bytes());
            rec[19..21].copy_from_slice(&r.device.to_le_bytes());
            rec[21] = r.endpoint;
            rec[22] = r.transfer;
            rec[23..27].copy_from_slice(&(r.data.len() as u32).to_le_bytes());
            rec.extend_from_slice(&r.data);
            f.extend_from_slice(&(100 + i as u32).to_le_bytes());
            f.extend_from_slice(&500_000u32.to_le_bytes());
            f.extend_from_slice(&(rec.len() as u32).to_le_bytes());
            f.extend_from_slice(&(rec.len() as u32).to_le_bytes());
            f.extend_from_slice(&rec);
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;

    #[test]
    fn parses_bulk_records() {
        let f = file(&[bulk(false, 0x02, &[0x1b, 0x40]), bulk(true, 0x81, &[0x06])]);
        let p = parse(&f).unwrap();
        assert_eq!(p.len(), 2);
        assert!(!p[0].completion && !p[0].is_in_endpoint());
        assert_eq!(p[0].transfer, TransferType::Bulk);
        assert_eq!(p[0].data, [0x1b, 0x40]);
        assert_eq!(p[0].device, 3);
        assert!(p[1].completion && p[1].is_in_endpoint());
        assert_eq!(p[1].data, [0x06]);
        assert!((p[1].timestamp - 101.5).abs() < 1e-9);
    }

    #[test]
    fn reads_control_stage() {
        let f = file(&[Record {
            completion: false,
            device: 3,
            endpoint: 0x80,
            transfer: 2,
            data: vec![0x80, 6, 0, 1, 0, 0, 18, 0],
        }]);
        let p = parse(&f).unwrap();
        assert_eq!(p[0].transfer, TransferType::Control);
        assert_eq!(p[0].control_stage, Some(0));
        assert_eq!(p[0].data.len(), 8);
    }

    #[test]
    fn tolerates_truncated_tail_and_rejects_other_formats() {
        let mut f = file(&[bulk(false, 0x02, &[1, 2, 3])]);
        f.truncate(f.len() - 1);
        assert!(parse(&f).unwrap().is_empty());
        assert!(parse(&[0x0a, 0x0d, 0x0d, 0x0a, 0, 0, 0, 0]).is_err());
        let mut eth = file(&[]);
        eth[20] = 1;
        assert!(parse(&eth).is_err());
    }
}
