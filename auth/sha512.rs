//! SHA-512 from FIPS 180-4, for Ed25519 (RFC 8032 §5.1): one 128-byte block at a time, streaming or in one call.
//!
//! The constants are the first 64 bits of the fractional parts of the square roots of the first eight primes (§5.3.5)
//! and the cube roots of the first eighty (§4.2.3), generated from that definition, not typed.

const H0: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
    0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];

const K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

/// A streaming digest: `update` any number of times, then `finish`.
pub struct Sha512 { h: [u64; 8], buf: [u8; 128], len: usize, bits: u128 }

impl Default for Sha512 {
    fn default() -> Self { Self { h: H0, buf: [0; 128], len: 0, bits: 0 } }
}

impl Sha512 {
    /// The initial state (§5.3.5).
    #[must_use] pub fn new() -> Self { Self::default() }

    /// Absorbs `data`, any length, compressing each full block as it completes.
    pub fn update(&mut self, mut data: &[u8]) {
        self.bits += data.len() as u128 * 8;
        if self.len > 0 {
            let take = (128 - self.len).min(data.len());
            for (d, s) in self.buf.iter_mut().skip(self.len).zip(data) { *d = *s; }
            (self.len, data) = (self.len + take, data.get(take..).unwrap_or(&[]));
            if self.len < 128 { return; }
            compress(&mut self.h, &self.buf);
            self.len = 0;
        }
        let mut blocks = data.chunks_exact(128);
        for b in &mut blocks { compress(&mut self.h, b); }
        let rest = blocks.remainder();
        for (d, s) in self.buf.iter_mut().zip(rest) { *d = *s; }
        self.len = rest.len();
    }

    /// Padding (§5.1.2): a 1 bit, zeros to 112 mod 128, the length in bits as 128 bits big-endian.
    #[must_use]
    pub fn finish(mut self) -> [u8; 64] {
        let bits = self.bits;
        self.update(&[0x80]);
        self.update(&[0; 128][..(240 - self.len) % 128]);
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 64];
        for (o, h) in out.chunks_exact_mut(8).zip(self.h) { for (d, s) in o.iter_mut().zip(h.to_be_bytes()) { *d = s; } }
        (self.buf, self.h) = ([0; 128], H0); // the last block may be a secret's: a seed, a prefix
        core::hint::black_box(&self);
        out
    }
}

/// The digest of a concatenation, which is every hash Ed25519 takes.
#[must_use]
pub fn sha512(parts: &[&[u8]]) -> [u8; 64] {
    let mut s = Sha512::new();
    for p in parts { s.update(p); }
    s.finish()
}

/// §6.4.2: the message schedule, then eighty rounds.
fn compress(h: &mut [u64; 8], block: &[u8]) {
    let mut w = [0u64; 80];
    for (w, c) in w.iter_mut().zip(block.chunks_exact(8)) { *w = c.iter().fold(0, |acc, &x| acc << 8 | u64::from(x)); }
    for t in 16..80 {
        let s0 = w[t - 15].rotate_right(1) ^ w[t - 15].rotate_right(8) ^ (w[t - 15] >> 7);
        let s1 = w[t - 2].rotate_right(19) ^ w[t - 2].rotate_right(61) ^ (w[t - 2] >> 6);
        w[t] = w[t - 16].wrapping_add(s0).wrapping_add(w[t - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for t in 0..80 {
        let t1 = hh.wrapping_add(e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41)).wrapping_add((e & f) ^ (!e & g)).wrapping_add(K[t]).wrapping_add(w[t]);
        let t2 = (a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39)).wrapping_add((a & b) ^ (a & c) ^ (b & c));
        (hh, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
    }
    for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) { *x = x.wrapping_add(y); }
    (w, a, b, c, d, e, f, g, hh) = ([0; 80], 0, 0, 0, 0, 0, 0, 0, 0); // the schedule opens with the block, which may be a seed or a prefix
    core::hint::black_box((&w, &a, &b, &c, &d, &e, &f, &g, &hh));
}
