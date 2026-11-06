//! Verification of a rustBoot MCU image held in memory.
//!
//! Upstream rustBoot verifies images by turning partition addresses into
//! pointers, which needs flash the CPU can read directly. THEJAS32's flash sits
//! behind an SPI controller, so the bootloader first copies the header and
//! firmware into RAM and then checks them here, as plain byte slices.
//!
//! Checking the RAM copy, rather than flash, also means the bytes that were
//! checked are exactly the bytes that will run.
//!
//! The header format is the one `rbsigner mcu-image` writes: a 256-byte header
//! (`magic`, `size`, then type-length-value fields, with `0xFF` padding allowed
//! between them) followed by the firmware.

use crate::crypto::signatures::{NistP256Signature, HDR_IMG_TYPE_AUTH};
use crate::rbconstants::*;
use crate::{Result, RustbootError};

use p256::{
    ecdsa::VerifyingKey,
    elliptic_curve::{generic_array::GenericArray, FieldSize},
    EncodedPoint, NistP256,
};
use sha2::{Digest, Sha256};

const TAG_END: u16 = 0x0000;
const TAG_VERSION: u16 = HDR_VERSION;
const TAG_TIMESTAMP: u16 = 0x0002;
const TAG_IMG_TYPE: u16 = HDR_IMG_TYPE;
const TAG_SHA256: u16 = HDR_SHA256;
const TAG_PUBKEY_DIGEST: u16 = HDR_PUBKEY_DIGEST;
const TAG_SIGNATURE: u16 = HDR_SIGNATURE;

/// Fields parsed from an image header, before the firmware has been read.
///
/// Parse the header first so the firmware size can be bounds-checked before
/// any firmware is copied into RAM.
#[derive(Debug, Clone, Copy)]
pub struct Header<'a> {
    raw: &'a [u8; IMAGE_HEADER_SIZE],
    /// Firmware size in bytes, from the header's `size` field.
    pub fw_size: usize,
    /// Firmware version.
    pub version: u32,
    /// The signer's timestamp (input file modification time, Unix seconds).
    pub timestamp: Option<u64>,
    img_type: u16,
    digest: &'a [u8],
    /// Offset of the SHA-256 field's tag. The hash covers the header up to here.
    digest_offset: usize,
    pubkey_digest: &'a [u8],
    signature: &'a [u8],
}

/// Parses a 256-byte image header.
///
/// Every field `rbsigner` writes is required, each may appear only once, and
/// no field may run past the end of the header.
pub fn parse_header(raw: &[u8; IMAGE_HEADER_SIZE]) -> Result<Header<'_>> {
    let magic = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
    if magic as usize != RUSTBOOT_MAGIC {
        return Err(RustbootError::InvalidImage);
    }
    let fw_size = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
    if fw_size == 0 {
        return Err(RustbootError::InvalidFirmwareSize);
    }

    let mut version = None;
    let mut timestamp = None;
    let mut img_type = None;
    let mut digest = None;
    let mut pubkey_digest = None;
    let mut signature = None;

    let mut pos = IMAGE_HEADER_OFFSET;
    loop {
        // Skip padding between fields.
        while pos < IMAGE_HEADER_SIZE && raw[pos] == HDR_PADDING {
            pos += 1;
        }
        if pos + 4 > IMAGE_HEADER_SIZE {
            // Ran out of header without an end marker.
            return Err(RustbootError::InvalidImage);
        }
        let tag_offset = pos;
        let tag = u16::from_le_bytes([raw[pos], raw[pos + 1]]);
        if tag == TAG_END {
            break;
        }
        let len = u16::from_le_bytes([raw[pos + 2], raw[pos + 3]]) as usize;
        pos += 4;
        if pos + len > IMAGE_HEADER_SIZE {
            return Err(RustbootError::InvalidHdrFieldLength);
        }
        let value = &raw[pos..pos + len];
        pos += len;

        let (slot_is_set, expected_len) = match tag {
            TAG_VERSION => (version.is_some(), HDR_VERSION_LEN),
            TAG_TIMESTAMP => (timestamp.is_some(), HDR_TIMESTAMP_LEN),
            TAG_IMG_TYPE => (img_type.is_some(), HDR_IMG_TYPE_LEN),
            TAG_SHA256 => (digest.is_some(), SHA256_DIGEST_SIZE),
            TAG_PUBKEY_DIGEST => (pubkey_digest.is_some(), SHA256_DIGEST_SIZE),
            TAG_SIGNATURE => (signature.is_some(), ECC_SIGNATURE_SIZE),
            _ => return Err(RustbootError::InvalidValue),
        };
        if slot_is_set {
            return Err(RustbootError::InvalidImage);
        }
        if len != expected_len {
            return Err(RustbootError::InvalidHdrFieldLength);
        }
        match tag {
            // rbsigner writes the version with `to_le_bytes`. Upstream's
            // `get_firmware_version` reads it big-endian, which turns version 1
            // into 0x0100_0000; this follows what the signer actually writes.
            TAG_VERSION => {
                version = Some(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
            }
            TAG_TIMESTAMP => {
                let mut b = [0u8; 8];
                b.copy_from_slice(value);
                timestamp = Some(u64::from_le_bytes(b));
            }
            TAG_IMG_TYPE => img_type = Some(u16::from_le_bytes([value[0], value[1]])),
            TAG_SHA256 => digest = Some((value, tag_offset)),
            TAG_PUBKEY_DIGEST => pubkey_digest = Some(value),
            TAG_SIGNATURE => signature = Some(value),
            _ => unreachable!(),
        }
    }

    let (digest, digest_offset) = digest.ok_or(RustbootError::TLVNotFound)?;
    Ok(Header {
        raw,
        fw_size,
        version: version.ok_or(RustbootError::TLVNotFound)?,
        timestamp,
        img_type: img_type.ok_or(RustbootError::TLVNotFound)?,
        digest,
        digest_offset,
        pubkey_digest: pubkey_digest.ok_or(RustbootError::TLVNotFound)?,
        signature: signature.ok_or(RustbootError::TLVNotFound)?,
    })
}

/// Why verification failed, in enough detail to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// The header could not be parsed.
    Header(RustbootError),
    /// `fw` is not the length the header says.
    SizeMismatch,
    /// The image type is not a signed (ECDSA P-256) application.
    WrongImageType(u16),
    /// The firmware or header bytes do not match the stored SHA-256.
    HashMismatch,
    /// The image was signed by a different key than the one in the bootloader.
    WrongKey,
    /// The key matches but the signature does not verify.
    BadSignature,
    /// The embedded public key is not a valid P-256 point.
    BadPublicKey,
}

/// Checks an image's integrity and signature.
///
/// `pubkey` is the raw 64-byte P-256 public key (`X || Y`), the first 64
/// bytes of the key file `rbsigner` reads.
pub fn verify(hdr: &Header<'_>, fw: &[u8], pubkey: &[u8; 64]) -> core::result::Result<(), VerifyError> {
    if fw.len() != hdr.fw_size {
        return Err(VerifyError::SizeMismatch);
    }
    // High byte: authentication type. Low byte: image type (application).
    if hdr.img_type & HDR_MASK_HIGHBYTE != HDR_IMG_TYPE_AUTH
        || hdr.img_type & HDR_MASK_LOWBYTE != HDR_IMG_TYPE_APP
    {
        return Err(VerifyError::WrongImageType(hdr.img_type));
    }

    let mut hasher = Sha256::new();
    hasher.update(&hdr.raw[..hdr.digest_offset]);
    hasher.update(fw);
    if hasher.clone().finalize().as_slice() != hdr.digest {
        return Err(VerifyError::HashMismatch);
    }

    // Checked before the signature so a key mix-up is reported as such,
    // rather than as a bad signature.
    if Sha256::digest(pubkey).as_slice() != hdr.pubkey_digest {
        return Err(VerifyError::WrongKey);
    }

    let point = EncodedPoint::from_untagged_bytes(
        GenericArray::<u8, <FieldSize<NistP256> as core::ops::Add>::Output>::from_slice(pubkey),
    );
    let verify_key = VerifyingKey::from_encoded_point(&point).map_err(|_| VerifyError::BadPublicKey)?;
    match (NistP256Signature { verify_key }).verify(hasher, hdr.signature) {
        Ok(true) => Ok(()),
        _ => Err(VerifyError::BadSignature),
    }
}
