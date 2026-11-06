//! THEJAS32 pieces shared by rustBoot and applications: the SPI flash driver,
//! the flash layout, and `confirm`, which an application calls once it is
//! running correctly after an update.

#![no_std]

pub mod flash;
pub mod layout;

pub use rb_state;

use flash::{Flash, FlashError};
use rb_state::log::{LogError, StateLog};

pub fn state_log() -> StateLog {
    StateLog { sectors: layout::STATE_SECTORS }
}

/// Marks the running image good, if it is on trial after an update.
///
/// Returns `Ok(true)` if it was on trial and is now confirmed, `Ok(false)` if
/// there was nothing to confirm. Without this, the next reset rolls back to
/// the previous image.
pub fn confirm(flash: &mut Flash) -> Result<bool, LogError<FlashError>> {
    let log = state_log();
    let (mut state, cursor) = log.load(flash)?;
    if !rb_state::confirm(&mut state) {
        return Ok(false);
    }
    log.save(flash, cursor, &state)?;
    Ok(true)
}
