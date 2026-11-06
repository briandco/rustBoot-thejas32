//! rustBoot update test application.
//!
//! Build-time settings (environment variables):
//! - `DEMO_VERSION`: the number it prints; sign it with the same version.
//! - `DEMO_CONFIRM=0`: never confirm, to test rollback. Default: confirm.

#![no_std]
#![no_main]

use core::fmt::Write;
use thejas32_hal::pac;
use thejas32_hal::serial::Serial;
use thejas32_hal::timer::Timer;

const VERSION: &str = match option_env!("DEMO_VERSION") {
    Some(v) => v,
    None => "0",
};
const CONFIRM: bool = match option_env!("DEMO_CONFIRM") {
    Some(v) => !(v.len() == 1 && v.as_bytes()[0] == b'0'),
    None => true,
};

#[riscv_rt::entry]
fn main() -> ! {
    let p = unsafe { pac::Peripherals::steal() };
    let mut uart = Serial::new_preconfigured(p.uart0);
    let _ = writeln!(uart, "demo-app v{} running (confirm: {})\r", VERSION, if CONFIRM { "yes" } else { "NO" });

    if CONFIRM {
        let mut flash = rb_core::flash::Flash::new(p.spi3);
        match rb_core::confirm(&mut flash) {
            Ok(true) => { let _ = writeln!(uart, "demo-app v{}: confirmed, this image is now active\r", VERSION); }
            Ok(false) => { let _ = writeln!(uart, "demo-app v{}: not on trial, nothing to confirm\r", VERSION); }
            Err(e) => { let _ = writeln!(uart, "demo-app v{}: confirm FAILED: {:?}\r", VERSION, e); }
        }
    }

    let mut timer = Timer::new(p.timer0);
    let mut n = 0u32;
    loop {
        timer.delay_ticks(100_000_000); // 1 s at 100 MHz
        n += 1;
        let _ = writeln!(uart, "demo-app v{} alive {}\r", VERSION, n);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
