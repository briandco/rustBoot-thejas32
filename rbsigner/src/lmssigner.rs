//! LMS (RFC 8554 / NIST SP 800-208) signing for hybrid rustBoot images.
//!
//! An image is first signed as usual (`mcu-image`, ECDSA P-256), then
//! `lms-sign` appends a trailer: `"LMS1"`, the signature length (u32 LE), and
//! an HSS signature over the image's SHA-256 digest, the same digest ECDSA
//! signs. rustBoot built with `lms` requires both.
//!
//! LMS keys are **stateful**: every signature uses a one-time key, and using
//! one twice breaks the scheme. hbs-lms signs in memory, advances the key, and
//! returns the signature only if our update callback succeeds. The callback
//! here reports success only after the advanced key is on disk (temp file,
//! fsync, rename, fsync of the directory), so a crash can lose a one-time key
//! but never reuse one.
//!
//! Parameters: one level, LMS_SHA256_M32_H10 + LMOTS_SHA256_N32_W4: 1024
//! signatures per key, 2512-byte signatures. The bootloader is built for
//! exactly these (see the thejas32 `.cargo/config.toml`).

use hbs_lms::{HssParameter, LmotsAlgorithm, LmsAlgorithm, Seed, Sha256_256};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub const LMS_TRAILER_MAGIC: [u8; 4] = *b"LMS1";
/// HSS (L=1) signature length for H10/W4: 4 + 4 + (4 + 32 + 67*32) + 4 + 10*32.
pub const LMS_SIGNATURE_LEN: usize = 2512;
pub const LMS_TRAILER_LEN: usize = 8 + LMS_SIGNATURE_LEN;

const HEADER_SIZE: usize = 256;
/// rbsigner's header layout: the digest covers header[..44] + firmware and is
/// stored at 48..80 (see mcusigner.rs).
const DIGEST_COVER_END: usize = 44;
const DIGEST_RANGE: core::ops::Range<usize> = 48..80;

fn parameters() -> [HssParameter<Sha256_256>; 1] {
    [HssParameter::new(LmotsAlgorithm::LmotsW4, LmsAlgorithm::LmsH10)]
}

/// Creates a new LMS key pair. Refuses to overwrite either file.
pub fn keygen(private_path: &Path, public_path: &Path) -> io::Result<()> {
    for p in [private_path, public_path] {
        if p.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists; refusing to overwrite an LMS key", p.display()),
            ));
        }
    }
    let mut seed_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut seed_bytes);
    let seed = Seed::<Sha256_256>::from(seed_bytes);
    seed_bytes.fill(0);
    let (signing_key, verifying_key) = hbs_lms::keygen::<Sha256_256>(&parameters(), &seed, None)
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "LMS key generation failed"))?;

    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(private_path)?;
    f.write_all(signing_key.as_slice())?;
    f.sync_all()?;
    let mut f = OpenOptions::new().write(true).create_new(true).open(public_path)?;
    f.write_all(verifying_key.as_slice())?;
    f.sync_all()?;
    Ok(())
}

/// Writes `data` to `path` so that a crash leaves either the old or the new
/// contents, never a mix: temp file, fsync, rename, fsync the directory.
pub fn write_durably(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("rbsigner")
    ));
    {
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}

/// Signs `message` with the key in `key_path`, advancing the stored key
/// before the signature is returned.
pub fn sign_with_key_file(message: &[u8], key_path: &Path) -> io::Result<Vec<u8>> {
    let key = fs::read(key_path)?;
    let mut persist_error = None;
    let signature = hbs_lms::sign::<Sha256_256>(
        message,
        &key,
        &mut |advanced: &[u8]| {
            write_durably(key_path, advanced, 0o600).map_err(|e| {
                persist_error = Some(e);
            })
        },
        None,
    );
    if let Some(e) = persist_error {
        return Err(io::Error::new(e.kind(), format!("could not save the advanced LMS key, so no signature was produced: {e}")));
    }
    let signature = signature.map_err(|_| {
        io::Error::new(io::ErrorKind::Other, "LMS signing failed (key exhausted or malformed)")
    })?;
    Ok(signature.as_ref().to_vec())
}

/// The digest an image's signatures cover, after checking it matches the one
/// in its header.
pub fn image_digest(image: &[u8]) -> io::Result<[u8; 32]> {
    if image.len() < HEADER_SIZE || image[..4] != *b"RUST" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a signed rustBoot image"));
    }
    let fw_size = u32::from_le_bytes(image[4..8].try_into().unwrap()) as usize;
    if image.len() != HEADER_SIZE + fw_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "image is {} bytes but its header says {}; it may already have an LMS trailer",
                image.len(),
                HEADER_SIZE + fw_size
            ),
        ));
    }
    let mut h = Sha256::new();
    h.update(&image[..DIGEST_COVER_END]);
    h.update(&image[HEADER_SIZE..]);
    let digest: [u8; 32] = h.finalize().into();
    if digest[..] != image[DIGEST_RANGE] {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "image digest does not match its header"));
    }
    Ok(digest)
}

/// Appends an LMS trailer to an ECDSA-signed image file, in place.
pub fn append_trailer(image_path: &Path, key_path: &Path) -> io::Result<usize> {
    let mut image = fs::read(image_path)?;
    let digest = image_digest(&image)?;
    let sig = sign_with_key_file(&digest, key_path)?;
    if sig.len() != LMS_SIGNATURE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("LMS signature is {} bytes, expected {LMS_SIGNATURE_LEN}: wrong key parameters?", sig.len()),
        ));
    }
    image.extend_from_slice(&LMS_TRAILER_MAGIC);
    image.extend_from_slice(&(sig.len() as u32).to_le_bytes());
    image.extend_from_slice(&sig);
    write_durably(image_path, &image, 0o644)?;
    Ok(image.len())
}
