# /// script
# requires-python = ">=3.10"
# dependencies = ["pyserial>=3.5,<4", "xmodem>=0.4.7,<0.5"]
# ///
"""Write an image to the ARIES v3 SPI flash, then watch it boot.

Same procedure as thejas32-rs's `just flash-persist`, in one serial session:

  1. ROM (J12 open) receives CDAC's flasher.bin over XMODEM and runs it.
  2. The flasher erases the first 256 KB of flash, then receives the image.
  3. You fit J12 and press RESET; this captures what the board prints.

The XMODEM handshake and upload come from thejas32-rs/tools/flash.py, which is
hardware-tested; only the two-stage sequencing is new.

Usage:
    uv run tools/flash_persist.py target/riscv32im-unknown-none-elf/release/flash.img
"""
from __future__ import annotations

import argparse
import importlib.util
import time
from pathlib import Path

import serial

HERE = Path(__file__).resolve().parent
THEJAS32_RS = HERE.parents[4] / "thejas32-rs"
FLASHER = THEJAS32_RS / "tools" / "flasher.bin"
# The first 256 KB: what flasher.bin erases (4 x 64 KB, from its disassembly).
FLASHER_LIMIT = 0x40000


def load_flash_py():
    spec = importlib.util.spec_from_file_location("thejas32_flash", THEJAS32_RS / "tools" / "flash.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def wait_for_flasher_prompt(connection: serial.Serial, timeout: float) -> bool:
    """Waits until flasher.bin has erased and asked for the image.

    Measured on the first run: flasher.bin's handshake 'C's arrive mixed with
    line breaks, so the ROM-style "reads holding only 'C'" test never fired.
    The flasher then gave up and fell back to the ROM, whose clean 'C's were
    taken as the flasher's, and the image went to RAM instead of flash.

    So key on text instead: the last erase line, then the prompt. Seeing the
    ROM banner in between means the flasher has already gone, and we stop
    rather than hand the image to the ROM.
    """
    deadline = time.monotonic() + timeout
    seen = ""
    erased = False
    while time.monotonic() < deadline:
        data = connection.read(256)
        if not data:
            continue
        text = data.decode("utf-8", errors="replace")
        print(text, end="", flush=True)
        seen += text
        if "Bootloader, ver" in seen:
            raise SystemExit("\nthe ROM bootloader came back: flasher.bin exited before receiving the image")
        marker = "Erase cmd 0xd8 at 0x030000"
        if not erased and marker in seen:
            erased = True
            # Keep what follows: the prompt often arrives in the same read.
            seen = seen[seen.index(marker) + len(marker):]
        if erased and "Please send file" in seen:
            # XMODEM.send waits for the receiver's 'C' itself.
            return True
    return False


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("image", type=Path)
    ap.add_argument("--port", default="/dev/ttyUSB0")
    ap.add_argument("--capture", type=float, default=20.0, help="seconds to watch the boot (default: %(default)s)")
    ap.add_argument("--wait-reset", type=float, default=600.0, help="seconds to wait for the first RESET (default: %(default)s)")
    args = ap.parse_args()

    fx = load_flash_py()
    size = args.image.stat().st_size
    if size > FLASHER_LIMIT:
        raise SystemExit(f"{args.image} is {size} bytes; flasher.bin only erases the first {FLASHER_LIMIT}")

    with serial.Serial(args.port, 115200, timeout=1, write_timeout=10) as conn:
        conn.reset_input_buffer()
        print("[1/3] J12 (BOOT_SEL) must be OPEN.")
        if not fx.wait_for_bootloader(conn, args.wait_reset):
            raise SystemExit("no ROM handshake; press RESET after starting this, with J12 open")
        if not fx.upload(conn, FLASHER):
            raise SystemExit("flasher.bin upload failed")
        conn.write(fx.START_PROGRAM)
        conn.flush()
        print("  sent flasher.bin; waiting for it to erase and listen", flush=True)

        print("[2/3] flasher running")
        if not wait_for_flasher_prompt(conn, 60):
            raise SystemExit("flasher never asked for the image")
        if not fx.upload(conn, args.image):
            raise SystemExit("image upload failed")
        conn.write(fx.START_PROGRAM)
        conn.flush()
        print(f"  sent {args.image.name} ({size} bytes)", flush=True)
        fx.capture(conn, 5)

        print("\n[3/3] Now FIT J12 (BOOT_SEL) and press RESET.", flush=True)
        print(f"  --- capturing {args.capture:.0f}s " + "-" * 40, flush=True)
        fx.capture(conn, args.capture)
        print()


if __name__ == "__main__":
    main()
