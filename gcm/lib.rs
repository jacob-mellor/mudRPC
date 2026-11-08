//! AES-256-GCM from FIPS 197 and SP 800-38D (SPEC.md layer 1, "AES-256-GCM behind the same seam"). The same rules as core:
//! `no_std`, no allocation, no crates, no unsafe; every index checked by construction, the tag compared by a fold.
//! `seal` and `open` are public for the NIST and Wycheproof vectors; a product seals through `Gcm`, the `Sealer`.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::many_single_char_names, // the letters are the standards' and the paper's; a reader checks the code against them
    clippy::missing_errors_doc, // each function's doc says what its errors mean in prose
    clippy::cast_possible_truncation, // a `try_from` would add a panic path the tripwire forbids; every narrowing cast here is of a value masked or bounded on its line
)]
pub mod aes;
pub mod ghash;

use aes::Schedule;
use mudrpc_core::seal::{Cipher, Error, Slots, NONCE, TAG};

/// The counter block: the 12-byte IV, then a 32-bit big-endian counter (SP 800-38D §7.1, J₀ when it is 1).
fn counter(nonce: &[u8; 12], i: u32) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..12].copy_from_slice(nonce);
    b[12..].copy_from_slice(&i.to_be_bytes());
    b
}

/// GCTR from inc32(J₀): two counter blocks per call to the cipher.
fn ctr(sk: &Schedule, nonce: &[u8; 12], data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(32).enumerate() {
        let i = (i as u32).wrapping_mul(2).wrapping_add(2);
        let mut ks = [counter(nonce, i), counter(nonce, i.wrapping_add(1))];
        aes::encrypt(sk, &mut ks);
        for (d, k) in chunk.iter_mut().zip(ks.as_flattened()) { *d ^= k; }
        ks = [[0; 16]; 2]; // keystream XOR ciphertext is the plaintext
        core::hint::black_box(&mut ks);
    }
}

/// SP 800-38D §7.1: GHASH under H = E(K, 0¹²⁸), masked with E(K, J₀); both blocks in one call.
fn tag(sk: &Schedule, nonce: &[u8; 12], ad: &[u8], ct: &[u8]) -> [u8; 16] {
    let mut b = [[0; 16], counter(nonce, 1)];
    aes::encrypt(sk, &mut b);
    let mut t = ghash::ghash(&b[0], ad, ct);
    for (t, e) in t.iter_mut().zip(b[1]) { *t ^= e; }
    b = [[0; 16]; 2]; // H forges tags
    core::hint::black_box(&mut b);
    t
}

/// Writes nonce ‖ ciphertext ‖ tag.
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], ad: &[u8], plain: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    seal_with(&aes::schedule(key), nonce, ad, plain, out)
}

fn seal_with(sk: &Schedule, nonce: &[u8; 12], ad: &[u8], plain: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    if plain.len() / 16 >= (u32::MAX - 1) as usize { return Err(Error::Full); } // SP 800-38D §5.2.1.1: counters 2..2³² − 1; unreachable through the peer
    let n = plain.len().checked_add(NONCE + TAG).ok_or(Error::Full)?;
    let Some(out) = out.get_mut(..n) else { return Err(Error::Full) };
    let (head, rest) = out.split_at_mut(NONCE);
    let (ct, t) = rest.split_at_mut(plain.len());
    head.copy_from_slice(nonce);
    ct.copy_from_slice(plain);
    ctr(sk, nonce, ct);
    t.copy_from_slice(&tag(sk, nonce, ad, ct));
    Ok(n)
}

/// Size is checked before the tag, as core's `open` checks it.
pub fn open(key: &[u8; 32], ad: &[u8], wire: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let Some((nonce, rest)) = wire.split_at_checked(NONCE).filter(|(_, r)| r.len() >= TAG) else { return Err(Error::Short) };
    let mut n = [0u8; 12];
    n.copy_from_slice(nonce);
    open_with(&aes::schedule(key), &n, ad, rest, out)
}

/// The nonce apart from the wire: `rest` is ciphertext ‖ tag.
fn open_with(sk: &Schedule, nonce: &[u8; 12], ad: &[u8], rest: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let Some(ct_len) = rest.len().checked_sub(TAG) else { return Err(Error::Short) };
    let (ct, t) = rest.split_at(ct_len);
    let Some(out) = out.get_mut(..ct_len) else { return Err(Error::Full) };
    let diff = tag(sk, nonce, ad, ct).iter().zip(t).fold(0, |acc, (a, b)| acc | (a ^ b)); // no early return, and opaque to the optimiser
    if core::hint::black_box(diff) != 0 { return Err(Error::Tag); }
    out.copy_from_slice(ct);
    ctr(sk, nonce, out);
    Ok(ct_len)
}

/// The header key, derived as QUIC derives its own: the two blocks `ff…ff ‖ 0` and `fe ff…ff ‖ 0` under the slot's key,
/// which GCM never encrypts (its blocks are 0 for H and IV ‖ counter from 1), so no mask and no checksum is one of GCM's.
fn header_key(sk: &Schedule) -> Schedule {
    let mut b = [counter(&[0xff; 12], 0), counter(&[0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], 0)];
    aes::encrypt(sk, &mut b);
    let mut k = [0u8; 32];
    for (d, s) in k.iter_mut().zip(b.as_flattened()) { *d = *s; }
    let hk = aes::schedule(&k);
    (b, k) = ([[0; 16]; 2], [0; 32]);
    core::hint::black_box((&mut b, &mut k));
    hk
}

/// The header mask, as QUIC protects its headers under AES: the first twelve bytes of AES-ECB, under the header key, of
/// the sixteen bytes after the header, which the receiver has before it needs the mask.
fn mask(hk: &Schedule, sample: &[u8]) -> [u8; 12] {
    let mut b = [[0u8; 16]; 2];
    for (d, s) in b[0].iter_mut().zip(sample) { *d = *s; }
    aes::encrypt(hk, &mut b);
    let mut m = [0u8; 12];
    m.copy_from_slice(&b[0][..12]);
    m
}

/// A key's checksum for `intact`: the mask of a zero sample. It never reaches the wire.
#[must_use]
pub fn check(key: &[u8; 32]) -> [u8; 12] { mask(&header_key(&aes::schedule(key)), &[]) }

/// FIPS 197 and SP 800-38D under the slots: `Gcm`.
pub enum Aes256Gcm {}

impl Cipher for Aes256Gcm {
    const VERSION: u8 = 3;
    fn check(key: &[u8; 32]) -> [u8; 12] { check(key) }
    fn seal(key: &[u8; 32], nonce: &[u8; 12], ad: &[u8], plain: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        let sk = aes::schedule(key);
        let n = seal_with(&sk, nonce, ad, plain, out)?;
        let m = mask(&header_key(&sk), out.get(NONCE..).unwrap_or(&[]));
        for (h, m) in out.iter_mut().zip(m) { *h ^= m; }
        Ok(n)
    }
    fn open(key: &[u8; 32], ad: &[u8], header: &[u8], rest: &[u8], out: &mut [u8]) -> Option<(usize, [u8; 12])> {
        let sk = aes::schedule(key);
        let mut nonce = [0u8; 12];
        for (n, (h, m)) in nonce.iter_mut().zip(header.iter().zip(mask(&header_key(&sk), rest))) { *n = h ^ m; }
        open_with(&sk, &nonce, ad, rest, out).ok().map(|len| (len, nonce))
    }
}

/// AES-256-GCM as the fleet's sealer, version 3 on the wire: `Key`'s three slots and its rotation, `Key`'s 136 bytes.
pub type Gcm = Slots<Aes256Gcm>;
