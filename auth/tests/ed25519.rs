//! RFC 8032 §7.1 vectors and the refusals a verifier owes: flipped bits, a non-canonical S, an undecodable point.
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
)]
use mudrpc_auth::ed25519::{public_key, sign, verify};
use mudrpc_auth::sha512::sha512;

fn h(s: &str) -> Vec<u8> { (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() }
fn a32(v: &[u8]) -> [u8; 32] { v.try_into().unwrap() }
fn a64(v: &[u8]) -> [u8; 64] { v.try_into().unwrap() }

const L: &str = "edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010";
fn join(r: &[u8], s: &[u8]) -> [u8; 64] { a64(&[r, s].concat()) }
fn plus_l(s: &[u8]) -> [u8; 32] {
    let (l, mut out, mut carry) = (h(L), [0u8; 32], 0u16);
    for i in 0..32 { let v = u16::from(s[i]) + u16::from(l[i]) + carry; out[i] = v as u8; carry = v >> 8; }
    out
}

const VECTORS: [(&str, &str, &str, &str); 3] = [
    ("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60", "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "",
     "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"),
    ("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb", "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c", "72",
     "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"),
    ("c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7", "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025", "af82",
     "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a"),
];

#[test]
fn the_rfc_8032_vectors_sign_and_verify_byte_for_byte() {
    for (seed, pk, msg, sig) in VECTORS {
        let (seed, pk, msg, sig) = (a32(&h(seed)), a32(&h(pk)), h(msg), a64(&h(sig)));
        assert_eq!(public_key(&seed), pk);
        assert_eq!(sign(&seed, &pk, &[&msg]), sig);
        assert!(verify(&pk, &[&msg], &sig));
    }
    // TEST SHA(abc): the message is the SHA-512 of "abc"
    let (seed, pk) = (a32(&h("833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42")), a32(&h("ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf")));
    let msg = sha512(&[b"abc"]);
    let sig = a64(&h("dc2a4459e7369633a52b1bf277839a00201009a3efbf3ecb69bea2186c26b58909351fc9ac90b3ecfdfbc7c66431e0303dca179c138ac17ad9bef1177331a704"));
    assert_eq!(public_key(&seed), pk);
    assert_eq!(sign(&seed, &pk, &[&msg]), sig);
    assert!(verify(&pk, &[&msg], &sig));
    assert_eq!(sign(&seed, &pk, &[&msg[..10], &msg[10..]]), sig, "a message in parts is the same message");
}

#[test]
fn every_flipped_bit_and_every_malleation_is_refused() {
    let (seed, msg) = ([7u8; 32], b"arm".to_vec());
    let pk = public_key(&seed);
    let sig = sign(&seed, &pk, &[&msg]);
    assert!(verify(&pk, &[&msg], &sig));
    assert_eq!(sign(&seed, &pk, &[&msg]), sig, "deterministic");
    for i in 0..64 { let mut s = sig; s[i] ^= 1 << (i % 8); assert!(!verify(&pk, &[&msg], &s), "signature bit {i}"); }
    for i in 0..32 { let mut p = pk; p[i] ^= 1 << (i % 8); assert!(!verify(&p, &[&msg], &sig), "key bit {i}"); }
    let mut m = msg.clone(); m[0] ^= 1;
    assert!(!verify(&pk, &[&m], &sig));
    assert!(!verify(&pk, &[b"ar", b"m!"], &sig));
    // S + L encodes the same residue: a malleable verifier accepts it, RFC 8032 §5.1.7 forbids it
    assert!(!verify(&pk, &[&msg], &join(&sig[..32], &plus_l(&sig[32..]))), "non-canonical S");
    assert!(!verify(&[0xff; 32], &[&msg], &sig), "y above p is not a point");
    let mut not_on_curve = pk; not_on_curve[0] ^= 0x55;
    assert!(!verify(&not_on_curve, &[&msg], &sig), "either not decodable or a different key; never a pass");
    assert!(!verify(&public_key(&[8; 32]), &[&msg], &sig), "another key");
}

/// The fast path against the slow one: `verify`'s one pass of \[S\]B − \[k\]A against the ladder `sign` and `public_key`
/// run, on 300 random keys and messages; and with the identity as A, where the equation is R = \[S\]B, on the edge
/// scalars 0, 1 and L − 1 and on each key's clamped scalar reduced by subtraction, the ladder's \[a\]B as R.
#[test]
fn verify_agrees_with_the_ladder_on_random_keys_and_the_edge_scalars() {
    let (l, one, b) = (a32(&h(L)), a32(&h(&format!("01{}", "00".repeat(31)))), a32(&h(&format!("58{}", "66".repeat(31)))));
    let (mut minus_b, mut l_1, id) = (b, l, one); // the identity's encoding, y = 1, is the scalar 1's
    (minus_b[31], l_1[0]) = (b[31] | 0x80, l[0] - 1);
    for (r, s) in [(id, [0; 32]), (b, one), (minus_b, l_1)] { assert!(verify(&id, &[b"x"], &join(&r, &s)), "[S]B for S = {}", s[0]); }
    assert!(!verify(&id, &[b"x"], &join(&id, &l)), "L is 0, but not canonical");
    let reduce = |mut a: [u8; 32]| loop {
        let (mut d, mut borrow) = ([0u8; 32], 0u16);
        for i in 0..32 { let v = 0x100 + u16::from(a[i]) - u16::from(l[i]) - borrow; d[i] = v as u8; borrow = u16::from(v < 0x100); }
        if borrow == 1 { return a; }
        a = d;
    };
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    for round in 0..300 {
        let seed: [u8; 32] = std::array::from_fn(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; x as u8 });
        let (msg, pk) = (&seed[..round % 33], public_key(&seed));
        let sig = sign(&seed, &pk, &[msg]);
        assert!(verify(&pk, &[msg], &sig), "round {round}");
        for s in [[0; 32], one, l_1] { assert!(!verify(&pk, &[msg], &join(&sig[..32], &s)), "round {round}, S = {}", s[0]); }
        let mut a = a32(&sha512(&[&seed])[..32]);
        (a[0], a[31]) = (a[0] & 248, a[31] & 63 | 64);
        let a = reduce(a);
        assert!(verify(&id, &[msg], &join(&pk, &a)), "round {round}: [a]B");
        assert!(!verify(&id, &[msg], &join(&pk, &plus_l(&a))), "round {round}: a + L");
    }
}

/// The number the spec states per target is measured, not guessed: `cargo test --release -p mudrpc-auth -- --ignored --nocapture`.
#[test]
#[ignore = "cost measurement: cargo test --release -p mudrpc-auth -- --ignored --nocapture"]
fn cost_of_sign_and_verify_on_this_host() {
    let (seed, msg) = ([3u8; 32], [0u8; 100]);
    let pk = public_key(&seed);
    let t = std::time::Instant::now();
    let mut sig = [0u8; 64];
    for _ in 0..100 { sig = sign(&seed, &pk, &[&msg]); }
    let sign_us = t.elapsed().as_micros() / 100;
    let t = std::time::Instant::now();
    for _ in 0..100 { assert!(verify(&pk, &[&msg], &sig)); }
    println!("sign {sign_us} us, verify {} us", t.elapsed().as_micros() / 100);
}
