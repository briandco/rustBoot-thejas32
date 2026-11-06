use crate::log::{decode, encode, Nor, StateLog, RECORD, SECTOR};
use crate::xmodem::{receive, Channel, XmodemError};
use crate::*;
use std::collections::VecDeque;

// ---------------------------------------------------------------- flash model

#[derive(Debug, PartialEq)]
struct PowerLoss;

/// NOR flash: program can only clear bits, erase sets a 4 KB sector to 0xFF.
/// `budget` counts programmed bytes and erases; when it runs out mid-operation
/// the operation is left partly done and the "power" stays off.
struct SimNor {
    base: u32,
    mem: Vec<u8>,
    budget: Option<usize>,
    dead: bool,
    /// Programs onto bytes that were not erased. The log must never do this.
    overwrites: usize,
}

impl SimNor {
    fn new(base: u32, size: usize) -> Self {
        SimNor { base, mem: vec![0xFF; size], budget: None, dead: false, overwrites: 0 }
    }
    fn spend(&mut self) -> bool {
        match self.budget {
            Some(0) => {
                self.dead = true;
                false
            }
            Some(ref mut n) => {
                *n -= 1;
                true
            }
            None => true,
        }
    }
    fn reboot(&mut self) {
        self.dead = false;
        self.budget = None;
    }
}

impl Nor for SimNor {
    type Error = PowerLoss;
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), PowerLoss> {
        if self.dead {
            return Err(PowerLoss);
        }
        let o = (addr - self.base) as usize;
        buf.copy_from_slice(&self.mem[o..o + buf.len()]);
        Ok(())
    }
    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), PowerLoss> {
        assert_eq!(addr as usize / 256, (addr as usize + data.len() - 1) / 256, "program crosses a page");
        for (i, &b) in data.iter().enumerate() {
            if self.dead || !self.spend() {
                return Err(PowerLoss);
            }
            let o = (addr - self.base) as usize + i;
            if self.mem[o] != 0xFF {
                self.overwrites += 1;
            }
            self.mem[o] &= b;
        }
        Ok(())
    }
    fn erase_sector(&mut self, addr: u32) -> Result<(), PowerLoss> {
        let o = (addr - self.base) as usize;
        if self.dead {
            return Err(PowerLoss);
        }
        if !self.spend() {
            // Interrupted erase: part of the sector erased, the rest not.
            for b in &mut self.mem[o..o + SECTOR as usize / 2] {
                *b = 0xFF;
            }
            return Err(PowerLoss);
        }
        for b in &mut self.mem[o..o + SECTOR as usize] {
            *b = 0xFF;
        }
        Ok(())
    }
}

const BASE: u32 = 0x7_0000;
fn log() -> StateLog {
    StateLog { sectors: [BASE, BASE + SECTOR] }
}

fn nth_state(i: u32) -> BootState {
    BootState {
        active: if i % 2 == 0 { Slot::A } else { Slot::B },
        pending: if i % 3 == 0 { None } else { Some((Slot::B, i)) },
        trial: if i % 5 == 0 { Some((Slot::A, i + 1)) } else { None },
        min_version: i,
    }
}

// ---------------------------------------------------------------- state log

#[test]
fn record_round_trips() {
    for i in 0..50 {
        let s = nth_state(i);
        assert_eq!(decode(&encode(&s, i * 7)), Some((s, i * 7)));
    }
}

#[test]
fn every_single_bit_flip_is_rejected() {
    let r = encode(&nth_state(4), 9);
    for byte in 0..RECORD {
        for bit in 0..8 {
            let mut bad = r;
            bad[byte] ^= 1 << bit;
            assert_eq!(decode(&bad), None, "flip at byte {byte} bit {bit} accepted");
        }
    }
}

#[test]
fn blank_flash_loads_default() {
    let mut nor = SimNor::new(BASE, 2 * SECTOR as usize);
    let (s, _) = log().load(&mut nor).unwrap();
    assert_eq!(s, BootState::default());
}

#[test]
fn garbage_flash_loads_default_and_is_then_usable() {
    let mut nor = SimNor::new(BASE, 2 * SECTOR as usize);
    for (i, b) in nor.mem.iter_mut().enumerate() {
        *b = (i * 31 + 7) as u8 & 0x7F; // never 0xFF, never a valid record
    }
    let (s, cur) = log().load(&mut nor).unwrap();
    assert_eq!(s, BootState::default());
    let want = nth_state(3);
    log().save(&mut nor, cur, &want).unwrap();
    assert_eq!(log().load(&mut nor).unwrap().0, want);
}

#[test]
fn many_saves_cross_both_sectors_and_never_overwrite() {
    let mut nor = SimNor::new(BASE, 2 * SECTOR as usize);
    let (_, mut cur) = log().load(&mut nor).unwrap();
    for i in 1..=600 {
        cur = log().save(&mut nor, cur, &nth_state(i)).unwrap();
        // A fresh load (as after a reset) sees exactly the latest.
        let (s, c) = log().load(&mut nor).unwrap();
        assert_eq!(s, nth_state(i));
        assert_eq!(c, cur);
    }
    assert_eq!(nor.overwrites, 0);
}

/// Cuts power at every byte of every program and at every erase, across
/// enough saves to wrap both sectors, then "reboots" and checks the state.
#[test]
fn power_loss_at_every_point_never_loses_a_completed_save() {
    const SAVES: u32 = 270; // > 2 sectors of 128 records
    let mut cut = 0usize;
    let mut cuts_tested = 0;
    loop {
        let mut nor = SimNor::new(BASE, 2 * SECTOR as usize);
        let (_, mut cur) = log().load(&mut nor).unwrap();
        nor.budget = Some(cut);
        let mut completed = 0u32;
        let mut interrupted = false;
        for i in 1..=SAVES {
            match log().save(&mut nor, cur, &nth_state(i)) {
                Ok(c) => {
                    cur = c;
                    completed = i;
                }
                Err(_) => {
                    interrupted = true;
                    break;
                }
            }
        }
        if !interrupted {
            break; // the budget outlasted every save: all cut points covered
        }
        nor.reboot();
        let (s, c) = log().load(&mut nor).unwrap();
        let before = if completed == 0 { BootState::default() } else { nth_state(completed) };
        let after = nth_state(completed + 1);
        assert!(s == before || s == after, "cut {cut}: loaded {s:?}, want state {completed} or {}", completed + 1);
        // The log keeps working after the cut.
        let next = nth_state(1000 + cut as u32);
        log().save(&mut nor, c, &next).unwrap();
        assert_eq!(log().load(&mut nor).unwrap().0, next, "cut {cut}: save after reboot lost");
        assert_eq!(nor.overwrites, 0, "cut {cut}: programmed over non-erased bytes");
        cut += 1;
        cuts_tested += 1;
    }
    assert!(cuts_tested > 270 * RECORD, "only {cuts_tested} cut points");
}

// ---------------------------------------------------------------- boot decision

/// `verify` stand-in: version per slot, `None` = invalid. Records calls.
fn verifier(a: Option<u32>, b: Option<u32>, calls: &mut Vec<Slot>) -> impl FnMut(Slot) -> Option<u32> + '_ {
    move |s| {
        calls.push(s);
        match s {
            Slot::A => a,
            Slot::B => b,
        }
    }
}

#[test]
fn normal_boot_uses_active() {
    let mut calls = vec![];
    let p = plan(BootState::default(), verifier(Some(1), None, &mut calls));
    assert_eq!(p.outcome, Outcome::Boot { slot: Slot::A, version: 1, trial: false });
    assert_eq!(p.state, BootState::default());
    assert_eq!(calls, [Slot::A]);
}

#[test]
fn full_update_cycle_install_trial_confirm() {
    let s0 = BootState { min_version: 1, ..Default::default() };
    let (target, s1) = install_begin(&s0, 2).unwrap();
    assert_eq!(target, Slot::B);
    let s2 = install_done(&s1, target, 2);

    let mut calls = vec![];
    let p = plan(s2, verifier(Some(1), Some(2), &mut calls));
    assert_eq!(p.outcome, Outcome::Boot { slot: Slot::B, version: 2, trial: true });
    assert_eq!(p.state.trial, Some((Slot::B, 2)));
    assert_eq!(p.state.pending, None);

    let mut s3 = p.state;
    assert!(confirm(&mut s3));
    assert_eq!(s3.active, Slot::B);
    assert_eq!(s3.min_version, 2);
    assert_eq!(s3.trial, None);

    let mut calls = vec![];
    let p = plan(s3, verifier(Some(1), Some(2), &mut calls));
    assert_eq!(p.outcome, Outcome::Boot { slot: Slot::B, version: 2, trial: false });
}

#[test]
fn unconfirmed_trial_rolls_back() {
    let s = BootState { trial: Some((Slot::B, 3)), ..Default::default() };
    let mut calls = vec![];
    let p = plan(s, verifier(Some(2), Some(3), &mut calls));
    assert_eq!(p.notes.rolled_back, Some(Slot::B));
    assert_eq!(p.outcome, Outcome::Boot { slot: Slot::A, version: 2, trial: false });
    assert_eq!(p.state.trial, None);
    assert_eq!(calls, [Slot::A], "the failed trial slot must not be booted again");
}

#[test]
fn invalid_pending_falls_back_to_active() {
    let s = BootState { pending: Some((Slot::B, 3)), ..Default::default() };
    let mut calls = vec![];
    let p = plan(s, verifier(Some(2), None, &mut calls));
    assert!(p.notes.pending_rejected);
    assert_eq!(p.outcome, Outcome::Boot { slot: Slot::A, version: 2, trial: false });
    assert_eq!(p.state.pending, None);
}

#[test]
fn pending_version_must_match_what_was_installed() {
    // Slot B holds a valid image, but not the one install recorded.
    let s = BootState { pending: Some((Slot::B, 5)), ..Default::default() };
    let mut calls = vec![];
    let p = plan(s, verifier(Some(2), Some(4), &mut calls));
    assert!(p.notes.pending_rejected);
    assert_eq!(p.outcome, Outcome::Boot { slot: Slot::A, version: 2, trial: false });
}

#[test]
fn broken_active_falls_back_to_other_slot() {
    let mut calls = vec![];
    let p = plan(BootState::default(), verifier(None, Some(4), &mut calls));
    assert!(p.notes.fell_back);
    assert_eq!(p.state.active, Slot::B);
    assert_eq!(p.outcome, Outcome::Boot { slot: Slot::B, version: 4, trial: false });
}

#[test]
fn old_images_are_never_booted() {
    let s = BootState { min_version: 5, ..Default::default() };
    let mut calls = vec![];
    let p = plan(s, verifier(Some(4), Some(3), &mut calls));
    assert!(p.notes.below_min_version);
    assert_eq!(p.outcome, Outcome::Recovery);
}

#[test]
fn old_images_are_never_installed() {
    let s = BootState { min_version: 5, ..Default::default() };
    assert_eq!(install_begin(&s, 4), Err(InstallError::VersionTooOld { version: 4, min: 5 }));
    assert!(install_begin(&s, 5).is_ok(), "reinstalling the same version is allowed");
}

#[test]
fn nothing_valid_means_recovery() {
    let mut calls = vec![];
    let p = plan(BootState::default(), verifier(None, None, &mut calls));
    assert_eq!(p.outcome, Outcome::Recovery);
}

#[test]
fn installing_during_a_trial_replaces_the_trial_image() {
    let s = BootState { trial: Some((Slot::B, 3)), ..Default::default() };
    let (target, next) = install_begin(&s, 4).unwrap();
    assert_eq!(target, Slot::B);
    assert_eq!(next.trial, None, "the slot being overwritten must not stay referenced");
}

#[test]
fn confirm_without_trial_changes_nothing() {
    let mut s = BootState::default();
    assert!(!confirm(&mut s));
    assert_eq!(s, BootState::default());
}

// ---------------------------------------------------------------- xmodem

/// Scripted XMODEM sender that reacts to what the receiver writes.
struct Sender {
    rx: VecDeque<u8>,
    blocks: Vec<Vec<u8>>,
    idx: usize,
    done: bool,
    corrupt: Vec<usize>,   // corrupt these blocks the first time they are sent
    lose_ack: Vec<usize>,  // pretend the ACK for these blocks was lost
    silent: bool,
}

fn make_blocks(data: &[u8], size: usize) -> Vec<Vec<u8>> {
    data.chunks(size)
        .enumerate()
        .map(|(i, chunk)| {
            let mut d = chunk.to_vec();
            d.resize(size, 0x1A);
            let n = (i + 1) as u8;
            let mut b = vec![if size == 128 { 0x01 } else { 0x02 }, n, !n];
            b.extend_from_slice(&d);
            b.extend_from_slice(&crc::crc16_xmodem(&d).to_be_bytes());
            b
        })
        .collect()
}

impl Sender {
    fn new(data: &[u8], size: usize) -> Self {
        Sender { rx: VecDeque::new(), blocks: make_blocks(data, size), idx: 0, done: false, corrupt: vec![], lose_ack: vec![], silent: false }
    }
    fn push_current(&mut self) {
        if self.idx == self.blocks.len() {
            self.rx.push_back(0x04);
            return;
        }
        let mut b = self.blocks[self.idx].clone();
        if let Some(pos) = self.corrupt.iter().position(|&c| c == self.idx) {
            self.corrupt.remove(pos);
            b[10] ^= 0xFF;
        }
        self.rx.extend(b);
    }
}

impl Channel for Sender {
    fn read_byte(&mut self, _timeout_ms: u32) -> Option<u8> {
        self.rx.pop_front()
    }
    fn write_byte(&mut self, b: u8) {
        if self.silent || self.done {
            return;
        }
        match b {
            b'C' if self.idx == 0 && self.rx.is_empty() => self.push_current(),
            0x06 => {
                if self.idx == self.blocks.len() {
                    self.done = true;
                } else if let Some(pos) = self.lose_ack.iter().position(|&c| c == self.idx) {
                    self.lose_ack.remove(pos);
                    self.push_current(); // resend the same block
                } else {
                    self.idx += 1;
                    self.push_current();
                }
            }
            0x15 => self.push_current(),
            _ => {}
        }
    }
}

fn image(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 13 + 5) as u8).collect()
}

#[test]
fn xmodem_receives_128_byte_blocks() {
    let data = image(1000);
    let mut s = Sender::new(&data, 128);
    let mut buf = vec![0u8; 4096];
    let n = receive(&mut s, &mut buf).unwrap();
    assert_eq!(n, 1024);
    assert_eq!(&buf[..1000], &data[..]);
    assert!(buf[1000..1024].iter().all(|&b| b == 0x1A));
    assert!(s.done);
}

#[test]
fn xmodem_receives_1k_blocks() {
    let data = image(3000);
    let mut s = Sender::new(&data, 1024);
    let mut buf = vec![0u8; 4096];
    assert_eq!(receive(&mut s, &mut buf).unwrap(), 3072);
    assert_eq!(&buf[..3000], &data[..]);
}

#[test]
fn xmodem_recovers_from_corrupt_blocks() {
    let data = image(1000);
    let mut s = Sender::new(&data, 128);
    s.corrupt = vec![0, 3, 7];
    let mut buf = vec![0u8; 4096];
    assert_eq!(receive(&mut s, &mut buf).unwrap(), 1024);
    assert_eq!(&buf[..1000], &data[..]);
}

#[test]
fn xmodem_ignores_a_repeated_block() {
    let data = image(1000);
    let mut s = Sender::new(&data, 128);
    s.lose_ack = vec![2, 5];
    let mut buf = vec![0u8; 4096];
    assert_eq!(receive(&mut s, &mut buf).unwrap(), 1024, "a duplicate must not be stored twice");
    assert_eq!(&buf[..1000], &data[..]);
}

#[test]
fn xmodem_refuses_an_image_larger_than_the_buffer() {
    let data = image(1000);
    let mut s = Sender::new(&data, 128);
    let mut buf = vec![0u8; 512];
    assert_eq!(receive(&mut s, &mut buf), Err(XmodemError::Overflow));
}

#[test]
fn xmodem_gives_up_without_a_sender() {
    let mut s = Sender::new(&image(10), 128);
    s.silent = true;
    let mut buf = vec![0u8; 512];
    assert_eq!(receive(&mut s, &mut buf), Err(XmodemError::NoSender));
}

#[test]
fn checksums_match_standard_check_values() {
    // The published "123456789" check values for CRC-32/ISO-HDLC and CRC-16/XMODEM.
    assert_eq!(crc::crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc::crc16_xmodem(b"123456789"), 0x31C3);
}
