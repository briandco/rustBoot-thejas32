//! THEJAS32 pieces shared by rustBoot and applications: the SPI flash driver,
//! the flash layout, `confirm` (an application calls it once it is running
//! correctly after an update), and `update` (an application installing its
//! own update, then rebooting into it).

#![no_std]

pub mod flash;
pub mod layout;
pub mod update;

pub use update::{reboot, Installer, UpdateError};

pub use rb_state;

use flash::{Flash, FlashError};
use rb_state::log::{LogError, StateLog};

pub fn state_log() -> StateLog {
    StateLog { sectors: layout::STATE_SECTORS }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmError {
    /// No boot state record at this build's `layout::STATE_SECTORS`. rustBoot
    /// records state before booting any installed image, so this almost
    /// always means the application was built against a different flash
    /// layout than the rustBoot on the board. Rebuild it. Nothing was written.
    NoBootState,
    Log(LogError<FlashError>),
}

impl From<FlashError> for ConfirmError {
    fn from(e: FlashError) -> Self {
        ConfirmError::Log(LogError::Flash(e))
    }
}

/// Marks the running image good, if it is on trial after an update.
///
/// Returns `Ok(true)` if it was on trial and is now confirmed, `Ok(false)` if
/// there was nothing to confirm. Without this, the next reset rolls back to
/// the previous image.
///
/// The application must be built with the same `rb-core` layout as the
/// rustBoot on the board; see `ConfirmError::NoBootState`.
pub fn confirm(flash: &mut Flash) -> Result<bool, ConfirmError> {
    let log = state_log();
    let (mut state, cursor) = log.load(flash)?;
    if cursor.seq() == 0 {
        return Err(ConfirmError::NoBootState);
    }
    if !rb_state::confirm(&mut state) {
        return Ok(false);
    }
    log.save(flash, cursor, &state).map_err(ConfirmError::Log)?;
    Ok(true)
}
