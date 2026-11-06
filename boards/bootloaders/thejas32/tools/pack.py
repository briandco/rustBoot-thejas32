#!/usr/bin/env python3
"""Combine rustBoot and a signed application into one SPI flash image.

Flash layout (must match rb-core/src/layout.rs):
  0x00000 .. 0x20000   rustBoot (the boot ROM loads this into IRAM)
  0x20000 .. 0x50000   slot A: 256-byte rustBoot header, then firmware

The image must stay inside the first 256 KB, the region CDAC's flasher.bin
erases (four 64 KB block erases), so a packed app is limited to 128 KB.
Larger apps, and every update, go through rustBoot's recovery mode instead
(`just update`). Packing an app is optional: leave it out to provision a
board with rustBoot alone.
"""
import argparse
import struct
import sys

RUSTBOOT_SLOT = 0x20000
BOOT_PART_OFFSET = 0x20000
FLASHER_LIMIT = 0x40000
BOOT_PART_SIZE = FLASHER_LIMIT - BOOT_PART_OFFSET
HEADER_SIZE = 0x100
RUSTBOOT_MAGIC = 0x54535552  # "RUST"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("rustboot", help="rustBoot raw binary (objcopy -O binary)")
    ap.add_argument("signed_app", nargs="?", help="application signed by rbsigner mcu-image")
    ap.add_argument("-o", "--output", required=True)
    args = ap.parse_args()

    rb = open(args.rustboot, "rb").read()
    if len(rb) > RUSTBOOT_SLOT:
        sys.exit(f"rustBoot is {len(rb)} bytes; its slot is {RUSTBOOT_SLOT}")
    if args.signed_app is None:
        with open(args.output, "wb") as f:
            f.write(rb)
        print(f"{args.output}: {len(rb)} bytes (rustBoot only; install apps with `just update`)")
        return
    app = open(args.signed_app, "rb").read()
    if len(app) > BOOT_PART_SIZE:
        sys.exit(f"signed app is {len(app)} bytes; at most {BOOT_PART_SIZE} fit below the flasher limit. Use `just update` instead.")
    magic, size = struct.unpack_from("<II", app, 0)
    if magic != RUSTBOOT_MAGIC:
        sys.exit(f"{args.signed_app} has no rustBoot header (magic {magic:#x}); sign it first")
    if size != len(app) - HEADER_SIZE:
        sys.exit(f"header says {size} firmware bytes but the file holds {len(app) - HEADER_SIZE}")

    # 0xFF matches erased flash.
    image = rb + b"\xff" * (BOOT_PART_OFFSET - len(rb)) + app
    with open(args.output, "wb") as f:
        f.write(image)
    print(f"{args.output}: {len(image)} bytes "
          f"(rustBoot {len(rb)} B at 0x0, app {size} B + header at {BOOT_PART_OFFSET:#x})")


if __name__ == "__main__":
    main()
