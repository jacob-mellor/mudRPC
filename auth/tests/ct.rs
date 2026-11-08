//! Constant time, measured (SPEC.md, the bar): a dudect-style test after Reparaz, Balasch and Verbauwhede 2016, written
//! from the paper. Two classes of input, timings interleaved at random, Welch's t over the lower 90% of each class (the
//! paper's cropping, against the operating system's noise); |t| above 4.5 is a difference the test can see. A host
//! measurement under `--ignored`, run before a release, built for release; the rig's Cortex-M4 run is the gate's real instrument.
//!   cargo test --release -p mudrpc-auth --test ct -- --ignored --nocapture
#![cfg(not(debug_assertions))] // the full run passes `--include-ignored`, and a debug build on a shared runner is not the instrument
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
    clippy::cast_precision_loss, // timings as f64 for the t statistic
)]
use mudrpc_auth::ed25519::{public_key, sign};
use mudrpc_core::seal::{open, seal};
use mudrpc_gcm::{aes, ghash};
use std::hint::black_box;
use std::time::Instant;

struct Rng(u64);
impl Rng { const fn next(&mut self) -> u64 { let mut x = self.0; x ^= x << 13; x ^= x >> 7; x ^= x << 17; self.0 = x; x } }

/// Welch's t between two samples, each cropped to its lower 90%.
fn welch(mut a: Vec<f64>, mut b: Vec<f64>) -> f64 {
    let crop = |v: &mut Vec<f64>| { v.sort_by(f64::total_cmp); v.truncate(v.len() * 9 / 10); };
    crop(&mut a);
    crop(&mut b);
    let stats = |v: &[f64]| { let m = v.iter().sum::<f64>() / v.len() as f64; (m, v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (v.len() - 1) as f64, v.len() as f64) };
    let ((ma, va, na), (mb, vb, nb)) = (stats(&a), stats(&b));
    (ma - mb) / (va / na + vb / nb).sqrt()
}

/// The median: the host's timer and scheduler throw single draws either way; a leak holds its sign.
fn median(v: &[f64]) -> f64 { let mut v = v.to_vec(); v.sort_by(f64::total_cmp); v[v.len() / 2] }

/// Two pools of secrets built before any clock runs, copies of one fixed secret and fresh ones; a round copies its
/// input out of one the same way for either class, so how a secret was written cannot show as a leak (one written byte by
/// byte and loaded whole at once stalls the load, which alone gave |t| near 5 at a million rounds).
fn pools<const N: usize>(fixed: [u8; N]) -> [Vec<[u8; N]>; 2] {
    let mut rng = Rng(7);
    [vec![fixed; 64], (0..64).map(|_| std::array::from_fn(|_| rng.next() as u8)).collect()]
}

/// `rounds` timings of `run`, each on an input `prepare` builds outside the clock for a class drawn at random, returned
/// per class in nanoseconds.
fn measure<T>(rounds: usize, mut prepare: impl FnMut(bool, u64) -> T, mut run: impl FnMut(&mut T)) -> (Vec<f64>, Vec<f64>) {
    let (mut rng, mut a, mut b) = (Rng(0x9e37_79b9_7f4a_7c15), Vec::new(), Vec::new());
    for _ in 0..rounds / 50 { run(&mut prepare(true, 1)); run(&mut prepare(false, 2)); } // warm the caches and the branch predictor on both classes
    for _ in 0..rounds {
        let (class, salt) = (rng.next() & 1 == 1, rng.next());
        let mut input = prepare(class, salt);
        let t = Instant::now();
        run(black_box(&mut input));
        (if class { &mut a } else { &mut b }).push(t.elapsed().as_nanos() as f64);
    }
    (a, b)
}

#[test]
#[ignore = "measurement: cargo test --release -p mudrpc-auth --test ct -- --ignored --nocapture"]
fn the_tag_compare_and_sign_show_no_leak_the_test_can_see() {
    // the tag compare: a wire whose tag is wrong in its first byte against one wrong in its last; both are refused, and
    // a compare that stopped at the first difference would tell them apart
    let (key, nonce, ad, plain) = ([7u8; 32], [1u8; 12], b"\x01nav", [5u8; 100]);
    let mut wire = [0u8; 160];
    let n = seal(&key, &nonce, ad, &plain, &mut wire).unwrap();
    let mut out = [0u8; 160];
    // each round's wire is the same copy patched with two writes, at the same address for both classes, so cache
    // placement cannot tell them apart; only `open` is inside the clock
    let patch = |c: bool, _| { let mut w = wire; w[n - 16] ^= u8::from(c); w[n - 1] ^= u8::from(!c); w };
    let tags: Vec<f64> = (0..5).map(|_| { let (a, b) = measure(200_000, patch, |w| { let _ = black_box(open(black_box(&key), ad, &w[..n], &mut out)); }); welch(a, b) }).collect();
    let t_tag = median(&tags);
    // sign: one fixed seed against a fresh seed every round, the same message; a ladder or a field element that took
    // a shortcut on some keys would tell them apart
    let (fixed, msg) = ([9u8; 32], [3u8; 64]);
    // the seed and its public key are derived outside the clock; only `sign` is inside it
    let keys = |c: bool, salt: u64| { let mut seed = fixed; if !c { for (i, x) in seed.iter_mut().enumerate() { *x = (salt >> (i % 8 * 8)) as u8 ^ i as u8; } } (seed, public_key(&seed)) };
    let signs: Vec<f64> = (0..5).map(|_| { let (a, b) = measure(20_000, keys, |(seed, pk)| { let _ = black_box(sign(seed, pk, &[&msg])); }); welch(a, b) }).collect();
    let t_sign = median(&signs);
    println!("tag compare: median t = {t_tag:+.2} of {tags:.2?} (first byte wrong vs last byte wrong, 5 × 200 000 rounds); sign: median t = {t_sign:+.2} of {signs:.2?} (fixed seed vs random seed, 5 × 20 000 rounds); |t| < 4.5 is the paper's line, and a leak keeps its sign");
    assert!(t_tag.abs() < 4.5 && t_sign.abs() < 4.5, "a timing difference the test can see: tag {t_tag:.2}, sign {t_sign:.2}");
}

#[test]
#[ignore = "measurement: cargo test --release -p mudrpc-auth --test ct -- --ignored --nocapture"]
fn the_aes_block_and_ghash_show_no_leak_the_test_can_see() {
    // the AES block, schedule and all, as the GCM sealer runs it per wire: one fixed key against a fresh key every
    // round, the same two blocks; a table or a branch on a key byte would tell them apart
    let (keys, hs) = (pools([9u8; 32]), pools([9u8; 16]));
    let blocks = |k: &mut [u8; 32]| { let mut b = [[3u8; 16]; 2]; aes::encrypt(&aes::schedule(k), &mut b); black_box(b); };
    let aes: Vec<f64> = (0..5).map(|_| { let (a, b) = measure(100_000, |c, salt| keys[usize::from(!c)][salt as usize % 64], blocks); welch(a, b) }).collect();
    let t_aes = median(&aes);
    // GHASH: one fixed H against a fresh H every round, over the same 13 bytes of ad and 64 of ciphertext
    let hash = |h: &mut [u8; 16]| { black_box(ghash::ghash(h, &[1; 13], &[2; 64])); };
    let ghs: Vec<f64> = (0..5).map(|_| { let (a, b) = measure(100_000, |c, salt| hs[usize::from(!c)][salt as usize % 64], hash); welch(a, b) }).collect();
    let t_gh = median(&ghs);
    println!("AES-256 block: median t = {t_aes:+.2} of {aes:.2?} (fixed key vs random key, 5 × 100 000 rounds); GHASH: median t = {t_gh:+.2} of {ghs:.2?} (fixed H vs random H, 5 × 100 000 rounds); |t| < 4.5 is the paper's line");
    assert!(t_aes.abs() < 4.5 && t_gh.abs() < 4.5, "a timing difference the test can see: AES {t_aes:.2}, GHASH {t_gh:.2}");
}
