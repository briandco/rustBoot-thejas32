//! Memory and flash layout. `rustboot.x` and `tools/pack.py` must agree.
//!
//! ```text
//! flash 0x000000  rustBoot (128 KB slot; the ROM loads the first 250 KB)
//!       0x020000  unused: CDAC's flasher.bin erases 0..0x40000 whenever
//!                 rustBoot is (re)provisioned, so nothing else lives here
//!       0x040000  slot A (192 KB): 256-byte header + firmware
//!       0x070000  slot B (192 KB)
//!       0x0A0000  boot state log (two 4 KB sectors)
//!       0x0A2000  free
//!
//! IRAM  0x200000  application (and the update receive buffer), 160 KB
//!       0x228000  rustBoot, 96 KB including its stack
//! ```
//!
//! With no state record, slot A is active.
//!
//! History: phase 1 put the image at 0x10000 inside a 64 KB rustBoot slot;
//! phase 2 moved to slots at 0x20000/0x50000, which reprovisioning rustBoot
//! partly erased. Apps and state now sit entirely above the flasher's reach.

use rb_state::Slot;

/// rustBoot's image header size. The bootloader asserts this equals
/// `rustBoot::rbconstants::IMAGE_HEADER_SIZE`.
pub const IMAGE_HEADER_SIZE: usize = 0x100;

/// Where applications are linked and entered: the start of IRAM. The ROM also
/// loads XMODEM images here, so the same build runs with or without rustBoot.
pub const APP_BASE: usize = 0x20_0000;

/// Size of the AT25SF161 (16 Mbit). Measured: no aliasing at -1 MB.
pub const FLASH_SIZE: usize = 0x20_0000;

/// rustBoot's own slot at the start of flash. The flash driver refuses to
/// erase or program anything below this, so rustBoot cannot brick itself.
pub const RUSTBOOT_SLOT_END: usize = 0x2_0000;

/// IRAM available to the application, from `APP_BASE` up to rustBoot
/// (`rustboot.x`). Also the update receive buffer, so it bounds image size.
pub const APP_REGION_SIZE: usize = 0x2_8000;

/// Each slot: a 256-byte header then the firmware. 64 KB-aligned and a whole
/// number of 64 KB blocks, so a slot erases in three block erases.
pub const SLOT_SIZE: usize = 0x3_0000;
pub const SLOT_A: u32 = 0x4_0000;
pub const SLOT_B: u32 = 0x7_0000;

/// The two sectors of the boot state log.
pub const STATE_SECTORS: [u32; 2] = [0xA_0000, 0xA_1000];

/// Largest firmware: limited by RAM (the image runs from, and is received
/// into, the application region), not by the 192 KB flash slot.
pub const MAX_FW_SIZE: usize = APP_REGION_SIZE - IMAGE_HEADER_SIZE;

/// Slot A under its phase 1 name, used by the flash self-test.
pub const BOOT_PART_OFFSET: u32 = SLOT_A;

/// End of the region CDAC's flasher.bin erases (four 64 KB block erases).
pub const FLASHER_ERASE_END: usize = 0x4_0000;

/// JEDEC ID of the ARIES v3 boot flash, an AT25SF161: manufacturer 0x1F,
/// device 0x86 0x01. Measured on this board.
pub const EXPECTED_JEDEC_ID: [u8; 3] = [0x1F, 0x86, 0x01];

pub fn slot_addr(slot: Slot) -> u32 {
    match slot {
        Slot::A => SLOT_A,
        Slot::B => SLOT_B,
    }
}

const _: () = assert!(SLOT_A as usize >= RUSTBOOT_SLOT_END);
// Reprovisioning rustBoot must never touch apps or state.
const _: () = assert!(SLOT_A as usize >= FLASHER_ERASE_END);
const _: () = assert!(SLOT_A as usize + SLOT_SIZE <= SLOT_B as usize);
const _: () = assert!(SLOT_B as usize + SLOT_SIZE <= STATE_SECTORS[0] as usize);
const _: () = assert!((STATE_SECTORS[1] as usize) + 0x1000 <= FLASH_SIZE);
const _: () = assert!(APP_REGION_SIZE <= SLOT_SIZE);
