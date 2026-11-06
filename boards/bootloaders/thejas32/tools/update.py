# /// script
# requires-python = ">=3.10"
# dependencies = ["pyserial>=3.5,<4", "xmodem>=0.4.7,<0.5"]
# ///
"""Talk to rustBoot's recovery console: install an update, or run commands.

Start this, then press RESET (J12 fitted). It catches rustBoot's recovery
prompt, sends 'r', and then:

  uv run tools/update.py signed.bin            # install signed.bin, then boot ('b')
  uv run tools/update.py signed.bin --no-boot  # install, stay in recovery
  uv run tools/update.py --cmds i              # just show slot info
  uv run tools/update.py --cmds cb             # confirm the trial, then boot

Everything the board prints is shown. The XMODEM upload comes from
thejas32-rs/tools/flash.py (hardware-tested).
"""
from __future__ import annotations

import argparse
import importlib.util
import sys
import time
from pathlib import Path

import serial

HERE = Path(__file__).resolve().parent
FLASH_PY = HERE.parents[4] / "thejas32-rs" / "tools" / "flash.py"


def load_fx():
    spec = importlib.util.spec_from_file_location("thejas32_flash", FLASH_PY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class Console:
    def __init__(self, conn: serial.Serial):
        self.conn = conn
        self.seen = ""

    def pump(self, seconds: float) -> None:
        """Echo output for `seconds`."""
        self.until([], seconds)

    def until(self, needles: list[str], timeout: float) -> str | None:
        """Echo output until one of `needles` appears (returns it) or timeout."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            data = self.conn.read(256)
            if data:
                text = data.decode("utf-8", errors="replace")
                sys.stdout.write(text)
                sys.stdout.flush()
                self.seen += text
            for n in needles:
                i = self.seen.find(n)
                if i >= 0:
                    self.seen = self.seen[i + len(n):]
                    return n
        return None

    def send(self, s: str) -> None:
        self.conn.write(s.encode())
        self.conn.flush()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("image", nargs="?", type=Path, help="signed image to install")
    ap.add_argument("--cmds", default=None, help="recovery commands to send instead of / after an install")
    ap.add_argument("--no-boot", action="store_true", help="after installing, stay in recovery")
    ap.add_argument("--port", default="/dev/ttyUSB0")
    ap.add_argument("--wait-reset", type=float, default=300, help="seconds to wait for RESET")
    ap.add_argument("--capture", type=float, default=15, help="seconds to watch after booting")
    args = ap.parse_args()

    fx = load_fx()
    with serial.Serial(args.port, 115200, timeout=0.2, write_timeout=10) as conn:
        conn.reset_input_buffer()
        con = Console(conn)
        print(f"waiting up to {args.wait_reset:.0f}s: press RESET (J12 fitted)", flush=True)
        if con.until(["for recovery"], args.wait_reset) is None:
            raise SystemExit("\nno rustBoot prompt; is J12 fitted, and was RESET pressed after starting this?")
        con.send("r")
        if con.until(["rb> "], 10) is None:
            raise SystemExit("\nrustBoot did not enter recovery")

        if args.image:
            con.send("u")
            if con.until(["send the signed image"], 10) is None:
                raise SystemExit("\nupload command not acknowledged")
            print(f"\n[sending {args.image.name}, {args.image.stat().st_size} bytes]", flush=True)
            t0 = time.monotonic()
            ok = fx.upload(conn, args.image)
            print(f"[XMODEM {'done' if ok else 'FAILED'} in {time.monotonic() - t0:.1f}s]", flush=True)
            result = con.until(["boots on trial next", "refused", "failed", "mismatch", "FAILED"], 120)
            con.until(["rb> "], 10)
            if result != "boots on trial next":
                print(f"\n[install did not complete: {result}]", flush=True)
                if args.cmds is None:
                    raise SystemExit(1)
                # Explicit --cmds still run, e.g. to boot after a refusal.

        cmds = args.cmds if args.cmds is not None else ("" if args.no_boot or not args.image else "b")
        for c in cmds:
            con.send(c)
            if c == "b":
                con.pump(args.capture)
                break
            con.until(["rb> "], 30)
        print()


if __name__ == "__main__":
    main()
