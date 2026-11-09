//! Reads an image header's size and version, without checking anything
//! cryptographic.
//!
//! An application installing an update uses this to refuse an oversized or
//! too-old image before erasing a slot. It does not make an image trusted:
//! rustBoot verifies the hash and signature before it ever boots one, so a
//! forged header here costs a wasted install, not a compromise.
//!
//! Header format (as `rbsigner mcu-image` writes it): 256 bytes; `magic`
//! "RUST" and the little-endian firmware size, then type-length-value fields
//! from offset 8 with `0xFF` padding allowed between them, ending at tag 0.

pub const HEADER_SIZE: usize = 0x100;

/// Hybrid images carry an LMS trailer after the firmware: `"LMS1"`, the
/// signature length (u32 LE) and an HSS signature (L=1, LMS_SHA256_M32_H10,
/// LMOTS_SHA256_N32_W4: 2512 bytes). Fixed size, so an image is exactly
/// header + firmware + trailer. Matches rbsigner's `lmssigner`.
pub const LMS_TRAILER_LEN: usize = 8 + 2512;
const MAGIC: u32 = 0x5453_5552; // "RUST", little-endian
const TAG_END: u16 = 0x0000;
const TAG_VERSION: u16 = 0x0001;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    /// Firmware bytes after the header.
    pub fw_size: usize,
    pub version: u32,
}

pub fn image_info(hdr: &[u8; HEADER_SIZE]) -> Option<ImageInfo> {
    let word = |i: usize| u32::from_le_bytes([hdr[i], hdr[i + 1], hdr[i + 2], hdr[i + 3]]);
    if word(0) != MAGIC {
        return None;
    }
    let fw_size = word(4) as usize;
    if fw_size == 0 {
        return None;
    }
    let mut pos = 8;
    loop {
        while pos < HEADER_SIZE && hdr[pos] == 0xFF {
            pos += 1;
        }
        if pos + 4 > HEADER_SIZE {
            return None;
        }
        let tag = u16::from_le_bytes([hdr[pos], hdr[pos + 1]]);
        if tag == TAG_END {
            return None; // no version field
        }
        let len = u16::from_le_bytes([hdr[pos + 2], hdr[pos + 3]]) as usize;
        pos += 4;
        if pos + len > HEADER_SIZE {
            return None;
        }
        if tag == TAG_VERSION {
            if len != 4 {
                return None;
            }
            return Some(ImageInfo { fw_size, version: word(pos) });
        }
        pos += len;
    }
}
