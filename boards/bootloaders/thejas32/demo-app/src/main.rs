//! rustBoot update test application.
//!
//! Build-time settings (environment variables):
//! - `DEMO_VERSION`: the number it prints; sign it with the same version.
//! - `DEMO_CONFIRM=0`: never confirm, to test rollback. Default: confirm.
//!
//! Console commands while running (UART0, 115200):
//! - `u`: receive a signed image over XMODEM, install it into the inactive
//!   slot while running, and reboot into it (`rb_core::Installer`).
//! - `R`: reboot (`rb_core::reboot`).

#![no_std]
#![no_main]

use core::fmt::Write;
use embedded_hal_nb::serial::{Read, Write as _};
use rb_core::flash::Flash;
use rb_core::rb_state::xmodem::{self, Channel, Sink};
use rb_core::{Installer, UpdateError};
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
const TICKS_PER_MS: u32 = 100_000; // 100 MHz, measured

struct Console {
    uart: Serial<pac::Uart0>,
    timer: Timer<pac::Timer0>,
}

impl Channel for Console {
    fn read_byte(&mut self, timeout_ms: u32) -> Option<u8> {
        let t0 = self.timer.counter();
        loop {
            match self.uart.read() {
                Ok(b) => return Some(b),
                Err(nb::Error::WouldBlock) => {}
                Err(nb::Error::Other(_)) => {} // XMODEM's CRC catches a lost byte
            }
            if t0.wrapping_sub(self.timer.counter()) / TICKS_PER_MS >= timeout_ms {
                return None;
            }
        }
    }
    fn write_byte(&mut self, b: u8) {
        let _ = nb::block!(self.uart.write(b));
    }
}

/// Feeds accepted XMODEM blocks to the installer, keeping its first error.
struct InstallSink<'a> {
    installer: &'a mut Installer,
    flash: &'a mut Flash,
    error: Option<UpdateError>,
}

impl Sink for InstallSink<'_> {
    fn write(&mut self, _offset: usize, data: &[u8]) -> Result<(), ()> {
        self.installer.write(self.flash, data).map_err(|e| {
            self.error = Some(e);
        })
    }
}

#[riscv_rt::entry]
fn main() -> ! {
    let p = unsafe { pac::Peripherals::steal() };
    let mut timer = Timer::new(p.timer0);
    timer.start_free_running();
    let mut con = Console { uart: Serial::new_preconfigured(p.uart0), timer };
    let mut flash = Flash::new(p.spi3);
    let _ = writeln!(con.uart, "demo-app v{} running (confirm: {})\r", VERSION, if CONFIRM { "yes" } else { "NO" });

    if CONFIRM {
        match rb_core::confirm(&mut flash) {
            Ok(true) => {
                let _ = writeln!(con.uart, "demo-app v{}: confirmed, this image is now active\r", VERSION);
            }
            Ok(false) => {
                let _ = writeln!(con.uart, "demo-app v{}: not on trial, nothing to confirm\r", VERSION);
            }
            Err(e) => {
                let _ = writeln!(con.uart, "demo-app v{}: confirm FAILED: {:?}\r", VERSION, e);
            }
        }
    }
    let _ = writeln!(con.uart, "commands: u = install update (XMODEM), R = reboot\r");

    let mut last = con.timer.counter();
    let mut n = 0u32;
    loop {
        if let Ok(c) = con.uart.read() {
            match c {
                b'u' => update(&mut con, &mut flash),
                b'R' => {
                    let _ = writeln!(con.uart, "rebooting\r");
                    con.uart.flush();
                    rb_core::reboot();
                }
                _ => {}
            }
            last = con.timer.counter();
        }
        if last.wrapping_sub(con.timer.counter()) >= 1000 * TICKS_PER_MS {
            last = con.timer.counter();
            n += 1;
            let _ = writeln!(con.uart, "demo-app v{} alive {}\r", VERSION, n);
        }
    }
}

fn update(con: &mut Console, flash: &mut Flash) {
    let _ = writeln!(con.uart, "update: send the signed image with XMODEM now\r");
    let mut installer = Installer::new();
    let mut sink = InstallSink { installer: &mut installer, flash, error: None };
    let result = xmodem::receive_to(con, &mut sink, usize::MAX);
    let error = sink.error;
    // Let the sender's tool finish and start listening before we print.
    while con.read_byte(300).is_some() {}
    match (result, error) {
        (Ok(n), None) => match installer.finish(flash) {
            Ok((slot, version)) => {
                let _ = writeln!(con.uart, "update: received {} bytes; v{} installed in slot {:?}; rebooting\r", n, version, slot);
                con.uart.flush();
                rb_core::reboot();
            }
            Err(e) => {
                let _ = writeln!(con.uart, "update FAILED: {:?}\r", e);
            }
        },
        (_, Some(e)) => {
            let _ = writeln!(con.uart, "update refused: {:?}\r", e);
        }
        (Err(e), None) => {
            let _ = writeln!(con.uart, "update FAILED: transfer {:?}\r", e);
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
