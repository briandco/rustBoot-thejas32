#!/usr/bin/env python3
"""Check the rustBoot binary and copy it to the image written by the flasher.

Flash layout (must match rb-core/src/layout.rs):
  0x000000 .. 0x020000   rustBoot, written with CDAC's flasher.bin (`just flash`)
  0x020000 .. 0x040000   unused: flasher.bin erases 0..0x40000 every time
  0x040000 ..            app slots and boot state, written only by rustBoot

Applications are never part of this image: they go in through rustBoot's
recovery mode (`just update`). That way reprovisioning rustBoot leaves the
installed apps and the boot state untouched.
"""
import argparse
import sys

RUSTBOOT_SLOT = 0x20000


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("rustboot", help="rustBoot raw binary (objcopy -O binary)")
    ap.add_argument("-o", "--output", required=True)
    args = ap.parse_args()

    rb = open(args.rustboot, "rb").read()
    if len(rb) > RUSTBOOT_SLOT:
        sys.exit(f"rustBoot is {len(rb)} bytes; its slot is {RUSTBOOT_SLOT}")
    if rb[:4] != b"\x97\x02\x00\x00":  # auipc t0, 0: the relocation stub
        sys.exit(f"{args.rustboot} does not start with rustBoot's relocation stub")
    with open(args.output, "wb") as f:
        f.write(rb)
    print(f"{args.output}: {len(rb)} bytes. Write it with `just flash`; install apps with `just update`.")


if __name__ == "__main__":
    main()
