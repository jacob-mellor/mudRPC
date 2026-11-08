//! The keys-die gate's scratch half (SPEC.md, the bar): what a stack probe finds after `sign`, `public_key`, a signed
//! `resume` and a signed `call`.
//! Needles live on the heap, the stack is scrubbed before the call under test, and the same probe runs without the
//! call as the control; a planted needle proves the probe can see. Reading the untouched stack is the one unsafe line a
//! test in this repository has, and it is what the gate is about. Measured on the release build, the product's shape: a
//! debug build copies every value it names and no zeroing reaches the copies.
//!   cargo test --release -p mudrpc-auth --test scrub
#![cfg(not(debug_assertions))] // `--include-ignored` would run an ignored test in debug; the file is compiled out instead
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals
    clippy::large_stack_arrays, // the 256 KiB frame is the scrub; the probe reads the 128 KiB below it
    clippy::missing_const_for_fn, // `scrub` exists for its effect on the stack at run time
)]
use mudrpc_auth::ed25519::{public_key, sign};
use mudrpc_auth::sha512::sha512;
use mudrpc_auth::signed::Signed;
use mudrpc_core::rpc::Peer;
use mudrpc_core::seal::Key;

type S = Signed<Key, 8>;
type N = Peer<S, 256, 4, 8, 16, 4>;
const KEY: [u8; 32] = [7; 32];

#[inline(never)]
fn scrub() { let mut z = [0u8; 256 * 1024]; std::hint::black_box(&mut z); }

/// Which needles the 128 KiB of stack below this frame holds.
#[inline(never)]
fn probe(needles: &[Vec<u8>]) -> Vec<bool> {
    let mut buf = core::mem::MaybeUninit::<[u8; 128 * 1024]>::uninit();
    let bytes: &[u8] = unsafe { core::slice::from_raw_parts(buf.as_mut_ptr() as *const u8, 128 * 1024) };
    needles.iter().map(|n| bytes.windows(n.len()).any(|w| w == &n[..])).collect()
}

/// A frame that leaves a needle on the stack on purpose, deep enough that the probe's own locals do not cover it: the
/// control that proves the probe sees.
#[inline(never)]
fn plant(needle: &[u8]) { let mut spot = [0u8; 4096]; spot[2048..2048 + needle.len()].copy_from_slice(needle); std::hint::black_box(&mut spot); }

#[inline(never)]
fn do_sign(sd: &[u8; 32], p: &[u8; 32]) -> [u8; 64] { sign(sd, p, &[b"hello"]) }

/// The seed's secret scalar and prefix, the needles every probe here looks for.
fn secrets(seed: &[u8; 32]) -> Vec<Vec<u8>> {
    let h = sha512(&[&seed[..]]);
    let mut a = h[..32].to_vec();
    a[0] &= 248;
    a[31] = (a[31] & 63) | 64;
    vec![a, h[32..].to_vec()]
}

#[test]
fn sign_leaves_nothing_of_the_seed_on_the_stack() {
    let sd = Box::new([0x5A; 32]);
    let p = Box::new(public_key(&sd));
    let needles = secrets(&sd);
    scrub();
    assert_eq!(probe(&needles), [false, false], "the control is clean");
    scrub();
    plant(&needles[1]);
    assert_eq!(probe(&needles), [false, true], "the probe sees a planted prefix");
    scrub();
    std::hint::black_box(do_sign(&sd, &p));
    assert_eq!(probe(&needles), [false, false], "[secret scalar, prefix] found after sign");
}

#[inline(never)]
fn signed_send(p: &mut N, payload: &[u8]) { p.call(0, "nav", "Upload", payload, 30).unwrap(); }

#[test]
fn a_signed_send_leaves_no_plaintext_on_the_stack() {
    let payload = b"MISSION-WAYPOINTS-SECRET-0123456789abcdefXYZ";
    let mut needle = vec![0x1a, payload.len() as u8]; // the envelope's payload field as encoded: tag, length, bytes
    needle.extend_from_slice(payload);
    let needles = vec![needle];
    let mut s = Box::new(N::new("gcs", S::new(Key::new(KEY), [9; 32], 1000), 1, 1).unwrap());
    scrub();
    assert_eq!(probe(&needles), [false], "the control is clean");
    scrub();
    plant(&needles[0]);
    assert_eq!(probe(&needles), [true], "the probe sees a planted payload");
    scrub();
    signed_send(&mut s, payload);
    assert_eq!(probe(&needles), [false], "the encoded plaintext found after a signed call");
}

#[inline(never)]
fn do_public_key(sd: &[u8; 32]) -> [u8; 32] { public_key(sd) }
#[inline(never)]
fn do_resume(p: &mut N) { p.resume(2).unwrap(); }

#[test]
fn public_key_and_a_signed_resume_leave_nothing_of_the_seed_on_the_stack() {
    let sd = Box::new([0x3C; 32]);
    let needles = secrets(&sd);
    let mut s = Box::new(N::new("gcs", S::new(Key::new(KEY), *sd, 1000), 1, 1).unwrap());
    scrub();
    assert_eq!(probe(&needles), [false, false], "the control is clean");
    scrub();
    plant(&needles[0]);
    assert_eq!(probe(&needles), [true, false], "the probe sees a planted scalar");
    scrub();
    std::hint::black_box(do_public_key(&sd));
    assert_eq!(probe(&needles), [false, false], "[secret scalar, prefix] found after public_key");
    scrub();
    do_resume(&mut s);
    assert_eq!(probe(&needles), [false, false], "[secret scalar, prefix] found after a signed resume, which checks the public key");
}
