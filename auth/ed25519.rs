//! Ed25519 from RFC 8032 §5.1.
//!
//! The field in radix 2⁵¹ (five u64 limbs with u128 products, the layout of curve25519-dalek's `FieldElement51`,
//! credited), extended twisted Edwards coordinates with the complete formulas of Hisil, Wong, Carter and Dawson 2008, a
//! constant-time Montgomery ladder for every secret scalar, Straus's double-scalar multiplication in variable time for
//! verification's public ones, and scalars modulo L in Montgomery form. Signing is deterministic (§5.1.6); verification
//! is the single equation of §5.1.7 and refuses a non-canonical S or an undecodable point. Every byte reader is a fold
//! over a chunk or a `get`: no index.
use crate::sha512::{sha512, Sha512};

const MASK: u64 = (1 << 51) - 1;
/// The field constants, generated from their definitions in RFC 8032 §5.1: d = -121665/121666, √-1 = 2^((p-1)/4), the
/// base point (x, 4/5) with x even; the group order L and what Montgomery multiplication needs for it.
const D: Fe = Fe([0x34dca135978a3, 0x1a8283b156ebd, 0x5e7a26001c029, 0x739c663a03cbb, 0x52036cee2b6ff]);
const D2: Fe = Fe([0x69b9426b2f159, 0x35050762add7a, 0x3cf44c0038052, 0x6738cc7407977, 0x2406d9dc56dff]);
const SQRT_M1: Fe = Fe([0x61b274a0ea0b0, 0x0d5a5fc8f189d, 0x7ef5e9cbd0c60, 0x78595a6804c9e, 0x2b8324804fc1d]);
const BX: Fe = Fe([0x62d608f25d51a, 0x412a4b4f6592a, 0x75b7171a4b31d, 0x1ff60527118fe, 0x216936d3cd6e5]);
const BY: Fe = Fe([0x6666666666658, 0x4cccccccccccc, 0x1999999999999, 0x3333333333333, 0x6666666666666]);
const L: Sc = [0x5812631a5cf5d3ed, 0x14def9dea2f79cd6, 0x0000000000000000, 0x1000000000000000];
const L_INV: u64 = 0xd2b51da312547e1b;
const R1: Sc = [0xd6ec31748d98951d, 0xc6ef5bf4737dcf70, 0xfffffffffffffffe, 0x0fffffffffffffff];
const R2: Sc = [0xa40611e3449c0f01, 0xd00e1ba768859347, 0xceec73d217f5be65, 0x0399411b7c309a3d];

#[derive(Clone, Copy)]
struct Fe([u64; 5]);

impl Fe {
    const ZERO: Self = Self([0; 5]);
    const ONE: Self = Self([1, 0, 0, 0, 0]);

    /// 255 bits little-endian; the top bit is ignored, as §5.1.3 says.
    fn from_bytes(b: &[u8; 32]) -> Self {
        let w = |i: usize| b.get(i..i + 8).map_or(0, |c| c.iter().rev().fold(0, |a, &x| a << 8 | u64::from(x)));
        Self([w(0) & MASK, (w(6) >> 3) & MASK, (w(12) >> 6) & MASK, (w(19) >> 1) & MASK, (w(24) >> 12) & MASK])
    }

    /// Fully reduced: the unique encoding below p.
    fn to_bytes(self) -> [u8; 32] {
        let mut l = self.carry().0;
        let mut q = (l[0] + 19) >> 51;
        for x in &l[1..] { q = (x + q) >> 51; }
        l[0] += 19 * q;
        for i in 0..4 { l[i + 1] += l[i] >> 51; l[i] &= MASK; }
        l[4] &= MASK;
        let words = [l[0] | l[1] << 51, l[1] >> 13 | l[2] << 38, l[2] >> 26 | l[3] << 25, l[3] >> 39 | l[4] << 12];
        let mut out = [0u8; 32];
        for (o, w) in out.chunks_exact_mut(8).zip(words) { o.copy_from_slice(&w.to_le_bytes()); }
        out
    }

    fn carry(self) -> Self {
        let mut l = self.0;
        for i in 0..4 { l[i + 1] += l[i] >> 51; l[i] &= MASK; }
        l[0] += (l[4] >> 51) * 19;
        l[4] &= MASK;
        l[1] += l[0] >> 51;
        l[0] &= MASK;
        Self(l)
    }

    const fn add(self, o: Self) -> Self { Self([self.0[0] + o.0[0], self.0[1] + o.0[1], self.0[2] + o.0[2], self.0[3] + o.0[3], self.0[4] + o.0[4]]) }
    /// self + 4p - o, so the result never borrows for any `o` with limbs below 2⁵³: a sum or difference of two
    /// reduced values. Every result here feeds a `mul`, which takes limbs up to 2⁵⁵.
    const fn sub(self, o: Self) -> Self {
        let a = self.0;
        Self([a[0] + ((1 << 53) - 76) - o.0[0], a[1] + ((1 << 53) - 4) - o.0[1], a[2] + ((1 << 53) - 4) - o.0[2], a[3] + ((1 << 53) - 4) - o.0[3], a[4] + ((1 << 53) - 4) - o.0[4]])
    }

    /// Schoolbook in u128 with the 2²⁵⁵ ≡ 19 fold; inputs may carry limbs up to 2⁵⁵, the output is reduced.
    fn mul(self, o: Self) -> Self {
        let (a, b) = (self.0, o.0);
        let m = |x: u64, y: u64| u128::from(x) * u128::from(y);
        let (b1, b2, b3, b4) = (b[1] * 19, b[2] * 19, b[3] * 19, b[4] * 19);
        let mut c = [
            m(a[0], b[0]) + m(a[1], b4) + m(a[2], b3) + m(a[3], b2) + m(a[4], b1),
            m(a[0], b[1]) + m(a[1], b[0]) + m(a[2], b4) + m(a[3], b3) + m(a[4], b2),
            m(a[0], b[2]) + m(a[1], b[1]) + m(a[2], b[0]) + m(a[3], b4) + m(a[4], b3),
            m(a[0], b[3]) + m(a[1], b[2]) + m(a[2], b[1]) + m(a[3], b[0]) + m(a[4], b4),
            m(a[0], b[4]) + m(a[1], b[3]) + m(a[2], b[2]) + m(a[3], b[1]) + m(a[4], b[0]),
        ];
        let mask = u128::from(MASK);
        for i in 0..4 { c[i + 1] += c[i] >> 51; c[i] &= mask; }
        c[0] += (c[4] >> 51) * 19;
        c[4] &= mask;
        c[1] += c[0] >> 51;
        c[0] &= mask;
        Self([c[0] as u64, c[1] as u64, c[2] as u64, c[3] as u64, c[4] as u64])
    }

    /// `n` squarings.
    fn sq(mut self, n: u32) -> Self { for _ in 0..n { self = self.mul(self); } self }

    /// self^(2²⁵⁰ − 1) and self¹¹, the addition chain of ref10's `fe_invert` (Bernstein, Duif, Lange, Schwabe and Yang
    /// 2011), credited: 250 squarings and ten multiplications, each comment the exponent so far.
    fn pow250(self) -> (Self, Self) {
        let z9 = self.sq(3).mul(self);
        let z11 = z9.mul(self.sq(1));
        let a = z11.sq(1).mul(z9); // 2⁵ − 1
        let b = a.sq(5).mul(a); // 2¹⁰ − 1
        let c = b.sq(10).mul(b); // 2²⁰ − 1
        let c = c.sq(20).mul(c).sq(10).mul(b); // 2⁵⁰ − 1
        let d = c.sq(50).mul(c); // 2¹⁰⁰ − 1
        (d.sq(100).mul(d).sq(50).mul(c), z11) // 2²⁵⁰ − 1
    }

    fn is_negative(self) -> bool { self.to_bytes()[0] & 1 == 1 }
    fn eq(self, o: Self) -> bool { self.to_bytes() == o.to_bytes() }
    fn cswap(a: &mut Self, b: &mut Self, bit: u8) {
        let m = 0u64.wrapping_sub(u64::from(bit));
        for (x, y) in a.0.iter_mut().zip(b.0.iter_mut()) { let t = m & (*x ^ *y); *x ^= t; *y ^= t; }
    }
}

/// Extended coordinates (X : Y : Z : T) with x = X/Z, y = Y/Z, xy = T/Z.
#[derive(Clone, Copy)]
struct Point { x: Fe, y: Fe, z: Fe, t: Fe }

impl Point {
    const ID: Self = Self { x: Fe::ZERO, y: Fe::ONE, z: Fe::ONE, t: Fe::ZERO };
    fn base() -> Self { Self { x: BX, y: BY, z: Fe::ONE, t: BX.mul(BY) } }

    /// add-2008-hwcd-3, complete on this curve, so the ladder below has no special case.
    fn add(&self, q: &Self) -> Self {
        let a = self.y.sub(self.x).mul(q.y.sub(q.x));
        let b = self.y.add(self.x).mul(q.y.add(q.x));
        let c = self.t.mul(D2).mul(q.t);
        let d = self.z.add(self.z).mul(q.z);
        let (e, f, g, h) = (b.sub(a), d.sub(c), d.add(c), b.add(a));
        Self { x: e.mul(f), y: g.mul(h), z: f.mul(g), t: e.mul(h) }
    }

    /// dbl-2008-hwcd with a = -1.
    fn double(&self) -> Self {
        let (a, b) = (self.x.mul(self.x), self.y.mul(self.y));
        let c = self.z.mul(self.z).add(self.z.mul(self.z));
        let e = self.x.add(self.y).mul(self.x.add(self.y)).sub(a).sub(b);
        let g = b.sub(a);
        let (f, h) = (g.sub(c), Fe::ZERO.sub(a).sub(b));
        Self { x: e.mul(f), y: g.mul(h), z: f.mul(g), t: e.mul(h) }
    }

    fn cswap(a: &mut Self, b: &mut Self, bit: u8) {
        Fe::cswap(&mut a.x, &mut b.x, bit);
        Fe::cswap(&mut a.y, &mut b.y, bit);
        Fe::cswap(&mut a.z, &mut b.z, bit);
        Fe::cswap(&mut a.t, &mut b.t, bit);
    }

    /// The Montgomery ladder: one addition and one doubling per bit, whatever the bit, with swaps by mask.
    fn mul(&self, s: &[u8; 32]) -> Self {
        let (mut r0, mut r1) = (Self::ID, *self);
        for i in (0..256).rev() {
            let bit = (s[i / 8] >> (i % 8)) & 1;
            Self::cswap(&mut r0, &mut r1, bit);
            (r0, r1) = (r0.double(), r0.add(&r1));
            Self::cswap(&mut r0, &mut r1, bit);
        }
        r0
    }

    /// \[s\]B + \[k\]self for public scalars below 2²⁵³, variable time: Straus 1964, known as Shamir's trick, one
    /// doubling a bit and one addition of B, self or B + self when either bit is set.
    fn straus(&self, s: &[u8; 32], k: &[u8; 32]) -> Self {
        let (b, mut r) = (Self::base(), Self::ID);
        let bk = b.add(self);
        for i in (0..253).rev() {
            r = r.double();
            match ((s[i / 8] >> (i % 8)) & 1, (k[i / 8] >> (i % 8)) & 1) { (1, 0) => r = r.add(&b), (0, 1) => r = r.add(self), (1, 1) => r = r.add(&bk), _ => {} }
        }
        r
    }

    /// §5.1.2: y with the sign of x in the top bit.
    fn encode(&self) -> [u8; 32] {
        let (t, z11) = self.z.pow250();
        let zi = t.sq(5).mul(z11); // z^(p − 2) = 1/z
        let (x, y) = (self.x.mul(zi), self.y.mul(zi));
        let mut out = y.to_bytes();
        out[31] |= u8::from(x.is_negative()) << 7;
        out
    }

    /// §5.1.3: refuses a non-canonical y, a y with no square root, and x = 0 with the sign bit set.
    fn decode(b: &[u8; 32]) -> Option<Self> {
        let (mut yb, sign) = (*b, b[31] >> 7);
        yb[31] &= 0x7f;
        let y = Fe::from_bytes(&yb);
        if y.to_bytes() != yb { return None; }
        let y2 = y.mul(y);
        let (u, v) = (y2.sub(Fe::ONE), y2.mul(D).add(Fe::ONE));
        let v3 = v.mul(v).mul(v);
        let w = u.mul(v3).mul(v3).mul(v);
        let mut x = u.mul(v3).mul(w.pow250().0.sq(2).mul(w)); // w^((p − 5)/8)
        let vx2 = v.mul(x).mul(x);
        if !vx2.eq(u) {
            if !vx2.add(u).eq(Fe::ZERO) { return None; }
            x = x.mul(SQRT_M1);
        }
        if x.eq(Fe::ZERO) && sign == 1 { return None; }
        if u8::from(x.is_negative()) != sign { x = Fe::ZERO.sub(x); }
        Some(Self { x, y, z: Fe::ONE, t: x.mul(y) })
    }
}

/// A scalar modulo L, four little-endian u64 limbs.
type Sc = [u64; 4];

/// Montgomery product a·b·2⁻²⁵⁶ mod L (CIOS), constant time, reduced below L whenever a·b < 2²⁵⁶·L: so for any 256-bit
/// a with b < L, and for the clamped secret scalar (below 2²⁵⁵) with a reduced one.
fn mont(a: &Sc, b: &Sc) -> Sc {
    let mut t = [0u64; 6];
    for &bi in b {
        let mut c = 0u128;
        for j in 0..4 { let s = u128::from(t[j]) + u128::from(a[j]) * u128::from(bi) + c; (t[j], c) = (s as u64, s >> 64); }
        let s = u128::from(t[4]) + c;
        (t[4], t[5]) = (s as u64, (s >> 64) as u64);
        let m = t[0].wrapping_mul(L_INV);
        let mut c = (u128::from(t[0]) + u128::from(m) * u128::from(L[0])) >> 64;
        for j in 1..4 { let s = u128::from(t[j]) + u128::from(m) * u128::from(L[j]) + c; (t[j - 1], c) = (s as u64, s >> 64); }
        let s = u128::from(t[4]) + c;
        (t[3], t[4]) = (s as u64, t[5] + (s >> 64) as u64);
    }
    reduce_once([t[0], t[1], t[2], t[3]], t[4])
}

/// t + hi·2²⁵⁶ < 2L: subtract L exactly when that does not borrow, by mask.
fn reduce_once(t: Sc, hi: u64) -> Sc {
    let (mut d, mut borrow) = ([0u64; 4], 0u64);
    for i in 0..4 {
        let (x, b1) = t[i].overflowing_sub(L[i]);
        let (y, b2) = x.overflowing_sub(borrow);
        (d[i], borrow) = (y, u64::from(b1 | b2));
    }
    let keep = 0u64.wrapping_sub(hi | (borrow ^ 1));
    [d[0] & keep | t[0] & !keep, d[1] & keep | t[1] & !keep, d[2] & keep | t[2] & !keep, d[3] & keep | t[3] & !keep]
}

fn sc_add(a: &Sc, b: &Sc) -> Sc {
    let (mut s, mut carry) = ([0u64; 4], 0u64);
    for i in 0..4 {
        let (x, c1) = a[i].overflowing_add(b[i]);
        let (y, c2) = x.overflowing_add(carry);
        (s[i], carry) = (y, u64::from(c1 | c2));
    }
    reduce_once(s, carry)
}

fn sc_mul(a: &Sc, b: &Sc) -> Sc { mont(&mont(a, b), &R2) }
fn sc_from(b: &[u8; 32]) -> Sc { let mut s = [0u64; 4]; for (w, c) in s.iter_mut().zip(b.chunks_exact(8)) { *w = c.iter().rev().fold(0, |a, &x| a << 8 | u64::from(x)); } s }
fn sc_to_bytes(s: &Sc) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (o, w) in out.chunks_exact_mut(8).zip(s) { o.copy_from_slice(&w.to_le_bytes()); }
    out
}
/// A 512-bit hash modulo L: lo + hi·2²⁵⁶.
fn sc_reduce(h: &[u8; 64]) -> Sc { let (lo, hi) = halves(h); sc_add(&mont(&sc_from(lo), &R1), &mont(&sc_from(hi), &R2)) }
fn sc_is_canonical(b: &[u8; 32]) -> bool { let s = sc_from(b); (0..4).rev().find(|&i| s[i] != L[i]).is_some_and(|i| s[i] < L[i]) }
/// Two 32-byte halves of a 64-byte array, with no index to check.
fn halves(b: &[u8; 64]) -> (&[u8; 32], &[u8; 32]) { let (lo, hi) = b.split_at(32); (lo.try_into().unwrap_or(&[0; 32]), hi.try_into().unwrap_or(&[0; 32])) }

/// §5.1.5: the secret scalar from the seed, clamped, and the prefix for the deterministic nonce.
fn expand(seed: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let mut h = sha512(&[seed]);
    let (mut a, mut prefix) = ([0u8; 32], [0u8; 32]);
    a.copy_from_slice(&h[..32]);
    prefix.copy_from_slice(&h[32..]);
    a[0] &= 248;
    a[31] = (a[31] & 63) | 64;
    h = [0; 64];
    core::hint::black_box(&h);
    (a, prefix)
}

/// §5.1.5: the public key of a seed, A = \[a\]B.
#[must_use]
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    let (mut a, mut prefix) = expand(seed);
    let pk = Point::base().mul(&a).encode();
    a.fill(0);
    prefix.fill(0); // the prefix recovers the scalar from one signature
    core::hint::black_box((&a, &prefix));
    pk
}

/// SHA-512 over a few fixed parts and then the message's parts, streamed: no array of slices to index.
fn hash(head: &[&[u8]], msg: &[&[u8]]) -> [u8; 64] {
    let mut s = Sha512::new();
    for p in head.iter().chain(msg) { s.update(p); }
    s.finish()
}

/// §5.1.6, deterministic: R = \[r\]B with r = H(prefix ‖ M), S = r + H(R ‖ A ‖ M)·a mod L.
///
/// `msg` is a concatenation; `pk` is the seed's public key, which the caller holds, so signing is one scalar
/// multiplication, not two. It must be the seed's own: two signatures of one message under two keys share the nonce and
/// give away the scalar.
#[must_use]
pub fn sign(seed: &[u8; 32], pk: &[u8; 32], msg: &[&[u8]]) -> [u8; 64] {
    let (mut a, mut prefix) = expand(seed);
    let mut hr = hash(&[&prefix], msg);
    let mut r = sc_reduce(&hr);
    let mut rs = sc_to_bytes(&r);
    let rb = Point::base().mul(&rs).encode();
    let k = sc_reduce(&hash(&[&rb, pk], msg));
    let s = sc_add(&r, &sc_mul(&k, &sc_from(&a)));
    let mut sig = [0u8; 64];
    for (d, b) in sig.iter_mut().zip(rb.iter().chain(&sc_to_bytes(&s))) { *d = *b; }
    (a, prefix, hr, r, rs) = ([0; 32], [0; 32], [0; 64], [0; 4], [0; 32]); // the secret scalar, the prefix and the nonce scalar die here
    core::hint::black_box((&a, &prefix, &hr, &r, &rs));
    sig
}

/// §5.1.7: \[S\]B = R + \[k\]A, with S canonical and both points decodable; anything else is false.
///
/// Every input is public, so variable time: \[S\]B − \[k\]A in one pass, encoded and compared with R's bytes, which
/// only a decodable R can equal.
#[must_use]
pub fn verify(pk: &[u8; 32], msg: &[&[u8]], sig: &[u8; 64]) -> bool {
    let (rb, s) = halves(sig);
    let Some(a) = Point::decode(pk) else { return false };
    if !sc_is_canonical(s) { return false; }
    let k = sc_to_bytes(&sc_reduce(&hash(&[rb, pk], msg)));
    Point { x: Fe::ZERO.sub(a.x), t: Fe::ZERO.sub(a.t), ..a }.straus(s, &k).encode() == *rb
}

/// A key that would make its roster line forgeable by anyone: not a point, the identity, or of small order (§5.1.7's
/// equation holds for S = 0 against such an A). Refused at roster load.
#[must_use]
pub fn small_order(pk: &[u8; 32]) -> bool {
    Point::decode(pk).is_none_or(|p| p.double().double().double().encode() == Point::ID.encode())
}
