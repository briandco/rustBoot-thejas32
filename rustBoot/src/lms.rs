//! LMS / HSS signature verification (RFC 8554), for small stacks.
//!
//! Verification only. Signing is done on the host with `hbs-lms`, which
//! handles the stateful private key. On THEJAS32, `hbs-lms`'s own verifier
//! needs 48-64 KB of stack (measured: `lm_ots::verify::generate_public_key_
//! candiate` has a 43 KB frame, because its hash-chain buffer reserves room
//! for every Winternitz parameter at once), more than rustBoot has. This
//! implementation computes each hash chain and feeds it straight into the
//! public-key hash, so it holds a few hash states at a time.
//!
//! Follows RFC 8554 Algorithm 4b (LM-OTS candidate public key), 6a (LMS
//! candidate root) and 6 (HSS). Supports the SHA-256 parameter sets with
//! n = m = 32: LM-OTS W1/W2/W4/W8 and LMS H5..H25. Anything else is
//! rejected. Tested against the RFC 8554 test vectors and against `hbs-lms`
//! signatures (rbsigner's tests).

use sha2::{Digest, Sha256};

const N: usize = 32;
const D_PBLC: [u8; 2] = [0x80, 0x80];
const D_MESG: [u8; 2] = [0x81, 0x81];
const D_LEAF: [u8; 2] = [0x82, 0x82];
const D_INTR: [u8; 2] = [0x83, 0x83];

/// The most HSS levels RFC 8554 allows.
const MAX_LEVELS: u32 = 8;

#[derive(Clone, Copy)]
struct Lmots {
    typecode: u32,
    w: u32,
    p: usize,
    ls: u32,
}

/// RFC 8554 Table 1 (SHA-256, n = 32).
fn lmots(typecode: u32) -> Option<Lmots> {
    let (w, p, ls) = match typecode {
        1 => (1, 265, 7),
        2 => (2, 133, 6),
        3 => (4, 67, 4),
        4 => (8, 34, 0),
        _ => return None,
    };
    Some(Lmots { typecode, w, p, ls })
}

/// RFC 8554 Table 2 (SHA-256, m = 32): tree height for a typecode.
fn lms_height(typecode: u32) -> Option<u32> {
    match typecode {
        5 => Some(5),
        6 => Some(10),
        7 => Some(15),
        8 => Some(20),
        9 => Some(25),
        _ => None,
    }
}

/// A byte-slice reader that fails instead of panicking.
struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.data.len() < n {
            return None;
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Some(head)
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

struct LmsPublicKey<'a> {
    lms_type: u32,
    height: u32,
    ots: Lmots,
    id: &'a [u8],   // I, 16 bytes
    root: &'a [u8], // T[1], 32 bytes
    /// The whole encoded key, which is what a parent level signs.
    encoded: &'a [u8],
}

fn read_public_key<'a>(r: &mut Reader<'a>) -> Option<LmsPublicKey<'a>> {
    let start = r.data;
    let lms_type = r.u32()?;
    let height = lms_height(lms_type)?;
    let ots = lmots(r.u32()?)?;
    let id = r.take(16)?;
    let root = r.take(N)?;
    let encoded = &start[..start.len() - r.data.len()];
    Some(LmsPublicKey { lms_type, height, ots, id, root, encoded })
}

/// coef(S, i, w) from RFC 8554 Section 3.1.3.
fn coef(s: &[u8], i: usize, w: u32) -> u32 {
    let w = w as usize;
    let mask = (1u32 << w) - 1;
    let byte = s[i * w / 8] as u32;
    let shift = 8 - (w * (i % (8 / w)) + w);
    mask & (byte >> shift)
}

/// Algorithm 4b: the candidate LM-OTS public key for `message`.
fn lmots_candidate(
    ots: Lmots,
    id: &[u8],
    q: [u8; 4],
    message: &[u8],
    c: &[u8],
    y: &[u8],
) -> [u8; N] {
    let mut h = Sha256::new();
    h.update(id);
    h.update(q);
    h.update(D_MESG);
    h.update(c);
    h.update(message);
    let qhash = h.finalize();

    // Q || Cksm(Q)
    let mut qc = [0u8; N + 2];
    qc[..N].copy_from_slice(&qhash);
    let max = (1u32 << ots.w) - 1;
    let mut sum: u32 = 0;
    for i in 0..(N * 8 / ots.w as usize) {
        sum += max - coef(&qc[..N], i, ots.w);
    }
    let cksm = ((sum << ots.ls) as u16).to_be_bytes();
    qc[N] = cksm[0];
    qc[N + 1] = cksm[1];

    let mut kc = Sha256::new();
    kc.update(id);
    kc.update(q);
    kc.update(D_PBLC);
    for i in 0..ots.p {
        let a = coef(&qc, i, ots.w);
        let mut tmp = [0u8; N];
        tmp.copy_from_slice(&y[i * N..(i + 1) * N]);
        let i16 = (i as u16).to_be_bytes();
        for j in a..max {
            let mut h = Sha256::new();
            h.update(id);
            h.update(q);
            h.update(i16);
            h.update([j as u8]);
            h.update(tmp);
            tmp.copy_from_slice(&h.finalize());
        }
        kc.update(tmp);
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&kc.finalize());
    out
}

/// Algorithm 6a: reads one LMS signature from `r` and checks it against
/// `key` for `message`.
fn lms_verify(key: &LmsPublicKey<'_>, message: &[u8], r: &mut Reader<'_>) -> Option<bool> {
    let q_bytes = r.take(4)?;
    let q = u32::from_be_bytes([q_bytes[0], q_bytes[1], q_bytes[2], q_bytes[3]]);
    let ots_type = r.u32()?;
    if ots_type != key.ots.typecode {
        return Some(false);
    }
    let c = r.take(N)?;
    let y = r.take(key.ots.p * N)?;
    let sig_type = r.u32()?;
    if sig_type != key.lms_type {
        return Some(false);
    }
    let path = r.take(key.height as usize * N)?;
    if q >= (1u32 << key.height) {
        return Some(false);
    }
    let q4 = [q_bytes[0], q_bytes[1], q_bytes[2], q_bytes[3]];
    let kc = lmots_candidate(key.ots, key.id, q4, message, c, y);

    let mut node = (1u32 << key.height) + q;
    let mut h = Sha256::new();
    h.update(key.id);
    h.update(node.to_be_bytes());
    h.update(D_LEAF);
    h.update(kc);
    let mut tmp = [0u8; N];
    tmp.copy_from_slice(&h.finalize());
    let mut i = 0;
    while node > 1 {
        let sibling = &path[i * N..(i + 1) * N];
        let mut h = Sha256::new();
        h.update(key.id);
        h.update((node / 2).to_be_bytes());
        h.update(D_INTR);
        if node % 2 == 1 {
            h.update(sibling);
            h.update(tmp);
        } else {
            h.update(tmp);
            h.update(sibling);
        }
        tmp.copy_from_slice(&h.finalize());
        node /= 2;
        i += 1;
    }
    Some(tmp[..] == *key.root)
}

/// Algorithm 6: verifies an HSS signature over `message` with an HSS public
/// key. Returns `false` for any malformed input, unsupported parameter set,
/// or trailing bytes.
pub fn hss_verify(message: &[u8], signature: &[u8], public_key: &[u8]) -> bool {
    hss_verify_inner(message, signature, public_key).unwrap_or(false)
}

fn hss_verify_inner(message: &[u8], signature: &[u8], public_key: &[u8]) -> Option<bool> {
    let mut pk = Reader { data: public_key };
    let levels = pk.u32()?;
    if levels == 0 || levels > MAX_LEVELS {
        return Some(false);
    }
    let mut key = read_public_key(&mut pk)?;
    if !pk.data.is_empty() {
        return Some(false);
    }

    let mut sig = Reader { data: signature };
    let nspk = sig.u32()?;
    if nspk + 1 != levels {
        return Some(false);
    }
    for _ in 0..nspk {
        // Each signed public key: an LMS signature by the current key over
        // the next level's encoded public key, then that key.
        let mut ahead = Reader { data: sig.data };
        skip_lms_signature(&key, &mut ahead)?;
        let next = read_public_key(&mut ahead)?;
        if !lms_verify(&key, next.encoded, &mut sig)? {
            return Some(false);
        }
        let _ = read_public_key(&mut sig)?;
        key = next;
    }
    if !lms_verify(&key, message, &mut sig)? {
        return Some(false);
    }
    Some(sig.data.is_empty())
}

/// Advances past one LMS signature made with `key`'s parameters.
fn skip_lms_signature(key: &LmsPublicKey<'_>, r: &mut Reader<'_>) -> Option<()> {
    r.take(4 + 4 + N + key.ots.p * N + 4 + key.height as usize * N)?;
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coef_matches_rfc_8554_example() {
        // RFC 8554 Section 3.1.3: S = 0x1234, coef(S, 7, 1) = 0, coef(S, 0, 4) = 1.
        let s = [0x12, 0x34];
        assert_eq!(coef(&s, 7, 1), 0);
        assert_eq!(coef(&s, 0, 4), 1);
        assert_eq!(coef(&s, 1, 4), 2);
        assert_eq!(coef(&s, 0, 8), 0x12);
    }

    use crate::lms_rfc_vectors::*;

    #[test]
    fn rfc_8554_test_case_1() {
        assert!(hss_verify(TC1_MESSAGE, TC1_SIGNATURE, TC1_PUBLIC_KEY));
    }

    #[test]
    fn rfc_8554_test_case_2() {
        assert!(hss_verify(TC2_MESSAGE, TC2_SIGNATURE, TC2_PUBLIC_KEY));
    }

    #[test]
    fn rfc_vectors_fail_when_any_sampled_byte_changes() {
        for (msg, sig, pk) in [(TC1_MESSAGE, TC1_SIGNATURE, TC1_PUBLIC_KEY), (TC2_MESSAGE, TC2_SIGNATURE, TC2_PUBLIC_KEY)] {
            for i in (0..sig.len()).step_by(53) {
                let mut s = sig.to_vec();
                s[i] ^= 0x01;
                assert!(!hss_verify(msg, &s, pk), "signature byte {i} flipped, still accepted");
            }
            for i in 0..pk.len() {
                let mut p = pk.to_vec();
                p[i] ^= 0x80;
                assert!(!hss_verify(msg, sig, &p), "public key byte {i} flipped, still accepted");
            }
            let mut m = msg.to_vec();
            m[0] ^= 1;
            assert!(!hss_verify(&m, sig, pk), "message changed, still accepted");
            assert!(!hss_verify(msg, &sig[..sig.len() - 1], pk), "truncated");
            let mut longer = sig.to_vec();
            longer.push(0);
            assert!(!hss_verify(msg, &longer, pk), "trailing byte");
        }
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        assert!(!hss_verify(b"m", &[], &[]));
        assert!(!hss_verify(b"m", &[0; 100], &[0; 60]));
        let mut pk = [0u8; 60];
        pk[3] = 1; // L = 1
        pk[7] = 6; // LMS H10
        pk[11] = 3; // LMOTS W4
        assert!(!hss_verify(b"m", &[0xFF; 2512], &pk));
    }
}
