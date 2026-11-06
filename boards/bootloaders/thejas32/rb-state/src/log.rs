//! Power-safe storage of `BootState` in two flash sectors.
//!
//! Each save appends a 32-byte record carrying a sequence number and a
//! CRC-32. Loading takes the valid record with the highest sequence number
//! from either sector. A record cut short by power loss fails its CRC and is
//! ignored, so the state is always either the one before the interrupted save
//! or the one after it.
//!
//! When the current sector is full, the record goes to the start of the other
//! sector, after erasing it. The full sector is left alone, so a power cut
//! during that erase or write still finds the previous record.
//!
//! Only erased (`0xFF`) bytes are ever programmed. A position holding anything
//! else, even a torn record, is never reused until its sector is erased.

use crate::crc::crc32;
use crate::{BootState, Slot};

/// The flash operations the log needs.
pub trait Nor {
    type Error;
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), Self::Error>;
    /// Programs erased bytes. Must not cross a 256-byte page.
    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), Self::Error>;
    /// Erases the 4 KB sector at `addr`.
    fn erase_sector(&mut self, addr: u32) -> Result<(), Self::Error>;
}

pub const SECTOR: u32 = 0x1000;
pub const RECORD: usize = 32;
const PER_SECTOR: u32 = SECTOR / RECORD as u32;
const MAGIC: [u8; 4] = *b"RBS1";
const NONE: u8 = 0xFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogError<E> {
    Flash(E),
    /// The record read back differently from what was written.
    Verify,
}

impl<E> From<E> for LogError<E> {
    fn from(e: E) -> Self {
        LogError::Flash(e)
    }
}

/// Where the next record goes, as found by `load`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    seq: u32,
    sector: u32, // 0 or 1
    next: u32,   // record index within the sector; PER_SECTOR = full
}

pub struct StateLog {
    /// Flash addresses of the two sectors.
    pub sectors: [u32; 2],
}

pub fn encode(s: &BootState, seq: u32) -> [u8; RECORD] {
    let mut r = [0u8; RECORD];
    r[0..4].copy_from_slice(&MAGIC);
    r[4..8].copy_from_slice(&seq.to_le_bytes());
    r[8] = s.active as u8;
    r[9] = s.pending.map_or(NONE, |(slot, _)| slot as u8);
    r[10] = s.trial.map_or(NONE, |(slot, _)| slot as u8);
    r[12..16].copy_from_slice(&s.pending.map_or(0, |(_, v)| v).to_le_bytes());
    r[16..20].copy_from_slice(&s.trial.map_or(0, |(_, v)| v).to_le_bytes());
    r[20..24].copy_from_slice(&s.min_version.to_le_bytes());
    let crc = crc32(&r[..28]);
    r[28..32].copy_from_slice(&crc.to_le_bytes());
    r
}

pub fn decode(r: &[u8; RECORD]) -> Option<(BootState, u32)> {
    if r[0..4] != MAGIC || crc32(&r[..28]).to_le_bytes() != r[28..32] {
        return None;
    }
    let word = |i: usize| u32::from_le_bytes([r[i], r[i + 1], r[i + 2], r[i + 3]]);
    let opt = |slot: u8, v: u32| -> Option<Option<(Slot, u32)>> {
        if slot == NONE {
            Some(None)
        } else {
            Slot::from_u8(slot).map(|s| Some((s, v)))
        }
    };
    let state = BootState {
        active: Slot::from_u8(r[8])?,
        pending: opt(r[9], word(12))?,
        trial: opt(r[10], word(16))?,
        min_version: word(20),
    };
    Some((state, word(4)))
}

impl StateLog {
    /// The current state (default if there is no valid record), and where
    /// the next record goes.
    pub fn load<N: Nor>(&self, nor: &mut N) -> Result<(BootState, Cursor), N::Error> {
        let mut best: Option<(BootState, u32, u32)> = None; // state, seq, sector
        let mut used = [0u32; 2]; // index after the last non-erased record
        for (si, &base) in self.sectors.iter().enumerate() {
            for i in 0..PER_SECTOR {
                let mut r = [0u8; RECORD];
                nor.read(base + i * RECORD as u32, &mut r)?;
                if r.iter().all(|&b| b == 0xFF) {
                    continue;
                }
                used[si] = i + 1;
                if let Some((state, seq)) = decode(&r) {
                    if best.map_or(true, |(_, s, _)| seq > s) {
                        best = Some((state, seq, si as u32));
                    }
                }
            }
        }
        Ok(match best {
            Some((state, seq, sector)) => (state, Cursor { seq, sector, next: used[sector as usize] }),
            // Nothing valid: start in sector 0 after whatever is there.
            None => (BootState::default(), Cursor { seq: 0, sector: 0, next: used[0] }),
        })
    }

    /// Appends `state`, returning the updated cursor.
    ///
    /// On error the cursor is stale (a position may now be partly written):
    /// call `load` again before the next save.
    pub fn save<N: Nor>(&self, nor: &mut N, cur: Cursor, state: &BootState) -> Result<Cursor, LogError<N::Error>> {
        let seq = cur.seq.wrapping_add(1);
        let (sector, index) = if cur.next < PER_SECTOR {
            (cur.sector, cur.next)
        } else {
            let other = cur.sector ^ 1;
            nor.erase_sector(self.sectors[other as usize])?;
            (other, 0)
        };
        let addr = self.sectors[sector as usize] + index * RECORD as u32;
        let rec = encode(state, seq);
        nor.program(addr, &rec)?;
        let mut back = [0u8; RECORD];
        nor.read(addr, &mut back)?;
        if back != rec {
            // Leave it: it fails its CRC or is the record we wanted. The next
            // save goes after it either way.
            return Err(LogError::Verify);
        }
        Ok(Cursor { seq, sector, next: index + 1 })
    }
}
