# /// script
# requires-python = ">=3.10"
# dependencies = ["pyserial>=3.5,<4"]
# ///
"""Send keystrokes to whatever is running on the board and show its output.

  uv run tools/console.py d            # send 'd', show output for 5 s
  uv run tools/console.py d R --capture 20

Each argument is sent as-is, one second apart. No reset is needed.
"""
from __future__ import annotations

import argparse
import sys
import time

import serial


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("keys", nargs="*", help="strings to send, one second apart")
    ap.add_argument("--port", default="/dev/ttyUSB0")
    ap.add_argument("--capture", type=float, default=5, help="seconds to watch after the last key")
    args = ap.parse_args()

    with serial.Serial(args.port, 115200, timeout=0.05) as conn:
        def pump(seconds: float) -> None:
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                data = conn.read(256)
                if data:
                    sys.stdout.write(data.decode("utf-8", errors="replace"))
                    sys.stdout.flush()

        conn.reset_input_buffer()
        for k in args.keys:
            conn.write(k.encode())
            conn.flush()
            pump(1)
        pump(args.capture)
    print()


if __name__ == "__main__":
    main()
