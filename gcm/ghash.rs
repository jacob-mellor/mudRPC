//! GHASH from SP 800-38D §6.3 and §6.4: the multiply in GF(2¹²⁸) bit by bit under masks, no table, no branch on a secret.

use core::hint::black_box;

const R: u128 = 0xe1 << 120;

/// SP 800-38D algorithm 1, with the bits of `x` taken from the top and every choice a mask.
///
/// `black_box` keeps each mask a mask: inlined into `ghash`, LLVM made them branches on Thumb-1, which has no conditional move.
#[must_use]
pub fn mul(mut x: u128, h: u128) -> u128 {
    let (mut z, mut v) = (0, h);
    for _ in 0..128 {
        z ^= v & black_box((x >> 127).wrapping_neg());
        v = (v >> 1) ^ (R & black_box((v & 1).wrapping_neg()));
        x <<= 1;
    }
    z
}

/// GHASH under `h` of `ad ‖ pad ‖ ct ‖ pad ‖ len(ad) ‖ len(ct)`, the lengths in bits, as §7.1 hashes them.
#[must_use]
pub fn ghash(h: &[u8; 16], ad: &[u8], ct: &[u8]) -> [u8; 16] {
    let h = u128::from_be_bytes(*h);
    let mut y = 0;
    for c in ad.chunks(16).chain(ct.chunks(16)) {
        let mut b = [0u8; 16];
        for (d, s) in b.iter_mut().zip(c) { *d = *s; }
        y = mul(y ^ u128::from_be_bytes(b), h);
    }
    mul(y ^ (u128::from(ad.len() as u64) << 67 | u128::from(ct.len() as u64) << 3), h).to_be_bytes()
}
