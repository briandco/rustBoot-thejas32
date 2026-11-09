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
just app-update ../../sign_images/signed_images/demo-v7_v7_signed.bin  # update through the running app: no reset
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
`confirm`, `Installer`, `reboot`; shared with apps), `demo-app` (update test app),
`crypto-bench` (crypto timings on the chip), and the bootloader itself (`src/`).

**Apps compile the flash layout in** (through `rb-core`), so after any
change to `rb-core/src/layout.rs`, rebuild and re-sign the apps as well as
rustBoot. An app built for another layout cannot find the boot state:
`confirm` then returns `NoBootState` and writes nothing, and the update rolls
back at the next reset. (Found on hardware: a demo built before the slots
moved reported "nothing to confirm".)

## Updates from a running application

A real device updates itself: the running application receives the image
and rustBoot does the rest. `rb-core` provides the pieces, and `demo-app`
uses them (console command `u`):

```rust
let mut installer = rb_core::Installer::new();
// for each chunk of the signed image, in order (e.g. per XMODEM block):
installer.write(&mut flash, chunk)?;      // header checked, slot erased, data streamed
let (slot, version) = installer.finish(&mut flash)?;  // now pending
rb_core::reboot();                         // rustBoot verifies, boots on trial
```

- **Streaming.** The image goes straight to flash a block at a time; the
  application needs no RAM buffer for it.
- **Checked before erasing.** The header's size and version are read first
  (`rb_state::image::image_info`, no crypto); an oversized or too-old image
  is refused before anything is erased.
- **Never trusted by the application.** The application does not verify the
  signature. rustBoot does, before running anything; a bad image is
  rejected at boot and the current version keeps running.
- **Power-safe,** like the recovery install: only the inactive slot is
  written, and it is marked pending only after the last byte.

### `reboot()`: a software restart

THEJAS32 has no reset register or watchdog that we found: none in vega-sdk
or the SVD. The watchdog address in TrusteD-V's BSP (`0x1000_3000`) is
marked there as an unverified placeholder, and reads as all zeros. So
`rb_core::reboot` jumps to the boot ROM's entry at `0x10000`. Measured on
hardware: the first ROM words are `li x1,0`, `li x2,0`, ... (register
clearing, i.e. reset code), and the jump runs the normal flash boot into
rustBoot. It is a software restart: peripherals keep their settings instead
of returning to reset defaults.

Consequence: a new version that **hangs** before confirming is not rolled
back until something resets the board (RESET, or a power cycle). Without a
hardware watchdog, a product needs one external to the SoC for unattended
recovery.

## Hybrid signatures: ECDSA P-256 + LMS

Every image needs two valid signatures over the same SHA-256 digest:
ECDSA P-256 in the 256-byte header (unchanged), and LMS (RFC 8554, NIST
SP 800-208), a post-quantum hash-based scheme, in a fixed 2,520-byte trailer
after the firmware. An attacker must break both. Images without the trailer
are rejected (`FAILED verification: LmsSignature`), so the LMS check cannot
be stripped off.

```
[256-byte header: ECDSA] [firmware] ["LMS1" | len | HSS signature (2,512 B)]
```

- **Parameters:** one level, LMS_SHA256_M32_H10 + LMOTS_SHA256_N32_W4:
  1,024 signatures per key, 2,512-byte signatures, 60-byte public key
  (`keys/lms_pubkey.bin`, compiled in).
- **Signing** (`rbsigner lms-keygen`, `lms-sign`; `just sign` does both
  signatures) uses `hbs-lms` 0.1.1. LMS keys are **stateful**: each signature
  uses a one-time key. hbs-lms returns a signature only after our callback
  has saved the advanced key, and that callback writes a temp file, fsyncs
  it and renames it over the key. A crash can waste a one-time key but never
  reuse one.
- **Verification** on the board is rustBoot's own `rustBoot/src/lms.rs`
  (RFC 8554 algorithms 4b, 6a, 6), not hbs-lms. Measured: hbs-lms's verifier
  needs 48-64 KB of stack (one function, `generate_public_key_candiate`, has a
  43 KB frame because it reserves buffers for every Winternitz width at once)
  against rustBoot's ~27 KB, and it overflowed on the board. The own
  verifier hashes chain by chain: its largest frame is 656 bytes, and it
  reuses rustBoot's SHA-256.
- **Tests for the own verifier:** RFC 8554 Appendix F test cases 1 and 2
  (two-level HSS); sampled bit flips in every part, truncation and trailing
  bytes; a planted bug (sibling order) makes both RFC cases fail; and
  agreement with hbs-lms on 24 valid and 884 corrupted signatures across
  W1/W2/W4/W8, H5/H10 and two-level HSS. On 24 of the corrupted inputs
  hbs-lms **panicked** (out-of-bounds read); the own verifier rejected them.

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

### Updates from a running application

No reset or jumper at any step: each update ended in `rb_core::reboot`.

| Step | Board output |
|---|---|
| v6 told to reboot, recovery installs v7 | v7 `confirmed` |
| v7 installs v8 (never confirms) | `pending A v8` → `ON TRIAL` |
| v8 told to reboot | `ROLLBACK: slot A was on trial and never confirmed` → v7 |
| v7 installs a tampered v9 | `slot A: v9 FAILED verification: HashMismatch`, `the newly installed image was rejected` → v7 |
| v7 offered v5 | `VersionTooOld { version: 5, min: 7 }`, nothing erased, v7 keeps running |
| v7 installs v10 | `ON TRIAL` → `confirmed` |

About 4 s per 18 KB image over UART at 115200.

### Crypto cost (`crypto-bench`, installed as v11 via `just app-update`)

TIMER0 at 100 MHz; three runs each, identical.

| Operation | Time |
|---|---|
| SHA-256, 64 KiB | 67.3 ms: 950 KB/s, about 66 us per 64-byte block |
| SHA-256, 59-byte message (2 blocks) | 161 us per call |
| LMS H10/W4 verify (own verifier, crypto-bench v17) | **54.4 ms** |
| Flash read, 45,388-byte image (SPI3 at 6.25 MHz) | 94 ms |
| `verify` of that image | 414 ms: SHA-256 47 ms + **ECDSA P-256 ~367 ms** |

Code size (symbol table of `rustboot-thejas32`, approximate because LTO
inlines across crates): SHA-256 ~6.8 KB, P-256/ECDSA ~13.5 KB, header parse
and glue ~4.2 KB, of a 66 KB bootloader.

Before LMS was added, its cost was estimated from the hash cost at ~34 ms
for H10/W4 (and ~13 ms for w=2, ~290 ms for w=8). Measured with the own
verifier: 49-54 ms; the difference is per-call setup across ~500 short
hashes.

### Power cut during an update from the running app

v18 (119 KB, transfer slowed with `app_update.py --slow-ms 100`) was
installing v19 into slot A when the USB cable, the board's only supply, was
pulled about 20 s in. On power-up the board booted v18 by itself. rustBoot
then reported `state: active B, min version 18` with nothing pending; slot A
held v19's header (written first) but was never tried, because the slot is
marked pending only after its last byte. The recovery-mode install was
tested the same way earlier (`tools/hwtest.py`).

### Hybrid ECDSA + LMS (after reprovisioning rustBoot)

| Step | Board output |
|---|---|
| First boot, old ECDSA-only apps in both slots | `v10 FAILED verification: LmsSignature`, `v11 ... LmsSignature`, recovery |
| Recovery installs hybrid v12 | `verified (sha256+ecdsa 386 ms, lms 49 ms)`, trial, confirmed |
| v12 installs v13 | `lms 53 ms`, trial, confirmed |
| v13 offered v14 without the LMS trailer | `Incomplete { written: 17920, total: 20440 }`, not installed |
| v13 installs v15 with one LMS signature byte flipped | `v15 FAILED verification: LmsSignature`, stays on v13 |
| crypto-bench v17 | LMS verify **54.4 ms**, ECDSA P-256 367 ms, SHA-256 950 KB/s |

Stack: 6,156 of 27,096 bytes. rustBoot is 71 KB (66 KB before LMS).
LMS time varies a little per image (49-54 ms): the hash-chain lengths depend
on the digest.

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
5. **LMS keys are stateful.** `lms-dev.key` changes on every signature and
   allows 1,024 in total. `just lms-info` shows how many are left; `lms-sign`
   prints the count after each signature and warns below 10%. An exhausted
   key is refused before it is touched (hbs-lms wipes a key after its last
   signature, so the file then reports "exhausted", not a count). Never restore an older copy of it, and never use
   the same key file from two machines: either reuses one-time keys, which
   breaks LMS. Back it up only by copying the current file after signing,
   and track how many signatures remain (q, the leaf index, is the first
   4 bytes of the LMS signature after `Nspk`).
6. **Keep an offline backup of `prod.der`.** If it is lost, deployed boards
   cannot be updated again without reprovisioning rustBoot (J12 and the
   flasher, at the board).
7. **Never ship the dev keys.** A board built with `rustboot-dev.der`'s
   `pubkey.bin` accepts anything signed on any machine that has the dev key.

Limits: rustBoot trusts exactly one key, and there is no revocation or
rotation. Replacing the key means reprovisioning rustBoot on every board.
Versions only go up: confirm a release before shipping it widely, because a
confirmed version raises the minimum for good.
