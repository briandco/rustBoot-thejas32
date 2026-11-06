//! Installing an update from a running application, and rebooting into it.
//!
//! The application streams the signed image (header first) into `Installer`,
//! then calls `finish` and `reboot`. rustBoot then verifies the image and
//! boots it on trial; the new version must call `confirm`, or the following
//! reboot rolls back. The application never verifies the signature itself:
//! rustBoot does, before it runs anything.
//!
//! Power-safe in the same way as rustBoot's recovery install: the slot being
//! written is never the active one, any reference to it is dropped from the
//! boot state before it is erased, and it is recorded as pending only after
//! every byte is written. A cut at any point leaves the current version
//! booting.

use crate::flash::{Flash, FlashError, BLOCK_SIZE};
use crate::layout::*;
use rb_state::image::{image_info, HEADER_SIZE};
use rb_state::log::{Cursor, LogError};
use rb_state::{install_begin, install_done, BootState, InstallError, Slot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateError {
    /// The first 256 bytes are not a rustBoot image header.
    BadHeader,
    /// The firmware is larger than an application may be.
    TooLarge { fw_size: usize, max: usize },
    /// Lower than the confirmed minimum version.
    VersionTooOld { version: u32, min: u32 },
    /// No boot state found: the application was built for a different flash
    /// layout than the rustBoot on the board (see `crate::confirm`).
    NoBootState,
    /// `finish` before the whole image arrived.
    Incomplete { written: usize, total: usize },
    Flash(FlashError),
    State(LogError<FlashError>),
}

impl From<FlashError> for UpdateError {
    fn from(e: FlashError) -> Self {
        UpdateError::Flash(e)
    }
}

impl From<LogError<FlashError>> for UpdateError {
    fn from(e: LogError<FlashError>) -> Self {
        UpdateError::State(e)
    }
}

struct Target {
    slot: Slot,
    base: u32,
    version: u32,
    /// Header + firmware bytes.
    total: usize,
    state: BootState,
    cursor: Cursor,
}

/// Streams one image into the inactive slot.
pub struct Installer {
    header: [u8; HEADER_SIZE],
    received: usize,
    target: Option<Target>,
}

impl Default for Installer {
    fn default() -> Self {
        Self::new()
    }
}

impl Installer {
    pub fn new() -> Self {
        Installer { header: [0; HEADER_SIZE], received: 0, target: None }
    }

    /// Feeds the next bytes of the image, in order. Bytes past the image's
    /// own size (transfer padding) are ignored. Nothing is erased until the
    /// whole header has arrived and passed its checks.
    pub fn write(&mut self, flash: &mut Flash, mut data: &[u8]) -> Result<(), UpdateError> {
        if self.received < HEADER_SIZE {
            let n = (HEADER_SIZE - self.received).min(data.len());
            self.header[self.received..self.received + n].copy_from_slice(&data[..n]);
            self.received += n;
            data = &data[n..];
            if self.received < HEADER_SIZE {
                return Ok(());
            }
            self.begin(flash)?;
        }
        let t = self.target.as_ref().expect("begin succeeded");
        let n = t.total.saturating_sub(self.received).min(data.len());
        if n > 0 {
            flash.program(t.base + self.received as u32, &data[..n])?;
            self.received += n;
        }
        Ok(())
    }

    fn begin(&mut self, flash: &mut Flash) -> Result<(), UpdateError> {
        let info = image_info(&self.header).ok_or(UpdateError::BadHeader)?;
        if info.fw_size > MAX_FW_SIZE {
            return Err(UpdateError::TooLarge { fw_size: info.fw_size, max: MAX_FW_SIZE });
        }
        let log = crate::state_log();
        let (state, cursor) = log.load(flash)?;
        if cursor.seq() == 0 {
            return Err(UpdateError::NoBootState);
        }
        let (slot, next) = install_begin(&state, info.version).map_err(|e| match e {
            InstallError::VersionTooOld { version, min } => UpdateError::VersionTooOld { version, min },
        })?;
        // Drop any reference to the target slot before erasing it.
        let cursor = if next != state { log.save(flash, cursor, &next)? } else { cursor };
        let base = slot_addr(slot);
        for i in 0..(SLOT_SIZE as u32 / BLOCK_SIZE) {
            flash.erase_block(base + i * BLOCK_SIZE)?;
        }
        flash.program(base, &self.header)?;
        self.target = Some(Target {
            slot,
            base,
            version: info.version,
            total: HEADER_SIZE + info.fw_size,
            state: next,
            cursor,
        });
        Ok(())
    }

    /// Records the written image as pending, so the next boot tries it.
    /// Returns the slot and version.
    pub fn finish(self, flash: &mut Flash) -> Result<(Slot, u32), UpdateError> {
        let t = self.target.ok_or(UpdateError::Incomplete { written: self.received, total: HEADER_SIZE })?;
        if self.received < t.total {
            return Err(UpdateError::Incomplete { written: self.received, total: t.total });
        }
        let pending = install_done(&t.state, t.slot, t.version);
        crate::state_log().save(flash, t.cursor, &pending)?;
        Ok((t.slot, t.version))
    }
}

/// Restarts through the boot ROM, so rustBoot runs and picks up a pending
/// update (or rolls back an unconfirmed one).
///
/// THEJAS32 has no reset register or watchdog that we know of, so this is a
/// jump to the ROM's entry at 0x10000, measured on hardware: the first words
/// there clear the registers one by one (`li x1,0`, `li x2,0`, ...), and
/// jumping there runs the normal flash boot into rustBoot. It is a software
/// restart: peripherals keep their current settings rather than returning to
/// reset defaults, so drivers must configure what they use.
pub fn reboot() -> ! {
    const ROM_ENTRY: usize = 0x1_0000;
    // SAFETY: interrupt sources are masked so nothing fires before the ROM
    // installs its own trap vector; control never returns.
    unsafe {
        core::arch::asm!("csrw mie, zero", "fence", "jr {0}", in(reg) ROM_ENTRY, options(noreturn));
    }
}
