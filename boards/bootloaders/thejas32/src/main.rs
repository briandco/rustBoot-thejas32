//! rustBoot for THEJAS32 (ARIES v3): verified boot, with A/B updates.
//!
//! Boot sequence:
//!
//! 1. The ROM (BOOT_SEL jumper J12 fitted) copies the start of SPI flash into
//!    IRAM at 0x200000 and jumps to it.
//! 2. `_reloc_start` moves rustBoot to the top 96 KB of IRAM (see `rustboot.x`).
//! 3. `rb_main` checks the flash, loads the boot state, and offers a short
//!    window to enter recovery (send 'r').
//! 4. `boot::Ctx::run` decides which slot to boot (`rb_state::plan`): it copies
//!    each candidate into RAM at 0x200000 and verifies the SHA-256 and ECDSA
//!    P-256 signature of that copy, and boots the first that passes.
//! 5. It jumps to 0x200000. With nothing bootable, it stays in recovery.
//!
//! Updates are received in recovery mode and written to the inactive slot;
//! see `rb_state` for the model and `boot.rs` for the install sequence.

#![no_std]
#![no_main]

mod boot;
#[cfg(feature = "spi-diag")]
mod spi_diag;
#[cfg(feature = "flash-selftest")]
mod flash_selftest;

use core::arch::{asm, global_asm};
use core::fmt::Write;

use rb_core::flash::Flash;
use rb_core::layout::*;
use thejas32_hal::pac;
use thejas32_hal::serial::Serial;

const _: () = assert!(IMAGE_HEADER_SIZE == rustBoot::rbconstants::IMAGE_HEADER_SIZE);

/// The public half of the signing key (`tools/keygen.py`). Images signed with
/// any other key are refused.
pub static PUBKEY: &[u8; 64] = include_bytes!("../keys/pubkey.bin");

/// Marker in the last bytes of the image (section `.image_tail`).
const TAIL_MAGIC: [u8; 8] = *b"RBTJ32\xA5\x5A";
#[used]
#[link_section = ".image_tail"]
static IMAGE_TAIL: [u8; 8] = TAIL_MAGIC;

extern "C" {
    static __image_start: u8;
    static __ebss: u8;
    static __stack_top: u8;
}

// Startup. Runs before any Rust code, with no stack.
//
// `_reloc_start` runs at the address the ROM loaded it to, so it only uses
// PC-relative references to itself and absolute (`lui`/`addi`) references to
// link-time symbols. It:
//   - masks every interrupt source through `mie`, leaving `mstatus` exactly as
//     the ROM set it (read-modify-writing `mstatus` has broken interrupts on
//     this SoC before);
//   - saves the ROM's `mtvec` in s2, to restore before entering the app;
//   - copies the image to its link address;
//   - executes `fence.i` under a temporary trap handler that skips the
//     instruction and sets s1 if it traps. Whether this core implements
//     `fence.i` has not been established, so this both stays safe and
//     reports the answer.
global_asm!(
    r#"
    .section .text.reloc, "ax"
    .global _reloc_start
_reloc_start:
    auipc t0, 0
    csrw  mie, zero
    csrr  s2, mtvec
    lui   t1, %hi(__image_start)
    addi  t1, t1, %lo(__image_start)
    lui   t2, %hi(__image_end)
    addi  t2, t2, %lo(__image_end)
    beq   t0, t1, 2f
1:
    lw    t3, 0(t0)
    sw    t3, 0(t1)
    addi  t0, t0, 4
    addi  t1, t1, 4
    bltu  t1, t2, 1b
2:
    fence
    li    s1, 0
    lla   t0, 3f
    csrw  mtvec, t0
    fence.i
    j     4f
    .balign 4
3:
    csrr  t3, mepc
    addi  t3, t3, 4
    csrw  mepc, t3
    li    s1, 1
    mret
4:
    lui   t0, %hi(_rb_start)
    addi  t0, t0, %lo(_rb_start)
    jr    t0

    .section .text._rb_start, "ax"
    .global _rb_start
_rb_start:
    lui   sp, %hi(__stack_top)
    addi  sp, sp, %lo(__stack_top)
    lui   t0, %hi(_rb_trap)
    addi  t0, t0, %lo(_rb_trap)
    csrw  mtvec, t0
    lui   t0, %hi(__sbss)
    addi  t0, t0, %lo(__sbss)
    lui   t1, %hi(__ebss)
    addi  t1, t1, %lo(__ebss)
    bgeu  t0, t1, 2f
1:
    sw    zero, 0(t0)
    addi  t0, t0, 4
    bltu  t0, t1, 1b
2:
    # Paint the (still unused) stack, so rb_main can report its high-water mark.
    lui   t2, %hi({paint})
    addi  t2, t2, %lo({paint})
    mv    t0, t1
3:
    bgeu  t0, sp, 4f
    sw    t2, 0(t0)
    addi  t0, t0, 4
    j     3b
4:
    mv    a0, s1
    mv    a1, s2
    call  rb_main
5:
    j     5b

    .balign 4
_rb_trap:
    csrr  a0, mcause
    csrr  a1, mepc
    csrr  a2, mtval
    call  rb_trap
"#,
    paint = const STACK_PAINT,
);

const STACK_PAINT: u32 = 0x5AA5_C33C;

/// Bytes of stack used so far: the distance from the top to the lowest word
/// that no longer holds the paint pattern.
fn stack_high_water() -> (usize, usize) {
    let bottom = unsafe { &__ebss as *const u8 as usize };
    let top = unsafe { &__stack_top as *const u8 as usize };
    let mut addr = bottom;
    while addr < top && unsafe { core::ptr::read_volatile(addr as *const u32) } == STACK_PAINT {
        addr += 4;
    }
    (top - addr, top - bottom)
}

fn console() -> Serial<pac::Uart0> {
    // SAFETY: rustBoot is single-threaded with interrupts masked, and every
    // user of UART0 is a blocking writer, so stealing it cannot race.
    let p = unsafe { pac::Peripherals::steal() };
    Serial::new_preconfigured(p.uart0)
}

fn halt(uart: &mut Serial<pac::Uart0>, why: core::fmt::Arguments) -> ! {
    let _ = writeln!(uart, "rustBoot: HALT: {}\r", why);
    loop {}
}

#[no_mangle]
extern "C" fn rb_main(fencei_trapped: u32, rom_mtvec: usize) -> ! {
    let mut uart = console();
    let _ = writeln!(uart, "\r\nrustBoot {} (THEJAS32)\r", env!("CARGO_PKG_VERSION"));

    // 1. Did the ROM load all of us? The tail marker is the last thing in the
    //    image; read it through a volatile pointer so the compiler cannot
    //    substitute the constant.
    let tail = unsafe { core::ptr::read_volatile(&IMAGE_TAIL) };
    if tail != TAIL_MAGIC {
        halt(&mut uart, format_args!("image tail marker missing: the ROM loaded only part of rustBoot"));
    }
    let _ = writeln!(
        uart,
        "  fence.i: {}\r",
        if fencei_trapped != 0 { "traps (not implemented); skipping it" } else { "ok" }
    );

    #[cfg(feature = "spi-diag")]
    {
        spi_diag::run(&mut uart);
        loop {}
    }
    #[cfg(feature = "flash-selftest")]
    {
        flash_selftest::run(&mut uart);
        loop {}
    }

    // 2. Never overwrite ourselves: the app region (also the upload buffer)
    //    must end below rustBoot.
    let rb_start = unsafe { &__image_start as *const u8 as usize };
    if APP_BASE + APP_REGION_SIZE > rb_start {
        halt(&mut uart, format_args!("layout error: app region overlaps rustBoot at {:#x}", rb_start));
    }

    // 3. The flash must be the part we expect.
    let p = unsafe { pac::Peripherals::steal() };
    let mut flash = Flash::new(p.spi3);
    let id = match flash.jedec_id() {
        Ok(id) => id,
        Err(e) => halt(&mut uart, format_args!("SPI error reading flash ID: {:?}", e)),
    };
    let _ = writeln!(uart, "  flash: JEDEC {:02x}:{:02x}:{:02x}\r", id[0], id[1], id[2]);
    if id != EXPECTED_JEDEC_ID {
        halt(&mut uart, format_args!("unexpected flash ID (want 1f:86:01, AT25SF161)"));
    }

    // 4. Decide, possibly via recovery. The chosen image is verified and in RAM.
    let mut ctx = match boot::Ctx::new(uart, flash, p.timer0) {
        Ok(c) => c,
        Err(e) => halt(&mut console(), format_args!("cannot read boot state: {:?}", e)),
    };
    let chosen = ctx.run();
    let uart = &mut ctx.uart;
    let (used, avail) = stack_high_water();
    let _ = writeln!(uart, "  stack: {} of {} bytes used\r", used, avail);
    let _ = writeln!(
        uart,
        "  booting slot {:?} v{}{}; jumping to {:#x}\r\n",
        chosen.slot,
        chosen.version,
        if chosen.trial { " ON TRIAL (must confirm)" } else { "" },
        APP_BASE
    );
    uart.flush(); // blocks until the last byte has left the shift register

    // 5. Hand over with the CSR state the ROM would have left: interrupt
    //    sources masked, its trap vector back in place.
    // SAFETY: the firmware at APP_BASE was just verified; control never returns.
    unsafe {
        asm!("csrw mie, zero", "csrw mtvec, {v}", "fence", v = in(reg) rom_mtvec);
        if fencei_trapped == 0 {
            asm!("fence.i");
        }
        asm!("jr {0}", in(reg) APP_BASE, options(noreturn));
    }
}

#[no_mangle]
extern "C" fn rb_trap(mcause: usize, mepc: usize, mtval: usize) -> ! {
    let mut uart = console();
    halt(
        &mut uart,
        format_args!("trap: mcause={:#x} mepc={:#x} mtval={:#x}", mcause, mepc, mtval),
    );
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let mut uart = console();
    halt(&mut uart, format_args!("panic: {}", info));
}
