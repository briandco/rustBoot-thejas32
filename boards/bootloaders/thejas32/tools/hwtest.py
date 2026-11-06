# /// script
# requires-python = ">=3.10"
# dependencies = ["pyserial>=3.5,<4", "xmodem>=0.4.7,<0.5"]
# ///
"""Board tests for the boot state log and power loss during an install.

Loads a `--features hwtest` rustBoot into RAM through the boot ROM (J12 open),
so the rustBoot in flash is never replaced. That build:

1. saves the boot state 300 times (past both sectors) and checks each reload;
2. starts installing the given image, writes half of it, and stops, asking
   for the power to be cut.

After unplugging, fit J12 and plug back in; then `just recovery ib` shows the
state and boots, and it must still be the previous image.

Usage:
    uv run tools/hwtest.py target/rustboot-hwtest.bin path/to/signed.bin
"""
from __future__ import annotations

import sys
from pathlib import Path

import serial

sys.path.insert(0, str(Path(__file__).resolve().parent))
from update import Console, load_fx  # noqa: E402


def main() -> None:
    rb, image = Path(sys.argv[1]), Path(sys.argv[2])
    fx = load_fx()
    with serial.Serial("/dev/ttyUSB0", 115200, timeout=0.2, write_timeout=10) as conn:
        conn.reset_input_buffer()
        con = Console(conn)
        print("J12 must be OPEN. Press RESET.", flush=True)
        # Only the ROM's UART mode prints this; flash boot never does.
        if con.until(["Please send file"], 300) is None:
            raise SystemExit("\nno ROM prompt (is J12 open?)")
        print(f"\n[ROM in UART mode: sending {rb.name}]", flush=True)
        if not fx.upload(conn, rb):
            raise SystemExit("ROM upload failed")
        con.send("\r")

        # The state-log test runs before the recovery prompt.
        if con.until(["for recovery"], 120) is None:
            raise SystemExit("\nthe test rustBoot never reached its prompt")
        con.send("r")
        if con.until(["rb> "], 10) is None:
            raise SystemExit("\nno recovery prompt")
        con.send("u")
        if con.until(["send the signed image"], 10) is None:
            raise SystemExit("\nupload not acknowledged")
        print(f"\n[sending {image.name}]", flush=True)
        if not fx.upload(conn, image):
            raise SystemExit("image upload failed")
        hit = con.until(["CUT POWER NOW", "refused", "failed"], 120)
        con.pump(1)
        if hit != "CUT POWER NOW":
            raise SystemExit(f"\ndid not reach the cut point ({hit})")
    print("\n\n>>> UNPLUG the USB cable now. Then FIT J12, and plug it back in. <<<\n", flush=True)


if __name__ == "__main__":
    main()
