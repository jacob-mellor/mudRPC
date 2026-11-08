//! The keys-die gate's AES half (SPEC.md, the bar): what a stack probe finds after a `Gcm` seal and open, and after the
//! key schedule alone.
//! The probe, the scrub and the planted control are auth/tests/scrub.rs's; the needles are the fleet key, the header key,
//! round keys of both schedules, GHASH's key and mask, and the keystream. Release shape only, as there.
//!   cargo test --release -p mudrpc-gcm --test scrub
#![cfg(not(debug_assertions))] // `--include-ignored` would run an ignored test in debug; the file is compiled out instead
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // the key's bytes are a small formula
    clippy::large_stack_arrays, // the 256 KiB frame is the scrub; the probe reads the 128 KiB below it
    clippy::missing_const_for_fn, // `scrub` exists for its effect on the stack at run time
)]
use mudrpc_core::seal::Sealer;
use mudrpc_gcm::{aes, Gcm};

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

/// The key and the header key, then for each one's schedule round key 1 and 14 as `encrypt` reads them and round key 14
/// as FIPS 197 writes it, the last words the expansion holds; then H, E(K, J₀) and the keystream of the first two blocks.
fn secrets(key: &[u8; 32]) -> Vec<Vec<u8>> {
    let words = |q: &[u32]| q.iter().flat_map(|w| w.to_ne_bytes()).collect::<Vec<u8>>();
    let rounds = |sk: &aes::Schedule| {
        let mut q: [u32; 8] = sk.0[112..].try_into().unwrap();
        aes::ortho(&mut q);
        [words(&sk.0[8..16]), words(&sk.0[112..]), words(&[q[0], q[2], q[4], q[6]])]
    };
    let sk = aes::schedule(key);
    let mut b = [[0xffu8; 16], [0xff; 16]];
    b[1][0] = 0xfe;
    for b in &mut b { b[12..].fill(0); }
    aes::encrypt(&sk, &mut b);
    let hk: [u8; 32] = b.as_flattened().try_into().unwrap();
    let mut z = [[0u8; 16]; 2];
    aes::encrypt(&aes::schedule(&hk), &mut z);
    assert_eq!(z[0][..12], mudrpc_gcm::check(key), "the header key as `Gcm` derives it");
    let block = |i: u8| { let mut c = [0u8; 16]; c[..12].copy_from_slice(&NONCE); c[15] = i; c };
    let (mut hj, mut ks) = ([[0; 16], block(1)], [block(2), block(3)]);
    aes::encrypt(&sk, &mut hj);
    aes::encrypt(&sk, &mut ks);
    let tail = vec![hj[0].to_vec(), hj[1].to_vec(), ks.as_flattened().to_vec()];
    [vec![key.to_vec(), hk.to_vec()], rounds(&sk).to_vec(), rounds(&aes::schedule(&hk)).to_vec(), tail].concat()
}

#[inline(never)]
fn seal_and_open(g: &mut Gcm) {
    let (mut plain, mut wire, mut out) = (*b"waypoint", [0u8; 64], [0u8; 64]);
    let n = g.seal(&NONCE, b"nav", &mut plain, 8, &mut wire).unwrap();
    assert_eq!(g.open(b"nav", &wire[..n], &mut out).unwrap().len, 8);
}

/// The free function the vectors use, which leaves a different stack than the sealer's pair.
#[inline(never)]
fn free_seal(key: &[u8; 32]) { std::hint::black_box(mudrpc_gcm::seal(key, &NONCE, b"nav", b"waypoint", &mut [0u8; 64]).unwrap()); }

#[test]
fn a_gcm_seal_and_open_leave_no_key_material_on_the_stack() {
    let key = key();
    let needles = secrets(&key);
    let mut g = Box::new(Gcm::new(key));
    scrub();
    assert_eq!(probe(&needles), [false; 11], "the control is clean");
    scrub();
    plant(&needles[2]);
    assert_eq!(probe(&needles), [false, false, true, false, false, false, false, false, false, false, false], "the probe sees a planted round key");
    scrub();
    seal_and_open(&mut g);
    assert_eq!(probe(&needles), [false; 11], "[key, header key, each one's round keys 1, 14 and 14 as words, H, E(K, J0), keystream] found after a seal and an open");
    scrub();
    free_seal(&key);
    assert_eq!(probe(&needles), [false; 11], "[the same] found after the free seal");
}

#[inline(never)]
fn schedule_only(key: &[u8; 32]) { std::hint::black_box(&aes::schedule(key)); }

#[test]
fn the_schedule_alone_leaves_no_round_key_on_the_stack() {
    let key = key();
    let needles = secrets(&key);
    scrub();
    schedule_only(&key);
    assert_eq!(probe(&needles), [false; 11], "[key, header key, each one's round keys 1, 14 and 14 as words, H, E(K, J0), keystream] found after the schedule");
}
