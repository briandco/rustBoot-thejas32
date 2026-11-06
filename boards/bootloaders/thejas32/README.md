# rustBoot for THEJAS32 (ARIES v3)

On reset, rustBoot loads an application from SPI flash, checks its SHA-256
hash and ECDSA P-256 signature, and runs it only if both pass (phase 1). It
keeps two application slots and installs signed updates received over UART,
with trial boot, confirmation, rollback and a minimum version (phase 2). Both
are tested on hardware; see "Measured on hardware" below.

Applications need no changes to boot. They stay linked at `0x200000`, and the
same `.bin` still runs when loaded over UART without rustBoot. To keep an
update, an application calls `rb_core::confirm` once it is running correctly.

## Quick start

```bash
just keygen              # once: keys/rustboot-dev.der (secret) + keys/pubkey.bin
just provision           # build rustBoot alone
just flash               # write it with the CDAC flasher (J12 open, then fitted)
just demo 1              # build + sign the demo app as v1
just update ../../sign_images/signed_images/demo-v1_v1_signed.bin   # press RESET; installs + boots it
just recovery i          # press RESET; prints the boot state and both slots
just test                # host tests
```

After provisioning, J12 stays fitted: updates go through rustBoot.
Reprovisioning rustBoot later (`just provision`, `just flash`) leaves the
installed apps and the boot state in place.

## Layout

```
SPI flash (AT25SF161, 2 MB)               IRAM (0x200000 .. 0x240000)
0x000000  rustBoot (128 KB slot)           0x200000  application, 160 KB
0x020000  unused (see below)                         (also the update receive buffer)
0x040000  slot A (192 KB)                  0x228000  rustBoot, 96 KB incl. stack
0x070000  slot B (192 KB)
0x0A0000  boot state log (2 x 4 KB)
```

CDAC's `flasher.bin`, the only way to write rustBoot itself, always erases
`0x0..0x40000`. Apps and state start above that, so reprovisioning rustBoot
cannot erase them. (Earlier layouts put slot A at `0x10000` and then
`0x20000`, where reprovisioning wiped it.)

Images are header (256 B) + firmware, up to 160 KB in total: the RAM they run
from bounds them, not the flash slot.

## How it boots

1. With J12 fitted, the ROM loads the first 250 KB of flash to `0x200000`
   and jumps there.
2. `_reloc_start` (position-independent) copies rustBoot to `0x228000`.
3. rustBoot checks the flash JEDEC ID and loads the boot state, then listens
   500 ms for `r` (recovery). Recovery boots normally after 60 s with no
   input.
4. `rb_state::plan` picks a slot. Each candidate is copied to `0x200000` and
   **that RAM copy** is verified, so the bytes checked are the bytes that run.
5. It restores the ROM's `mtvec`, masks interrupts via `mie`, and jumps to
   `0x200000`. With nothing bootable, it stays in recovery.

## Updates

THEJAS32 runs every image from RAM, so an application can sit in either slot,
and rustBoot switches slots instead of swapping their contents (upstream
rustBoot swaps sectors because MCUs execute in place):

- **Install** (recovery `u`): receive over XMODEM into RAM, check size,
  signature and minimum version, and only then erase and write the
  *inactive* slot, read it back, and record it as pending. The active slot
  is never written during an update.
- **Trial:** the next boot records the pending slot as on trial and boots it.
- **Confirm:** the application calls `rb_core::confirm`, or recovery `c`. The
  slot becomes active, and the minimum version rises to its version.
- **Rollback:** a reset while still on trial means it never confirmed, so the
  boot after that returns to the previous active slot.
- **Anti-rollback:** images below the minimum version are neither installed
  nor booted.

The boot state is a log of 32-byte CRC-32 records in two flash sectors
(`rb-state/src/log.rs`). A save interrupted by power loss leaves either the old
state or the new one. Host tests cut power at every byte of every write and
erase across 270 saves, and planted bugs make them fail.

Crates: `rb-state` (logic, host-tested), `rb-core` (flash driver, layout,
`confirm`; shared with apps), `demo-app` (update test app), and the
bootloader itself (`src/`).

**Apps compile the flash layout in** (through `rb-core`), so after any
change to `rb-core/src/layout.rs`, rebuild and re-sign the apps as well as
rustBoot. An app built for another layout cannot find the boot state:
`confirm` then returns `NoBootState` and writes nothing, and the update rolls
back at the next reset. (Found on hardware: a demo built before the slots
moved reported "nothing to confirm".)

## What changed from upstream rustBoot

| Change | Why |
|---|---|
| New `rustBoot::verify` (slice-based header parse + verify) | Upstream reads images through raw pointers to memory-mapped flash. THEJAS32 flash is only reachable over SPI. |
| `rbsigner`: removed `assert!(mtime < atime)` | A new file on `relatime` has equal mtime and atime (measured here), so signing any freshly built image panicked. |
| `rbsigner`: host tests `thejas32_verify_tests` | Show that the signer and verifier agree, and that tampering is caught (7 cases). |
| This crate | THEJAS32 startup, SPI flash reader, boot logic. |
| `boards/Cargo.toml` excludes this crate | It is RISC-V and has its own workspace. |

`verify` reads the version as little-endian, which is what `rbsigner` writes.
Upstream's `get_firmware_version` reads it big-endian (version 1 becomes
`0x01000000`). Upstream's `verify_integrity` panics on a hash mismatch, so its
fallback path can never run. `verify` returns an error instead.

## Where each hardware fact comes from

| Fact | Source |
|---|---|
| Flash is on **SPI3** (`0x1020_0200`), CS0, mode 3, ÷16 | Disassembly of CDAC `flasher.bin`, then confirmed on hardware (above). The thejas32-rs README memory map says `0x1020_0100`, which is SPI2 and reads nothing. |
| `flasher.bin` erases only `0x0..0x40000` | Same disassembly: four 64 KB erases. Hence the 256 KB layout. |
| Flash is an AT25SF161, JEDEC `1f:86:01` | thejas32-rs README (measured), checked again at every boot |
| IRAM `0x200000..0x240000` | thejas32-rs README (measured) |
| Read command `0x03` + 24-bit address | vega-sdk AT25SF161 example |

## Measured on hardware (2026-11-06, ARIES v3)

First boot from flash: rustBoot verified `timer-selftest` v1 and handed over,
and the self-test passed 8/8. With one bit flipped in the firmware's last byte
(`just tamper`), it printed `HALT: verification failed: HashMismatch` and did
not run the image.

| Question | Answer |
|---|---|
| Does `fence.i` work? | Yes: it ran without trapping. |
| How much flash does the ROM copy? | 250 KB from `0x0` (ROM prints `Copying 250KB from address: 0x000000`), so rustBoot's 53 KB loads in full. |
| Stack used by P-256 verification | 3,372 of 12,048 bytes (phase 1). |
| Which controller is the flash on? | SPI3. SPI2 reads `00:00:00`. |
| SPI settings | `DBITS = 0` (8 reads `ff:ff:ff`), with chip select held through the last byte. Releasing it before the last byte, as the HAL's `SpiBus` does, loses that byte. See `src/flash.rs` and `--features spi-diag`. |

### Flash write driver (phase 2, `--features flash-selftest`)

13/13 checks on hardware, on the last 4 KB sector (`0x1FF000`): erase, aligned
page, a 300-byte write crossing a page boundary, neighbours untouched, no
aliasing at -1 MB (so the chip is 2 MB), writes into rustBoot's slot refused,
and rustBoot's slot and the BOOT header unchanged afterwards. SR1 and SR2 read
`0x00`, so no block protection is set. Status bit 1 is the write-enable latch:
every write checks it.

Timing, by TIMER0 at 100 MHz: 4 KB erase **495 us**, 256-byte page program
**915 us** (including the SPI transfer). ⚠ The erase was of a sector that was
probably already blank, and 0.5 ms is shorter than NOR erases usually take.
Measure the erase of a written sector before relying on this figure for the
swap design.

### Updates (phase 2)

One reset per step, J12 fitted throughout:

| Step | Board output |
|---|---|
| Fresh board, no app | `slot A: no image`, `slot B: no image`, recovery prompt |
| Install v1 | `installed v1 in slot B` → `ON TRIAL` → app: `confirmed` |
| Reset, install v2 | state `active B, min version 1` → slot A, trial, confirmed |
| Reset, install v3 (never confirms) | state `active A, min version 2` → slot B, trial, no confirm |
| Reset, install v1 | `refused: VersionTooOld { version: 1, min: 2 }` |
| Boot | `ROLLBACK: slot B was on trial and never confirmed` → slot A v2 |

Upload ran at about 1.3–4 KB/s (2.4–7.6 s for 10 KB). Erasing a slot (3 x 64 KB)
took 22–172 ms; programming 10 KB took 37 ms. Stack use is 5.5 of 33 KB.

### Power loss and state-log wrap (`--features hwtest`, `tools/hwtest.py`)

The test build is loaded into RAM through the ROM, so the rustBoot in flash is
never replaced by it.

- **State log wrap:** 300 saves, three runs, from different starting points.
  Each switched sector exactly at the 128th record (e.g. save 120 after 8
  existing records) and every reload matched. About 7.2 s per 300 saves.
- **Power cut mid-install:** installing v4 stopped with slot B half written
  (5,186 of 10,372 bytes) and not marked pending; the USB was unplugged. On
  power-up the board booted v2 by itself. rustBoot then reported
  `state: active A, min version 2` with nothing pending, and booted slot A v2,
  ignoring slot B's partial v4.

This cut lands between flash operations, as a cut during programming would
leave things. A cut in the middle of a state save is covered by the host test
(every byte of every write) but was not reproduced by hand: the window is
about 1 ms.

### Layout move and recovery timeout

After moving apps and state above `0x40000`:

- Reprovisioned rustBoot onto empty slots: recovery, then after 60 s idle
  `no input for 60 s; leaving recovery`, then back to recovery (nothing to boot).
- Installed v5 (rebuilt for the new layout): trial, confirmed, `active B, min version 5`.
- Reprovisioned rustBoot again: it booted v5 straight away with
  `state: active B, min version 5`. Reflashing rustBoot no longer touches apps.

## Security notes

- **Keys.** Upstream's `boards/sign_images/keygen/ecc256.der` contains its
  private key and is public. Don't use it. `keys/rustboot-dev.der` is a
  development key generated locally; it is git-ignored, and the committed
  `keys/pubkey.bin` is its public half. See "Production keys" below.
- **Not protected against physical access.** Anyone who can remove the J12
  jumper can load unsigned code over UART, because the ROM boots from UART
  when J12 is open. rustBoot only protects the flash boot path.
- **Anti-rollback state is in ordinary flash.** Anyone who can run their
  own code (J12 open) can rewrite it and reset the minimum version. THEJAS32
  has no fuses or secure storage known to us.
- **Recovery has no authentication.** Anyone on the UART can upload, but only
  images signed with your key are accepted. It times out after 60 s idle.

## Production keys

The signing key is the whole security of this bootloader: whoever holds the
private half can make firmware every board accepts. For real devices:

1. **Generate it on a machine that is not the build machine**, ideally offline:
   `python3 tools/keygen.py --out prod.der --pubkey pubkey.bin`.
   `prod.der` is created readable only by its owner.
2. **Copy only `pubkey.bin` to the build machine**, replacing
   `keys/pubkey.bin`, and commit it. The public half is not secret.
3. **Build and provision rustBoot with it** (`just provision`, `just flash`).
   The public key is compiled in, so a board only trusts the key it was
   provisioned with.
4. **Sign releases on the key machine**: copy the unsigned `.bin` to a
   checkout there and, from its `rbsigner/` directory, run
   `cargo run --release -- mcu-image <app.bin> nistp256 <prod.der> <version>`
   (rbsigner always writes to `../boards/sign_images/signed_images/`). Copy
   the signed image back. The private key never leaves that machine.
5. **Keep an offline backup of `prod.der`.** If it is lost, deployed boards
   cannot be updated again without reprovisioning rustBoot (J12 and the
   flasher, at the board).
6. **Never ship the dev key.** A board built with `rustboot-dev.der`'s
   `pubkey.bin` accepts anything signed on any machine that has the dev key.

Limits: rustBoot trusts exactly one key, and there is no revocation or
rotation. Replacing the key means reprovisioning rustBoot on every board.
Versions only go up: confirm a release before shipping it widely, because a
confirmed version raises the minimum for good.
