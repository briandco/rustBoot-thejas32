//! Read access to the ARIES v3 boot flash (AT25SF161) over SPI.
//!
//! Which controller, and how it is set up, comes from disassembling CDAC's
//! `flasher.bin` (thejas32-rs/tools), not from documentation:
//!
//! - `main` probes and drives the flash as SPI controller **3** (`li a0,3`
//!   before each driver call at 0x2039bc..0x203a28). The driver maps
//!   controller 3 to `0x1020_0200` (base 0x1020_0100, plus 0x100 for odd
//!   numbers, at 0x202590..0x2025a4). That is the HAL's `Spi3`. The README's
//!   memory map names 0x1020_0100, which is `Spi2`.
//! - The control word is chip select 0, SPI mode 3, MSB first, and DBITS = 0
//!   (0x201b08..0x201b5c). The baud code is 2 (0x2025ac), i.e. divide by 16.
//! - Chip select (CSAAT) is held for the opcode and every data byte, and
//!   released only after the last one (`at25sf161Read`, 0x201ed0..0x201f9c).
//!
//! Both of the last two differ from the HAL defaults, and both were measured
//! on hardware (`--features spi-diag`, 2026-11-06):
//!
//! - DBITS 0 reads the ID as 1f:86:01; DBITS 8 (HAL default) reads ff:ff:ff,
//!   with either 16- or 32-bit control writes.
//! - Releasing CSAAT *before* the final byte, as the HAL's `framed` does, read
//!   1f:86:00: the last byte is lost. Holding it through, as here, reads 01.
//! - SPI2 (0x1020_0100) reads 00:00:00, so the flash is not on SPI2.
//!
//! `RxData` behaves as a 16-bit shift register (the read after the 0x86 byte
//! returned 0x8601); the received byte is its low 8 bits, which is what
//! `transfer_byte` returns.
//!
//! The command set is from CDAC's AT25SF161 example
//! (vega-sdk/examples/spi/at25sf161_spi_eeprom): `0x9F` reads the JEDEC ID,
//! `0x03` reads data from a 24-bit address.

//!
//! Write and erase (phase 2) use the same example's opcodes: write-enable
//! `0x06`, page program `0x02` (256-byte pages), 4 KB erase `0x20`, status
//! `0x05` with bit 0 = busy. Status bit 1 is taken to be the write-enable
//! latch, per the standard SPI NOR layout; `write_enable` checks it after every
//! `0x06`, so a wrong assumption shows up as `WriteNotEnabled`, not as a
//! silent failed write.

use thejas32_hal::fence;
use thejas32_hal::pac::Spi3;
use thejas32_hal::spi::{Config, Error, Spi};

use crate::layout::{FLASH_SIZE, RUSTBOOT_SLOT_END};

const CMD_READ_JEDEC_ID: u8 = 0x9F;
const CMD_READ: u8 = 0x03;
const CMD_WRITE_ENABLE: u8 = 0x06;
const CMD_READ_STATUS1: u8 = 0x05;
const CMD_READ_STATUS2: u8 = 0x35;
const CMD_PAGE_PROGRAM: u8 = 0x02;
const CMD_ERASE_4K: u8 = 0x20;
/// 64 KB block erase. Proven on this chip: flasher.bin uses it (`Erase cmd 0xd8`).
const CMD_ERASE_64K: u8 = 0xD8;

const STATUS_BUSY: u8 = 1 << 0;
const STATUS_WEL: u8 = 1 << 1;

/// Smallest erasable unit.
pub const SECTOR_SIZE: u32 = 0x1000;
/// Large erase unit.
pub const BLOCK_SIZE: u32 = 0x1_0000;
/// Largest single program operation; programs never cross a page boundary.
pub const PAGE_SIZE: usize = 256;

/// Status polls before giving up. One poll is a two-byte SPI transaction
/// (a few microseconds at 6.25 MHz), so this is several seconds.
const BUSY_POLL_LIMIT: u32 = 2_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashError {
    Spi(Error),
    /// The chip stayed busy past `BUSY_POLL_LIMIT`.
    Timeout,
    /// Write enable did not set the write-enable latch.
    WriteNotEnabled(u8),
    /// Erase address not sector-aligned.
    Unaligned,
    /// The range touches rustBoot's own slot or runs past the chip.
    OutOfBounds,
}

impl From<Error> for FlashError {
    fn from(e: Error) -> Self {
        FlashError::Spi(e)
    }
}

pub struct Flash {
    spi: Spi<Spi3>,
}

impl Flash {
    pub fn new(spi3: Spi3) -> Self {
        Flash {
            spi: Spi::new(spi3, Config::default().with_bits_per_transfer(0)),
        }
    }

    /// As `new`, with an explicit DBITS field value (diagnostics only).
    pub fn new_with_dbits(spi3: Spi3, dbits: u8) -> Self {
        Flash {
            spi: Spi::new(spi3, Config::default().with_bits_per_transfer(dbits)),
        }
    }

    /// Manufacturer and device ID.
    pub fn jedec_id(&mut self) -> Result<[u8; 3], FlashError> {
        let mut id = [0u8; 3];
        self.transaction(&[CMD_READ_JEDEC_ID], &[], &mut id)?;
        Ok(id)
    }

    /// Reads `buf.len()` bytes starting at flash address `addr`.
    pub fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), FlashError> {
        if addr as usize + buf.len() > FLASH_SIZE {
            return Err(FlashError::OutOfBounds);
        }
        self.transaction(&addr_cmd(CMD_READ, addr), &[], buf)?;
        Ok(())
    }

    pub fn status1(&mut self) -> Result<u8, FlashError> {
        let mut s = [0u8];
        self.transaction(&[CMD_READ_STATUS1], &[], &mut s)?;
        Ok(s[0])
    }

    pub fn status2(&mut self) -> Result<u8, FlashError> {
        let mut s = [0u8];
        self.transaction(&[CMD_READ_STATUS2], &[], &mut s)?;
        Ok(s[0])
    }

    /// Erases the 4 KB sector at `addr` (must be sector-aligned). Returns the
    /// number of status polls the erase took.
    pub fn erase_sector(&mut self, addr: u32) -> Result<u32, FlashError> {
        if addr % SECTOR_SIZE != 0 {
            return Err(FlashError::Unaligned);
        }
        check_writable(addr, SECTOR_SIZE as usize)?;
        self.write_enable()?;
        self.transaction(&addr_cmd(CMD_ERASE_4K, addr), &[], &mut [])?;
        self.wait_ready()
    }

    /// Erases the 64 KB block at `addr` (must be block-aligned).
    pub fn erase_block(&mut self, addr: u32) -> Result<u32, FlashError> {
        if addr % BLOCK_SIZE != 0 {
            return Err(FlashError::Unaligned);
        }
        check_writable(addr, BLOCK_SIZE as usize)?;
        self.write_enable()?;
        self.transaction(&addr_cmd(CMD_ERASE_64K, addr), &[], &mut [])?;
        self.wait_ready()
    }

    /// Programs `data` at `addr`, split at page boundaries. The target must
    /// already be erased: programming can only clear bits.
    pub fn program(&mut self, mut addr: u32, mut data: &[u8]) -> Result<(), FlashError> {
        check_writable(addr, data.len())?;
        while !data.is_empty() {
            let room = PAGE_SIZE - (addr as usize % PAGE_SIZE);
            let n = room.min(data.len());
            self.write_enable()?;
            self.transaction(&addr_cmd(CMD_PAGE_PROGRAM, addr), &data[..n], &mut [])?;
            self.wait_ready()?;
            addr += n as u32;
            data = &data[n..];
        }
        Ok(())
    }

    fn write_enable(&mut self) -> Result<(), FlashError> {
        self.transaction(&[CMD_WRITE_ENABLE], &[], &mut [])?;
        let s = self.status1()?;
        if s & STATUS_WEL == 0 {
            return Err(FlashError::WriteNotEnabled(s));
        }
        Ok(())
    }

    fn wait_ready(&mut self) -> Result<u32, FlashError> {
        for polls in 0..BUSY_POLL_LIMIT {
            if self.status1()? & STATUS_BUSY == 0 {
                return Ok(polls);
            }
        }
        Err(FlashError::Timeout)
    }

    /// One chip-select assertion: `cmd`, then `tx`, then clocks in
    /// `rx.len()` bytes. CSAAT is set before the first byte and cleared after
    /// the last, as flasher.bin does; clearing it before the last byte loses
    /// that byte (measured, see the module docs). Each CSAAT write is fenced
    /// before the data write that depends on it, since this bus can reorder
    /// writes to different registers of one peripheral.
    fn transaction(&mut self, cmd: &[u8], tx: &[u8], rx: &mut [u8]) -> Result<(), Error> {
        self.spi.wait_if_busy();
        self.spi.set_csaat(true);
        fence();
        let result = (|| {
            for &byte in cmd.iter().chain(tx) {
                self.spi.transfer_byte(byte)?;
            }
            for slot in rx.iter_mut() {
                *slot = self.spi.transfer_byte(0)?;
            }
            Ok(())
        })();
        self.spi.wait_if_busy();
        self.spi.set_csaat(false);
        fence();
        result
    }
}

fn addr_cmd(op: u8, addr: u32) -> [u8; 4] {
    [op, (addr >> 16) as u8, (addr >> 8) as u8, addr as u8]
}

/// Never touch rustBoot's own slot, and never run past the chip.
fn check_writable(addr: u32, len: usize) -> Result<(), FlashError> {
    if (addr as usize) < RUSTBOOT_SLOT_END || addr as usize + len > FLASH_SIZE {
        return Err(FlashError::OutOfBounds);
    }
    Ok(())
}

impl rb_state::log::Nor for Flash {
    type Error = FlashError;
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), FlashError> {
        Flash::read(self, addr, buf)
    }
    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), FlashError> {
        Flash::program(self, addr, data)
    }
    fn erase_sector(&mut self, addr: u32) -> Result<(), FlashError> {
        Flash::erase_sector(self, addr).map(|_| ())
    }
}
