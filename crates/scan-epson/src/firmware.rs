//! Firmware upload. The V500 boots without firmware (status `28 00`) and needs Epson's
//! `esfw7c.bin`, which ships with Epson Scan and is not redistributable: it is read from the
//! user's own installation, never bundled.
//!
//! The file is a 256-byte `EDLA` header followed by the image. The upload is:
//!
//! ```text
//! > 1b 06             < 06
//! > u32 length        (image length + 1 checksum byte, from the header)
//! > image             (in 0xfff0-byte writes, like Epson's driver)
//! > checksum          (low byte of the image's byte sum)   < 06
//! > 1b 16             < 06
//! > 80                < 06   then the scanner boots, which takes several seconds
//! ```

use std::time::{Duration, Instant};

use scan_core::ScanError;

use crate::transport::Transport;

/// Where Epson Scan installs the V500 firmware.
pub const V500_FIRMWARE_PATH: &str = r"C:\Windows\System32\esfw7c.bin";

const MAGIC: &[u8; 4] = b"EDLA";
const HEADER_LEN: usize = 0x100;
const ACK: u8 = 0x06;
const STATUS: [u8; 2] = [0x1b, 0x03];
/// Status byte 0 bit set until firmware is running.
pub const STATUS_NO_FIRMWARE: u8 = 0x20;
const CHUNK: usize = 0xfff0;

/// A parsed firmware file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Firmware {
    /// USB vendor id the image is for.
    pub vendor_id: u16,
    pub image: Vec<u8>,
}

impl Firmware {
    pub fn parse(file: &[u8]) -> Result<Self, ScanError> {
        let bad = |why: &str| ScanError::Backend(format!("not an Epson firmware file: {why}"));
        if file.len() < HEADER_LEN || &file[..4] != MAGIC {
            return Err(bad("no EDLA header"));
        }
        let u32_at = |o: usize| u32::from_le_bytes(file[o..o + 4].try_into().unwrap()) as usize;
        let offset = u32_at(0x40);
        // The length sent to the scanner counts the trailing checksum byte.
        let len = u32_at(0x4c)
            .checked_sub(1)
            .ok_or_else(|| bad("empty image"))?;
        let image = file
            .get(offset..offset + len)
            .ok_or_else(|| bad("image runs past the end of the file"))?;
        Ok(Self {
            vendor_id: u16::from_le_bytes([file[0x4a], file[0x4b]]),
            image: image.to_vec(),
        })
    }

    pub fn checksum(&self) -> u8 {
        self.image.iter().fold(0u8, |s, &b| s.wrapping_add(b))
    }
}

/// Read the scanner's two status bytes.
pub fn status(dev: &mut dyn Transport) -> Result<[u8; 2], ScanError> {
    dev.write(&STATUS)?;
    let mut reply = [0u8; 2];
    dev.read_exact(&mut reply)?;
    Ok(reply)
}

/// Whether the scanner is waiting for firmware.
pub fn needs_firmware(dev: &mut dyn Transport) -> Result<bool, ScanError> {
    Ok(status(dev)?[0] & STATUS_NO_FIRMWARE != 0)
}

fn expect_ack(dev: &mut dyn Transport, step: &str) -> Result<(), ScanError> {
    let mut reply = [0u8; 1];
    dev.read_exact(&mut reply)?;
    if reply[0] == ACK {
        Ok(())
    } else {
        Err(ScanError::Backend(format!(
            "firmware upload: {step} answered {:02x}, expected ack",
            reply[0]
        )))
    }
}

/// Upload `fw` and wait up to `boot_timeout` for the scanner to come up.
pub fn upload(
    dev: &mut dyn Transport,
    fw: &Firmware,
    boot_timeout: Duration,
) -> Result<(), ScanError> {
    dev.write(&[0x1b, 0x06])?;
    expect_ack(dev, "1b 06")?;
    let len = u32::try_from(fw.image.len() + 1)
        .map_err(|_| ScanError::Backend("firmware image too large".into()))?;
    dev.write(&len.to_le_bytes())?;
    for chunk in fw.image.chunks(CHUNK) {
        dev.write(chunk)?;
    }
    dev.write(&[fw.checksum()])?;
    expect_ack(dev, "checksum")?;
    dev.write(&[0x1b, 0x16])?;
    expect_ack(dev, "1b 16")?;
    dev.write(&[0x80])?;
    expect_ack(dev, "80")?;

    // The scanner doesn't answer while it boots; a status read blocks or times out.
    let start = Instant::now();
    loop {
        match status(dev) {
            Ok(s) if s[0] & STATUS_NO_FIRMWARE == 0 => return Ok(()),
            Ok(_) | Err(_) if start.elapsed() < boot_timeout => {
                std::thread::sleep(Duration::from_millis(500))
            }
            Ok(s) => {
                return Err(ScanError::Backend(format!(
                    "scanner still reports {s:02x?} after firmware upload"
                )));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Exchange, ScriptedTransport};

    fn file(image: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; HEADER_LEN];
        f[..4].copy_from_slice(MAGIC);
        f[0x40..0x44].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
        f[0x4a..0x4c].copy_from_slice(&0x04b8u16.to_le_bytes());
        f[0x4c..0x50].copy_from_slice(&(image.len() as u32 + 1).to_le_bytes());
        f.extend_from_slice(image);
        // Real files carry one more byte that isn't sent.
        f.push(0xeb);
        f
    }

    #[test]
    fn parses_header() {
        let fw = Firmware::parse(&file(&[1, 2, 0xff])).unwrap();
        assert_eq!(fw.vendor_id, 0x04b8);
        assert_eq!(fw.image, [1, 2, 0xff]);
        assert_eq!(fw.checksum(), 2);
        assert!(Firmware::parse(b"MZ").is_err());
        let mut short = file(&[1, 2, 3]);
        short.truncate(HEADER_LEN + 1);
        assert!(Firmware::parse(&short).is_err());
    }

    #[test]
    fn uploads_and_waits_for_boot() {
        let fw = Firmware::parse(&file(&[1, 2, 0xff])).unwrap();
        let mut dev = ScriptedTransport::new([
            Exchange::new(&[0x1b, 0x06], &[&[ACK]]),
            Exchange::new(&4u32.to_le_bytes(), &[]),
            Exchange::new(&[1, 2, 0xff], &[]),
            Exchange::new(&[2], &[&[ACK]]),
            Exchange::new(&[0x1b, 0x16], &[&[ACK]]),
            Exchange::new(&[0x80], &[&[ACK]]),
            Exchange::new(&STATUS, &[&[0x28, 0x00]]),
            Exchange::new(&STATUS, &[&[0x18, 0x81]]),
        ]);
        upload(&mut dev, &fw, Duration::from_secs(5)).unwrap();
        assert!(dev.finished());
    }

    #[test]
    fn rejects_nak() {
        let fw = Firmware::parse(&file(&[1])).unwrap();
        let mut dev = ScriptedTransport::new([Exchange::new(&[0x1b, 0x06], &[&[0x15]])]);
        assert!(upload(&mut dev, &fw, Duration::ZERO).is_err());
    }
}
