//! Command framing of the V500's protocol (see `docs/epson-v500`).
//!
//! A command is two bytes (`1b xx` or `1e xx`). Commands with parameters are acked with `06`,
//! then the parameters are sent and acked again; queries answer with their reply instead.
//! `1e 47` answers with image data in blocks, each followed by an 8-byte trailer.

use std::time::Duration;

use scan_core::ScanError;

use crate::data::BlockAssembler;
use crate::transport::Transport;

const ACK: u8 = 0x06;
const STATUS: [u8; 2] = [0x1b, 0x03];
/// Bit of the first status byte that is set while the carriage moves.
const STATUS_BUSY: u8 = 0x40;
const MAX_POLLS: usize = 2000;

/// A scanner connection that speaks in commands rather than transfers.
pub struct Protocol<'a> {
    dev: &'a mut dyn Transport,
    poll_delay: Duration,
}

fn protocol_error(what: String) -> ScanError {
    ScanError::Backend(what)
}

impl<'a> Protocol<'a> {
    pub fn new(dev: &'a mut dyn Transport, poll_delay: Duration) -> Self {
        Self { dev, poll_delay }
    }

    fn read_reply(&mut self, len: usize) -> Result<Vec<u8>, ScanError> {
        let mut buf = vec![0u8; len.max(1)];
        let n = self.dev.read(&mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn expect_ack(&mut self, cmd: [u8; 2]) -> Result<(), ScanError> {
        match self.read_reply(1)?[..] {
            [ACK] => Ok(()),
            ref r => Err(protocol_error(format!(
                "command {cmd:02x?}: expected an ack, got {r:02x?}"
            ))),
        }
    }

    /// Send a command and its parameters, which may be sent in several writes (`1e 84` sends
    /// a header, then the table) and are acked once.
    pub fn command(&mut self, cmd: [u8; 2], params: &[&[u8]]) -> Result<(), ScanError> {
        self.dev.write(&cmd)?;
        self.expect_ack(cmd)?;
        if params.is_empty() {
            return Ok(());
        }
        for p in params {
            self.dev.write(p)?;
        }
        self.expect_ack(cmd)
    }

    /// Send a command without parameters and return its reply of up to `len` bytes.
    pub fn query(&mut self, cmd: [u8; 2], len: usize) -> Result<Vec<u8>, ScanError> {
        self.dev.write(&cmd)?;
        self.read_reply(len)
    }

    /// Send a command, then its parameters, and return the reply to the parameters.
    pub fn request(
        &mut self,
        cmd: [u8; 2],
        params: &[u8],
        len: usize,
    ) -> Result<Vec<u8>, ScanError> {
        self.dev.write(&cmd)?;
        self.expect_ack(cmd)?;
        self.dev.write(params)?;
        self.read_reply(len)
    }

    /// Repeat `cmd` until its reply satisfies `done`.
    pub fn poll(
        &mut self,
        cmd: [u8; 2],
        len: usize,
        done: impl Fn(&[u8]) -> bool,
    ) -> Result<Vec<u8>, ScanError> {
        for attempt in 0..MAX_POLLS {
            if attempt > 0 {
                std::thread::sleep(self.poll_delay);
            }
            let reply = self.query(cmd, len)?;
            if done(&reply) {
                return Ok(reply);
            }
        }
        Err(protocol_error(format!("scanner never finished {cmd:02x?}")))
    }

    /// Wait until the carriage has stopped.
    pub fn wait_ready(&mut self) -> Result<(), ScanError> {
        self.poll(STATUS, 2, |r| {
            r.first().is_some_and(|b| b & STATUS_BUSY == 0)
        })
        .map(drop)
    }

    /// Start a read with `1e 47` and return `len` bytes of image data, trailers removed.
    pub fn read_data(&mut self, len: usize) -> Result<Vec<u8>, ScanError> {
        let mut data = Vec::with_capacity(len);
        self.read_data_into(len, &mut |block| data.extend_from_slice(block))?;
        Ok(data)
    }

    /// [`Self::read_data`], handing `received` each block of the `len` bytes as it comes
    /// instead of keeping them.
    pub fn read_data_into(
        &mut self,
        len: usize,
        received: &mut dyn FnMut(&[u8]),
    ) -> Result<(), ScanError> {
        self.dev.write(&[0x1e, 0x47])?;
        let mut done = 0;
        let mut blocks = BlockAssembler::default();
        let mut buf = vec![0u8; 65536];
        while done < len {
            let n = self.dev.read(&mut buf).map_err(|e| {
                protocol_error(format!(
                    "image read failed after {} of {len} bytes: {e}",
                    done + blocks.pending()
                ))
            })?;
            if n == 0 {
                return Err(protocol_error(format!(
                    "scanner stopped after {done} of {len} image bytes"
                )));
            }
            if let Some(block) = blocks.push(&buf[..n]) {
                log::trace!("block of {} bytes", block.len());
                if block.len() > len - done {
                    log::warn!("read {} image bytes, expected {len}", done + block.len());
                }
                let block = &block[..block.len().min(len - done)];
                done += block.len();
                received(block);
            }
        }
        Ok(())
    }
}
