//! The primary sources' vectors, byte for byte: FIPS 197 appendix A.3 (the AES-256 key expansion) and C.3 (an AES-256
//! block), and test cases 13 to 16 of the GCM specification as submitted to NIST, which SP 800-38D standardised
//! (`gcm-spec.pdf`, appendix B; 17 and 18 use 8- and 60-byte IVs, and a mudRPC nonce is always 12 bytes).
#![warn(clippy::pedantic, clippy::nursery)]
use mudrpc_gcm::{aes, ghash, open, seal};

fn hex(s: &str) -> Vec<u8> { let s: String = s.split_whitespace().collect(); (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() }

#[test]
fn fips197_c3_aes256_block() {
    let key: [u8; 32] = hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f").try_into().unwrap();
    let p: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
    let mut b = [p, [0; 16]];
    aes::encrypt(&aes::schedule(&key), &mut b);
    assert_eq!(b[0].to_vec(), hex("8ea2b7ca516745bfeafc49904b496089"));
    assert_eq!(b[1], { let mut z = [[0; 16]; 2]; aes::encrypt(&aes::schedule(&key), &mut z); z[0] }, "the two blocks of a call are independent");
}

#[test]
fn fips197_a3_aes256_key_expansion() {
    let key: [u8; 32] = hex("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4").try_into().unwrap();
    let w = hex("603deb10 15ca71be 2b73aef0 857d7781 1f352c07 3b6108d7 2d9810a3 0914dff4
        9ba35411 8e6925af a51a8b5f 2067fcde a8b09c1a 93d194cd be49846e b75d5b9a d59aecb8 5bf3c917 fee94248 de8ebe96
        b5a9328a 2678a647 98312229 2f6c79b3 812c81ad dadf48ba 24360af2 fab8b464 98c5bfc9 bebd198e 268c3ba7 09e04214
        68007bac b2df3316 96e939e4 6c518d80 c814e204 76a9fb8a 5025c02d 59c58239 de136967 6ccc5a71 fa256395 9674ee15
        5886ca5d 2e2f31d7 7e0af1fa 27cf73c3 749c47ab 18501dda e2757e4f 7401905a cafaaae3 e4d59b34 9adf6ace bd10190d
        fe4890d1 e6188d0b 046df344 706c631e");
    let sk = aes::schedule(&key);
    let mut got = Vec::new();
    for rk in sk.0.chunks_exact(8) {
        let mut q: [u32; 8] = rk.try_into().unwrap();
        aes::ortho(&mut q); // the schedule is stored as bit-planes; ortho is its own inverse
        for pair in q.chunks_exact(2) { assert_eq!(pair[0], pair[1], "both lanes carry the same key"); got.extend_from_slice(&pair[0].to_le_bytes()); }
    }
    assert_eq!(got, w, "w[0..60]");
}

#[test]
fn gcm_spec_test_cases_13_to_16() {
    let k15 = "feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308";
    let p15 = "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255";
    let c15 = "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662898015ad";
    let cases = [
        (13, "00".repeat(32), "00".repeat(12), String::new(), String::new(), String::new(), "530f8afbc74536b9a963b4f1c4cb738b"),
        (14, "00".repeat(32), "00".repeat(12), String::new(), "00".repeat(16), "cea7403d4d606b6e074ec5d3baf39d18".into(), "d0d1c8a799996bf0265b98b5d48ab919"),
        (15, k15.into(), "cafebabefacedbaddecaf888".into(), String::new(), p15.into(), c15.into(), "b094dac5d93471bdec1a502270e3cc6c"),
        (16, k15.into(), "cafebabefacedbaddecaf888".into(), "feedfacedeadbeeffeedfacedeadbeefabaddad2".into(), p15[..120].into(), c15[..120].into(), "76fc6ece0f4e1768cddf8853bb2d551b"),
    ];
    for (n, key, iv, aad, p, c, t) in cases {
        let (key, iv, aad, p) = (<[u8; 32]>::try_from(hex(&key)).unwrap(), <[u8; 12]>::try_from(hex(&iv)).unwrap(), hex(&aad), hex(&p));
        let mut wire = vec![0u8; 12 + p.len() + 16];
        assert_eq!(seal(&key, &iv, &aad, &p, &mut wire), Ok(wire.len()), "case {n}");
        assert_eq!((&wire[..12], wire[12..12 + p.len()].to_vec(), wire[12 + p.len()..].to_vec()), (&iv[..], hex(&c), hex(t)), "case {n}");
        let mut out = vec![0u8; p.len()];
        assert_eq!(open(&key, &aad, &wire, &mut out), Ok(p.len()), "case {n}");
        assert_eq!(out, p, "case {n}");
    }
    // case 14's intermediate values, so a wrong tag says which half is wrong
    let mut h = [[0u8; 16]; 2];
    aes::encrypt(&aes::schedule(&[0; 32]), &mut h);
    assert_eq!(h[0].to_vec(), hex("dc95c078a2408989ad48a21492842087"), "H");
    assert_eq!(ghash::ghash(&h[0], &[], &hex("cea7403d4d606b6e074ec5d3baf39d18")).to_vec(), hex("83de425c5edc5d498f382c441041ca92"), "GHASH");
}
