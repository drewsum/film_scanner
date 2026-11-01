//! Image data framing. After `1e 47` the scanner sends one block of image data followed by an
//! 8-byte trailer:
//!
//! ```text
//! 02 <u32 LE block length> 00 80 00
//! ```
//!
//! The trailer is not always a transfer of its own: when the block isn't a multiple of the
//! 512-byte USB packet size, the last transfer carries the block's final bytes and the trailer
//! together (e.g. 128 + 8 bytes).

const TRAILER_LEN: usize = 8;
/// Most bytes of short replies that may precede a block.
pub const MAX_PREFIX: usize = 64;

/// The block length announced by `trailer`, if it is one.
pub fn parse_trailer(trailer: &[u8]) -> Option<usize> {
    match trailer {
        [0x02, l0, l1, l2, l3, 0x00, 0x80, 0x00] => {
            Some(u32::from_le_bytes([*l0, *l1, *l2, *l3]) as usize)
        }
        _ => None,
    }
}

/// Collects the bytes read after a data command and splits off each block at its trailer.
#[derive(Debug, Default)]
pub struct BlockAssembler {
    buf: Vec<u8>,
}

impl BlockAssembler {
    /// Add the payload of one read. Returns the block's image data when `chunk` completes it.
    ///
    /// Up to [`MAX_PREFIX`] bytes before the block (an ack or status reply read since the last
    /// reset) are dropped.
    pub fn push(&mut self, chunk: &[u8]) -> Option<Vec<u8>> {
        self.buf.extend_from_slice(chunk);
        let body = self.buf.len().checked_sub(TRAILER_LEN)?;
        let len = parse_trailer(&self.buf[body..])?;
        let prefix = body.checked_sub(len).filter(|&p| p <= MAX_PREFIX)?;
        self.buf.truncate(body);
        self.buf.drain(..prefix);
        Some(std::mem::take(&mut self.buf))
    }

    /// Forget a partial block, e.g. when the host sends the next command.
    /// Bytes received of the block in progress.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    pub fn reset(&mut self) {
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trailer(len: u32) -> Vec<u8> {
        let mut t = vec![0x02];
        t.extend_from_slice(&len.to_le_bytes());
        t.extend_from_slice(&[0x00, 0x80, 0x00]);
        t
    }

    #[test]
    fn parses_captured_trailers() {
        assert_eq!(
            parse_trailer(&[0x02, 0x00, 0x00, 0x06, 0x00, 0x00, 0x80, 0x00]),
            Some(393_216)
        );
        assert_eq!(
            parse_trailer(&[0x02, 0x80, 0xf4, 0x03, 0x00, 0x00, 0x80, 0x00]),
            Some(259_200)
        );
        assert_eq!(parse_trailer(&[0x06]), None);
    }

    #[test]
    fn splits_blocks_with_shared_trailer_transfer() {
        let mut a = BlockAssembler::default();
        assert_eq!(a.push(&[1; 512]), None);
        // Last bytes of the block arrive together with the trailer.
        let mut tail = vec![2; 128];
        tail.extend(trailer(640));
        let block = a.push(&tail).unwrap();
        assert_eq!(block.len(), 640);
        assert_eq!(&block[510..514], &[1, 1, 2, 2]);
        // Separate trailer transfer.
        assert_eq!(a.push(&[3; 512]), None);
        assert_eq!(a.push(&trailer(512)).unwrap(), vec![3; 512]);
        // A trailer whose length doesn't fit is image data.
        assert_eq!(a.push(&trailer(9)), None);
        a.reset();
        assert_eq!(a.push(&trailer(0)).unwrap(), Vec::<u8>::new());
        // An ack before the block is dropped.
        a.push(&[0x06]);
        a.push(&[4; 512]);
        assert_eq!(a.push(&trailer(512)).unwrap(), vec![4; 512]);
    }
}
