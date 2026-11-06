//! Boot decision, recovery console and update install.

use core::fmt::Write;
use embedded_hal_nb::serial::{Read, Write as _};

use rb_core::flash::{Flash, BLOCK_SIZE};
use rb_core::layout::*;
use rb_state::log::{Cursor, StateLog};
use rb_state::xmodem::{self, Channel};
use rb_state::{install_begin, install_done, plan, BootState, Outcome, Slot};
use rustBoot::verify::{parse_header, verify};
use thejas32_hal::pac;
use thejas32_hal::serial::Serial;
use thejas32_hal::timer::Timer;

use crate::PUBKEY;

pub type Uart = Serial<pac::Uart0>;

/// How long rustBoot listens for 'r' before booting.
const RECOVERY_WINDOW_MS: u32 = 500;
const TICKS_PER_MS: u32 = 100_000; // 100 MHz, measured

pub struct Ctx {
    pub uart: Uart,
    flash: Flash,
    timer: Timer<pac::Timer0>,
    log: StateLog,
    state: BootState,
    cursor: Cursor,
}

/// What `run` decided to boot. The image is already in RAM at `APP_BASE`.
pub struct Chosen {
    pub slot: Slot,
    pub version: u32,
    pub trial: bool,
}

impl Ctx {
    pub fn new(uart: Uart, flash: Flash, timer0: pac::Timer0) -> Result<Self, rb_core::flash::FlashError> {
        let mut timer = Timer::new(timer0);
        timer.start_free_running();
        let log = rb_core::state_log();
        let mut flash = flash;
        let (state, cursor) = log.load(&mut flash)?;
        Ok(Ctx { uart, flash, timer, log, state, cursor })
    }

    pub fn run(&mut self) -> Chosen {
        self.print_state();
        let _ = writeln!(self.uart, "  press 'r' within {} ms for recovery\r", RECOVERY_WINDOW_MS);
        if self.read_byte(RECOVERY_WINDOW_MS) == Some(b'r') {
            self.recovery();
        }
        loop {
            let start = self.state;
            let p = plan(start, |slot| self.load_slot(slot));
            let n = p.notes;
            if let Some(slot) = n.rolled_back {
                let _ = writeln!(self.uart, "  ROLLBACK: slot {:?} was on trial and never confirmed\r", slot);
            }
            if n.pending_rejected {
                let _ = writeln!(self.uart, "  the newly installed image was rejected; keeping the current one\r");
            }
            if n.fell_back {
                let _ = writeln!(self.uart, "  the active slot is unusable; the other slot is now active\r");
            }
            if n.below_min_version {
                let _ = writeln!(self.uart, "  an image below the minimum version (v{}) was refused\r", start.min_version);
            }
            if p.state != start && !self.save(p.state) {
                if let Outcome::Boot { trial: true, .. } = p.outcome {
                    // Booting a trial that is not recorded would make the
                    // rollback impossible.
                    let _ = writeln!(self.uart, "  cannot record the trial boot; refusing to boot it\r");
                    self.recovery();
                    continue;
                }
            }
            match p.outcome {
                Outcome::Boot { slot, version, trial } => return Chosen { slot, version, trial },
                Outcome::Recovery => {
                    let _ = writeln!(self.uart, "  no bootable image\r");
                    self.recovery();
                }
            }
        }
    }

    // ------------------------------------------------------------ slots

    /// Copies a slot's firmware to `APP_BASE` and verifies it there.
    fn load_slot(&mut self, slot: Slot) -> Option<u32> {
        let base = slot_addr(slot);
        let mut hdr_buf = [0u8; IMAGE_HEADER_SIZE];
        if self.flash.read(base, &mut hdr_buf).is_err() {
            let _ = writeln!(self.uart, "  slot {:?}: flash read error\r", slot);
            return None;
        }
        let hdr = match parse_header(&hdr_buf) {
            Ok(h) => h,
            Err(_) => {
                let _ = writeln!(self.uart, "  slot {:?}: no image\r", slot);
                return None;
            }
        };
        if hdr.fw_size > MAX_FW_SIZE {
            let _ = writeln!(self.uart, "  slot {:?}: size {} exceeds {}\r", slot, hdr.fw_size, MAX_FW_SIZE);
            return None;
        }
        let fw = app_ram(hdr.fw_size);
        if self.flash.read(base + IMAGE_HEADER_SIZE as u32, fw).is_err() {
            let _ = writeln!(self.uart, "  slot {:?}: flash read error\r", slot);
            return None;
        }
        match verify(&hdr, fw, PUBKEY) {
            Ok(()) => {
                let _ = writeln!(self.uart, "  slot {:?}: v{}, {} bytes, verified\r", slot, hdr.version, hdr.fw_size);
                Some(hdr.version)
            }
            Err(e) => {
                let _ = writeln!(self.uart, "  slot {:?}: v{} FAILED verification: {:?}\r", slot, hdr.version, e);
                None
            }
        }
    }

    fn print_state(&mut self) {
        let s = self.state;
        let _ = write!(self.uart, "  state: active {:?}, min version {}", s.active, s.min_version);
        if let Some((slot, v)) = s.pending {
            let _ = write!(self.uart, ", pending {:?} v{}", slot, v);
        }
        if let Some((slot, v)) = s.trial {
            let _ = write!(self.uart, ", on trial {:?} v{}", slot, v);
        }
        let _ = writeln!(self.uart, "\r");
    }

    fn save(&mut self, s: BootState) -> bool {
        match self.log.save(&mut self.flash, self.cursor, &s) {
            Ok(c) => {
                self.cursor = c;
                self.state = s;
                true
            }
            Err(e) => {
                let _ = writeln!(self.uart, "  state save FAILED: {:?}\r", e);
                // The cursor may now point at a partly written record.
                if let Ok((state, cursor)) = self.log.load(&mut self.flash) {
                    self.state = state;
                    self.cursor = cursor;
                }
                false
            }
        }
    }

    // ------------------------------------------------------------ recovery

    fn recovery(&mut self) {
        let _ = writeln!(self.uart, "\r\nrustBoot recovery: u = upload image (XMODEM), c = confirm trial, i = info, b = boot\r");
        loop {
            let _ = write!(self.uart, "rb> ");
            let c = loop {
                if let Some(c) = self.read_byte(1000) {
                    break c;
                }
            };
            match c {
                b'u' => {
                    let _ = writeln!(self.uart, "u\r");
                    self.upload();
                }
                b'c' => {
                    let _ = writeln!(self.uart, "c\r");
                    let mut s = self.state;
                    if rb_state::confirm(&mut s) {
                        if self.save(s) {
                            let _ = writeln!(self.uart, "confirmed: slot {:?} is active, min version {}\r", s.active, s.min_version);
                        }
                    } else {
                        let _ = writeln!(self.uart, "nothing on trial\r");
                    }
                }
                b'i' => {
                    let _ = writeln!(self.uart, "i\r");
                    self.print_state();
                    for slot in [Slot::A, Slot::B] {
                        let mut h = [0u8; IMAGE_HEADER_SIZE];
                        let _ = self.flash.read(slot_addr(slot), &mut h);
                        match parse_header(&h) {
                            Ok(h) => {
                                let _ = writeln!(self.uart, "  slot {:?}: v{}, {} bytes (header only; verified at boot)\r", slot, h.version, h.fw_size);
                            }
                            Err(_) => {
                                let _ = writeln!(self.uart, "  slot {:?}: no image\r", slot);
                            }
                        }
                    }
                }
                b'b' => {
                    let _ = writeln!(self.uart, "b\r");
                    return;
                }
                b'\r' | b'\n' => {
                    let _ = writeln!(self.uart, "\r");
                }
                _ => {
                    let _ = writeln!(self.uart, "?  u = upload, c = confirm, i = info, b = boot\r");
                }
            }
        }
    }

    /// Receives a signed image, checks it, and installs it into the inactive
    /// slot. The active slot is not touched, so a failure or power cut at any
    /// point leaves the board booting what it booted before.
    fn upload(&mut self) {
        let _ = writeln!(self.uart, "send the signed image with XMODEM now\r");
        let buf = app_ram(APP_REGION_SIZE);
        let n = match xmodem::receive(self, buf) {
            Ok(n) => n,
            Err(e) => {
                self.pause_for_sender();
                let _ = writeln!(self.uart, "upload failed: {:?}\r", e);
                return;
            }
        };
        self.pause_for_sender();
        let _ = writeln!(self.uart, "received {} bytes\r", n);

        // 1. Check it completely before touching flash.
        if n < IMAGE_HEADER_SIZE {
            let _ = writeln!(self.uart, "refused: too short for a header\r");
            return;
        }
        let hdr_bytes: &[u8; IMAGE_HEADER_SIZE] = buf[..IMAGE_HEADER_SIZE].try_into().unwrap();
        let hdr = match parse_header(hdr_bytes) {
            Ok(h) => h,
            Err(e) => {
                let _ = writeln!(self.uart, "refused: not a rustBoot image ({})\r", e);
                return;
            }
        };
        let total = IMAGE_HEADER_SIZE + hdr.fw_size;
        if hdr.fw_size > MAX_FW_SIZE || total > n {
            let _ = writeln!(self.uart, "refused: header says {} firmware bytes, received {}\r", hdr.fw_size, n);
            return;
        }
        if let Err(e) = verify(&hdr, &buf[IMAGE_HEADER_SIZE..total], PUBKEY) {
            let _ = writeln!(self.uart, "refused: {:?}\r", e);
            return;
        }
        let version = hdr.version;
        let (target, before) = match install_begin(&self.state, version) {
            Ok(x) => x,
            Err(e) => {
                let _ = writeln!(self.uart, "refused: {:?}\r", e);
                return;
            }
        };
        let _ = writeln!(self.uart, "verified v{}; installing into slot {:?}\r", version, target);

        // 2. Forget anything that pointed at the target slot, then write it.
        if before != self.state && !self.save(before) {
            return;
        }
        let base = slot_addr(target);
        let t0 = self.timer.counter();
        for i in 0..(SLOT_SIZE as u32 / BLOCK_SIZE) {
            if let Err(e) = self.flash.erase_block(base + i * BLOCK_SIZE) {
                let _ = writeln!(self.uart, "erase failed: {:?}\r", e);
                return;
            }
        }
        let t1 = self.timer.counter();
        if let Err(e) = self.flash.program(base, &buf[..total]) {
            let _ = writeln!(self.uart, "program failed: {:?}\r", e);
            return;
        }
        let t2 = self.timer.counter();

        // 3. Read back before recording it.
        let mut chunk = [0u8; 256];
        let mut off = 0;
        while off < total {
            let len = chunk.len().min(total - off);
            if self.flash.read(base + off as u32, &mut chunk[..len]).is_err() || chunk[..len] != buf[off..off + len] {
                let _ = writeln!(self.uart, "read-back mismatch at {:#x}; slot not marked pending\r", base + off as u32);
                return;
            }
            off += len;
        }
        let _ = writeln!(
            self.uart,
            "written and read back: erase {} ms, program {} ms\r",
            t0.wrapping_sub(t1) / TICKS_PER_MS,
            t1.wrapping_sub(t2) / TICKS_PER_MS
        );

        // 4. Only now is it pending.
        let after = install_done(&self.state, target, version);
        if self.save(after) {
            let _ = writeln!(self.uart, "installed v{} in slot {:?}; it boots on trial next ('b' or reset)\r", version, target);
        }
    }

    /// The host's XMODEM tool takes a moment to stop and start listening.
    fn pause_for_sender(&mut self) {
        while self.read_byte(300).is_some() {}
    }

    // ------------------------------------------------------------ uart

    fn read_byte(&mut self, timeout_ms: u32) -> Option<u8> {
        let t0 = self.timer.counter();
        loop {
            match self.uart.read() {
                Ok(b) => return Some(b),
                Err(nb::Error::WouldBlock) => {}
                // Framing or overrun: the byte is lost; XMODEM's CRC catches it.
                Err(nb::Error::Other(_)) => {}
            }
            if t0.wrapping_sub(self.timer.counter()) / TICKS_PER_MS >= timeout_ms {
                return None;
            }
        }
    }
}

impl Channel for Ctx {
    fn read_byte(&mut self, timeout_ms: u32) -> Option<u8> {
        Ctx::read_byte(self, timeout_ms)
    }
    fn write_byte(&mut self, b: u8) {
        let _ = nb::block!(self.uart.write(b));
    }
}

/// The application region of IRAM, `APP_BASE..APP_BASE+len`.
fn app_ram(len: usize) -> &'static mut [u8] {
    assert!(len <= APP_REGION_SIZE);
    // SAFETY: IRAM below rustBoot that no Rust object owns; `rb_main` checks
    // at startup that APP_BASE + APP_REGION_SIZE does not reach rustBoot's region.
    // Callers use one slice at a time.
    unsafe { core::slice::from_raw_parts_mut(APP_BASE as *mut u8, len) }
}
