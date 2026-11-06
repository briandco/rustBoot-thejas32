#!/usr/bin/env python3
"""Generate a P-256 signing key for rustBoot on THEJAS32.

Writes two files:
  <out>.der    96 bytes: public X||Y (64) then private scalar (32). This is the
               layout rbsigner reads (it takes the private key from byte 0x40).
               Keep it secret; anyone holding it can sign firmware the board
               will run.
  pubkey.bin   64 bytes: public X||Y, compiled into the bootloader.

Upstream rustBoot ships a key whose private half is public, so a new key is
required before the bootloader protects anything.
"""
import argparse
import os
import sys

from cryptography.hazmat.backends import default_backend
from cryptography.hazmat.primitives.asymmetric import ec


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default="keys/rustboot-dev.der", help="signing key file to create")
    ap.add_argument("--pubkey", default="keys/pubkey.bin", help="public key file for the bootloader")
    ap.add_argument("--force", action="store_true", help="overwrite existing files")
    args = ap.parse_args()

    for path in (args.out, args.pubkey):
        if os.path.exists(path) and not args.force:
            sys.exit(f"{path} exists; refusing to overwrite it (pass --force). "
                     "Replacing the key makes every image signed with the old one unbootable.")

    key = ec.generate_private_key(ec.SECP256R1(), default_backend())
    priv = key.private_numbers().private_value.to_bytes(32, "big")
    pub = key.public_key().public_numbers()
    pub_xy = pub.x.to_bytes(32, "big") + pub.y.to_bytes(32, "big")

    fd = os.open(args.out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "wb") as f:
        f.write(pub_xy + priv)
    with open(args.pubkey, "wb") as f:
        f.write(pub_xy)
    print(f"wrote {args.out} (secret, 96 bytes) and {args.pubkey} (64 bytes)")


if __name__ == "__main__":
    main()
