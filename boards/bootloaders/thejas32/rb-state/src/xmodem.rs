//! XMODEM receiver (CRC mode; 128-byte SOH and 1024-byte STX blocks).
//!
//! `receive_to` hands each block to a `Sink` as it arrives, so an image can
//! be written to flash without holding all of it in RAM (an application
//! receiving its own update). `receive` is the same into one buffer (rustBoot's
//! recovery mode, which has the whole application region free).
//!
//! The sender pads the last block (with 0x1A), so the received length is a
//! multiple of 128; the image's own header says how many bytes are real.

use crate::crc::crc16_xmodem;

const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const EOT: u8 = 0x04;
const ACK: u8 = 0x06;
const NAK: u8 = 0x15;
const CAN: u8 = 0x18;
const CRC_MODE: u8 = b'C';

/// Byte I/O with a receive timeout.
pub trait Channel {
    fn read_byte(&mut self, timeout_ms: u32) -> Option<u8>;
    fn write_byte(&mut self, b: u8);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmodemError {
    /// The sender never started.
    NoSender,
    /// Too many consecutive bad blocks or timeouts.
    TooManyErrors,
    /// The sender cancelled.
    Cancelled,
    /// A block arrived out of sequence.
    Sequence,
    /// The image is larger than the buffer.
    Overflow,
    /// The sink refused a block (e.g. a flash write failed, or the image was
    /// rejected from its header). The transfer was cancelled.
    Sink,
}

/// Receives data in order, as XMODEM blocks are accepted.
pub trait Sink {
    /// `offset` is where `data` starts in the transfer. Called once per
    /// accepted block, in order; a repeated block is not passed again.
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), ()>;
}

struct BufSink<'a> {
    buf: &'a mut [u8],
}

impl Sink for BufSink<'_> {
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), ()> {
        let end = offset + data.len();
        if end > self.buf.len() {
            return Err(());
        }
        self.buf[offset..end].copy_from_slice(data);
        Ok(())
    }
}

/// Seconds of 'C' polls before giving up on a sender.
const START_TRIES: u32 = 60;
const MAX_ERRORS: u32 = 10;
const BYTE_TIMEOUT_MS: u32 = 1000;

/// Receives into `buf`. Returns the received length.
pub fn receive<C: Channel>(ch: &mut C, buf: &mut [u8]) -> Result<usize, XmodemError> {
    let cap = buf.len();
    let mut sink = BufSink { buf };
    receive_to(ch, &mut sink, cap)
}

/// Receives into `sink`, refusing more than `limit` bytes. Returns the
/// received length.
pub fn receive_to<C: Channel, S: Sink>(ch: &mut C, sink: &mut S, limit: usize) -> Result<usize, XmodemError> {
    let mut len = 0usize;
    let mut expect: u8 = 1;
    let mut errors = 0u32;
    let mut started = false;

    // Ask for CRC mode until the sender starts.
    let mut first = None;
    for _ in 0..START_TRIES {
        ch.write_byte(CRC_MODE);
        if let Some(b) = ch.read_byte(BYTE_TIMEOUT_MS) {
            first = Some(b);
            break;
        }
    }
    let mut next = first.ok_or(XmodemError::NoSender)?;

    loop {
        match next {
            EOT => {
                ch.write_byte(ACK);
                return Ok(len);
            }
            CAN => return Err(XmodemError::Cancelled),
            SOH | STX => {
                let size = if next == SOH { 128 } else { 1024 };
                match read_block(ch, size) {
                    Some((blk, data)) if blk == expect => {
                        if len + size > limit {
                            cancel(ch);
                            return Err(XmodemError::Overflow);
                        }
                        if sink.write(len, &data[..size]).is_err() {
                            cancel(ch);
                            return Err(XmodemError::Sink);
                        }
                        len += size;
                        expect = expect.wrapping_add(1);
                        errors = 0;
                        started = true;
                        ch.write_byte(ACK);
                    }
                    // Our ACK was lost and the sender repeated the block.
                    Some((blk, _)) if started && blk == expect.wrapping_sub(1) => ch.write_byte(ACK),
                    Some(_) => {
                        cancel(ch);
                        return Err(XmodemError::Sequence);
                    }
                    None => {
                        errors += 1;
                        if errors > MAX_ERRORS {
                            cancel(ch);
                            return Err(XmodemError::TooManyErrors);
                        }
                        purge(ch);
                        ch.write_byte(NAK);
                    }
                }
            }
            _ => {
                // Line noise between blocks.
                errors += 1;
                if errors > MAX_ERRORS {
                    cancel(ch);
                    return Err(XmodemError::TooManyErrors);
                }
            }
        }
        next = loop {
            match ch.read_byte(BYTE_TIMEOUT_MS * 10) {
                Some(b) => break b,
                None => {
                    errors += 1;
                    if errors > MAX_ERRORS {
                        cancel(ch);
                        return Err(XmodemError::TooManyErrors);
                    }
                    ch.write_byte(if started { NAK } else { CRC_MODE });
                }
            }
        };
    }
}

/// Reads block number, its complement, `size` data bytes and the CRC.
/// `None` if anything is missing or wrong.
fn read_block<C: Channel>(ch: &mut C, size: usize) -> Option<(u8, [u8; 1024])> {
    let blk = ch.read_byte(BYTE_TIMEOUT_MS)?;
    let inv = ch.read_byte(BYTE_TIMEOUT_MS)?;
    let mut data = [0u8; 1024];
    for slot in data[..size].iter_mut() {
        *slot = ch.read_byte(BYTE_TIMEOUT_MS)?;
    }
    let hi = ch.read_byte(BYTE_TIMEOUT_MS)?;
    let lo = ch.read_byte(BYTE_TIMEOUT_MS)?;
    if blk != !inv || crc16_xmodem(&data[..size]) != u16::from_be_bytes([hi, lo]) {
        return None;
    }
    Some((blk, data))
}

/// Drains the line until it goes quiet, so a NAK is not sent mid-block.
fn purge<C: Channel>(ch: &mut C) {
    while ch.read_byte(100).is_some() {}
}

fn cancel<C: Channel>(ch: &mut C) {
    for _ in 0..3 {
        ch.write_byte(CAN);
    }
}
