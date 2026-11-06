# /// script
# requires-python = ">=3.10"
# dependencies = ["pyserial>=3.5,<4", "xmodem>=0.4.7,<0.5"]
# ///
"""Update a board through its running application, with no reset or jumper.

The application (see demo-app: `u` command, `rb_core::Installer`) receives
the signed image over XMODEM, writes it to the inactive slot, and reboots.
rustBoot then verifies it and boots it on trial. This prints the whole
sequence, through the new version's first lines.

  uv run tools/app_update.py path/to/signed.bin
"""
from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

import serial

sys.path.insert(0, str(Path(__file__).resolve().parent))
from update import Console, load_fx  # noqa: E402


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("image", type=Path)
    ap.add_argument("--port", default="/dev/ttyUSB0")
    ap.add_argument("--capture", type=float, default=8, help="seconds to watch after the reboot")
    args = ap.parse_args()

    fx = load_fx()
    with serial.Serial(args.port, 115200, timeout=0.05, write_timeout=10) as conn:
        conn.reset_input_buffer()
        con = Console(conn)
        con.send("u")
        if con.until(["send the signed image"], 10) is None:
            raise SystemExit("\nthe running app did not offer an update (does it have the 'u' command?)")
        print(f"\n[sending {args.image.name}, {args.image.stat().st_size} bytes]", flush=True)
        t0 = time.monotonic()
        ok = fx.upload(conn, args.image)
        print(f"[XMODEM {'done' if ok else 'stopped'} after {time.monotonic() - t0:.1f}s]", flush=True)
        # A refusal can arrive while the XMODEM sender is still reading the
        # line (it cancels after the header), swallowing "update refused", so
        # the reasons themselves are matched too.
        hit = con.until(
            ["rebooting", "refused", "FAILED", "VersionTooOld", "BadHeader", "TooLarge", "NoBootState"], 30
        )
        if hit != "rebooting":
            con.pump(1)
            raise SystemExit(f"\nupdate not installed ({hit})")
        con.pump(args.capture)
    print()


if __name__ == "__main__":
    main()
