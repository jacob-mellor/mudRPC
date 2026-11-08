//! ChaCha20-Poly1305 from RFC 8439 and the `Sealer` a peer needs: a header masked per wire, three key slots, no randomness.
//!
//! Poly1305 limb arithmetic follows poly1305-donna (Andrew Moon, public domain). Constants are not touched for style.
//! Every index is checked by construction; the tag compare is a fold the optimiser cannot shorten.
//! `block`, `poly1305`, `seal`, `open` and `check` are public for the RFC and Wycheproof vectors and the probes that
//! hunt for keys; a nonce is the peer's to make, so a product seals through a `Sealer`, never through these.
use crate::codec::le;
use core::marker::PhantomData;

/// Bytes in a key.
pub const KEY: usize = 32;
/// Bytes in a nonce, and in the masked header that carries it.
pub const NONCE: usize = 12;
/// Bytes in a tag.
pub const TAG: usize = 16;

/// Short, Tag and Full are `open`'s; Sig, Roster and Budget are `verify`'s: a signature that fails, a sender with no roster
/// line, a receiver out of verifications this second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub enum Error { Short, Tag, Full, Sig, Roster, Budget }

// RFC 8439 §2.1: the quarter round.
const fn qr(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]); s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]); s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]); s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]); s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// RFC 8439 §2.3: one 64-byte keystream block.
#[must_use]
pub fn block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut s = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574, 0, 0, 0, 0, 0, 0, 0, 0, counter, 0, 0, 0]; // "expand 32-byte k"
    for (w, c) in s[4..12].iter_mut().zip(key.chunks_exact(4)) { *w = le(c) as u32; }
    for (w, c) in s[13..].iter_mut().zip(nonce.chunks_exact(4)) { *w = le(c) as u32; }
    let mut init = s;
    for _ in 0..10 {
        qr(&mut s, 0, 4, 8, 12); qr(&mut s, 1, 5, 9, 13); qr(&mut s, 2, 6, 10, 14); qr(&mut s, 3, 7, 11, 15);
        qr(&mut s, 0, 5, 10, 15); qr(&mut s, 1, 6, 11, 12); qr(&mut s, 2, 7, 8, 13); qr(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for ((o, w), i) in out.chunks_exact_mut(4).zip(s).zip(init) { o.copy_from_slice(&w.wrapping_add(i).to_le_bytes()); }
    (s, init) = ([0; 16], [0; 16]); // both hold the key
    core::hint::black_box((&mut s, &mut init));
    out
}

/// RFC 8439 §2.4: encryption is XOR with the keystream from counter 1.
fn xor_stream(key: &[u8; 32], nonce: &[u8; 12], data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(64).enumerate() {
        let mut ks = block(key, 1 + i as u32, nonce);
        for (d, k) in chunk.iter_mut().zip(&ks) { *d ^= k; }
        ks = [0; 64]; // keystream XOR ciphertext is the plaintext
        core::hint::black_box(&mut ks);
    }
}

const M44: u64 = (1 << 44) - 1;
const M42: u64 = (1 << 42) - 1;

/// RFC 8439 §2.5, streaming, with three 44/44/42-bit limbs (donna-64).
struct Poly { r: [u64; 3], s: [u64; 2], pad: [u64; 2], h: [u64; 3], buf: [u8; 16], n: usize }

impl Poly {
    fn new(key: &[u8; 32]) -> Self {
        let (t0, t1) = (le(&key[0..8]), le(&key[8..16]));
        let r = [t0 & 0xffc_0fff_ffff, ((t0 >> 44) | (t1 << 20)) & 0xfff_ffc0_ffff, (t1 >> 24) & 0x000f_ffff_fc0f]; // clamp, §2.5.1
        Self { r, s: [r[1] * 20, r[2] * 20], pad: [le(&key[16..24]), le(&key[24..32])], h: [0; 3], buf: [0; 16], n: 0 }
    }

    fn block(&mut self, m: &[u8; 16], hibit: u64) {
        let (t0, t1) = (le(&m[0..8]), le(&m[8..16]));
        let ([r0, r1, r2], [s1, s2]) = (self.r, self.s);
        let h0 = u128::from(self.h[0] + (t0 & M44));
        let h1 = u128::from(self.h[1] + (((t0 >> 44) | (t1 << 20)) & M44));
        let h2 = u128::from(self.h[2] + (((t1 >> 24) & M42) | hibit));
        let d0 = h0 * u128::from(r0) + h1 * u128::from(s2) + h2 * u128::from(s1);
        let d1 = h0 * u128::from(r1) + h1 * u128::from(r0) + h2 * u128::from(s2) + (d0 >> 44);
        let d2 = h0 * u128::from(r2) + h1 * u128::from(r1) + h2 * u128::from(r0) + (d1 >> 44);
        let h0 = (d0 as u64 & M44) + (d2 >> 42) as u64 * 5;
        self.h = [h0 & M44, (d1 as u64 & M44) + (h0 >> 44), d2 as u64 & M42];
    }

    fn update(&mut self, mut m: &[u8]) {
        while !m.is_empty() {
            let take = (16 - self.n).min(m.len());
            for (d, s) in self.buf.iter_mut().skip(self.n).zip(m) { *d = *s; }
            self.n += take;
            m = m.get(take..).unwrap_or(&[]);
            if self.n == 16 { let b = self.buf; self.block(&b, 1 << 40); self.n = 0; }
        }
    }

    fn finish(&mut self) -> [u8; 16] {
        if self.n > 0 {
            let mut b = [0u8; 16];
            for (d, s) in b.iter_mut().zip(self.buf.iter().take(self.n)) { *d = *s; }
            if let Some(pad) = b.get_mut(self.n) { *pad = 1; }
            self.block(&b, 0);
        }
        let [mut h0, mut h1, mut h2] = self.h;
        for _ in 0..2 {
            h1 += h0 >> 44; h0 &= M44;
            h2 += h1 >> 44; h1 &= M44;
            h0 += (h2 >> 42) * 5; h2 &= M42;
        }
        // h - p, kept only when h >= p, selected without a branch
        let g0 = h0 + 5;
        let g1 = h1 + (g0 >> 44);
        let g2 = h2.wrapping_add(g1 >> 44).wrapping_sub(1 << 42);
        let keep = (g2 >> 63).wrapping_sub(1);
        let (h0, h1, h2) = ((h0 & !keep) | (g0 & M44 & keep), (h1 & !keep) | (g1 & M44 & keep), (h2 & !keep) | (g2 & keep));
        let [p0, p1] = self.pad;
        let h0 = h0 + (p0 & M44);
        let h1 = h1 + (((p0 >> 44) | (p1 << 20)) & M44) + (h0 >> 44);
        let h2 = (h2 + ((p1 >> 24) & M42) + (h1 >> 44)) & M42;
        let (h0, h1) = (h0 & M44, h1 & M44);
        let mut tag = [0u8; 16];
        tag[..8].copy_from_slice(&(h0 | (h1 << 44)).to_le_bytes());
        tag[8..].copy_from_slice(&((h1 >> 20) | (h2 << 24)).to_le_bytes());
        (self.r, self.s, self.pad, self.h) = ([0; 3], [0; 2], [0; 2], [0; 3]); // r forges tags
        core::hint::black_box(self);
        tag
    }
}

/// RFC 8439 §2.5: the one-time authenticator of `msg` under `key`.
#[must_use]
pub fn poly1305(key: &[u8; 32], msg: &[u8]) -> [u8; 16] {
    let mut p = Poly::new(key);
    p.update(msg);
    p.finish()
}

/// RFC 8439 §2.8: the tag over ad ‖ pad16 ‖ ciphertext ‖ pad16 ‖ len(ad) ‖ len(ciphertext), under the block-0 key.
fn tag(key: &[u8; 32], nonce: &[u8; 12], ad: &[u8], ct: &[u8]) -> [u8; 16] {
    let mut otk = [0u8; 32];
    otk.copy_from_slice(&block(key, 0, nonce)[..32]);
    let mut p = Poly::new(&otk);
    otk = [0; 32]; // `Poly` holds its own copy, which `finish` zeroes
    core::hint::black_box(&mut otk);
    let zero = [0u8; 16];
    for part in [ad, ct] { p.update(part); p.update(&zero[..(16 - part.len() % 16) % 16]); }
    p.update(&(ad.len() as u64).to_le_bytes());
    p.update(&(ct.len() as u64).to_le_bytes());
    p.finish()
}

/// Writes nonce ‖ ciphertext ‖ tag.
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], ad: &[u8], plain: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    if plain.len() / 64 >= (u32::MAX - 1) as usize { return Err(Error::Full); } // RFC 8439 §2.4: counters 1..2³² − 2, since 2³² − 1 is the header mask's; unreachable through the peer, stated here because `seal` is public
    let n = plain.len().checked_add(NONCE + TAG).ok_or(Error::Full)?;
    let Some(out) = out.get_mut(..n) else { return Err(Error::Full) };
    let (head, rest) = out.split_at_mut(NONCE);
    let (ct, t) = rest.split_at_mut(plain.len());
    head.copy_from_slice(nonce);
    ct.copy_from_slice(plain);
    xor_stream(key, nonce, ct);
    t.copy_from_slice(&tag(key, nonce, ad, ct));
    Ok(n)
}

/// Size is checked before the tag: a length compare leaks nothing and saves a MAC over a flood.
pub fn open(key: &[u8; 32], ad: &[u8], wire: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let Some((nonce, rest)) = wire.split_at_checked(NONCE).filter(|(_, r)| r.len() >= TAG) else { return Err(Error::Short) };
    let mut n = [0u8; 12];
    n.copy_from_slice(nonce);
    open_with(key, &n, ad, rest, out)
}

/// The nonce apart from the wire: `rest` is ciphertext ‖ tag.
fn open_with(key: &[u8; 32], nonce: &[u8; 12], ad: &[u8], rest: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let Some(ct_len) = rest.len().checked_sub(TAG) else { return Err(Error::Short) };
    let (ct, t) = rest.split_at(ct_len);
    let Some(out) = out.get_mut(..ct_len) else { return Err(Error::Full) };
    let diff = tag(key, nonce, ad, ct).iter().zip(t).fold(0, |acc, (a, b)| acc | (a ^ b)); // no early return, and opaque to the optimiser
    if core::hint::black_box(diff) != 0 { return Err(Error::Tag); }
    out.copy_from_slice(ct);
    xor_stream(key, nonce, out);
    Ok(ct_len)
}

/// The header mask for one wire, as QUIC protects its headers: from the key and the twelve bytes after the header, which
/// the receiver has before it needs the mask, under the one counter the AEAD never reaches. A constant per key would be
/// a pad reused over a nonce whose high bytes are zero in every real fleet.
fn mask(key: &[u8; 32], sample: &[u8]) -> [u8; 12] {
    let (mut s, mut m) = ([0u8; 12], [0u8; 12]);
    for (d, b) in s.iter_mut().zip(sample) { *d = *b; }
    let mut b = block(key, u32::MAX, &s);
    m.copy_from_slice(&b[..12]);
    b = [0; 64];
    core::hint::black_box(&mut b);
    m
}

/// A key's checksum for `intact`: the mask of a zero sample. It never reaches the wire.
#[must_use]
pub fn check(key: &[u8; 32]) -> [u8; 12] { mask(key, &[0; 12]) }

/// Which key slot opened a wire. A host rotating after a compromise retires `Previous` early on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub enum Slot { Current, Previous, Next }

/// The slots in the order `open` tries them.
pub const SLOTS: [Slot; 3] = [Slot::Current, Slot::Previous, Slot::Next];

/// What `open` found: the plaintext length, the nonce under the mask, the slot whose key opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub struct Opened { pub len: usize, pub nonce: [u8; 12], pub slot: Slot }

/// What a peer needs from its cipher.
///
/// Core never sees a key and never produces a nonce at random: the peer hands the sealer `sender ‖ epoch ‖ counter`. A
/// sealer may key by topic or sender from the associated data; for one it does not know it refuses, never defaults. It
/// lives inside the peer's saved block, so it is plain data (`repr(C)`, no pointers). Nothing here has a default: a
/// sealer states its version, its trailer, its roster, and what it does on verify and resume.
pub trait Sealer {
    /// The wire's version byte: 1 is the unsigned wire. A sealer that changes the wire's shape names its own, so a mixed
    /// fleet is two `Version(n)` counts and never a downgrade. Taken: 1 `Key`, 2 `Signed<Key>`, 3 `Gcm`, 4 `Signed<Gcm>`.
    const VERSION: u8;
    /// Bytes at the end of the plaintext that are the sealer's, not the envelope's: a signature. 0 for `Key`.
    const TRAILER: usize;
    /// Senders this sealer believes, which the peer keeps a window for each of: 0 for one that believes every key holder.
    const ROSTER: usize;
    /// Fills `plain[n..n + TRAILER]` in place, then writes header ‖ ciphertext of `plain[..n + TRAILER]` ‖ tag, where
    /// the header is the nonce under this sealer's masking; `plain[..n]` is the message, and a `plain` with no room is `Full`.
    fn seal(&mut self, nonce: &[u8; 12], ad: &[u8], plain: &mut [u8], n: usize, out: &mut [u8]) -> Result<usize, Error>;
    /// The tag check and nothing more: the plaintext it yields is not yet trusted.
    fn open(&mut self, ad: &[u8], wire: &[u8], out: &mut [u8]) -> Result<Opened, Error>;
    /// After `open` and every verdict the peer reaches from public facts, before anything is marked: the expensive check,
    /// if the sealer has one. On success the plaintext is unchanged, since the peer decodes the product's fields from it
    /// again; on failure it is zeroed, so a caller that reads on error finds nothing.
    fn verify(&mut self, now: u32, ad: &[u8], nonce: &[u8; 12], plain: &mut [u8]) -> Result<(), Error>;
    /// Every byte of a restored sealer accounted for; a peer refuses to resume on one that is not.
    fn intact(&self) -> bool;
    /// Uptime restarted: whatever the sealer counts per second starts over, as the peer's own counts do.
    fn resume(&mut self);
}

/// A cipher under the three key slots: one wire sealed or opened under one key, its header masked, and the key's checksum.
pub trait Cipher {
    /// `Sealer::VERSION` for its slots.
    const VERSION: u8;
    /// A key's checksum for `intact`. It never reaches the wire.
    fn check(key: &[u8; 32]) -> [u8; 12];
    /// Writes header ‖ ciphertext ‖ tag, the header the nonce under this cipher's mask.
    fn seal(key: &[u8; 32], nonce: &[u8; 12], ad: &[u8], plain: &[u8], out: &mut [u8]) -> Result<usize, Error>;
    /// The plaintext's length and the unmasked nonce, if `key` opens header ‖ `rest`.
    fn open(key: &[u8; 32], ad: &[u8], header: &[u8], rest: &[u8], out: &mut [u8]) -> Option<(usize, [u8; 12])>;
}

/// RFC 8439 under the slots: `Key`.
pub enum ChaCha20Poly1305 {}

impl Cipher for ChaCha20Poly1305 {
    const VERSION: u8 = 1;
    fn check(key: &[u8; 32]) -> [u8; 12] { check(key) }
    fn seal(key: &[u8; 32], nonce: &[u8; 12], ad: &[u8], plain: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        let n = seal(key, nonce, ad, plain, out)?;
        let m = mask(key, out.get(NONCE..).unwrap_or(&[]));
        for (h, m) in out.iter_mut().zip(m) { *h ^= m; }
        Ok(n)
    }
    fn open(key: &[u8; 32], ad: &[u8], header: &[u8], rest: &[u8], out: &mut [u8]) -> Option<(usize, [u8; 12])> {
        let mut nonce = [0u8; 12];
        for (n, (h, m)) in nonce.iter_mut().zip(header.iter().zip(mask(key, rest))) { *n = h ^ m; }
        open_with(key, &nonce, ad, rest, out).ok().map(|len| (len, nonce))
    }
}

/// One fleet key with a previous and a next for a flag day that is hours wide on a mesh, under either cipher.
#[repr(C)]
pub struct Slots<C: Cipher> { keys: [[u8; 32]; 3], checks: [[u8; 12]; 3], present: [u8; 3], _pad: [u8; 1], cipher: PhantomData<C> }
/// The shipped Sealer: the slots under ChaCha20-Poly1305, version 1 on the wire.
pub type Key = Slots<ChaCha20Poly1305>;
const _: () = assert!(size_of::<Key>() == 96 + 36 + 3 + 1); // a multiple of 8, as a sealer in a peer's block is

impl<C: Cipher> Slots<C> {
    /// The fleet key in the current slot; the other two are empty.
    #[must_use]
    #[cold] // once a boot, as `stage` once a flag day; inlined, Cortex-M0's text grows 9 KB
    pub fn new(key: [u8; 32]) -> Self {
        let mut k = Self { keys: [[0; 32]; 3], checks: [[0; 12]; 3], present: [0; 3], _pad: [0], cipher: PhantomData };
        k.set(0, key);
        k
    }
    /// The key the fleet will switch to; opens `Next` wires from units that switched first.
    #[cold]
    pub fn stage(&mut self, next: [u8; 32]) { self.set(2, next); }
    /// Previous ← current ← next. Nothing happens without a staged key. The epoch is not touched.
    pub const fn rotate(&mut self) {
        if self.present[2] == 0 { return; }
        (self.keys[1], self.checks[1], self.present[1]) = (self.keys[0], self.checks[0], 1);
        (self.keys[0], self.checks[0], self.present[0]) = (self.keys[2], self.checks[2], 1);
        self.clear(2);
    }
    /// Drops the previous key: the grace period is over, late wires under it are refused from now on.
    pub const fn retire(&mut self) { self.clear(1); }
    fn set(&mut self, i: usize, key: [u8; 32]) { self.keys[i] = key; self.checks[i] = C::check(&key); self.present[i] = 1; }
    /// Zeroes a slot: a vacated key does not outlive it. `black_box` keeps the stores; core has no unsafe for a volatile write.
    const fn clear(&mut self, i: usize) {
        (self.keys[i], self.checks[i], self.present[i]) = ([0; 32], [0; 12], 0);
        core::hint::black_box(self);
    }
}

impl<C: Cipher> Drop for Slots<C> {
    fn drop(&mut self) { for i in 0..3 { self.clear(i); } }
}

impl<C: Cipher> Sealer for Slots<C> {
    const VERSION: u8 = C::VERSION;
    const TRAILER: usize = 0;
    const ROSTER: usize = 0;

    fn seal(&mut self, nonce: &[u8; 12], ad: &[u8], plain: &mut [u8], n: usize, out: &mut [u8]) -> Result<usize, Error> {
        C::seal(&self.keys[0], nonce, ad, plain.get(..n).ok_or(Error::Full)?, out)
    }

    fn open(&mut self, ad: &[u8], wire: &[u8], out: &mut [u8]) -> Result<Opened, Error> {
        let Some((header, rest)) = wire.split_at_checked(NONCE).filter(|(_, r)| r.len() >= TAG) else { return Err(Error::Short) };
        if out.len() < rest.len() - TAG { return Err(Error::Full); }
        for ((slot, key), _) in SLOTS.into_iter().zip(&self.keys).zip(self.present).filter(|&(_, present)| present == 1) {
            if let Some((len, nonce)) = C::open(key, ad, header, rest, out) { return Ok(Opened { len, nonce, slot }); }
        }
        Err(Error::Tag)
    }

    fn verify(&mut self, _: u32, _: &[u8], _: &[u8; 12], _: &mut [u8]) -> Result<(), Error> { Ok(()) }

    /// A present slot carries the key's checksum; an absent slot is zero.
    fn intact(&self) -> bool {
        self.present[0] == 1 && (0..3).all(|i| match self.present[i] { 1 => self.checks[i] == C::check(&self.keys[i]), 0 => self.keys[i] == [0; 32] && self.checks[i] == [0; 12], _ => false })
    }

    fn resume(&mut self) {}
}
