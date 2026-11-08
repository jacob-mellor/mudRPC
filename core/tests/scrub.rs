//! The keys-die gate's half for core's cipher (SPEC.md, the bar): what a stack probe finds after a seal and an open,
//! free and through `Key`, and after a peer's `call`.
//! The probe, the scrub and the planted control are auth/tests/scrub.rs's; the needles are the key, the keystream, the
//! Poly1305 one-time key as `Poly` holds it, the header mask, and each block's state before the final add, which the
//! rounds invert to the key. Release shape only, as there.
//!   cargo test --release -p mudrpc-core --test scrub
#![cfg(not(debug_assertions))] // `--include-ignored` would run an ignored test in debug; the file is compiled out instead
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // the key's bytes are a small formula
    clippy::large_stack_arrays, // the 256 KiB frame is the scrub; the probe reads the 128 KiB below it
    clippy::missing_const_for_fn, // `scrub` exists for its effect on the stack at run time
)]
use mudrpc_core::rpc::{Outgoing, Peer};
use mudrpc_core::seal::{self, block, Key, Sealer};

type N = Peer<Key, 256, 4, 8, 16, 4>;

#[inline(never)]
fn scrub() { let mut z = [0u8; 256 * 1024]; std::hint::black_box(&mut z); }

/// Which needles the 128 KiB of stack below this frame holds.
#[inline(never)]
fn probe(needles: &[Vec<u8>]) -> Vec<bool> {
    let mut buf = core::mem::MaybeUninit::<[u8; 128 * 1024]>::uninit();
    let bytes: &[u8] = unsafe { core::slice::from_raw_parts(buf.as_mut_ptr() as *const u8, 128 * 1024) };
    needles.iter().map(|n| bytes.windows(n.len()).any(|w| w == &n[..])).collect()
}

/// A frame that leaves a needle on the stack on purpose: the control that proves the probe sees.
#[inline(never)]
fn plant(needle: &[u8]) { let mut spot = [0u8; 4096]; spot[2048..2048 + needle.len()].copy_from_slice(needle); std::hint::black_box(&mut spot); }

/// A key no stack holds by chance.
fn key() -> [u8; 32] { core::array::from_fn(|i| (i as u8).wrapping_mul(0x9d) ^ 0x5a) }

/// The nonce of the seals under test.
const NONCE: [u8; 12] = [1; 12];

/// The plaintext of the seals under test.
const PLAIN: &[u8; 8] = b"waypoint";

const WHAT: &str = "[key, keystream 1, its state, one-time key, its state, r and pad as Poly holds them, mask block, its state, keystream 1's second half, s as Poly holds it]";

/// The key; block 1's keystream and the state it is added to; block 0's first 32 bytes, the one-time key, and its
/// state; r clamped into limbs and the pad as words; the header mask's block under `sample`, and its state; then the
/// keystream's second half and `r`'s two multiples, which the release build was seen to leave alone.
fn secrets(key: &[u8; 32], nonce: &[u8; 12], sample: &[u8; 12]) -> Vec<Vec<u8>> {
    let state = |counter: u32, n: &[u8; 12]| {
        let out = block(key, counter, n);
        let mut init = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574, 0, 0, 0, 0, 0, 0, 0, 0, counter, 0, 0, 0];
        for (w, c) in init[4..12].iter_mut().zip(key.chunks_exact(4)) { *w = u32::from_le_bytes(c.try_into().unwrap()); }
        for (w, c) in init[13..].iter_mut().zip(n.chunks_exact(4)) { *w = u32::from_le_bytes(c.try_into().unwrap()); }
        let s = out.chunks_exact(4).zip(init).flat_map(|(o, i)| u32::from_le_bytes(o.try_into().unwrap()).wrapping_sub(i).to_ne_bytes());
        [out.to_vec(), s.collect()]
    };
    let [ks, ks_state] = state(1, nonce);
    let [b0, b0_state] = state(0, nonce);
    let [mask, mask_state] = state(u32::MAX, sample);
    let le = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap());
    let (t0, t1) = (le(&b0[0..8]), le(&b0[8..16]));
    let r = [t0 & 0xffc_0fff_ffff, ((t0 >> 44) | (t1 << 20)) & 0xfff_ffc0_ffff, (t1 >> 24) & 0x000f_ffff_fc0f];
    let pad = [le(&b0[16..24]), le(&b0[24..32])];
    let words = |q: &[u64]| q.iter().flat_map(|w| w.to_ne_bytes()).collect::<Vec<u8>>();
    let half = ks[32..].to_vec();
    vec![key.to_vec(), ks, ks_state, b0[..32].to_vec(), b0_state, words(&r), words(&pad), mask, mask_state, half, words(&[r[1] * 20, r[2] * 20])]
}

#[inline(never)]
fn free_seal_and_open(key: &[u8; 32]) {
    let (mut wire, mut out) = ([0u8; 64], [0u8; 64]);
    let n = seal::seal(key, &NONCE, b"nav", PLAIN, &mut wire).unwrap();
    assert_eq!(seal::open(key, b"nav", &wire[..n], &mut out), Ok(8));
}

#[inline(never)]
fn key_seal(k: &mut Key, wire: &mut [u8; 64]) -> usize { k.seal(&NONCE, b"nav", &mut { *PLAIN }, 8, wire).unwrap() }

#[inline(never)]
fn key_open(k: &mut Key, wire: &[u8]) { assert_eq!(k.open(b"nav", wire, &mut [0u8; 64]).unwrap().len, 8); }

#[test]
fn a_chacha_seal_and_open_leave_no_keystream_or_one_time_key_on_the_stack() {
    let key = key();
    let mut wire = Box::new([0u8; 64]);
    let mut k = Box::new(Key::new(key));
    let n = key_seal(&mut k, &mut wire);
    let needles = secrets(&key, &NONCE, wire[12..24].try_into().unwrap());
    scrub();
    assert_eq!(probe(&needles), [false; 11], "the control is clean");
    scrub();
    plant(&needles[3]);
    assert_eq!(probe(&needles), [false, false, false, true, false, false, true, false, false, false, false], "the probe sees a planted one-time key and the pad in it");
    scrub();
    free_seal_and_open(&key);
    assert_eq!(probe(&needles), [false; 11], "{WHAT} found after the free seal and open");
    scrub();
    key_seal(&mut k, &mut wire);
    assert_eq!(probe(&needles), [false; 11], "{WHAT} found after Key::seal");
    scrub();
    key_open(&mut k, &wire[..n]);
    assert_eq!(probe(&needles), [false; 11], "{WHAT} found after Key::open");
}

#[inline(never)]
fn call(p: &mut N) -> [u8; 12] { p.call(0, "nav", "Upload", b"waypoint", 30).unwrap().0 }

/// The peer's first wire, past the listen it queued at `new`.
fn first_wire(p: &mut N) -> Vec<u8> {
    loop {
        match p.outgoing(0) { Some(Outgoing::Send { wire, .. }) => return wire.to_vec(), Some(_) => p.sent(), None => panic!("no wire") }
    }
}

#[test]
fn a_call_through_a_peer_leaves_no_keystream_or_one_time_key_on_the_stack() {
    let key = key();
    let mk = || Box::new(N::new("gcs", Key::new(key), 1, 1).unwrap());
    let (mut twin, mut p) = (mk(), mk());
    let nonce = call(&mut twin);
    let wire = first_wire(&mut twin);
    let needles = secrets(&key, &nonce, wire[13..25].try_into().unwrap()); // version, masked nonce, then the sample
    scrub();
    assert_eq!(probe(&needles), [false; 11], "the control is clean");
    scrub();
    plant(&needles[1]);
    assert_eq!(probe(&needles), [false, true, false, false, false, false, false, false, false, true, false], "the probe sees a planted keystream and its half");
    scrub();
    assert_eq!(call(&mut p), nonce, "the twin's nonce");
    assert_eq!(probe(&needles), [false; 11], "{WHAT} found after a call");
    assert_eq!(first_wire(&mut p), wire, "the twin's wire, so the mask needles are the call's");
}
