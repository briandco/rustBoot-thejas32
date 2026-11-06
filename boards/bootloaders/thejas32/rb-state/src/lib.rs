//! Boot state, boot decision and XMODEM receive for rustBoot on THEJAS32.
//!
//! Everything here is plain logic over small traits, with no hardware access,
//! so it runs and is tested on the host. That includes simulated power loss at
//! every byte of every flash write (see `tests`).
//!
//! # Update model
//!
//! THEJAS32 runs every image from RAM, so an application can be stored in
//! either of two flash slots. An update is installed into the slot that is not
//! active, and the boot decision chooses a slot. Nothing is swapped: the active
//! slot is never written during an update, so a power cut can at worst leave
//! the new slot unusable.
//!
//! - `install`: the new image is written to the inactive slot and recorded as
//!   *pending*.
//! - Next boot: the pending image is recorded as *on trial* and booted.
//! - The application calls `confirm`: it becomes *active*, and `min_version`
//!   rises to its version (anti-rollback).
//! - A reset while still on trial means it never confirmed: the boot after
//!   that rolls back to the previous active slot.

#![cfg_attr(not(test), no_std)]

pub mod crc;
pub mod log;
pub mod xmodem;

#[cfg(test)]
mod tests;

/// One of the two application slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    A = 0,
    B = 1,
}

impl Slot {
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
    fn from_u8(v: u8) -> Option<Slot> {
        match v {
            0 => Some(Slot::A),
            1 => Some(Slot::B),
            _ => None,
        }
    }
}

/// Persistent boot state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootState {
    /// The slot that boots normally.
    pub active: Slot,
    /// Installed but not yet booted: (slot, version).
    pub pending: Option<(Slot, u32)>,
    /// Booted once on trial, awaiting `confirm`: (slot, version).
    pub trial: Option<(Slot, u32)>,
    /// Lowest version that may be installed or booted.
    pub min_version: u32,
}

impl Default for BootState {
    /// A board with no state record: slot A (where phase 1 put the image) is
    /// active, and any version is allowed.
    fn default() -> Self {
        BootState { active: Slot::A, pending: None, trial: None, min_version: 0 }
    }
}

/// What the bootloader should do, and the state to save *before* doing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub state: BootState,
    pub outcome: Outcome,
    pub notes: Notes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Boot `slot`, which verified as `version`. `trial` if it must confirm.
    Boot { slot: Slot, version: u32, trial: bool },
    /// No bootable slot.
    Recovery,
}

/// Things that happened while deciding, for the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Notes {
    /// The previous boot was a trial of this slot and it never confirmed.
    pub rolled_back: Option<Slot>,
    /// The pending image failed verification or was below `min_version`.
    pub pending_rejected: bool,
    /// The active slot was unusable and the other slot is now active.
    pub fell_back: bool,
    /// A slot verified but its version is below `min_version`.
    pub below_min_version: bool,
}

/// Decides what to boot. `verify(slot)` must load and fully verify that
/// slot, returning its version, or `None` if it is not a valid signed image.
/// The bootloader boots the slot `verify` was last called with successfully,
/// so this returns as soon as one passes.
pub fn plan(mut s: BootState, mut verify: impl FnMut(Slot) -> Option<u32>) -> Plan {
    let mut notes = Notes::default();

    if let Some((slot, _)) = s.trial.take() {
        notes.rolled_back = Some(slot);
    }

    if let Some((slot, want)) = s.pending.take() {
        match verify(slot) {
            Some(v) if v == want && v >= s.min_version => {
                s.trial = Some((slot, v));
                return Plan { state: s, outcome: Outcome::Boot { slot, version: v, trial: true }, notes };
            }
            _ => notes.pending_rejected = true,
        }
    }

    for slot in [s.active, s.active.other()] {
        match verify(slot) {
            Some(v) if v >= s.min_version => {
                if slot != s.active {
                    notes.fell_back = true;
                    s.active = slot;
                }
                return Plan { state: s, outcome: Outcome::Boot { slot, version: v, trial: false }, notes };
            }
            Some(_) => notes.below_min_version = true,
            None => {}
        }
    }
    Plan { state: s, outcome: Outcome::Recovery, notes }
}

/// Called by the application once it is running correctly. Returns whether
/// the state changed (and so must be saved).
pub fn confirm(s: &mut BootState) -> bool {
    match s.trial.take() {
        Some((slot, v)) => {
            s.active = slot;
            s.min_version = s.min_version.max(v);
            true
        }
        None => false,
    }
}

/// Why an install was refused before anything was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallError {
    /// Lower than the confirmed minimum.
    VersionTooOld { version: u32, min: u32 },
}

/// Step 1 of an install: where to write, and the state to save *before*
/// erasing it, which drops any reference to that slot. Step 2 is
/// `install_done` once the slot is written and read back.
pub fn install_begin(s: &BootState, version: u32) -> Result<(Slot, BootState), InstallError> {
    if version < s.min_version {
        return Err(InstallError::VersionTooOld { version, min: s.min_version });
    }
    let target = s.active.other();
    let mut next = *s;
    next.pending = None;
    next.trial = None;
    Ok((target, next))
}

/// Step 2 of an install: record the written slot as pending.
pub fn install_done(s: &BootState, slot: Slot, version: u32) -> BootState {
    let mut next = *s;
    next.pending = Some((slot, version));
    next
}
