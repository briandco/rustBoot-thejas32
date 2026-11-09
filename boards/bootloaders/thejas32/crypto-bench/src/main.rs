//! Times the crypto rustBoot uses, on THEJAS32, as a baseline for choosing
//! other schemes (Ed25519, LMS).
//!
//! Installed like any update (it is a signed image). It never confirms, so
//! the reboot after it (`R`) rolls the board back to the previous version.
//!
//! Measures, with TIMER0 free-running at 100 MHz:
//! 1. SHA-256 throughput over 64 KiB;
//! 2. SHA-256 of a 55-byte message (one compression), which is what LMS and
//!    other hash-based signatures are made of;
//! 3. reading this image from flash, and rustBoot's full `verify` of it
//!    (SHA-256 + ECDSA P-256), with the hash part timed separately so the
//!    signature part can be told apart.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::hint::black_box;
use embedded_hal_nb::serial::Read;
use rb_core::flash::Flash;
use rb_core::layout::{slot_addr, IMAGE_HEADER_SIZE, LMS_TRAILER_LEN};
use rustBoot::verify::{parse_header, verify, verify_lms};
use sha2::{Digest, Sha256};
use thejas32_hal::pac;
use thejas32_hal::serial::Serial;
use thejas32_hal::timer::Timer;

const VERSION: &str = match option_env!("BENCH_VERSION") {
    Some(v) => v,
    None => "0",
};
static PUBKEY: &[u8; 64] = include_bytes!("../../keys/pubkey.bin");
static LMS_PUBKEY: &[u8; 60] = include_bytes!("../../keys/lms_pubkey.bin");
const TICKS_PER_US: u32 = 100;
const BUF_LEN: usize = 0x1_0000;
static mut BUF: [u8; BUF_LEN] = [0; BUF_LEN];

struct Clock(Timer<pac::Timer0>);
impl Clock {
    fn now(&self) -> u32 {
        self.0.counter()
    }
    /// Microseconds since `t0` (down-counter).
    fn us(&self, t0: u32) -> u32 {
        t0.wrapping_sub(self.0.counter()) / TICKS_PER_US
    }
}

#[riscv_rt::entry]
fn main() -> ! {
    let p = unsafe { pac::Peripherals::steal() };
    let mut uart = Serial::new_preconfigured(p.uart0);
    let mut timer = Timer::new(p.timer0);
    timer.start_free_running();
    let clk = Clock(timer);
    let mut flash = Flash::new(p.spi3);
    // SAFETY: the only reference to BUF, single-threaded.
    let buf: &mut [u8; BUF_LEN] = unsafe { &mut *core::ptr::addr_of_mut!(BUF) };

    let _ = writeln!(uart, "crypto-bench v{} (never confirms; 'R' reboots and rolls back)\r", VERSION);

    // 1. Bulk SHA-256.
    for (i, b) in buf.iter_mut().enumerate() {
        *b = i as u8;
    }
    for run in 0..3 {
        let t0 = clk.now();
        let d = Sha256::digest(black_box(&buf[..]));
        let us = clk.us(t0);
        black_box(d);
        let _ = writeln!(
            uart,
            "  sha256 64 KiB, run {}: {} us = {} KB/s\r",
            run + 1,
            us,
            (BUF_LEN as u64 * 1_000_000 / 1024 / us.max(1) as u64)
        );
    }

    // 2. One-block SHA-256, 1000 times.
    let msg = [0x5Au8; 55];
    let t0 = clk.now();
    for i in 0..1000u32 {
        let mut h = Sha256::new();
        h.update(black_box(&msg));
        h.update(i.to_le_bytes());
        black_box(h.finalize());
    }
    let us = clk.us(t0);
    let _ = writeln!(uart, "  sha256 one block (59 B) x1000: {} us = {}.{} us per hash\r", us, us / 1000, (us % 1000) / 100);

    // 3. This image, from flash, verified as rustBoot does.
    let log = rb_core::state_log();
    let slot = match log.load(&mut flash) {
        Ok((s, _)) => s.trial.map(|(slot, _)| slot).unwrap_or(s.active),
        Err(e) => {
            let _ = writeln!(uart, "  cannot read boot state: {:?}\r", e);
            idle(&mut uart)
        }
    };
    let base = slot_addr(slot);
    let mut hdr = [0u8; IMAGE_HEADER_SIZE];
    let _ = flash.read(base, &mut hdr);
    let h = match parse_header(&hdr) {
        Ok(h) => h,
        Err(e) => {
            let _ = writeln!(uart, "  own slot {:?}: header error {}\r", slot, e);
            idle(&mut uart)
        }
    };
    let fw = &mut buf[..h.fw_size.min(BUF_LEN)];
    let t0 = clk.now();
    let _ = flash.read(base + IMAGE_HEADER_SIZE as u32, fw);
    let read_us = clk.us(t0);
    let _ = writeln!(uart, "  own image: slot {:?}, v{}, {} bytes; flash read {} us\r", slot, h.version, h.fw_size, read_us);

    for run in 0..3 {
        let t0 = clk.now();
        let r = verify(&h, fw, PUBKEY);
        let total = clk.us(t0);
        // The hash rustBoot computes: header up to the digest field (offset
        // 44 in rbsigner's layout) plus the firmware.
        let t1 = clk.now();
        let mut hh = Sha256::new();
        hh.update(&hdr[..44]);
        hh.update(&fw[..]);
        black_box(hh.finalize());
        let hash = clk.us(t1);
        let _ = writeln!(
            uart,
            "  verify run {}: {:?}, total {} us = sha256 {} us + ecdsa-p256 ~{} us\r",
            run + 1,
            r,
            total,
            hash,
            total.saturating_sub(hash)
        );
    }
    // 4. The LMS half of the hybrid check, over the same digest.
    let mut trailer = [0u8; LMS_TRAILER_LEN];
    let _ = flash.read(base + (IMAGE_HEADER_SIZE + h.fw_size) as u32, &mut trailer);
    for run in 0..3 {
        let t0 = clk.now();
        let r = verify_lms(h.digest(), &trailer, LMS_PUBKEY);
        let us = clk.us(t0);
        let _ = writeln!(uart, "  lms (H10/W4) verify run {}: {:?}, {} us\r", run + 1, r, us);
    }
    let _ = writeln!(uart, "bench done\r");
    idle(&mut uart)
}

fn idle(uart: &mut Serial<pac::Uart0>) -> ! {
    loop {
        if let Ok(b'R') = uart.read() {
            let _ = writeln!(uart, "rebooting\r");
            uart.flush();
            rb_core::reboot();
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
