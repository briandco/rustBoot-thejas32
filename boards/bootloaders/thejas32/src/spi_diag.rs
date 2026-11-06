//! SPI flash diagnostic (`--features spi-diag`). Not part of the bootloader.
//!
//! First hardware run: the HAL `Spi` read the flash JEDEC ID on SPI3 as
//! 00:00:00, seconds after CDAC's flasher.bin read 1f:86:01 on the same bus.
//! This tries each difference between the two, one at a time, with raw
//! register access that mirrors flasher.bin's disassembly:
//!
//! - control register written 16-bit (`sh`, as flasher.bin) or 32-bit (`sw`, as the PAC)
//! - DBITS field 0 (as flasher.bin) or 8 (as the HAL)
//! - controller SPI3 (flasher.bin) or SPI2 (thejas32-rs README address)
//!
//! Register offsets and status bits, all matching the PAC: control +0x0,
//! status +0x4 (busy bit 4, rx_complete bit 6, tx_hold_empty bit 7),
//! baud +0x8, txdata +0xC, rxdata +0x10 (read as 16-bit).

use core::fmt::Write;
use core::ptr::{read_volatile, write_volatile};
use thejas32_hal::fence;

const SPI2: usize = 0x1020_0100;
const SPI3: usize = 0x1020_0200;

const ST_BUSY: u32 = 1 << 4;
const ST_RX_COMPLETE: u32 = 1 << 6;
const ST_TX_HOLD_EMPTY: u32 = 1 << 7;
const CSAAT: u16 = 1 << 8;
/// Mode 3 (bits 5:4), MSB first, fixed peripheral, CS0, interrupts off.
const CTRL_BASE: u16 = 0b11 << 4;
const SPIN_LIMIT: u32 = 200_000;

struct Raw {
    base: usize,
    half: bool,
    ctrl: u16,
    timeouts: u32,
}

impl Raw {
    fn status(&self) -> u32 {
        unsafe { read_volatile((self.base + 4) as *const u32) }
    }
    fn write_ctrl(&mut self, v: u16) {
        self.ctrl = v;
        unsafe {
            if self.half {
                write_volatile(self.base as *mut u16, v);
            } else {
                write_volatile(self.base as *mut u32, v as u32);
            }
        }
        fence();
    }
    fn wait(&mut self, mask: u32, want_set: bool) {
        for _ in 0..SPIN_LIMIT {
            if (self.status() & mask != 0) == want_set {
                return;
            }
        }
        self.timeouts += 1;
    }
    fn tx(&mut self, b: u8) {
        self.wait(ST_BUSY, false);
        self.wait(ST_TX_HOLD_EMPTY, true);
        unsafe { write_volatile((self.base + 0xC) as *mut u32, b as u32) };
        fence();
    }
    fn rx(&mut self) -> u16 {
        self.wait(ST_RX_COMPLETE, true);
        unsafe { read_volatile((self.base + 0x10) as *const u16) }
    }
    /// flasher.bin's at25sf161 read: CSAAT on, opcode, then for each byte
    /// transmit 0 and receive; CSAAT off.
    fn read_id(&mut self) -> [u16; 3] {
        let mut id = [0u16; 3];
        self.wait(ST_BUSY, false);
        self.write_ctrl(self.ctrl | CSAAT);
        self.tx(0x9F);
        for slot in id.iter_mut() {
            self.tx(0);
            *slot = self.rx();
        }
        self.write_ctrl(self.ctrl & !CSAAT);
        id
    }
}

fn run_variant<W: Write>(out: &mut W, name: &str, base: usize, half: bool, dbits: u16) {
    let mut r = Raw { base, half, ctrl: 0, timeouts: 0 };
    // Baud code 2 (divide by 16), as flasher.bin.
    unsafe { write_volatile((base + 8) as *mut u32, 2 << 4) };
    fence();
    r.write_ctrl(CTRL_BASE | (dbits << 9));
    let ctrl_rb = unsafe { read_volatile(base as *const u32) };
    let id = r.read_id();
    let id2 = r.read_id();
    let _ = writeln!(
        out,
        "  {:<28} ctrl={:#06x} id={:04x}:{:04x}:{:04x} again={:04x}:{:04x}:{:04x} timeouts={}\r",
        name, ctrl_rb, id[0], id[1], id[2], id2[0], id2[1], id2[2], r.timeouts
    );
}

pub fn run<W: Write>(out: &mut W) {
    let _ = writeln!(out, "SPI flash ID diagnostic (want 1f:86:01)\r");
    run_variant(out, "SPI3 sh dbits=0 (flasher)", SPI3, true, 0);
    run_variant(out, "SPI3 sh dbits=8", SPI3, true, 8);
    run_variant(out, "SPI3 sw dbits=0", SPI3, false, 0);
    run_variant(out, "SPI3 sw dbits=8 (HAL)", SPI3, false, 8);
    run_variant(out, "SPI2 sh dbits=0", SPI2, true, 0);
    run_variant(out, "SPI2 sw dbits=8", SPI2, false, 8);
    // And the HAL driver itself, configured like flasher.bin.
    let p = unsafe { thejas32_hal::pac::Peripherals::steal() };
    let mut f = rb_core::flash::Flash::new_with_bits(p.spi3, 8);
    let _ = writeln!(out, "  HAL Flash 8-bit (DBITS 0)     id={:02x?}\r", f.jedec_id());
    let p = unsafe { thejas32_hal::pac::Peripherals::steal() };
    let mut f = rb_core::flash::Flash::new_with_bits(p.spi3, 16);
    let _ = writeln!(out, "  HAL Flash 16-bit (DBITS 8)    id={:02x?}\r", f.jedec_id());
    let _ = writeln!(out, "done\r");
}
