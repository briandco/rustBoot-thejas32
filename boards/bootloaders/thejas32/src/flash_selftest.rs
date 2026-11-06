//! Flash erase/program self-test (`--features flash-selftest`). Not part of
//! the bootloader. Load it into RAM over UART; it writes only the last 4 KB
//! sector of the chip and checks that nothing else changed.

use core::fmt::Write;
use thejas32_hal::pac;
use thejas32_hal::timer::Timer;

use rb_core::flash::{Flash, FlashError, PAGE_SIZE, SECTOR_SIZE};
use rb_core::layout::{BOOT_PART_OFFSET, FLASH_SIZE};

const TEST_SECTOR: u32 = FLASH_SIZE as u32 - SECTOR_SIZE; // 0x1FF000
const TICKS_PER_US: u32 = 100; // 100 MHz, measured (thejas32-rs clock-probe)

fn pattern(i: usize, salt: u8) -> u8 {
    ((i * 7 + 3) as u8) ^ ((i >> 8) as u8) ^ salt
}

struct Check<'a, W: Write> {
    out: &'a mut W,
    passed: u32,
    total: u32,
}

impl<W: Write> Check<'_, W> {
    fn check(&mut self, name: &str, ok: bool) {
        self.total += 1;
        if ok {
            self.passed += 1;
        }
        let _ = writeln!(self.out, "  [{}] {:<44} {}\r", self.total, name, if ok { "PASS" } else { "FAIL" });
    }
}

fn all_ff(flash: &mut Flash, addr: u32, len: usize) -> Result<bool, FlashError> {
    let mut buf = [0u8; 256];
    let mut off = 0;
    while off < len {
        let n = buf.len().min(len - off);
        flash.read(addr + off as u32, &mut buf[..n])?;
        if buf[..n].iter().any(|&b| b != 0xFF) {
            return Ok(false);
        }
        off += n;
    }
    Ok(true)
}

fn matches(flash: &mut Flash, addr: u32, len: usize, salt: u8, start: usize) -> Result<bool, FlashError> {
    let mut buf = [0u8; 256];
    let mut off = 0;
    while off < len {
        let n = buf.len().min(len - off);
        flash.read(addr + off as u32, &mut buf[..n])?;
        if (0..n).any(|i| buf[i] != pattern(start + off + i, salt)) {
            return Ok(false);
        }
        off += n;
    }
    Ok(true)
}

fn run_inner<W: Write>(out: &mut W) -> Result<(u32, u32), FlashError> {
    let p = unsafe { pac::Peripherals::steal() };
    let mut flash = Flash::new(p.spi3);
    let mut timer = Timer::new(p.timer0);
    timer.start_free_running();
    let us = |t0: u32, t1: u32| t0.wrapping_sub(t1) / TICKS_PER_US; // down-counter

    let id = flash.jedec_id()?;
    let s1 = flash.status1()?;
    let s2 = flash.status2()?;
    let _ = writeln!(out, "flash self-test: JEDEC {:02x?}, SR1={:#04x} SR2={:#04x}, test sector {:#x}\r", id, s1, s2, TEST_SECTOR);

    // Snapshots of regions that must not change.
    let mut rb_before = [0u8; 64];
    flash.read(0, &mut rb_before)?;
    let mut hdr_before = [0u8; 64];
    flash.read(BOOT_PART_OFFSET, &mut hdr_before)?;

    let mut c = Check { out, passed: 0, total: 0 };

    // 1. Erase.
    let t0 = timer.counter();
    let polls = flash.erase_sector(TEST_SECTOR)?;
    let t1 = timer.counter();
    let _ = writeln!(c.out, "      4 KB erase: {} us, {} status polls\r", us(t0, t1), polls);
    c.check("erase leaves 4096 bytes of 0xFF", all_ff(&mut flash, TEST_SECTOR, SECTOR_SIZE as usize)?);

    // 2. One aligned page.
    let mut page = [0u8; PAGE_SIZE];
    for (i, b) in page.iter_mut().enumerate() {
        *b = pattern(i, 0x00);
    }
    let t0 = timer.counter();
    flash.program(TEST_SECTOR, &page)?;
    let t1 = timer.counter();
    let _ = writeln!(c.out, "      256 B page program: {} us\r", us(t0, t1));
    c.check("page reads back exactly", matches(&mut flash, TEST_SECTOR, PAGE_SIZE, 0x00, 0)?);
    c.check("next page still erased", all_ff(&mut flash, TEST_SECTOR + 256, 256)?);

    // 3. 300 bytes from 0x180: crosses the page boundary at 0x200.
    let mut buf = [0u8; 300];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = pattern(i, 0x5A);
    }
    flash.program(TEST_SECTOR + 0x180, &buf)?;
    c.check("page-crossing 300 B write reads back", matches(&mut flash, TEST_SECTOR + 0x180, 300, 0x5A, 0)?);
    c.check("bytes before it (0x100..0x180) untouched", all_ff(&mut flash, TEST_SECTOR + 0x100, 0x80)?);
    c.check("bytes after it (0x2AC..) untouched", all_ff(&mut flash, TEST_SECTOR + 0x180 + 300, 0x100)?);
    c.check("first page still intact", matches(&mut flash, TEST_SECTOR, PAGE_SIZE, 0x00, 0)?);

    // 4. The chip really is 2 MB: a 1 MB part would alias the test sector to 0x0FF000.
    let mut alias = [0u8; 16];
    flash.read(TEST_SECTOR - 0x10_0000, &mut alias)?;
    c.check("no aliasing at -1 MB (chip >= 2 MB)", (0..16).any(|i| alias[i] != pattern(i, 0x00)));

    // 5. The driver refuses rustBoot's slot without touching the chip.
    c.check("erase of sector 0 refused", flash.erase_sector(0) == Err(FlashError::OutOfBounds));
    c.check("program at 0x100 refused", flash.program(0x100, &[0]) == Err(FlashError::OutOfBounds));

    // 6. Clean up, and confirm nothing else moved.
    flash.erase_sector(TEST_SECTOR)?;
    c.check("second erase restores 0xFF", all_ff(&mut flash, TEST_SECTOR, SECTOR_SIZE as usize)?);
    let mut rb_after = [0u8; 64];
    flash.read(0, &mut rb_after)?;
    c.check("rustBoot slot unchanged", rb_before == rb_after);
    let mut hdr_after = [0u8; 64];
    flash.read(BOOT_PART_OFFSET, &mut hdr_after)?;
    c.check("BOOT partition header unchanged", hdr_before == hdr_after);

    Ok((c.passed, c.total))
}

pub fn run<W: Write>(out: &mut W) {
    match run_inner(out) {
        Ok((p, t)) => {
            let _ = writeln!(out, "result: {}/{} checks passed\r", p, t);
        }
        Err(e) => {
            let _ = writeln!(out, "result: ABORTED, flash error {:?}\r", e);
        }
    }
}
